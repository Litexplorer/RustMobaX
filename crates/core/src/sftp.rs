//! SFTP 客户端：在一条独立 SSH 连接上开 sftp 子系统，提供列表与文件操作。
//!
//! 协议编解码交给 russh-sftp（它只依赖「字节流」不绑定 russh 版本），
//! 本模块负责把异步会话包装成同步 API，并统一成 CoreError。

use std::future::Future;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileAttributes, OpenFlags};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::error::{CoreError, CoreResult};
use crate::ssh::{runtime, SshConnection, SshOptions};
use crate::store::Store;

/// 单个 SFTP 请求的等待上限。
///
/// 参数：无
/// 核心流程：所有同步包装都按这个上限等待，避免服务端无响应时永久卡住调用线程。
const OPERATION_TIMEOUT: Duration = Duration::from_secs(60);

/// 流式传输的默认分块大小（256 KiB）。
///
/// 参数：无
/// 核心流程：与 russh-sftp 的单包上限（256 KiB）对齐，一块正好一个请求，
/// 库内不会再切一刀；内存占用只与这个常量有关，与文件大小无关。
pub const DEFAULT_TRANSFER_CHUNK: usize = 256 * 1024;

/// 传输中途落盘的临时文件后缀。
///
/// 参数：无
/// 核心流程：传输先写 `<目标>.rmxpart`，成功后再改名成目标；
/// 中途中断时目标名上永远不会出现半截文件，残留的 `.rmxpart` 就是续传起点。
pub const PART_SUFFIX: &str = ".rmxpart";

/// 一次流式传输的账目。
///
/// 无参数
/// 核心流程：`written` 只算本次写入，`total` 是整份文件的大小，
/// `resumed_from` 是本次起点偏移；前端据此区分「这次传了多少」与「文件多大」。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferOutcome {
    /// 本次实际写入的字节数（不含续传跳过的部分）。
    pub written: u64,
    /// 整份文件的总字节数，对端没给大小时为 0。
    pub total: u64,
    /// 本次从第几个字节开始写，0 表示从头写。
    pub resumed_from: u64,
}

/// 一次流式传输的中间结论。
///
/// 无参数
/// 核心流程：区分「正常读完」与「被取消」两种收尾方式，
/// 前者改名成目标、后者保留临时文件供续传，避免用错误文案做字符串判断。
enum TransferStep {
    /// 正常读完，携带本次写入字节数。
    Completed(u64),
    /// 收到取消信号，携带本次已写入字节数。
    Cancelled(u64),
}

/// 目录里的一项。
///
/// 无参数
/// 核心流程：把 SFTP 的属性包压成前端表格需要的字段，时间用 Unix 秒。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SftpEntry {
    /// 文件名（不含父目录）。
    pub name: String,
    /// 完整路径（父目录 + 文件名，已按 `/` 拼接）。
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    /// 权限位（含文件类型位，例如目录是 0o040755）。
    pub permissions: u32,
    /// 修改时间（Unix 秒），服务端未提供时为 0。
    pub modified: i64,
}

/// 一个 SFTP 会话。
///
/// 无参数
/// 核心流程：独占一条 SSH 连接（与 shell 的连接互不影响），
/// 内部持有 russh-sftp 会话，克隆出的副本共享同一连接与请求队列。
#[derive(Clone)]
pub struct SftpClient {
    connection: SshConnection,
    session: Arc<SftpSession>,
}

/// 把 russh-sftp 的错误收敛成 CoreError。
///
/// 参数：action 为动作描述（含目标路径），err 为底层 SFTP 错误
/// 核心流程：统一拼成「动作失败：原因」，外层加前缀后即为前端看到的文案。
fn to_core_error(action: &str, err: SftpError) -> CoreError {
    CoreError::Sftp(format!("{action}失败：{err}"))
}

/// 路径归一化。
///
/// 参数：path 为前端传来的远端路径
/// 核心流程：去掉首尾空白，空串按家目录处理（SFTP 允许空路径表示默认目录）。
fn normalize_path(path: &str) -> String {
    path.trim().to_string()
}

/// 在常驻运行时上跑一段 SFTP 异步流程并同步等待结果。
///
/// 参数：label 为动作名（用于超时与断连文案），future 为实际异步操作
/// 核心流程：把 future 派发到常驻运行时，同步侧用一次性通道等结果；
/// 超时与「任务没回结果就结束」两种异常都给独立文案。
fn run_blocking<T, F>(label: &str, future: F) -> CoreResult<T>
where
    F: Future<Output = CoreResult<T>> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    runtime().spawn(async move {
        let _ = tx.send(future.await);
    });
    match rx.recv_timeout(OPERATION_TIMEOUT) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(CoreError::Sftp(format!(
            "{label}超时（{} 秒无响应）",
            OPERATION_TIMEOUT.as_secs()
        ))),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(CoreError::Sftp(format!("{label}失败：SFTP 连接已断开")))
        }
    }
}

/// 在当前线程直接驱动一段需要借用外部回调的异步流程。
///
/// 参数：_label 为动作名（保留在签名里，便于将来定位是哪个操作卡住），future 为实际异步操作
/// 核心流程：用常驻运行时 block_on 把 future 就地跑完，因此 future 可以借用
/// 进度回调与取消标志，不必做成 `'static + Send`（这是传输要支持进度与取消的前提）；
/// 代价是调用线程必须自己能阻塞，命令层统一走 spawn_blocking。
///
/// 这里刻意不判断「是否处在异步上下文」：tokio 的阻塞线程池在工作线程里也会进入运行时
/// 上下文（`Handle::try_current()` 同样为真），用那个信号判断会把 spawn_blocking 里的
/// 正常调用全部误判成异步上下文，传输在写出第一个字节前就被拒。
/// 真在 async worker 上误用会在 block_on 处由 tokio 自己 panic，属于开发期立刻能暴露的错误，
/// 比一条在生产路径上必然误伤的守卫可靠。
fn run_borrowed<T, F>(_label: &str, future: F) -> CoreResult<T>
where
    F: Future<Output = CoreResult<T>>,
{
    runtime().block_on(future)
}

/// 拼远端临时文件路径。
///
/// 参数：remote 为已归一化的远端目标路径
/// 核心流程：直接在目标名后接 `.rmxpart`，与目标同目录，
/// 这样收尾的 RENAME 不会跨目录。
fn remote_part_path(remote: &str) -> String {
    format!("{remote}{PART_SUFFIX}")
}

/// 拼本地临时文件路径。
///
/// 参数：local 为本地目标路径
/// 核心流程：往 OsString 上追加后缀再还原成 PathBuf，
/// 全程不经过 UTF-8 转换，Windows 上的非 UTF-8 路径也不会丢字符。
fn local_part_path(local: &Path) -> PathBuf {
    let mut name = local.as_os_str().to_os_string();
    name.push(PART_SUFFIX);
    PathBuf::from(name)
}

/// 构造取消错误。
///
/// 参数：bytes 为取消时已累计写入的字节数
/// 核心流程：文案固定带字节数，前端可以直接提示「已传 N 字节，可续传」。
fn cancelled_error(bytes: u64) -> CoreError {
    CoreError::Sftp(format!("传输已取消（已写入 {bytes} 字节）"))
}

/// 拼「数据已保留在临时文件」的错误。
///
/// 参数：part 为临时文件路径，reason 为失败原因
/// 核心流程：统一在原因后面补上「数据已完整保留在 <临时文件>，可用续传继续」，
/// 保证任何收尾失败都不会让用户以为数据丢了、也不必猜续传还能不能用。
fn part_kept_error(part: &str, reason: String) -> CoreError {
    CoreError::Sftp(format!("{reason}，数据已完整保留在 {part}，可用续传继续"))
}

/// 收尾改名：目标已存在时按「先删旧文件再改名」兜底。
///
/// 参数：session 为 SFTP 会话，part 为临时文件路径，target 为最终路径，
/// total 为期望的总字节数，first 为第一次 RENAME 的失败原因
/// 核心流程：SFTP v3 的 RENAME 不保证覆盖已存在的目标，OpenSSH 的 sftp-server 在目标存在时
/// 直接回 SSH_FX_FAILURE，而 russh-sftp 3.0.1 的高层会话没有 posix-rename@openssh.com 的封装，
/// 所以这里自己兜底：目标不存在说明失败另有原因，原样报回；目标存在则先确认 `.rmxpart`
/// 大小等于期望总大小（删旧文件与改名之间有个窗口，临时文件不完整就不删，宁可让用户续传补齐），
/// 再删目标、重试一次改名。任一步失败都保留 `.rmxpart`，续传语义不受影响。
async fn rename_part_into_place(
    session: &SftpSession,
    part: &str,
    target: &str,
    total: u64,
    first: SftpError,
) -> CoreResult<()> {
    // 目标不存在 -> 失败另有原因（权限、跨设备、父目录没了），别用删除掩盖真实错误
    if session.metadata(target.to_string()).await.is_err() {
        return Err(part_kept_error(
            part,
            format!("远端临时文件 {part} 改名为 {target} 失败（{first}）"),
        ));
    }

    // 删旧目标之前先确认临时文件是完整的：否则删了旧文件、改名又失败，等于两个都没了
    let part_size = session
        .metadata(part.to_string())
        .await
        .map(|meta| meta.size.unwrap_or(0))
        .unwrap_or(0);
    if part_size != total {
        return Err(part_kept_error(
            part,
            format!(
                "目标 {target} 已存在且无法覆盖（改名被拒绝：{first}），远端临时文件只有 {part_size} 字节、期望 {total} 字节，为避免删掉旧文件后改名再失败已中止"
            ),
        ));
    }

    session
        .remove_file(target.to_string())
        .await
        .map_err(|err| {
            part_kept_error(
                part,
                format!(
                    "目标 {target} 已存在且无法覆盖（改名被拒绝：{first}；删除旧文件失败：{err}）"
                ),
            )
        })?;

    session
        .rename(part.to_string(), target.to_string())
        .await
        .map_err(|err| {
            part_kept_error(
                part,
                format!("远端临时文件 {part} 改名为 {target} 失败（{err}）"),
            )
        })
}

/// 尽力删除远端残留文件。
///
/// 参数：session 为 SFTP 会话，path 为要删的远端路径
/// 核心流程：删不掉只打日志：清理失败是脏数据，不该盖住真正的传输错误。
async fn delete_remote_quietly(session: &SftpSession, path: &str) {
    if let Err(err) = session.remove_file(path.to_string()).await {
        eprintln!("清理远端临时文件 {path} 失败：{err}");
    }
}

/// 尽力删除本地残留文件。
///
/// 参数：path 为要删的本地路径
/// 核心流程：文件本来就不在（NotFound）算成功，其它失败只打日志。
fn delete_local_quietly(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => eprintln!("清理本地临时文件 {} 失败：{err}", path.display()),
    }
}

/// 取本地文件的权限位。
///
/// 参数：meta 为本地文件的元数据
/// 核心流程：Unix 下取低 12 位（含 setuid/setgid/sticky），其它平台返回 None，
/// 因为 Windows 没有可直接映射到 SFTP 的 POSIX 权限位。
#[cfg(unix)]
fn local_mode(meta: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(meta.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn local_mode(_meta: &std::fs::Metadata) -> Option<u32> {
    None
}

/// 从本地文件属性推出要在远端复刻的属性。
///
/// 参数：meta 为本地文件的 symlink_metadata 结果
/// 核心流程：权限位取本地模式，修改时间取秒级 mtime（SFTP v3 只认 u32 秒）；
/// 取不到 mtime 时只带权限，两者都没有时返回空属性，调用方据此跳过 SETSTAT。
fn inherited_attrs(meta: &std::fs::Metadata) -> FileAttributes {
    let mut attrs = FileAttributes::default();
    attrs.permissions = local_mode(meta);
    if let Ok(modified) = meta.modified() {
        if let Ok(since) = modified.duration_since(std::time::UNIX_EPOCH) {
            attrs.mtime = Some(since.as_secs().min(u64::from(u32::MAX)) as u32);
        }
    }
    attrs
}

/// 取远端路径的父目录。
///
/// 参数：path 为已归一化的远端路径
/// 核心流程：按最后一个 `/` 切分：`/a/b` 得 `/a`，`/a` 得 `/`，
/// 不含 `/` 的相对名返回 None（此时由服务端默认目录接管，不做校验）。
fn remote_parent(path: &str) -> Option<String> {
    match path.rfind('/') {
        Some(0) => Some("/".to_string()),
        Some(index) => Some(path[..index].to_string()),
        None => None,
    }
}

impl SftpClient {
    /// 建立一条独立的 SFTP 会话。
    ///
    /// 参数：store 为已知主机库，options 为连接参数（含认证方式与凭据）
    /// 核心流程：先按 SshOptions 完整握手并认证出一条新连接 →
    /// 在其上开 sftp 子系统通道 → 把它包成字节流做 SFTP v3 握手（INIT/VERSION）；
    /// 任一步失败都断开整条连接，避免留下半开连接。
    pub fn connect(store: &Store, options: &SshOptions) -> CoreResult<Self> {
        let connection = SshConnection::connect(store, options)?;
        let connection_for_open = connection.clone();
        let connection_for_cleanup = connection.clone();

        let session = run_blocking("打开 SFTP 会话", async move {
            let channel = connection_for_open.open_subsystem("sftp").await?;
            let session = SftpSession::new(channel.into_stream())
                .await
                .map_err(|err| CoreError::Sftp(format!("SFTP 握手失败：{err}")))?;
            Ok(Arc::new(session))
        });

        match session {
            Ok(session) => Ok(Self {
                connection,
                session,
            }),
            Err(err) => {
                // 握手失败 -> 直接把这条连接断掉，别让它挂在注册表之外
                runtime().spawn(async move {
                    let _ = connection_for_cleanup.disconnect().await;
                });
                Err(err)
            }
        }
    }

    /// 列出目录内容。
    ///
    /// 参数：path 为远端目录路径
    /// 核心流程：OPENDIR 拿句柄 → 循环 READDIR 直到服务端回 EOF →
    /// 把每项属性映射成 SftpEntry，按「目录在前、再按名字」排序后返回。
    pub fn list(&self, path: &str) -> CoreResult<Vec<SftpEntry>> {
        let session = Arc::clone(&self.session);
        let target = normalize_path(path);
        run_blocking("列目录", async move {
            let target_for_error = target.clone();
            let read_dir = session.read_dir(target).await.map_err(|err| {
                to_core_error(&format!("列目录 {target_for_error} "), err)
            })?;

            let mut entries = Vec::new();
            for entry in read_dir {
                let metadata = entry.metadata();
                entries.push(SftpEntry {
                    name: entry.file_name(),
                    path: entry.path(),
                    is_dir: metadata.is_dir(),
                    size: metadata.size.unwrap_or(0),
                    permissions: metadata.permissions.unwrap_or(0),
                    modified: metadata.mtime.map(i64::from).unwrap_or(0),
                });
            }
            // 目录在前，同类按名字排 -> 前端不用再排一遍
            entries.sort_by(|left, right| {
                right
                    .is_dir
                    .cmp(&left.is_dir)
                    .then_with(|| left.name.cmp(&right.name))
            });
            Ok(entries)
        })
    }

    /// 读取整个文件。
    ///
    /// 参数：path 为远端文件路径，max_bytes 为允许读入内存的最大字节数
    /// 核心流程：OPEN 后先用 FSTAT 保守判断大小（超过上限直接报错）→
    /// 再按上限 +1 字节封顶读取，读到的实际长度仍超限同样报错，
    /// 这样即使服务端不返回 size 也不会把大文件拖进内存。
    pub fn read_file(&self, path: &str, max_bytes: usize) -> CoreResult<Vec<u8>> {
        let session = Arc::clone(&self.session);
        let target = normalize_path(path);
        run_blocking("读取文件", async move {
            let target_for_error = target.clone();
            let mut file = session
                .open(target)
                .await
                .map_err(|err| to_core_error(&format!("打开 {target_for_error} "), err))?;

            // 服务端不支持 fstat 时忽略，后面的读取上限仍然兜底
            let declared_too_big = file
                .metadata()
                .await
                .map(|metadata| metadata.size.unwrap_or(0) > max_bytes as u64)
                .unwrap_or(false);
            if declared_too_big {
                return Err(CoreError::Sftp(format!(
                    "文件 {target_for_error} 超过读取上限（{} 字节）",
                    max_bytes
                )));
            }

            let mut buffer = Vec::new();
            let cap = max_bytes.saturating_add(1) as u64;
            (&mut file)
                .take(cap)
                .read_to_end(&mut buffer)
                .await
                .map_err(|err| CoreError::Sftp(format!("读取 {target_for_error} 失败：{err}")))?;

            if buffer.len() > max_bytes {
                return Err(CoreError::Sftp(format!(
                    "文件 {target_for_error} 超过读取上限（{} 字节）",
                    max_bytes
                )));
            }

            let _ = file.close().await;
            Ok(buffer)
        })
    }

    /// 写入（覆盖）整个文件。
    ///
    /// 参数：path 为远端文件路径，bytes 为要写入的字节
    /// 核心流程：以 CREATE + WRITE + TRUNCATE 打开 → 整段写满 → 关闭句柄，
    /// 关闭同时等待服务端确认写入，避免"返回成功但数据还在飞"。
    pub fn write_file(&self, path: &str, bytes: &[u8]) -> CoreResult<()> {
        let session = Arc::clone(&self.session);
        let target = normalize_path(path);
        let payload = bytes.to_vec();
        run_blocking("写入文件", async move {
            let target_for_error = target.clone();
            let mut file = session
                .create(target)
                .await
                .map_err(|err| to_core_error(&format!("创建 {target_for_error} "), err))?;
            file.write_all(&payload)
                .await
                .map_err(|err| CoreError::Sftp(format!("写入 {target_for_error} 失败：{err}")))?;
            file.close()
                .await
                .map_err(|err| CoreError::Sftp(format!("关闭 {target_for_error} 失败：{err}")))?;
            Ok(())
        })
    }

    /// 把本地文件流式上传到远端（不限文件大小，内容不经过内存整抄）。
    ///
    /// 参数：remote_path 为远端目标文件路径，local_path 为本地源文件路径，
    /// chunk 为分块字节数（0 表示用 DEFAULT_TRANSFER_CHUNK）
    /// 核心流程：薄封装，固定「不续传、无进度、永不取消」，
    /// 内部转调 upload_file_from_with，返回本次写入字节数；
    /// 保留这条签名是为了不影响已有调用点（例如批量上传）。
    pub fn upload_file_from(
        &self,
        remote_path: &str,
        local_path: &Path,
        chunk: usize,
    ) -> CoreResult<u64> {
        let never_cancel = AtomicBool::new(false);
        self.upload_file_from_with(
            remote_path,
            local_path,
            chunk,
            false,
            &mut |_| {},
            &never_cancel,
        )
        .map(|outcome| outcome.written)
    }

    /// 把本地文件流式上传到远端，支持临时文件、续传、进度、取消与属性继承。
    ///
    /// 参数：remote_path 为远端目标文件路径，local_path 为本地源文件路径，
    /// chunk 为分块字节数（0 表示用 DEFAULT_TRANSFER_CHUNK），resume 为真时接着残留的
    /// `.rmxpart` 续传，progress 为进度回调（参数含续传部分，是累计写入字节），
    /// cancel 为取消标志（置真后当前这块写完就收手）
    /// 核心流程：先校验本地源（符号链接直接拒绝，免得写穿到链接目标）→ 远端父目录必须存在 →
    /// 写 `<目标>.rmxpart`（resume 且残留存在时以上次大小作为起点，否则清零重写）→
    /// 循环「读一块本地 / 写一块远端 / 回调进度」，每块之后检查取消 →
    /// 关闭句柄 → 改名成目标（目标已存在且服务端拒绝覆盖时，校验临时文件完整后先删目标再改名）→
    /// SETSTAT 复刻本机权限与 mtime。
    /// 取消时保留 `.rmxpart` 供下次续传，其它失败尽量删掉 `.rmxpart`，目标名上永不出现半截文件。
    /// 峰值内存只有一块缓冲区，与文件大小无关。
    pub fn upload_file_from_with(
        &self,
        remote_path: &str,
        local_path: &Path,
        chunk: usize,
        resume: bool,
        progress: &mut dyn FnMut(u64),
        cancel: &AtomicBool,
    ) -> CoreResult<TransferOutcome> {
        let session = Arc::clone(&self.session);
        let target = normalize_path(remote_path);
        let source = local_path.to_path_buf();
        let part = remote_part_path(&target);
        let block = if chunk == 0 {
            DEFAULT_TRANSFER_CHUNK
        } else {
            chunk
        };

        // symlink_metadata 不跟随链接：上传链接本身会把内容写进链接目标，这里直接拦下
        let local_meta = std::fs::symlink_metadata(&source).map_err(|err| {
            CoreError::Sftp(format!("本地文件 {} 打不开：{err}", source.display()))
        })?;
        if local_meta.file_type().is_symlink() {
            return Err(CoreError::Validation(format!(
                "不支持上传符号链接：{}",
                source.display()
            )));
        }
        // 属性在传输前就取好，避免传完再去看本地文件（那时可能已经被改动）
        let total = local_meta.len();
        let attrs = inherited_attrs(&local_meta);

        run_borrowed("上传文件", async move {
            let mut local = std::fs::File::open(&source).map_err(|err| {
                CoreError::Sftp(format!("本地文件 {} 打不开：{err}", source.display()))
            })?;

            // 父目录不存在就先拦下：远端建文件会回 NoSuchFile，但文案说不清原因
            if let Some(parent) = remote_parent(&target) {
                if session.metadata(parent.clone()).await.is_err() {
                    return Err(CoreError::Sftp(format!(
                        "远端目录 {parent} 不存在或不可访问，请先新建目录再上传到 {target}"
                    )));
                }
            }

            // 续传起点：有残留就接着写；没有残留（或没要求续传）就从 0 开始，不报错
            let offset = if resume {
                session
                    .metadata(part.clone())
                    .await
                    .map(|meta| meta.size.unwrap_or(0))
                    .unwrap_or(0)
            } else {
                0
            };
            // 残留比源文件还大 -> 源文件变了，那半截数据不可信，从头来
            let offset = if offset > total { 0 } else { offset };

            let flags = if offset == 0 {
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE
            } else {
                OpenFlags::WRITE | OpenFlags::CREATE
            };
            let mut remote = session
                .open_with_flags(part.clone(), flags)
                .await
                .map_err(|err| to_core_error(&format!("远端创建 {part} "), err))?;

            if offset > 0 {
                // 远端写指针也要移到续传起点：SFTP 的写请求带显式偏移，
                // 只是不截断文件还不够，位置不对会把整段数据盖到文件头上
                remote.seek(SeekFrom::Start(offset)).await.map_err(|err| {
                    CoreError::Sftp(format!(
                        "远端临时文件 {part} 定位到第 {offset} 字节失败：{err}"
                    ))
                })?;
                local.seek(SeekFrom::Start(offset)).map_err(|err| {
                    CoreError::Sftp(format!(
                        "本地文件 {} 定位到第 {offset} 字节失败：{err}",
                        source.display()
                    ))
                })?;
            }

            // 循环单独放在一段 async 里：它的结果要交给外层决定删残留还是保留
            let step = async {
                let mut buffer = vec![0u8; block];
                let mut written: u64 = 0;
                loop {
                    if cancel.load(Ordering::SeqCst) {
                        // 先把已写入的数据冲给服务端，再留着临时文件等下次续传
                        let _ = remote.close().await;
                        return Ok(TransferStep::Cancelled(written));
                    }
                    let read = local.read(&mut buffer).map_err(|err| {
                        CoreError::Sftp(format!(
                            "读取本地文件 {} 中断（已上传 {} 字节）：{err}",
                            source.display(),
                            offset + written
                        ))
                    })?;
                    if read == 0 {
                        break;
                    }
                    remote.write_all(&buffer[..read]).await.map_err(|err| {
                        CoreError::Sftp(format!(
                            "写入远端 {part} 中断（已上传 {} 字节）：{err}",
                            offset + written
                        ))
                    })?;
                    written += read as u64;
                    progress(offset + written);
                }

                // 关闭句柄会等所有写请求被服务端确认，避免"返回成功但数据还在飞"
                remote.close().await.map_err(|err| {
                    CoreError::Sftp(format!(
                        "关闭远端 {part} 失败（已上传 {} 字节）：{err}",
                        offset + written
                    ))
                })?;
                Ok(TransferStep::Completed(written))
            }
            .await;

            match step {
                // 取消：保留 .rmxpart，前端可以用 resume=true 接着传
                Ok(TransferStep::Cancelled(written)) => Err(cancelled_error(offset + written)),
                // 其它失败：半截残留没有续传价值，尽量清掉
                Err(err) => {
                    delete_remote_quietly(&session, &part).await;
                    Err(err)
                }
                Ok(TransferStep::Completed(written)) => {
                    // SFTP v3 的 RENAME 不保证覆盖已存在的目标：目标已存在时先删再改名，
                    // 否则数据会永远卡在 .rmxpart 上，只能无限续传
                    if let Err(err) = session.rename(part.clone(), target.clone()).await {
                        rename_part_into_place(&session, &part, &target, total, err).await?;
                    }

                    // 属性继承失败只记日志：内容已落对位置，不因权限设置失败判传输失败
                    if attrs.permissions.is_some() || attrs.mtime.is_some() {
                        if let Err(err) = session.set_metadata(target.clone(), attrs).await {
                            eprintln!("上传 {target} 成功，但复刻本机权限/修改时间失败：{err}");
                        }
                    }

                    Ok(TransferOutcome {
                        written,
                        total,
                        resumed_from: offset,
                    })
                }
            }
        })
    }

    /// 把远端文件流式下载到本地（不限文件大小，内容不经过内存整抄）。
    ///
    /// 参数：remote_path 为远端源文件路径，local_path 为本地落盘路径，
    /// chunk 为分块字节数（0 表示用 DEFAULT_TRANSFER_CHUNK）
    /// 核心流程：薄封装，固定「不续传、无进度、永不取消」，
    /// 内部转调 download_file_to_with，返回本次写入字节数。
    pub fn download_file_to(
        &self,
        remote_path: &str,
        local_path: &Path,
        chunk: usize,
    ) -> CoreResult<u64> {
        let never_cancel = AtomicBool::new(false);
        self.download_file_to_with(
            remote_path,
            local_path,
            chunk,
            false,
            &mut |_| {},
            &never_cancel,
        )
        .map(|outcome| outcome.written)
    }

    /// 把远端文件流式下载到本地，支持临时文件、续传、进度与取消。
    ///
    /// 参数：remote_path 为远端源文件路径，local_path 为本地落盘路径，
    /// chunk 为分块字节数（0 表示用 DEFAULT_TRANSFER_CHUNK），resume 为真时接着残留的
    /// `.rmxpart` 续传，progress 为进度回调（含续传部分，是累计写入字节），
    /// cancel 为取消标志
    /// 核心流程：先校验本地父目录存在（不静默建目录）→ 远端 lstat，是符号链接就拒绝 →
    /// 用远端大小与残留大小决定起点（resume=false 或残留不可信时从 0 开始）→
    /// 远端 seek 到起点、本地 `.rmxpart` 追加写 → 循环「读一块远端 / 写一块本地 / 回调进度」，
    /// 每块之后检查取消 → 本地改名成目标（目标已存在时先删，兼容 Windows 的 rename 语义）。
    /// 取消时保留 `.rmxpart` 供下次续传，其它失败尽量删掉它，目标名上永不出现半截文件。
    pub fn download_file_to_with(
        &self,
        remote_path: &str,
        local_path: &Path,
        chunk: usize,
        resume: bool,
        progress: &mut dyn FnMut(u64),
        cancel: &AtomicBool,
    ) -> CoreResult<TransferOutcome> {
        let session = Arc::clone(&self.session);
        let target = normalize_path(remote_path);
        let destination = local_path.to_path_buf();
        let part = local_part_path(&destination);
        let block = if chunk == 0 {
            DEFAULT_TRANSFER_CHUNK
        } else {
            chunk
        };

        run_borrowed("下载文件", async move {
            if let Some(parent) = destination.parent() {
                if !parent.as_os_str().is_empty() && !parent.is_dir() {
                    return Err(CoreError::Sftp(format!(
                        "本地目录 {} 不存在，请先新建目录再下载",
                        parent.display()
                    )));
                }
            }

            // lstat 不跟随链接：远端目标是链接时拒绝，免得把链接目标的内容当成本次下载对象
            // （服务端不支持 LSTAT 时拿不到属性，交给后面的 OPEN 自己报错）
            let remote_meta = session.symlink_metadata(target.clone()).await.ok();
            if let Some(meta) = remote_meta.as_ref() {
                if meta.is_symlink() {
                    return Err(CoreError::Validation(format!(
                        "不支持下载符号链接：{target}"
                    )));
                }
            }
            let total = remote_meta
                .as_ref()
                .and_then(|meta| meta.size)
                .unwrap_or(0);

            // 续传起点：本地残留的长度；远端大小未知或残留比远端还大时都不信它
            let offset = if resume {
                std::fs::metadata(&part)
                    .map(|meta| meta.len())
                    .unwrap_or(0)
            } else {
                0
            };
            let offset = if total == 0 || offset > total { 0 } else { offset };

            let mut remote = session
                .open(target.clone())
                .await
                .map_err(|err| to_core_error(&format!("远端打开 {target} "), err))?;
            if offset > 0 {
                remote.seek(SeekFrom::Start(offset)).await.map_err(|err| {
                    CoreError::Sftp(format!(
                        "远端 {target} 定位到第 {offset} 字节失败：{err}"
                    ))
                })?;
            }

            let mut local = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(offset == 0)
                .open(&part)
                .map_err(|err| {
                    CoreError::Sftp(format!(
                        "本地临时文件 {} 无法写入：{err}",
                        part.display()
                    ))
                })?;
            if offset > 0 {
                local.seek(SeekFrom::Start(offset)).map_err(|err| {
                    CoreError::Sftp(format!(
                        "本地临时文件 {} 定位到第 {offset} 字节失败：{err}",
                        part.display()
                    ))
                })?;
            }

            // 循环单独放在一段 async 里：它的结果要交给外层决定删残留还是保留
            let step = async {
                let mut buffer = vec![0u8; block];
                let mut written: u64 = 0;
                loop {
                    if cancel.load(Ordering::SeqCst) {
                        let _ = remote.close().await;
                        return Ok(TransferStep::Cancelled(written));
                    }
                    let read = remote.read(&mut buffer).await.map_err(|err| {
                        CoreError::Sftp(format!(
                            "读取远端 {target} 中断（已下载 {} 字节）：{err}",
                            offset + written
                        ))
                    })?;
                    if read == 0 {
                        break;
                    }
                    local.write_all(&buffer[..read]).map_err(|err| {
                        CoreError::Sftp(format!(
                            "写入本地临时文件 {} 中断（已下载 {} 字节）：{err}",
                            part.display(),
                            offset + written
                        ))
                    })?;
                    written += read as u64;
                    progress(offset + written);
                }

                local.flush().map_err(|err| {
                    CoreError::Sftp(format!(
                        "本地临时文件 {} 落盘失败（已下载 {} 字节）：{err}",
                        part.display(),
                        offset + written
                    ))
                })?;
                // 只读句柄，关不掉不影响已经落盘的内容 -> 只记日志，不判传输失败
                if let Err(err) = remote.close().await {
                    eprintln!("下载 {target} 完成，但关闭远端句柄失败：{err}");
                }
                Ok(TransferStep::Completed(written))
            }
            .await;

            match step {
                // 取消：保留 .rmxpart，前端可以用 resume=true 接着传
                Ok(TransferStep::Cancelled(written)) => Err(cancelled_error(offset + written)),
                // 其它失败：半截残留没有续传价值，尽量清掉
                Err(err) => {
                    delete_local_quietly(&part);
                    Err(err)
                }
                Ok(TransferStep::Completed(written)) => {
                    // Windows 的 rename 不能覆盖已存在的文件 -> 先删目标再改名
                    if destination.exists() {
                        if let Err(err) = std::fs::remove_file(&destination) {
                            delete_local_quietly(&part);
                            return Err(CoreError::Sftp(format!(
                                "覆盖本地文件 {} 失败：{err}",
                                destination.display()
                            )));
                        }
                    }
                    std::fs::rename(&part, &destination).map_err(|err| {
                        CoreError::Sftp(format!(
                            "本地临时文件 {} 改名为 {} 失败（{err}），数据已完整保留在 {}，可用续传继续",
                            part.display(),
                            destination.display(),
                            part.display()
                        ))
                    })?;

                    Ok(TransferOutcome {
                        written,
                        total,
                        resumed_from: offset,
                    })
                }
            }
        })
    }

    /// 新建目录。
    ///
    /// 参数：path 为远端目录路径
    /// 核心流程：直接发 MKDIR；父目录不存在或已存在由服务端状态码决定，
    /// 原样转成「Sftp 错误：...」交给前端展示。
    pub fn create_dir(&self, path: &str) -> CoreResult<()> {
        let session = Arc::clone(&self.session);
        let target = normalize_path(path);
        run_blocking("新建目录", async move {
            session
                .create_dir(target.clone())
                .await
                .map_err(|err| to_core_error(&format!("新建目录 {target} "), err))
        })
    }

    /// 删除文件或目录。
    ///
    /// 参数：path 为远端路径，is_dir 为是否按目录删除
    /// 核心流程：目录走 RMDIR（只删空目录），文件走 REMOVE；
    /// 类型不对时服务端会回失败状态，这里原样上报。
    pub fn remove(&self, path: &str, is_dir: bool) -> CoreResult<()> {
        let session = Arc::clone(&self.session);
        let target = normalize_path(path);
        run_blocking("删除", async move {
            let result = if is_dir {
                session.remove_dir(target.clone()).await
            } else {
                session.remove_file(target.clone()).await
            };
            result.map_err(|err| to_core_error(&format!("删除 {target} "), err))
        })
    }

    /// 重命名（同目录或跨目录移动）。
    ///
    /// 参数：from 为原路径，to 为新路径
    /// 核心流程：发 RENAME；同目录改名与移动都用它，目标已存在时按服务端策略覆盖。
    pub fn rename(&self, from: &str, to: &str) -> CoreResult<()> {
        let session = Arc::clone(&self.session);
        let source = normalize_path(from);
        let destination = normalize_path(to);
        run_blocking("重命名", async move {
            session
                .rename(source.clone(), destination.clone())
                .await
                .map_err(|err| {
                    to_core_error(&format!("重命名 {source} -> {destination} "), err)
                })
        })
    }

    /// 关闭会话。
    ///
    /// 参数：无
    /// 核心流程：关 SFTP 会话 → 断开整条 SSH 连接；两步都忽略错误，
    /// 保证前端重复调用或连接已断时也不报错。
    pub fn close(&self) {
        let session = Arc::clone(&self.session);
        let _ = run_blocking("关闭 SFTP 会话", async move {
            session
                .close()
                .await
                .map_err(|err| to_core_error("关闭 SFTP 会话", err))
        });

        let connection = self.connection.clone();
        let _ = run_blocking("断开连接", async move { connection.disconnect().await });
    }

    /// 连接是否已关闭。
    ///
    /// 参数：无
    /// 核心流程：直接读底层 SSH 通道状态，供上层判断是否需要重连。
    pub fn is_closed(&self) -> bool {
        self.connection.is_closed()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::Mutex;

    use russh::server::{self, Auth, ChannelOpenHandle, Msg, Server, Session};
    use russh::{Channel, ChannelId};
    use russh_sftp::protocol::{
        Attrs, Data, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode,
    };
    use russh_sftp::protocol::File as RemoteFile;
    use tokio::net::TcpListener;

    use super::*;

    /// 测试服务端的两种行为。
    #[derive(Debug, Clone, Copy)]
    enum ServerMode {
        /// 正常提供内存文件系统。
        InMemory,
        /// 收下 sftp 子系统后立刻关通道 -> 用来验证"SFTP 握手失败"的文案。
        DropSubsystem,
    }

    /// 内存文件系统：目录集合 + 文件内容，测试里用来替身远端磁盘。
    #[derive(Clone, Default)]
    struct FakeFs {
        dirs: Arc<Mutex<Vec<String>>>,
        files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        opened_dirs: Arc<Mutex<Vec<String>>>,
        /// SETSTAT 设过的属性，按远端路径存，用来验证属性继承。
        attrs: Arc<Mutex<HashMap<String, FileAttributes>>>,
        /// 符号链接：链接路径 -> 目标路径，用来验证「拒绝下载符号链接」。
        links: Arc<Mutex<HashMap<String, String>>>,
        /// 因目标已存在而被拒绝的 RENAME 次数，用来证明覆盖兜底路径真的被走到。
        rename_rejections: Arc<Mutex<usize>>,
    }

    impl FakeFs {
        /// 初始化根目录。
        ///
        /// 参数：无
        /// 核心流程：放入 "/" 与用例会用到的父目录，避免每条用例重复建。
        fn new() -> Self {
            let fs = Self::default();
            fs.dirs.lock().expect("锁可用").push("/".to_string());
            fs
        }

        /// 判断路径是否是已知目录。
        ///
        /// 参数：path 为远端路径
        /// 核心流程：加锁查目录集合，根目录恒定存在。
        fn is_dir(&self, path: &str) -> bool {
            path == "/" || self.dirs.lock().expect("锁可用").iter().any(|item| item == path)
        }
    }

    /// russh-sftp 的服务端实现：用 FakeFs 冒充远端文件系统。
    struct FakeSftp {
        fs: FakeFs,
    }

    impl FakeSftp {
        /// 取某个路径的属性，并叠加 SETSTAT 设过的权限与时间。
        ///
        /// 参数：path 为远端路径，len 为内容长度，dir 表示是否是目录
        /// 核心流程：先用 attrs_of 造默认属性，再按路径查 SETSTAT 记录覆盖权限与 mtime，
        /// 这样「上传后远端 mtime 与本地一致」这类断言才有观察点。
        fn attrs_for(&self, path: &str, len: usize, dir: bool) -> FileAttributes {
            let mut attrs = attrs_of(len, dir);
            if let Some(saved) = self.fs.attrs.lock().expect("锁可用").get(path) {
                if saved.permissions.is_some() {
                    attrs.permissions = saved.permissions;
                }
                if saved.mtime.is_some() {
                    attrs.mtime = saved.mtime;
                }
            }
            attrs
        }
    }

    /// 拼一个成功状态包。
    ///
    /// 参数：id 为请求号
    /// 核心流程：SFTP v3 的 OK 状态必须带文案与语言标签。
    fn ok_status(id: u32) -> Status {
        Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_string(),
            language_tag: "en-US".to_string(),
        }
    }

    /// 把文件内容包成属性。
    ///
    /// 参数：len 为长度，dir 表示是否是目录
    /// 核心流程：普通文件给 0o644，目录补上 DIR 类型位，
    /// 这样客户端算出的 is_dir 与真实服务器一致。
    fn attrs_of(len: usize, dir: bool) -> FileAttributes {
        FileAttributes {
            size: Some(len as u64),
            permissions: Some(if dir { 0o040755 } else { 0o100644 }),
            mtime: Some(1_700_000_000),
            ..Default::default()
        }
    }

    /// 取父目录与文件名。
    ///
    /// 参数：path 为完整路径
    /// 核心流程：按最后一个 `/` 切分，根目录下按空父目录返回。
    fn split_path(path: &str) -> (String, String) {
        match path.rfind('/') {
            Some(index) => (path[..index].to_string(), path[index + 1..].to_string()),
            None => (String::new(), path.to_string()),
        }
    }

    impl russh_sftp::server::Handler for FakeSftp {
        type Error = StatusCode;

        fn unimplemented(&self) -> Self::Error {
            StatusCode::OpUnsupported
        }

        async fn init(
            &mut self,
            _version: u32,
            _extensions: HashMap<String, String>,
        ) -> Result<russh_sftp::protocol::Version, Self::Error> {
            Ok(russh_sftp::protocol::Version::new())
        }

        async fn open(
            &mut self,
            id: u32,
            filename: String,
            pflags: OpenFlags,
            _attrs: FileAttributes,
        ) -> Result<Handle, Self::Error> {
            let mut files = self.fs.files.lock().expect("锁可用");
            // 只有 O_TRUNC 才清空内容：单带 CREATE 在真实服务端只是「没有就建」，不清空，
            // 续传正是靠这一点把新数据接在旧数据后面。
            if pflags.contains(OpenFlags::TRUNCATE) {
                files.insert(filename.clone(), Vec::new());
            } else if pflags.contains(OpenFlags::CREATE) {
                files.entry(filename.clone()).or_default();
            } else if !files.contains_key(&filename) {
                return Err(StatusCode::NoSuchFile);
            }
            Ok(Handle {
                id,
                handle: filename,
            })
        }

        async fn close(&mut self, id: u32, _handle: String) -> Result<Status, Self::Error> {
            Ok(ok_status(id))
        }

        async fn read(
            &mut self,
            id: u32,
            handle: String,
            offset: u64,
            len: u32,
        ) -> Result<Data, Self::Error> {
            let files = self.fs.files.lock().expect("锁可用");
            let content = files.get(&handle).ok_or(StatusCode::NoSuchFile)?;
            let start = offset as usize;
            if start >= content.len() {
                return Err(StatusCode::Eof);
            }
            let end = usize::min(start + len as usize, content.len());
            Ok(Data {
                id,
                data: content[start..end].to_vec(),
            })
        }

        async fn write(
            &mut self,
            id: u32,
            handle: String,
            offset: u64,
            data: Vec<u8>,
        ) -> Result<Status, Self::Error> {
            let mut files = self.fs.files.lock().expect("锁可用");
            let content = files.entry(handle).or_default();
            let start = offset as usize;
            if content.len() < start + data.len() {
                content.resize(start + data.len(), 0);
            }
            content[start..start + data.len()].copy_from_slice(&data);
            Ok(ok_status(id))
        }

        async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
            let len = {
                let files = self.fs.files.lock().expect("锁可用");
                files.get(&handle).ok_or(StatusCode::NoSuchFile)?.len()
            };
            let attrs = self.attrs_for(&handle, len, false);
            Ok(Attrs { id, attrs })
        }

        async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
            let len = self
                .fs
                .files
                .lock()
                .expect("锁可用")
                .get(&path)
                .map(|content| content.len());
            if let Some(len) = len {
                let attrs = self.attrs_for(&path, len, false);
                return Ok(Attrs { id, attrs });
            }
            if self.fs.is_dir(&path) {
                let attrs = self.attrs_for(&path, 0, true);
                return Ok(Attrs { id, attrs });
            }
            Err(StatusCode::NoSuchFile)
        }

        async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
            // 链接按「链接自身」回答：类型位是 LNK，大小取目标路径长度，不跟随
            let link_target = self.fs.links.lock().expect("锁可用").get(&path).cloned();
            if let Some(target) = link_target {
                let mut attrs = attrs_of(target.len(), false);
                attrs.permissions = Some(0o120777);
                return Ok(Attrs { id, attrs });
            }
            self.stat(id, path).await
        }

        async fn setstat(
            &mut self,
            id: u32,
            path: String,
            attrs: FileAttributes,
        ) -> Result<Status, Self::Error> {
            let exists = self.fs.files.lock().expect("锁可用").contains_key(&path)
                || self.fs.is_dir(&path);
            if !exists {
                return Err(StatusCode::NoSuchFile);
            }
            self.fs
                .attrs
                .lock()
                .expect("锁可用")
                .insert(path, attrs);
            Ok(ok_status(id))
        }

        async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
            if !self.fs.is_dir(&path) {
                return Err(StatusCode::NoSuchFile);
            }
            self.fs.opened_dirs.lock().expect("锁可用").push(path.clone());
            Ok(Handle { id, handle: path })
        }

        async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
            // 一个目录只回一批：取走句柄后下一遍 READDIR 就会收到 EOF
            let pending = {
                let mut opened = self.fs.opened_dirs.lock().expect("锁可用");
                match opened.iter().position(|item| item == &handle) {
                    Some(index) => {
                        opened.remove(index);
                        true
                    }
                    None => false,
                }
            };
            if !pending {
                return Err(StatusCode::Eof);
            }
            let prefix = if handle == "/" {
                "/".to_string()
            } else {
                format!("{handle}/")
            };
            let mut files = Vec::new();
            for (path, content) in self.fs.files.lock().expect("锁可用").iter() {
                match path.strip_prefix(&prefix) {
                    Some(name) if !name.is_empty() && !name.contains('/') => {
                        let attrs = self.attrs_for(path, content.len(), false);
                        files.push(RemoteFile::new(name, attrs));
                    }
                    _ => {}
                }
            }
            for dir in self.fs.dirs.lock().expect("锁可用").iter() {
                if dir == "/" {
                    continue;
                }
                match dir.strip_prefix(&prefix) {
                    Some(name) if !name.is_empty() && !name.contains('/') => {
                        let attrs = self.attrs_for(dir, 0, true);
                        files.push(RemoteFile::new(name, attrs));
                    }
                    _ => {}
                }
            }
            Ok(Name { id, files })
        }

        async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
            Ok(Name {
                id,
                files: vec![RemoteFile::dummy(if path.is_empty() { "/" } else { &path })],
            })
        }

        async fn mkdir(
            &mut self,
            id: u32,
            path: String,
            _attrs: FileAttributes,
        ) -> Result<Status, Self::Error> {
            let (parent, _) = split_path(&path);
            let parent = if parent.is_empty() { "/".to_string() } else { parent };
            if !self.fs.is_dir(&parent) || self.fs.is_dir(&path) {
                return Err(StatusCode::Failure);
            }
            self.fs.dirs.lock().expect("锁可用").push(path);
            Ok(ok_status(id))
        }

        async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
            if !self.fs.dirs.lock().expect("锁可用").iter().any(|item| item == &path) {
                return Err(StatusCode::NoSuchFile);
            }
            self.fs.dirs.lock().expect("锁可用").retain(|item| item != &path);
            Ok(ok_status(id))
        }

        async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
            let removed = self.fs.files.lock().expect("锁可用").remove(&filename);
            match removed {
                Some(_) => Ok(ok_status(id)),
                None => Err(StatusCode::NoSuchFile),
            }
        }

        async fn rename(
            &mut self,
            id: u32,
            oldpath: String,
            newpath: String,
        ) -> Result<Status, Self::Error> {
            // 仿 OpenSSH 的 sftp-server：目标已存在时 RENAME 直接回 SSH_FX_FAILURE。
            // SFTP v3 允许服务端这么做，覆不覆盖得由客户端自己解决——这正是真机上
            // 「传完改不了名」的现场，测试服务端必须还原它，兜底路径才有看门狗。
            if self
                .fs
                .files
                .lock()
                .expect("锁可用")
                .contains_key(&newpath)
            {
                *self.fs.rename_rejections.lock().expect("锁可用") += 1;
                return Err(StatusCode::Failure);
            }
            let moved = self.fs.files.lock().expect("锁可用").remove(&oldpath);
            match moved {
                Some(content) => {
                    self.fs
                        .files
                        .lock()
                        .expect("锁可用")
                        .insert(newpath.clone(), content);
                    // 属性跟着走，免得 SETSTAT 结果留在旧路径上
                    let mut attrs = self.fs.attrs.lock().expect("锁可用");
                    if let Some(saved) = attrs.remove(&oldpath) {
                        attrs.insert(newpath, saved);
                    }
                    Ok(ok_status(id))
                }
                None => Err(StatusCode::NoSuchFile),
            }
        }
    }

    /// 测试用 SSH 服务端：认证放行，sftp 子系统转给 FakeSftp。
    #[derive(Clone)]
    struct TestSshServer {
        mode: ServerMode,
        /// 所有子系统共用这一份内存文件系统，用例才能预置链接、断言残留文件。
        fs: FakeFs,
    }

    struct TestSshHandler {
        mode: ServerMode,
        fs: FakeFs,
        /// 开着但还没交给子系统的通道，按 id 暂存。
        channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
    }

    impl server::Handler for TestSshHandler {
        type Error = russh::Error;

        async fn auth_password(
            &mut self,
            _user: &str,
            _password: &str,
        ) -> Result<Auth, Self::Error> {
            Ok(Auth::Accept)
        }

        async fn channel_open_session(
            &mut self,
            channel: Channel<Msg>,
            reply: ChannelOpenHandle,
            _session: &mut Session,
        ) -> Result<(), Self::Error> {
            self.channels
                .lock()
                .expect("锁可用")
                .insert(channel.id(), channel);
            reply.accept().await;
            Ok(())
        }

        /// 子系统请求：sftp 就交给 russh-sftp 服务端，其余一律拒绝。
        ///
        /// 参数：channel 为通道号，name 为子系统名
        /// 核心流程：从暂存表取出通道 → 应答成功 → 按其字节流转交服务端循环；
        /// DropSubsystem 模式直接关通道，用来模拟"握手无响应"。
        async fn subsystem_request(
            &mut self,
            channel: ChannelId,
            name: &str,
            session: &mut Session,
        ) -> Result<(), Self::Error> {
            if name != "sftp" {
                let _ = session.channel_failure(channel);
                return Ok(());
            }
            let channel = self.channels.lock().expect("锁可用").remove(&channel);
            let Some(channel) = channel else {
                return Ok(());
            };

            match self.mode {
                ServerMode::DropSubsystem => {
                    let _ = session.close(channel.id());
                }
                ServerMode::InMemory => {
                    let _ = session.channel_success(channel.id());
                    russh_sftp::server::run(
                        channel.into_stream(),
                        FakeSftp { fs: self.fs.clone() },
                    )
                    .await;
                }
            }
            Ok(())
        }
    }

    impl Server for TestSshServer {
        type Handler = TestSshHandler;

        fn new_client(&mut self, _peer: Option<SocketAddr>) -> Self::Handler {
            TestSshHandler {
                mode: self.mode,
                fs: self.fs.clone(),
                channels: Arc::new(Mutex::new(HashMap::new())),
            }
        }
    }

    /// 起一个带内存 SFTP 服务端的 SSH 服务端，并把文件系统句柄交给调用方。
    ///
    /// 参数：mode 决定子系统是否正常服务
    /// 核心流程：绑 127.0.0.1:0 拿随机端口 → 生成临时主机密钥 →
    /// 后台跑 russh server → 返回端口与共享的内存文件系统，
    /// 用例可以往里面预置符号链接之类的特殊条目。
    async fn spawn_server_with_fs(mode: ServerMode) -> (u16, FakeFs) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("应能绑定端口");
        let port = listener.local_addr().expect("应能取到本地地址").port();
        let key = russh::keys::PrivateKey::random(
            &mut rand::rng(),
            russh::keys::Algorithm::Ed25519,
        )
        .expect("应能生成主机密钥");
        let config = Arc::new(server::Config {
            keys: vec![key],
            ..Default::default()
        });

        let fs = FakeFs::new();
        let server_fs = fs.clone();
        tokio::spawn(async move {
            let mut server = TestSshServer {
                mode,
                fs: server_fs,
            };
            let _ = server.run_on_socket(config, &listener).await;
        });

        (port, fs)
    }

    /// 起一个带内存 SFTP 服务端的 SSH 服务端。
    ///
    /// 参数：mode 决定子系统是否正常服务
    /// 核心流程：转调 spawn_server_with_fs，只要端口；
    /// 测试结束由进程退出回收，不显式关闭。
    async fn spawn_server(mode: ServerMode) -> u16 {
        spawn_server_with_fs(mode).await.0
    }

    /// 构造指向本地测试服务端的连接参数。
    ///
    /// 参数：port 为服务端端口
    /// 核心流程：固定 127.0.0.1 + 密码认证 + 已确认指纹，用例只关心端口。
    fn test_options(port: u16) -> SshOptions {
        SshOptions {
            host: "127.0.0.1".into(),
            port,
            username: "tester".into(),
            auth: crate::ssh::SshAuth::Password("secret".into()),
            cols: 80,
            rows: 24,
            term: "xterm-256color".into(),
            accept_unknown_host_key: true,
            jump: None,
        }
    }

    #[test]
    fn reports_clear_error_when_port_closed() {
        let store = runtime()
            .block_on(async { Store::open_in_memory().await })
            .expect("内存库");

        let result = SftpClient::connect(&store, &test_options(1));
        match result {
            Ok(_) => panic!("端口未监听时应失败"),
            Err(err) => {
                let message = err.to_string();
                assert!(
                    message.contains("连接"),
                    "错误应指明连接问题，实际：{message}"
                );
            }
        }
    }

    #[test]
    fn reports_sftp_error_when_handshake_stalls() {
        let store = runtime()
            .block_on(async { Store::open_in_memory().await })
            .expect("内存库");
        let port = runtime().block_on(spawn_server(ServerMode::DropSubsystem));

        let result = SftpClient::connect(&store, &test_options(port));
        match result {
            Ok(_) => panic!("子系统被关掉时握手应失败"),
            Err(err) => {
                let message = err.to_string();
                assert!(
                    message.starts_with("Sftp 错误："),
                    "错误应是 Sftp 分类，实际：{message}"
                );
                assert!(
                    message.contains("握手失败"),
                    "错误应指明是 SFTP 握手阶段，实际：{message}"
                );
            }
        }
    }

    #[test]
    fn lists_and_edits_files_against_local_sftp_server() {
        let store = runtime()
            .block_on(async { Store::open_in_memory().await })
            .expect("内存库");
        let port = runtime().block_on(spawn_server(ServerMode::InMemory));
        let client = SftpClient::connect(&store, &test_options(port)).expect("应能建立 SFTP 会话");

        // 新建目录 -> 写文件 -> 读回来
        client.create_dir("/docs").expect("应能新建目录");
        client
            .write_file("/docs/hello.txt", "你好 SFTP".as_bytes())
            .expect("应能写入文件");
        let read_back = client.read_file("/docs/hello.txt", 1024).expect("应能读取文件");
        assert_eq!(
            read_back,
            "你好 SFTP".as_bytes(),
            "读回内容应与写入一致"
        );

        // 列目录 -> 名字、类型、大小、时间都对得上
        let entries = client.list("/docs").expect("应能列目录");
        assert_eq!(entries.len(), 1, "目录里应只有一个文件，实际：{entries:?}");
        assert_eq!(entries[0].name, "hello.txt");
        assert_eq!(entries[0].path, "/docs/hello.txt");
        assert!(!entries[0].is_dir, "文件不应标成目录");
        assert_eq!(entries[0].size, "你好 SFTP".len() as u64);
        assert_eq!(entries[0].modified, 1_700_000_000);

        // 超限读取 -> 明确报错而不是把大文件拖进内存
        let too_big = client.read_file("/docs/hello.txt", 4);
        match too_big {
            Ok(data) => panic!("超过上限时应报错，实际读到 {} 字节", data.len()),
            Err(err) => assert!(
                err.to_string().contains("超过读取上限"),
                "错误应说明超出上限，实际：{err}"
            ),
        }

        // 重命名 -> 旧名字消失、新名字出现
        client
            .rename("/docs/hello.txt", "/docs/renamed.txt")
            .expect("应能重命名");
        let entries = client.list("/docs").expect("应能列目录");
        assert_eq!(entries[0].name, "renamed.txt");

        // 删除文件与目录 -> 之后列目录应报错
        client
            .remove("/docs/renamed.txt", false)
            .expect("应能删除文件");
        client.remove("/docs", true).expect("应能删除目录");
        assert!(
            client.list("/docs").is_err(),
            "目录已删除，列目录应当失败"
        );

        client.close();
    }

    /// 造一个临时工作目录。
    ///
    /// 参数：无
    /// 核心流程：在系统临时目录下拼一个带 UUID 的子目录建好，
    /// 用完由 save_and_compare 或用例末尾整体删除，避免不同用例互相踩到同名文件。
    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rmx-sftp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("应能创建临时目录");
        dir
    }

    /// 生成可校验的分块测试内容。
    ///
    /// 参数：size 为字节数
    /// 核心流程：按「下标取模 251」填充，每个字节都与位置绑定，
    /// 一旦分块拼接错位，首尾比对就会立刻失败。
    fn patterned_bytes(size: usize) -> Vec<u8> {
        (0..size).map(|index| (index % 251) as u8).collect()
    }

    #[test]
    fn streams_large_file_up_and_down_without_truncation() {
        let store = runtime()
            .block_on(async { Store::open_in_memory().await })
            .expect("内存库");
        let port = runtime().block_on(spawn_server(ServerMode::InMemory));
        let client = SftpClient::connect(&store, &test_options(port)).expect("应能建立 SFTP 会话");

        let dir = temp_dir();
        let source = dir.join("source.bin");
        let fetched = dir.join("fetched.bin");

        // 5 MiB 内容 -> 远超默认 256 KiB 分块，必然走多块，能暴露截断问题
        let size = 5 * 1024 * 1024;
        let content = patterned_bytes(size);
        std::fs::write(&source, &content).expect("应能写入本地测试文件");

        client.create_dir("/incoming").expect("应能新建远端目录");

        // 上传：分块大小取默认值，返回值必须等于文件大小
        let uploaded = client
            .upload_file_from("/incoming/source.bin", &source, DEFAULT_TRANSFER_CHUNK)
            .expect("应能流式上传");
        assert_eq!(uploaded, size as u64, "上传字节数应等于文件大小");

        let entries = client.list("/incoming").expect("应能列目录");
        assert_eq!(
            entries[0].size, size as u64,
            "远端文件大小应与本地一致，实际：{entries:?}"
        );

        // 下载：换一个刻意不对齐的分块大小，验证零碎块也能正确拼接
        let downloaded = client
            .download_file_to("/incoming/source.bin", &fetched, 100 * 1024 + 7)
            .expect("应能流式下载");
        assert_eq!(downloaded, size as u64, "下载字节数应等于文件大小");

        let round_trip = std::fs::read(&fetched).expect("应能读回下载的文件");
        assert_eq!(round_trip.len(), size, "下载后文件长度应一致");
        assert_eq!(
            &round_trip[..256],
            &content[..256],
            "文件头部应完全一致"
        );
        assert_eq!(
            &round_trip[size - 256..],
            &content[size - 256..],
            "文件尾部应完全一致（证明没有截断）"
        );

        // 任意中间一块也抽一段比对，防止只对上首尾
        let middle = 2 * 1024 * 1024 + 123;
        assert_eq!(
            &round_trip[middle..middle + 256],
            &content[middle..middle + 256],
            "中间分块边界处内容应一致"
        );

        client.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_upload_when_remote_parent_is_missing() {
        let store = runtime()
            .block_on(async { Store::open_in_memory().await })
            .expect("内存库");
        let port = runtime().block_on(spawn_server(ServerMode::InMemory));
        let client = SftpClient::connect(&store, &test_options(port)).expect("应能建立 SFTP 会话");

        let dir = temp_dir();
        let source = dir.join("payload.bin");
        std::fs::write(&source, b"payload").expect("应能写入本地测试文件");

        let err = client
            .upload_file_from("/missing-dir/payload.bin", &source, DEFAULT_TRANSFER_CHUNK)
            .expect_err("远端父目录不存在时应报错");
        let text = err.to_string();
        assert!(
            text.contains("/missing-dir"),
            "错误应指明缺失的远端目录，实际：{text}"
        );
        assert!(
            text.contains("不存在"),
            "错误应说明是目录不存在，实际：{text}"
        );

        client.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_upload_when_local_file_is_missing() {
        let store = runtime()
            .block_on(async { Store::open_in_memory().await })
            .expect("内存库");
        let port = runtime().block_on(spawn_server(ServerMode::InMemory));
        let client = SftpClient::connect(&store, &test_options(port)).expect("应能建立 SFTP 会话");

        let dir = temp_dir();
        let missing = dir.join("nope.bin");
        client.create_dir("/incoming").expect("应能新建远端目录");

        let err = client
            .upload_file_from("/incoming/nope.bin", &missing, DEFAULT_TRANSFER_CHUNK)
            .expect_err("本地文件不存在时应报错");
        assert!(
            err.to_string().contains("本地文件"),
            "错误应指明是本地文件问题，实际：{err}"
        );

        client.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_download_when_local_parent_is_missing() {
        let store = runtime()
            .block_on(async { Store::open_in_memory().await })
            .expect("内存库");
        let port = runtime().block_on(spawn_server(ServerMode::InMemory));
        let client = SftpClient::connect(&store, &test_options(port)).expect("应能建立 SFTP 会话");

        client.create_dir("/incoming").expect("应能新建远端目录");
        client
            .write_file("/incoming/remote.bin", b"remote-payload")
            .expect("应能写入远端文件");

        let dir = temp_dir();
        let target = dir.join("no-such-dir").join("remote.bin");
        let err = client
            .download_file_to("/incoming/remote.bin", &target, DEFAULT_TRANSFER_CHUNK)
            .expect_err("本地父目录不存在时应报错");
        let text = err.to_string();
        assert!(
            text.contains("本地目录") && text.contains("不存在"),
            "错误应指明本地目录缺失，实际：{text}"
        );

        client.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 起一个带内存 SFTP 服务端的会话，并建好用例常用的远端目录。
    ///
    /// 参数：无
    /// 核心流程：把「开内存库 → 起服务端 → 连 SFTP → 建 /incoming」这四步收拢，
    /// 传输类用例只关心文件内容，不必每处重复接线。
    fn connect_with_incoming() -> (SftpClient, std::path::PathBuf) {
        let store = runtime()
            .block_on(async { Store::open_in_memory().await })
            .expect("内存库");
        let port = runtime().block_on(spawn_server(ServerMode::InMemory));
        let client = SftpClient::connect(&store, &test_options(port)).expect("应能建立 SFTP 会话");
        client.create_dir("/incoming").expect("应能新建远端目录");
        let dir = temp_dir();
        (client, dir)
    }

    /// 列出远端目录里的文件名。
    ///
    /// 参数：client 为已连上的会话，path 为目录
    /// 核心流程：只取名字，用于断言「目标在不在」「残留还在不在」。
    fn remote_names(client: &SftpClient, path: &str) -> Vec<String> {
        client
            .list(path)
            .expect("应能列目录")
            .into_iter()
            .map(|entry| entry.name)
            .collect()
    }

    #[test]
    fn resumes_upload_after_cancel_with_leftover_temp_file() {
        let (client, dir) = connect_with_incoming();
        let source = dir.join("resume.bin");
        let content = patterned_bytes(1024 * 1024);
        std::fs::write(&source, &content).expect("应能写入本地测试文件");

        // 先传一半再取消：小块 + 进度到 320 KiB 时把取消标志立起来
        let cancel = AtomicBool::new(false);
        let chunk = 64 * 1024;
        let mut progress = |bytes: u64| {
            if bytes >= 320 * 1024 {
                cancel.store(true, Ordering::SeqCst);
            }
        };
        let err = client
            .upload_file_from_with(
                "/incoming/resume.bin",
                &source,
                chunk,
                false,
                &mut progress,
                &cancel,
            )
            .expect_err("中途取消应返回错误");
        assert!(
            err.to_string().contains("传输已取消"),
            "错误应说明是取消，实际：{err}"
        );

        // 中断现场：目标名上不能有半截文件，残留必须在 .rmxpart 上
        let names = remote_names(&client, "/incoming");
        assert!(
            names.iter().any(|name| name == "resume.bin.rmxpart"),
            "中断后应留下临时文件，实际：{names:?}"
        );
        assert!(
            !names.iter().any(|name| name == "resume.bin"),
            "目标名上不应出现半截文件，实际：{names:?}"
        );
        let partial = client
            .list("/incoming")
            .expect("应能列目录")
            .into_iter()
            .find(|entry| entry.name == "resume.bin.rmxpart")
            .map(|entry| entry.size)
            .expect("临时文件应可查到大小");
        assert!(
            partial > 0 && partial < content.len() as u64,
            "残留大小应在 0 与文件大小之间，实际：{partial}"
        );

        // 续传：从残留末尾接着写，最终改名成目标
        let never_cancel = AtomicBool::new(false);
        let outcome = client
            .upload_file_from_with(
                "/incoming/resume.bin",
                &source,
                chunk,
                true,
                &mut |_| {},
                &never_cancel,
            )
            .expect("应能续传完成");
        assert_eq!(
            outcome.resumed_from, partial,
            "续传起点应等于残留大小，实际：{outcome:?}"
        );
        assert_eq!(outcome.total, content.len() as u64, "总大小应是源文件大小");
        assert_eq!(
            outcome.written + outcome.resumed_from,
            content.len() as u64,
            "本次写入加续传偏移应等于文件大小，实际：{outcome:?}"
        );

        let uploaded = client
            .read_file("/incoming/resume.bin", content.len() + 16)
            .expect("应能读回远端文件");
        assert_eq!(uploaded, content, "续传后的内容应与源文件逐字节一致");
        let names = remote_names(&client, "/incoming");
        assert!(
            !names.iter().any(|name| name.ends_with(PART_SUFFIX)),
            "成功后临时文件应消失，实际：{names:?}"
        );

        client.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelled_error_reports_bytes_written() {
        let (client, dir) = connect_with_incoming();
        let source = dir.join("cancel.bin");
        std::fs::write(&source, patterned_bytes(512 * 1024)).expect("应能写入本地测试文件");

        let cancel = AtomicBool::new(false);
        let mut seen = 0u64;
        let mut progress = |bytes: u64| {
            seen = bytes;
            if bytes >= 128 * 1024 {
                cancel.store(true, Ordering::SeqCst);
            }
        };
        let err = client
            .upload_file_from_with(
                "/incoming/cancel.bin",
                &source,
                64 * 1024,
                false,
                &mut progress,
                &cancel,
            )
            .expect_err("取消应返回错误");

        let text = err.to_string();
        assert!(
            text.contains(&format!("已写入 {seen} 字节")),
            "文案应带上已写入字节数 {seen}，实际：{text}"
        );

        client.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn leaves_no_temp_file_after_successful_upload() {
        let (client, dir) = connect_with_incoming();
        let source = dir.join("done.bin");
        let content = patterned_bytes(300 * 1024);
        std::fs::write(&source, &content).expect("应能写入本地测试文件");

        let never_cancel = AtomicBool::new(false);
        let outcome = client
            .upload_file_from_with(
                "/incoming/done.bin",
                &source,
                DEFAULT_TRANSFER_CHUNK,
                false,
                &mut |_| {},
                &never_cancel,
            )
            .expect("应能上传成功");
        assert_eq!(outcome.written, content.len() as u64);
        assert_eq!(outcome.resumed_from, 0, "从头传时续传偏移应为 0");

        let names = remote_names(&client, "/incoming");
        assert!(
            names.iter().any(|name| name == "done.bin"),
            "目标文件应存在，实际：{names:?}"
        );
        assert!(
            !names.iter().any(|name| name.ends_with(PART_SUFFIX)),
            "成功后不应留下临时文件，实际：{names:?}"
        );

        client.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keeps_remote_clean_when_local_source_is_missing() {
        let (client, dir) = connect_with_incoming();
        let missing = dir.join("nope.bin");
        let never_cancel = AtomicBool::new(false);

        let err = client
            .upload_file_from_with(
                "/incoming/nope.bin",
                &missing,
                DEFAULT_TRANSFER_CHUNK,
                false,
                &mut |_| {},
                &never_cancel,
            )
            .expect_err("本地源不存在时应报错");
        assert!(
            err.to_string().contains("本地文件"),
            "错误应指明是本地文件问题，实际：{err}"
        );

        let names = remote_names(&client, "/incoming");
        assert!(
            names.is_empty(),
            "本地源不存在时远端不应留下任何文件，实际：{names:?}"
        );

        client.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_upload_of_symbolic_link() {
        let (client, dir) = connect_with_incoming();
        let real = dir.join("real.bin");
        std::fs::write(&real, b"real-payload").expect("应能写入真实文件");
        let link = dir.join("link.bin");
        std::os::unix::fs::symlink(&real, &link).expect("应能创建符号链接");

        let never_cancel = AtomicBool::new(false);
        let err = client
            .upload_file_from_with(
                "/incoming/link.bin",
                &link,
                DEFAULT_TRANSFER_CHUNK,
                false,
                &mut |_| {},
                &never_cancel,
            )
            .expect_err("符号链接应被拒绝");
        let text = err.to_string();
        assert!(
            text.contains("不支持上传符号链接"),
            "错误应点明不支持符号链接，实际：{text}"
        );
        assert!(
            matches!(err, CoreError::Validation(_)),
            "符号链接应是校验错误，实际：{err:?}"
        );
        assert!(
            remote_names(&client, "/incoming").is_empty(),
            "被拒绝时远端不应有任何残留"
        );

        client.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn copies_local_mtime_and_mode_to_remote() {
        use std::os::unix::fs::PermissionsExt;

        let (client, dir) = connect_with_incoming();
        let source = dir.join("attrs.bin");
        std::fs::write(&source, b"attrs-payload").expect("应能写入本地测试文件");
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o640))
            .expect("应能改权限");

        let local_meta = std::fs::metadata(&source).expect("应能读本地属性");
        let local_mtime = local_meta
            .modified()
            .expect("应能取到修改时间")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("修改时间应在纪元之后")
            .as_secs();

        let never_cancel = AtomicBool::new(false);
        client
            .upload_file_from_with(
                "/incoming/attrs.bin",
                &source,
                DEFAULT_TRANSFER_CHUNK,
                false,
                &mut |_| {},
                &never_cancel,
            )
            .expect("应能上传成功");

        let entry = client
            .list("/incoming")
            .expect("应能列目录")
            .into_iter()
            .find(|item| item.name == "attrs.bin")
            .expect("远端应能查到目标文件");

        let diff = (entry.modified as i64 - local_mtime as i64).abs();
        assert!(diff <= 2, "远端 mtime 应与本地一致（允许 2 秒误差），实际差 {diff} 秒");
        assert_eq!(
            entry.permissions & 0o777,
            0o640,
            "远端权限应复刻本地的 0o640，实际：{:o}",
            entry.permissions
        );

        client.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_download_of_remote_symbolic_link() {
        let store = runtime()
            .block_on(async { Store::open_in_memory().await })
            .expect("内存库");
        let (port, fs) = runtime().block_on(spawn_server_with_fs(ServerMode::InMemory));
        let client = SftpClient::connect(&store, &test_options(port)).expect("应能建立 SFTP 会话");

        client.create_dir("/incoming").expect("应能新建远端目录");
        client
            .write_file("/incoming/real.bin", b"real-payload")
            .expect("应能写入远端文件");
        // 直接在内存文件系统里塞一个链接（SFTP v3 的 SYMLINK 依赖服务端扩展，这里不走协议）
        fs.links
            .lock()
            .expect("锁可用")
            .insert("/incoming/link.bin".to_string(), "/incoming/real.bin".to_string());

        let dir = temp_dir();
        let target = dir.join("link.bin");
        let never_cancel = AtomicBool::new(false);
        let err = client
            .download_file_to_with(
                "/incoming/link.bin",
                &target,
                DEFAULT_TRANSFER_CHUNK,
                false,
                &mut |_| {},
                &never_cancel,
            )
            .expect_err("远端符号链接应被拒绝");
        assert!(
            err.to_string().contains("不支持下载符号链接"),
            "错误应点明不支持下载符号链接，实际：{err}"
        );
        assert!(!target.exists(), "被拒绝时不该在本地落下任何文件");

        client.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resumes_download_after_cancel_with_leftover_temp_file() {
        let (client, dir) = connect_with_incoming();
        let content = patterned_bytes(1024 * 1024);
        client
            .write_file("/incoming/big.bin", &content)
            .expect("应能写入远端文件");
        let target = dir.join("big.bin");
        let part = dir.join(format!("big.bin{PART_SUFFIX}"));

        let cancel = AtomicBool::new(false);
        let mut progress = |bytes: u64| {
            if bytes >= 320 * 1024 {
                cancel.store(true, Ordering::SeqCst);
            }
        };
        let err = client
            .download_file_to_with(
                "/incoming/big.bin",
                &target,
                64 * 1024,
                false,
                &mut progress,
                &cancel,
            )
            .expect_err("中途取消应返回错误");
        assert!(
            err.to_string().contains("传输已取消"),
            "错误应说明是取消，实际：{err}"
        );
        assert!(!target.exists(), "目标名上不应出现半截文件");
        let partial = std::fs::metadata(&part)
            .expect("取消后应留下本地临时文件")
            .len();
        assert!(
            partial > 0 && partial < content.len() as u64,
            "残留大小应在 0 与文件大小之间，实际：{partial}"
        );

        let never_cancel = AtomicBool::new(false);
        let outcome = client
            .download_file_to_with(
                "/incoming/big.bin",
                &target,
                64 * 1024,
                true,
                &mut |_| {},
                &never_cancel,
            )
            .expect("应能续传下载");
        assert_eq!(outcome.resumed_from, partial, "续传起点应等于残留大小");
        assert_eq!(
            outcome.written + outcome.resumed_from,
            content.len() as u64,
            "本次写入加续传偏移应等于文件大小，实际：{outcome:?}"
        );
        assert_eq!(
            std::fs::read(&target).expect("应能读回本地文件"),
            content,
            "续传后的内容应与远端逐字节一致"
        );
        assert!(!part.exists(), "完成后临时文件应消失");

        client.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回归用例：在阻塞线程池里跑一次上传，不能被当成「异步上下文」拒掉。
    ///
    /// 参数：无
    /// 核心流程：复刻命令层形状（run_transfer 用 spawn_blocking 包一层）→ 在阻塞线程里
    /// 上传一个小文件 → 断言传输成功、远端内容逐字节一致、临时文件已清理。
    /// tokio 的阻塞线程会进入运行时上下文，旧的 Handle::try_current 守卫在这里必然误判，
    /// 因此这条用例就是那个缺陷的看门狗。
    #[test]
    fn upload_succeeds_inside_spawn_blocking() {
        let (client, dir) = connect_with_incoming();
        let source = dir.join("blocking.bin");
        let content = patterned_bytes(64 * 1024);
        let expected = content.clone();
        let size = content.len() as u64;
        std::fs::write(&source, &content).expect("应能写入本地测试文件");

        let result = runtime().block_on(async move {
            tokio::task::spawn_blocking(move || {
                let never_cancel = AtomicBool::new(false);
                let outcome = client.upload_file_from_with(
                    "/incoming/blocking.bin",
                    &source,
                    0,
                    false,
                    &mut |_| {},
                    &never_cancel,
                )?;
                let uploaded = client.read_file("/incoming/blocking.bin", size as usize + 16)?;
                let names = remote_names(&client, "/incoming");
                client.close();
                Ok::<_, CoreError>((outcome, uploaded, names))
            })
            .await
            .expect("阻塞任务不应 panic")
        });

        let (outcome, uploaded, names) = match result {
            Ok(value) => value,
            Err(err) => panic!("阻塞线程里上传应成功，实际报错：{err}"),
        };
        assert_eq!(
            outcome.written, size,
            "应写完整份文件，实际：{outcome:?}"
        );
        assert_eq!(uploaded, expected, "远端内容应与本地逐字节一致");
        assert!(
            !names.iter().any(|name| name.ends_with(PART_SUFFIX)),
            "成功后临时文件应消失，实际：{names:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回归用例：上传到已存在的远端同名文件必须能覆盖。
    ///
    /// 参数：无
    /// 核心流程：测试服务端还原 OpenSSH 的 RENAME 语义（目标已存在就回 SSH_FX_FAILURE）→
    /// 先传内容 A，再在 spawn_blocking 里把内容 B 传到同一路径（复刻生产路径）→
    /// 断言第二次成功、远端内容为 B、无 `.rmxpart` 残留，且服务端确实拒绝过一次 RENAME，
    /// 证明走的是「先删旧文件再改名」的兜底路径，而不是恰好没触发覆盖。
    #[test]
    fn overwrites_existing_remote_file_on_upload() {
        let store = runtime()
            .block_on(async { Store::open_in_memory().await })
            .expect("内存库");
        let (port, fs) = runtime().block_on(spawn_server_with_fs(ServerMode::InMemory));
        let client = SftpClient::connect(&store, &test_options(port)).expect("应能建立 SFTP 会话");
        client.create_dir("/incoming").expect("应能新建远端目录");

        let dir = temp_dir();
        let source = dir.join("overwrite.bin");

        // 第一次：内容 A
        let first_content = patterned_bytes(48 * 1024);
        std::fs::write(&source, &first_content).expect("应能写入本地测试文件");
        let never_cancel = AtomicBool::new(false);
        client
            .upload_file_from_with(
                "/incoming/overwrite.bin",
                &source,
                0,
                false,
                &mut |_| {},
                &never_cancel,
            )
            .expect("首次上传应成功");
        assert_eq!(
            client
                .read_file("/incoming/overwrite.bin", first_content.len() + 16)
                .expect("应能读回远端文件"),
            first_content,
            "首次上传后远端应是内容 A"
        );

        // 第二次：内容 B 传到同一路径，长度也不同，避免「大小一样但内容错」蒙过去
        let second_content = patterned_bytes(96 * 1024 + 7);
        std::fs::write(&source, &second_content).expect("应能改写本地测试文件");
        let expected = second_content.clone();
        let size = second_content.len();
        let client_for_upload = client.clone();

        let result = runtime().block_on(async move {
            tokio::task::spawn_blocking(move || {
                let never_cancel = AtomicBool::new(false);
                let outcome = client_for_upload.upload_file_from_with(
                    "/incoming/overwrite.bin",
                    &source,
                    0,
                    false,
                    &mut |_| {},
                    &never_cancel,
                )?;
                let uploaded =
                    client_for_upload.read_file("/incoming/overwrite.bin", size + 16)?;
                let names = remote_names(&client_for_upload, "/incoming");
                Ok::<_, CoreError>((outcome, uploaded, names))
            })
            .await
            .expect("阻塞任务不应 panic")
        });

        let (outcome, uploaded, names) = match result {
            Ok(value) => value,
            Err(err) => panic!("覆盖已存在的远端文件应成功，实际报错：{err}"),
        };
        assert_eq!(
            outcome.written, size as u64,
            "应写完整份新内容，实际：{outcome:?}"
        );
        assert_eq!(uploaded, expected, "远端内容应被内容 B 完全覆盖");
        assert!(
            names.iter().any(|name| name == "overwrite.bin"),
            "目标文件应存在，实际：{names:?}"
        );
        assert!(
            !names.iter().any(|name| name.ends_with(PART_SUFFIX)),
            "成功后不应留下临时文件，实际：{names:?}"
        );

        // 服务端拒绝过一次 RENAME：说明覆盖兜底路径确实被走到
        let rejections = *fs.rename_rejections.lock().expect("锁可用");
        assert_eq!(
            rejections, 1,
            "应恰好触发一次「目标已存在」的改名拒绝，实际：{rejections}"
        );

        client.close();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
