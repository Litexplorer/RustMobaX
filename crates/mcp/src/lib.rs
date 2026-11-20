//! RustMobaX 的 MCP 服务层：把会话库、终端与文件能力封装成 MCP 工具。
//!
//! 本 crate 刻意不依赖 Tauri。宿主能力通过 [`Host`] 注入，桌面壳与将来的
//! 无头模式各实现一份，工具代码只写一遍，也因此在无 GUI 环境下可以被测试。

pub mod ansi;
pub mod audit;
pub mod control;
mod host;
pub mod server;
pub mod terminal_host;
pub mod transport;

pub use async_trait::async_trait;
pub use audit::{McpAudit, NoopAudit, StoreAudit};
pub use control::{McpControl, McpStatus};
pub use host::{
    Host, OpenedTerminal, SshAuthSpec, SshSpec, StoreHost, TerminalChunk, TerminalInfo,
    TerminalSpec,
};
pub use server::MobaxServer;
pub use terminal_host::TerminalHost;
pub use transport::{McpServerConfig, serve, serve_on};
