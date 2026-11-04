//! SSH 隧道：本地转发、远程转发与 SOCKS5 动态转发。
//!
//! 每条隧道独占一条 SSH 连接（与 shell、SFTP 的连接互不影响），
//! 同步 API 内部把异步流程派发到常驻运行时，与 ssh/sftp 共用同一套线程。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, watch};

use crate::error::{CoreError, CoreResult};
use crate::ssh::{runtime, ForwardedChannel, SshConnection, SshOptions};
use crate::store::Store;

/// 启动隧道的等待上限。
///
/// 参数：无
/// 核心流程：握手、认证与远端转发申请都在这个上限内完成，超时按隧道错误返回，
/// 避免命令层因为网络黑洞永久挂住。
const START_TIMEOUT: Duration = Duration::from_secs(20);

/// 停止隧道的等待上限。
///
/// 参数：无
/// 核心流程：撤销远端监听、等监听循环退出、断开连接三步共用这个上限。
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// 转发方向。
///
/// 无参数
/// 核心流程：Local 为本地监听转发到远端目标；Remote 为远端监听转发回本地直连目标；
/// Dynamic 为本地 SOCKS5，目标由客户端逐连接给出。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TunnelKind {
    Local,
    Remote,
    Dynamic,
}

impl TunnelKind {
    /// 取稳定的字符串标识。
    ///
    /// 参数：无
    /// 核心流程：与 serde 的序列化结果保持一致（全小写），
    /// 命令层与前端判断类型时不用再匹配枚举。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
            Self::Dynamic => "dynamic",
        }
    }

    /// 从配置里的字符串还原，未知值返回 None。
    ///
    /// 参数：raw 为落库或前端传来的类型字符串
    /// 核心流程：与 as_str 一一对应，未知类型交给调用方报明确错误，
    /// 不做"猜一个最近似类型"的静默兜底。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "local" => Some(Self::Local),
            "remote" => Some(Self::Remote),
            "dynamic" => Some(Self::Dynamic),
            _ => None,
        }
    }

    /// 取中文名称。
    ///
    /// 参数：无
    /// 核心流程：给描述文案与错误提示用，避免在格式化处写三份 match。
    fn label(&self) -> &'static str {
        match self {
            Self::Local => "本地转发",
            Self::Remote => "远程转发",
            Self::Dynamic => "动态转发",
        }
    }
}

/// 一条隧道的参数。
///
/// 无参数
/// 核心流程：kind 决定语义，listen_* 是建隧道这一侧的监听点，
/// target_* 是被访问的目标（Dynamic 忽略这两个字段，由 SOCKS5 请求逐连接给出）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelSpec {
    pub kind: TunnelKind,
    pub listen_host: String,
    pub listen_port: u16,
    pub target_host: String,
    pub target_port: u16,
}

impl TunnelSpec {
    /// 校验参数是否可执行。
    ///
    /// 参数：无
    /// 核心流程：监听地址必填、端口 0 表示让系统/远端分配故放行；
    /// Local 与 Remote 必须有目标，Dynamic 由 SOCKS5 请求给出目标所以跳过。
    pub fn validate(&self) -> CoreResult<()> {
        if self.listen_host.trim().is_empty() {
            return Err(CoreError::Tunnel("监听地址不能为空".into()));
        }
        if matches!(self.kind, TunnelKind::Dynamic) {
            return Ok(());
        }
        if self.target_host.trim().is_empty() {
            return Err(CoreError::Tunnel(format!(
                "{} 需要填目标地址",
                self.kind.label()
            )));
        }
        if self.target_port == 0 {
            return Err(CoreError::Tunnel(format!(
                "{} 需要填 1 到 65535 之间的目标端口",
                self.kind.label()
            )));
        }
        Ok(())
    }
}

/// 一条运行中的隧道。
///
/// 无参数
/// 核心流程：内部用 Arc 共享「隧道自己的 SSH 连接 + 关闭信号 + 运行标志」，
/// 克隆出的副本指向同一条隧道，stop 只生效一次且对多次调用幂等。
#[derive(Clone)]
pub struct TunnelHandle {
    inner: Arc<TunnelInner>,
}

/// 隧道的共享状态。
///
/// 无参数
/// 核心流程：连接用于开通道与断开；shutdown 通知监听循环退出；
/// finished 让 stop 能等到端口真正释放，避免调用方立刻重绑同一端口时偶发失败。
struct TunnelInner {
    spec: TunnelSpec,
    /// 实际生效的监听端口（本地绑定或远端分配的端口）。
    listen_port: u16,
    connection: SshConnection,
    shutdown: watch::Sender<bool>,
    running: Arc<AtomicBool>,
    finished: Mutex<Option<oneshot::Receiver<()>>>,
}

impl TunnelHandle {
    /// 启动一条隧道（同步）。
    ///
    /// 参数：spec 为隧道参数，store 为已知主机库，options 为连接参数
    /// 核心流程：先做参数校验 -> 把「握手 + 建立监听」的异步流程派发到常驻运行时 ->
    /// 同步侧用一次性通道等结果；超时与任务异常各给独立文案。
    pub fn start(spec: &TunnelSpec, store: &Store, options: &SshOptions) -> CoreResult<Self> {
        spec.validate()?;
        let spec = spec.clone();
        let store = store.clone();
        let options = options.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        runtime().spawn(async move {
            let _ = tx.send(start_async(spec, &store, &options).await);
        });
        match rx.recv_timeout(START_TIMEOUT) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(CoreError::Tunnel(format!(
                "启动超时（{} 秒内没有完成握手或监听申请）",
                START_TIMEOUT.as_secs()
            ))),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(CoreError::Tunnel("启动失败：后台任务意外退出".into()))
            }
        }
    }

    /// 停止隧道。
    ///
    /// 参数：无
    /// 核心流程：翻转运行标志并通知监听循环 -> 等它退出（端口随之释放） ->
    /// 远程转发要先撤销远端监听 -> 最后断开这条隧道独占的 SSH 连接；
    /// 重复调用直接返回成功，不会重复拆连接。
    pub fn stop(&self) -> CoreResult<()> {
        // 只有第一次调用真正拆，其余调用视为已完成
        if !self.inner.running.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        let _ = self.inner.shutdown.send(true);
        let finished = self
            .inner
            .finished
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());

        let connection = self.inner.connection.clone();
        let spec = self.inner.spec.clone();
        let listen_port = self.inner.listen_port;
        let (tx, rx) = std::sync::mpsc::channel();
        runtime().spawn(async move {
            // 等监听循环退出 -> 端口在重启同一隧道前已经真正释放
            if let Some(done) = finished {
                let _ = tokio::time::timeout(STOP_TIMEOUT, done).await;
            }
            // 远程转发必须显式撤销，否则远端会一直占着那个端口
            if matches!(spec.kind, TunnelKind::Remote) {
                let _ = connection
                    .cancel_tcpip_forward(&spec.listen_host, listen_port)
                    .await;
            }
            let _ = connection.disconnect().await;
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(STOP_TIMEOUT);
        Ok(())
    }

    /// 取一条人可读的隧道描述。
    ///
    /// 参数：无
    /// 核心流程：按方向拼出「监听点 -> 目标」；动态转发没有固定目标，
    /// 监听点后直接标 SOCKS5，前端列表里可直接展示。
    pub fn describe(&self) -> String {
        let spec = &self.inner.spec;
        let listen = format!("{}:{}", spec.listen_host, self.inner.listen_port);
        match spec.kind {
            TunnelKind::Local => {
                format!("{listen} -> {}:{}", spec.target_host, spec.target_port)
            }
            TunnelKind::Remote => format!(
                "远端 {listen} -> {}:{}",
                spec.target_host, spec.target_port
            ),
            TunnelKind::Dynamic => format!("{listen} -> SOCKS5 动态转发"),
        }
    }

    /// 隧道是否仍在工作。
    ///
    /// 参数：无
    /// 核心流程：既要没被 stop，也要底层 SSH 连接没断；
    /// 服务端把连接踢掉时这里会立刻变成 false，前端据此提示重连。
    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::SeqCst) && !self.inner.connection.is_closed()
    }

    /// 取实际生效的监听端口。
    ///
    /// 参数：无
    /// 核心流程：listen_port 传 0 时由系统或远端分配，
    /// 调用方拿这个返回值才知道真实端口。
    pub fn listen_port(&self) -> u16 {
        self.inner.listen_port
    }

    /// 取隧道参数。
    ///
    /// 参数：无
    /// 核心流程：只读借用，供命令层回填前端列表。
    pub fn spec(&self) -> &TunnelSpec {
        &self.inner.spec
    }
}

/// 启动隧道的异步实现。
///
/// 参数：spec 为隧道参数，store 为已知主机库，options 为连接参数
/// 核心流程：本地与动态转发先占本地端口（端口被占用时立即报错，不必等握手）再连 SSH；
/// 远程转发先连 SSH 并申请远端监听，最后把监听循环交给后台任务。
async fn start_async(
    spec: TunnelSpec,
    store: &Store,
    options: &SshOptions,
) -> CoreResult<TunnelHandle> {
    let running = Arc::new(AtomicBool::new(true));
    let (shutdown, _) = watch::channel(false);
    // 订阅必须早于返回句柄 -> stop 抢在后台任务启动之前时也不会漏掉唤醒
    let shutdown_rx = shutdown.subscribe();
    let (done_tx, done_rx) = oneshot::channel();

    let (connection, listen_port, task) = match spec.kind {
        TunnelKind::Local => {
            let listener = bind_local(&spec).await?;
            let listen_port = listener.local_addr().map_err(|err| {
                CoreError::Tunnel(format!("读取本地监听地址失败：{err}"))
            })?.port();
            let connection = SshConnection::connect_async(store, options).await?;
            let serve = ServeTask::Local {
                connection: connection.clone(),
                spec: spec.clone(),
                listener,
            };
            (connection, listen_port, serve)
        }
        TunnelKind::Dynamic => {
            let listener = bind_local(&spec).await?;
            let listen_port = listener.local_addr().map_err(|err| {
                CoreError::Tunnel(format!("读取本地监听地址失败：{err}"))
            })?.port();
            let connection = SshConnection::connect_async(store, options).await?;
            let serve = ServeTask::Dynamic {
                connection: connection.clone(),
                listener,
            };
            (connection, listen_port, serve)
        }
        TunnelKind::Remote => {
            // forwarded-tcpip 信箱必须在握手前装进 handler，事后补不上
            let (forward_tx, forward_rx) = mpsc::unbounded_channel();
            let connection =
                SshConnection::connect_async_with_forward(store, options, forward_tx).await?;
            let remote_port = match connection
                .tcpip_forward(&spec.listen_host, spec.listen_port)
                .await
            {
                Ok(port) => port,
                Err(err) => {
                    // 远端不接这个请求 -> 立刻断掉连接，别留下只建了一半的隧道
                    let _ = connection.disconnect().await;
                    return Err(CoreError::Tunnel(format!(
                        "远端拒绝转发 {}:{}（服务端可能未开启 AllowTcpForwarding）：{err}",
                        spec.listen_host, spec.listen_port
                    )));
                }
            };
            let listen_port = u16::try_from(remote_port).unwrap_or(spec.listen_port);
            let serve = ServeTask::Remote {
                connection: connection.clone(),
                spec: spec.clone(),
                forwarded: forward_rx,
            };
            (connection, listen_port, serve)
        }
    };

    spawn_serve(task, shutdown_rx, done_tx);
    Ok(TunnelHandle {
        inner: Arc::new(TunnelInner {
            spec,
            listen_port,
            connection,
            shutdown,
            running,
            finished: Mutex::new(Some(done_rx)),
        }),
    })
}

/// 绑定本地监听端口。
///
/// 参数：spec 为隧道参数（用其中的 listen_host/listen_port）
/// 核心流程：端口被占用时给「换一个端口」的中文提示，
/// 其余错误（无权限、地址不可用）原样带上地址与原因。
async fn bind_local(spec: &TunnelSpec) -> CoreResult<TcpListener> {
    match TcpListener::bind((spec.listen_host.as_str(), spec.listen_port)).await {
        Ok(listener) => Ok(listener),
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => Err(CoreError::Tunnel(format!(
            "{} 监听失败：{}:{} 已被占用，请换一个端口",
            spec.kind.label(),
            spec.listen_host,
            spec.listen_port
        ))),
        Err(err) => Err(CoreError::Tunnel(format!(
            "{} 监听失败：无法监听 {}:{}：{err}",
            spec.kind.label(),
            spec.listen_host,
            spec.listen_port
        ))),
    }
}

/// 后台监听循环要跑的具体形态。
///
/// 无参数
/// 核心流程：三种隧道的前置步骤不同（本地绑端口 / 远端申请转发 / 本地 SOCKS5），
/// 收尾都是「跑循环直到收到关闭信号」，这里只负责把各自的上下文传下去。
enum ServeTask {
    /// 本地监听 -> direct-tcpip 通道。
    Local {
        connection: SshConnection,
        spec: TunnelSpec,
        listener: TcpListener,
    },
    /// 本地 SOCKS5 -> direct-tcpip 通道。
    Dynamic {
        connection: SshConnection,
        listener: TcpListener,
    },
    /// 远端监听 -> 本地直连目标。
    Remote {
        connection: SshConnection,
        spec: TunnelSpec,
        forwarded: mpsc::UnboundedReceiver<ForwardedChannel>,
    },
}

/// 把监听循环交给常驻运行时。
///
/// 参数：task 为循环上下文，shutdown 为关闭信号，done 为退出回执
/// 核心流程：循环返回后立刻回执，stop 靠这个回执确认端口已释放。
fn spawn_serve(task: ServeTask, shutdown: watch::Receiver<bool>, done: oneshot::Sender<()>) {
    runtime().spawn(async move {
        match task {
            ServeTask::Local {
                connection,
                spec,
                listener,
            } => serve_local(connection, spec, listener, shutdown).await,
            ServeTask::Dynamic {
                connection,
                listener,
            } => serve_dynamic(connection, listener, shutdown).await,
            ServeTask::Remote {
                connection,
                spec,
                forwarded,
            } => serve_remote(connection, spec, forwarded, shutdown).await,
        }
        let _ = done.send(());
    });
}

/// 本地转发主循环。
///
/// 参数：connection 为隧道独占的连接，spec 为目标，listener 为已占好的本地端口，
/// shutdown 为关闭信号
/// 核心流程：每接受一个本地连接就单独起一个任务去和远端建 direct-tcpip 通道，
/// 单个连接失败不影响其他连接；收到关闭信号就退出并把端口交还系统。
async fn serve_local(
    connection: SshConnection,
    spec: TunnelSpec,
    listener: TcpListener,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut health = tokio::time::interval(HEALTH_INTERVAL);
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            // 连接被服务端断开 -> 退出循环并释放本地端口
            _ = health.tick() => {
                if connection.is_closed() {
                    break;
                }
            }
            accepted = listener.accept() => {
                let Ok((stream, peer)) = accepted else { continue };
                let connection = connection.clone();
                let target_host = spec.target_host.clone();
                let target_port = spec.target_port;
                tokio::spawn(async move {
                    relay_direct(&connection, &target_host, target_port, stream, peer).await;
                });
            }
        }
    }
}

/// 把一条本地 TCP 连接接到远端的 direct-tcpip 通道上。
///
/// 参数：connection 为隧道连接，target_host/target_port 为目标，
/// stream 为本地连接，peer 为本地来访者地址（作为 originator 报给远端）
/// 核心流程：开通道 -> 双向拷贝字节；目标不可达时远端会拒绝开通道，
/// 这里直接断开本地连接，避免前端看到一条「连上了却永远没数据」的连接。
async fn relay_direct(
    connection: &SshConnection,
    target_host: &str,
    target_port: u16,
    stream: tokio::net::TcpStream,
    peer: std::net::SocketAddr,
) {
    let mut stream = stream;
    let channel = match connection
        .open_direct_tcpip(
            target_host,
            target_port,
            &peer.ip().to_string(),
            peer.port(),
        )
        .await
    {
        Ok(channel) => channel,
        Err(_) => return,
    };
    let mut remote = channel.into_stream();
    let _ = tokio::io::copy_bidirectional(&mut stream, &mut remote).await;
}

/// SOCKS5 协议版本号。
///
/// 参数：无
/// 核心流程：握手与请求报文的首字节都必须是它，其他值一律按非法版本拒绝。
const SOCKS5_VERSION: u8 = 0x05;

/// 无认证方式编号。
///
/// 参数：无
/// 核心流程：本轮只支持免认证，客户端不提供它就没法谈。
const SOCKS5_METHOD_NONE: u8 = 0x00;

/// CONNECT 命令编号。
///
/// 参数：无
/// 核心流程：BIND 与 UDP ASSOCIATE 都不支持，收到就回「命令不支持」。
const SOCKS5_CMD_CONNECT: u8 = 0x01;

/// IPv4 地址类型。
///
/// 参数：无
/// 核心流程：后跟 4 字节地址，转成点分十进制交给远端代连。
const SOCKS5_ATYP_IPV4: u8 = 0x01;

/// 域名地址类型。
///
/// 参数：无
/// 核心流程：后跟 1 字节长度 + 域名，原样交给远端解析，本地不做 DNS。
const SOCKS5_ATYP_DOMAIN: u8 = 0x03;

/// 成功应答码。
///
/// 参数：无
/// 核心流程：回复成功后客户端才开始发业务数据。
const SOCKS5_REPLY_OK: u8 = 0x00;

/// 目标不可达应答码。
///
/// 参数：无
/// 核心流程：远端拒绝开通道（目标不可达/被策略拒绝）时回这个码，
/// 浏览器等客户端据此立刻报错而不是干等。
const SOCKS5_REPLY_HOST_UNREACHABLE: u8 = 0x04;

/// 命令不支持应答码。
///
/// 参数：无
/// 核心流程：收到 BIND / UDP ASSOCIATE 时回这个码。
const SOCKS5_REPLY_CMD_UNSUPPORTED: u8 = 0x07;

/// 连接健康检查间隔。
///
/// 参数：无
/// 核心流程：SSH 连接被服务端断开时 russh 不会回调通知我们，
/// 监听循环按这个间隔轮询一次，及时退出并把端口/远端转发释放掉。
const HEALTH_INTERVAL: Duration = Duration::from_secs(5);

/// SOCKS5 握手等待上限。
///
/// 参数：无
/// 核心流程：连上本地端口却不发数据的客户端不能一直占着任务，
/// 超时按握手失败处理并断开。
const SOCKS5_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// 一个 SOCKS5 CONNECT 请求解析出的目标。
///
/// 无参数
/// 核心流程：host 可能是 IPv4 点分或域名（都交给远端解析连），port 为 1 到 65535。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Socks5Target {
    host: String,
    port: u16,
}

/// 从客户端提供的方法列表里选择认证方式。
///
/// 参数：version 为客户端声明的协议版本，methods 为它支持的方法编号
/// 核心流程：版本必须是 5 -> 必须包含「无认证」（0x00）-> 返回 0x00；
/// 任一不满足都给中文原因，调用方据此决定是否回复「无可用方法」。
fn socks5_pick_method(version: u8, methods: &[u8]) -> Result<u8, String> {
    if version != SOCKS5_VERSION {
        return Err(format!("不支持的 SOCKS 版本：{version}（只支持 SOCKS5）"));
    }
    if !methods.contains(&SOCKS5_METHOD_NONE) {
        return Err("客户端不接受无认证方式，本轮只支持免认证".into());
    }
    Ok(SOCKS5_METHOD_NONE)
}

/// 把请求里的地址段解成字符串。
///
/// 参数：addr_type 为地址类型，raw 为该类型对应的原始字节
/// 核心流程：IPv4 转点分十进制；域名按 UTF-8 解；
/// IPv6 与未知类型直接报错（本轮按要求只支持 IPv4 与域名）。
fn socks5_parse_address(addr_type: u8, raw: &[u8]) -> Result<String, String> {
    match addr_type {
        SOCKS5_ATYP_IPV4 => {
            if raw.len() != 4 {
                return Err(format!("IPv4 地址长度应为 4 字节，实际 {}", raw.len()));
            }
            Ok(std::net::Ipv4Addr::new(raw[0], raw[1], raw[2], raw[3]).to_string())
        }
        SOCKS5_ATYP_DOMAIN => {
            if raw.is_empty() {
                return Err("域名长度不能为 0".into());
            }
            String::from_utf8(raw.to_vec()).map_err(|_| "域名不是合法的 UTF-8 文本".to_string())
        }
        other => Err(format!(
            "暂不支持该地址类型 {other}（只支持 IPv4 与域名）"
        )),
    }
}

/// 校验 CONNECT 请求并给出目标。
///
/// 参数：version 为请求版本，command 为命令编号，host/port 为解析出的目标
/// 核心流程：版本与命令都必须受支持、端口不能为 0，全部通过才返回目标；
/// 这样协议判定与网络读写解耦，单测可以直接覆盖各种非法组合。
fn socks5_parse_connect(
    version: u8,
    command: u8,
    host: &str,
    port: u16,
) -> Result<Socks5Target, String> {
    if version != SOCKS5_VERSION {
        return Err(format!("不支持的 SOCKS 版本：{version}（只支持 SOCKS5）"));
    }
    if command != SOCKS5_CMD_CONNECT {
        return Err(format!("只支持 CONNECT 命令，收到 {command}"));
    }
    if host.is_empty() {
        return Err("目标地址不能为空".into());
    }
    if port == 0 {
        return Err("目标端口不能为 0".into());
    }
    Ok(Socks5Target {
        host: host.to_string(),
        port,
    })
}

/// 完成一次 SOCKS5 协商与 CONNECT 解析。
///
/// 参数：stream 为已接受的本地连接
/// 核心流程：读版本与方法列表 -> 回「无认证」-> 读请求头 -> 按地址类型读地址与端口 ->
/// 地址非法或类型不支持时回对应应答码再报错，成功只做解析不做连接。
async fn socks5_negotiate(stream: &mut tokio::net::TcpStream) -> CoreResult<Socks5Target> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // 版本 + 方法数
    let mut head = [0u8; 2];
    stream
        .read_exact(&mut head)
        .await
        .map_err(|err| CoreError::Tunnel(format!("SOCKS5 协商失败：读取版本与方法数出错：{err}")))?;
    let mut methods = vec![0u8; usize::from(head[1])];
    if !methods.is_empty() {
        stream.read_exact(&mut methods).await.map_err(|err| {
            CoreError::Tunnel(format!("SOCKS5 协商失败：读取认证方式列表出错：{err}"))
        })?;
    }
    let method = socks5_pick_method(head[0], &methods).map_err(|reason| {
        // 版本/方法不匹配 -> 回 0xFF 告知没有可用方式
        CoreError::Tunnel(format!("SOCKS5 协商失败：{reason}"))
    })?;
    stream
        .write_all(&[SOCKS5_VERSION, method])
        .await
        .map_err(|err| CoreError::Tunnel(format!("SOCKS5 协商失败：回复认证方式出错：{err}")))?;

    // 请求头：版本、命令、保留位、地址类型
    let mut request = [0u8; 4];
    stream.read_exact(&mut request).await.map_err(|err| {
        CoreError::Tunnel(format!("SOCKS5 请求解析失败：读取请求头出错：{err}"))
    })?;
    let address_type = request[3];

    // 地址段：IPv4 固定 4 字节，域名先读 1 字节长度
    let raw_address = match address_type {
        SOCKS5_ATYP_IPV4 => {
            let mut buffer = [0u8; 4];
            stream.read_exact(&mut buffer).await.map_err(|err| {
                CoreError::Tunnel(format!("SOCKS5 请求解析失败：读取 IPv4 地址出错：{err}"))
            })?;
            buffer.to_vec()
        }
        SOCKS5_ATYP_DOMAIN => {
            let mut length = [0u8; 1];
            stream.read_exact(&mut length).await.map_err(|err| {
                CoreError::Tunnel(format!("SOCKS5 请求解析失败：读取域名长度出错：{err}"))
            })?;
            let mut buffer = vec![0u8; usize::from(length[0])];
            if !buffer.is_empty() {
                stream.read_exact(&mut buffer).await.map_err(|err| {
                    CoreError::Tunnel(format!("SOCKS5 请求解析失败：读取域名出错：{err}"))
                })?;
            }
            buffer
        }
        other => {
            // 不认识的地址类型没法继续读，回「命令不支持」再断开
            let _ = socks5_reply(stream, SOCKS5_REPLY_CMD_UNSUPPORTED).await;
            return Err(CoreError::Tunnel(format!(
                "SOCKS5 请求解析失败：暂不支持该地址类型 {other}（只支持 IPv4 与域名）"
            )));
        }
    };

    let mut port_bytes = [0u8; 2];
    stream.read_exact(&mut port_bytes).await.map_err(|err| {
        CoreError::Tunnel(format!("SOCKS5 请求解析失败：读取目标端口出错：{err}"))
    })?;

    let host = socks5_parse_address(address_type, &raw_address)
        .map_err(|reason| CoreError::Tunnel(format!("SOCKS5 请求解析失败：{reason}")))?;
    let port = u16::from_be_bytes(port_bytes);

    socks5_parse_connect(request[0], request[1], &host, port)
        .map_err(|reason| CoreError::Tunnel(format!("SOCKS5 请求解析失败：{reason}")))
}

/// 回一个 SOCKS5 应答。
///
/// 参数：stream 为本地连接，code 为应答码（0 表示成功）
/// 核心流程：固定用 IPv4 + 0.0.0.0:0 作为绑定地址回填，
/// 客户端只关心应答码，绑定地址对 CONNECT 场景没有实际意义。
async fn socks5_reply(stream: &mut tokio::net::TcpStream, code: u8) -> CoreResult<()> {
    use tokio::io::AsyncWriteExt;

    let reply = [
        SOCKS5_VERSION,
        code,
        0x00, // 保留位
        SOCKS5_ATYP_IPV4,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    stream
        .write_all(&reply)
        .await
        .map_err(|err| CoreError::Tunnel(format!("SOCKS5 应答发送失败：{err}")))
}

/// 动态转发主循环。
///
/// 参数：connection 为隧道独占的连接，listener 为本地 SOCKS5 端口，
/// shutdown 为关闭信号
/// 核心流程：每个来访连接单独起任务跑 SOCKS5 握手，目标由客户端逐连接给出，
/// 因此这里不需要 spec 里的 target_*。
async fn serve_dynamic(
    connection: SshConnection,
    listener: TcpListener,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut health = tokio::time::interval(HEALTH_INTERVAL);
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            // 连接被服务端断开 -> 退出循环并释放本地端口
            _ = health.tick() => {
                if connection.is_closed() {
                    break;
                }
            }
            accepted = listener.accept() => {
                let Ok((stream, peer)) = accepted else { continue };
                let connection = connection.clone();
                tokio::spawn(async move {
                    handle_socks5(connection, stream, peer).await;
                });
            }
        }
    }
}

/// 处理一个 SOCKS5 客户端。
///
/// 参数：connection 为隧道连接，stream 为本地连接，peer 为客户端地址
/// 核心流程：先协商并解析目标（失败就断开）-> 用 direct-tcpip 让远端代连 ->
/// 成功回 0x00 后开始双向拷贝；远端拒绝开通道时回「目标不可达」，客户端立刻知道原因。
async fn handle_socks5(
    connection: SshConnection,
    mut stream: tokio::net::TcpStream,
    peer: std::net::SocketAddr,
) {
    let target = match tokio::time::timeout(SOCKS5_HANDSHAKE_TIMEOUT, socks5_negotiate(&mut stream))
        .await
    {
        Ok(Ok(target)) => target,
        Ok(Err(_)) => return,
        Err(_) => {
            let _ = socks5_reply(&mut stream, SOCKS5_REPLY_HOST_UNREACHABLE).await;
            return;
        }
    };

    let channel = match connection
        .open_direct_tcpip(
            &target.host,
            target.port,
            &peer.ip().to_string(),
            peer.port(),
        )
        .await
    {
        Ok(channel) => channel,
        Err(_) => {
            let _ = socks5_reply(&mut stream, SOCKS5_REPLY_HOST_UNREACHABLE).await;
            return;
        }
    };

    if socks5_reply(&mut stream, SOCKS5_REPLY_OK).await.is_err() {
        return;
    }

    let mut remote = channel.into_stream();
    let _ = tokio::io::copy_bidirectional(&mut stream, &mut remote).await;
}

/// 远程转发主循环。
///
/// 参数：spec 为本地目标，forwarded 为服务端回连通道的信箱，shutdown 为关闭信号
/// 核心流程：每收到一条 forwarded-tcpip 通道就单独起任务去连本地目标；
/// 信箱关闭（连接断开）或收到关闭信号就退出。
async fn serve_remote(
    connection: SshConnection,
    spec: TunnelSpec,
    mut forwarded: mpsc::UnboundedReceiver<ForwardedChannel>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut health = tokio::time::interval(HEALTH_INTERVAL);
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            // 连接断了就不会再有 forwarded-tcpip 通道，及时退出
            _ = health.tick() => {
                if connection.is_closed() {
                    break;
                }
            }
            received = forwarded.recv() => {
                let Some(item) = received else { break };
                let target_host = spec.target_host.clone();
                let target_port = spec.target_port;
                tokio::spawn(async move {
                    relay_forwarded(&target_host, target_port, item).await;
                });
            }
        }
    }
}

/// 把服务端回连的通道接到本地目标上。
///
/// 参数：target_host/target_port 为本地目标，forwarded 为服务端开好的通道
/// 核心流程：直连本地目标 -> 与通道做双向拷贝；本地目标不可达时关掉通道，
/// 退化为「远端看到的连接立刻结束」，语义与 OpenSSH 一致。
async fn relay_forwarded(target_host: &str, target_port: u16, forwarded: ForwardedChannel) {
    let mut local = match tokio::net::TcpStream::connect((target_host, target_port)).await {
        Ok(stream) => stream,
        Err(_) => {
            let _ = forwarded.channel.close().await;
            return;
        }
    };
    let mut remote = forwarded.channel.into_stream();
    let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Mutex;

    use russh::server::{self, Auth, ChannelOpenHandle, Msg, Server, Session};
    use russh::{Channel, ChannelId};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;
    use crate::ssh::SshAuth;

    /// 测试服务端对远程转发的态度。
    #[derive(Debug, Clone, Copy)]
    enum ForwardMode {
        /// 接受申请，并主动开一条 forwarded-tcpip 通道。
        Accept,
        /// 直接拒绝（模拟服务端关掉 AllowTcpForwarding）。
        Refuse,
    }

    /// 服务端被观测到的数据。
    ///
    /// 无参数
    /// 核心流程：远程转发用例里，服务端把从通道读到的字节记在这里，
    /// 测试线程轮询它来断言「本地目标确实收到并回显了数据」。
    #[derive(Clone, Default)]
    struct Probe {
        received: Arc<Mutex<Vec<u8>>>,
    }

    /// 测试用 SSH 服务端。
    #[derive(Clone)]
    struct TestServer {
        mode: ForwardMode,
        probe: Probe,
    }

    struct TestHandler {
        mode: ForwardMode,
        probe: Probe,
        /// 已由本 handler 开出、尚待客户端确认的 forwarded-tcpip 通道。
        pending: Option<ChannelId>,
    }

    impl server::Handler for TestHandler {
        type Error = russh::Error;

        async fn auth_password(
            &mut self,
            _user: &str,
            _password: &str,
        ) -> Result<Auth, Self::Error> {
            Ok(Auth::Accept)
        }

        async fn auth_publickey(
            &mut self,
            _user: &str,
            _key: server::VerifiedPublicKey<'_>,
        ) -> Result<Auth, Self::Error> {
            Ok(Auth::Accept)
        }

        /// 处理本地/动态转发要开的通道。
        ///
        /// 参数：channel 为新通道，host_to_connect/port_to_connect 为客户端要求代连的目标
        /// 核心流程：接受通道 -> 真的去连目标 -> 与通道做双向拷贝，
        /// 这样客户端视角就是「连上本地端口等于连上目标」。
        async fn channel_open_direct_tcpip(
            &mut self,
            channel: Channel<Msg>,
            host_to_connect: &str,
            port_to_connect: u32,
            _originator_address: &str,
            _originator_port: u32,
            reply: ChannelOpenHandle,
            _session: &mut Session,
        ) -> Result<(), Self::Error> {
            reply.accept().await;
            let target = format!("{host_to_connect}:{port_to_connect}");
            tokio::spawn(async move {
                let Ok(mut stream) = TcpStream::connect(&target).await else {
                    let _ = channel.close().await;
                    return;
                };
                let mut remote = channel.into_stream();
                let _ = tokio::io::copy_bidirectional(&mut stream, &mut remote).await;
            });
            Ok(())
        }

        /// 处理远程转发申请。
        ///
        /// 参数：address 为请求监听的地址，port 为请求的端口（可被改写），session 为会话
        /// 核心流程：Refuse 模式直接拒绝；Accept 模式立刻开一条 forwarded-tcpip 通道
        /// 并塞一段标记数据，模拟「远端有人连进来了」，端口改写为 9001 供断言。
        async fn tcpip_forward(
            &mut self,
            address: &str,
            port: &mut u32,
            session: &mut Session,
        ) -> Result<bool, Self::Error> {
            if matches!(self.mode, ForwardMode::Refuse) {
                return Ok(false);
            }
            let channel =
                session.channel_open_forwarded_tcpip(address, *port, "127.0.0.1", 4321)?;
            // 通道要等客户端确认后才能写数据，这里只记下 id
            self.pending = Some(channel);
            *port = 9001;
            Ok(true)
        }

        /// 通道被客户端确认后补发标记数据。
        ///
        /// 参数：id 为刚确认的通道，session 为会话
        /// 核心流程：只有我们自己开出的 forwarded-tcpip 通道才发数据
        /// （russh 规定确认前不能写，所以业务数据集中放在这里）。
        async fn channel_open_confirmation(
            &mut self,
            id: ChannelId,
            _max_packet_size: u32,
            _window_size: u32,
            session: &mut Session,
        ) -> Result<(), Self::Error> {
            if self.pending == Some(id) {
                let _ = session.data(id, b"REMOTE-HELLO".to_vec());
            }
            Ok(())
        }

        /// 记录从转发通道收到的字节。
        ///
        /// 参数：channel 为通道，data 为字节，session 为会话
        /// 核心流程：只记录不回写，避免和本地回显服务互相弹球。
        async fn data(
            &mut self,
            _channel: ChannelId,
            data: &[u8],
            _session: &mut Session,
        ) -> Result<(), Self::Error> {
            self.probe
                .received
                .lock()
                .expect("锁可用")
                .extend_from_slice(data);
            Ok(())
        }
    }

    impl Server for TestServer {
        type Handler = TestHandler;

        fn new_client(&mut self, _peer: Option<SocketAddr>) -> Self::Handler {
            TestHandler {
                mode: self.mode,
                probe: self.probe.clone(),
                pending: None,
            }
        }
    }

    /// 起一个测试 SSH 服务端。
    ///
    /// 参数：mode 为对远程转发的态度
    /// 核心流程：绑 127.0.0.1:0 拿随机端口 -> 生成临时主机密钥 -> 后台跑 russh server，
    /// 返回端口与观测点。
    async fn spawn_server(mode: ForwardMode) -> (u16, Probe) {
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

        let probe = Probe::default();
        let server = TestServer {
            mode,
            probe: probe.clone(),
        };
        tokio::spawn(async move {
            let mut server = server;
            let _ = server.run_on_socket(config, &listener).await;
        });

        (port, probe)
    }

    /// 起一个本地 TCP 回显服务。
    ///
    /// 参数：无
    /// 核心流程：绑 127.0.0.1:0 -> 每个连接单独起任务原样回写，
    /// 作为「隧道目标」验证字节确实走完了全程。
    async fn spawn_tcp_echo() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("应能绑定端口");
        let addr = listener.local_addr().expect("应能取到本地地址");
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buffer = vec![0u8; 4096];
                    while let Ok(read) = stream.read(&mut buffer).await {
                        if read == 0 {
                            break;
                        }
                        if stream.write_all(&buffer[..read]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        addr
    }

    /// 构造指向测试服务端的连接参数。
    ///
    /// 参数：port 为服务端端口
    /// 核心流程：固定 127.0.0.1 + 密码认证 + 已确认指纹，用例只关心端口。
    fn test_options(port: u16) -> SshOptions {
        SshOptions {
            host: "127.0.0.1".into(),
            port,
            username: "tester".into(),
            auth: SshAuth::Password("secret".into()),
            cols: 80,
            rows: 24,
            term: "xterm-256color".into(),
            accept_unknown_host_key: true,
            jump: None,
        }
    }

    /// 打开一个内存会话库。
    ///
    /// 参数：无
    /// 核心流程：每个用例独立一个库，known_hosts 不会互相干扰。
    fn memory_store() -> Store {
        runtime()
            .block_on(async { Store::open_in_memory().await })
            .expect("内存库")
    }

    /// 连上端口写入一段字节并读回等长回显。
    ///
    /// 参数：port 为本地端口，payload 为要发送的字节
    /// 核心流程：带超时地连接、写入、精确读回等长数据，
    /// 任何一步超时都让用例失败而不是挂住。
    fn exchange(port: u16, payload: &[u8]) -> Vec<u8> {
        runtime().block_on(async move {
            const LIMIT: Duration = Duration::from_secs(10);
            let mut stream = tokio::time::timeout(LIMIT, TcpStream::connect(("127.0.0.1", port)))
                .await
                .expect("连接不应超时")
                .expect("应能连上本地监听端口");
            stream.write_all(payload).await.expect("写入应成功");
            let mut buffer = vec![0u8; payload.len()];
            tokio::time::timeout(LIMIT, stream.read_exact(&mut buffer))
                .await
                .expect("读取不应超时")
                .expect("应能读回等长数据");
            buffer
        })
    }

    #[test]
    fn socks5_negotiation_accepts_no_auth() {
        assert_eq!(socks5_pick_method(SOCKS5_VERSION, &[0x00]), Ok(0x00));
        // 列表里带别的认证方式也无所谓，只要有无认证就够
        assert_eq!(socks5_pick_method(SOCKS5_VERSION, &[0x02, 0x00]), Ok(0x00));
    }

    #[test]
    fn socks5_negotiation_rejects_bad_version_and_missing_method() {
        match socks5_pick_method(0x04, &[0x00]) {
            Err(reason) => assert!(reason.contains("版本"), "应指出是版本问题，实际：{reason}"),
            Ok(method) => panic!("版本不对时应拒绝，实际返回 {method}"),
        }
        match socks5_pick_method(SOCKS5_VERSION, &[0x02]) {
            Err(reason) => {
                assert!(reason.contains("无认证"), "应指出缺少无认证，实际：{reason}")
            }
            Ok(method) => panic!("没有无认证时应拒绝，实际返回 {method}"),
        }
    }

    #[test]
    fn socks5_connect_request_parses_ipv4_and_domain() {
        let ipv4 = socks5_parse_address(SOCKS5_ATYP_IPV4, &[127, 0, 0, 1]).expect("IPv4 应可解析");
        assert_eq!(ipv4, "127.0.0.1");
        let target = socks5_parse_connect(SOCKS5_VERSION, SOCKS5_CMD_CONNECT, &ipv4, 5432)
            .expect("CONNECT 应被接受");
        assert_eq!(target, Socks5Target { host: "127.0.0.1".into(), port: 5432 });

        let domain =
            socks5_parse_address(SOCKS5_ATYP_DOMAIN, b"db.internal").expect("域名应可解析");
        assert_eq!(domain, "db.internal");
        let target = socks5_parse_connect(SOCKS5_VERSION, SOCKS5_CMD_CONNECT, &domain, 3306)
            .expect("域名目标应被接受");
        assert_eq!(target.port, 3306);
    }

    #[test]
    fn socks5_connect_request_rejects_bad_command_and_version() {
        // BIND = 0x02，本轮不支持
        match socks5_parse_connect(SOCKS5_VERSION, 0x02, "127.0.0.1", 80) {
            Err(reason) => assert!(
                reason.contains("CONNECT"),
                "应指出只支持 CONNECT，实际：{reason}"
            ),
            Ok(target) => panic!("BIND 不应被接受，实际：{target:?}"),
        }
        match socks5_parse_connect(0x04, SOCKS5_CMD_CONNECT, "127.0.0.1", 80) {
            Err(reason) => assert!(reason.contains("版本"), "应指出是版本问题，实际：{reason}"),
            Ok(target) => panic!("版本不对时不应被接受，实际：{target:?}"),
        }
        // 地址类型只支持 IPv4 与域名
        match socks5_parse_address(0x04, &[0u8; 16]) {
            Err(reason) => assert!(
                reason.contains("IPv4"),
                "应提示只支持 IPv4 与域名，实际：{reason}"
            ),
            Ok(address) => panic!("IPv6 不应被接受，实际：{address}"),
        }
    }

    #[test]
    fn local_tunnel_forwards_tcp_end_to_end() {
        let store = memory_store();
        let (ssh_port, _probe) = runtime().block_on(spawn_server(ForwardMode::Accept));
        let echo = runtime().block_on(spawn_tcp_echo());

        let spec = TunnelSpec {
            kind: TunnelKind::Local,
            listen_host: "127.0.0.1".into(),
            listen_port: 0,
            target_host: echo.ip().to_string(),
            target_port: echo.port(),
        };
        let handle = TunnelHandle::start(&spec, &store, &test_options(ssh_port))
            .expect("应能启动本地转发");
        assert!(handle.is_running(), "刚启动的隧道应处于运行状态");
        assert_ne!(handle.listen_port(), 0, "应拿到系统分配的本地端口");
        assert!(
            handle
                .describe()
                .contains(&format!("-> {}:{}", echo.ip(), echo.port())),
            "描述应包含目标，实际：{}",
            handle.describe()
        );

        let echoed = exchange(handle.listen_port(), b"PING-LOCAL");
        assert_eq!(echoed, b"PING-LOCAL", "本地转发应能原样回显");

        handle.stop().expect("停止应成功");
        assert!(!handle.is_running(), "停止后不应再算运行中");
    }

    #[test]
    fn local_tunnel_reports_busy_port() {
        // 先占住一个端口，隧道应在握手之前就报「已被占用」
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("应能占住端口");
        let port = occupied.local_addr().expect("应能取到地址").port();
        let store = memory_store();

        let spec = TunnelSpec {
            kind: TunnelKind::Local,
            listen_host: "127.0.0.1".into(),
            listen_port: port,
            target_host: "127.0.0.1".into(),
            target_port: 5432,
        };
        match TunnelHandle::start(&spec, &store, &test_options(1)) {
            Err(CoreError::Tunnel(message)) => assert!(
                message.contains("已被占用"),
                "错误应说明端口被占用，实际：{message}"
            ),
            Ok(_) => panic!("端口被占用时不应启动成功"),
            Err(err) => panic!("错误应归为隧道分类，实际：{err}"),
        }
    }

    #[test]
    fn local_tunnel_releases_port_after_stop() {
        let store = memory_store();
        let (ssh_port, _probe) = runtime().block_on(spawn_server(ForwardMode::Accept));
        let echo = runtime().block_on(spawn_tcp_echo());

        let spec = TunnelSpec {
            kind: TunnelKind::Local,
            listen_host: "127.0.0.1".into(),
            listen_port: 0,
            target_host: echo.ip().to_string(),
            target_port: echo.port(),
        };
        let handle = TunnelHandle::start(&spec, &store, &test_options(ssh_port))
            .expect("应能启动本地转发");
        let port = handle.listen_port();
        handle.stop().expect("停止应成功");
        // stop 会等监听循环退出 -> 端口应立刻可以重新绑定
        assert!(
            std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(),
            "停止后端口 {port} 应已释放"
        );
    }

    #[test]
    fn dynamic_tunnel_relays_socks5_connect() {
        let store = memory_store();
        let (ssh_port, _probe) = runtime().block_on(spawn_server(ForwardMode::Accept));
        let echo = runtime().block_on(spawn_tcp_echo());

        let spec = TunnelSpec {
            kind: TunnelKind::Dynamic,
            listen_host: "127.0.0.1".into(),
            listen_port: 0,
            target_host: String::new(),
            target_port: 0,
        };
        let handle = TunnelHandle::start(&spec, &store, &test_options(ssh_port))
            .expect("应能启动动态转发");
        let port = handle.listen_port();
        assert!(handle.describe().contains("SOCKS5"), "描述应标出 SOCKS5");

        // 手写一遍 SOCKS5 客户端：无认证 + CONNECT 域名目标
        let echoed = runtime().block_on(async move {
            const LIMIT: Duration = Duration::from_secs(10);
            let mut stream = tokio::time::timeout(LIMIT, TcpStream::connect(("127.0.0.1", port)))
                .await
                .expect("连接不应超时")
                .expect("应能连上 SOCKS5 端口");

            stream.write_all(&[SOCKS5_VERSION, 1, SOCKS5_METHOD_NONE]).await.expect("写协商应成功");
            let mut method = [0u8; 2];
            stream.read_exact(&mut method).await.expect("读协商应成功");
            assert_eq!(method, [SOCKS5_VERSION, SOCKS5_METHOD_NONE]);

            let domain = b"127.0.0.1";
            let mut request = vec![SOCKS5_VERSION, SOCKS5_CMD_CONNECT, 0x00, SOCKS5_ATYP_DOMAIN];
            request.push(u8::try_from(domain.len()).expect("域名长度应能放进 1 字节"));
            request.extend_from_slice(domain);
            request.extend_from_slice(&echo.port().to_be_bytes());
            stream.write_all(&request).await.expect("写请求应成功");

            let mut reply = [0u8; 10];
            stream.read_exact(&mut reply).await.expect("读应答应成功");
            assert_eq!(reply[1], SOCKS5_REPLY_OK, "CONNECT 应被接受");

            stream.write_all(b"PING-SOCKS").await.expect("写业务数据应成功");
            let mut buffer = vec![0u8; b"PING-SOCKS".len()];
            stream.read_exact(&mut buffer).await.expect("应读回回显");
            buffer
        });

        assert_eq!(echoed, b"PING-SOCKS", "SOCKS5 转发应能原样回显");
        handle.stop().expect("停止应成功");
    }

    #[test]
    fn remote_tunnel_reports_forward_rejection() {
        let store = memory_store();
        let (ssh_port, _probe) = runtime().block_on(spawn_server(ForwardMode::Refuse));

        let spec = TunnelSpec {
            kind: TunnelKind::Remote,
            listen_host: "127.0.0.1".into(),
            listen_port: 9000,
            target_host: "127.0.0.1".into(),
            target_port: 5432,
        };
        match TunnelHandle::start(&spec, &store, &test_options(ssh_port)) {
            Err(CoreError::Tunnel(message)) => assert!(
                message.contains("远端拒绝转发"),
                "错误应说明远端拒绝了转发，实际：{message}"
            ),
            Ok(handle) => {
                handle.stop().ok();
                panic!("远端拒绝时不应启动成功");
            }
            Err(err) => panic!("错误应归为隧道分类，实际：{err}"),
        }
    }

    #[test]
    fn remote_tunnel_relays_forwarded_channel() {
        let store = memory_store();
        let (ssh_port, probe) = runtime().block_on(spawn_server(ForwardMode::Accept));
        let echo = runtime().block_on(spawn_tcp_echo());

        let spec = TunnelSpec {
            kind: TunnelKind::Remote,
            listen_host: "127.0.0.1".into(),
            listen_port: 0,
            target_host: echo.ip().to_string(),
            target_port: echo.port(),
        };
        let handle = TunnelHandle::start(&spec, &store, &test_options(ssh_port))
            .expect("应能启动远程转发");
        assert_eq!(handle.listen_port(), 9001, "应采用服务端回报的端口");

        // 服务端开通道后立刻发 REMOTE-HELLO -> 隧道转到本地回显服务 -> 回显回到通道
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut received = Vec::new();
        while std::time::Instant::now() < deadline {
            received = probe.received.lock().expect("锁可用").clone();
            if received.windows(12).any(|window| window == b"REMOTE-HELLO") {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            received.windows(12).any(|window| window == b"REMOTE-HELLO"),
            "远程转发应把通道数据经本地回显服务转回来，实际：{:?}",
            String::from_utf8_lossy(&received)
        );

        handle.stop().expect("停止应成功");
    }
}
