use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use rustmobax_core::{
    default_shell, ErrorPayload, OutputSink, SerialOptions, SerialPortInfo, SerialSession, SshAuth,
    SshOptions, SshSession, TelnetOptions, TelnetSession, TerminalOptions, TerminalOutput,
    TerminalSession, Vault,
};
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::State;
use uuid::Uuid;

use crate::state::AppState;

/// 启动本地终端时的请求参数。
///
/// 无参数
/// 核心流程：前端按实际可见区域算出 cols/rows 传进来，保证首屏就排对。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalStartRequest {
    pub cols: u16,
    pub rows: u16,
    pub shell: Option<String>,
    pub cwd: Option<String>,
}

/// 推给前端的事件。
///
/// 无参数
/// 核心流程：数据用 base64 承载，避免把字节数组拆成 JSON 数字数组带来的体积放大。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum TerminalEvent {
    Data { chunk: String },
    Exit { code: Option<i32> },
}

/// 启动一个本地终端。
///
/// 参数：request 为尺寸与可选的 shell，on_event 为前端事件通道
/// 核心流程：登记标识 → 生成输出回调（base64 编码后推给前端）→ 开 PTY →
/// 存入注册表并返回标识。
#[tauri::command]
pub fn start_local_terminal(
    state: State<'_, AppState>,
    request: TerminalStartRequest,
    on_event: Channel<TerminalEvent>,
) -> Result<String, ErrorPayload> {
    let id = Uuid::new_v4().to_string();

    // 读线程回调 -> 把字节编码成 base64 后推给前端
    let sink: OutputSink = Box::new(move |output| {
        let event = match output {
            TerminalOutput::Data(bytes) => TerminalEvent::Data {
                chunk: BASE64.encode(bytes),
            },
            TerminalOutput::Exit(code) => TerminalEvent::Exit { code },
        };
        let _ = on_event.send(event);
    });

    let options = TerminalOptions {
        shell: request.shell.clone().filter(|value| !value.trim().is_empty()),
        cwd: request.cwd.clone().filter(|value| !value.trim().is_empty()),
        cols: request.cols,
        rows: request.rows,
        ..Default::default()
    };

    let session = TerminalSession::spawn(&options, sink).map_err(ErrorPayload::from)?;
    register_pty_terminal(&state, id, session)
}

/// 把一个本地 PTY 会话登记进终端注册表。
///
/// 参数：state 为应用状态，id 为终端标识，session 为已启动的本地会话
/// 核心流程：本地终端与 WSL 终端走同一条注册路径 —— 同一个注册表、同一个
/// 标识生成方式，后续的写入 / 尺寸 / 关闭 / cwd 命令对两者完全通用。
fn register_pty_terminal(
    state: &AppState,
    id: String,
    session: TerminalSession,
) -> Result<String, ErrorPayload> {
    if !state.terminals.insert(id.clone(), Arc::new(session)) {
        return Err(ErrorPayload::from(rustmobax_core::CoreError::Terminal(
            "终端注册表不可用".into(),
        )));
    }
    Ok(id)
}

/// 启动 WSL 终端时的请求参数。
///
/// 无参数
/// 核心流程：distro 为空串表示用 WSL 默认发行版（此时不传 `-d`），
/// 其余与本地终端一致，只多了一个发行版选择。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WslStartRequest {
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub distro: Option<String>,
}

/// 启动一个 WSL 终端。
///
/// 参数：request 为尺寸与发行版名，on_event 为前端事件通道
/// 核心流程：把发行版拼成 `wsl.exe -d <发行版> --cd ~` → 当成普通本地 PTY 启动
/// （同一个事件通道、同一套 base64 编码）→ 登记进同一张终端注册表。
/// 这里不新写 transport：WSL 只是换了个可执行程序的本地 PTY。
#[tauri::command]
pub fn start_wsl_terminal(
    state: State<'_, AppState>,
    request: WslStartRequest,
    on_event: Channel<TerminalEvent>,
) -> Result<String, ErrorPayload> {
    let id = Uuid::new_v4().to_string();

    // 读线程回调 -> 与本地终端、SSH、Telnet 完全同一条事件通道
    let sink: OutputSink = Box::new(move |output| {
        let event = match output {
            TerminalOutput::Data(bytes) => TerminalEvent::Data {
                chunk: BASE64.encode(bytes),
            },
            TerminalOutput::Exit(code) => TerminalEvent::Exit { code },
        };
        let _ = on_event.send(event);
    });

    let distro = request.distro.clone().unwrap_or_default();
    let (program, args) = rustmobax_core::wsl::build_command(&distro);

    let options = TerminalOptions {
        shell: Some(program),
        args,
        // 工作目录交给 wsl.exe 的 `--cd ~`，这里不再叠加宿主机的 HOME
        cwd: None,
        cols: request.cols,
        rows: request.rows,
        ..Default::default()
    };

    let session = TerminalSession::spawn(&options, sink).map_err(ErrorPayload::from)?;
    register_pty_terminal(&state, id, session)
}

/// 列出本机已安装的 WSL 发行版。
///
/// 参数：无
/// 核心流程：枚举要起外部进程（`wsl.exe -l -q`），放到阻塞线程池里跑，
/// 避免占住 async 工作线程；非 Windows 平台核心层直接回空列表，
/// 前端据「空列表」给出安装引导，因此这里不是错误。
#[tauri::command]
pub async fn wsl_list_distros() -> Result<Vec<String>, ErrorPayload> {
    tauri::async_runtime::spawn_blocking(rustmobax_core::wsl::list_distros)
        .await
        .map_err(|err| {
            ErrorPayload::from(rustmobax_core::CoreError::Terminal(format!(
                "任务异常：{err}"
            )))
        })
}

/// 把用户输入写到终端。
///
/// 参数：id 为终端标识，data 为 base64 编码的字节
/// 核心流程：解码后写入 PTY，终端不存在时报未找到。
#[tauri::command]
pub fn write_local_terminal(
    state: State<'_, AppState>,
    id: String,
    data: String,
) -> Result<(), ErrorPayload> {
    let bytes = BASE64
        .decode(data)
        .map_err(|err| ErrorPayload::from(rustmobax_core::CoreError::Terminal(format!(
            "输入解码失败：{err}"
        ))))?;
    let session = state.terminals.get(&id).ok_or_else(|| {
        ErrorPayload::from(rustmobax_core::CoreError::NotFound(format!("终端 {id}")))
    })?;
    session.write(&bytes).map_err(ErrorPayload::from)
}

/// 调整终端尺寸。
///
/// 参数：id 为终端标识，cols/rows 为新尺寸
/// 核心流程：交给核心层 resize，内核会给 shell 发 SIGWINCH。
#[tauri::command]
pub fn resize_local_terminal(
    state: State<'_, AppState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), ErrorPayload> {
    let session = state.terminals.get(&id).ok_or_else(|| {
        ErrorPayload::from(rustmobax_core::CoreError::NotFound(format!("终端 {id}")))
    })?;
    session.resize(cols, rows).map_err(ErrorPayload::from)
}

/// 关闭终端。
///
/// 参数：id 为终端标识
/// 核心流程：先从注册表摘下再杀进程，重复调用不报错。
#[tauri::command]
pub fn close_local_terminal(state: State<'_, AppState>, id: String) -> Result<(), ErrorPayload> {
    if let Some(session) = state.terminals.remove(&id) {
        session.close().map_err(ErrorPayload::from)?;
    }
    Ok(())
}

/// 返回平台默认 shell。
///
/// 参数：无
/// 核心流程：供设置界面展示"将使用哪个 shell"，避免前端自己猜平台差异。
#[tauri::command]
pub fn default_shell_name() -> String {
    default_shell()
}

/// 查终端当前的本地工作目录。
///
/// 参数：terminal_id 为终端注册表里的标识
/// 核心流程：从注册表取会话 → 同步读子进程 cwd（Linux 读 /proc/<pid>/cwd）。
/// 远端 shell 的目录在远端，由它自己用 OSC 7 上报，不走这条口子；
/// 读不到（进程已退出、非 Linux）返回 None，不当作错误。
#[tauri::command]
pub fn terminal_cwd(
    state: State<'_, AppState>,
    terminal_id: String,
) -> Result<Option<String>, ErrorPayload> {
    let session = state.terminals.get(&terminal_id).ok_or_else(|| {
        ErrorPayload::from(rustmobax_core::CoreError::NotFound(format!(
            "终端 {terminal_id}"
        )))
    })?;
    Ok(session.cwd())
}

/// 启动 Telnet 终端时的请求参数。
///
/// 无参数
/// 核心流程：主机、端口与初始尺寸；term 留空则用 xterm-256color
/// （这一步还不做协议协商，term 只预留给下一步的 TERMINAL-TYPE）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelnetStartRequest {
    pub host: String,
    pub port: u16,
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub term: Option<String>,
}

/// 启动一个 Telnet 终端。
///
/// 参数：request 为地址与尺寸，on_event 为前端事件通道
/// 核心流程：与本地/SSH 终端同一条事件通道（base64 数据 + 退出事件）→
/// 在阻塞线程里建 TCP 连接（这一步不做 IAC 协商）→ 登记进终端注册表并返回标识。
#[tauri::command]
pub async fn start_telnet_terminal(
    state: State<'_, AppState>,
    request: TelnetStartRequest,
    on_event: Channel<TerminalEvent>,
) -> Result<String, ErrorPayload> {
    let id = Uuid::new_v4().to_string();

    let sink: OutputSink = Box::new(move |output| {
        let event = match output {
            TerminalOutput::Data(bytes) => TerminalEvent::Data {
                chunk: BASE64.encode(bytes),
            },
            TerminalOutput::Exit(code) => TerminalEvent::Exit { code },
        };
        let _ = on_event.send(event);
    });

    let mut options = TelnetOptions {
        host: request.host.trim().to_string(),
        port: request.port,
        cols: request.cols,
        rows: request.rows,
        ..Default::default()
    };
    if let Some(term) = request
        .term
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        options.term = term.to_string();
    }

    // 建连是阻塞调用 -> 放到阻塞线程池，别占住 async 工作线程
    let session = tauri::async_runtime::spawn_blocking(move || {
        TelnetSession::connect(&options, sink).map(Arc::new)
    })
    .await
    .map_err(|err| {
        ErrorPayload::from(rustmobax_core::CoreError::Terminal(format!(
            "任务异常：{err}"
        )))
    })?
    .map_err(ErrorPayload::from)?;

    if !state.terminals.insert(id.clone(), session) {
        return Err(ErrorPayload::from(rustmobax_core::CoreError::Terminal(
            "终端注册表不可用".into(),
        )));
    }
    Ok(id)
}

/// 启动串口终端时的请求参数。
///
/// 无参数
/// 核心流程：设备路径必填；线路参数（波特率/数据位/停止位/校验/流控）都可省略，
/// 省略时按 9600-8-N-1 无流控处理 —— 老前端不传这些字段也不会报错。
/// 停止位用浮点是因为表单里存在 1.5 这一档，1.5 由核心层给出「当前平台不支持」
/// 的明确错误，不会被静默折成 1 或 2。
/// 这里刻意不含 cols/rows：串口没有窗口协商协议，尺寸对它没有意义。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SerialStartRequest {
    pub path: String,
    #[serde(default)]
    pub baud_rate: Option<u32>,
    #[serde(default)]
    pub data_bits: Option<u8>,
    /// 1 / 1.5 / 2（1.5 目前没有底层枚举，会得到明确错误）
    #[serde(default)]
    pub stop_bits: Option<f32>,
    #[serde(default)]
    pub parity: Option<String>,
    #[serde(default)]
    pub flow_control: Option<String>,
}

/// 列出本机串口设备。
///
/// 参数：无
/// 核心流程：枚举要走系统调用（无 libudev 时还会扫 /sys），放到阻塞线程池里跑，
/// 避免占住 async 工作线程；查不到设备时核心层回空列表而不是报错，
/// 前端据「空列表」给出「未检测到串口设备」的提示。
#[tauri::command]
pub async fn serial_list_ports() -> Result<Vec<SerialPortInfo>, ErrorPayload> {
    tauri::async_runtime::spawn_blocking(rustmobax_core::serial::list_ports)
        .await
        .map_err(|err| {
            ErrorPayload::from(rustmobax_core::CoreError::Terminal(format!(
                "任务异常：{err}"
            )))
        })
}

/// 启动一个串口终端。
///
/// 参数：request 为设备路径与可选的线路参数，on_event 为前端事件通道
/// 核心流程：与 Telnet 同形 —— 同一条事件通道（base64 数据 + 退出事件）→
/// 在阻塞线程里打开串口并起读线程 → 登记进同一张终端注册表并返回标识。
/// 串口没有窗口协商，前端后续的 resize 会被核心层当作空操作。
#[tauri::command]
pub async fn start_serial_terminal(
    state: State<'_, AppState>,
    request: SerialStartRequest,
    on_event: Channel<TerminalEvent>,
) -> Result<String, ErrorPayload> {
    let id = Uuid::new_v4().to_string();

    let sink: OutputSink = Box::new(move |output| {
        let event = match output {
            TerminalOutput::Data(bytes) => TerminalEvent::Data {
                chunk: BASE64.encode(bytes),
            },
            TerminalOutput::Exit(code) => TerminalEvent::Exit { code },
        };
        let _ = on_event.send(event);
    });

    // 未传的字段保持 SerialOptions::defaults 的默认值（9600-8-N-1 无流控）；
    // 传了就原样采纳，非法值交给 core 的校验给出点名到字段的错误 ——
    // 这里不再「悄悄丢掉不合法输入」（比如把波特率 0 变成 9600），否则用户看到的
    // 是连上了却对不上话，比直接报错难排查得多。
    let mut options = SerialOptions::defaults(request.path.trim());
    if let Some(baud_rate) = request.baud_rate {
        options.baud_rate = baud_rate;
    }
    if let Some(data_bits) = request.data_bits {
        options.data_bits = data_bits;
    }
    if let Some(stop_bits) = request.stop_bits {
        // 1.5 停止位在这里就得到可读错误，不必等到底层打开失败
        options.stop_bits =
            rustmobax_core::serial::parse_stop_bits(stop_bits).map_err(ErrorPayload::from)?;
    }
    if let Some(parity) = request.parity.filter(|value| !value.trim().is_empty()) {
        options.parity = parity;
    }
    if let Some(flow_control) = request.flow_control.filter(|value| !value.trim().is_empty()) {
        options.flow_control = flow_control;
    }
    // 打开前先整体校验一次 -> 纯计算、不碰设备，错误信息点名具体字段
    options.validate().map_err(ErrorPayload::from)?;

    // 打开串口是阻塞调用 -> 放到阻塞线程池，别占住 async 工作线程
    let session = tauri::async_runtime::spawn_blocking(move || {
        SerialSession::open(&options, sink).map(Arc::new)
    })
    .await
    .map_err(|err| {
        ErrorPayload::from(rustmobax_core::CoreError::Terminal(format!(
            "任务异常：{err}"
        )))
    })?
    .map_err(ErrorPayload::from)?;

    if !state.terminals.insert(id.clone(), session) {
        return Err(ErrorPayload::from(rustmobax_core::CoreError::Terminal(
            "终端注册表不可用".into(),
        )));
    }
    Ok(id)
}

/// 认证方式。
///
/// 无参数
/// 核心流程：密码可以现场输入，也可以只给凭据库里的标识；
/// 私钥从会话里保存的路径读取。凭据标识在连接前才解出明文。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum SshAuthRequest {
    Password { password: String },
    PrivateKey { path: String, passphrase: Option<String> },
    Credential { id: String },
}

/// 跳板机请求参数。
///
/// 无参数
/// 核心流程：一跳跳板机的连接四要素，形状与目标一致，
/// 前端把「直连」与「经跳板」写成同一套表单即可；只支持一跳，故没有嵌套字段。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JumpRequest {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SshAuthRequest,
    /// 用户已在界面上确认过跳板机的指纹。
    #[serde(default)]
    pub accept_unknown_host_key: bool,
}

/// 启动 SSH 终端时的请求参数。
///
/// 无参数
/// 核心流程：主机三要素 + 认证方式 + 终端尺寸，可选配一跳跳板机。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshStartRequest {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SshAuthRequest,
    pub cols: u16,
    pub rows: u16,
    /// 用户已在界面上确认过该主机指纹。
    pub accept_unknown_host_key: bool,
    /// 可选的一跳跳板机；缺省即直连。
    #[serde(default)]
    pub jump: Option<JumpRequest>,
}

/// 把请求里的认证方式解成真正的凭据。
///
/// 参数：vault 为凭据库，auth 为前端传来的认证方式
/// 核心流程：密码与私钥直接透传；凭据库引用在这里才 reveal 成明文，
/// 明文只留在后端内存里，不回流前端。
pub(crate) async fn resolve_auth(
    vault: &Vault,
    auth: &SshAuthRequest,
) -> Result<SshAuth, ErrorPayload> {
    match auth {
        SshAuthRequest::Password { password } => Ok(SshAuth::Password(password.clone())),
        SshAuthRequest::PrivateKey { path, passphrase } => Ok(SshAuth::PrivateKey {
            path: path.clone(),
            passphrase: passphrase.clone(),
        }),
        SshAuthRequest::Credential { id } => {
            let secret = vault.reveal(id).await.map_err(ErrorPayload::from)?;
            Ok(SshAuth::Password(secret))
        }
    }
}

/// 把可选的跳板请求解成 SshOptions 的 jump 字段。
///
/// 参数：vault 为凭据库，jump 为前端传来的跳板配置
/// 核心流程：没配跳板就返回 None（行为与没有该字段时完全一致）；
/// 配了就把跳板机拼成一套 SshOptions，终端尺寸用默认值（跳板机只做转发，
/// 不会开 PTY），并且跳板机自身不再带 jump（只允许一跳）。
pub(crate) async fn resolve_jump(
    vault: &Vault,
    jump: Option<&JumpRequest>,
) -> Result<Option<Box<SshOptions>>, ErrorPayload> {
    let Some(jump) = jump else {
        return Ok(None);
    };
    let auth = resolve_auth(vault, &jump.auth).await?;
    Ok(Some(Box::new(SshOptions {
        host: jump.host.clone(),
        port: jump.port,
        username: jump.username.clone(),
        auth,
        cols: 80,
        rows: 24,
        term: "xterm-256color".to_string(),
        accept_unknown_host_key: jump.accept_unknown_host_key,
        jump: None,
    })))
}

/// 启动一个 SSH 终端。
///
/// 参数：request 为连接参数，on_event 为前端事件通道
/// 核心流程：连接与认证在阻塞线程里完成（内部要跑异步运行时），
/// 成功后再把会话登记进注册表。
#[tauri::command]
pub async fn start_ssh_terminal(
    state: State<'_, AppState>,
    request: SshStartRequest,
    on_event: Channel<TerminalEvent>,
) -> Result<String, ErrorPayload> {
    let id = Uuid::new_v4().to_string();

    let sink: OutputSink = Box::new(move |output| {
        let event = match output {
            TerminalOutput::Data(bytes) => TerminalEvent::Data {
                chunk: BASE64.encode(bytes),
            },
            TerminalOutput::Exit(code) => TerminalEvent::Exit { code },
        };
        let _ = on_event.send(event);
    });

    // 按认证方式解出真正的凭据 -> 凭据库引用在这里才变成明文
    let auth = resolve_auth(&state.vault, &request.auth).await?;
    let jump = resolve_jump(&state.vault, request.jump.as_ref()).await?;

    let options = SshOptions {
        host: request.host.clone(),
        port: request.port,
        username: request.username.clone(),
        auth,
        cols: request.cols,
        rows: request.rows,
        term: "xterm-256color".to_string(),
        accept_unknown_host_key: request.accept_unknown_host_key,
        jump,
    };

    let store = state.store.clone();
    // 连接会阻塞在内部运行时上 -> 放到阻塞线程池，避免占住 async 工作线程
    let session = tauri::async_runtime::spawn_blocking(move || {
        SshSession::connect(&store, &options, sink).map(Arc::new)
    })
    .await
    .map_err(|err| {
        ErrorPayload::from(rustmobax_core::CoreError::Ssh(format!("任务异常：{err}")))
    })?
    .map_err(ErrorPayload::from)?;

    if !state.terminals.insert(id.clone(), session) {
        return Err(ErrorPayload::from(rustmobax_core::CoreError::Ssh(
            "终端注册表不可用".into(),
        )));
    }
    Ok(id)
}

/// 信任并记录一台主机的指纹。
///
/// 参数：host/port 定位目标，key_type 与 sha256 为用户在界面上核对过的值
/// 核心流程：用户点"信任"后写入 known_hosts，下次连接直接放行。
#[tauri::command]
pub async fn trust_host_key(
    state: State<'_, AppState>,
    host: String,
    port: u16,
    key_type: String,
    sha256: String,
) -> Result<(), ErrorPayload> {
    state
        .store
        .known_host_upsert(&host, port, &key_type, &sha256)
        .await
        .map_err(ErrorPayload::from)
}

/// 删除某台主机的指纹记录。
///
/// 参数：host/port 定位目标
/// 核心流程：指纹变更时用户必须显式删除旧记录，之后重新连接会再次要求确认。
#[tauri::command]
pub async fn forget_host_key(
    state: State<'_, AppState>,
    host: String,
    port: u16,
) -> Result<u64, ErrorPayload> {
    state
        .store
        .known_host_delete(&host, port, None)
        .await
        .map_err(ErrorPayload::from)
}

/// 列出所有已记录的主机指纹。
///
/// 参数：无
/// 核心流程：供界面展示与排查，按主机与端口排序。
#[tauri::command]
pub async fn list_known_hosts(
    state: State<'_, AppState>,
) -> Result<Vec<rustmobax_core::KnownHost>, ErrorPayload> {
    state
        .store
        .known_host_list()
        .await
        .map_err(ErrorPayload::from)
}

/// 列出本机可用的远程桌面客户端。
///
/// 参数：无
/// 核心流程：探测要扫 PATH 并判断可执行位（Linux 上可能几百个目录），
/// 放到阻塞线程池里跑，避免占住 async 工作线程；一个都没装时核心层回空列表，
/// 前端据「空列表」给出安装引导。
#[tauri::command]
pub async fn remote_desktop_clients(
) -> Result<Vec<rustmobax_core::RemoteDesktopClient>, ErrorPayload> {
    tauri::async_runtime::spawn_blocking(rustmobax_core::detect_clients)
        .await
        .map_err(|err| {
            ErrorPayload::from(rustmobax_core::CoreError::Terminal(format!(
                "任务异常：{err}"
            )))
        })
}

/// 用系统客户端打开一个远程桌面会话。
///
/// 参数：kind 为客户端类型，host / port / username 为目标会话信息
/// 核心流程：spawn 一个长驻 GUI 进程会阻塞到它退出，因此放在阻塞线程池里跑；
/// 密码不参与传递（由客户端自己弹框询问），避免口令出现在同机可见的进程列表里。
#[tauri::command]
pub async fn launch_remote_desktop(
    kind: String,
    host: String,
    port: u16,
    username: String,
) -> Result<(), ErrorPayload> {
    tauri::async_runtime::spawn_blocking(move || {
        rustmobax_core::launch(&kind, &host, port, &username)
    })
    .await
    .map_err(|err| {
        ErrorPayload::from(rustmobax_core::CoreError::Terminal(format!(
            "任务异常：{err}"
        )))
    })?
    .map_err(ErrorPayload::from)
}
