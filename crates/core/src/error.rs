use serde::Serialize;

/// 核心层统一错误类型。
///
/// 无参数
/// 核心流程：把底层的数据库/IO 错误收敛成对用户可读的中文文案，前端按 kind 分类展示。
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("数据库错误：{0}")]
    Db(#[from] sqlx::Error),
    #[error("文件读写错误：{0}")]
    Io(#[from] std::io::Error),
    #[error("序列化失败：{0}")]
    Serde(#[from] serde_json::Error),
    #[error("校验失败：{0}")]
    Validation(String),
    #[error("未找到：{0}")]
    NotFound(String),
    #[error("导入失败：{0}")]
    Import(String),
    #[error("终端错误：{0}")]
    Terminal(String),
    #[error("SSH 错误：{0}")]
    Ssh(String),
    #[error("Sftp 错误：{0}")]
    Sftp(String),
    #[error("隧道错误：{0}")]
    Tunnel(String),
    #[error("凭据库错误：{0}")]
    Vault(String),
    #[error("未知主机 {host}:{port}，请确认指纹后再连接", host = .0.host, port = .0.port)]
    HostKeyUnknown(crate::model::HostKeyInfo),
    #[error("主机密钥已变更：{host}:{port}，为防中间人攻击已阻断连接", host = .0.host, port = .0.port)]
    HostKeyMismatch(crate::model::HostKeyInfo),
}

/// 前端可见的错误负载。
///
/// 无参数
/// 核心流程：把 CoreError 归类为稳定的 kind 字符串，避免前端解析中文文案做判断。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorPayload {
    pub kind: String,
    pub message: String,
    /// 结构化附加信息，前端按 kind 解析（目前用于主机指纹）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

impl From<CoreError> for ErrorPayload {
    fn from(err: CoreError) -> Self {
        let kind = match &err {
            CoreError::Db(_) => "db",
            CoreError::Io(_) => "io",
            CoreError::Serde(_) => "serde",
            CoreError::Validation(_) => "validation",
            CoreError::NotFound(_) => "notFound",
            CoreError::Import(_) => "import",
            CoreError::Terminal(_) => "terminal",
            CoreError::Ssh(_) => "ssh",
            CoreError::Sftp(_) => "sftp",
            CoreError::Tunnel(_) => "tunnel",
            CoreError::Vault(_) => "vault",
            CoreError::HostKeyUnknown(_) => "hostKeyUnknown",
            CoreError::HostKeyMismatch(_) => "hostKeyMismatch",
        };
        let detail = match &err {
            CoreError::HostKeyUnknown(info) | CoreError::HostKeyMismatch(info) => {
                serde_json::to_value(info).ok()
            }
            _ => None,
        };
        Self {
            kind: kind.to_string(),
            message: err.to_string(),
            detail,
        }
    }
}

pub type CoreResult<T> = Result<T, CoreError>;
