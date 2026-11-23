//! 真终端宿主：用真实的本地 PTY 与 SSH 连接应答 MCP 的终端工具。
//!
//! 这一层不依赖 Tauri，也不需要 GUI：开终端就是把会话跑在阻塞线程池里，
//! 输出进回滚缓冲，读工具按游标取走。因此无头环境与集成测试都能走完整链路。

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rustmobax_core::{
    CoreError, CoreResult, OutputSink, Session, SessionGroup, SessionInput, SshAuth, SshOptions,
    SshSession, Store, TerminalControl, TerminalOptions, TerminalOutput, TerminalRegistry,
    TerminalSession, TunnelConfig,
};

use crate::ansi::strip_ansi;
use crate::host::{
    Host, OpenedTerminal, SshAuthSpec, SshSpec, TerminalChunk, TerminalInfo, TerminalSpec,
};

/// 终端标识的序号，保证同一纳秒内开多个终端也不撞号。
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// 生成一个新的终端标识。
///
/// 无参数
/// 核心流程：时间戳纳秒加自增序号，既便于排查又保证唯一；
/// 标识只在当前进程内有意义，不落库也不跨进程复用。
fn next_terminal_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let seq = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    format!("term-{nanos}-{seq}")
}

/// 把待写入的文本按目标终端的要求归一换行。
///
/// 参数：data 为调用方传来的原始文本，windows_local 表示目标是 Windows 上的本机 PTY
/// 核心流程：非 Windows 本机终端原样返回；Windows 本机 PTY 的 ConPTY 只认回车提交，
/// 因此把孤立的 `\n`（前一个字符不是 `\r`）补成 `\r\n`，已经是 `\r\n` 的不重复处理；
/// 纯函数不判断平台，平台差异由调用方用 `cfg!(windows)` 传入。
pub fn normalize_local_input(data: &str, windows_local: bool) -> String {
    if !windows_local {
        return data.to_string();
    }
    let mut output = String::with_capacity(data.len() + 8);
    let mut previous = '\0';
    for ch in data.chars() {
        if ch == '\n' && previous != '\r' {
            output.push('\r');
        }
        output.push(ch);
        previous = ch;
    }
    output
}

/// 造一个把终端输出交付出去的闭包。
///
/// 参数：verbose 为真时把数据同时写到标准错误，exited 为退出标志
/// 核心流程：数据帧按开关写日志，退出帧只置标志；
/// 闭包被输出泵的冲刷线程直接调用，因此不能阻塞太久。
fn output_sink(verbose: bool, exited: Arc<AtomicBool>) -> OutputSink {
    Box::new(move |output| match output {
        TerminalOutput::Data(bytes) => {
            if !verbose {
                return;
            }
            let mut log = std::io::stderr();
            let _ = log.write_all(&bytes);
            let _ = log.flush();
        }
        TerminalOutput::Exit(_) => exited.store(true, Ordering::SeqCst),
    })
}

/// 把工具层的 SSH 入参转成 core 的连接参数。
///
/// 参数：spec 为工具层收到的 SSH 参数
/// 核心流程：认证方式一一对应，agent 未实现直接回校验错误；
/// 尺寸与 TERM 补上最小值与默认值，其余字段原样透传。
fn ssh_options(spec: SshSpec) -> CoreResult<SshOptions> {
    let auth = match spec.auth {
        SshAuthSpec::Password { password } => SshAuth::Password(password),
        SshAuthSpec::PrivateKey { path, passphrase } => SshAuth::PrivateKey { path, passphrase },
        SshAuthSpec::Agent => {
            return Err(CoreError::Validation(
                "当前版本不支持 agent 认证，请改用密码或私钥".into(),
            ));
        }
    };
    let term = if spec.term.trim().is_empty() {
        "xterm-256color".to_string()
    } else {
        spec.term
    };
    Ok(SshOptions {
        host: spec.host,
        port: spec.port,
        username: spec.username,
        auth,
        cols: spec.cols.max(1),
        rows: spec.rows.max(1),
        term,
        accept_unknown_host_key: spec.accept_unknown_host_key,
        jump: None,
        // 工具层没提这个开关时按开启处理，堡垒机准入才不会被静默跳过
        agent_forward: spec.agent_forward.unwrap_or(true),
    })
}

/// 带真实终端的宿主实现。
///
/// 无参数
/// 核心流程：只读能力转发给会话库；终端能力在本地 PTY 与 SSH 之间选择，
/// 会话建好后登记到注册表，读工具按游标取回滚缓冲，不需要再持有连接本身。
pub struct TerminalHost {
    store: Store,
    terminals: TerminalRegistry,
    /// 为真时把终端输出打到标准错误，默认丢弃。
    verbose: bool,
    /// 每个终端的运行期事实，开终端时登记，关终端时移除。
    facts: Mutex<HashMap<String, TerminalFacts>>,
}

/// 一个终端在宿主侧的运行期事实。
///
/// 无参数
/// 核心流程：退出标志供读取时判定进程是否已结束，尺寸与累计写入字节供资源层
/// 拼 `mobax://terminals` 的正文，三者都随终端的生命周期走。
/// 终端种类，决定写入前是否需要按平台归一换行。
///
/// 无参数
/// 核心流程：本机 PTY 与远端会话在写入语义上不同，登记时定下来，
/// 写入时只按这个字段判断，不再回头解析入参。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalKind {
    /// 本机 PTY。
    Local,
    /// 远端 SSH 会话。
    Remote,
}

impl TerminalKind {
    /// 判断是不是本机终端。
    ///
    /// 无参数
    /// 核心流程：只有本机 PTY 会受宿主平台换行差异影响，远端会话一律原样发送。
    fn is_local(self) -> bool {
        matches!(self, Self::Local)
    }
}

struct TerminalFacts {
    /// 终端种类，写入前据此决定是否归一换行。
    kind: TerminalKind,
    /// 退出标志，输出泵收到退出事件时置真。
    exited: Arc<AtomicBool>,
    /// 生效的列数。
    cols: u16,
    /// 生效的行数。
    rows: u16,
    /// 累计写入终端的字节数。
    written: u64,
}

impl TerminalHost {
    /// 用给定会话库构造宿主。
    ///
    /// 参数：store 为已打开的会话库
    /// 核心流程：终端表与事实表都为空，终端输出默认丢弃，不产生任何 IO。
    pub fn new(store: Store) -> Self {
        Self {
            store,
            terminals: TerminalRegistry::default(),
            verbose: false,
            facts: Mutex::new(HashMap::new()),
        }
    }

    /// 打开或关闭终端输出的日志开关。
    ///
    /// 参数：verbose 为真时把每段输出写到标准错误
    /// 核心流程：只改字段，构造之后随时生效，已经开着的终端也按新开关处理。
    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    /// 取注册表里一个终端，不存在时给中文未找到错误。
    ///
    /// 参数：id 为终端标识
    /// 核心流程：先查注册表，未命中的文案统一带标识，便于调用方定位。
    fn session(&self, id: &str) -> CoreResult<Arc<dyn TerminalControl>> {
        self.terminals
            .get(id)
            .ok_or_else(|| CoreError::NotFound(id.to_string()))
    }
}

#[async_trait]
impl Host for TerminalHost {
    async fn list_groups(&self) -> CoreResult<Vec<SessionGroup>> {
        self.store.list_groups().await
    }

    async fn list_sessions(&self, keyword: &str) -> CoreResult<Vec<Session>> {
        self.store.list_sessions(keyword).await
    }

    async fn get_session(&self, id: &str) -> CoreResult<Session> {
        self.store.get_session(id).await
    }

    async fn list_tunnel_configs(&self) -> CoreResult<Vec<TunnelConfig>> {
        self.store.tunnel_config_list().await
    }

    async fn open_terminal(&self, spec: TerminalSpec) -> CoreResult<OpenedTerminal> {
        let id = next_terminal_id();
        let exited = Arc::new(AtomicBool::new(false));
        let sink = output_sink(self.verbose, Arc::clone(&exited));

        // 两条路径内部都有阻塞逻辑 -> 一律丢进阻塞线程池，别占住异步执行器
        let (session, cols, rows, kind): (Arc<dyn TerminalControl>, u16, u16, TerminalKind) =
            match spec {
                TerminalSpec::Local {
                    shell,
                    cwd,
                    cols,
                    rows,
                } => {
                    let cols = cols.max(1);
                    let rows = rows.max(1);
                    let options = TerminalOptions {
                        shell,
                        args: Vec::new(),
                        cwd,
                        term: "xterm-256color".to_string(),
                        cols,
                        rows,
                    };
                    let session =
                        tokio::task::spawn_blocking(move || TerminalSession::spawn(&options, sink))
                            .await
                            .map_err(|err| CoreError::Terminal(format!("启动终端任务失败：{err}")))??;
                    (Arc::new(session), cols, rows, TerminalKind::Local)
                }
                TerminalSpec::Ssh { cols, rows, ssh } => {
                    let cols = cols.max(1);
                    let rows = rows.max(1);
                    let options = ssh_options(SshSpec { cols, rows, ..ssh })?;
                    let store = self.store.clone();
                    let session = tokio::task::spawn_blocking(move || {
                        SshSession::connect(&store, &options, sink)
                    })
                    .await
                    .map_err(|err| CoreError::Terminal(format!("启动 SSH 任务失败：{err}")))??;
                    (Arc::new(session), cols, rows, TerminalKind::Remote)
                }
                // 会话凭据只在桌面壳里有，无头宿主不认识已保存会话，直接给明确错误
                TerminalSpec::Session { .. } => {
                    return Err(CoreError::Terminal("当前宿主不支持按会话打开终端".into()));
                }
            };

        self.terminals.insert(id.clone(), session);
        self.facts.lock().expect("终端事实表锁已中毒").insert(
            id.clone(),
            TerminalFacts {
                kind,
                exited,
                cols,
                rows,
                written: 0,
            },
        );
        Ok(OpenedTerminal { id, cols, rows })
    }

    async fn terminal_read(
        &self,
        id: &str,
        cursor: u64,
        max: usize,
        plain: bool,
    ) -> CoreResult<TerminalChunk> {
        let session = self.session(id)?;
        let chunk = session
            .scrollback(cursor, max.max(1))
            .ok_or_else(|| CoreError::Terminal("该终端不支持回滚读取".into()))?;
        let text = String::from_utf8_lossy(&chunk.bytes).into_owned();
        let text = if plain { strip_ansi(&text) } else { text };
        let exited = self
            .facts
            .lock()
            .expect("终端事实表锁已中毒")
            .get(id)
            .map(|facts| facts.exited.load(Ordering::SeqCst))
            .unwrap_or(false);
        Ok(TerminalChunk {
            start: chunk.start,
            next: chunk.next,
            truncated: chunk.truncated,
            text,
            exited,
        })
    }

    async fn terminal_write(&self, id: &str, data: &str) -> CoreResult<()> {
        let session = self.session(id)?;
        let local = self
            .facts
            .lock()
            .expect("终端事实表锁已中毒")
            .get(id)
            .map(|facts| facts.kind.is_local())
            .unwrap_or(false);
        // 只有 Windows 上的本机 PTY 需要把裸换行补成回车换行，远端会话原样发送
        let payload = normalize_local_input(data, local && cfg!(windows));
        session.write(payload.as_bytes())?;
        let written = payload.len() as u64;
        if let Some(facts) = self.facts.lock().expect("终端事实表锁已中毒").get_mut(id) {
            facts.written += written;
        }
        Ok(())
    }

    async fn terminal_resize(&self, id: &str, cols: u16, rows: u16) -> CoreResult<()> {
        self.session(id)?.resize(cols.max(1), rows.max(1))?;
        if let Some(facts) = self.facts.lock().expect("终端事实表锁已中毒").get_mut(id) {
            facts.cols = cols.max(1);
            facts.rows = rows.max(1);
        }
        Ok(())
    }

    async fn terminal_close(&self, id: &str) -> CoreResult<()> {
        let session = self.session(id)?;
        session.close()?;
        self.terminals.remove(id);
        self.facts.lock().expect("终端事实表锁已中毒").remove(id);
        Ok(())
    }

    async fn terminal_exec(&self, id: &str, command: &str, max_bytes: usize) -> CoreResult<String> {
        let session = self.session(id)?;
        let id = id.to_string();
        let command = command.to_string();
        // exec 内部要跑自己的异步运行时 -> 放到阻塞线程池，避免把异步工作线程堵死
        tokio::task::spawn_blocking(move || {
            let connection = session.connection().ok_or_else(|| {
                CoreError::Terminal(format!("终端 {id} 不是 SSH 会话，无法直接执行命令"))
            })?;
            connection.exec(&command, max_bytes)
        })
        .await
        .map_err(|err| CoreError::Terminal(format!("执行 SSH 命令的任务失败：{err}")))?
    }

    async fn list_terminals(&self) -> Vec<TerminalInfo> {
        let facts = self.facts.lock().expect("终端事实表锁已中毒");
        let mut terminals: Vec<TerminalInfo> = facts
            .iter()
            .map(|(id, item)| TerminalInfo {
                id: id.clone(),
                cols: Some(item.cols),
                rows: Some(item.rows),
                written: Some(item.written),
            })
            .collect();
        terminals.sort_by(|left, right| left.id.cmp(&right.id));
        terminals
    }

    async fn create_group(&self, name: &str, parent_id: Option<&str>) -> CoreResult<SessionGroup> {
        self.store.create_group(name, parent_id).await
    }

    async fn rename_group(&self, id: &str, name: &str) -> CoreResult<()> {
        self.store.rename_group(id, name).await
    }

    async fn delete_group(&self, id: &str) -> CoreResult<()> {
        self.store.delete_group(id).await
    }

    async fn move_group(&self, id: &str, parent_id: Option<&str>) -> CoreResult<()> {
        self.store.move_group(id, parent_id).await
    }

    async fn create_session(&self, input: &SessionInput) -> CoreResult<Session> {
        self.store.create_session(input).await
    }

    async fn update_session(&self, id: &str, input: &SessionInput) -> CoreResult<Session> {
        self.store.update_session(id, input).await
    }

    async fn delete_session(&self, id: &str) -> CoreResult<()> {
        self.store.delete_session(id).await
    }

    async fn move_session(&self, id: &str, group_id: Option<&str>) -> CoreResult<()> {
        self.store.move_session(id, group_id).await
    }

    async fn copy_session(&self, id: &str) -> CoreResult<Session> {
        self.store.copy_session(id).await
    }

    async fn settings_get_all(&self) -> CoreResult<Vec<(String, String)>> {
        self.store.settings_get_all().await
    }

    async fn settings_set(&self, key: &str, value: &str) -> CoreResult<()> {
        // 这里再拦一次：桌面壳与无头宿主各有自己的入口，漏一处就等于没拦
        if key.starts_with("mcp.") {
            return Err(CoreError::Validation(
                "不允许通过 MCP 修改 MCP 自身配置".into(),
            ));
        }
        self.store.settings_set(key, value).await
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_local_input;

    /// 非 Windows 本机终端（含远端 shell）一律原样发送。
    #[test]
    fn leaves_input_unchanged_off_windows() {
        assert_eq!(normalize_local_input("echo a\nb\n", false), "echo a\nb\n");
    }

    /// Windows 本机 PTY 的裸 \n 要补成 \r\n 才能提交。
    #[test]
    fn turns_bare_newline_into_crlf_on_windows() {
        assert_eq!(normalize_local_input("echo a\n", true), "echo a\r\n");
        assert_eq!(
            normalize_local_input("echo a\necho b", true),
            "echo a\r\necho b"
        );
    }

    /// 已经是 \r\n 的不重复补回车，避免变成 \r\r\n。
    #[test]
    fn does_not_double_convert_existing_crlf() {
        assert_eq!(normalize_local_input("echo a\r\n", true), "echo a\r\n");
        assert_eq!(
            normalize_local_input("a\r\nb\nc\r\n", true),
            "a\r\nb\r\nc\r\n"
        );
    }

    /// 不含换行的输入在 Windows 下也不变。
    #[test]
    fn leaves_input_without_newline_untouched() {
        assert_eq!(normalize_local_input("Get-Date", true), "Get-Date");
        assert_eq!(normalize_local_input("", true), "");
    }
}
