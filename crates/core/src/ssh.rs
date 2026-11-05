use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use russh::client;
use russh::client::AuthResult;
use russh::keys::PrivateKeyWithHashAlg;
use russh::{ChannelMsg, Disconnect};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use crate::error::{CoreError, CoreResult};
use crate::model::HostKeyInfo;
use crate::store::Store;
use crate::terminal::{start_output_pump, OutputSink, TerminalControl};

/// 连接超时。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// 一次性命令的等待上限。
///
/// 参数：无
/// 核心流程：远端命令卡住时按这个上限收尾，避免采样线程被永久占住。
const EXEC_TIMEOUT: Duration = Duration::from_secs(20);

/// 探测远端 shell 类型的等待上限。
///
/// 参数：无
/// 核心流程：开终端前先用一条 exec 问一句「你是什么 shell」，
/// 超时就当不支持（不注入），绝不为了这个锦上添花的功能拖慢连接。
const SHELL_PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// 探测远端 shell 的命令。
///
/// 参数：无
/// 核心流程：exec 通道由远端的登录 shell 解释，所以 $SHELL 就是登录 shell，
/// $0 兜底（某些环境不设 SHELL）。只用 POSIX 语法与外部 printf，
/// 保证 fish / csh 之类的 shell 也能照常跑完而不报错。
const SHELL_PROBE_COMMAND: &str = r#"printf '%s\n' "$SHELL" "$0""#;

/// 探测输出的接收上限。
const SHELL_PROBE_MAX_BYTES: usize = 256;

/// 开完 shell 到「它停在提示符上」的总时长上限。
///
/// 参数：无
/// 核心流程：远端还在跑 .bashrc 时写进 PTY 的字节会被内核原样回显，
/// 所以要等它先把话说完；等不到就放弃注入（一个字节都不写），绝不为此拖住连接。
/// 这段等待同时也是「探测 shell 类型」与「收首个 session 的 motd」的窗口 ——
/// 三者并发进行，所以这个上限要盖得住「重 .bashrc」：正常登录本来也要等到提示符才算能用。
/// 取值要盖得住探测（1.5 秒上限）：拿不到输出时最多白等这么久。
const SHELL_READY_TIMEOUT: Duration = Duration::from_millis(4000);

/// 一段输出之后静默多久算「shell 已经把话说完、停在提示符上」。
///
/// 参数：无
/// 核心流程：远端跑启动脚本时是一阵一阵地输出，用这段静默当「已经到提示符」的判据。
/// 首批输出（motd）可能早就在缓冲里了，所以判据是「距最后一段输出静默满这么久」，
/// 而不是「从这一刻起还等不等得到新输出」。
const SHELL_READY_QUIET: Duration = Duration::from_millis(250);

/// 注入窗口的时长上限（每一段窗口各算一次）。
///
/// 参数：无
/// 核心流程：窗口内读到的字节一律不送前端，所以必须有兜底 —— 慢 shell 上
/// 等不到提示符就按这个上限收手，把后续字节交还给正常转发，
/// 绝不让「吞输出」变成永久状态。整段注入最多两段窗口，最坏多花约 4 秒。
const INJECT_WINDOW_TIMEOUT: Duration = Duration::from_millis(2000);

/// 临时关掉远端 PTY 回显的那一行。
///
/// 参数：无
/// 核心流程：这一行写下去会被行编辑器回显，但那段回显落在注入窗口里被丢弃，
/// 所以它不会出现在屏幕上；关回显是为了让后面那句注入命令尽量不被回显
/// （bash 的 readline 会自己接管回显，所以这一步不能当成唯一保障）。
/// `2>/dev/null` 兜住「拿不到 tty 时 stty 报错」的情况。
const ECHO_OFF_LINE: &str = "stty -echo 2>/dev/null";

/// 把回显恢复回去的那一句。
///
/// 参数：无
/// 核心流程：和注入语句写在同一条命令里，保证「装钩子」与「恢复回显」同生共死，
/// 不会出现钩子装上了、回显却没恢复（用户看不见自己打字）的状态。
const ECHO_RESTORE: &str = "stty echo";

/// 连接上开出的一个通道。
///
/// 无参数
/// 核心流程：shell 通道、子系统通道（如 sftp）、exec 通道都是这个类型，
/// 由调用方独占读写，底层共用同一条已认证连接。
pub type SshChannel = russh::Channel<client::Msg>;

/// 刚开好的交互式 shell。
///
/// 无参数
/// 核心流程：把通道和「注入钩子之前已经读到的远端输出」一起交回去 ——
/// 装 OSC 7 钩子前必须先等 shell 开口，那段时间读到的登录 banner、motd 全文
/// （含颜色序列）与提示符必须原样还给上层，一个字节都不能吞；
/// 注入窗口（从关回显起）里读到的回显字节则一律丢掉。
pub struct OpenedShell {
    /// 通道本身，调用方独占读写。
    pub channel: SshChannel,
    /// 注入窗口之前读到的输出（motd + 提示符），按原顺序先灌进输出泵。
    pub pending: Vec<u8>,
}

/// 首连结果槽。
///
/// 无参数
/// 核心流程：后台任务把「连接 + shell 通道」或分类后的错误放进槽里，
/// 同步侧 take 一次取走，结构化错误按值传递不做 clone。
type ShellReady = Arc<Mutex<Option<CoreResult<(SshConnection, OpenedShell)>>>>;

/// 服务端回连时开出来的 forwarded-tcpip 通道。
///
/// 无参数
/// 核心流程：远程转发下，服务端每接到一个外部连接就新开一条这样的通道；
/// 由 ClientHandler 收下并投递到隧道侧的信箱，隧道再连本地目标做双向转发。
pub struct ForwardedChannel {
    /// 已就绪的通道，调用方可直接 into_stream 当字节流用。
    pub channel: SshChannel,
    /// 服务端侧的监听点（即请求转发的地址）。
    pub connected_address: String,
    /// 服务端侧的监听端口。
    pub connected_port: u32,
    /// 真正发起连接的外部客户端地址。
    pub originator_address: String,
    /// 真正发起连接的外部客户端端口。
    pub originator_port: u32,
}

/// 认证方式。
///
/// 无参数
/// 核心流程：只描述"怎么证明身份"，凭据本身由调用方从凭据库或运行时输入取出。
#[derive(Debug, Clone)]
pub enum SshAuth {
    Password(String),
    /// 私钥认证：路径 + 可选的口令。
    PrivateKey {
        path: String,
        passphrase: Option<String>,
    },
}

/// 建立 SSH 会话所需的参数。
///
/// 无参数
/// 核心流程：连接三要素 + 终端尺寸，认证方式单独给，便于后续加私钥；
/// 配了 jump 就先连跳板机，再从跳板机打通到目标（只允许一跳）。
#[derive(Debug, Clone)]
pub struct SshOptions {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SshAuth,
    pub cols: u16,
    pub rows: u16,
    pub term: String,
    /// 用户已确认过指纹 -> 允许把未知主机写进 known_hosts。
    pub accept_unknown_host_key: bool,
    /// 跳板机配置（None 表示直连目标）；跳板机自身不能再配跳板。
    pub jump: Option<Box<SshOptions>>,
}

impl SshOptions {
    /// 校验跳板配置是否合法。
    ///
    /// 参数：无
    /// 核心流程：只允许一跳 -> 跳板机的 jump 必须为空，
    /// 否则报校验错误，避免多级跳板这种未实现的能力被静默忽略。
    fn validate_jump(&self) -> CoreResult<()> {
        if self.jump.as_ref().and_then(|jump| jump.jump.as_ref()).is_some() {
            return Err(CoreError::Validation(
                "只支持一跳跳板：跳板机不能再配置跳板".into(),
            ));
        }
        Ok(())
    }
}

/// 发给后台任务的指令。
///
/// 无参数
/// 核心流程：同步侧的写/改尺寸/关闭都通过通道排队，由后台任务串行作用在 channel 上。
enum SshCommand {
    Write(Vec<u8>),
    Resize(u16, u16),
    Close,
}

/// 一次连接中主机密钥的判定结果。
///
/// 无参数
/// 核心流程：check_server_key 里记录观察到的指纹与判定分支，
/// 连接失败后由外层据此给出"未知主机"或"密钥变更"的准确原因。
#[derive(Default)]
struct HostKeyProbe {
    observed: Option<HostKeyInfo>,
    unknown: bool,
    mismatch: bool,
}

/// 客户端回调。
///
/// 无参数
/// 核心流程：用 known_hosts 决定是否接受服务端密钥：
/// 指纹一致放行、指纹变化硬阻断、未知主机则阻断并等待用户确认；
/// 另外持有可选的 forwarded-tcpip 信箱，供远程转发接收服务端回连。
struct ClientHandler {
    store: Store,
    host: String,
    port: u16,
    accept_unknown: bool,
    probe: Arc<Mutex<HostKeyProbe>>,
    /// 远程转发专用信箱；None 表示这条连接不接收 forwarded-tcpip 通道。
    forward: Option<mpsc::UnboundedSender<ForwardedChannel>>,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    /// 校验服务端主机密钥。
    ///
    /// 参数：key 为服务端公钥或证书
    /// 核心流程：算出 SHA256/MD5 指纹 → 与库中记录比对 →
    /// 一致放行；变化阻断；未知时按用户是否已确认决定放行并落库或阻断。
    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let public = key.public_key();
        let key_type = public.algorithm().as_str().to_string();
        let sha256 = public.fingerprint(russh::keys::HashAlg::Sha256).to_string();
        let info = HostKeyInfo {
            host: self.host.clone(),
            port: self.port,
            key_type: key_type.clone(),
            sha256: sha256.clone(),
            md5: md5_fingerprint(&public),
        };
        if let Ok(mut probe) = self.probe.lock() {
            probe.observed = Some(info);
        }

        match self
            .store
            .known_host_get(&self.host, self.port, &key_type)
            .await
        {
            // 与记录一致 -> 放行
            Ok(Some(saved)) if saved == sha256 => Ok(true),
            // 有记录但不一致 -> 硬阻断，必须人工处理
            Ok(Some(_)) => {
                if let Ok(mut probe) = self.probe.lock() {
                    probe.mismatch = true;
                }
                Ok(false)
            }
            // 未知主机 -> 用户确认过则落库放行，否则阻断
            Ok(None) => {
                if self.accept_unknown {
                    let _ = self
                        .store
                        .known_host_upsert(&self.host, self.port, &key_type, &sha256)
                        .await;
                    return Ok(true);
                }
                if let Ok(mut probe) = self.probe.lock() {
                    probe.unknown = true;
                }
                Ok(false)
            }
            Err(_) => Ok(false),
        }
    }

    /// 接收服务端回连的 forwarded-tcpip 通道。
    ///
    /// 参数：channel 为服务端新开的通道，connected_* 为远端监听点，
    /// originator_* 为真正发起连接的外部客户端，reply 为接受/拒绝句柄
    /// 核心流程：只有远程转发（构造连接时传了信箱）才接受这类通道 ->
    /// 先 accept 再连同元信息投递；没人接收就丢弃 reply，
    /// 由 russh 自动回一个拒绝，避免为无人消费的通道占住整条连接。
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: SshChannel,
        connected_address: &str,
        connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let Some(forward) = self.forward.as_ref() else {
            return Ok(());
        };
        reply.accept().await;
        let _ = forward.send(ForwardedChannel {
            channel,
            connected_address: connected_address.to_string(),
            connected_port,
            originator_address: originator_address.to_string(),
            originator_port,
        });
        Ok(())
    }
}

/// 计算 MD5 指纹。
///
/// 参数：key 为服务端公钥
/// 核心流程：对 SSH 线格式公钥做 MD5，按 `aa:bb:...` 输出，
/// 方便和 OpenSSH 的 `ssh-keygen -l -E md5` 结果对照。
fn md5_fingerprint(key: &russh::keys::PublicKey) -> String {
    match key.to_bytes() {
        Ok(bytes) => {
            let digest = md5::compute(&bytes);
            let parts: Vec<String> = digest.iter().map(|byte| format!("{byte:02x}")).collect();
            format!("MD5:{}", parts.join(":"))
        }
        Err(_) => "MD5:不可用".to_string(),
    }
}

/// 给跳板机这一跳的失败补上归属。
///
/// 参数：err 为连接跳板机阶段的错误，jump 为该跳板机的连接参数
/// 核心流程：主机密钥类错误本身已带跳板机地址，原样返回以保住结构化信息
/// （前端据此弹确认框，且地址指向跳板机而不是目标）；
/// 其余错误补一句「跳板机 host:port」，让人一眼看出卡在哪一层。
fn jumper_error(err: CoreError, jump: &SshOptions) -> CoreError {
    match err {
        CoreError::HostKeyUnknown(info) => CoreError::HostKeyUnknown(info),
        CoreError::HostKeyMismatch(info) => CoreError::HostKeyMismatch(info),
        other => CoreError::Ssh(format!(
            "跳板机 {}:{} 连接失败：{other}",
            jump.host, jump.port
        )),
    }
}

/// 同步侧等待握手的时间上限。
///
/// 参数：options 为连接参数
/// 核心流程：直连只有一次握手；经跳板要「先跳板、再目标」两次，
/// 上限相应放宽一倍，避免单跳就把同步侧等超时。
fn sync_wait(options: &SshOptions) -> Duration {
    if options.jump.is_some() {
        CONNECT_TIMEOUT * 2
    } else {
        CONNECT_TIMEOUT
    }
}

/// 一条已认证的 SSH 连接。
///
/// 无参数
/// 核心流程：内部用 Arc 共享 russh 的 Handle，克隆出的副本指向同一条连接，
/// 因此 shell、SFTP、隧道都能各自在它上面开自己的通道而互不影响；
/// 经跳板建立时还持有跳板连接，避免跳板被提前释放把目标连接带走。
#[derive(Clone)]
pub struct SshConnection {
    handle: Arc<client::Handle<ClientHandler>>,
    /// 目标主机地址，建连时记下，供界面标注数据来源。
    host: String,
    /// 建立本连接所依赖的跳板机连接（None 表示直连）。
    ///
    /// 只靠它延长跳板连接的生命周期，没有读取点，故显式放行 dead_code。
    #[allow(dead_code)]
    jumper: Option<Arc<SshConnection>>,
}

impl SshConnection {
    /// 建立连接并完成认证（异步）。
    ///
    /// 参数：store 为已知主机库，options 为连接参数
    /// 核心流程：TCP + 握手（主机指纹三态校验）→ 密码或私钥认证；
    /// options 里配了 jump 时自动走跳板（先连跳板，再在跳板通道上握手）；
    /// 本层只负责连接本身，不开任何通道，失败原因在这里就分类完成。
    pub async fn connect_async(store: &Store, options: &SshOptions) -> CoreResult<Self> {
        Self::connect_async_inner(store, options, None).await
    }

    /// 建立连接并带上 forwarded-tcpip 信箱（异步）。
    ///
    /// 参数：store 为已知主机库，options 为连接参数，
    /// forward 为服务端回连通道的投递地址
    /// 核心流程：与普通连接完全一致，只是把信箱装进 ClientHandler，
    /// 这样远程转发才能收到服务端替它接受的连接（握手前必须装好，之后补不上）。
    pub async fn connect_async_with_forward(
        store: &Store,
        options: &SshOptions,
        forward: mpsc::UnboundedSender<ForwardedChannel>,
    ) -> CoreResult<Self> {
        Self::connect_async_inner(store, options, Some(forward)).await
    }

    /// 建立连接的实际实现。
    ///
    /// 参数：store 为已知主机库，options 为连接参数，
    /// forward 为可选的 forwarded-tcpip 信箱
    /// 核心流程：没配跳板就在纯 TCP 上握手认证；配了跳板则先连跳板
    /// （跳板机自己的指纹也校验一遍），再在跳板开出的通道流上完成目标握手；
    /// 握手失败时用探测结果区分「未知主机」与「指纹变更」。
    async fn connect_async_inner(
        store: &Store,
        options: &SshOptions,
        forward: Option<mpsc::UnboundedSender<ForwardedChannel>>,
    ) -> CoreResult<Self> {
        options.validate_jump()?;
        let Some(jump) = options.jump.as_deref() else {
            return Self::connect_tcp(store, options, forward).await;
        };

        // 跳板机是独立的一跳 -> 它自己的指纹要按跳板机地址单独记账与报错
        let jumper = Self::connect_tcp(store, jump, None)
            .await
            .map_err(|err| jumper_error(err, jump))?;
        Self::open_via(&jumper, store, options, forward).await
    }

    /// 在纯 TCP 上建立连接并完成握手认证。
    ///
    /// 参数：store 为已知主机库，options 为连接参数，
    /// forward 为可选的 forwarded-tcpip 信箱
    /// 核心流程：先连 TCP（超时与端口不可达各自成文案）→
    /// 再交给泛型握手完成指纹三态校验与认证。
    async fn connect_tcp(
        store: &Store,
        options: &SshOptions,
        forward: Option<mpsc::UnboundedSender<ForwardedChannel>>,
    ) -> CoreResult<Self> {
        let stream = match tokio::time::timeout(
            CONNECT_TIMEOUT,
            TcpStream::connect((options.host.as_str(), options.port)),
        )
        .await
        {
            Err(_) => return Err(CoreError::Ssh("连接超时：无法到达主机".into())),
            Ok(Err(err)) => return Err(CoreError::Ssh(format!("连接失败：{err}"))),
            Ok(Ok(stream)) => stream,
        };

        let session = Self::handshake(store, options, stream, forward).await?;
        Ok(Self {
            handle: Arc::new(session),
            host: options.host.clone(),
            jumper: None,
        })
    }

    /// 在跳板连接上开一条直达目标的通道，并在通道流上完成目标握手。
    ///
    /// 参数：jumper 为已认证的跳板连接，store 为已知主机库，
    /// options 为目标的连接参数（其中 jump 必须为空），
    /// forward 为可选的 forwarded-tcpip 信箱
    /// 核心流程：direct-tcpip 到 options.host:options.port →
    /// 把通道当字节流做目标握手与认证（指纹按目标 host:port 记账）→
    /// 把跳板连接挂进返回值，保证目标连接的存活期不超过跳板。
    async fn open_via(
        jumper: &SshConnection,
        store: &Store,
        options: &SshOptions,
        forward: Option<mpsc::UnboundedSender<ForwardedChannel>>,
    ) -> CoreResult<Self> {
        let channel = jumper
            .open_direct_tcpip(&options.host, options.port, "127.0.0.1", 0)
            .await?;
        let session = Self::handshake(store, options, channel.into_stream(), forward).await?;
        Ok(Self {
            handle: Arc::new(session),
            host: options.host.clone(),
            jumper: Some(Arc::new(jumper.clone())),
        })
    }

    /// 经跳板连接建立到目标的连接（异步）。
    ///
    /// 参数：jumper 为已认证的跳板连接，store 为已知主机库，
    /// options 为目标的连接参数
    /// 核心流程：拒绝「已经给了跳板还要再配跳板」的组合 → 在跳板开 direct-tcpip 通道 →
    /// 在通道流上完成目标握手与认证；失败的跳板通道随流释放自动关闭。
    pub async fn connect_via_async(
        jumper: &SshConnection,
        store: &Store,
        options: &SshOptions,
    ) -> CoreResult<Self> {
        if options.jump.is_some() {
            return Err(CoreError::Validation(
                "只支持一跳跳板：跳板机不能再配置跳板".into(),
            ));
        }
        Self::open_via(jumper, store, options, None).await
    }

    /// 经跳板连接建立到目标的连接（同步）。
    ///
    /// 参数：jumper 为已认证的跳板连接，store 为已知主机库，
    /// options 为目标的连接参数
    /// 核心流程：把异步流程派发到常驻运行时，同步侧等结果；
    /// 结构化指纹错误按值传回，不做 clone。
    pub fn connect_via(
        jumper: &SshConnection,
        store: &Store,
        options: &SshOptions,
    ) -> CoreResult<Self> {
        let (tx, rx) = std::sync::mpsc::channel();
        let store = store.clone();
        let options = options.clone();
        let jumper = jumper.clone();
        runtime().spawn(async move {
            let _ = tx.send(Self::connect_via_async(&jumper, &store, &options).await);
        });
        match rx.recv_timeout(CONNECT_TIMEOUT) {
            Ok(result) => result,
            Err(_) => Err(CoreError::Ssh("连接超时".into())),
        }
    }

    /// 在任意双向字节流上完成握手与认证。
    ///
    /// 参数：store 为已知主机库，options 为连接参数，
    /// stream 为已建立的字节流（纯 TCP，或跳板连接上开出的 direct-tcpip 通道流），
    /// forward 为可选的 forwarded-tcpip 信箱
    /// 核心流程：装好 known_hosts 判定回调 → 在流上跑 SSH 握手（主机指纹三态校验）→
    /// 密码或私钥认证；主机密钥被拒时用探测结果区分「未知主机」与「指纹变更」。
    async fn handshake<S>(
        store: &Store,
        options: &SshOptions,
        stream: S,
        forward: Option<mpsc::UnboundedSender<ForwardedChannel>>,
    ) -> CoreResult<client::Handle<ClientHandler>>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let config = Arc::new(client::Config::default());

        // 主机密钥探测 -> 把 check_server_key 的判定结果带回外层用于报错
        let probe = Arc::new(Mutex::new(HostKeyProbe::default()));
        let handler = ClientHandler {
            store: store.clone(),
            host: options.host.clone(),
            port: options.port,
            accept_unknown: options.accept_unknown_host_key,
            probe: Arc::clone(&probe),
            forward,
        };

        let mut session = match tokio::time::timeout(
            CONNECT_TIMEOUT,
            client::connect_stream(config, stream, handler),
        )
        .await
        {
            Err(_) => return Err(CoreError::Ssh("连接超时：无法到达主机".into())),
            Ok(Err(err)) => {
                // 主机密钥被拒时，用探测到的信息给出准确原因
                if let Ok(state) = probe.lock() {
                    if state.mismatch {
                        if let Some(info) = state.observed.clone() {
                            return Err(CoreError::HostKeyMismatch(info));
                        }
                    }
                    if state.unknown {
                        if let Some(info) = state.observed.clone() {
                            return Err(CoreError::HostKeyUnknown(info));
                        }
                    }
                }
                return Err(CoreError::Ssh(format!("连接失败：{err}")));
            }
            Ok(Ok(session)) => session,
        };

        // 认证 -> 失败要能区分"密码错"、私钥不被接受、私钥读取失败
        let (authenticated, failure_hint) = match &options.auth {
            SshAuth::Password(password) => (
                session
                    .authenticate_password(&options.username, password)
                    .await
                    .map_err(|err| CoreError::Ssh(format!("认证异常：{err}")))?,
                "认证失败：用户名或密码错误",
            ),
            SshAuth::PrivateKey { path, passphrase } => {
                let key = russh::keys::load_secret_key(path, passphrase.as_deref())
                    .map_err(|err| CoreError::Ssh(format!("读取私钥失败（{path}）：{err}")))?;
                let key = PrivateKeyWithHashAlg::new(Arc::new(key), None);
                (
                    session
                        .authenticate_publickey(&options.username, key)
                        .await
                        .map_err(|err| CoreError::Ssh(format!("认证异常：{err}")))?,
                    "认证失败：私钥不被该主机接受",
                )
            }
        };
        if !matches!(authenticated, AuthResult::Success) {
            return Err(CoreError::Ssh(failure_hint.to_string()));
        }

        Ok(session)
    }

    /// 建立连接并完成认证（同步）。
    ///
    /// 参数：store 为已知主机库，options 为连接参数
    /// 核心流程：把异步流程派发到常驻运行时，同步侧用一次性通道等结果；
    /// 结构化错误按值传递（不 clone），未知主机与指纹变更的 detail 不会丢。
    pub fn connect(store: &Store, options: &SshOptions) -> CoreResult<Self> {
        let (tx, rx) = std::sync::mpsc::channel();
        let store = store.clone();
        let options = options.clone();
        // 经跳板要连两次 -> 同步等待上限按跳数放宽
        let wait = sync_wait(&options);
        runtime().spawn(async move {
            let _ = tx.send(Self::connect_async(&store, &options).await);
        });
        match rx.recv_timeout(wait) {
            Ok(result) => result,
            Err(_) => Err(CoreError::Ssh("连接超时".into())),
        }
    }

    /// 在连接上开一个交互式 shell 通道。
    ///
    /// 参数：term 为终端类型，cols/rows 为初始尺寸
    /// 核心流程：**先开交互 shell 通道**（`channel_open_session` → PTY → shell）→
    /// **再**用第二个 session 探远端登录 shell → 给 bash/zsh 装上 OSC 7 提示符钩子。
    /// 顺序不能反：PAM 的 motd 只发给本连接上的**第一个** session，
    /// 若让探测用的 exec 通道抢先，motd 会被它吃掉并丢弃，交互 shell（第二个 session）
    /// 永远拿不回那段文字。探测期间 shell 通道上的字节（motd 全文 + 提示符）
    /// 全部缓冲在 pending 里、一个都不丢，注入窗口里的回显则照旧吞掉；
    /// 探测失败或 shell 不支持时一个字节都不写，motd 与 shell 照常可用。
    /// 前三步各有独立的失败文案，成功后通道由调用方独占。
    pub async fn open_shell(&self, term: &str, cols: u16, rows: u16) -> CoreResult<OpenedShell> {
        // 1) 先把交互 shell 通道占住「首个 session」的位置：motd 归它
        let mut channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|err| CoreError::Ssh(format!("打开会话通道失败：{err}")))?;

        channel
            .request_pty(false, term, u32::from(cols), u32::from(rows), 0, 0, &[])
            .await
            .map_err(|err| CoreError::Ssh(format!("申请 PTY 失败：{err}")))?;

        channel
            .request_shell(false)
            .await
            .map_err(|err| CoreError::Ssh(format!("启动 shell 失败：{err}")))?;

        // 2) 探测（第二个 session，此时 motd 已经发完）与 shell 通道的读取并发进行：
        //    shell 一开就会吐 motd 然后停在提示符上，这段字节必须边收边攒、绝不丢弃
        let mut pending = Vec::new();
        let (shell, ready) = buffer_shell_while_probing(
            &mut channel,
            &mut pending,
            detect_remote_shell(&self.handle),
        )
        .await;

        // 3) 探测结果决定要不要注入；窗口外的字节（motd 全文）已全部在 pending 里，
        //    窗口内的回显由后端吞掉；探测失败/不支持 -> 静默跳过，终端本身照常可用
        install_cwd_hook(&mut channel, shell, ready).await;

        Ok(OpenedShell { channel, pending })
    }

    /// 在连接上开一个子系统通道。
    ///
    /// 参数：name 为子系统名（SFTP 用 "sftp"）
    /// 核心流程：开 session channel → 请求子系统；
    /// 本轮只交付通道本身，协议报文由调用方在该通道上读写。
    pub async fn open_subsystem(&self, name: &str) -> CoreResult<SshChannel> {
        let channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|err| CoreError::Ssh(format!("打开会话通道失败：{err}")))?;

        channel
            .request_subsystem(true, name)
            .await
            .map_err(|err| CoreError::Ssh(format!("请求子系统 {name} 失败：{err}")))?;

        Ok(channel)
    }

    /// 开一条直达目标的转发通道。
    ///
    /// 参数：host/port 为目标地址（由远端代连），
    /// originator_* 为本地来访者地址（仅用于记录，可传 "127.0.0.1"/0）
    /// 核心流程：发 channel-open(direct-tcpip) → 等远端确认；
    /// 确认后拿到的通道即 byte 流，可直接和本地 TCP 做双向转发。
    pub async fn open_direct_tcpip(
        &self,
        host: &str,
        port: u16,
        originator_address: &str,
        originator_port: u16,
    ) -> CoreResult<SshChannel> {
        self.handle
            .channel_open_direct_tcpip(
                host,
                u32::from(port),
                originator_address,
                u32::from(originator_port),
            )
            .await
            .map_err(|err| {
                CoreError::Ssh(format!(
                    "打开到 {host}:{port} 的转发通道失败（目标可能不可达或被策略拒绝）：{err}"
                ))
            })
    }

    /// 在连接上一次性执行命令并收回输出。
    ///
    /// 参数：command 为要执行的命令（交给远端 shell 解释），
    /// max_bytes 为 stdout 与 stderr 合计的接收上限
    /// 核心流程：把异步执行派发到常驻运行时并用一次性通道等结果 →
    /// 开 exec 通道、边收边攒输出，到上限立刻停止接收 → 退出码非 0 时
    /// 把 stderr 一并放进中文错误返回，成功则只回 stdout 文本。
    pub fn exec(&self, command: &str, max_bytes: usize) -> CoreResult<String> {
        let handle = Arc::clone(&self.handle);
        let command = command.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        runtime().spawn(async move {
            let _ = tx.send(exec_on(handle, &command, max_bytes).await);
        });
        match rx.recv_timeout(EXEC_TIMEOUT) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(CoreError::Ssh(format!(
                "远端命令超时（{} 秒无响应）",
                EXEC_TIMEOUT.as_secs()
            ))),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(CoreError::Ssh("远端命令执行失败：连接已断开".into()))
            }
        }
    }

    /// 请求远端监听一个端口（远程转发）。
    ///
    /// 参数：address 为远端监听地址，port 为远端监听端口（0 表示让远端分配）
    /// 核心流程：发 tcpip-forward 请求 → 返回远端实际监听的端口；
    /// 远端拒绝转发时给出明确的「远端拒绝转发」文案。
    pub async fn tcpip_forward(&self, address: &str, port: u16) -> CoreResult<u32> {
        self.handle
            .tcpip_forward(address, u32::from(port))
            .await
            .map_err(|err| CoreError::Ssh(err.to_string()))
    }

    /// 取消远端监听（远程转发收尾）。
    ///
    /// 参数：address 为远端监听地址，port 为远端监听端口
    /// 核心流程：发 cancel-tcpip-forward；连接已断时也返回成功（幂等）。
    pub async fn cancel_tcpip_forward(&self, address: &str, port: u16) -> CoreResult<()> {
        let _ = self.handle.cancel_tcpip_forward(address, u32::from(port)).await;
        Ok(())
    }

    /// 断开整条连接。
    ///
    /// 参数：无
    /// 核心流程：发送应用层断开消息；连接已断时也返回成功（幂等）。
    pub async fn disconnect(&self) -> CoreResult<()> {        let _ = self
            .handle
            .disconnect(Disconnect::ByApplication, "", "en")
            .await;
        Ok(())
    }

    /// 连接是否已关闭。
    ///
    /// 参数：无
    /// 核心流程：直接读 russh Handle 的底层通道状态，供上层判断能否继续复用。
    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    /// 取目标主机地址。
    ///
    /// 参数：无
    /// 核心流程：返回建连时记录的目标地址，供指标等附属功能标注数据来源。
    pub fn host(&self) -> &str {
        &self.host
    }
}

/// 在已认证连接上一次性执行命令（异步）。
///
/// 参数：handle 为连接句柄，command 为命令文本，max_bytes 为接收上限
/// 核心流程：开 session 通道 → 请求 exec → 循环收 stdout(stderr)，
/// 攒够上限就停并关通道 → 非 0 退出码转成带 stderr 的中文错误。
async fn exec_on(
    handle: Arc<client::Handle<ClientHandler>>,
    command: &str,
    max_bytes: usize,
) -> CoreResult<String> {
    let mut channel = handle
        .channel_open_session()
        .await
        .map_err(|err| CoreError::Ssh(format!("打开执行通道失败：{err}")))?;

    channel
        .exec(true, command.to_string())
        .await
        .map_err(|err| CoreError::Ssh(format!("远端拒绝执行命令：{err}")))?;

    let limit = max_bytes.max(1);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code: Option<u32> = None;

    loop {
        let Some(message) = channel.wait().await else {
            break;
        };
        match message {
            ChannelMsg::Data { data } => append_capped(&mut stdout, &data, limit),
            ChannelMsg::ExtendedData { data, ext } if ext == 1 => {
                append_capped(&mut stderr, &data, limit)
            }
            ChannelMsg::ExitStatus { exit_status } => exit_code = Some(exit_status),
            ChannelMsg::Eof | ChannelMsg::Close => break,
            _ => {}
        }
        // 到上限就停 -> 远端再吐多少都不收，避免一条命令撑爆内存
        if stdout.len() + stderr.len() >= limit {
            break;
        }
    }

    let _ = channel.close().await;

    if let Some(code) = exit_code.filter(|code| *code != 0) {
        let detail = String::from_utf8_lossy(&stderr).trim().to_string();
        let detail = if detail.is_empty() {
            "远端未输出错误信息".to_string()
        } else {
            detail
        };
        return Err(CoreError::Ssh(format!("远端命令退出码 {code}：{detail}")));
    }

    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

/// 把一段数据追加到缓冲，超出上限的部分丢弃。
///
/// 参数：target 为目标缓冲，data 为本轮收到的字节，limit 为总上限
/// 核心流程：算出剩余额度，只追加额度内的那一段，已满则直接忽略。
fn append_capped(target: &mut Vec<u8>, data: &[u8], limit: usize) {
    let room = limit.saturating_sub(target.len());
    if room == 0 {
        return;
    }
    target.extend_from_slice(&data[..data.len().min(room)]);
}

/// 远端 shell 的提示符钩子能力。
///
/// 无参数
/// 核心流程：只区分「能装钩子的 bash / zsh」与「其余一律不碰」，
/// 少一个分支就少一类把用户 shell 弄坏的可能。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteShell {
    /// bash：钩子是 PROMPT_COMMAND。
    Bash,
    /// zsh：钩子是 precmd_functions。
    Zsh,
    /// sh/dash/fish/csh 等：不注入，静默跳过。
    Unsupported,
}

impl RemoteShell {
    /// 从探测输出里认出 shell。
    ///
    /// 参数：output 为探测命令的 stdout（多行，形如 "/bin/bash\nbash"）
    /// 核心流程：按空白切开逐个词，去掉目录前缀与登录 shell 的 "-" 前缀后
    /// 比名字；认不出来就回 Unsupported，绝不靠猜。
    fn classify(output: &str) -> Self {
        for token in output.split_whitespace() {
            let name = token.rsplit('/').next().unwrap_or(token).trim_start_matches('-');
            match name {
                "bash" => return Self::Bash,
                "zsh" => return Self::Zsh,
                _ => {}
            }
        }
        Self::Unsupported
    }
}

/// 生成远端 shell 的 OSC 7 注入脚本。
///
/// 参数：shell 为探测到的 shell 类型
/// 核心流程：按 shell 类型拼「装钩子」的语句 —— 一律追加而不是覆盖，
/// 用户原有的 PROMPT_COMMAND / precmd 钩子原样保留；
/// 不支持的 shell 返回 None，调用方据此完全不发任何字节。
fn osc7_setup_script(shell: RemoteShell) -> Option<String> {
    let install = match shell {
        // printf 里 \033\\ 就是 OSC 的终止符 ST，必须原样送到远端 shell
        RemoteShell::Bash => concat!(
            r#"__rmx_osc7(){ printf '\033]7;file://%s%s\033\\' "${HOSTNAME:-localhost}" "$PWD"; }; "#,
            r#"PROMPT_COMMAND="__rmx_osc7${PROMPT_COMMAND:+;$PROMPT_COMMAND}""#,
        ),
        RemoteShell::Zsh => concat!(
            r#"__rmx_osc7(){ printf '\033]7;file://%s%s\033\\' "${HOST:-localhost}" "$PWD"; }; "#,
            // 用数组过滤追加 -> 保留用户已有 precmd 钩子，重复注入也不会堆叠
            r#"precmd_functions=(__rmx_osc7 ${precmd_functions:#__rmx_osc7})"#,
        ),
        RemoteShell::Unsupported => return None,
    };
    Some(install.to_string())
}

/// 生成一次注入要写进 PTY 的整行内容。
///
/// 参数：shell 为探测到的 shell 类型
/// 核心流程：装钩子 → 立刻恢复回显。两件事写在同一行里，shell 会按顺序执行：
/// 即使中间被打断，也不会出现「钩子装上了但回显还关着」的状态。
/// 这一行不含任何擦行/上移的 ANSI 序列 —— 屏幕上的痕迹靠“注入窗口丢字节”消除，
/// 不靠猜行数去擦（猜多了会把 motd 一起吃掉）。
fn cwd_hook_payload(shell: RemoteShell) -> Option<String> {
    let script = osc7_setup_script(shell)?;
    Some(format!("{script}; {ECHO_RESTORE}"))
}

/// 给远端 shell 装上 OSC 7 提示符钩子。
///
/// 参数：channel 为刚开好的 shell 通道，shell 为探测到的 shell 类型，
/// ready 为「shell 已经开口并静默下来（停在提示符上）」的证据（由
/// `buffer_shell_while_probing` 顺带判出来）
/// 核心流程：写进 PTY 的命令一定会被行编辑器回显（bash 的 readline 自己接管回显，
/// `stty -echo` 都拦不住），把回显再从屏幕上擦掉是不可能的 —— 这正是一次次擦不干净的原因。
/// 所以换个方向：**注入窗口内读到的字节一律不送前端**，让后端自己把回显吃掉。
/// 1. 不认识的 shell、或拿不到「已经停在提示符上」的证据 -> 一个字节都不写；
/// 2. 从关回显那一行开始进入注入窗口；
/// 3. 窗口里读到「除了自己那行回显之外的提示符」才算回显真的关了 -> 窗口关闭；
/// 4. 发「装钩子 + 恢复回显」，它的回显同样落在第二段窗口里被丢掉。
///
/// 窗口只按「看到提示符」关，不按固定字节数/固定时长猜；另有超时兜底，
/// 超时就放弃剩余丢弃、恢复正常转发（慢 shell 上不会永久吞输出）。
/// 任何一步拿不到证据就收手，并把回显恢复回来：终端可用性优先于目录跟随。
/// 关回显只能借远端 shell 的 `stty`：这条 PTY 的 master 在 sshd 那台机器上，
/// 本地既没有 fd 也没有 tcsetattr 的对象，pty-req 里的 mode 又是「服务端可以不理」的，
/// 靠不住，所以用 shell 自己能执行的一条命令来关、再用同样一条命令恢复。
/// 整段注入都在 `open_shell` 返回之前完成 -> 前端还没拿到通道句柄，写不进用户输入，
/// 所以这个窗口吞不到任何用户自己的字节。
async fn install_cwd_hook(channel: &mut SshChannel, shell: RemoteShell, ready: bool) {
    let Some(payload) = cwd_hook_payload(shell) else {
        return; // 不认识的 shell：一个字节都不发
    };

    // 1) 没等到 shell 停在提示符上（没开口 / 一直在输出）-> 一个字节都不写：
    //    此时写进去会被回显，而窗口的判据还没建立
    if !ready {
        return;
    }

    // 2) 关回显；从这一句写下去起进入注入窗口：回显字节一律丢弃
    if !write_line(channel, ECHO_OFF_LINE).await {
        return;
    }

    // 3) 窗口关闭条件 = 看到提示符（自己那行回显不算证据）
    if !swallow_until_prompt(channel, ECHO_OFF_LINE.as_bytes(), INJECT_WINDOW_TIMEOUT).await {
        // 没拿到证据：钩子不装，只把回显打开（绝不把用户留在看不见自己打字的状态），
        // 窗口已按超时结束 -> 后续字节照常转发，静默降级
        let _ = write_line(channel, ECHO_RESTORE).await;
        return;
    }

    // 4) 注入 + 恢复回显，一次写完 -> 中间插不进任何用户输入；
    //    这一行自己的回显也落在第二段窗口里被丢掉，屏幕上不留任何痕迹
    if !write_line(channel, &payload).await {
        return;
    }
    let _ = swallow_until_prompt(channel, payload.as_bytes(), INJECT_WINDOW_TIMEOUT).await;
}

/// 注入窗口：在窗口内读到远端输出，但一个字节都不交给前端。
///
/// 参数：channel 为 shell 通道，own_echo 为我们刚写进去那行命令的字节，
/// timeout 为这段窗口的时长上限
/// 核心流程：边读边攒一块「窗口内看到的字节」，攒到的内容一旦能证明
/// 「这行命令真的执行完了」（抠掉自己那行回显后还剩提示符这类实质内容）就关窗；
/// 攒到的东西一律不 push —— 这正是把注入回显从屏幕上抹掉的办法。
/// 超时/通道结束也照样关窗返回 false，调用方据此决定是降级还是继续装钩子。
async fn swallow_until_prompt(
    channel: &mut SshChannel,
    own_echo: &[u8],
    timeout: Duration,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    let mut seen = Vec::new();
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            return false;
        }
        let remaining = deadline - now;
        match tokio::time::timeout(remaining, channel.wait()).await {
            // 窗口内的数据：只用于判断「执行完了没」，绝不外送
            Ok(Some(ChannelMsg::Data { data })) => {
                seen.extend_from_slice(&data);
                if has_prompt_after_own_echo(&seen, own_echo) {
                    return true;
                }
            }
            Ok(Some(ChannelMsg::ExtendedData { data, .. })) => {
                seen.extend_from_slice(&data);
                if has_prompt_after_own_echo(&seen, own_echo) {
                    return true;
                }
            }
            // 通道自身的其他消息（窗口调整、退出码…）不是输出，继续等
            Ok(Some(_)) => continue,
            // 通道结束或到点：关窗，剩余字节交回正常转发
            Ok(None) | Err(_) => return false,
        }
    }
}

/// 判断窗口内的输出里，除了我们自己那行命令的回显之外还有没有别的内容。
///
/// 参数：seen 为窗口里读到的全部字节，own_echo 为我们刚写进去那行命令的字节
/// 核心流程：行编辑器会先回显我们刚敲的那一行，这段字节不能当成「shell 已经执行」的
/// 证据；把它抠掉之后还有实质内容（提示符等），才说明那行命令真的跑完了。
/// 回显可能分片到达，所以「尾巴只是回显的开头」时不算证据，要再等更多字节。
fn has_prompt_after_own_echo(seen: &[u8], own_echo: &[u8]) -> bool {
    let mut index = 0;
    while index < seen.len() {
        let rest = &seen[index..];
        if rest.starts_with(own_echo) {
            index += own_echo.len();
            continue;
        }
        // 还没收全的回显不算证据（否则会把「回显分片」误判成提示符）
        if own_echo.starts_with(rest) {
            return false;
        }
        if !matches!(seen[index], b'\r' | b'\n' | b'\t' | b' ') {
            return true;
        }
        index += 1;
    }
    false
}

/// 边探测 shell 类型、边把 shell 通道上的输出缓冲下来。
///
/// 参数：channel 为刚请求完 shell 的交互通道，pending 为缓冲（motd 等首批输出），
/// probe 为探测远端 shell 类型的 future
/// 核心流程：探测走**另一条** session 通道，而这条交互通道上的字节一个都不能丢 ——
/// motd 是 PAM 在本连接首个 session 上一次性打出来的，错过就再也拿不回来；
/// 所以这里让两件事并发：探测在等 RTT 的同时，把通道上到达的字节全部攒进 pending。
/// 「可以注入了」的判据也相应调整 —— 不再是「等它开口」，
/// 而是「已经开过口、且距最后一段输出已经静默满 quiet」，
/// 因为首批输出很可能在探测回来之前就已经躺在缓冲里了。
/// 整段等待以 timeout 为上限；通道提前结束或到点都返回「没就绪」，调用方据此不写任何字节。
/// 读到的字节一律追加进 pending，绝不丢弃（motd 必须完整还给用户）。
/// 返回（探测到的 shell 类型，是否真的静默到提示符）。
async fn buffer_shell_while_probing<F>(
    channel: &mut SshChannel,
    pending: &mut Vec<u8>,
    probe: F,
) -> (RemoteShell, bool)
where
    F: std::future::Future<Output = RemoteShell>,
{
    let mut probe = std::pin::pin!(probe);
    let mut probed: Option<RemoteShell> = None;
    let mut last_output: Option<std::time::Instant> = None;
    let deadline = std::time::Instant::now() + SHELL_READY_TIMEOUT;
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            return (probed.unwrap_or(RemoteShell::Unsupported), false);
        }
        // 开过口 + 静默满 quiet + 探测有结果 -> shell 已经停在提示符上，可以注入了
        if let (Some(last), Some(shell)) = (last_output, probed)
            && now.duration_since(last) >= SHELL_READY_QUIET
        {
            return (shell, true);
        }
        // 这一轮最多等多久：说过话就只看静默，没说过话就一直等到整体超时
        // （慢启动的 .bashrc 不能当成「不说话」）
        let slice = match last_output {
            Some(last) => std::cmp::min(
                SHELL_READY_QUIET.saturating_sub(now.duration_since(last)),
                deadline - now,
            ),
            None => deadline - now,
        };
        let slice = slice.max(Duration::from_millis(1));
        tokio::select! {
            // 探测结果一到就记下来：它自带 1.5 秒上限，不会把这里拖住
            shell = &mut probe, if probed.is_none() => probed = Some(shell),
            msg = tokio::time::timeout(slice, channel.wait()) => match msg {
                Ok(Some(ChannelMsg::Data { data })) => {
                    pending.extend_from_slice(&data);
                    last_output = Some(std::time::Instant::now());
                }
                Ok(Some(ChannelMsg::ExtendedData { data, .. })) => {
                    pending.extend_from_slice(&data);
                    last_output = Some(std::time::Instant::now());
                }
                // 通道自身的其他消息（窗口调整、退出码…）不是输出，继续等
                Ok(Some(_)) => {}
                // 通道结束 -> 不会再有输出了
                Ok(None) => return (probed.unwrap_or(RemoteShell::Unsupported), false),
                // 这段静默/剩余时长到点 -> 回到循环头判断
                Err(_) => {}
            },
        }
    }
}

/// 往 shell 通道写一行命令。
///
/// 参数：channel 为 shell 通道，line 为命令文本（不含回车）
/// 核心流程：命令 + `\r` 一次写完，返回是否写成功；失败一律由调用方静默处理。
async fn write_line(channel: &mut SshChannel, line: &str) -> bool {
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\r');
    channel.data(&bytes[..]).await.is_ok()
}

/// 探测远端登录 shell 类型。
///
/// 参数：handle 为已认证连接
/// 核心流程：在独立的 exec 通道上跑一条只读的 printf（不碰交互式 shell），
/// 超时或执行失败都按不支持处理 —— 探测本身不能成为连接失败的理由。
/// 调用方必须**先**开好交互 shell 通道再调这里：这条 exec 通道是本连接的第二条
/// session，PAM 的 motd 已经发给了第一条（交互 shell）并被缓冲下来。
async fn detect_remote_shell(handle: &Arc<client::Handle<ClientHandler>>) -> RemoteShell {
    let probe = tokio::time::timeout(
        SHELL_PROBE_TIMEOUT,
        exec_on(
            Arc::clone(handle),
            SHELL_PROBE_COMMAND,
            SHELL_PROBE_MAX_BYTES,
        ),
    )
    .await;
    match probe {
        Ok(Ok(output)) => RemoteShell::classify(&output),
        _ => RemoteShell::Unsupported,
    }
}

/// 一个运行中的 SSH 终端。
///
/// 无参数
/// 核心流程：同步侧只持有连接句柄、指令通道与关闭标志，真正的网络 IO 在后台任务里。
pub struct SshSession {
    connection: SshConnection,
    commands: mpsc::UnboundedSender<SshCommand>,
    closed: Arc<AtomicBool>,
}

impl SshSession {
    /// 建立连接并打开交互式 shell。
    ///
    /// 参数：options 为连接参数，sink 为输出回调
    /// 核心流程：同步等待「握手 + 认证 + 开 channel + 请求 PTY/Shell」全部成功，
    /// 再把该通道的读写循环交给后台任务，失败原因分类返回。
    pub fn connect(store: &Store, options: &SshOptions, sink: OutputSink) -> CoreResult<Self> {
        let pump = start_output_pump(sink)?;
        let closed = Arc::new(AtomicBool::new(false));
        let (commands, command_rx) = mpsc::unbounded_channel();

        // 首连结果经由 ready 槽回传，(连接, shell 通道) 一起带回同步侧
        let ready: ShellReady = Arc::new(Mutex::new(None));
        let ready_slot = Arc::clone(&ready);
        let options = options.clone();
        let store = store.clone();
        let mut command_rx = command_rx;
        // 经跳板要连两次 -> 同步等待上限按跳数放宽
        let wait = sync_wait(&options);

        // 在常驻 runtime 上跑异步流程 -> 同步侧只用 channel 交互
        runtime().spawn(async move {
            let result = async {
                let connection = SshConnection::connect_async(&store, &options).await?;
                let channel = connection
                    .open_shell(&options.term, options.cols, options.rows)
                    .await?;
                Ok((connection, channel))
            }
            .await;
            if let Ok(mut slot) = ready_slot.lock() {
                *slot = Some(result);
            }
        });

        // 等待首连结果 -> 让调用方明确知道连没连上（经跳板时是两次握手，放宽上限）
        let deadline = std::time::Instant::now() + wait;
        loop {
            if let Ok(mut slot) = ready.lock() {
                // take 而不是 clone -> 保留 HostKeyUnknown/Mismatch 等结构化错误
                if let Some(result) = slot.take() {
                    let (connection, opened) = result?;
                    let pump_for_task = Arc::clone(&pump);
                    let closed_for_task = Arc::clone(&closed);
                    let connection_for_task = connection.clone();
                    // 注入钩子期间读到的登录 banner 先入队 ->
                    // 保证「banner 在注入输出之前」的顺序，和真实字节流一致
                    if !opened.pending.is_empty() {
                        pump.push(&opened.pending);
                    }
                    // 通道读写循环单独跑 -> 同步侧继续用指令队列交互
                    runtime().spawn(async move {
                        serve(
                            connection_for_task,
                            opened.channel,
                            &mut command_rx,
                            &pump_for_task,
                            &closed_for_task,
                        )
                        .await;
                    });
                    return Ok(Self {
                        connection,
                        commands,
                        closed,
                    });
                }
            }
            if std::time::Instant::now() > deadline {
                return Err(CoreError::Ssh("连接超时".into()));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// 取底层连接。
    ///
    /// 参数：无
    /// 核心流程：把可克隆的连接句柄交给上层，后续 SFTP 与隧道
    /// 直接在它上面开新通道，避免重复握手与认证。
    pub fn connection(&self) -> &SshConnection {
        &self.connection
    }

    /// 把用户输入发到远端。
    ///
    /// 参数：data 为原始字节
    /// 核心流程：投递到指令队列，队列断开说明连接已结束。
    pub fn write(&self, data: &[u8]) -> CoreResult<()> {
        self.commands
            .send(SshCommand::Write(data.to_vec()))
            .map_err(|_| CoreError::Ssh("连接已断开".into()))
    }

    /// 调整远端终端尺寸。
    ///
    /// 参数：cols 列数，rows 行数
    /// 核心流程：投递 resize 指令，由后台任务发 window-change。
    pub fn resize(&self, cols: u16, rows: u16) -> CoreResult<()> {
        self.commands
            .send(SshCommand::Resize(cols, rows))
            .map_err(|_| CoreError::Ssh("连接已断开".into()))
    }

    /// 关闭会话。
    ///
    /// 参数：无
    /// 核心流程：先发关闭指令让后台任务正常断开，重复调用不报错。
    pub fn close(&self) -> CoreResult<()> {
        let _ = self.commands.send(SshCommand::Close);
        self.closed.store(true, Ordering::SeqCst);
        Ok(())
    }
}

impl TerminalControl for SshSession {
    /// 转发写入。
    fn write(&self, data: &[u8]) -> CoreResult<()> {
        SshSession::write(self, data)
    }

    /// 转发尺寸调整。
    fn resize(&self, cols: u16, rows: u16) -> CoreResult<()> {
        SshSession::resize(self, cols, rows)
    }

    /// 转发关闭。
    fn close(&self) -> CoreResult<()> {
        SshSession::close(self)
    }

    /// 暴露底层连接。
    ///
    /// 参数：无
    /// 核心流程：SSH 会话本身就跑在一条已认证连接上，直接把它借给上层，
    /// 远端指标等功能可以在同一条连接上另开 exec 通道。
    fn connection(&self) -> Option<&SshConnection> {
        Some(&self.connection)
    }
}

/// 常驻的 SSH 异步运行时。
///
/// 参数：无
/// 核心流程：全进程只建一次，避免每次连接都起线程池；
/// SFTP 模块复用同一个运行时，省掉第二套线程。
pub(crate) fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("rustmobax-ssh")
            .enable_all()
            .build()
            .expect("SSH 运行时初始化失败")
    })
}

/// 后台主流程：在已开好的 shell 通道上收发数据。
///
/// 参数：connection 为通道所属连接，channel 为 shell 通道，
/// commands 为同步侧指令，pump 为输出泵
/// 核心流程：进入 select 循环，同时处理远端数据与本地指令；
/// 结束时关闭通道、断开连接，并把退出码写进输出泵。
async fn serve(
    connection: SshConnection,
    mut channel: SshChannel,
    commands: &mut mpsc::UnboundedReceiver<SshCommand>,
    pump: &Arc<crate::terminal::Pump>,
    closed: &Arc<AtomicBool>,
) {
    let mut exit_code: Option<i32> = None;
    loop {
        if closed.load(Ordering::SeqCst) {
            break;
        }
        tokio::select! {
            message = channel.wait() => {
                match message {
                    Some(ChannelMsg::Data { data }) => pump.push(&data),
                    Some(ChannelMsg::ExtendedData { data, .. }) => pump.push(&data),
                    Some(ChannelMsg::ExitStatus { exit_status }) => {
                        exit_code = i32::try_from(exit_status).ok();
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                    Some(_) => {}
                }
            }
            command = commands.recv() => {
                match command {
                    Some(SshCommand::Write(data)) => {
                        if channel.data(&data[..]).await.is_err() {
                            break;
                        }
                    }
                    Some(SshCommand::Resize(cols, rows)) => {
                        let _ = channel
                            .window_change(u32::from(cols), u32::from(rows), 0, 0)
                            .await;
                    }
                    Some(SshCommand::Close) | None => break,
                }
            }
        }
    }

    let _ = channel.close().await;
    // 关闭语义与旧实现一致：shell 结束即断开整条连接
    let _ = connection.disconnect().await;
    pump.close_with(exit_code);
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::net::SocketAddr;
    use std::sync::mpsc;

    use russh::server::{self, Auth, ChannelOpenHandle, Msg, Server, Session};
    use russh::{Channel, ChannelId};
    use tokio::net::TcpListener;

    use super::*;

    /// 服务端对 exec 请求的应答方式。
    ///
    /// 无参数
    /// 核心流程：默认把命令原样当输出写回（"执行结果" 就是命令本身），
    /// 需要固定输出或失败场景时由专用启动函数切换。
    #[derive(Clone, Copy)]
    enum ExecMode {
        /// 把收到的命令文本原样写回 stdout。
        Echo,
        /// 写回固定串（模拟远端脚本输出）。
        Fixed(&'static str),
        /// 往 stderr 写错误并以退出码 1 结束（模拟命令失败）。
        Fail,
        /// 直接拒绝 exec 请求（模拟服务端禁用 exec），探测只能按失败处理。
        Reject,
    }

    /// 测试用服务端：接受任意密码，把收到的数据原样回显，
    /// 并具备跳板机能力（把 direct-tcpip 通道接到真实目标端口）。
    #[derive(Clone)]
    struct EchoServer {
        /// shell 建立后先吐这行字 -> 用来区分「连到的到底是哪台机器」。
        banner: &'static str,
        /// 已交给字节流转发的通道（direct-tcpip），各连接共享一份。
        relayed: Arc<Mutex<HashSet<ChannelId>>>,
        /// exec 请求的应答方式。
        exec_mode: ExecMode,
        /// 是否像真行编辑器那样在回显之后补一句提示符。
        prompt: bool,
        /// 是否只在「本连接的第一条 session」上吐 banner（模拟 PAM：motd 只发给首个 session）。
        first_session_only: bool,
        /// 各连接上 shell 通道收到的字节（只记录客户端写进来的）。
        received: Arc<Mutex<Vec<u8>>>,
    }

    struct EchoHandler {
        banner: &'static str,
        /// 这些通道的数据由转发任务处理，回显会把转发的报文搅乱。
        relayed: Arc<Mutex<HashSet<ChannelId>>>,
        /// exec 请求的应答方式。
        exec_mode: ExecMode,
        /// 是否在回显之后补一句提示符（模拟真 shell 的「已就绪」信号）。
        prompt: bool,
        /// 是否只在「本连接的第一条 session」上吐 banner。
        first_session_only: bool,
        /// 本连接上已经开过几条 session 通道（每开一条 +1）。
        session_seq: u32,
        /// 通道 -> 它是本连接的第几条 session（从 1 开始）。
        session_ordinal: std::collections::HashMap<ChannelId, u32>,
        /// shell 通道收到的字节 -> 用例据此断言客户端到底发了什么。
        received: Arc<Mutex<Vec<u8>>>,
    }

    impl server::Handler for EchoHandler {
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
            _key: russh::server::VerifiedPublicKey<'_>,
        ) -> Result<Auth, Self::Error> {
            Ok(Auth::Accept)
        }

        async fn channel_open_session(
            &mut self,
            channel: Channel<Msg>,
            reply: ChannelOpenHandle,
            _session: &mut Session,
        ) -> Result<(), Self::Error> {
            // 记下这是本连接的第几条 session -> 用来模拟「motd 只发给第一个 session」
            self.session_seq += 1;
            self.session_ordinal.insert(channel.id(), self.session_seq);
            reply.accept().await;
            Ok(())
        }

        async fn shell_request(
            &mut self,
            channel: ChannelId,
            session: &mut Session,
        ) -> Result<(), Self::Error> {
            // 模拟 PAM：motd 只发给本连接的第一条 session，
            // 抢占首 session 的那条通道（例如探测用的 exec）会把 motd 吃掉
            if self.first_session_only
                && self.session_ordinal.get(&channel).copied() != Some(1)
            {
                return Ok(());
            }
            let _ = session.data(channel, format!("{}\r\n", self.banner).into_bytes());
            Ok(())
        }

        /// 跳板机能力：接受 direct-tcpip 并替客户端连到真实目标。
        ///
        /// 参数：channel 为新通道，host_to_connect/port_to_connect 为要连的目标，
        /// originator_* 为发起方地址（测试不关心），reply 为接受/拒绝句柄
        /// 核心流程：先连目标 TCP（连不上就不 accept，russh 自动回拒绝）→
        /// accept 后把通道转成字节流，与目标 TCP 做双向拷贝，直到任一侧关闭。
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
            let target = format!("{host_to_connect}:{port_to_connect}");
            let Ok(mut tcp) = tokio::net::TcpStream::connect(&target).await else {
                return Ok(());
            };
            // 先登记再 accept -> 之后进来的数据一律交给转发任务，不回显
            if let Ok(mut set) = self.relayed.lock() {
                set.insert(channel.id());
            }
            reply.accept().await;
            tokio::spawn(async move {
                let mut stream = channel.into_stream();
                let _ = tokio::io::copy_bidirectional(&mut stream, &mut tcp).await;
            });
            Ok(())
        }

        /// 子系统请求：回成功并吐一句标记，供多通道测试断言。
        ///
        /// 参数：channel 为通道，name 为子系统名
        /// 核心流程：先应答 channel_success，再往该通道写 SUBSYS 标记，
        /// 让客户端确认这条通道确实被服务端接住了。
        /// exec 请求：按 ExecMode 回输出并结束通道。
        ///
        /// 参数：channel 为通道，data 为客户端发来的命令文本
        /// 核心流程：先应答成功 -> 按模式写 stdout 或 stderr -> 非 0 退出码场景
        /// 补一条 exit-status -> 最后 eof + close，客户端据此结束等待。
        async fn exec_request(
            &mut self,
            channel: ChannelId,
            data: &[u8],
            session: &mut Session,
        ) -> Result<(), Self::Error> {
            let _ = match self.exec_mode {
                // 拒绝这条 exec 请求 -> 客户端只能当「探测失败」处理
                ExecMode::Reject => session.channel_failure(channel),
                _ => session.channel_success(channel),
            };
            match self.exec_mode {
                ExecMode::Echo => {
                    let _ = session.data(channel, data.to_vec());
                }
                ExecMode::Fixed(text) => {
                    let _ = session.data(channel, text.as_bytes().to_vec());
                }
                ExecMode::Fail => {
                    let _ = session.extended_data(channel, 1, b"REMOTE_METRICS_FAIL\n".to_vec());
                    let _ = session.exit_status_request(channel, 1);
                }
                ExecMode::Reject => {}
            }
            let _ = session.eof(channel);
            let _ = session.close(channel);
            Ok(())
        }

        async fn subsystem_request(
            &mut self,
            channel: ChannelId,
            _name: &str,
            session: &mut Session,
        ) -> Result<(), Self::Error> {
            let _ = session.channel_success(channel);
            let _ = session.data(channel, b"SUBSYS\r\n".to_vec());
            Ok(())
        }

        async fn data(
            &mut self,
            channel: ChannelId,
            data: &[u8],
            session: &mut Session,
        ) -> Result<(), Self::Error> {
            // 转发通道的数据由 relay 任务送往真实目标 -> 这里不能再回显
            if self
                .relayed
                .lock()
                .map(|set| set.contains(&channel))
                .unwrap_or(false)
            {
                return Ok(());
            }
            // 先记账再回显 -> 用例可以断言「服务端到底收到过什么」
            if let Ok(mut log) = self.received.lock() {
                log.extend_from_slice(data);
            }
            let _ = session.data(channel, data.to_vec());
            if self.prompt && data.ends_with(b"\r") {
                // 真行编辑器执行完一行会重画提示符 -> 注入侧据此确认「命令真的执行了」
                let _ = session.data(channel, b"\r\nPS> ".to_vec());
            }
            Ok(())
        }
    }

    impl Server for EchoServer {
        type Handler = EchoHandler;

        fn new_client(&mut self, _peer_addr: Option<SocketAddr>) -> Self::Handler {
            EchoHandler {
                banner: self.banner,
                relayed: Arc::clone(&self.relayed),
                exec_mode: self.exec_mode,
                prompt: self.prompt,
                first_session_only: self.first_session_only,
                session_seq: 0,
                session_ordinal: std::collections::HashMap::new(),
                received: Arc::clone(&self.received),
            }
        }
    }

    /// 起一个本地 echo SSH 服务端（默认 banner）。
    ///
    /// 参数：无
    /// 核心流程：等价于用 WELCOME 当 banner 起一台，保持既有用例的断言不变。
    async fn spawn_echo_server() -> u16 {
        spawn_banner_server("WELCOME").await
    }

    /// 起一个带自定义 banner 的本地 echo SSH 服务端。
    ///
    /// 参数：banner 为 shell 建立后先吐的标记文字
    /// 核心流程：等价于 exec 回显命令的默认服务端，保持既有用例行为不变。
    async fn spawn_banner_server(banner: &'static str) -> u16 {
        spawn_server_with(banner, ExecMode::Echo).await
    }

    /// 起一个 exec 返回固定串的服务端。
    ///
    /// 参数：reply 为 exec 时写回 stdout 的固定文本
    /// 核心流程：banner 用默认值，仅切换 exec 应答方式，供远端指标解析用例使用。
    async fn spawn_exec_server(reply: &'static str) -> u16 {
        spawn_server_with("WELCOME", ExecMode::Fixed(reply)).await
    }

    /// 起一个 exec 必定失败的服务端。
    ///
    /// 参数：无
    /// 核心流程：复用 WELCOME banner，exec 时写 stderr 并以退出码 1 结束。
    async fn spawn_exec_fail_server() -> u16 {
        spawn_server_with("WELCOME", ExecMode::Fail).await
    }

    /// 起一个可配置 exec 行为、带提示符且记账的服务端（banner 用默认值）。
    ///
    /// 参数：reply 为 exec（shell 探测）时写回的固定文本
    /// 核心流程：等价于带 motd 的版本，只是 motd 用默认的 WELCOME。
    #[allow(dead_code)]
    async fn spawn_prompt_shell_server(reply: &'static str) -> u16 {
        spawn_full_server("WELCOME", ExecMode::Fixed(reply), true).await.0
    }

    /// 起一个带自定义 motd 的 prompt 型服务端，并把「服务端收到的字节」交回来。
    ///
    /// 参数：banner 为登录后先吐的整段 motd（可含 ANSI 颜色），
    /// reply 为 exec（shell 探测）时写回的固定文本
    /// 核心流程：注入窗口内客户端不会把回显交给前端，所以只能从服务端侧
    /// 反推「客户端到底写了什么」；这也是「钩子真的发出去了」的唯一可观测证据。
    async fn spawn_recording_motd_server(
        banner: &'static str,
        reply: &'static str,
    ) -> (u16, Arc<Mutex<Vec<u8>>>) {
        spawn_full_server(banner, ExecMode::Fixed(reply), true).await
    }

    /// 起一个可配置 exec 行为的本地 SSH 服务端。
    ///
    /// 参数：banner 为 shell 建立后先吐的标记文字，exec_mode 为 exec 应答方式
    /// 核心流程：默认不打提示符，保持既有用例行为不变。
    async fn spawn_server_with(banner: &'static str, exec_mode: ExecMode) -> u16 {
        spawn_full_server(banner, exec_mode, false).await.0
    }

    /// 起一个可配置 exec 行为与提示符行为的本地 SSH 服务端。
    ///
    /// 参数：banner 为 shell 建立后先吐的标记文字，exec_mode 为 exec 应答方式，
    /// prompt 为是否在回显后补提示符
    /// 核心流程：等价于「motd 发给每条 session」的版本，保持既有用例行为不变。
    async fn spawn_full_server(
        banner: &'static str,
        exec_mode: ExecMode,
        prompt: bool,
    ) -> (u16, Arc<Mutex<Vec<u8>>>) {
        spawn_full_server_with(banner, exec_mode, prompt, false).await
    }

    /// 起一台「motd 只发给本连接第一个 session」的服务端（模拟 PAM 的真实行为）。
    ///
    /// 参数：motd 为登录后首个 session 上吐出的整段 motd，exec_mode 为 exec 应答方式
    /// 核心流程：谁先开 session 通道，motd 就归谁 —— 探测用的 exec 通道若抢先，
    /// 交互 shell 就再也拿不到 motd，正是线上「连接后 motd 丢失」的复现条件。
    async fn spawn_first_session_motd_server(
        motd: &'static str,
        exec_mode: ExecMode,
    ) -> (u16, Arc<Mutex<Vec<u8>>>) {
        spawn_full_server_with(motd, exec_mode, true, true).await
    }

    /// 起一个可配置 exec 行为与提示符行为的本地 SSH 服务端。
    ///
    /// 参数：banner 为 shell 建立后先吐的标记文字，exec_mode 为 exec 应答方式，
    /// prompt 为是否在回显后补提示符，
    /// first_session_only 为「banner 是否只发给本连接的第一条 session」
    /// 核心流程：绑 127.0.0.1:0 拿随机端口 → 后台跑 russh server →
    /// 返回端口与服务端收到的字节（用于反推客户端到底写了什么）。
    async fn spawn_full_server_with(
        banner: &'static str,
        exec_mode: ExecMode,
        prompt: bool,
        first_session_only: bool,
    ) -> (u16, Arc<Mutex<Vec<u8>>>) {
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

        let received: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let received_for_server = Arc::clone(&received);
        tokio::spawn(async move {
            let mut server = EchoServer {
                banner,
                relayed: Arc::new(Mutex::new(HashSet::new())),
                exec_mode,
                prompt,
                first_session_only,
                received: received_for_server,
            };
            let _ = server.run_on_socket(config, &listener).await;
        });

        (port, received)
    }

    #[test]
    fn connects_with_password_and_receives_data() {
        let (tx, rx) = mpsc::channel();
        let port = runtime().block_on(spawn_echo_server());
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");

        let session = SshSession::connect(
            &store,
            &SshOptions {
                host: "127.0.0.1".into(),
                port,
                username: "tester".into(),
                auth: SshAuth::Password("secret".into()),
                cols: 100,
                rows: 30,
                term: "xterm-256color".into(),
                accept_unknown_host_key: true,
                jump: None,
            },
            Box::new(move |output| {
                let _ = tx.send(output);
            }),
        )
        .expect("应能连接测试服务端");

        // 服务端在 shell 建立后立刻发问候 -> 客户端应收到
        let mut collected = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(crate::terminal::TerminalOutput::Data(bytes)) => {
                    collected.push_str(&String::from_utf8_lossy(&bytes));
                    if collected.contains("WELCOME") {
                        break;
                    }
                }
                Ok(crate::terminal::TerminalOutput::Exit(_)) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(collected.contains("WELCOME"), "应收到服务端问候，实际：{collected:?}");

        // 写入后应被原样回显
        session.write(b"PING\n").expect("写入应成功");
        let mut echoed = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(crate::terminal::TerminalOutput::Data(bytes)) => {
                    echoed.push_str(&String::from_utf8_lossy(&bytes));
                    if echoed.contains("PING") {
                        break;
                    }
                }
                Ok(crate::terminal::TerminalOutput::Exit(_)) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(echoed.contains("PING"), "写入应被回显，实际：{echoed:?}");

        session.close().expect("关闭应成功");
    }

    /// 构造一套指向本地 echo 服务端的连接参数。
    ///
    /// 参数：port 为服务端端口，accept 为是否已确认指纹
    /// 核心流程：集中默认值，让三态测试只表达差异点。
    fn echo_options(port: u16, accept: bool) -> SshOptions {
        SshOptions {
            host: "127.0.0.1".into(),
            port,
            username: "tester".into(),
            auth: SshAuth::Password("secret".into()),
            cols: 80,
            rows: 24,
            term: "xterm-256color".into(),
            accept_unknown_host_key: accept,
            jump: None,
        }
    }

    #[test]
    fn rejects_unknown_host_key_when_not_confirmed() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let port = runtime().block_on(spawn_echo_server());

        let result = SshSession::connect(&store, &echo_options(port, false), Box::new(|_| {}));

        match result {
            Err(CoreError::HostKeyUnknown(info)) => {
                assert_eq!(info.port, port);
                assert!(info.sha256.starts_with("SHA256:"), "应给出 SHA256 指纹");
                assert!(info.md5.starts_with("MD5:"), "应给出 MD5 指纹");
            }
            Ok(_) => panic!("未知主机应被阻断，但连接成功了"),
            Err(err) => panic!("应报未知主机，实际：{err}"),
        }
    }

    #[test]
    fn records_host_key_after_confirmation() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let port = runtime().block_on(spawn_echo_server());

        let session = SshSession::connect(&store, &echo_options(port, true), Box::new(|_| {}))
            .expect("确认后应能连上");

        let hosts = runtime().block_on(async { store.known_host_list().await }).expect("应能读列表");
        assert!(
            hosts
                .iter()
                .any(|item| item.port == port && item.fingerprint.starts_with("SHA256:")),
            "确认后应把指纹写入 known_hosts，实际：{hosts:?}"
        );
        session.close().expect("关闭应成功");
    }

    #[test]
    fn blocks_when_fingerprint_changed() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let port = runtime().block_on(spawn_echo_server());

        // 先正常连一次落库 -> 拿到真实 key_type
        let first = SshSession::connect(&store, &echo_options(port, true), Box::new(|_| {}))
            .expect("首次连接应成功");
        first.close().expect("关闭应成功");

        let hosts = runtime().block_on(async { store.known_host_list().await }).expect("应能读列表");
        let saved = hosts
            .iter()
            .find(|item| item.port == port)
            .expect("应已记录主机");
        let key_type = saved.key_type.clone();

        // 篡改指纹 -> 模拟中间人换了密钥
        runtime().block_on(async {
            store
                .known_host_upsert("127.0.0.1", port, &key_type, "SHA256:forged")
                .await
        })
        .expect("应能写入伪造指纹");

        let result = SshSession::connect(&store, &echo_options(port, true), Box::new(|_| {}));
        match result {
            Err(CoreError::HostKeyMismatch(info)) => {
                assert_ne!(info.sha256, "SHA256:forged", "应报告真实指纹而不是记录值");
            }
            Ok(_) => panic!("指纹变更时应被阻断，但连接成功了"),
            Err(err) => panic!("应报指纹变更，实际：{err}"),
        }
    }

    /// 生成一对测试密钥并写到临时目录。
    ///
    /// 参数：无
    /// 核心流程：返回 (私钥路径, 临时目录)，测试结束时由调用方清理。
    fn write_temp_key() -> (std::path::PathBuf, std::path::PathBuf) {
        let key = russh::keys::PrivateKey::random(
            &mut rand::rng(),
            russh::keys::Algorithm::Ed25519,
        )
        .expect("应能生成密钥");
        let dir = std::env::temp_dir().join(format!(
            "rmx-key-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        std::fs::create_dir_all(&dir).expect("应能建临时目录");
        let path = dir.join("id_ed25519");
        let pem = key
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .expect("应能导出 OpenSSH 私钥");
        std::fs::write(&path, pem.as_bytes()).expect("应能写私钥文件");
        (path, dir)
    }

    #[test]
    fn authenticates_with_private_key() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let port = runtime().block_on(spawn_echo_server());
        let (key_path, dir) = write_temp_key();

        let options = SshOptions {
            auth: SshAuth::PrivateKey {
                path: key_path.to_string_lossy().to_string(),
                passphrase: None,
            },
            ..echo_options(port, true)
        };
        let session = SshSession::connect(&store, &options, Box::new(|_| {}))
            .expect("私钥认证应能连上");
        session.close().expect("关闭应成功");

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn reports_clear_error_when_key_missing() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let port = runtime().block_on(spawn_echo_server());

        let options = SshOptions {
            auth: SshAuth::PrivateKey {
                path: "/nonexistent/id_rsa".into(),
                passphrase: None,
            },
            ..echo_options(port, true)
        };
        let result = SshSession::connect(&store, &options, Box::new(|_| {}));
        let message = match result {
            Ok(_) => panic!("私钥不存在时应失败"),
            Err(err) => err.to_string(),
        };
        assert!(
            message.contains("读取私钥失败"),
            "错误应指明是私钥读取问题，实际：{message}"
        );
    }

    #[test]
    fn reports_error_when_port_closed() {
        // 未监听的端口 -> 应快速给出连接失败而不是卡住
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let result = SshSession::connect(
            &store,
            &SshOptions {
                host: "127.0.0.1".into(),
                port: 1,
                username: "tester".into(),
                auth: SshAuth::Password("secret".into()),
                cols: 80,
                rows: 24,
                term: "xterm-256color".into(),
                accept_unknown_host_key: true,
                jump: None,
            },
            Box::new(|_| {}),
        );
        assert!(result.is_err(), "连不上时应返回错误");
        let message = result.err().map(|err| err.to_string()).unwrap_or_default();
        assert!(message.contains("连接"), "错误文案应指明连接问题，实际：{message}");
    }

    /// 在通道上等到一段文本出现。
    ///
    /// 参数：channel 为待读通道，needle 为目标片段
    /// 核心流程：带超时地连续 wait，把收到的数据拼成字符串，
    /// 命中目标片段、通道结束或超时为止。
    fn read_channel_until(channel: &mut SshChannel, needle: &str) -> String {
        let mut collected = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !collected.contains(needle) {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match runtime().block_on(async { tokio::time::timeout(remaining, channel.wait()).await }) {
                Ok(Some(ChannelMsg::Data { data })) => {
                    collected.push_str(&String::from_utf8_lossy(&data));
                }
                Ok(Some(ChannelMsg::ExtendedData { data, .. })) => {
                    collected.push_str(&String::from_utf8_lossy(&data));
                }
                Ok(Some(ChannelMsg::Eof)) | Ok(Some(ChannelMsg::Close)) | Ok(None) => break,
                Ok(Some(_)) => {}
                Err(_) => break,
            }
        }
        collected
    }

    /// open_shell 交付的首批字节（pending）加上通道上后续到达的字节。
    ///
    /// 参数：pending 为 open_shell 返回的首批输出，channel 为同一条通道，needle 为目标片段
    /// 核心流程：motd 这类首批输出现在由 pending 交付（探测期间缓冲、一个字节都没丢），
    /// 所以要断言「shell 到底说了什么」必须两处一起看；pending 里已命中就不再等通道。
    fn pending_then_channel(pending: &[u8], channel: &mut SshChannel, needle: &str) -> String {
        let mut text = String::from_utf8_lossy(pending).to_string();
        if text.contains(needle) {
            return text;
        }
        text.push_str(&read_channel_until(channel, needle));
        text
    }

    /// 在一个固定时间窗内收通道上的数据。
    ///
    /// 参数：channel 为待读通道，window 为观察时长
    /// 核心流程：用于断言「这段时间里通道上到底有没有东西」——
    /// 注入窗口把回显吞掉之后，这类「应该什么都没有」的断言只能靠固定时间窗来证。
    fn read_channel_for(channel: &mut SshChannel, window: Duration) -> String {
        let mut collected = String::new();
        let deadline = std::time::Instant::now() + window;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match runtime().block_on(async { tokio::time::timeout(remaining, channel.wait()).await }) {
                Ok(Some(ChannelMsg::Data { data })) => {
                    collected.push_str(&String::from_utf8_lossy(&data));
                }
                Ok(Some(ChannelMsg::ExtendedData { data, .. })) => {
                    collected.push_str(&String::from_utf8_lossy(&data));
                }
                Ok(Some(ChannelMsg::Eof)) | Ok(Some(ChannelMsg::Close)) | Ok(None) => break,
                Ok(Some(_)) => {}
                Err(_) => break,
            }
        }
        collected
    }

    /// 取服务端收到的字节（用例据此反推客户端写了什么）。
    ///
    /// 参数：log 为服务端记账句柄
    /// 核心流程：注入窗口内的回显不会到前端，只能从服务端侧确认「字节确实发出去了」。
    fn recorded(log: &Arc<Mutex<Vec<u8>>>) -> String {
        log.lock()
            .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
            .unwrap_or_default()
    }

    #[test]
    fn opens_shell_and_subsystem_on_same_connection() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let port = runtime().block_on(spawn_echo_server());

        // 只连接一次 -> 后续两种通道都复用这条连接
        let connection = SshConnection::connect(&store, &echo_options(port, true))
            .expect("应能建立连接");

        let opened = runtime()
            .block_on(connection.open_shell("xterm-256color", 80, 24))
            .expect("应能在连接上开 shell 通道");
        let mut shell = opened.channel;
        let shell_id = shell.id();
        assert!(
            pending_then_channel(&opened.pending, &mut shell, "WELCOME").contains("WELCOME"),
            "shell 通道应收到问候"
        );

        // 同一条连接上再开一个子系统通道
        let mut subsystem = runtime()
            .block_on(connection.open_subsystem("sftp"))
            .expect("应能在同一连接上开子系统通道");
        assert_ne!(shell_id, subsystem.id(), "两条通道应是彼此独立的通道");
        assert!(
            read_channel_until(&mut subsystem, "SUBSYS").contains("SUBSYS"),
            "子系统通道应收到服务端标记"
        );

        // 子系统通道开着时，原 shell 通道仍然可读写
        runtime()
            .block_on(shell.data(&b"PING\n"[..]))
            .expect("shell 通道应仍可写");
        assert!(
            read_channel_until(&mut shell, "PING").contains("PING"),
            "shell 通道应仍能收到回显"
        );

        assert!(!connection.is_closed(), "两条通道并存时连接应保持");
        runtime().block_on(connection.disconnect()).expect("断开应成功");
    }

    #[test]
    fn session_shares_connection_with_subsystem_channel() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let port = runtime().block_on(spawn_echo_server());

        let session = SshSession::connect(&store, &echo_options(port, true), Box::new(|_| {}))
            .expect("应能连上测试服务端");

        // 从会话句柄取回连接 -> 在 shell 之外再开一个子系统通道
        let mut subsystem = runtime()
            .block_on(session.connection().open_subsystem("sftp"))
            .expect("会话的连接应能再开子系统通道");
        assert!(
            read_channel_until(&mut subsystem, "SUBSYS").contains("SUBSYS"),
            "子系统通道应收到服务端标记"
        );

        // 子系统通道开着时，shell 侧指令通道仍然可用
        session.write(b"PING\n").expect("写入应成功");
        session.resize(100, 30).expect("调整尺寸应成功");
        assert!(!session.connection().is_closed(), "会话连接应保持打开");

        // 以 trait 口径取连接 -> 远端指标靠这一条区分本机与远端
        let control: &dyn TerminalControl = &session;
        assert!(
            control.connection().is_some(),
            "SSH 会话应暴露底层连接"
        );

        session.close().expect("关闭应成功");
    }

    /// bash 注入脚本：装钩子、发 OSC 7、并且不吞用户原本的 PROMPT_COMMAND。
    #[test]
    fn osc7_script_appends_bash_prompt_command_hook() {
        let script = osc7_setup_script(RemoteShell::Bash).expect("bash 应生成注入脚本");

        assert!(
            script.contains(r"printf '\033]7;file://%s%s\033\\'"),
            "应输出带 ST 终止符的 OSC 7，实际：{script}"
        );
        assert!(script.contains(r#""$PWD""#), "应上报当前目录，实际：{script}");
        assert!(
            script.contains("${PROMPT_COMMAND:+;$PROMPT_COMMAND}"),
            "应把用户原有的 PROMPT_COMMAND 追加在后面而不是覆盖，实际：{script}"
        );
        // 追加语义：赋值右侧引用旧值，而不是直接赋成常量
        assert!(
            script.contains(r#"PROMPT_COMMAND="__rmx_osc7${PROMPT_COMMAND"#),
            "赋值应保留旧值，实际：{script}"
        );
        // 回显是在注入前用 stty -echo 关掉的，脚本里不再自带擦行语句
        assert!(
            !script.contains(r"\033[1A"),
            "脚本里不该再有擦行语句（回显已经在注入前关掉了），实际：{script}"
        );
        assert!(!script.contains('\r'), "脚本里不能有换行/回车字节");
    }

    /// zsh 注入脚本：用 precmd_functions 追加，且不碰 PROMPT_COMMAND。
    #[test]
    fn osc7_script_uses_precmd_functions_for_zsh() {
        let script = osc7_setup_script(RemoteShell::Zsh).expect("zsh 应生成注入脚本");

        assert!(
            script.contains(r"printf '\033]7;file://%s%s\033\\'"),
            "应输出 OSC 7，实际：{script}"
        );
        assert!(
            script.contains("precmd_functions=(__rmx_osc7 ${precmd_functions:#__rmx_osc7})"),
            "应保留用户已有 precmd 钩子并去重，实际：{script}"
        );
        assert!(
            !script.contains("PROMPT_COMMAND"),
            "zsh 不该用 bash 的钩子，实际：{script}"
        );
    }

    /// 不支持的 shell 完全不注入；探测结果按几种真实形态都能认出来。
    #[test]
    fn osc7_script_skips_unsupported_shell() {
        assert!(
            osc7_setup_script(RemoteShell::Unsupported).is_none(),
            "不支持的 shell 应返回 None（调用方一个字节都不发）"
        );
        assert!(
            cwd_hook_payload(RemoteShell::Unsupported).is_none(),
            "不支持的 shell 不该拼出任何要发送的内容"
        );

        assert_eq!(RemoteShell::classify("/bin/bash\nbash\n"), RemoteShell::Bash);
        assert_eq!(RemoteShell::classify("-bash\n"), RemoteShell::Bash);
        assert_eq!(RemoteShell::classify("/usr/bin/zsh\nzsh\n"), RemoteShell::Zsh);
        assert_eq!(
            RemoteShell::classify("/usr/bin/fish\n"),
            RemoteShell::Unsupported
        );
        assert_eq!(RemoteShell::classify("/bin/sh\n"), RemoteShell::Unsupported);
        // 探测器把命令原样回显（服务端不支持 exec）时也不能误判成 bash
        assert_eq!(
            RemoteShell::classify(r#"printf '%s\n' "$SHELL" "$0""#),
            RemoteShell::Unsupported
        );
        assert_eq!(RemoteShell::classify(""), RemoteShell::Unsupported);
    }

    /// 注入整行的顺序：先装钩子，再恢复回显；不含任何擦行语句。
    #[test]
    fn osc7_payload_restores_echo_before_erasing() {
        let payload = cwd_hook_payload(RemoteShell::Bash).expect("bash 应拼出注入内容");

        let install_at = payload.find("__rmx_osc7").expect("应包含钩子");
        let restore_at = payload.find(ECHO_RESTORE).expect("应包含恢复回显的语句");

        assert!(
            install_at < restore_at,
            "应先装钩子再恢复回显，实际：{payload}"
        );
        // 回显靠「注入窗口里丢字节」消除，payload 里不能再有任何擦行/上移动作
        assert!(
            !payload.contains(r"\033[1A") && !payload.contains(r"\033[2K"),
            "payload 不该再擦行（擦多了会连 motd 一起吃掉），实际：{payload}"
        );
        assert!(
            !payload.contains(r"printf '\r"),
            "payload 不该再用 printf 输出擦行序列，实际：{payload}"
        );
        assert!(!payload.contains('\r'), "整行里不能有回车字节：{payload}");
    }

    /// 关回显之后，只有「自己那行的回显」不算 shell 已经执行完成的证据。
    #[test]
    fn osc7_prompt_evidence_ignores_own_echo() {
        let echo = ECHO_OFF_LINE.as_bytes();
        // 只有自己那行的回显（行编辑器收到什么回什么）-> 还不能注入
        assert!(!has_prompt_after_own_echo(echo, echo));
        assert!(!has_prompt_after_own_echo(
            format!("{}\r\n", ECHO_OFF_LINE).as_bytes(),
            echo
        ));
        // 回显分片到达（尾巴只是回显的开头）也不能当成提示符证据
        assert!(!has_prompt_after_own_echo(b"stty -echo 2>/dev/nu", echo));
        assert!(!has_prompt_after_own_echo(b"stty", echo));
        // 回显之外还有提示符 -> 说明这条命令真的执行完了
        assert!(has_prompt_after_own_echo(
            format!("{}\r\nhuya@host:~$ ", ECHO_OFF_LINE).as_bytes(),
            echo
        ));
        assert!(has_prompt_after_own_echo(b"\r\nP> ", echo));
        assert!(!has_prompt_after_own_echo(b"", echo));
        // 注入行自己的回显同理（第二段窗口用的就是这一条）
        let payload = cwd_hook_payload(RemoteShell::Bash).expect("bash 应拼出注入内容");
        assert!(!has_prompt_after_own_echo(
            payload.as_bytes(),
            payload.as_bytes()
        ));
        assert!(has_prompt_after_own_echo(
            format!("{payload}\r\nPS> ").as_bytes(),
            payload.as_bytes()
        ));
    }

    /// 擦行逻辑已删除：payload 里不许出现任何清行/上移的 ANSI 序列。
    #[test]
    fn osc7_clear_sequence_grows_on_narrow_terminals() {
        // 用例名沿用历史（原先是验证「窄终端多擦几行」），现在验证「根本不再擦」：
        // 屏幕上的注入痕迹由注入窗口丢字节消除，与终端宽度无关
        for shell in [RemoteShell::Bash, RemoteShell::Zsh] {
            let payload = cwd_hook_payload(shell).expect("支持的 shell 应拼出注入内容");
            for banned in [r"\033[1A", r"\033[2K", r"\033[K", r"\033[2J", r"\r"] {
                assert!(
                    !payload.contains(banned),
                    "payload 不该包含擦行/上移序列 {banned}，实际：{payload}"
                );
            }
            assert!(
                !payload.contains('\u{1b}'),
                "payload 里不能出现裸 ESC 字节（只允许 printf 的 \\033 字面量），实际：{payload}"
            );
        }
    }

    /// 探测到 bash 时，注入要按「关回显 → 写钩子 → 恢复回显」发出去，
    /// 而且这些字节一个都不许进前端。
    #[test]
    fn injects_osc7_hook_into_shell_channel() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        // exec 返回 "bash" -> 探测认定 bash -> open_shell 应发注入语句；
        // 这台服务端会像真行编辑器那样在回显后补提示符，注入才会走到第三步
        let (port, received) =
            runtime().block_on(spawn_recording_motd_server("WELCOME", "/bin/bash\nbash\n"));

        let connection = SshConnection::connect(&store, &echo_options(port, true))
            .expect("应能连上测试服务端");
        let opened = runtime()
            .block_on(connection.open_shell("xterm-256color", 80, 24))
            .expect("应能在连接上开 shell 通道");
        let mut shell = opened.channel;

        let pending = String::from_utf8_lossy(&opened.pending).to_string();
        assert!(
            pending.contains("WELCOME"),
            "注入前读到的登录 banner 必须原样交回上层，实际：{pending:?}"
        );
        // 注入从「关回显」那一行开始 -> 它的回显落在窗口里被丢弃，不该上屏
        assert!(
            !pending.contains(ECHO_OFF_LINE),
            "注入窗口内的回显不能进前端（否则就是屏幕上那行注入命令），实际：{pending:?}"
        );
        assert!(
            !pending.contains("__rmx_osc7"),
            "钩子本身绝不能上屏，实际：{pending:?}"
        );

        // 前端看不到 -> 只能从服务端侧确认「这些字节确实发出去了」
        let sent = recorded(&received);
        assert!(
            sent.contains(ECHO_OFF_LINE),
            "写钩子之前应先关掉远端回显，实际：{sent:?}"
        );
        assert!(
            sent.contains("__rmx_osc7") && sent.contains("PROMPT_COMMAND"),
            "应把钩子函数与 PROMPT_COMMAND 钩子写进 shell 通道，实际：{sent:?}"
        );
        assert!(
            sent.contains(ECHO_RESTORE),
            "注入后必须把回显恢复回来，实际：{sent:?}"
        );
        assert!(
            !sent.contains(r"\033[1A") && !sent.contains(r"\033[2K"),
            "注入内容不该带任何擦行序列，实际：{sent:?}"
        );

        // 两段窗口把注入的回显全吃掉了 -> 通道上不该再有残留字节留给前端
        let leaked = read_channel_for(&mut shell, Duration::from_millis(300));
        assert!(
            leaked.is_empty(),
            "注入窗口内的字节必须被后端吞掉，实际漏出：{leaked:?}"
        );

        runtime().block_on(connection.disconnect()).expect("断开应成功");
    }

    /// 注入窗口内的回显被丢弃：服务器把注入行原样回显，但这些字节不进 pending。
    #[test]
    fn osc7_injection_window_swallows_own_echo() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let (port, received) =
            runtime().block_on(spawn_recording_motd_server("WELCOME", "/bin/zsh\nzsh\n"));

        let connection = SshConnection::connect(&store, &echo_options(port, true))
            .expect("应能连上测试服务端");
        let opened = runtime()
            .block_on(connection.open_shell("xterm-256color", 80, 24))
            .expect("应能在连接上开 shell 通道");
        let mut shell = opened.channel;

        // 服务端确实把注入行回显回来了（这是真机上「屏幕上出现注入命令」的来源）
        let sent = recorded(&received);
        assert!(sent.contains("precmd_functions"), "zsh 钩子应已发出：{sent:?}");

        // 但回显字节一个都没交回前端
        let pending = String::from_utf8_lossy(&opened.pending).to_string();
        for banned in [ECHO_OFF_LINE, "precmd_functions", "PS>"] {
            assert!(
                !pending.contains(banned),
                "窗口内的字节不该进前端：{banned} 出现在 {pending:?}"
            );
        }
        let leaked = read_channel_for(&mut shell, Duration::from_millis(300));
        assert!(leaked.is_empty(), "窗口结束后也不该补发回显，实际：{leaked:?}");

        runtime().block_on(connection.disconnect()).expect("断开应成功");
    }

    /// 注入窗口开启前的字节（motd 全文，含颜色序列）必须原样转发，一个都不许吞。
    #[test]
    fn osc7_motd_before_window_is_forwarded() {
        /// 模拟真机 motd：多行 + 一行带颜色的链接。
        const MOTD: &str = "Last login: Thu Nov  5 11:04:39 2026 from 172.25.102.136\r\nWelcome to Ubuntu 22.04 LTS\r\n\x1b[1;34mhttps://help.ubuntu.com/\x1b[0m";

        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let (port, _received) =
            runtime().block_on(spawn_recording_motd_server(MOTD, "/bin/bash\nbash\n"));

        let connection = SshConnection::connect(&store, &echo_options(port, true))
            .expect("应能连上测试服务端");
        let opened = runtime()
            .block_on(connection.open_shell("xterm-256color", 80, 24))
            .expect("应能在连接上开 shell 通道");

        let pending = String::from_utf8_lossy(&opened.pending).to_string();
        assert!(
            pending.contains("Last login: Thu Nov  5 11:04:39 2026"),
            "motd 第一行必须完整，实际：{pending:?}"
        );
        assert!(
            pending.contains("Welcome to Ubuntu 22.04 LTS"),
            "motd 第二行不能被擦掉，实际：{pending:?}"
        );
        assert!(
            pending.contains("\x1b[1;34mhttps://help.ubuntu.com/\x1b[0m"),
            "motd 里的颜色序列必须原样保留，实际：{pending:?}"
        );
        assert!(
            !pending.contains(r"\033[1A") && !pending.contains(r"\033[2K"),
            "前端不该收到任何擦行序列，实际：{pending:?}"
        );

        runtime().block_on(connection.disconnect()).expect("断开应成功");
    }

    /// 没拿到「shell 已经执行」的证据时，只关回显不装钩子，并且把回显恢复回去。
    #[test]
    fn injection_aborts_without_prompt_evidence() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        // 这台服务端只回显、从不打提示符 -> 注入侧拿不到证据，窗口按超时收手
        let (port, received) =
            runtime().block_on(spawn_full_server("WELCOME", ExecMode::Fixed("/bin/bash\nbash\n"), false));

        let connection = SshConnection::connect(&store, &echo_options(port, true))
            .expect("应能连上测试服务端");
        let opened = runtime()
            .block_on(connection.open_shell("xterm-256color", 80, 24))
            .expect("应能在连接上开 shell 通道");
        let mut shell = opened.channel;

        // 窗口内读到的内容一律不进前端；窗口结束后恢复转发 -> 兜底的 stty echo 回显可见
        let seen = read_channel_until(&mut shell, ECHO_RESTORE);
        assert!(
            !seen.contains("__rmx_osc7"),
            "拿不到证据时不该写钩子，实际：{seen:?}"
        );
        assert!(
            seen.contains(ECHO_RESTORE),
            "必须把回显恢复回来，绝不把用户留在看不见自己打字的状态，实际：{seen:?}"
        );
        let pending = String::from_utf8_lossy(&opened.pending).to_string();
        assert!(
            pending.contains("WELCOME") && !pending.contains(ECHO_OFF_LINE),
            "窗口外的 banner 要转发、窗口内的回显要吞掉，实际：{pending:?}"
        );
        // 服务端侧确认降级时确实只发了 stty echo，钩子一个字节都没发
        let sent = recorded(&received);
        assert!(
            sent.contains(ECHO_RESTORE) && !sent.contains("__rmx_osc7"),
            "降级路径只该恢复回显，实际：{sent:?}"
        );

        runtime().block_on(connection.disconnect()).expect("断开应成功");
    }

    /// 窗口超时后必须恢复正常转发，不能永久吞输出。
    #[test]
    fn osc7_window_timeout_resumes_forwarding() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        // 从不出提示符的服务端 -> 两段窗口都会走到超时兜底
        let port = runtime().block_on(spawn_server_with("WELCOME", ExecMode::Fixed("/bin/bash\nbash\n")));

        let connection = SshConnection::connect(&store, &echo_options(port, true))
            .expect("应能连上测试服务端");
        let opened = runtime()
            .block_on(connection.open_shell("xterm-256color", 80, 24))
            .expect("应能在连接上开 shell 通道");
        let mut shell = opened.channel;

        // 用户自己的输入必须照常回显 -> 说明吞字节窗口已经关掉
        runtime()
            .block_on(async { shell.data(&b"PING_AFTER_WINDOW\r"[..]).await })
            .expect("窗口结束后应仍可写");
        let seen = read_channel_until(&mut shell, "PING_AFTER_WINDOW");
        assert!(
            seen.contains("PING_AFTER_WINDOW"),
            "窗口超时后必须恢复转发，实际：{seen:?}"
        );
        assert!(
            !String::from_utf8_lossy(&opened.pending).contains(ECHO_OFF_LINE),
            "窗口内的注入回显依旧不该进前端"
        );

        runtime().block_on(connection.disconnect()).expect("断开应成功");
    }

    /// 探测不到 bash/zsh 时，开 shell 后一个字节都不写。
    #[test]
    fn unsupported_shell_writes_nothing() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let (port, received) =
            runtime().block_on(spawn_recording_motd_server("WELCOME", "/bin/sh\nsh\n"));

        let connection = SshConnection::connect(&store, &echo_options(port, true))
            .expect("应能连上测试服务端");
        let opened = runtime()
            .block_on(connection.open_shell("xterm-256color", 80, 24))
            .expect("应能在连接上开 shell 通道");
        let mut shell = opened.channel;

        // banner 现在经 pending 交付（探测期间缓冲，一个字节都没吞）
        let pending = String::from_utf8_lossy(&opened.pending).to_string();
        assert!(
            pending.contains("WELCOME"),
            "应照常拿到登录 banner，实际：{pending:?}"
        );
        assert!(
            !pending.contains(ECHO_OFF_LINE) && !pending.contains("__rmx_osc7"),
            "不支持的 shell 不该收到任何注入字节，实际：{pending:?}"
        );
        // 一个字节都没写 -> 通道上也不该再有残留
        let leaked = read_channel_for(&mut shell, Duration::from_millis(200));
        assert!(leaked.is_empty(), "不支持的 shell 不该有任何回显，实际：{leaked:?}");
        // 服务端侧的账本更硬：一个字节都没收到（连 stty 都没碰）
        assert!(
            recorded(&received).is_empty(),
            "不支持的 shell 一个字节都不该发，实际：{:?}",
            recorded(&received)
        );

        runtime().block_on(connection.disconnect()).expect("断开应成功");
    }

    /// 模拟 Ubuntu 真机 motd（`/run/motd.dynamic`）：多行、含空行与对齐的列。
    const UBUNTU_MOTD: &str = concat!(
        "Welcome to Ubuntu 22.04.5 LTS (GNU/Linux 5.15.0-119-generic x86_64)\r\n",
        "\r\n",
        " * Documentation:  https://help.ubuntu.com\r\n",
        " * Management:     https://landscape.canonical.com\r\n",
        " * Support:        https://ubuntu.com/pro\r\n",
        "\r\n",
        "  System information as of Thu Nov  5 11:04:39 UTC 2026\r\n",
        "\r\n",
        "  System load:  0.08               Processes:             231\r\n",
        "  Usage of /:   41.3% of 97.87GB   Users logged in:       1\r\n",
        "\r\n",
        "42 updates can be applied immediately.\r\n",
    );

    /// motd 归首个 session：交互 shell 先开通道，exec 探测后开 ->
    /// pending 里必须能拿到 motd 全文（不只是「Last login + 提示符」）。
    #[test]
    fn open_shell_delivers_full_motd_from_first_session() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        // 这台服务端把 motd 只发给本连接的第一个 session（与 PAM 行为一致）：
        // 探测用的 exec 通道一旦抢占首 session，motd 就彻底丢了
        let (port, received) = runtime().block_on(spawn_first_session_motd_server(
            UBUNTU_MOTD,
            ExecMode::Fixed("/bin/bash\nbash\n"),
        ));

        let connection = SshConnection::connect(&store, &echo_options(port, true))
            .expect("应能连上测试服务端");
        let opened = runtime()
            .block_on(connection.open_shell("xterm-256color", 80, 24))
            .expect("应能在连接上开 shell 通道");

        let pending = String::from_utf8_lossy(&opened.pending).to_string();
        // 逐行比对：出问题时能直接看出丢的是哪一行，而不是只知道「少了点什么」
        for line in UBUNTU_MOTD.lines().filter(|line| !line.trim().is_empty()) {
            assert!(
                pending.contains(line),
                "motd 丢了这一行：{line:?}，实际：{pending:?}"
            );
        }
        // 顺序也要对：motd 必须从第一行开始原样出现
        assert!(
            pending.starts_with(UBUNTU_MOTD),
            "motd 必须完整地从第一行开始出现，实际：{pending:?}"
        );
        // 探测成功且是 bash -> 钩子照常装，但注入痕迹一个字节都不能进 pending
        let sent = recorded(&received);
        assert!(
            sent.contains("__rmx_osc7") && sent.contains("PROMPT_COMMAND"),
            "探测到 bash 时应照常注入钩子，实际：{sent:?}"
        );
        assert!(
            !pending.contains("__rmx_osc7") && !pending.contains(ECHO_OFF_LINE),
            "注入痕迹不能混进 pending，实际：{pending:?}"
        );

        runtime().block_on(connection.disconnect()).expect("断开应成功");
    }

    /// 探测失败（服务端拒 exec）时 motd 仍完整、shell 仍可用、一个字节都不注入。
    #[test]
    fn open_shell_keeps_motd_when_probe_rejected() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let (port, received) = runtime().block_on(spawn_first_session_motd_server(
            UBUNTU_MOTD,
            ExecMode::Reject,
        ));

        let connection = SshConnection::connect(&store, &echo_options(port, true))
            .expect("应能连上测试服务端");
        let opened = runtime()
            .block_on(connection.open_shell("xterm-256color", 80, 24))
            .expect("探测失败也不该让开 shell 失败");
        let mut shell = opened.channel;

        let pending = String::from_utf8_lossy(&opened.pending).to_string();
        assert!(
            pending.starts_with(UBUNTU_MOTD),
            "探测失败时 motd 更要完整，实际：{pending:?}"
        );
        // 探测拿不到结果 -> 当不支持处理 -> 一个字节都不写（先断言账本，再写用户输入）
        assert!(
            recorded(&received).is_empty(),
            "探测失败时不该写任何注入字节，实际：{:?}",
            recorded(&received)
        );

        // shell 本体照常可用：写进去的字节会被回显
        runtime()
            .block_on(async { shell.data(&b"PING_AFTER_PROBE\r"[..]).await })
            .expect("探测失败后 shell 仍应可写");
        let seen = read_channel_until(&mut shell, "PING_AFTER_PROBE");
        assert!(
            seen.contains("PING_AFTER_PROBE"),
            "探测失败后 shell 必须照常转发，实际：{seen:?}"
        );

        runtime().block_on(connection.disconnect()).expect("断开应成功");
    }

    /// 探测成功但 shell 不支持（fish）：motd 完整、一个字节都不注入。
    #[test]
    fn open_shell_keeps_motd_for_unsupported_shell() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let (port, received) = runtime().block_on(spawn_first_session_motd_server(
            UBUNTU_MOTD,
            ExecMode::Fixed("/usr/bin/fish\nfish\n"),
        ));

        let connection = SshConnection::connect(&store, &echo_options(port, true))
            .expect("应能连上测试服务端");
        let opened = runtime()
            .block_on(connection.open_shell("xterm-256color", 80, 24))
            .expect("应能在连接上开 shell 通道");

        let pending = String::from_utf8_lossy(&opened.pending).to_string();
        assert!(
            pending.starts_with(UBUNTU_MOTD),
            "fish 上 motd 同样要完整，实际：{pending:?}"
        );
        assert!(
            !pending.contains(ECHO_OFF_LINE) && !pending.contains("__rmx_osc7"),
            "fish 不该收到任何注入字节，实际：{pending:?}"
        );
        assert!(
            recorded(&received).is_empty(),
            "fish 一个字节都不该发，实际：{:?}",
            recorded(&received)
        );

        runtime().block_on(connection.disconnect()).expect("断开应成功");
    }

    #[test]
    fn exec_returns_command_output() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let port = runtime().block_on(spawn_exec_server("CPU 1.0 MEM 2 1 NET 3.0 4.0\n"));

        let connection = SshConnection::connect(&store, &echo_options(port, true))
            .expect("应能连上测试服务端");

        let output = connection.exec("echo hello", 4096).expect("应能执行命令");
        assert_eq!(
            output.trim(),
            "CPU 1.0 MEM 2 1 NET 3.0 4.0",
            "应拿回服务端写回的那段文本"
        );
    }

    #[test]
    fn exec_truncates_when_output_reaches_limit() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let port = runtime().block_on(spawn_echo_server());

        let connection = SshConnection::connect(&store, &echo_options(port, true))
            .expect("应能连上测试服务端");

        // 服务端把命令原样写回 -> 命令多长输出就多长，上限必须把它截住
        let command = "x".repeat(2048);
        let output = connection.exec(&command, 64).expect("超限不算失败，只是截断");

        assert!(
            output.len() <= 64,
            "输出应被截断到上限内，实际 {} 字节",
            output.len()
        );
        assert!(!output.is_empty(), "应至少收回首段字节");
        assert!(
            command.starts_with(&output),
            "截断应保留前缀而不是乱序拼接"
        );
    }

    #[test]
    fn exec_reports_stderr_and_exit_code() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let port = runtime().block_on(spawn_exec_fail_server());

        let connection = SshConnection::connect(&store, &echo_options(port, true))
            .expect("应能连上测试服务端");

        let err = connection.exec("false", 4096).expect_err("非 0 退出码应报错");
        let text = err.to_string();
        assert!(text.contains("退出码 1"), "错误应带退出码：{text}");
        assert!(
            text.contains("REMOTE_METRICS_FAIL"),
            "错误应带回 stderr 内容：{text}"
        );
    }

    #[test]
    fn samples_remote_metrics_over_exec_channel() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let port = runtime()
            .block_on(spawn_exec_server("CPU 12.5 MEM 16000000 8000000 NET 1.5 2.5\n"));

        let connection = SshConnection::connect(&store, &echo_options(port, true))
            .expect("应能连上测试服务端");

        let remote = crate::metrics_remote::sample_remote(&connection, "demo-host")
            .expect("应能解析远端输出");

        assert_eq!(remote.host, "demo-host");
        assert_eq!(remote.scope, "remote");
        assert!((remote.metrics.cpu_percent - 12.5).abs() < 0.01);
        assert_eq!(remote.metrics.mem_total_mb, 15_625);
        assert_eq!(remote.metrics.mem_used_mb, 7_812);
        assert_eq!(remote.metrics.rx_kbps, 1.5);
        assert_eq!(remote.metrics.tx_kbps, 2.5);
    }

    /// 构造一套带跳板的连接参数。
    ///
    /// 参数：target_port 为目标机端口，jumper_port 为跳板机端口，
    /// accept_target/accept_jumper 为两侧是否已确认过指纹
    /// 核心流程：目标参数照常，jump 里塞一台跳板机（跳板机自己没有 jump）。
    fn jump_options(
        target_port: u16,
        jumper_port: u16,
        accept_target: bool,
        accept_jumper: bool,
    ) -> SshOptions {
        SshOptions {
            jump: Some(Box::new(echo_options(jumper_port, accept_jumper))),
            ..echo_options(target_port, accept_target)
        }
    }

    #[test]
    fn connects_to_target_through_jumper() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        // A = 跳板（能把 direct-tcpip 接到真实端口），B = 目标；两台用不同 banner 区分
        let jumper_port = runtime().block_on(spawn_banner_server("JUMPER"));
        let target_port = runtime().block_on(spawn_banner_server("TARGET"));

        let connection = SshConnection::connect(
            &store,
            &jump_options(target_port, jumper_port, true, true),
        )
        .expect("应能经跳板连到目标");
        let opened = runtime()
            .block_on(connection.open_shell("xterm-256color", 80, 24))
            .expect("应在目标机上开出 shell");
        let mut shell = opened.channel;

        let seen = pending_then_channel(&opened.pending, &mut shell, "TARGET");
        assert!(seen.contains("TARGET"), "shell 应来自目标机，实际：{seen:?}");
        assert!(!seen.contains("JUMPER"), "不应停在跳板机上，实际：{seen:?}");

        // 写入应被目标机回显 -> 通道确实是穿到 B 的
        runtime()
            .block_on(shell.data(&b"PING\n"[..]))
            .expect("写入应成功");
        assert!(
            read_channel_until(&mut shell, "PING").contains("PING"),
            "应收到目标机回显"
        );

        // 两层指纹各按自己 host:port 记账
        let hosts = runtime().block_on(async { store.known_host_list().await }).expect("应能读列表");
        let ports: Vec<u16> = hosts.iter().map(|item| item.port).collect();
        assert!(
            ports.contains(&jumper_port) && ports.contains(&target_port),
            "跳板机与目标机的指纹都应记录，实际：{ports:?}"
        );

        assert!(!connection.is_closed(), "经跳板建立的连接应保持打开");
        runtime().block_on(connection.disconnect()).expect("断开应成功");
    }

    #[test]
    fn connects_through_jumper_with_connect_via() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let jumper_port = runtime().block_on(spawn_banner_server("JUMPER"));
        let target_port = runtime().block_on(spawn_banner_server("TARGET"));

        let jumper = SshConnection::connect(&store, &echo_options(jumper_port, true))
            .expect("跳板机应能直连");
        let target = SshConnection::connect_via(&jumper, &store, &echo_options(target_port, true))
            .expect("应能在跳板通道上完成目标握手");

        // 目标连接内部持有跳板 -> 丢掉本地跳板句柄后目标仍应可用
        drop(jumper);
        let opened = runtime()
            .block_on(target.open_shell("xterm-256color", 80, 24))
            .expect("跳板句柄释放后目标连接仍应可用");
        let mut shell = opened.channel;
        assert!(
            pending_then_channel(&opened.pending, &mut shell, "TARGET").contains("TARGET"),
            "shell 应来自目标机"
        );

        runtime().block_on(target.disconnect()).expect("断开应成功");
    }

    #[test]
    fn reports_target_host_key_unknown_through_jumper() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let jumper_port = runtime().block_on(spawn_banner_server("JUMPER"));
        let target_port = runtime().block_on(spawn_banner_server("TARGET"));

        // 跳板机已确认，目标机未确认 -> 错误必须指向目标机的端口
        let result =
            SshConnection::connect(&store, &jump_options(target_port, jumper_port, false, true));
        match result {
            Err(CoreError::HostKeyUnknown(info)) => {
                assert_eq!(info.port, target_port, "错误应指向目标机而不是跳板机");
                assert!(info.sha256.starts_with("SHA256:"), "应给出 SHA256 指纹");
            }
            Ok(_) => panic!("目标机指纹未记录时应被阻断"),
            Err(err) => panic!("应报目标机未知主机，实际：{err}"),
        }

        // 跳板机的指纹按确认落库，目标机因被阻断而没有落库
        let hosts = runtime().block_on(async { store.known_host_list().await }).expect("应能读列表");
        assert!(
            hosts.iter().any(|item| item.port == jumper_port),
            "跳板机的指纹应已记录，实际：{hosts:?}"
        );
        assert!(
            !hosts.iter().any(|item| item.port == target_port),
            "目标机被阻断时不应落库，实际：{hosts:?}"
        );
    }

    #[test]
    fn reports_jumper_host_key_unknown_before_target() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let jumper_port = runtime().block_on(spawn_banner_server("JUMPER"));
        let target_port = runtime().block_on(spawn_banner_server("TARGET"));

        // 跳板机未确认 -> 连目标之前就得拦下，错误指向跳板机
        let result =
            SshConnection::connect(&store, &jump_options(target_port, jumper_port, true, false));
        match result {
            Err(CoreError::HostKeyUnknown(info)) => {
                assert_eq!(info.port, jumper_port, "错误应指向跳板机");
                assert_ne!(info.port, target_port, "不能在跳板机上报目标的错");
            }
            Ok(_) => panic!("跳板机指纹未记录时应被阻断"),
            Err(err) => panic!("应报跳板机未知主机，实际：{err}"),
        }

        let hosts = runtime().block_on(async { store.known_host_list().await }).expect("应能读列表");
        assert!(hosts.is_empty(), "跳板机未确认时不应留下任何指纹，实际：{hosts:?}");
    }

    #[test]
    fn rejects_nested_jump() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");

        // 跳板机自己又配了跳板 -> 只允许一跳，直接报校验错误
        let mut inner = echo_options(2, true);
        inner.jump = Some(Box::new(echo_options(3, true)));
        let options = SshOptions {
            jump: Some(Box::new(inner)),
            ..echo_options(1, true)
        };

        match SshConnection::connect(&store, &options) {
            Err(CoreError::Validation(message)) => {
                assert!(message.contains("一跳"), "错误应说明只支持一跳，实际：{message}");
            }
            Ok(_) => panic!("多级跳板应被拒绝"),
            Err(err) => panic!("应报校验错误，实际：{err}"),
        }
    }
    #[test]
    fn session_connects_through_jumper() {
        let store = runtime().block_on(async { Store::open_in_memory().await }).expect("内存库");
        let jumper_port = runtime().block_on(spawn_banner_server("JUMPER"));
        let target_port = runtime().block_on(spawn_banner_server("TARGET"));

        // 终端会话这一层：只看输出就能确认穿过了跳板打到目标机
        let (tx, rx) = mpsc::channel();
        let session = SshSession::connect(
            &store,
            &jump_options(target_port, jumper_port, true, true),
            Box::new(move |output| {
                let _ = tx.send(output);
            }),
        )
        .expect("经跳板应能建立终端会话");

        let mut collected = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(crate::terminal::TerminalOutput::Data(bytes)) => {
                    collected.push_str(&String::from_utf8_lossy(&bytes));
                    if collected.contains("TARGET") {
                        break;
                    }
                }
                Ok(crate::terminal::TerminalOutput::Exit(_)) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(
            collected.contains("TARGET"),
            "终端输出应来自目标机，实际：{collected:?}"
        );

        session.close().expect("关闭应成功");
    }
}
