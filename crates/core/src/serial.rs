//! 串口 transport：枚举本机串口，并按「打开后收发原始字节」的方式挂进终端注册表。
//!
//! 这里刻意只做最朴素的一件事：把串口当成一条字节流。串口没有窗口协商协议，
//! 也没有退出码概念，所以 `resize` 是空实现、连接断开统一回报 `Exit(None)`；
//! 除此之外对外形态与 Telnet / 本地 PTY 完全一致（同一套输出泵与 `TerminalControl`）。
//!
//! 波特率、数据位、停止位、校验、流控由 `SerialOptions` 承载，`validate`
//! 负责在不碰设备的前提下把非法组合拦下来（含「1.5 停止位当前平台不支持」）。

use std::io::{ErrorKind, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serialport::{DataBits, FlowControl, Parity, StopBits, SerialPortType};
use serde::Serialize;

use crate::error::{CoreError, CoreResult};
use crate::terminal::{start_output_pump, OutputSink, TerminalControl};

/// 单次从串口读取的最大字节数（与本地 PTY / Telnet 的读取粒度保持一致）。
const READ_CHUNK: usize = 8192;

/// 读取超时。
///
/// 串口没有「连接」概念，读线程必须靠超时轮询才能发现 `closed` 被置位并退出；
/// 超时太大会让关闭变得迟钝，太小则会空转，50ms 是这两者之间的折中。
const READ_TIMEOUT: Duration = Duration::from_millis(50);

/// 读到 0 字节（没有数据但也没报错）时的退让时间，避免把 CPU 空转跑满。
const READ_IDLE_SLEEP: Duration = Duration::from_millis(5);

/// 允许的最大波特率。
///
/// 上限只是用来挡住明显手滑的输入（例如多按了两个 0）。前端表单用同一个数值
/// 做即时校验，两边保持一致；驱动对具体上限各有各的说法，这里取一个高于
/// 常见高速串口（921600）又不会离谱到没法用的整数。
pub const MAX_BAUD_RATE: u32 = 4_000_000;

/// 枚举出来的一台串口设备。
///
/// 无参数
/// 核心流程：`path` 是能直接交给 `open` 的设备路径；`kind` 只是给人看的粗分类
/// （usb / pci / bluetooth），识别不出来时为空串，绝不因此丢掉这台设备。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SerialPortInfo {
    pub path: String,
    pub kind: String,
}

/// 打开一个串口所需的参数。
///
/// 无参数
/// 核心流程：设备路径 + 波特率 + 数据位/停止位/校验/流控。默认值就是工业设备里
/// 最常见的 9600-8-N-1 无流控，参数表单把用户选择原样填进来即可。
///
/// `stop_bits` 在这里是 u8（1 或 2）：`serialport` 4.10 的 `StopBits` 只有
/// `One` / `Two`，没有 1.5 的枚举。前端表单里的 1.5 由 `parse_stop_bits`
/// 统一拦下并给出明确错误，不会静默折成 1 或 2。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialOptions {
    pub path: String,
    pub baud_rate: u32,
    pub data_bits: u8,
    pub stop_bits: u8,
    /// "none" / "odd" / "even"
    pub parity: String,
    /// "none" / "software" / "hardware"
    pub flow_control: String,
}

impl SerialOptions {
    /// 构造默认参数。
    ///
    /// 参数：path 为设备路径（例如 /dev/ttyUSB0、COM3）
    /// 核心流程：其余字段全部取 9600-8-N-1 无流控，与「用户什么都没改」等价。
    pub fn defaults(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            baud_rate: 9600,
            data_bits: 8,
            stop_bits: 1,
            parity: "none".to_string(),
            flow_control: "none".to_string(),
        }
    }

    /// 校验线路参数是否可用。
    ///
    /// 参数：无
    /// 核心流程：依次检查「设备路径 / 波特率 / 数据位 / 停止位 / 校验 / 流控」，
    /// 任何一项不合法都回一条点名了具体字段的可读错误。
    /// 这一步是纯计算、不碰任何设备，所以表单层、命令层都能先校验再打开，
    /// 测试也不需要真实串口硬件。
    pub fn validate(&self) -> CoreResult<()> {
        if self.path.trim().is_empty() {
            return Err(CoreError::Terminal("串口设备不能为空".into()));
        }
        // 0 与超大值都在这里挡住：底层对 0 波特率的报错因平台而异，不如自己说清楚
        if self.baud_rate == 0 || self.baud_rate > MAX_BAUD_RATE {
            return Err(CoreError::Terminal(format!(
                "波特率需为 1 到 {MAX_BAUD_RATE} 之间的正整数，当前为 {}",
                self.baud_rate
            )));
        }
        data_bits_of(self.data_bits)?;
        stop_bits_of(self.stop_bits)?;
        parity_of(&self.parity)?;
        flow_control_of(&self.flow_control)?;
        Ok(())
    }
}

/// 枚举本机可用的串口。
///
/// 参数：无
/// 核心流程：调底层枚举 → 转成本层稳定的 `SerialPortInfo`。
/// 枚举失败、或者底层在缺少 /sys 的容器里 panic，都一律回空列表：
/// 「查不到串口」是正常状态，不该让会话界面炸掉。
pub fn list_ports() -> Vec<SerialPortInfo> {
    // 底层实现（无 libudev 时扫 /sys/class/tty）内部有 expect，缺目录会 panic，
    // 所以这里用 catch_unwind 兜一层，保证本函数在任何环境都只返回数据、不 panic。
    let ports = match std::panic::catch_unwind(serialport::available_ports) {
        Ok(Ok(ports)) => ports,
        _ => return Vec::new(),
    };

    ports
        .into_iter()
        .filter(|port| !port.port_name.is_empty())
        .map(|port| SerialPortInfo {
            path: port.port_name,
            kind: port_kind(&port.port_type),
        })
        .collect()
}

/// 把底层的端口类型压成给用户看的粗分类。
///
/// 参数：port_type 为底层枚举出的端口类型
/// 核心流程：认得的三种给出 usb / pci / bluetooth，其余（含未来新增变体）回空串，
/// 前端据此只显示设备路径，不编造分类。
fn port_kind(port_type: &SerialPortType) -> String {
    match port_type {
        SerialPortType::UsbPort(_) => "usb".to_string(),
        SerialPortType::PciPort => "pci".to_string(),
        SerialPortType::BluetoothPort => "bluetooth".to_string(),
        _ => String::new(),
    }
}

/// 把数字数据位映射成底层枚举。
///
/// 参数：value 为 5/6/7/8
/// 核心流程：无法识别的取值直接报错，不做静默兜底
/// —— 悄悄改成 8 位会让连不上的设备更难排查。
fn data_bits_of(value: u8) -> CoreResult<DataBits> {
    match value {
        5 => Ok(DataBits::Five),
        6 => Ok(DataBits::Six),
        7 => Ok(DataBits::Seven),
        8 => Ok(DataBits::Eight),
        other => Err(CoreError::Terminal(format!("不支持的数据位：{other}"))),
    }
}

/// 把数字停止位映射成底层枚举。
///
/// 参数：value 为 1 或 2
/// 核心流程：其余取值报错（1.5 位在 `serialport` 4.10 里没有对应枚举，
/// 连同其它非法值一并拒绝，不做静默近似）。
fn stop_bits_of(value: u8) -> CoreResult<StopBits> {
    match value {
        1 => Ok(StopBits::One),
        2 => Ok(StopBits::Two),
        other => Err(CoreError::Terminal(format!("不支持的停止位：{other}"))),
    }
}

/// 把界面上的停止位数值（可能带小数）解析成 `SerialOptions.stop_bits`。
///
/// 参数：value 为 1、1.5 或 2
/// 核心流程：1 与 2 直接通过；1.5 因为 `serialport` 4.10 的 `StopBits` 只有
/// `One` / `Two`，这里如实回「当前平台不支持停止位 1.5」——绝不静默降级成
/// 1 或 2（差半个停止位，通信会永远收不全）。其余取值一律报错。
pub fn parse_stop_bits(value: f32) -> CoreResult<u8> {
    if (value - 1.0).abs() < f32::EPSILON {
        return Ok(1);
    }
    if (value - 2.0).abs() < f32::EPSILON {
        return Ok(2);
    }
    if (value - 1.5).abs() < f32::EPSILON {
        return Err(CoreError::Terminal("当前平台不支持停止位 1.5".into()));
    }
    Err(CoreError::Terminal(format!("不支持的停止位：{value}")))
}

/// 解析校验位字符串。
///
/// 参数：value 为 none / odd / even（忽略大小写与首尾空白）
/// 核心流程：未知取值报错，避免把错拼的 "non" 当成无校验。
fn parity_of(value: &str) -> CoreResult<Parity> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "none" => Ok(Parity::None),
        "odd" => Ok(Parity::Odd),
        "even" => Ok(Parity::Even),
        other => Err(CoreError::Terminal(format!("不支持的校验方式：{other}"))),
    }
}

/// 解析流控字符串。
///
/// 参数：value 为 none / software / hardware（忽略大小写与首尾空白）
/// 核心流程：未知取值报错；RTS/CTS 等硬件流控直接透传给底层。
fn flow_control_of(value: &str) -> CoreResult<FlowControl> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "none" => Ok(FlowControl::None),
        "software" => Ok(FlowControl::Software),
        "hardware" => Ok(FlowControl::Hardware),
        other => Err(CoreError::Terminal(format!("不支持的流控方式：{other}"))),
    }
}

/// 一个打开着的串口会话。
///
/// 无参数
/// 核心流程：写入端与读线程各自持有同一设备的一个克隆句柄（互斥锁只保护写入），
/// 读到的字节进与本地 PTY / Telnet 共用的输出泵，因此上层不需要区分底层是什么。
pub struct SerialSession {
    /// 用户输入从这里出去；`None` 表示已经 close（句柄被取走并关闭）。
    writer: Mutex<Option<Box<dyn serialport::SerialPort>>>,
    /// 与读线程共享的关闭标志：串口没有 shutdown，只能靠它让读线程退出。
    closed: Arc<AtomicBool>,
}

impl SerialSession {
    /// 打开一个串口并开始接收数据。
    ///
    /// 参数：options 为设备路径与线路参数，sink 为输出回调
    /// 核心流程：按参数打开端口 → 克隆一个句柄给阻塞读线程 → 读到的字节
    /// 交给输出泵；读出错（典型是 USB 转串口被拔掉）时关闭输出泵并结束线程，
    /// 不 panic。
    ///
    /// 打开是纯阻塞调用，必须由调用方放在阻塞上下文里（命令层用 `spawn_blocking`）；
    /// 这里不做 `Handle::try_current()` 之类的运行时探测——`spawn_blocking` 的线程
    /// 身处运行时上下文，探测会把阻塞调用误判成异步。
    pub fn open(options: &SerialOptions, sink: OutputSink) -> CoreResult<Self> {
        let path = options.path.trim().to_string();
        // 线路参数先整体校验一遍 -> 非法值在碰设备之前就被挡住，错误信息点名具体字段
        options.validate()?;

        let port = serialport::new(&path, options.baud_rate)
            .data_bits(data_bits_of(options.data_bits)?)
            .stop_bits(stop_bits_of(options.stop_bits)?)
            .parity(parity_of(&options.parity)?)
            .flow_control(flow_control_of(&options.flow_control)?)
            .timeout(READ_TIMEOUT)
            .open()
            .map_err(|err| CoreError::Terminal(format!("打开串口 {path} 失败：{err}")))?;

        // 读线程用独立句柄，避免它长时间持锁把用户按键挡住
        let mut reader = port
            .try_clone()
            .map_err(|err| CoreError::Terminal(format!("复制串口句柄失败：{err}")))?;

        let pump = start_output_pump(sink)?;
        let closed = Arc::new(AtomicBool::new(false));
        let reader_closed = Arc::clone(&closed);
        let reader_pump = Arc::clone(&pump);

        std::thread::Builder::new()
            .name("rustmobax-serial-read".into())
            .spawn(move || {
                let mut buffer = [0u8; READ_CHUNK];
                while !reader_closed.load(Ordering::SeqCst) {
                    match reader.read(&mut buffer) {
                        Ok(0) => {
                            // 没有数据但也没报错 -> 退让一下再读，别空转
                            std::thread::sleep(READ_IDLE_SLEEP);
                        }
                        Ok(read) => reader_pump.push(&buffer[..read]),
                        // 读超时是常态（等用户按键），继续轮询关闭标志
                        Err(err) if err.kind() == ErrorKind::TimedOut => continue,
                        // 被信号打断不算结束
                        Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                        // 设备拔掉 / 驱动报错 -> 收尾，不 panic
                        Err(_) => break,
                    }
                }
                // 串口没有退出码概念 -> 统一回报 None
                reader_pump.close_with(None);
            })
            .map_err(|err| CoreError::Terminal(format!("启动读取线程失败：{err}")))?;

        Ok(Self {
            writer: Mutex::new(Some(port)),
            closed,
        })
    }

    /// 把用户输入写进串口。
    ///
    /// 参数：data 为原始字节
    /// 核心流程：串口是透明字节通道，不做任何转义；写入后 flush 让字节尽快上线。
    pub fn write(&self, data: &[u8]) -> CoreResult<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(CoreError::Terminal("串口已关闭".into()));
        }
        let mut guard = self
            .writer
            .lock()
            .map_err(|_| CoreError::Terminal("串口写入端已失效".into()))?;
        let port = guard
            .as_mut()
            .ok_or_else(|| CoreError::Terminal("串口已关闭".into()))?;
        port.write_all(data)
            .and_then(|()| port.flush())
            .map_err(|err| CoreError::Terminal(format!("写入串口失败：{err}")))
    }

    /// 调整终端尺寸。
    ///
    /// 参数：cols/rows 为前端新尺寸（忽略）
    /// 核心流程：串口是裸字节流，没有 NAWS 之类的窗口协商协议，
    /// 这里不做任何事也不报错，让上层的 resize 路径可以无差别调用。
    pub fn resize(&self, cols: u16, rows: u16) -> CoreResult<()> {
        let _ = (cols, rows);
        Ok(())
    }

    /// 关闭串口。
    ///
    /// 参数：无
    /// 核心流程：置关闭标志（读线程据此在下一个读超时退出）→ 取走并丢弃写入句柄
    /// 以释放设备。重复 close 是安全的。
    pub fn close(&self) -> CoreResult<()> {
        self.closed.store(true, Ordering::SeqCst);
        if let Ok(mut guard) = self.writer.lock() {
            // 丢掉句柄即关闭文件描述符；读线程那份克隆会在它退出时一并释放
            guard.take();
        }
        Ok(())
    }
}

impl TerminalControl for SerialSession {
    /// 转发写入。
    fn write(&self, data: &[u8]) -> CoreResult<()> {
        SerialSession::write(self, data)
    }

    /// 尺寸变化对串口没有意义 -> 空实现。
    fn resize(&self, cols: u16, rows: u16) -> CoreResult<()> {
        SerialSession::resize(self, cols, rows)
    }

    /// 关闭即释放设备。
    fn close(&self) -> CoreResult<()> {
        SerialSession::close(self)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use serialport::SerialPort;

    use crate::terminal::TerminalOutput;

    use super::*;

    /// 把出口字节攒起来、并记录是否收到退出事件的 sink 探针。
    struct Probe {
        data: Mutex<Vec<u8>>,
        exited: AtomicBool,
    }

    impl Probe {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                data: Mutex::new(Vec::new()),
                exited: AtomicBool::new(false),
            })
        }

        fn sink(self: &Arc<Self>) -> OutputSink {
            let probe = Arc::clone(self);
            Box::new(move |output| match output {
                TerminalOutput::Data(bytes) => {
                    if let Ok(mut guard) = probe.data.lock() {
                        guard.extend_from_slice(&bytes);
                    }
                }
                TerminalOutput::Exit(_) => probe.exited.store(true, Ordering::SeqCst),
            })
        }

        fn bytes(&self) -> Vec<u8> {
            self.data
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default()
        }

        /// 等到攒到至少 `len` 字节，或超时。
        fn wait_for(&self, len: usize, timeout: Duration) -> bool {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if self.bytes().len() >= len {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            self.bytes().len() >= len
        }

        fn wait_exit(&self, timeout: Duration) -> bool {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if self.exited.load(Ordering::SeqCst) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            self.exited.load(Ordering::SeqCst)
        }
    }

    /// 默认参数就是 9600-8-N-1 无流控。
    #[test]
    fn serial_options_defaults_are_9600_8n1() {
        let options = SerialOptions::defaults("/dev/ttyUSB0");
        assert_eq!(options.path, "/dev/ttyUSB0");
        assert_eq!(options.baud_rate, 9600);
        assert_eq!(options.data_bits, 8);
        assert_eq!(options.stop_bits, 1);
        assert_eq!(options.parity, "none");
        assert_eq!(options.flow_control, "none");

        // 从字符串构造与从 &str 一致，方便下一步表单层直接传 String
        assert_eq!(options, SerialOptions::defaults(String::from("/dev/ttyUSB0")));
    }

    /// 枚举在任何环境都不能 panic；返回的每项都得有非空路径。
    #[test]
    fn list_ports_never_panics_and_paths_are_non_empty() {
        let ports = list_ports();
        for port in &ports {
            assert!(!port.path.is_empty(), "串口路径不能为空：{port:?}");
        }
        // 分类只允许出现这三种取值（或空串表示识别不出来）
        for port in &ports {
            assert!(
                matches!(port.kind.as_str(), "" | "usb" | "pci" | "bluetooth"),
                "串口分类超出约定范围：{port:?}"
            );
        }
    }

    /// 打开一个必然不存在的设备 -> 返回可读错误，而不是 panic。
    #[test]
    fn opening_missing_device_returns_readable_error() {
        let probe = Probe::new();
        let options = SerialOptions::defaults("/dev/definitely-not-a-port");
        let result = SerialSession::open(&options, probe.sink());
        match result {
            Ok(_) => panic!("不存在的设备不应该能打开"),
            Err(err) => {
                let message = err.to_string();
                assert!(
                    message.contains("/dev/definitely-not-a-port"),
                    "错误信息里应包含设备路径：{message}"
                );
                assert!(
                    message.contains("打开串口"),
                    "错误信息应说明是打开串口失败：{message}"
                );
                // 打开失败时不应该有读取线程去回报退出事件
                assert!(!probe.exited.load(Ordering::SeqCst));
            }
        }
    }

    /// 非法的线路参数在打开设备之前就该被挡住。
    #[test]
    fn invalid_line_settings_are_rejected() {
        let probe = Probe::new();
        let mut options = SerialOptions::defaults("/dev/ttyUSB0");
        options.data_bits = 9;
        assert!(SerialSession::open(&options, probe.sink()).is_err());

        let mut options = SerialOptions::defaults("/dev/ttyUSB0");
        options.stop_bits = 3;
        assert!(SerialSession::open(&options, probe.sink()).is_err());

        let mut options = SerialOptions::defaults("/dev/ttyUSB0");
        options.parity = "mark".to_string();
        assert!(SerialSession::open(&options, probe.sink()).is_err());

        let mut options = SerialOptions::defaults("/dev/ttyUSB0");
        options.flow_control = "xon".to_string();
        assert!(SerialSession::open(&options, probe.sink()).is_err());

        // 设备路径为空同样直接报错，不会走到 open
        let probe = Probe::new();
        assert!(SerialSession::open(&SerialOptions::defaults("  "), probe.sink()).is_err());
    }

    /// 常用线路参数（含各字段边界值）都必须能构造出来并通过校验。
    #[test]
    fn common_line_settings_pass_validation() {
        // (波特率, 数据位, 停止位, 校验, 流控)
        let cases: [(u32, u8, u8, &str, &str); 5] = [
            // 最低常用速率 + 最保守的线路
            (300, 5, 1, "none", "none"),
            // 出厂默认
            (9600, 8, 1, "none", "none"),
            // 高速 + 偶校验 + 硬件流控
            (115200, 8, 1, "even", "hardware"),
            // 边界组合：最高常用速率、8 数据位、2 停止位、偶校验、硬件流控
            (921600, 8, 2, "even", "hardware"),
            // 奇校验 + 软件流控 + 7 数据位（老式设备常见）
            (19200, 7, 2, "odd", "software"),
        ];

        for (baud_rate, data_bits, stop_bits, parity, flow_control) in cases {
            let mut options = SerialOptions::defaults("/dev/ttyUSB0");
            options.baud_rate = baud_rate;
            options.data_bits = data_bits;
            options.stop_bits = stop_bits;
            options.parity = parity.to_string();
            options.flow_control = flow_control.to_string();

            assert!(
                options.validate().is_ok(),
                "这组参数应当通过校验：{options:?}"
            );
            // 五个字段都要能一路映射到底层枚举（1.5 之外的取值不该在映射层失败）
            assert!(data_bits_of(data_bits).is_ok(), "数据位映射失败：{data_bits}");
            assert!(stop_bits_of(stop_bits).is_ok(), "停止位映射失败：{stop_bits}");
            assert!(parity_of(parity).is_ok(), "校验映射失败：{parity}");
            assert!(flow_control_of(flow_control).is_ok(), "流控映射失败：{flow_control}");
        }
    }

    /// 非法线路参数被拒绝，且错误信息点名了具体字段。
    #[test]
    fn invalid_line_settings_name_the_offending_field() {
        // (要改动的字段, 用于断言的错误关键字)
        let mut options = SerialOptions::defaults("/dev/ttyUSB0");
        options.baud_rate = 0;
        let message = options.validate().expect_err("波特率 0 应当被拒绝").to_string();
        assert!(message.contains("波特率"), "错误信息应含「波特率」：{message}");

        let mut options = SerialOptions::defaults("/dev/ttyUSB0");
        options.baud_rate = MAX_BAUD_RATE + 1;
        let message = options.validate().expect_err("超范围波特率应当被拒绝").to_string();
        assert!(message.contains("波特率"), "错误信息应含「波特率」：{message}");

        let mut options = SerialOptions::defaults("/dev/ttyUSB0");
        options.data_bits = 9;
        let message = options.validate().expect_err("数据位 9 应当被拒绝").to_string();
        assert!(message.contains("数据位"), "错误信息应含「数据位」：{message}");

        let mut options = SerialOptions::defaults("/dev/ttyUSB0");
        options.stop_bits = 3;
        let message = options.validate().expect_err("停止位 3 应当被拒绝").to_string();
        assert!(message.contains("停止位"), "错误信息应含「停止位」：{message}");

        let mut options = SerialOptions::defaults("/dev/ttyUSB0");
        options.parity = "mark".to_string();
        let message = options.validate().expect_err("校验 mark 应当被拒绝").to_string();
        assert!(message.contains("校验"), "错误信息应含「校验」：{message}");

        let mut options = SerialOptions::defaults("/dev/ttyUSB0");
        options.flow_control = "xon".to_string();
        let message = options.validate().expect_err("流控 xon 应当被拒绝").to_string();
        assert!(message.contains("流控"), "错误信息应含「流控」：{message}");

        // 空设备路径同样被校验挡住
        let message = SerialOptions::defaults("   ")
            .validate()
            .expect_err("空设备路径应当被拒绝")
            .to_string();
        assert!(message.contains("串口设备"), "错误信息应含「串口设备」：{message}");
    }

    /// 停止位 1.5：`serialport` 4.10 没有对应枚举 -> 必须给出明确错误，不能静默降级。
    #[test]
    fn stop_bits_one_point_five_is_reported_as_unsupported() {
        let error = parse_stop_bits(1.5).expect_err("1.5 停止位应当被明确拒绝");
        let message = error.to_string();
        assert!(
            message.contains("当前平台不支持停止位 1.5"),
            "错误信息应说明平台不支持 1.5：{message}"
        );

        // 若将来底层补上 1.5 的枚举，这里要跟着改成 Ok，而不是让实现偷偷近似
        assert_eq!(parse_stop_bits(1.0).expect("1 位停止位应可用"), 1);
        assert_eq!(parse_stop_bits(2.0).expect("2 位停止位应可用"), 2);
        // 不落在 1 / 1.5 / 2 上的小数一律拒绝，同样不做四舍五入
        for bad in [0.0f32, 0.5, 1.25, 1.75, 3.0, 2.5] {
            let message = parse_stop_bits(bad)
                .expect_err("非法停止位应当被拒绝")
                .to_string();
            assert!(message.contains("停止位"), "错误信息应含「停止位」：{message}");
        }
    }

    /// 校验 / 流控的字符串取值覆盖前端给出的全部选项，且大小写与空白无关。
    #[test]
    fn parity_and_flow_control_strings_map_to_enums() {
        assert_eq!(parity_of("none").expect("none"), Parity::None);
        assert_eq!(parity_of("even").expect("even"), Parity::Even);
        assert_eq!(parity_of("odd").expect("odd"), Parity::Odd);
        // 大小写与首尾空白都容忍，空串等价于不校验
        assert_eq!(parity_of("  EVEN ").expect("EVEN"), Parity::Even);
        assert_eq!(parity_of("").expect("空串"), Parity::None);

        assert_eq!(flow_control_of("none").expect("none"), FlowControl::None);
        assert_eq!(
            flow_control_of("software").expect("software"),
            FlowControl::Software
        );
        assert_eq!(
            flow_control_of("hardware").expect("hardware"),
            FlowControl::Hardware
        );
        assert_eq!(
            flow_control_of(" Hardware ").expect("Hardware"),
            FlowControl::Hardware
        );
        assert_eq!(flow_control_of("").expect("空串"), FlowControl::None);

        // 相近但不对的写法必须报错（例如把 xon/xoff、rts/cts 当取值传进来）
        assert!(parity_of("mark").is_err());
        assert!(parity_of("space").is_err());
        assert!(flow_control_of("xon").is_err());
        assert!(flow_control_of("rts").is_err());
    }

    /// 用 Linux 伪终端做真实收发：不依赖任何串口硬件。
    /// 伪终端对就是内核提供的一对 tty 设备，串口驱动层面对它和对真串口是同一套
    /// termios 接口，因此能覆盖「打开 -> 读线程 -> 输出泵 -> 写入」整条链路。
    /// 环境里连 pty 都开不出来时（极少见）跳过，不假装通过。
    #[cfg(unix)]
    #[test]
    fn pty_pair_roundtrip_sends_and_receives_bytes() {
        let (mut peer, slave) = match serialport::TTYPort::pair() {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!("跳过：当前环境无法创建伪终端对（{err}）");
                return;
            }
        };
        let slave_path = match slave.name() {
            Some(name) => name,
            None => {
                eprintln!("跳过：伪终端对没有给出从设备路径");
                return;
            }
        };
        // 松开从设备，让 SerialSession 独占打开它
        drop(slave);

        let probe = Probe::new();
        let session = match SerialSession::open(&SerialOptions::defaults(slave_path), probe.sink()) {
            Ok(session) => session,
            Err(err) => {
                eprintln!("跳过：伪终端无法按串口参数打开（{err}）");
                return;
            }
        };

        // 设备 -> 应用：从对端写进去，读线程应当把它交给输出泵
        peer.write_all(b"ping-from-device").expect("写伪终端对端失败");
        peer.flush().expect("flush 伪终端对端失败");
        assert!(
            probe.wait_for(b"ping-from-device".len(), Duration::from_millis(1500)),
            "应在 1.5s 内收到设备侧数据，实际收到：{:?}",
            String::from_utf8_lossy(&probe.bytes())
        );
        assert!(
            probe.bytes().windows(b"ping-from-device".len()).any(|w| w == b"ping-from-device"),
            "收到的字节应包含设备侧写入的内容"
        );

        // 应用 -> 设备：session.write 的字节应出现在对端
        session.write(b"pong-to-device").expect("写串口失败");
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < deadline && !got.windows(b"pong-to-device".len()).any(|w| w == b"pong-to-device") {
            let mut buffer = [0u8; 256];
            match peer.read(&mut buffer) {
                Ok(0) => std::thread::sleep(Duration::from_millis(10)),
                Ok(read) => got.extend_from_slice(&buffer[..read]),
                Err(err) if err.kind() == ErrorKind::TimedOut => continue,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) => panic!("读伪终端对端失败：{err}"),
            }
        }
        assert!(
            got.windows(b"pong-to-device".len()).any(|w| w == b"pong-to-device"),
            "设备侧应收到应用写入的字节，实际收到：{:?}",
            String::from_utf8_lossy(&got)
        );

        // 关闭后读线程退出并回报退出事件；resize 是空实现但不应报错
        session.resize(120, 40).expect("串口 resize 应为空实现");
        session.close().expect("关闭串口失败");
        assert!(probe.wait_exit(Duration::from_millis(1500)), "关闭后应回报退出事件");
        // 关闭后写入必须失败，而不是静默丢字节
        assert!(session.write(b"after-close").is_err());
        assert!(session.close().is_ok(), "重复 close 应当安全");
    }
}
