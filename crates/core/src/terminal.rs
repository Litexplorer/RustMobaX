use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use crate::error::{CoreError, CoreResult};

/// 单次从 PTY 读取的最大字节数。
const READ_CHUNK: usize = 8192;

/// 冲刷间隔：把这段时间内攒下的数据合成一次交付。
const FLUSH_INTERVAL: Duration = Duration::from_millis(8);

/// 从终端推给上层的消息。
///
/// 无参数
/// 核心流程：数据与退出事件走同一条通道，上层按变体分别处理，避免用 EOF 猜测进程状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalOutput {
    /// 一段原始字节，可能包含控制序列。
    Data(Vec<u8>),
    /// 进程或连接结束，附带退出码（拿不到时为 None）。
    Exit(Option<i32>),
}

/// 输出回调。
///
/// 无参数
/// 核心流程：冲刷线程直接调用，命令层在里面把数据转发给前端，因此不需要额外的异步通道。
pub type OutputSink = Box<dyn Fn(TerminalOutput) + Send + Sync + 'static>;

/// 终端的统一控制面。
///
/// 无参数
/// 核心流程：本地 PTY 与 SSH 会话都实现这一组动作，命令层只认 trait，
/// 不关心底层是进程还是网络连接。
pub trait TerminalControl: Send + Sync {
    /// 写入用户输入。
    fn write(&self, data: &[u8]) -> CoreResult<()>;
    /// 调整终端尺寸。
    fn resize(&self, cols: u16, rows: u16) -> CoreResult<()>;
    /// 关闭会话。
    fn close(&self) -> CoreResult<()>;
    /// 取底层 SSH 连接。
    ///
    /// 参数：无
    /// 核心流程：默认返回 None（本地 PTY 没有远端连接），
    /// SSH 会话覆写为返回自己的连接，远端指标据此区分本机与远端口径。
    fn connection(&self) -> Option<&crate::ssh::SshConnection> {
        None
    }
    /// 取当前工作目录。
    ///
    /// 参数：无
    /// 核心流程：默认返回 None —— SSH 会话的目录在远端，由远端 shell
    /// 自己用 OSC 7 上报；本地 PTY 覆写为读子进程的 cwd。
    /// 拿不到一律回 None，让上层静默跳过，而不是把它变成一次连接错误。
    fn cwd(&self) -> Option<String> {
        None
    }
}

/// 输出泵：读端与交付端之间的缓冲。
///
/// 无参数
/// 核心流程：读端只往里塞，后台每 8ms 整块取走，
/// 用「按时间聚合」换掉「每 8KB 推一次」，避免高频输出把上层队列堆爆。
pub(crate) struct Pump {
    buffer: Mutex<Vec<u8>>,
    closed: AtomicBool,
    exit_code: Mutex<Option<Option<i32>>>,
}

impl Default for Pump {
    fn default() -> Self {
        Self {
            buffer: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
            exit_code: Mutex::new(None),
        }
    }
}

impl Pump {
    /// 追加新读到的字节。
    ///
    /// 参数：data 为原始字节
    /// 核心流程：加锁追加；锁被占用时丢弃本轮，后续读取会补上。
    pub(crate) fn push(&self, data: &[u8]) {
        if let Ok(mut guard) = self.buffer.lock() {
            guard.extend_from_slice(data);
        }
    }

    /// 取走当前积累的全部字节。
    ///
    /// 参数：无
    /// 核心流程：缓冲为空返回 None，否则整块取走并清空。
    fn take(&self) -> Option<Vec<u8>> {
        let mut guard = self.buffer.lock().ok()?;
        if guard.is_empty() {
            return None;
        }
        Some(std::mem::take(&mut *guard))
    }

    /// 标记结束并记录退出码。
    ///
    /// 参数：code 为退出码，None 表示拿不到
    /// 核心流程：先落退出码再置关闭位，保证冲刷线程读到的是最终值。
    pub(crate) fn close_with(&self, code: Option<i32>) {
        if let Ok(mut guard) = self.exit_code.lock() {
            *guard = Some(code);
        }
        self.closed.store(true, Ordering::SeqCst);
    }

    /// 读端是否已关闭。
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// 取记录的退出码。
    fn exit_code(&self) -> Option<i32> {
        self.exit_code
            .lock()
            .ok()
            .and_then(|guard| guard.flatten())
    }
}

/// 启动输出泵。
///
/// 参数：sink 为上层回调
/// 核心流程：建 Pump → 起一个 8ms 定时线程整块交付 → 读端关闭后先排空缓冲，
/// 再回调一次 Exit，确保数据永远排在退出事件之前。
pub(crate) fn start_output_pump(sink: OutputSink) -> CoreResult<Arc<Pump>> {
    let pump = Arc::new(Pump::default());
    let flusher = Arc::clone(&pump);

    std::thread::Builder::new()
        .name("rustmobax-term-flush".into())
        .spawn(move || {
            loop {
                std::thread::sleep(FLUSH_INTERVAL);
                if let Some(chunk) = flusher.take() {
                    sink(TerminalOutput::Data(chunk));
                    continue;
                }
                if flusher.is_closed() {
                    // 关闭瞬间可能还有残留 -> 再取一次再收尾
                    if let Some(chunk) = flusher.take() {
                        sink(TerminalOutput::Data(chunk));
                    }
                    break;
                }
            }
            sink(TerminalOutput::Exit(flusher.exit_code()));
        })
        .map_err(|err| CoreError::Terminal(format!("启动冲刷线程失败：{err}")))?;

    Ok(pump)
}

/// 启动本地终端所需的参数。
///
/// 无参数
/// 核心流程：只保留壳程序、工作目录、TERM 与初始尺寸，其余走继承的环境变量。
#[derive(Debug, Clone)]
pub struct TerminalOptions {
    pub shell: Option<String>,
    /// 传给壳程序的参数。
    ///
    /// 默认空 -> 现有调用点行为完全不变；
    /// WSL 终端靠它把 `wsl.exe -d <发行版> --cd ~` 拼成一个本地进程。
    pub args: Vec<String>,
    pub cwd: Option<String>,
    /// 传给子进程的 TERM，决定终端能力协商。
    pub term: String,
    pub cols: u16,
    pub rows: u16,
}

impl Default for TerminalOptions {
    fn default() -> Self {
        Self {
            shell: None,
            args: Vec::new(),
            cwd: None,
            term: "xterm-256color".to_string(),
            cols: 80,
            rows: 24,
        }
    }
}

/// 一个运行中的本地终端。
///
/// 无参数
/// 核心流程：持有写入端、主端点与子进程，读端交给独立线程塞进输出泵。
pub struct TerminalSession {
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    /// 子进程 pid，拿得到才记录 -> 供工作目录查询按 pid 反查
    pid: Option<u32>,
}

impl TerminalSession {
    /// 启动一个本地 shell。
    ///
    /// 参数：options 为壳程序与尺寸，sink 为输出回调
    /// 核心流程：开 PTY → 生成子进程 → 释放从端避免占位 → 拆出读写端 →
    /// 后台线程持续读取灌进输出泵，读到 EOF 时回报退出码。
    pub fn spawn(options: &TerminalOptions, sink: OutputSink) -> CoreResult<Self> {
        let pty_system = native_pty_system();
        let size = PtySize {
            rows: options.rows.max(1),
            cols: options.cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        };

        // 开一对 PTY -> 拿到主端与从端
        let pair = pty_system
            .openpty(size)
            .map_err(|err| CoreError::Terminal(format!("创建 PTY 失败：{err}")))?;

        let shell = options
            .shell
            .clone()
            .unwrap_or_else(|| default_shell().to_string());
        let mut command = CommandBuilder::new(shell);
        // 追加壳程序参数 -> WSL 之类的场景用参数指定目标发行版；
        // 不带参数时循环不执行，与改动前完全一致
        for arg in &options.args {
            command.arg(arg);
        }
        if let Some(cwd) = options.cwd.clone().or_else(default_cwd) {
            command.cwd(cwd);
        }
        // 显式声明终端能力 -> 否则 less/vim 会因 TERM 为空而降级成 dumb 终端
        command.env("TERM", options.term.clone());
        command.env("COLORTERM", "truecolor");

        // 生成子进程 -> 进入交互式 shell
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|err| CoreError::Terminal(format!("启动 shell 失败：{err}")))?;
        // pid 要在包进 Arc 之前取 -> 之后只靠它查 /proc/<pid>/cwd
        let pid = child.process_id();

        // 释放从端 -> 否则主端读不到 EOF
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| CoreError::Terminal(format!("获取读取端失败：{err}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|err| CoreError::Terminal(format!("获取写入端失败：{err}")))?;

        let child = Arc::new(Mutex::new(child));
        let watcher = Arc::clone(&child);
        let pump = start_output_pump(sink)?;
        let reader_pump = Arc::clone(&pump);

        // 读线程 -> 只把字节搬进输出泵，不直接调用上层
        std::thread::Builder::new()
            .name("rustmobax-pty-read".into())
            .spawn(move || {
                let mut buffer = [0u8; READ_CHUNK];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => reader_pump.push(&buffer[..read]),
                        Err(_) => break,
                    }
                }
                let code = watcher
                    .lock()
                    .ok()
                    .and_then(|mut handle| handle.try_wait().ok().flatten())
                    .and_then(|status| i32::try_from(status.exit_code()).ok());
                reader_pump.close_with(code);
            })
            .map_err(|err| CoreError::Terminal(format!("启动读取线程失败：{err}")))?;

        Ok(Self {
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            child,
            pid,
        })
    }

    /// 把用户输入写进 PTY。
    ///
    /// 参数：data 为原始字节
    /// 核心流程：加锁写入并立即 flush，保证按键即时到达 shell。
    pub fn write(&self, data: &[u8]) -> CoreResult<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| CoreError::Terminal("写入端已失效".into()))?;
        writer
            .write_all(data)
            .map_err(|err| CoreError::Terminal(format!("写入失败：{err}")))?;
        writer
            .flush()
            .map_err(|err| CoreError::Terminal(format!("刷新失败：{err}")))?;
        Ok(())
    }

    /// 调整终端尺寸。
    ///
    /// 参数：cols 列数，rows 行数
    /// 核心流程：把新尺寸交给主端点，内核会给子进程发 SIGWINCH。
    pub fn resize(&self, cols: u16, rows: u16) -> CoreResult<()> {
        let master = self
            .master
            .lock()
            .map_err(|_| CoreError::Terminal("主端点已失效".into()))?;
        master
            .resize(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| CoreError::Terminal(format!("调整尺寸失败：{err}")))?;
        Ok(())
    }

    /// 读取子进程当前的工作目录。
    ///
    /// 参数：无
    /// 核心流程：拿 spawn 时记下的 pid 去问内核（Linux 读 /proc/<pid>/cwd），
    /// 进程已退出、目录已消失或平台不支持时返回 None，不向上报错。
    pub fn cwd(&self) -> Option<String> {
        read_process_cwd(self.pid?)
    }

    /// 结束子进程。
    ///
    /// 参数：无
    /// 核心流程：直接杀子进程，读线程随之读到 EOF 并回报退出事件；
    /// 进程已退出时不视为错误。
    pub fn kill(&self) -> CoreResult<()> {
        let mut child = self
            .child
            .lock()
            .map_err(|_| CoreError::Terminal("进程句柄已失效".into()))?;
        if child.try_wait().ok().flatten().is_some() {
            return Ok(());
        }
        child
            .kill()
            .map_err(|err| CoreError::Terminal(format!("结束进程失败：{err}")))?;
        Ok(())
    }
}

impl TerminalControl for TerminalSession {
    /// 转发写入。
    fn write(&self, data: &[u8]) -> CoreResult<()> {
        TerminalSession::write(self, data)
    }

    /// 转发尺寸调整。
    fn resize(&self, cols: u16, rows: u16) -> CoreResult<()> {
        TerminalSession::resize(self, cols, rows)
    }

    /// 关闭即杀进程。
    fn close(&self) -> CoreResult<()> {
        self.kill()
    }

    /// 转发工作目录查询。
    fn cwd(&self) -> Option<String> {
        TerminalSession::cwd(self)
    }
}

/// 取默认 shell。
///
/// 参数：无
/// 核心流程：Windows 用 PowerShell，其它平台优先 $SHELL，取不到退回 /bin/bash。
pub fn default_shell() -> String {
    if cfg!(windows) {
        "powershell.exe".to_string()
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
    }
}

/// 取用户主目录。
///
/// 参数：无
/// 核心流程：按平台读 HOME / USERPROFILE，都没有就交给 shell 自己决定。
fn default_cwd() -> Option<String> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

/// 读指定进程的当前工作目录。
///
/// 参数：pid 为目标进程号
/// 核心流程：Linux 下 /proc/<pid>/cwd 是指向进程 cwd 的符号链接，
/// 直接 read_link 即可；进程不存在、目录已被删掉或平台不支持都回 None。
/// shell exec 成别的程序时 pid 不变，所以同一条路径对 bash/zsh/fish
/// 以及「bash 里 exec 出来的程序」一律成立。
#[cfg(target_os = "linux")]
fn read_process_cwd(pid: u32) -> Option<String> {
    let path = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    let text = path.to_string_lossy().into_owned();
    // 目录被删后内核会在链接末尾加 " (deleted)" -> 去掉才是可用路径
    let text = text
        .strip_suffix(" (deleted)")
        .map(str::to_string)
        .unwrap_or(text);
    if text.is_empty() {
        return None;
    }
    Some(text)
}

/// 非 Linux 平台没有统一的「按 pid 查 cwd」接口。
///
/// 参数：pid 为目标进程号
/// 核心流程：一律返回 None —— 不引入平台专有依赖，也不假装拿到了目录；
/// 这些平台的终端目录跟随因此暂不生效，但不会 panic、不会报错。
#[cfg(not(target_os = "linux"))]
fn read_process_cwd(_pid: u32) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// 收集直到出现指定文本或超时。
    ///
    /// 参数：rx 为输出通道，needle 为期望文本，timeout 为最长等待
    /// 核心流程：边收边拼，命中即返回，超时返回已累积的内容便于排查。
    fn read_until(
        rx: &mpsc::Receiver<TerminalOutput>,
        needle: &str,
        timeout: Duration,
    ) -> String {
        let mut collected = String::new();
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(TerminalOutput::Data(bytes)) => {
                    collected.push_str(&String::from_utf8_lossy(&bytes));
                    if collected.contains(needle) {
                        return collected;
                    }
                }
                Ok(TerminalOutput::Exit(_)) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        collected
    }

    #[test]
    fn spawns_shell_and_echoes_command_output() {
        let (tx, rx) = mpsc::channel();
        let session = TerminalSession::spawn(
            &TerminalOptions {
                shell: Some("/bin/sh".into()),
                cols: 100,
                rows: 30,
                ..Default::default()
            },
            Box::new(move |output| {
                let _ = tx.send(output);
            }),
        )
        .expect("PTY 应能启动");

        session.write(b"echo CRUSH_PTY_OK\n").expect("写入应成功");
        let output = read_until(&rx, "CRUSH_PTY_OK", Duration::from_secs(5));

        assert!(
            output.contains("CRUSH_PTY_OK"),
            "应能读到命令回显，实际收到：{output:?}"
        );

        session.kill().expect("关闭应成功");
    }

    #[test]
    fn reports_exit_when_process_ends() {
        let (tx, rx) = mpsc::channel();
        let session = TerminalSession::spawn(
            &TerminalOptions {
                shell: Some("/bin/sh".into()),
                cols: 80,
                rows: 24,
                ..Default::default()
            },
            Box::new(move |output| {
                let _ = tx.send(output);
            }),
        )
        .expect("PTY 应能启动");

        session.write(b"exit 0\n").expect("写入应成功");

        let mut saw_exit = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(TerminalOutput::Exit(_)) => {
                    saw_exit = true;
                    break;
                }
                Ok(TerminalOutput::Data(_)) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(saw_exit, "进程退出后应收到退出事件");
    }

    #[test]
    fn local_terminal_has_no_remote_connection() {
        let (tx, _rx) = mpsc::channel();
        let session = TerminalSession::spawn(
            &TerminalOptions {
                shell: Some("/bin/sh".into()),
                cols: 80,
                rows: 24,
                ..Default::default()
            },
            Box::new(move |output| {
                let _ = tx.send(output);
            }),
        )
        .expect("PTY 应能启动");

        // 本地 PTY 没有 SSH 连接 -> 远端指标应据此拒绝
        let control: &dyn TerminalControl = &session;
        assert!(
            control.connection().is_none(),
            "本地终端不应暴露远端连接"
        );

        session.kill().expect("关闭应成功");
    }

    /// 等 cwd 变成期望值。
    ///
    /// 参数：session 为终端会话，expected 为期望目录，timeout 为最长等待
    /// 核心流程：cd 是异步生效的（写进 PTY 后由 shell chdir），
    /// 所以轮询同一个查询接口，直到命中或超时，返回最后一次读到的值。
    #[cfg(target_os = "linux")]
    fn wait_for_cwd(session: &TerminalSession, expected: &str, timeout: Duration) -> Option<String> {
        let deadline = std::time::Instant::now() + timeout;
        let mut last = None;
        while std::time::Instant::now() < deadline {
            last = session.cwd();
            if last.as_deref() == Some(expected) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        last
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn reports_shell_working_directory_after_cd() {
        // /proc/<pid>/cwd 给的是内核口径的规范路径（软链接已被解析），
        // 所以期望值也取 canonicalize 的结果，避免撞上「/tmp 是软链」这类环境差异
        let target = std::fs::canonicalize("/usr").expect("/usr 应存在");
        let target = target.to_string_lossy().into_owned();

        let (tx, _rx) = mpsc::channel();
        let session = TerminalSession::spawn(
            &TerminalOptions {
                shell: Some("/bin/sh".into()),
                cwd: Some("/".into()),
                cols: 80,
                rows: 24,
                ..Default::default()
            },
            Box::new(move |output| {
                let _ = tx.send(output);
            }),
        )
        .expect("PTY 应能启动");

        // 起步目录来自 spawn 参数
        assert_eq!(
            wait_for_cwd(&session, "/", Duration::from_secs(3)).as_deref(),
            Some("/"),
            "shell 刚启动时应读到传入的工作目录"
        );

        session.write(b"cd /usr\n").expect("写入应成功");
        assert_eq!(
            wait_for_cwd(&session, &target, Duration::from_secs(5)).as_deref(),
            Some(target.as_str()),
            "cd 之后应读到新的工作目录"
        );

        session.kill().expect("关闭应成功");
    }

    #[test]
    fn cwd_query_returns_none_instead_of_error() {
        // 不存在的进程 -> 只能回 None，绝不能 panic 或报错
        assert_eq!(read_process_cwd(u32::MAX), None, "取不到目录时应返回 None");

        // 进程已退出后同一个查询也应安静地回 None，而不是 Err
        let (tx, _rx) = mpsc::channel();
        let session = TerminalSession::spawn(
            &TerminalOptions {
                shell: Some("/bin/sh".into()),
                cols: 80,
                rows: 24,
                ..Default::default()
            },
            Box::new(move |output| {
                let _ = tx.send(output);
            }),
        )
        .expect("PTY 应能启动");
        session.kill().expect("关闭应成功");

        // 进程刚被杀时 /proc 可能还在回收 -> 用轮询等它变成 None
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut last = Some(String::from("pending"));
        while std::time::Instant::now() < deadline {
            last = session.cwd();
            if last.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(last, None, "进程退出后应返回 None");
    }

    #[test]
    fn merges_burst_output_into_fewer_deliveries() {
        let (tx, rx) = mpsc::channel();
        let deliveries = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&deliveries);

        let session = TerminalSession::spawn(
            &TerminalOptions {
                shell: Some("/bin/sh".into()),
                cols: 120,
                rows: 40,
                ..Default::default()
            },
            Box::new(move |output| {
                if matches!(output, TerminalOutput::Data(_)) {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
                let _ = tx.send(output);
            }),
        )
        .expect("PTY 应能启动");

        // 约 288 KB 输出 -> 按 8KB 分块推应是 ~36 次，合并后应显著更少
        session
            .write(b"seq 1 50000; echo BURST_DONE\n")
            .expect("写入应成功");
        let output = read_until(&rx, "BURST_DONE", Duration::from_secs(20));

        assert!(output.contains("BURST_DONE"), "应读到结束标记");
        assert!(output.contains("50000"), "末尾数字应完整到达，未丢字节");

        let count = deliveries.load(Ordering::SeqCst);
        assert!(
            count < 25,
            "突发输出应被合并成少量推送，实际 {count} 次"
        );

        session.kill().expect("关闭应成功");
    }

    /// 默认参数的 args 必须为空 -> 现有调用点行为不受新增字段影响。
    #[test]
    fn default_options_have_no_extra_args() {
        let options = TerminalOptions::default();
        assert!(options.args.is_empty(), "默认不应给壳程序传任何参数");
    }

    /// 传入 args 时要真的落到子进程命令行上。
    ///
    /// 不依赖任何特定 shell 的交互行为：直接用 `-c` 跑一条 echo，
    /// 读到标记即说明参数确实被传下去了。
    #[test]
    #[cfg(unix)]
    fn passes_extra_args_to_child_process() {
        let (tx, rx) = mpsc::channel();
        let session = TerminalSession::spawn(
            &TerminalOptions {
                shell: Some("/bin/sh".into()),
                args: vec!["-c".into(), "echo CRUSH_ARGS_OK".into()],
                cols: 80,
                rows: 24,
                ..Default::default()
            },
            Box::new(move |output| {
                let _ = tx.send(output);
            }),
        )
        .expect("PTY 应能启动");

        let output = read_until(&rx, "CRUSH_ARGS_OK", Duration::from_secs(5));
        assert!(
            output.contains("CRUSH_ARGS_OK"),
            "参数应被传给子进程并产生输出，实际收到：{output:?}"
        );

        session.kill().expect("关闭应成功");
    }
}
