//! MCP 服务的桌面壳适配：把设置里的开关、端口与令牌接到启停控制器上。
//!
//! 这一层只认 Tauri 与设置表：读设置、必要时生成令牌、调用 [`McpControl`]、把结果
//! 整理成前端能直接展示的结构。真正的监听与停止由 `rustmobax-mcp` 负责。

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use rand_core::{OsRng, RngCore};
use rustmobax_core::{
    AuthKind, CoreError, CoreResult, ErrorPayload, McpAuditEntry, Protocol, Session, SessionGroup,
    SessionInput, SshAuth, Store, MCP_AUDIT_DEFAULT_ROWS, MCP_AUDIT_MAX_ROWS,
};
use rustmobax_mcp::{
    Host, McpAudit, McpControl, OpenedTerminal, SshAuthSpec, SshSpec, StoreAudit, TerminalChunk,
    TerminalHost, TerminalSpec, async_trait,
};
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::terminal::{resolve_auth, SshAuthRequest};
use crate::state::AppState;

/// 开关设置键。
const KEY_ENABLED: &str = "mcp.enabled";
/// 端口设置键。
const KEY_PORT: &str = "mcp.port";
/// 令牌设置键。
const KEY_TOKEN: &str = "mcp.token";
/// 端口未配置或写坏时使用的默认值。
const DEFAULT_PORT: u16 = 47820;
/// 一字节的十六进制宽度，与 32 字节令牌拼成 64 个字符。
const HEX_WIDTH: usize = 2;

/// agent 打开终端后推给前端的事件负载。
///
/// 无参数
/// 核心流程：只带前端做通知需要的四个字段，按 camelCase 序列化，
/// 前端据此查出会话名，在状态栏提示「agent 做了什么」。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOpened {
    /// 终端标识。
    pub terminal_id: String,
    /// 打开它的会话标识。
    pub session_id: String,
    /// 生效的列数。
    pub cols: u16,
    /// 生效的行数。
    pub rows: u16,
}

/// agent 改动会话库或设置后推给前端的事件负载。
///
/// 无参数
/// 核心流程：只带一个粗粒度的种类标签，前端据此提示并重拉列表；
/// 按 camelCase 序列化，与前端约定的 `mcp:changed` 事件配套使用。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpChanged {
    /// 变更种类，取值为 sessions、groups、settings。
    pub kind: String,
}

/// 分组变更的种类标签。
const CHANGED_GROUPS: &str = "groups";
/// 会话变更的种类标签。
const CHANGED_SESSIONS: &str = "sessions";
/// 应用设置变更的种类标签。
const CHANGED_SETTINGS: &str = "settings";

/// 会话 extra 里与认证相关的引用。
///
/// 无参数
/// 核心流程：字段与前端会话表单写入的键一一对应（camelCase）；
/// 这里只读引用，明文密码与口令都不落在会话表里。
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionExtra {
    /// 私钥文件路径。
    #[serde(default)]
    private_key_path: Option<String>,
    /// 私钥是否带口令。
    #[serde(default)]
    key_has_passphrase: bool,
    /// 密码凭据在凭据库里的标识。
    #[serde(default)]
    credential_id: Option<String>,
    /// 私钥口令凭据在凭据库里的标识。
    #[serde(default)]
    passphrase_credential_id: Option<String>,
}

/// 解析会话的高级配置。
///
/// 参数：raw 为会话 extra 的原始字符串
/// 核心流程：解析失败或不是对象时按空配置处理，脏数据不该挡住 agent 开终端。
fn parse_extra(raw: Option<&str>) -> SessionExtra {
    raw.and_then(|value| serde_json::from_str::<SessionExtra>(value).ok())
        .unwrap_or_default()
}

/// 把凭据库错误转回核心错误。
///
/// 参数：payload 为 `resolve_auth` 返回的前端错误
/// 核心流程：message 已经是完整的中文文案，直接装进 Vault 变体，
/// 保持「请先解锁」这类提示原样透传给 agent。
fn vault_error(payload: ErrorPayload) -> CoreError {
    CoreError::Vault(payload.message)
}

/// 把核心认证方式映射成工具层的认证参数。
///
/// 参数：auth 为核心层的认证方式
/// 核心流程：密码与私钥一一对应，核心层没有的形态不会被造出来。
fn auth_spec(auth: SshAuth) -> SshAuthSpec {
    match auth {
        SshAuth::Password(password) => SshAuthSpec::Password { password },
        SshAuth::PrivateKey { path, passphrase } => SshAuthSpec::PrivateKey { path, passphrase },
    }
}

/// 把未知主机指纹转成给 agent 看的提示。
///
/// 参数：error 为宿主返回的错误
/// 核心流程：只改写 HostKeyUnknown，别的错误原样上抛；
/// agent 不能替用户确认指纹，只能提示人先在界面上连一次。
fn host_key_error(error: CoreError) -> CoreError {
    match error {
        CoreError::HostKeyUnknown(info) => CoreError::Ssh(format!(
            "未知主机 {}:{}，请先在应用中手动连接该会话一次以确认指纹",
            info.host, info.port
        )),
        other => other,
    }
}

/// 桌面壳里的宿主实现。
///
/// 无参数
/// 核心流程：只读能力与本机/显式 SSH 终端全部委托给 [`TerminalHost`]；
/// 按已保存会话打开时先取会话、解出凭据再转成显式 SSH 参数连接，
/// 成功后向前端推一条通知事件，让人类知道 agent 动了哪台机器。
pub struct DesktopHost {
    app: AppHandle,
    store: Store,
    terminal: TerminalHost,
}

impl DesktopHost {
    /// 构造桌面宿主。
    ///
    /// 参数：app 为应用句柄，store 为会话库
    /// 核心流程：内部另建一份 [`TerminalHost`]，其会话库取自应用状态，
    /// 保证与前端手动开的终端读写的是同一个注册表。
    pub fn new(app: AppHandle, store: Store) -> Self {
        let terminal = TerminalHost::new(app.state::<AppState>().store.clone());
        Self {
            app,
            store,
            terminal,
        }
    }

    /// 按已保存会话解析认证参数。
    ///
    /// 参数：session 为已保存会话
    /// 核心流程：password 走凭据库里的密码引用，key 走私钥路径加可选口令，
    /// 两者都需要先解锁凭据库；agent 尚未在核心层实现，明确报不支持。
    async fn session_auth(&self, session: &Session) -> CoreResult<SshAuthSpec> {
        let extra = parse_extra(session.extra.as_deref());
        let state = self.app.state::<AppState>();
        let vault = &state.vault;

        match session.auth_kind {
            AuthKind::Password => {
                let credential_id = extra
                    .credential_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        CoreError::Validation(format!(
                            "会话 {} 没有保存密码凭据，无法打开终端",
                            session.id
                        ))
                    })?;
                if !vault.is_unlocked() {
                    return Err(CoreError::Vault("凭据库已锁定，请先解锁".into()));
                }
                let auth = resolve_auth(
                    vault,
                    &SshAuthRequest::Credential {
                        id: credential_id.to_string(),
                    },
                )
                .await
                .map_err(vault_error)?;
                Ok(auth_spec(auth))
            }
            AuthKind::Key => {
                let path = extra
                    .private_key_path
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        CoreError::Validation(format!(
                            "会话 {} 没有保存私钥路径，无法打开终端",
                            session.id
                        ))
                    })?
                    .to_string();
                let passphrase = if extra.key_has_passphrase {
                    let credential_id = extra
                        .passphrase_credential_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| {
                            CoreError::Validation(format!(
                                "会话 {} 的私钥带口令但没有保存口令凭据，无法打开终端",
                                session.id
                            ))
                        })?;
                    if !vault.is_unlocked() {
                        return Err(CoreError::Vault("凭据库已锁定，请先解锁".into()));
                    }
                    let auth = resolve_auth(
                        vault,
                        &SshAuthRequest::Credential {
                            id: credential_id.to_string(),
                        },
                    )
                    .await
                    .map_err(vault_error)?;
                    match auth {
                        SshAuth::Password(secret) => Some(secret),
                        SshAuth::PrivateKey { .. } => None,
                    }
                } else {
                    None
                };
                Ok(SshAuthSpec::PrivateKey { path, passphrase })
            }
            AuthKind::Agent => Err(CoreError::Validation(format!(
                "会话 {} 使用 SSH Agent 认证，当前版本不支持",
                session.id
            ))),
        }
    }

    /// 通知前端会话库或设置已被改动。
    ///
    /// 参数：kind 为变更种类标签，取 groups、sessions、settings 三者之一
    /// 核心流程：直接 emit 一条 `mcp:changed`；事件发不出去只影响界面刷新，
    /// 不影响已经成功的写操作，因此这里忽略发送错误。
    fn notify_changed(&self, kind: &str) {
        let _ = self.app.emit(
            "mcp:changed",
            McpChanged {
                kind: kind.to_string(),
            },
        );
    }

    /// 按已保存会话打开一个 SSH 终端。
    ///
    /// 参数：session_id 为会话标识，cols 与 rows 为初始尺寸
    /// 核心流程：取会话 -> 校验协议与主机 -> 解凭据 -> 以显式 SSH 参数交给
    /// 内部 TerminalHost -> 成功后推通知事件。指纹一律不接受未知，
    /// 未信任时返回带处理指引的错误，不让 agent 自己确认。
    async fn open_session(
        &self,
        session_id: &str,
        cols: u16,
        rows: u16,
    ) -> CoreResult<OpenedTerminal> {
        let session = self.store.get_session(session_id).await?;

        if !matches!(session.protocol, Protocol::Ssh | Protocol::Sftp) {
            return Err(CoreError::Validation(format!(
                "会话 {} 的协议不支持终端",
                session.id
            )));
        }

        let host = session
            .host
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                CoreError::Validation(format!("会话 {} 没有填写主机，无法打开终端", session.id))
            })?
            .to_string();
        let auth = self.session_auth(&session).await?;

        let ssh = SshSpec {
            host,
            port: session.port.unwrap_or(22),
            username: session.username.clone().unwrap_or_default(),
            cols,
            rows,
            term: "xterm-256color".to_string(),
            accept_unknown_host_key: false,
            auth,
        };
        let opened = self
            .terminal
            .open_terminal(TerminalSpec::Ssh { cols, rows, ssh })
            .await
            .map_err(host_key_error)?;

        let _ = self.app.emit(
            "terminal:opened",
            TerminalOpened {
                terminal_id: opened.id.clone(),
                session_id: session.id.clone(),
                cols: opened.cols,
                rows: opened.rows,
            },
        );
        Ok(opened)
    }
}

#[async_trait]
impl Host for DesktopHost {
    async fn list_groups(
        &self,
    ) -> CoreResult<Vec<rustmobax_core::SessionGroup>> {
        self.terminal.list_groups().await
    }

    async fn list_sessions(&self, keyword: &str) -> CoreResult<Vec<Session>> {
        self.terminal.list_sessions(keyword).await
    }

    async fn get_session(&self, id: &str) -> CoreResult<Session> {
        self.terminal.get_session(id).await
    }

    async fn list_tunnel_configs(
        &self,
    ) -> CoreResult<Vec<rustmobax_core::TunnelConfig>> {
        self.terminal.list_tunnel_configs().await
    }

    async fn open_terminal(&self, spec: TerminalSpec) -> CoreResult<OpenedTerminal> {
        if let TerminalSpec::Session {
            session_id,
            cols,
            rows,
        } = &spec
        {
            return self.open_session(session_id, *cols, *rows).await;
        }
        self.terminal.open_terminal(spec).await
    }

    async fn terminal_read(
        &self,
        id: &str,
        cursor: u64,
        max: usize,
        plain: bool,
    ) -> CoreResult<TerminalChunk> {
        self.terminal.terminal_read(id, cursor, max, plain).await
    }

    async fn terminal_write(&self, id: &str, data: &str) -> CoreResult<()> {
        self.terminal.terminal_write(id, data).await
    }

    async fn terminal_resize(&self, id: &str, cols: u16, rows: u16) -> CoreResult<()> {
        self.terminal.terminal_resize(id, cols, rows).await
    }

    async fn terminal_close(&self, id: &str) -> CoreResult<()> {
        self.terminal.terminal_close(id).await
    }

    async fn list_terminals(&self) -> Vec<rustmobax_mcp::TerminalInfo> {
        self.terminal.list_terminals().await
    }

    async fn terminal_exec(&self, id: &str, command: &str, max_bytes: usize) -> CoreResult<String> {
        self.terminal.terminal_exec(id, command, max_bytes).await
    }

    async fn create_group(&self, name: &str, parent_id: Option<&str>) -> CoreResult<SessionGroup> {
        let group = self.terminal.create_group(name, parent_id).await?;
        self.notify_changed(CHANGED_GROUPS);
        Ok(group)
    }

    async fn rename_group(&self, id: &str, name: &str) -> CoreResult<()> {
        self.terminal.rename_group(id, name).await?;
        self.notify_changed(CHANGED_GROUPS);
        Ok(())
    }

    async fn delete_group(&self, id: &str) -> CoreResult<()> {
        self.terminal.delete_group(id).await?;
        self.notify_changed(CHANGED_GROUPS);
        Ok(())
    }

    async fn move_group(&self, id: &str, parent_id: Option<&str>) -> CoreResult<()> {
        self.terminal.move_group(id, parent_id).await?;
        self.notify_changed(CHANGED_GROUPS);
        Ok(())
    }

    async fn create_session(&self, input: &SessionInput) -> CoreResult<Session> {
        let session = self.terminal.create_session(input).await?;
        self.notify_changed(CHANGED_SESSIONS);
        Ok(session)
    }

    async fn update_session(&self, id: &str, input: &SessionInput) -> CoreResult<Session> {
        let session = self.terminal.update_session(id, input).await?;
        self.notify_changed(CHANGED_SESSIONS);
        Ok(session)
    }

    async fn delete_session(&self, id: &str) -> CoreResult<()> {
        self.terminal.delete_session(id).await?;
        self.notify_changed(CHANGED_SESSIONS);
        Ok(())
    }

    async fn move_session(&self, id: &str, group_id: Option<&str>) -> CoreResult<()> {
        self.terminal.move_session(id, group_id).await?;
        self.notify_changed(CHANGED_SESSIONS);
        Ok(())
    }

    async fn copy_session(&self, id: &str) -> CoreResult<Session> {
        let session = self.terminal.copy_session(id).await?;
        self.notify_changed(CHANGED_SESSIONS);
        Ok(session)
    }

    async fn settings_get_all(&self) -> CoreResult<Vec<(String, String)>> {
        self.terminal.settings_get_all().await
    }

    async fn settings_set(&self, key: &str, value: &str) -> CoreResult<()> {
        // 拦截在内部宿主里做，这里只在写成功后通知前端重读设置
        self.terminal.settings_set(key, value).await?;
        self.notify_changed(CHANGED_SETTINGS);
        Ok(())
    }
}

/// 前端看到的 MCP 服务状态。
///
/// 无参数
/// 核心流程：开关与端口取自设置，运行结论取自控制器，令牌明文回给本机前端用于配置
/// 外部客户端；字段名按 camelCase 序列化，与其余命令保持一致。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatusView {
    /// 设置里的开关。
    pub enabled: bool,
    /// 设置里的端口。
    pub port: u16,
    /// 当前是否在监听。
    pub running: bool,
    /// 实际监听地址，未运行时为空。
    pub addr: Option<String>,
    /// 当前令牌，供前端展示与复制。
    pub token: String,
    /// 最近一次启动失败的原因，成功后为空。
    pub last_error: Option<String>,
}

/// 桌面壳里的 MCP 运行时。
///
/// 无参数
/// 核心流程：持有启停控制器与最近一次生效的令牌。令牌另外缓存一份，设置表读不出来时
/// 仍能在状态里展示上次用过的值，免得前端看到空白以为服务没配置。
pub struct McpRuntime {
    control: McpControl,
    token: Mutex<Option<String>>,
}

impl Default for McpRuntime {
    /// 构造空的运行时。
    ///
    /// 参数：无
    /// 核心流程：等价于 [`McpRuntime::new`]，方便直接交给 `app.manage`。
    fn default() -> Self {
        Self::new()
    }
}

impl McpRuntime {
    /// 构造一个空的运行时。
    ///
    /// 参数：无
    /// 核心流程：控制器为空槽位，令牌缓存为空，不读设置也不起服务。
    pub fn new() -> Self {
        Self {
            control: McpControl::new(),
            token: Mutex::new(None),
        }
    }

    /// 记下最近一次生效的令牌。
    ///
    /// 参数：token 为刚用过或刚生成的令牌
    /// 核心流程：加锁覆盖；锁被污染时静默跳过，缓存只影响展示，不影响服务本身。
    fn remember(&self, token: &str) {
        if let Ok(mut cached) = self.token.lock() {
            *cached = Some(token.to_string());
        }
    }

    /// 取缓存的令牌。
    ///
    /// 参数：无
    /// 核心流程：锁可用就克隆一份，否则当作没有缓存。
    fn cached(&self) -> Option<String> {
        self.token.lock().ok().and_then(|cached| cached.clone())
    }
}

/// 构造交给 `app.manage` 的运行时。
///
/// 参数：无
/// 核心流程：直接新建一份，保持 setup 里一行可读。
pub fn state() -> McpRuntime {
    McpRuntime::new()
}

/// 按当前设置应用一次启停配置。
///
/// 参数：app 为应用句柄
/// 核心流程：先读全部设置得到开关与端口，令牌缺失或为空时生成 64 字符十六进制并写回，
/// 然后把宿主与令牌交给控制器；控制器报错时把它转成前端错误，交给调用方决定文案。
pub async fn apply_from_settings(app: &AppHandle) -> Result<(), ErrorPayload> {
    let store = app.state::<AppState>().store.clone();
    let runtime = app.state::<McpRuntime>();

    let settings = read_settings(&store).await.map_err(ErrorPayload::from)?;
    let enabled = settings
        .get(KEY_ENABLED)
        .is_some_and(|value| matches!(value.as_str(), "true" | "1"));
    let port = settings
        .get(KEY_PORT)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let token = match settings.get(KEY_TOKEN).filter(|value| !value.is_empty()) {
        Some(token) => token.clone(),
        None => {
            let token = generate_token();
            store
                .settings_set(KEY_TOKEN, &token)
                .await
                .map_err(ErrorPayload::from)?;
            token
        }
    };

    runtime.remember(&token);
    let host: Arc<dyn Host> = Arc::new(DesktopHost::new(app.clone(), store.clone()));
    let audit: Arc<dyn McpAudit> = Arc::new(StoreAudit::new(store));
    let status = runtime
        .control
        .apply(host, audit, enabled, port, &token)
        .await;

    match status.last_error {
        Some(reason) if enabled => Err(ErrorPayload::from(CoreError::Validation(reason))),
        _ => Ok(()),
    }
}

/// 汇总当前状态给前端。
///
/// 参数：app 为应用句柄
/// 核心流程：设置读不出来时按未配置处理，运行结论与失败原因取自控制器，令牌优先用
/// 设置里的值，其次用缓存，保证界面上总能看到当前令牌。
pub async fn status(app: &AppHandle) -> McpStatusView {
    let store = app.state::<AppState>().store.clone();
    let runtime = app.state::<McpRuntime>();

    let settings = read_settings(&store).await.unwrap_or_default();
    let enabled = settings
        .get(KEY_ENABLED)
        .is_some_and(|value| matches!(value.as_str(), "true" | "1"));
    let port = settings
        .get(KEY_PORT)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let control = runtime.control.status();
    let token = settings
        .get(KEY_TOKEN)
        .filter(|value| !value.is_empty())
        .cloned()
        .or_else(|| runtime.cached())
        .unwrap_or_default();

    McpStatusView {
        enabled,
        port,
        running: control.running,
        addr: control.bind_addr.map(|addr| addr.to_string()),
        token,
        last_error: control.last_error,
    }
}

/// 换一个令牌并按需重启服务。
///
/// 参数：app 为应用句柄
/// 核心流程：先生成新令牌写回设置，再走一次完整的应用流程；服务正在跑时控制器会先停
/// 旧的再起新的，客户端必须换用新令牌才能继续访问。
pub async fn reset_token(app: &AppHandle) -> Result<(), ErrorPayload> {
    let store = app.state::<AppState>().store.clone();
    let token = generate_token();
    store
        .settings_set(KEY_TOKEN, &token)
        .await
        .map_err(ErrorPayload::from)?;

    apply_from_settings(app).await
}

/// 读取全部设置并转成查找表。
///
/// 参数：store 为会话库
/// 核心流程：一次查询取回全部键值，失败原样上抛，由调用方决定是报错还是按默认值处理。
async fn read_settings(store: &Store) -> CoreResult<HashMap<String, String>> {
    Ok(store.settings_get_all().await?.into_iter().collect())
}

/// 生成一个新的访问令牌。
///
/// 参数：无
/// 核心流程：从系统随机源取 32 字节，逐字节格式化成两位十六进制后拼接，得到 64 个
/// 字符的令牌；取随机数失败会 panic，因为此时无法产出可用的凭据。
fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);

    let mut token = String::with_capacity(bytes.len() * HEX_WIDTH);
    for byte in bytes {
        token.push_str(&format!("{byte:02x}"));
    }
    token
}

/// 查询 MCP 服务状态。
///
/// 参数：app 为应用句柄
/// 核心流程：转交 [`status`]，把结果序列化给前端。
#[tauri::command]
pub async fn mcp_status(app: AppHandle) -> McpStatusView {
    status(&app).await
}

/// 重置访问令牌并应用配置。
///
/// 参数：app 为应用句柄
/// 核心流程：转交 [`reset_token`]，旧令牌立刻失效。
#[tauri::command]
pub async fn mcp_reset_token(app: AppHandle) -> Result<(), ErrorPayload> {
    reset_token(&app).await
}

/// 按当前设置重新应用一次配置。
///
/// 参数：app 为应用句柄
/// 核心流程：转交 [`apply_from_settings`]，前端改完设置后可以用它手动重试启动。
#[tauri::command]
pub async fn mcp_apply(app: AppHandle) -> Result<(), ErrorPayload> {
    apply_from_settings(&app).await
}

/// 查询最近的 MCP 工具调用审计。
///
/// 参数：app 为应用句柄；limit 为期望条数，缺省 100，最多 500
/// 核心流程：读取会话库并取出最近若干条，新的在前；参数只带摘要，
/// 库里没有工具调用时返回空数组而不是报错。
#[tauri::command]
pub async fn mcp_audit(
    app: AppHandle,
    limit: Option<usize>,
) -> Result<Vec<McpAuditEntry>, ErrorPayload> {
    let store = app.state::<AppState>().store.clone();
    let limit = limit
        .unwrap_or(MCP_AUDIT_DEFAULT_ROWS)
        .min(MCP_AUDIT_MAX_ROWS);
    store.mcp_audit_recent(limit).await.map_err(ErrorPayload::from)
}
