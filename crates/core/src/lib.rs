//! RustMobaX 核心层：会话库、导入导出、本地终端与后续协议实现的落脚点。
//!
//! 本 crate 刻意不依赖 Tauri，桌面壳只做参数转发，
//! 这样会话数据、会话文件解析与 PTY 终端都能在无 GUI 环境里被测试。

mod error;
mod export;
mod fonts;
mod import;
mod metrics;
mod metrics_remote;
mod model;
/// 远程桌面：检测本机的 RDP / VNC 客户端并拼出启动参数，命令层直接用这个命名空间。
pub mod remote_desktop;
/// 串口：枚举本机设备与裸字节收发，命令层直接用这个命名空间。
pub mod serial;
mod sftp;
mod ssh;
mod store;
mod telnet;
mod tunnel;
mod terminal;
mod vault;
/// WSL：枚举发行版与拼接 `wsl.exe` 命令，命令层直接用这个命名空间。
pub mod wsl;

pub use error::{CoreError, CoreResult, ErrorPayload};
pub use export::export_json;
pub use fonts::{system_fonts, FontFamily};
pub use import::{
    apply_import, parse_mxtsessions, ImportPreview, ImportReport, PendingSession, SkippedEntry,
};
pub use metrics::{MetricsSampler, SystemMetrics};
pub use metrics_remote::{disk_usage, sample_remote, DiskUsage, GpuMetrics, RemoteMetrics};
pub use model::{
    default_tunnel_name, AuthKind, Protocol, Session, SessionGroup, SessionInput, TunnelConfig,
    TunnelConfigInput,
};
pub use remote_desktop::{build_launch_args, detect_clients, launch, RemoteDesktopClient};
pub use serial::{SerialOptions, SerialPortInfo, SerialSession};
pub use sftp::{SftpClient, SftpEntry, TransferOutcome, DEFAULT_TRANSFER_CHUNK, PART_SUFFIX};
pub use ssh::{
    ForwardedChannel, SshAuth, SshChannel, SshConnection, SshOptions, SshSession,
};
pub use store::{
    Credential, CustomTheme, KnownHost, Store, THEME_NAME_MAX_BYTES, THEME_PAYLOAD_MAX_BYTES,
};
pub use terminal::{
    default_shell, OutputSink, TerminalControl, TerminalOptions, TerminalOutput, TerminalSession,
};
pub use telnet::{TelnetOptions, TelnetSession};
pub use tunnel::{TunnelHandle, TunnelKind, TunnelSpec};
pub use vault::Vault;
