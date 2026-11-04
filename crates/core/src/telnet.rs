//! Telnet 客户端：TCP 直通 + 最小 IAC 协商。
//!
//! 协商只做真实 telnetd 必须的那一小撮：ECHO / SGA / TTYPE / NAWS，其余选项
//! 一律拒绝。目的是让 `0xFF` 命令字节不再被当成终端输出、让远端知道窗口尺寸，
//! 把完整的 RFC 1143 选项状态机留给将来（现在用「每个方向一次表态」的简化表）。
//!
//! 对外形态与本地 PTY 完全一致：解析出的普通数据进共用输出泵，协商应答直接
//! 回写 socket，上层看不到任何协议细节。

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::{CoreError, CoreResult};
use crate::terminal::{start_output_pump, OutputSink, TerminalControl};

/// 单次从 socket 读取的最大字节数（与本地 PTY 的读取粒度保持一致）。
const READ_CHUNK: usize = 8192;

/// 建立 TCP 连接的超时时间。
///
/// 没有它的话，连一台被防火墙丢包的主机会一直挂着，用户永远等不到错误。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 命令字节 IAC（Interpret As Command），也是数据里的转义字符。
const IAC: u8 = 0xFF;
const DONT: u8 = 0xFE;
const DO: u8 = 0xFD;
const WONT: u8 = 0xFC;
const WILL: u8 = 0xFB;
const SB: u8 = 0xFA;
const SE: u8 = 0xF0;

/// 我们认识的选项；不在这个白名单里的一律 WONT/DONT。
const OPT_ECHO: u8 = 1;
const OPT_SGA: u8 = 3;
const OPT_TTYPE: u8 = 24;
const OPT_NAWS: u8 = 31;

/// TERMINAL-TYPE 子协商的命令字节。
const TTYPE_IS: u8 = 0;
const TTYPE_SEND: u8 = 1;

/// 单个子协商最多累积多少字节。
///
/// 对端不发 `IAC SE` 时不能让缓冲无限长，否则一句恶意的 `SB` 就能吃掉内存。
const SUB_MAX: usize = 1024;

/// 建立 Telnet 会话所需的参数。
///
/// 无参数
/// 核心流程：host/port 决定连到哪里；term 用于回 TERMINAL-TYPE，
/// cols/rows 既是初始窗口尺寸，也是 NAWS 重新上报的来源。
#[derive(Debug, Clone)]
pub struct TelnetOptions {
    pub host: String,
    pub port: u16,
    /// 终端类型，回 TERMINAL-TYPE 子协商用。
    pub term: String,
    pub cols: u16,
    pub rows: u16,
}

impl Default for TelnetOptions {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 23,
            term: "xterm-256color".to_string(),
            cols: 80,
            rows: 24,
        }
    }
}

/// IAC 解析器的状态。
///
/// 之所以要显式存状态，是因为一个协商序列完全可能被 TCP 拆成两次 read 到达
/// （`[0xFF]` + `[0xFB, 0x18]` 这种），解析器必须能跨调用续上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParseState {
    /// 普通数据
    Data,
    /// 刚读到 IAC，等命令字节
    Iac,
    /// 读到 IAC + WILL/WONT/DO/DONT，等选项字节
    Negotiation(u8),
    /// 读到 IAC SB，正在收集子协商内容
    SubData,
    /// 子协商里读到 IAC，等 SE（或转义的 IAC）
    SubIac,
}

/// 纯 IAC 状态机：与网络、线程完全解耦，因此可以被单测直接喂字节。
///
/// 无参数
/// 核心流程：`feed` 逐字节推进状态，产出「给终端的数据」与「回给对端的应答」
/// 两份字节流；`initial` 给出连接建立后应主动发送的协商首包；`resize` 在
/// NAWS 协商成功后产出新的窗口尺寸子协商。
pub struct TelnetParser {
    state: ParseState,
    /// 当前子协商累积的字节（第 0 个是选项号）。
    sub: Vec<u8>,
    /// 本端选项表态：true = 已发 WILL，false = 已发 WONT。
    ///
    /// 存下表态结果就实现了幂等：对端重复发同一个 DO 不会被我们反复应答，
    /// 从而避免协商风暴。
    local: HashMap<u8, bool>,
    /// 对端选项表态：true = 已发 DO，false = 已发 DONT。
    remote: HashMap<u8, bool>,
    term: String,
    cols: u16,
    rows: u16,
    /// NAWS 是否已协商成功（我们发了 WILL NAWS 且收到对端 DO NAWS）。
    naws: bool,
}

impl TelnetParser {
    /// 构造解析器。
    ///
    /// 参数：term 为终端类型，cols/rows 为初始尺寸
    /// 核心流程：所有选项都处于「未表态」状态，等对端来问或由 `initial` 主动开口。
    pub fn new(term: &str, cols: u16, rows: u16) -> Self {
        Self {
            state: ParseState::Data,
            sub: Vec::new(),
            local: HashMap::new(),
            remote: HashMap::new(),
            term: term.to_string(),
            cols,
            rows,
            naws: false,
        }
    }

    /// 连接建立后应当主动发送的首包。
    ///
    /// 参数：无
    /// 核心流程：`IAC WILL NAWS`（我想报窗口尺寸）+ `IAC WILL TTYPE`（我能报终端类型）。
    /// 只表态、不发子协商：NAWS 的子协商必须等对端 `DO NAWS` 之后才发，
    /// 提前发会被多数 telnetd 直接丢弃。
    pub fn initial(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        for option in [OPT_NAWS, OPT_TTYPE] {
            self.local.insert(option, true);
            push_command(&mut out, WILL, option);
        }
        out
    }

    /// 喂一段任意分片的输入。
    ///
    /// 参数：input 为这次从 socket 读到的字节
    /// 核心流程：逐字节推进状态机；返回 `(数据, 应答)` —— 数据按序推给终端，
    /// 应答原样写回 socket。跨调用状态保存在 `self.state` 里，因此分片不影响结果。
    pub fn feed(&mut self, input: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut data = Vec::with_capacity(input.len());
        let mut reply = Vec::new();

        for &byte in input {
            match self.state {
                ParseState::Data => {
                    if byte == IAC {
                        self.state = ParseState::Iac;
                    } else {
                        data.push(byte);
                    }
                }
                ParseState::Iac => match byte {
                    // 转义写法 -> 真的就是数据里的一个 0xFF
                    IAC => {
                        data.push(IAC);
                        self.state = ParseState::Data;
                    }
                    WILL | WONT | DO | DONT => self.state = ParseState::Negotiation(byte),
                    SB => {
                        self.sub.clear();
                        self.state = ParseState::SubData;
                    }
                    // 其余单字节命令（GA/NOP/DM...）对终端没有意义，直接吞掉
                    _ => self.state = ParseState::Data,
                },
                ParseState::Negotiation(command) => {
                    self.state = ParseState::Data;
                    match command {
                        WILL => self.on_will(byte, &mut reply),
                        WONT => self.on_wont(byte),
                        DO => self.on_do(byte, &mut reply),
                        DONT => self.on_dont(byte),
                        _ => {}
                    }
                }
                ParseState::SubData => {
                    if byte == IAC {
                        self.state = ParseState::SubIac;
                    } else if self.sub.len() < SUB_MAX {
                        self.sub.push(byte);
                    }
                }
                ParseState::SubIac => match byte {
                    SE => {
                        self.state = ParseState::Data;
                        self.finish_subnegotiation(&mut reply);
                    }
                    // 子协商里的 0xFF 同样是双写转义
                    IAC => {
                        if self.sub.len() < SUB_MAX {
                            self.sub.push(IAC);
                        }
                        self.state = ParseState::SubData;
                    }
                    // 协议异常：丢掉这个命令字节，继续收集
                    _ => self.state = ParseState::SubData,
                },
            }
        }

        (data, reply)
    }

    /// 更新本地记录的窗口尺寸，并在 NAWS 已协商时产出要发送的子协商。
    ///
    /// 参数：cols/rows 为前端新尺寸
    /// 核心流程：先无条件记下尺寸（协商还没完成时它就是稍后要补发的值），
    /// 再按 NAWS 是否协商成功决定要不要立刻回报。返回空表示「只更新了本地值」。
    pub fn resize(&mut self, cols: u16, rows: u16) -> Vec<u8> {
        self.cols = cols;
        self.rows = rows;
        if !self.naws {
            return Vec::new();
        }
        let mut out = Vec::new();
        push_naws(&mut out, cols, rows);
        out
    }

    /// 对端 `WILL option`：它愿意自己开启某个选项。
    fn on_will(&mut self, option: u8, reply: &mut Vec<u8>) {
        if self.remote.contains_key(&option) {
            return;
        }
        // 只接受 ECHO/SGA 由对端开启；其余（含 NAWS/TTYPE）都拒绝
        if option == OPT_ECHO || option == OPT_SGA {
            self.remote.insert(option, true);
            push_command(reply, DO, option);
        } else {
            self.remote.insert(option, false);
            push_command(reply, DONT, option);
        }
    }

    /// 对端 `DO option`：它要求我们开启某个选项。
    fn on_do(&mut self, option: u8, reply: &mut Vec<u8>) {
        if !self.local.contains_key(&option) {
            if is_locally_supported(option) {
                self.local.insert(option, true);
                push_command(reply, WILL, option);
            } else {
                self.local.insert(option, false);
                push_command(reply, WONT, option);
                return;
            }
        }
        // 我们已经开口 WILL、对端也 DO 了 -> 协商完成，这时才能发 NAWS 子协商
        if option == OPT_NAWS && !self.naws && self.local.get(&option) == Some(&true) {
            self.naws = true;
            push_naws(reply, self.cols, self.rows);
        }
    }

    /// 对端 `WONT option`：它不再开启某选项。
    ///
    /// 不需要应答，但要把表态清掉，否则它回心转意时我们会一直沉默。
    fn on_wont(&mut self, option: u8) {
        self.remote.remove(&option);
    }

    /// 对端 `DONT option`：它要求我们关闭某选项。
    fn on_dont(&mut self, option: u8) {
        self.local.remove(&option);
        if option == OPT_NAWS {
            self.naws = false;
        }
    }

    /// 处理一个完整的 `SB ... SE`。
    fn finish_subnegotiation(&mut self, reply: &mut Vec<u8>) {
        match self.sub.first() {
            // 对端问终端类型 -> 回 IS <term>
            Some(&OPT_TTYPE) if self.sub.get(1) == Some(&TTYPE_SEND) => {
                reply.extend_from_slice(&[IAC, SB, OPT_TTYPE, TTYPE_IS]);
                reply.extend_from_slice(self.term.as_bytes());
                reply.extend_from_slice(&[IAC, SE]);
            }
            // 对端发 NAWS 是异常（方向反了），其余子协商我们不参与 -> 整体丢弃
            _ => {}
        }
        self.sub.clear();
    }
}

/// 把用户输入里的 `0xFF` 双写。
///
/// 不双写的话，服务器会把数据里的 `0xFF` 当成命令引导符，导致后续字节被吞。
pub fn escape_iac(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 8);
    for &byte in data {
        out.push(byte);
        if byte == IAC {
            out.push(IAC);
        }
    }
    out
}

/// 我们允许对端要求本端开启的选项。
fn is_locally_supported(option: u8) -> bool {
    matches!(option, OPT_ECHO | OPT_SGA | OPT_TTYPE | OPT_NAWS)
}

/// 追加 `IAC <command> <option>`。
fn push_command(out: &mut Vec<u8>, command: u8, option: u8) {
    out.extend_from_slice(&[IAC, command, option]);
}

/// 追加 `IAC SB NAWS <cols_hi> <cols_lo> <rows_hi> <rows_lo> IAC SE`。
fn push_naws(out: &mut Vec<u8>, cols: u16, rows: u16) {
    let [cols_hi, cols_lo] = cols.to_be_bytes();
    let [rows_hi, rows_lo] = rows.to_be_bytes();
    out.extend_from_slice(&[
        IAC, SB, OPT_NAWS, cols_hi, cols_lo, rows_hi, rows_lo, IAC, SE,
    ]);
}

/// 一个运行中的 Telnet 会话。
///
/// 无参数
/// 核心流程：写入端与解析器都被读线程共享（协商应答要回写 socket、resize 要
/// 改解析器里的尺寸），读端交给独立阻塞线程灌进与本地 PTY 共用的输出泵，
/// 因此上层不需要区分底层是进程还是 socket。
pub struct TelnetSession {
    /// 与读线程共享：用户数据与协商应答都从这里出去，靠互斥锁保证不交错。
    writer: Arc<Mutex<TcpStream>>,
    /// 与读线程共享：resize 要改它的尺寸，读线程要喂它字节。
    parser: Arc<Mutex<TelnetParser>>,
    /// 只用于 close：shutdown 之后阻塞中的读线程会立刻读到 EOF 并收尾输出泵。
    handle: TcpStream,
    closed: AtomicBool,
}

impl TelnetSession {
    /// 建立一条 Telnet 连接。
    ///
    /// 参数：options 为地址、终端类型与初始尺寸，sink 为输出回调
    /// 核心流程：解析地址 → 逐个地址带超时连接 → 主动发 WILL NAWS/WILL TTYPE →
    /// 起阻塞读线程（解析后分别送输出泵与 socket）→ 连接断开时回报 Exit(None)。
    ///
    /// 这里的连接是纯阻塞调用，必须由调用方放在阻塞上下文里（命令层用
    /// `spawn_blocking`）；不做 `Handle::try_current()` 之类的运行时探测，
    /// 因为 `spawn_blocking` 的线程身处运行时上下文，探测会把阻塞调用误判成异步。
    pub fn connect(options: &TelnetOptions, sink: OutputSink) -> CoreResult<Self> {
        let host = options.host.trim().to_string();
        if host.is_empty() {
            return Err(CoreError::Terminal("Telnet 主机不能为空".into()));
        }

        let addrs = (host.as_str(), options.port)
            .to_socket_addrs()
            .map_err(|err| {
                CoreError::Terminal(format!("解析主机 {host}:{} 失败：{err}", options.port))
            })?;

        let mut stream = None;
        let mut last_error = None;
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
                Ok(connected) => {
                    stream = Some(connected);
                    break;
                }
                Err(err) => last_error = Some(err),
            }
        }
        let stream = match (stream, last_error) {
            (Some(stream), _) => stream,
            (None, Some(err)) => {
                return Err(CoreError::Terminal(format!(
                    "连接 {host}:{} 失败：{err}",
                    options.port
                )))
            }
            (None, None) => {
                return Err(CoreError::Terminal(format!(
                    "连接 {host}:{} 失败：主机没有可用地址",
                    options.port
                )))
            }
        };

        // 交互式终端都是小包 -> 关掉 Nagle，避免按键被攒包延迟
        let _ = stream.set_nodelay(true);

        let mut reader = stream
            .try_clone()
            .map_err(|err| CoreError::Terminal(format!("复制读取句柄失败：{err}")))?;
        let handle = stream
            .try_clone()
            .map_err(|err| CoreError::Terminal(format!("复制关闭句柄失败：{err}")))?;

        let mut parser = TelnetParser::new(&options.term, options.cols, options.rows);
        let greeting = parser.initial();
        let parser = Arc::new(Mutex::new(parser));
        let writer = Arc::new(Mutex::new(stream));

        // 首包必须在读线程启动前发完：这样它一定排在任何协商应答之前
        {
            let mut guard = writer
                .lock()
                .map_err(|_| CoreError::Terminal("写入端已失效".into()))?;
            guard
                .write_all(&greeting)
                .and_then(|()| guard.flush())
                .map_err(|err| CoreError::Terminal(format!("发送协商首包失败：{err}")))?;
        }

        let pump = start_output_pump(sink)?;
        let reader_pump = Arc::clone(&pump);
        let reader_parser = Arc::clone(&parser);
        let reader_writer = Arc::clone(&writer);

        // 读线程 -> 解析出普通数据送输出泵、协商应答回 socket，不直接调用上层
        std::thread::Builder::new()
            .name("rustmobax-telnet-read".into())
            .spawn(move || {
                let mut buffer = [0u8; READ_CHUNK];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => {
                            let parsed = match reader_parser.lock() {
                                Ok(mut guard) => guard.feed(&buffer[..read]),
                                Err(_) => break,
                            };
                            let (data, reply) = parsed;
                            if !reply.is_empty() {
                                if let Ok(mut out) = reader_writer.lock() {
                                    let _ = out.write_all(&reply);
                                    let _ = out.flush();
                                }
                            }
                            if !data.is_empty() {
                                reader_pump.push(&data);
                            }
                        }
                        // 被信号打断不算结束 -> 继续读
                        Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                // Telnet 没有退出码概念 -> 统一回报 None
                reader_pump.close_with(None);
            })
            .map_err(|err| CoreError::Terminal(format!("启动读取线程失败：{err}")))?;

        Ok(Self {
            writer,
            parser,
            handle,
            closed: AtomicBool::new(false),
        })
    }

    /// 把用户输入写进 socket。
    ///
    /// 参数：data 为原始字节
    /// 核心流程：先把每个 `0xFF` 双写成 `0xFF 0xFF`（不转义的话服务器会把用户
    /// 数据当成命令），再写入并 flush。
    pub fn write(&self, data: &[u8]) -> CoreResult<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(CoreError::Terminal("Telnet 连接已关闭".into()));
        }
        self.send_raw(&escape_iac(data))
    }

    /// 调整远端窗口尺寸。
    ///
    /// 参数：cols/rows 为前端新尺寸
    /// 核心流程：交给解析器记录；NAWS 已协商成功就立刻发新的尺寸子协商，
    /// 否则只更新本地值，等 `DO NAWS` 到达时由解析器把最新尺寸补发出去。
    pub fn resize(&self, cols: u16, rows: u16) -> CoreResult<()> {
        let update = {
            let mut parser = self
                .parser
                .lock()
                .map_err(|_| CoreError::Terminal("协商状态已失效".into()))?;
            parser.resize(cols, rows)
        };
        if update.is_empty() {
            return Ok(());
        }
        self.send_raw(&update)
    }

    /// 关闭连接。
    ///
    /// 参数：无
    /// 核心流程：置关闭位 → shutdown 两端；读线程随之读到 EOF 并回报退出事件。
    /// 连接已经断开时不算错误（重复 close 是安全的）。
    pub fn close(&self) -> CoreResult<()> {
        self.closed.store(true, Ordering::SeqCst);
        match self.handle.shutdown(Shutdown::Both) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotConnected => Ok(()),
            Err(err) => Err(CoreError::Terminal(format!("关闭连接失败：{err}"))),
        }
    }

    /// 把已经编码好的字节写进 socket（不做任何转义）。
    ///
    /// 协商应答和用户数据共用同一把锁，保证两边的字节流不会交错。
    fn send_raw(&self, bytes: &[u8]) -> CoreResult<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| CoreError::Terminal("写入端已失效".into()))?;
        writer
            .write_all(bytes)
            .and_then(|()| writer.flush())
            .map_err(|err| CoreError::Terminal(format!("写入失败：{err}")))
    }
}

impl TerminalControl for TelnetSession {
    /// 转发写入。
    fn write(&self, data: &[u8]) -> CoreResult<()> {
        TelnetSession::write(self, data)
    }

    /// 转发尺寸变化。
    fn resize(&self, cols: u16, rows: u16) -> CoreResult<()> {
        TelnetSession::resize(self, cols, rows)
    }

    /// 关闭即断开 socket。
    fn close(&self) -> CoreResult<()> {
        TelnetSession::close(self)
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::atomic::AtomicBool;
    use std::sync::{mpsc, Arc};
    use std::time::Instant;

    use crate::terminal::TerminalOutput;

    use super::*;

    /// 连接建立后应当主动发出的首包。
    const GREETING: [u8; 6] = [IAC, WILL, OPT_NAWS, IAC, WILL, OPT_TTYPE];

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

        /// 构造一个绑定到本探针的回调。
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

        fn exited(&self) -> bool {
            self.exited.load(Ordering::SeqCst)
        }
    }

    /// 起一个只听 127.0.0.1 的假 telnet 服务端，返回监听器与端口。
    fn listen() -> (TcpListener, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定测试端口失败");
        let port = listener.local_addr().expect("读取本地地址失败").port();
        (listener, port)
    }

    fn options_for(port: u16) -> TelnetOptions {
        TelnetOptions {
            host: "127.0.0.1".to_string(),
            port,
            ..Default::default()
        }
    }

    /// 一直读到条件成立（或超时）为止，返回读到的全部字节。
    ///
    /// 带超时是必须的：否则断言条件永远不成立时测试会挂死而不是失败。
    fn read_until(stream: &mut TcpStream, mut done: impl FnMut(&[u8]) -> bool) -> Vec<u8> {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("设置读超时失败");
        let mut received = Vec::new();
        let mut buffer = [0u8; 256];
        while !done(&received) {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => received.extend_from_slice(&buffer[..read]),
                Err(_) => break,
            }
        }
        received
    }

    /// 轮询等待某个条件成立，超时返回 false。
    ///
    /// 输出泵按 8ms 聚合交付，所以断言不能立刻读，必须等它冲出来。
    fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if condition() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn new_parser() -> TelnetParser {
        TelnetParser::new("xterm-256color", 80, 24)
    }

    // ---------------- 纯解析器用例 ----------------

    #[test]
    fn passes_plain_data_through_untouched() {
        let mut parser = new_parser();
        let text = "你好，world\r\n".as_bytes();

        let (data, reply) = parser.feed(text);

        assert_eq!(data, text.to_vec(), "普通数据必须原样透传，含中文 UTF-8");
        assert!(reply.is_empty(), "普通数据不应产生任何协商应答");
    }

    #[test]
    fn reassembles_negotiation_split_across_reads() {
        let mut whole = new_parser();
        let (whole_data, whole_reply) = whole.feed(&[IAC, WILL, OPT_TTYPE]);

        let mut split = new_parser();
        let (first_data, first_reply) = split.feed(&[IAC]);
        let (second_data, second_reply) = split.feed(&[WILL, OPT_TTYPE]);

        assert!(first_data.is_empty() && first_reply.is_empty(), "半个序列不应产出任何东西");
        assert_eq!(second_data, whole_data, "分片不应影响数据结果");
        assert_eq!(
            [first_reply, second_reply].concat(),
            whole_reply,
            "分片不应影响应答结果"
        );
        assert_eq!(whole_reply, vec![IAC, DONT, OPT_TTYPE], "服务器不能替本端开 TTYPE");
    }

    #[test]
    fn answers_will_echo_with_do() {
        let mut parser = new_parser();
        let (data, reply) = parser.feed(&[IAC, WILL, OPT_ECHO]);

        assert!(data.is_empty());
        assert_eq!(reply, vec![IAC, DO, OPT_ECHO]);
    }

    #[test]
    fn answers_do_sga_with_will() {
        let mut parser = new_parser();
        let (data, reply) = parser.feed(&[IAC, DO, OPT_SGA]);

        assert!(data.is_empty());
        assert_eq!(reply, vec![IAC, WILL, OPT_SGA]);
    }

    #[test]
    fn refuses_unknown_will_option() {
        let mut parser = new_parser();
        let (_, reply) = parser.feed(&[IAC, WILL, 99]);

        assert_eq!(reply, vec![IAC, DONT, 99], "未知选项必须被拒绝");
    }

    #[test]
    fn refuses_unknown_do_option() {
        let mut parser = new_parser();
        let (_, reply) = parser.feed(&[IAC, DO, 99]);

        assert_eq!(reply, vec![IAC, WONT, 99], "未知选项必须被拒绝");
    }

    #[test]
    fn does_not_answer_the_same_option_twice() {
        let mut parser = new_parser();
        let (_, first) = parser.feed(&[IAC, WILL, OPT_ECHO]);
        let (_, second) = parser.feed(&[IAC, WILL, OPT_ECHO]);
        let (_, third) = parser.feed(&[IAC, WILL, OPT_ECHO]);

        assert_eq!(first, vec![IAC, DO, OPT_ECHO]);
        assert!(second.is_empty(), "重复的 WILL 不应重复应答");
        assert!(third.is_empty(), "重复的 WILL 不应重复应答");
    }

    #[test]
    fn answers_terminal_type_send() {
        let mut parser = new_parser();
        let (data, reply) = parser.feed(&[IAC, SB, OPT_TTYPE, TTYPE_SEND, IAC, SE]);

        assert!(data.is_empty());
        let mut expected = vec![IAC, SB, OPT_TTYPE, TTYPE_IS];
        expected.extend_from_slice(b"xterm-256color");
        expected.extend_from_slice(&[IAC, SE]);
        assert_eq!(reply, expected);
    }

    #[test]
    fn decodes_escaped_iac_in_data() {
        let mut parser = new_parser();
        let (data, reply) = parser.feed(&[b'a', IAC, IAC, b'b']);
        assert_eq!(data, vec![b'a', IAC, b'b'], "0xFF 0xFF 应解码成单个 0xFF");
        assert!(reply.is_empty());

        // 被拆开到达时同样要能解码
        let mut split = new_parser();
        let (head, _) = split.feed(&[b'a', IAC]);
        let (tail, _) = split.feed(&[IAC, b'b']);
        assert_eq!([head, tail].concat(), vec![b'a', IAC, b'b']);
    }

    #[test]
    fn escapes_iac_in_user_input() {
        assert_eq!(
            escape_iac(&[b'a', IAC, b'b']),
            vec![b'a', IAC, IAC, b'b'],
            "用户数据里的 0xFF 必须双写"
        );
        assert_eq!(escape_iac(b"cmd\r\n"), b"cmd\r\n".to_vec(), "无 0xFF 时不应变形");
    }

    #[test]
    fn keeps_window_size_until_naws_is_agreed() {
        let mut parser = new_parser();
        let greeting = parser.initial();
        assert_eq!(greeting, GREETING.to_vec());

        // 只发了 WILL，对端还没 DO -> resize 不产生任何待发字节
        assert!(parser.resize(100, 40).is_empty(), "NAWS 未协商时不应提前发子协商");

        // 对端 DO NAWS -> 补发的是最后一次 resize 的尺寸
        let (_, reply) = parser.feed(&[IAC, DO, OPT_NAWS]);
        assert_eq!(reply, vec![IAC, SB, OPT_NAWS, 0, 100, 0, 40, IAC, SE]);

        // 协商完成之后每次 resize 都立刻上报
        let update = parser.resize(120, 30);
        assert_eq!(update, vec![IAC, SB, OPT_NAWS, 0, 120, 0, 30, IAC, SE]);
    }

    #[test]
    fn ignores_naws_subnegotiation_from_peer() {
        let mut parser = new_parser();
        let (data, reply) = parser.feed(&[IAC, SB, OPT_NAWS, 0, 80, 0, 24, IAC, SE]);

        assert!(data.is_empty() && reply.is_empty(), "对端发 NAWS 属于异常，应整体忽略");
    }

    // ---------------- 真实 socket 用例 ----------------

    #[test]
    fn delivers_bytes_from_server_to_sink() {
        let (listener, port) = listen();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("服务端应接受连接");
            stream.write_all(b"hello\r\n").expect("服务端发送失败");
            // 保持连接直到客户端主动关掉 -> 否则会话会先看到 EOF
            let mut buffer = [0u8; 64];
            let _ = stream.read(&mut buffer);
        });

        let probe = Probe::new();
        let session = TelnetSession::connect(&options_for(port), probe.sink()).expect("连接应成功");

        assert!(
            wait_until(|| !probe.bytes().is_empty()),
            "5 秒内没收到服务端数据"
        );
        assert_eq!(probe.bytes(), b"hello\r\n".to_vec());

        session.close().expect("关闭应成功");
        server.join().expect("服务端线程异常");
    }

    #[test]
    fn strips_negotiation_from_server_output() {
        let (listener, port) = listen();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("服务端应接受连接");
            // 先来一串协商，再发真正的终端输出
            stream
                .write_all(&[IAC, WILL, OPT_ECHO, IAC, WILL, OPT_SGA])
                .expect("服务端发送失败");
            stream.write_all(b"ready\r\n").expect("服务端发送失败");
            let mut buffer = [0u8; 64];
            let _ = stream.read(&mut buffer);
        });

        let probe = Probe::new();
        let session = TelnetSession::connect(&options_for(port), probe.sink()).expect("连接应成功");

        assert!(
            wait_until(|| probe.bytes().ends_with(b"ready\r\n")),
            "5 秒内没收到服务端数据"
        );
        assert_eq!(probe.bytes(), b"ready\r\n".to_vec(), "0xFF 字节不能漏进终端");

        session.close().expect("关闭应成功");
        server.join().expect("服务端线程异常");
    }

    #[test]
    fn writes_user_input_to_server() {
        let (listener, port) = listen();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("服务端应接受连接");
            read_until(&mut stream, |seen| seen.ends_with(b"cmd\r\n"))
        });

        let probe = Probe::new();
        let session = TelnetSession::connect(&options_for(port), probe.sink()).expect("连接应成功");
        session.write(b"cmd\r\n").expect("写入应成功");

        let received = server.join().expect("服务端线程异常");
        assert_eq!(
            &received[..GREETING.len()],
            &GREETING,
            "连接建立后应先发协商首包"
        );
        assert_eq!(&received[GREETING.len()..], b"cmd\r\n", "用户输入应原样送达");

        session.close().expect("关闭应成功");
    }

    #[test]
    fn doubles_iac_in_user_input_on_the_wire() {
        let (listener, port) = listen();
        let payload = vec![b'a', IAC, b'b'];
        let expected: Vec<u8> = {
            let mut wire = GREETING.to_vec();
            wire.extend_from_slice(&[b'a', IAC, IAC, b'b']);
            wire
        };
        let want = expected.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("服务端应接受连接");
            read_until(&mut stream, |seen| seen.len() >= want.len())
        });

        let probe = Probe::new();
        let session = TelnetSession::connect(&options_for(port), probe.sink()).expect("连接应成功");
        session.write(&payload).expect("写入应成功");

        assert_eq!(
            server.join().expect("服务端线程异常"),
            expected,
            "用户数据里的 0xFF 必须双写后再上线"
        );
        session.close().expect("关闭应成功");
    }

    #[test]
    fn reports_window_size_after_naws_is_agreed() {
        let (listener, port) = listen();
        let (ready, wait) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("服务端应接受连接");
            stream
                .write_all(&[IAC, DO, OPT_NAWS])
                .expect("服务端发送失败");
            let initial = read_until(&mut stream, |seen| {
                seen.ends_with(&[IAC, SB, OPT_NAWS, 0, 80, 0, 24, IAC, SE])
            });
            assert!(
                initial.ends_with(&[IAC, SB, OPT_NAWS, 0, 80, 0, 24, IAC, SE]),
                "收到 DO NAWS 后应立刻上报初始尺寸，实际为：{initial:?}"
            );
            let _ = ready.send(());
            let resized = read_until(&mut stream, |seen| {
                seen.ends_with(&[IAC, SB, OPT_NAWS, 0, 100, 0, 40, IAC, SE])
            });
            assert!(
                resized.ends_with(&[IAC, SB, OPT_NAWS, 0, 100, 0, 40, IAC, SE]),
                "协商完成后 resize 应立即上报新尺寸，实际为：{resized:?}"
            );
        });

        let probe = Probe::new();
        let session = TelnetSession::connect(&options_for(port), probe.sink()).expect("连接应成功");
        wait.recv_timeout(Duration::from_secs(5))
            .expect("5 秒内没等到初始窗口尺寸");
        session.resize(100, 40).expect("调整尺寸应成功");

        server.join().expect("服务端线程异常");
        session.close().expect("关闭应成功");
    }

    #[test]
    fn close_disconnects_and_reports_exit() {
        let (listener, port) = listen();
        let (sender, receiver) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("服务端应接受连接");
            // 先把协商首包读掉，后面的 EOF 才是我们要观察的信号
            read_until(&mut stream, |seen| seen.len() >= GREETING.len());
            let mut buffer = [0u8; 64];
            // 客户端 close 之后这里应当读到 0（EOF）而不是继续阻塞
            let read = stream.read(&mut buffer).expect("服务端读取失败");
            let _ = sender.send(read);
        });

        let probe = Probe::new();
        let session = TelnetSession::connect(&options_for(port), probe.sink()).expect("连接应成功");
        session.close().expect("关闭应成功");

        let read = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("5 秒内没等到服务端读到 EOF");
        assert_eq!(read, 0, "关闭后服务端应读到 EOF");
        assert!(wait_until(|| probe.exited()), "关闭后应收到一次 Exit 事件");
        server.join().expect("服务端线程异常");
    }

    #[test]
    fn reports_readable_error_when_nothing_listens() {
        let (listener, port) = listen();
        // 端口拿到就立刻释放 -> 之后连接会被拒绝
        drop(listener);

        let probe = Probe::new();
        let result = TelnetSession::connect(&options_for(port), probe.sink());
        let error = result.err().expect("连接未监听端口应当失败");
        let message = error.to_string();

        assert!(
            message.contains("连接") && message.contains(&port.to_string()),
            "错误信息应说明连不上哪个地址，实际为：{message}"
        );
    }
}
