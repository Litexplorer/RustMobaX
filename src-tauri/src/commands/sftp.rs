use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;

use rustmobax_core::{
    CoreError, CoreResult, ErrorPayload, SftpClient, SftpEntry, SshOptions, TransferOutcome,
    DEFAULT_TRANSFER_CHUNK,
};
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::State;
use uuid::Uuid;

use crate::commands::terminal::{resolve_auth, resolve_jump, JumpRequest, SshAuthRequest};
use crate::state::AppState;

/// 文本读取上限。
///
/// 参数：无
/// 核心流程：编辑器和预览都只要文本，超过 1 MiB 的文件直接拒绝，
/// 避免 SFTP 把大文件整段拉进内存再塞给前端。
const MAX_TEXT_BYTES: usize = 1024 * 1024;

/// 建立 SFTP 会话时的请求参数。
///
/// 无参数
/// 核心流程：只带连接三要素与认证方式；cols/rows 由命令层填默认值，
/// 前端不必为文件面板关心终端尺寸；可选配一跳跳板机。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SftpConnectRequest {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SshAuthRequest,
    /// 用户已在界面上确认过该主机指纹。
    pub accept_unknown_host_key: bool,
    /// 可选的一跳跳板机；缺省即直连。
    #[serde(default)]
    pub jump: Option<JumpRequest>,
}

/// 按标识取一个 SFTP 会话。
///
/// 参数：state 为应用状态，id 为会话标识
/// 核心流程：命中即返回 Arc，未命中报「未找到」，前端据此提示重连。
fn client_of(state: &AppState, id: &str) -> Result<Arc<SftpClient>, ErrorPayload> {
    state
        .sftp
        .get(id)
        .ok_or_else(|| ErrorPayload::from(CoreError::NotFound(format!("SFTP 会话 {id}"))))
}

/// 在阻塞线程池里跑一个同步 SFTP 操作。
///
/// 参数：operation 为实际要执行的同步调用
/// 核心流程：SFTP 的同步包装内部会等异步运行时，不能占住 async 工作线程，
/// 因此统一丢给 spawn_blocking，再把 JoinError 归成 Sftp 分类。
async fn run_blocking<F, T>(operation: F) -> Result<T, ErrorPayload>
where
    F: FnOnce() -> rustmobax_core::CoreResult<T> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(|err| ErrorPayload::from(CoreError::Sftp(format!("任务异常：{err}"))))?
        .map_err(ErrorPayload::from)
}

/// 打开一个独立的 SFTP 会话。
///
/// 参数：request 为连接参数（含认证方式与凭据引用）
/// 核心流程：按认证方式解出凭据（凭据库引用在这里才变成明文）→
/// 在阻塞线程里用同一套 SshOptions 新建连接并开 sftp 子系统 →
/// 登记进注册表并返回标识。
#[tauri::command]
pub async fn sftp_connect(
    state: State<'_, AppState>,
    request: SftpConnectRequest,
) -> Result<String, ErrorPayload> {
    let auth = resolve_auth(&state.vault, &request.auth).await?;
    let jump = resolve_jump(&state.vault, request.jump.as_ref()).await?;

    // SFTP 不需要 PTY 尺寸，填终端默认值即可
    let options = SshOptions {
        host: request.host.clone(),
        port: request.port,
        username: request.username.clone(),
        auth,
        cols: 80,
        rows: 24,
        term: "xterm-256color".to_string(),
        accept_unknown_host_key: request.accept_unknown_host_key,
        jump,
    };

    let store = state.store.clone();
    let client = tauri::async_runtime::spawn_blocking(move || {
        SftpClient::connect(&store, &options).map(Arc::new)
    })
    .await
    .map_err(|err| ErrorPayload::from(CoreError::Sftp(format!("任务异常：{err}"))))?
    .map_err(ErrorPayload::from)?;

    let id = Uuid::new_v4().to_string();
    if !state.sftp.insert(id.clone(), client) {
        return Err(ErrorPayload::from(CoreError::Sftp(
            "SFTP 注册表不可用".into(),
        )));
    }
    Ok(id)
}

/// 列出远端目录。
///
/// 参数：id 为 SFTP 会话标识，path 为远端目录路径
/// 核心流程：取会话 → 阻塞执行 list → 返回已排序的条目。
#[tauri::command]
pub async fn sftp_list(
    state: State<'_, AppState>,
    id: String,
    path: String,
) -> Result<Vec<SftpEntry>, ErrorPayload> {
    let client = client_of(&state, &id)?;
    run_blocking(move || client.list(&path)).await
}

/// 以文本方式读取远端文件。
///
/// 参数：id 为 SFTP 会话标识，path 为远端文件路径
/// 核心流程：按上限读取字节 → 含 NUL 视为二进制直接拒绝 →
/// 再按 UTF-8 解码，失败说明不是文本文件。
#[tauri::command]
pub async fn sftp_read_text(
    state: State<'_, AppState>,
    id: String,
    path: String,
) -> Result<String, ErrorPayload> {
    let client = client_of(&state, &id)?;
    let target = path.clone();
    let bytes = run_blocking(move || client.read_file(&target, MAX_TEXT_BYTES)).await?;

    if bytes.contains(&0u8) {
        return Err(ErrorPayload::from(CoreError::Sftp(format!(
            "{path} 疑似二进制文件，无法按文本打开"
        ))));
    }
    String::from_utf8(bytes).map_err(|err| {
        ErrorPayload::from(CoreError::Sftp(format!(
            "{path} 不是 UTF-8 文本，无法按文本打开：{err}"
        )))
    })
}

/// 以文本方式写入远端文件。
///
/// 参数：id 为 SFTP 会话标识，path 为远端文件路径，content 为文本内容
/// 核心流程：整段覆盖写入，不做换行转换（保持前端传进来的字节）。
#[tauri::command]
pub async fn sftp_write_text(
    state: State<'_, AppState>,
    id: String,
    path: String,
    content: String,
) -> Result<(), ErrorPayload> {
    let client = client_of(&state, &id)?;
    run_blocking(move || client.write_file(&path, content.as_bytes())).await
}

/// 新建远端目录。
///
/// 参数：id 为 SFTP 会话标识，path 为远端目录路径
/// 核心流程：透传给核心层的 MKDIR，父目录不存在时由服务端状态码报错。
#[tauri::command]
pub async fn sftp_mkdir(
    state: State<'_, AppState>,
    id: String,
    path: String,
) -> Result<(), ErrorPayload> {
    let client = client_of(&state, &id)?;
    run_blocking(move || client.create_dir(&path)).await
}

/// 删除远端文件或目录。
///
/// 参数：id 为 SFTP 会话标识，path 为远端路径，is_dir 表示按目录删除
/// 核心流程：目录走 RMDIR（只删空目录），文件走 REMOVE，类型由前端按列表结果给出。
#[tauri::command]
pub async fn sftp_remove(
    state: State<'_, AppState>,
    id: String,
    path: String,
    is_dir: bool,
) -> Result<(), ErrorPayload> {
    let client = client_of(&state, &id)?;
    run_blocking(move || client.remove(&path, is_dir)).await
}

/// 重命名或移动远端条目。
///
/// 参数：id 为 SFTP 会话标识，from 为原路径，to 为新路径
/// 核心流程：直接发 RENAME，同目录改名与跨目录移动都走这里。
#[tauri::command]
pub async fn sftp_rename(
    state: State<'_, AppState>,
    id: String,
    from: String,
    to: String,
) -> Result<(), ErrorPayload> {
    let client = client_of(&state, &id)?;
    run_blocking(move || client.rename(&from, &to)).await
}

/// 关闭 SFTP 会话。
///
/// 参数：id 为 SFTP 会话标识
/// 核心流程：先从注册表摘下再关连接，重复调用不报错（未命中直接返回成功）。
#[tauri::command]
pub async fn sftp_close(state: State<'_, AppState>, id: String) -> Result<(), ErrorPayload> {
    if let Some(client) = state.sftp.remove(&id) {
        tauri::async_runtime::spawn_blocking(move || client.close())
            .await
            .map_err(|err| ErrorPayload::from(CoreError::Sftp(format!("任务异常：{err}"))))?;
    }
    Ok(())
}

/// 二进制读取的默认上限。
///
/// 参数：无
/// 核心流程：文本编辑器与上传下载共用一条通道，缺省 8 MiB，
/// 调用方可以传更小的值来限制；再大就要分片，本轮不支持。
const MAX_BINARY_BYTES: u64 = 8 * 1024 * 1024;

/// 按 base64 读取远端文件。
///
/// 参数：id 为 SFTP 会话标识，path 为远端路径，max_bytes 为可选上限
/// 核心流程：先看上限 -> 读字节 -> base64 编码；用字节通道而不是文本通道，
/// 图片、压缩包这类内容才不会被 UTF-8 校验挡掉。
#[tauri::command]
pub async fn sftp_read_file_base64(
    state: State<'_, AppState>,
    id: String,
    path: String,
    max_bytes: Option<u64>,
) -> Result<String, ErrorPayload> {
    let client = client_of(&state, &id)?;
    let limit = max_bytes.unwrap_or(MAX_BINARY_BYTES).min(MAX_BINARY_BYTES) as usize;
    run_blocking(move || client.read_file(&path, limit))
        .await
        .map(|bytes| BASE64.encode(bytes))
}

/// 按 base64 写入远端文件。
///
/// 参数：id 为 SFTP 会话标识，path 为远端路径，data 为 base64 文本
/// 核心流程：先解码（坏数据不落远端）-> 走字节写通道覆盖写入。
#[tauri::command]
pub async fn sftp_write_file_base64(
    state: State<'_, AppState>,
    id: String,
    path: String,
    data: String,
) -> Result<(), ErrorPayload> {
    let client = client_of(&state, &id)?;
    let bytes = BASE64.decode(data.as_bytes()).map_err(|_| {
        ErrorPayload::from(CoreError::Validation("文件内容不是合法的 base64".into()))
    })?;
    run_blocking(move || client.write_file(&path, &bytes)).await
}

/// 传输进度推送的字节间隔（1 MiB）。
///
/// 参数：无
/// 核心流程：小块文件没必要每块都推，攒够 1 MiB 再发一条，
/// 既能让进度条动起来，又不会把 IPC 刷爆。
const PROGRESS_EVERY_BYTES: u64 = 1024 * 1024;

/// 传输进度推送的时间间隔（200 ms）。
///
/// 参数：无
/// 核心流程：慢速链路上 1 MiB 可能要走好几秒，用时间兜底保证
/// 至少每 200 ms 有一条进度，用户不会以为界面卡死。
const PROGRESS_EVERY_TIME: Duration = Duration::from_millis(200);

/// 一次传输的生命周期事件。
///
/// 无参数
/// 核心流程：按 `kind` 区分三种事件，前端用同一条 Channel 收进度与收尾，
/// 不必自己猜测哪次 invoke 对应哪次回调。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TransferEvent {
    /// 已累计写入的字节数（含续传部分）。
    Progress { bytes: u64 },
    /// 全部写完并已改名成目标文件。
    Done { bytes: u64 },
    /// 失败或被取消，message 直接可展示。
    Error { message: String },
}

/// 传输事件出口：负责节流，并保证结束事件一定发出去。
///
/// 无参数
/// 核心流程：持有前端的事件通道与上次推送的时间/字节数；
/// 进度按「1 MiB 或 200 ms」节流，结束事件不节流。
/// 通道是必填参数：tauri 2 只给 `Channel<T>` 本身实现了 CommandArg，
/// `Option<Channel<T>>` 这种写法过不了类型检查（前端每次传一个也就够了）。
struct EventSink {
    channel: Channel<TransferEvent>,
    /// 上次推送时的累计字节数
    last_bytes: u64,
    /// 上次推送的时刻
    last_at: Instant,
}

impl EventSink {
    /// 建一个事件出口。
    ///
    /// 参数：channel 为前端传来的事件通道
    /// 核心流程：把节流基线清零，时间基线取当前时刻。
    fn new(channel: Channel<TransferEvent>) -> Self {
        Self {
            channel,
            last_bytes: 0,
            last_at: Instant::now(),
        }
    }

    /// 推一条进度。
    ///
    /// 参数：bytes 为当前累计写入字节数
    /// 核心流程：字节增量不足 1 MiB 且距上次不足 200 ms 时直接丢掉，
    /// 否则刷新基线并推送。
    fn progress(&mut self, bytes: u64) {
        let advanced = bytes.saturating_sub(self.last_bytes) >= PROGRESS_EVERY_BYTES;
        if !advanced && self.last_at.elapsed() < PROGRESS_EVERY_TIME {
            return;
        }
        self.last_bytes = bytes;
        self.last_at = Instant::now();
        self.send(TransferEvent::Progress { bytes });
    }

    /// 推一条结束事件。
    ///
    /// 参数：outcome 为传输结果
    /// 核心流程：成功推 done（用「续传起点 + 本次写入」当总字节数，
    /// 服务端没报大小也能给前端一个准确值），失败推 error 并带上原始文案。
    fn finish(&mut self, outcome: &CoreResult<TransferOutcome>) {
        match outcome {
            Ok(result) => self.send(TransferEvent::Done {
                bytes: result.resumed_from + result.written,
            }),
            Err(err) => self.send(TransferEvent::Error {
                message: err.to_string(),
            }),
        }
    }

    /// 真正发一条事件。
    ///
    /// 参数：event 为要发的事件
    /// 核心流程：发送失败（前端已卸载）也算正常，传输不该因为收不到进度而失败。
    fn send(&self, event: TransferEvent) {
        let _ = self.channel.send(event);
    }
}

/// 跑一次带进度、取消与事件推送的传输。
///
/// 参数：state 为应用状态，cancel_token 为前端生成的取消令牌，
/// on_event 为事件通道，run 为实际传输（入参是进度回调与取消标志）
/// 核心流程：先把令牌登记进取消表拿到标志 → 在阻塞线程里跑传输，
/// 进度经 EventSink 节流后推给前端 → 无论成败都推一条 done/error →
/// 最后把令牌从取消表移除（重复取消也不会留下垃圾）。
async fn run_transfer<F>(
    state: State<'_, AppState>,
    cancel_token: String,
    on_event: Channel<TransferEvent>,
    run: F,
) -> Result<TransferOutcome, ErrorPayload>
where
    F: FnOnce(&mut dyn FnMut(u64), &AtomicBool) -> CoreResult<TransferOutcome> + Send + 'static,
{
    let cancel = state.transfers.register(&cancel_token);
    let mut sink = EventSink::new(on_event);

    let result = tauri::async_runtime::spawn_blocking(move || {
        // 进度闭包借用 sink，跑完立刻释放借用，后面才能发结束事件
        let outcome = {
            let mut progress = |bytes: u64| sink.progress(bytes);
            run(&mut progress, &cancel)
        };
        sink.finish(&outcome);
        outcome
    })
    .await
    .map_err(|err| ErrorPayload::from(CoreError::Sftp(format!("任务异常：{err}"))))?
    .map_err(ErrorPayload::from);

    state.transfers.finish(&cancel_token);
    result
}

/// 把一个本地文件流式上传到远端。
///
/// 参数：id 为 SFTP 会话标识，remote_path 为远端目标文件路径，local_path 为本机文件路径，
/// resume 为真时接着残留的 `.rmxpart` 续传，cancel_token 为取消令牌，
/// on_event 为进度事件通道
/// 核心流程：取会话 → 在阻塞线程里按 DEFAULT_TRANSFER_CHUNK 分块拷到 `<目标>.rmxpart`
/// （内容不进 IPC、不限大小）→ 成功后改名成目标并复刻本机属性 →
/// 返回本次写入量、总大小与续传起点。远端父目录不存在时直接报错，不做静默创建。
#[tauri::command]
pub async fn sftp_upload_local_file(
    state: State<'_, AppState>,
    id: String,
    remote_path: String,
    local_path: String,
    resume: bool,
    cancel_token: String,
    on_event: Channel<TransferEvent>,
) -> Result<TransferOutcome, ErrorPayload> {
    let client = client_of(&state, &id)?;
    run_transfer(state, cancel_token, on_event, move |progress, cancel| {
        client.upload_file_from_with(
            &remote_path,
            Path::new(&local_path),
            DEFAULT_TRANSFER_CHUNK,
            resume,
            progress,
            cancel,
        )
    })
    .await
}

/// 把一个远端文件流式下载到本地。
///
/// 参数：id 为 SFTP 会话标识，remote_path 为远端源文件路径，local_path 为本机落盘路径，
/// resume 为真时接着本地残留的 `.rmxpart` 续传，cancel_token 为取消令牌，
/// on_event 为进度事件通道
/// 核心流程：取会话 → 在阻塞线程里分块拷到 `<目标>.rmxpart` → 成功后改名成目标 →
/// 返回本次写入量、总大小与续传起点。本地父目录不存在时直接报错，不做静默创建。
#[tauri::command]
pub async fn sftp_download_remote_file(
    state: State<'_, AppState>,
    id: String,
    remote_path: String,
    local_path: String,
    resume: bool,
    cancel_token: String,
    on_event: Channel<TransferEvent>,
) -> Result<TransferOutcome, ErrorPayload> {
    let client = client_of(&state, &id)?;
    run_transfer(state, cancel_token, on_event, move |progress, cancel| {
        client.download_file_to_with(
            &remote_path,
            Path::new(&local_path),
            DEFAULT_TRANSFER_CHUNK,
            resume,
            progress,
            cancel,
        )
    })
    .await
}

/// 取消一次正在进行的传输。
///
/// 参数：cancel_token 为前端生成、随上传/下载请求发给后端的令牌
/// 核心流程：把取消表里对应标志置真，拷贝循环在当前这块写完后退出并返回取消错误；
/// 令牌不存在（已结束、从没开始）时什么也不做，因此重复取消不算错误。
#[tauri::command]
pub fn sftp_cancel_transfer(
    state: State<'_, AppState>,
    cancel_token: String,
) -> Result<(), ErrorPayload> {
    state.transfers.cancel(&cancel_token);
    Ok(())
}
