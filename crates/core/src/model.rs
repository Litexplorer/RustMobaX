use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};

/// 会话协议类型。
///
/// 无参数
/// 核心流程：以稳定字符串落库，序列化给前端时用小写名。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Ssh,
    Sftp,
    Local,
    Telnet,
    Rdp,
    Vnc,
    Serial,
    Wsl,
}

impl Protocol {
    /// 转为落库字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Ssh => "ssh",
            Protocol::Sftp => "sftp",
            Protocol::Local => "local",
            Protocol::Telnet => "telnet",
            Protocol::Rdp => "rdp",
            Protocol::Vnc => "vnc",
            Protocol::Serial => "serial",
            Protocol::Wsl => "wsl",
        }
    }

    /// 从落库字符串还原，未知值返回 None。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "ssh" => Some(Protocol::Ssh),
            "sftp" => Some(Protocol::Sftp),
            "local" => Some(Protocol::Local),
            "telnet" => Some(Protocol::Telnet),
            "rdp" => Some(Protocol::Rdp),
            "vnc" => Some(Protocol::Vnc),
            "serial" => Some(Protocol::Serial),
            "wsl" => Some(Protocol::Wsl),
            _ => None,
        }
    }

    /// 该协议是否必须有主机名。
    ///
    /// 串口没有网络主机语义，WSL 是本地子系统，二者都不需要主机。
    pub fn requires_host(self) -> bool {
        matches!(
            self,
            Protocol::Ssh | Protocol::Sftp | Protocol::Telnet | Protocol::Rdp | Protocol::Vnc
        )
    }
}

/// 凭据获取方式。
///
/// 无参数
/// 核心流程：会话只记方式，真正凭据在 E6 的凭据库中按引用取。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    Password,
    Key,
    Agent,
}

impl AuthKind {
    /// 转为落库字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            AuthKind::Password => "password",
            AuthKind::Key => "key",
            AuthKind::Agent => "agent",
        }
    }

    /// 从落库字符串还原，未知值返回 None。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "password" => Some(AuthKind::Password),
            "key" => Some(AuthKind::Key),
            "agent" => Some(AuthKind::Agent),
            _ => None,
        }
    }
}

/// 会话分组。
///
/// 无参数
/// 核心流程：parent_id 为空表示根分组，sort_order 决定同级顺序。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionGroup {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub sort_order: i64,
}

/// 已保存的会话。
///
/// 无参数
/// 核心流程：承载连接所需的全部非敏感信息，凭据本身不在此结构中。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub group_id: Option<String>,
    pub name: String,
    pub protocol: Protocol,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub auth_kind: AuthKind,
    pub extra: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 新建或更新会话的入参。
///
/// 无参数
/// 核心流程：只含用户可编辑字段，id 与时间戳由存储层生成。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInput {
    pub name: String,
    pub group_id: Option<String>,
    pub protocol: Protocol,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub auth_kind: AuthKind,
    pub extra: Option<String>,
}

impl SessionInput {
    /// 校验必填项。
    ///
    /// 参数：无
    /// 核心流程：名称必填；SSH/SFTP 必须有主机；端口必须大于 0。
    /// 用户名刻意不强制，留空表示沿用远端登录名（对齐 MobaXterm 的可选语义）。
    pub fn validate(&self) -> CoreResult<()> {
        if self.name.trim().is_empty() {
            return Err(CoreError::Validation("会话名称不能为空".into()));
        }
        if self.protocol.requires_host() {
            let host = self.host.as_deref().unwrap_or("").trim();
            if host.is_empty() {
                return Err(CoreError::Validation("该协议必须填写主机".into()));
            }
        }
        if let Some(port) = self.port {
            if port == 0 {
                return Err(CoreError::Validation("端口必须大于 0".into()));
            }
        }
        Ok(())
    }

    /// 取实际生效端口。
    ///
    /// 参数：无
    /// 核心流程：未填端口时按协议给默认值（SSH/SFTP 22、Telnet 23、RDP 3389、VNC 5900），
    /// 用户显式填写的端口优先；本地终端、串口与 WSL 没有网络端口，恒为 None。
    pub fn effective_port(&self) -> Option<u16> {
        match self.protocol {
            Protocol::Ssh | Protocol::Sftp => Some(self.port.unwrap_or(22)),
            Protocol::Telnet => Some(self.port.unwrap_or(23)),
            Protocol::Rdp => Some(self.port.unwrap_or(3389)),
            Protocol::Vnc => Some(self.port.unwrap_or(5900)),
            Protocol::Local | Protocol::Serial | Protocol::Wsl => None,
        }
    }
}

/// 已保存的隧道配置。
///
/// 无参数
/// 核心流程：只存"接到哪个会话、怎么转发"的意图；运行中的隧道在应用注册表里，
/// 进程重启后按这些配置重新拉起。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelConfig {
    pub id: String,
    pub name: String,
    /// 隧道借用的会话标识，连接参数（主机、认证）都从该会话取。
    pub session_id: String,
    /// local / remote / dynamic。
    pub kind: String,
    /// 监听地址，空串表示默认 127.0.0.1。
    pub listen_host: String,
    /// 监听端口，0 表示让系统或远端自动分配。
    pub listen_port: u16,
    /// 目标地址，dynamic 类型可为空。
    pub target_host: String,
    /// 目标端口，dynamic 类型可为 0。
    pub target_port: u16,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 新建或更新隧道配置的入参。
///
/// 无参数
/// 核心流程：只含用户可编辑字段，id 与时间戳由存储层生成；
/// 各字段给默认值，前端少传一个转发字段也不会整条请求反序列化失败。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelConfigInput {
    #[serde(default)]
    pub name: String,
    pub session_id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub listen_host: String,
    #[serde(default)]
    pub listen_port: u16,
    #[serde(default)]
    pub target_host: String,
    #[serde(default)]
    pub target_port: u16,
}

impl TunnelConfigInput {
    /// 校验必填项。
    ///
    /// 参数：无
    /// 核心流程：类型必须是三种转发之一，且必须挂在一个已存在的会话上；
    /// 目标地址的有无留给启动时由 TunnelSpec 校验，这里不重复判断。
    pub fn validate(&self) -> CoreResult<()> {
        if !matches!(self.kind.trim(), "local" | "remote" | "dynamic") {
            return Err(CoreError::Validation(format!(
                "隧道类型只能是 local、remote 或 dynamic，收到 {}",
                self.kind
            )));
        }
        if self.session_id.trim().is_empty() {
            return Err(CoreError::Validation("隧道必须选择所属会话".into()));
        }
        Ok(())
    }

    /// 取实际生效的名称。
    ///
    /// 参数：无
    /// 核心流程：用户填了就用用户的，没填按转发关系生成描述，
    /// 避免列表里出现一列空名字（前端不传 name 也能用）。
    pub fn effective_name(&self) -> String {
        let name = self.name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
        default_tunnel_name(
            self.kind.trim(),
            &self.listen_host,
            self.listen_port,
            &self.target_host,
            self.target_port,
        )
    }
}

/// 按转发关系生成隧道描述。
///
/// 参数：kind 为隧道类型，listen_host/listen_port 为监听点，
/// target_host/target_port 为目标（dynamic 忽略目标）
/// 核心流程：本地与远程给出「方向 端口 → 目标:端口」，动态转发给出
/// SOCKS5 监听点；端口 0 表示自动分配。
pub fn default_tunnel_name(
    kind: &str,
    listen_host: &str,
    listen_port: u16,
    target_host: &str,
    target_port: u16,
) -> String {
    let port = if listen_port == 0 {
        "自动".to_string()
    } else {
        listen_port.to_string()
    };
    if kind == "dynamic" {
        let host = listen_host.trim();
        let host = if host.is_empty() { "127.0.0.1" } else { host };
        return format!("动态 SOCKS5 {host}:{port}");
    }
    let label = if kind == "remote" { "远程" } else { "本地" };
    format!("{label} {port} → {target_host}:{target_port}")
}

/// 当前时间戳，单位毫秒。
///
/// 无参数
/// 核心流程：从系统时钟取 Unix 时间，失败时退化为 0。
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 一次连接中观察到的主机密钥信息。
///
/// 无参数
/// 核心流程：同时给出 SHA256 与 MD5 两种指纹，供用户按习惯核对，
/// 也用于"未知主机"弹窗与"密钥变更"告警。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKeyInfo {
    pub host: String,
    pub port: u16,
    pub key_type: String,
    pub sha256: String,
    pub md5: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造只关心协议与端口的会话入参。
    fn input(protocol: Protocol, port: Option<u16>) -> SessionInput {
        SessionInput {
            name: "测试会话".to_string(),
            group_id: None,
            protocol,
            host: None,
            port,
            username: None,
            auth_kind: AuthKind::Password,
            extra: None,
        }
    }

    /// 新增的五个协议必须能原样往返落库字符串。
    #[test]
    fn new_protocols_roundtrip_as_str_and_parse() {
        let cases = [
            (Protocol::Telnet, "telnet"),
            (Protocol::Rdp, "rdp"),
            (Protocol::Vnc, "vnc"),
            (Protocol::Serial, "serial"),
            (Protocol::Wsl, "wsl"),
        ];
        for (protocol, raw) in cases {
            assert_eq!(protocol.as_str(), raw);
            assert_eq!(Protocol::parse(raw), Some(protocol));
        }
    }

    /// 原有三个协议的映射不能被新变体带偏。
    #[test]
    fn legacy_protocols_roundtrip_unchanged() {
        let cases = [
            (Protocol::Ssh, "ssh"),
            (Protocol::Sftp, "sftp"),
            (Protocol::Local, "local"),
        ];
        for (protocol, raw) in cases {
            assert_eq!(protocol.as_str(), raw);
            assert_eq!(Protocol::parse(raw), Some(protocol));
        }
    }

    /// 只有网络协议要求主机；串口与 WSL 是本地语义。
    #[test]
    fn requires_host_matches_protocol_semantics() {
        assert!(Protocol::Ssh.requires_host());
        assert!(Protocol::Sftp.requires_host());
        assert!(Protocol::Telnet.requires_host());
        assert!(Protocol::Rdp.requires_host());
        assert!(Protocol::Vnc.requires_host());
        assert!(!Protocol::Local.requires_host());
        assert!(!Protocol::Serial.requires_host());
        assert!(!Protocol::Wsl.requires_host());
    }

    /// 未填端口时按协议给默认值，本地类协议无端口。
    #[test]
    fn effective_port_uses_protocol_defaults() {
        assert_eq!(input(Protocol::Ssh, None).effective_port(), Some(22));
        assert_eq!(input(Protocol::Sftp, None).effective_port(), Some(22));
        assert_eq!(input(Protocol::Telnet, None).effective_port(), Some(23));
        assert_eq!(input(Protocol::Rdp, None).effective_port(), Some(3389));
        assert_eq!(input(Protocol::Vnc, None).effective_port(), Some(5900));
        assert_eq!(input(Protocol::Local, None).effective_port(), None);
        assert_eq!(input(Protocol::Serial, None).effective_port(), None);
        assert_eq!(input(Protocol::Wsl, None).effective_port(), None);
    }

    /// 用户显式填写的端口优先于协议默认值。
    #[test]
    fn effective_port_prefers_explicit_port() {
        assert_eq!(input(Protocol::Ssh, Some(2222)).effective_port(), Some(2222));
        assert_eq!(input(Protocol::Telnet, Some(2323)).effective_port(), Some(2323));
        assert_eq!(input(Protocol::Rdp, Some(13389)).effective_port(), Some(13389));
        assert_eq!(input(Protocol::Vnc, Some(5901)).effective_port(), Some(5901));
    }

    /// 未知协议字符串仍然报错，不做兜底。
    #[test]
    fn unknown_protocol_parse_fails() {
        assert_eq!(Protocol::parse("gopher"), None);
        assert_eq!(Protocol::parse(""), None);
        assert_eq!(Protocol::parse("SSH"), None);
    }
}
