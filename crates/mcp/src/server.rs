//! MCP 服务端实现：把宿主能力暴露成 MCP 工具、资源与提示模板。
//!
//! 这一层只认 [`Host`] 抽象，不认 Tauri、不认数据库，也不认 HTTP 传输。当前挂一个探针、
//! 会话与隧道的查询工具、终端的开关读写缩放五件套、会话 / 分组 / 设置三组写工具，
//! 另加一组只读资源与三个排查模板，让客户端能直接读上下文而不用多花一次工具调用。

use std::sync::Arc;
use std::time::Instant;

use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    handler::server::{
        router::tool::ToolRouter,
        tool::ToolCallContext,
        wrapper::{Json, Parameters},
    },
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
        GetPromptRequestParams, GetPromptResponse, GetPromptResult, Implementation, JsonObject,
        ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult,
        PaginatedRequestParams, Prompt, PromptArgument, PromptMessage, ReadResourceRequestParams,
        ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, ResourceTemplate,
        Role, ServerCapabilities, ServerConfig,
    },
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use rustmobax_core::{AuthKind, CoreError, Protocol, Session, SessionGroup, SessionInput, TunnelConfig};
use serde::{Deserialize, Serialize};

use crate::Host;
use crate::audit::{McpAudit, digest_tool_arguments};
use crate::host::{TerminalInfo, TerminalSpec};

/// 探针工具的输出。
///
/// 无参数
/// 核心流程：固定的一句话加上宿主里的会话总数，字段名与取值都直接序列化给客户端，
/// 便于人工确认服务端读到的是真实会话库而不是空实现。
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct PingOutput {
    /// 固定的一句话，表明服务端已完成一次完整的工具调用。
    pub message: String,
    /// 宿主当前持有的会话总数。
    pub session_count: usize,
}

/// 会话列表工具的入参。
///
/// 参数：keyword 为可选关键词，缺省视为空串
/// 核心流程：只承载一个过滤条件，序列化规则与协议保持一致。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionListParams {
    /// 关键词，缺省或空串表示不过滤。
    pub keyword: Option<String>,
}

/// 会话详情工具的入参。
///
/// 参数：id 为会话标识
/// 核心流程：只承载定位条件，空串在工具里直接按参数错误拒绝。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionGetParams {
    /// 会话标识。
    pub id: String,
}

/// 隧道列表工具的入参。
///
/// 参数：keyword 为可选关键词，limit 为每页条数，offset 为起始偏移
/// 核心流程：只承载过滤与分页条件，缺省值在工具里统一归一，序列化规则与协议一致。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TunnelListParams {
    /// 关键词，缺省或空白表示不过滤。
    pub keyword: Option<String>,
    /// 每页条数，缺省 20，上限 200，0 视为使用默认值。
    pub limit: Option<u32>,
    /// 起始偏移，缺省 0。
    pub offset: Option<u32>,
}

/// 新建分组工具的入参。
///
/// 参数：name 为分组名，parentId 为父分组标识
/// 核心流程：父分组缺省表示建在根下，名称非空由工具层先挡一道。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GroupCreateParams {
    /// 分组名称。
    pub name: String,
    /// 父分组标识，缺省表示根分组。
    pub parent_id: Option<String>,
}

/// 重命名分组工具的入参。
///
/// 参数：id 为分组标识，name 为新名称
/// 核心流程：只承载定位条件与新名字，其余字段由存储层保持原样。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GroupRenameParams {
    /// 分组标识。
    pub id: String,
    /// 新的分组名称。
    pub name: String,
}

/// 删除分组工具的入参。
///
/// 参数：id 为分组标识
/// 核心流程：只承载定位条件，删除的级联语义由存储层决定。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GroupDeleteParams {
    /// 分组标识。
    pub id: String,
}

/// 移动分组工具的入参。
///
/// 参数：id 为分组标识，parentId 为新的父分组
/// 核心流程：父分组缺省表示挪到根下；成环由存储层拒绝。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GroupMoveParams {
    /// 分组标识。
    pub id: String,
    /// 新的父分组标识，缺省表示根分组。
    pub parent_id: Option<String>,
}

/// 新建会话工具的入参。
///
/// 参数：name 为会话名，protocol 为协议，authKind 为凭据方式，其余为可选连接信息
/// 核心流程：协议与凭据方式都用落库字符串传，解析失败由工具层回参数错误；
/// groupId 缺省表示不分组。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionCreateParams {
    /// 会话名称。
    pub name: String,
    /// 协议，取值 ssh、sftp、local、telnet、rdp、vnc、serial、wsl。
    pub protocol: String,
    /// 主机名或 IP，需要主机的协议必填。
    pub host: Option<String>,
    /// 端口，缺省按协议默认值。
    pub port: Option<u16>,
    /// 登录用户名。
    pub username: Option<String>,
    /// 凭据方式，取值 password、key、agent。
    pub auth_kind: String,
    /// 所属分组标识，缺省表示不分组。
    pub group_id: Option<String>,
    /// 附加参数，原样透传。
    pub extra: Option<String>,
}

/// 更新会话工具的入参。
///
/// 参数：id 为会话标识，其余字段与新建会话一致
/// 核心流程：整份字段覆盖，不做字段级合并；缺省字段会被写成空而不是保留旧值。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdateParams {
    /// 会话标识。
    pub id: String,
    /// 会话名称。
    pub name: String,
    /// 协议，取值 ssh、sftp、local、telnet、rdp、vnc、serial、wsl。
    pub protocol: String,
    /// 主机名或 IP，需要主机的协议必填。
    pub host: Option<String>,
    /// 端口，缺省按协议默认值。
    pub port: Option<u16>,
    /// 登录用户名。
    pub username: Option<String>,
    /// 凭据方式，取值 password、key、agent。
    pub auth_kind: String,
    /// 所属分组标识，缺省表示不分组。
    pub group_id: Option<String>,
    /// 附加参数，原样透传。
    pub extra: Option<String>,
}

/// 删除会话工具的入参。
///
/// 参数：id 为会话标识
/// 核心流程：只承载定位条件，删除不可恢复。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionDeleteParams {
    /// 会话标识。
    pub id: String,
}

/// 移动会话工具的入参。
///
/// 参数：id 为会话标识，groupId 为目标分组
/// 核心流程：groupId 缺省表示移出分组，会话其余字段不变。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionMoveParams {
    /// 会话标识。
    pub id: String,
    /// 目标分组标识，缺省表示未分组。
    pub group_id: Option<String>,
}

/// 复制会话工具的入参。
///
/// 参数：id 为被复制的会话标识
/// 核心流程：只承载定位条件，副本命名由存储层派生。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionCopyParams {
    /// 会话标识。
    pub id: String,
}

/// 写入设置工具的入参。
///
/// 参数：key 为设置键，value 为设置值
/// 核心流程：`mcp.` 开头的键由宿主直接拒绝，工具层不重复判断前缀。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SettingsSetParams {
    /// 设置键名。
    pub key: String,
    /// 设置值，统一按字符串写库。
    pub value: String,
}

/// 终端读取工具的入参。
///
/// 参数：id 为终端标识，cursor 为游标，max 为单次最多字节数，plain 为是否剥离控制序列
/// 核心流程：三个可选字段各带缺省值，客户端只传 id 也能正常读。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalReadParams {
    /// 终端标识。
    pub id: String,
    /// 上次返回的 next，缺省 0 表示从头读。
    #[serde(default)]
    pub cursor: u64,
    /// 本次最多取的字节数，缺省 8192，上限 65536。
    #[serde(default = "default_read_max")]
    pub max: usize,
    /// 是否剥离 ANSI 控制序列，缺省为真。
    #[serde(default = "default_plain")]
    pub plain: bool,
}

/// 缺省的终端读取字节数。
///
/// 无参数
/// 核心流程：固定 8 KiB，刚好一屏多一点的量，避免一次拉回整段历史。
fn default_read_max() -> usize {
    8192
}

/// 缺省的 ANSI 剥离开关。
///
/// 无参数
/// 核心流程：默认剥离，模型拿到的是干净文本；需要原样字节时显式传 false。
fn default_plain() -> bool {
    true
}

/// 终端写入工具的入参。
///
/// 参数：id 为终端标识，data 为要写入的文本
/// 核心流程：数据原样转发，需要回车时由调用方在 data 里自带换行。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalWriteParams {
    /// 终端标识。
    pub id: String,
    /// 要写入终端的文本。
    pub data: String,
}

/// 终端缩放工具的入参。
///
/// 参数：id 为终端标识，cols 为列数，rows 为行数
/// 核心流程：尺寸按原值下发，非零校验交给宿主与终端实现。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalResizeParams {
    /// 终端标识。
    pub id: String,
    /// 列数。
    pub cols: u16,
    /// 行数。
    pub rows: u16,
}

/// 终端关闭工具的入参。
///
/// 参数：id 为终端标识
/// 核心流程：只承载定位条件，关闭后终端即从注册表消失。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCloseParams {
    /// 终端标识。
    pub id: String,
}

/// 终端执行命令工具的入参。
///
/// 参数：id 为终端标识，command 为要执行的命令，maxBytes 为最多取回的字节数
/// 核心流程：只承载一次性的命令与输出上限，缺省上限按 64 KiB 处理。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalExecParams {
    /// 终端标识。
    pub id: String,
    /// 要执行的命令。
    pub command: String,
    /// 本次最多取回的字节数，缺省 65536，上限 1048576。
    #[serde(default = "default_exec_max")]
    pub max_bytes: usize,
}

/// 缺省的终端命令输出上限。
///
/// 无参数
/// 核心流程：固定 64 KiB，够绝大多数一次性命令的 stdout，又不至于把上下文撑爆。
fn default_exec_max() -> usize {
    65536
}

/// 对外输出的会话视图。
///
/// 无参数
/// 核心流程：core 的 [`Session`] 没有 JSON Schema 能力，这里镜像一份用于生成 MCP 的
/// 输出契约；字段与 core 一一对应，枚举落成它既有的小写字符串，保证两边 JSON 一致。
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    /// 会话标识。
    pub id: String,
    /// 所属分组标识，未分组时为空。
    pub group_id: Option<String>,
    /// 会话名称。
    pub name: String,
    /// 协议类型，取 core 的小写字符串。
    pub protocol: String,
    /// 主机地址，本地会话为空。
    pub host: Option<String>,
    /// 端口，缺省时由协议决定。
    pub port: Option<u16>,
    /// 登录用户名。
    pub username: Option<String>,
    /// 凭据方式，取 core 的小写字符串。
    pub auth_kind: String,
    /// 附加参数，原样透传。
    pub extra: Option<String>,
    /// 创建时间，Unix 秒。
    pub created_at: i64,
    /// 更新时间，Unix 秒。
    pub updated_at: i64,
}

impl From<Session> for SessionView {
    /// 把 core 的会话转成对外视图。
    ///
    /// 参数：session 为 core 的会话结构
    /// 核心流程：逐字段搬移，协议与凭据方式取 `as_str` 的小写名，不改变任何取值。
    fn from(session: Session) -> Self {
        Self {
            id: session.id,
            group_id: session.group_id,
            name: session.name,
            protocol: session.protocol.as_str().to_string(),
            host: session.host,
            port: session.port,
            username: session.username,
            auth_kind: session.auth_kind.as_str().to_string(),
            extra: session.extra,
            created_at: session.created_at,
            updated_at: session.updated_at,
        }
    }
}

/// 对外输出的隧道视图。
///
/// 无参数
/// 核心流程：与会话视图同理，镜像 core 的 [`TunnelConfig`] 以生成输出契约，
/// 只描述配置意图，不含运行状态。
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TunnelView {
    /// 隧道标识。
    pub id: String,
    /// 隧道名称。
    pub name: String,
    /// 借用的会话标识。
    pub session_id: String,
    /// 转发类型，取 core 的原始字符串。
    pub kind: String,
    /// 监听地址，空串表示默认回环。
    pub listen_host: String,
    /// 监听端口，0 表示自动分配。
    pub listen_port: u16,
    /// 目标地址，动态转发可为空。
    pub target_host: String,
    /// 目标端口，动态转发可为 0。
    pub target_port: u16,
    /// 会话建立后是否自动拉起。
    pub auto_start: bool,
    /// 创建时间，Unix 秒。
    pub created_at: i64,
    /// 更新时间，Unix 秒。
    pub updated_at: i64,
}

impl From<TunnelConfig> for TunnelView {
    /// 把 core 的隧道配置转成对外视图。
    ///
    /// 参数：tunnel 为 core 的隧道配置
    /// 核心流程：逐字段搬移，不做单位换算也不补默认值，缺省语义保留原样。
    fn from(tunnel: TunnelConfig) -> Self {
        Self {
            id: tunnel.id,
            name: tunnel.name,
            session_id: tunnel.session_id,
            kind: tunnel.kind,
            listen_host: tunnel.listen_host,
            listen_port: tunnel.listen_port,
            target_host: tunnel.target_host,
            target_port: tunnel.target_port,
            auto_start: tunnel.auto_start,
            created_at: tunnel.created_at,
            updated_at: tunnel.updated_at,
        }
    }
}

/// 会话列表工具的输出。
///
/// 无参数
/// 核心流程：总数与数组一起给，客户端不必再数一遍。
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionListOutput {
    /// 本次返回的会话条数。
    pub count: usize,
    /// 会话数组。
    pub sessions: Vec<SessionView>,
}

/// 隧道列表工具的输出。
///
/// 参数：无
/// 核心流程：过滤后的总数与本页数组一起给，offset 与 limit 回显实际生效值，
/// 客户端不必自己记上一页的入参。
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TunnelListOutput {
    /// 过滤后的隧道总数。
    pub total: usize,
    /// 本页返回的隧道条数。
    pub count: usize,
    /// 本页起始偏移，回显实际生效值。
    pub offset: u32,
    /// 每页条数，回显实际生效值。
    pub limit: u32,
    /// 本页隧道数组。
    pub tunnels: Vec<TunnelView>,
}

/// 对外输出的分组视图。
///
/// 无参数
/// 核心流程：core 的 [`SessionGroup`] 没有 JSON Schema 能力，这里镜像一份用于生成
/// 输出契约；字段与 core 一一对应，父级缺省表示根分组。
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GroupView {
    /// 分组标识。
    pub id: String,
    /// 父分组标识，根分组为空。
    pub parent_id: Option<String>,
    /// 分组名称。
    pub name: String,
    /// 同级排序值。
    pub sort_order: i64,
}

impl From<SessionGroup> for GroupView {
    /// 把 core 的分组转成对外视图。
    ///
    /// 参数：group 为 core 的分组结构
    /// 核心流程：逐字段搬移，不重排也不改顺序值。
    fn from(group: SessionGroup) -> Self {
        Self {
            id: group.id,
            parent_id: group.parent_id,
            name: group.name,
            sort_order: group.sort_order,
        }
    }
}

/// 一条应用设置的对外视图。
///
/// 无参数
/// 核心流程：键值都按字符串原样给出，不猜测类型也不补默认值。
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SettingView {
    /// 设置键名。
    pub key: String,
    /// 设置值。
    pub value: String,
}

/// 设置列表工具的输出。
///
/// 无参数
/// 核心流程：总数与数组一起给，数组按 key 升序，客户端不必再排一次。
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SettingsListOutput {
    /// 本次返回的设置条数。
    pub count: usize,
    /// 设置数组，按 key 升序。
    pub settings: Vec<SettingView>,
}

/// 隧道列表的默认每页条数。
const TUNNEL_PAGE_DEFAULT: u32 = 20;
/// 隧道列表的每页条数上限。
const TUNNEL_PAGE_MAX: u32 = 200;

/// 把入参里的每页条数归一成实际生效值。
///
/// 参数：limit 为客户端传入的每页条数
/// 核心流程：缺省与 0 都按默认 20 处理，超过上限按 200 截断，其余原样使用。
fn tunnel_page_limit(limit: Option<u32>) -> u32 {
    match limit {
        None | Some(0) => TUNNEL_PAGE_DEFAULT,
        Some(value) => value.min(TUNNEL_PAGE_MAX),
    }
}

/// 取转发类型的中文别名。
///
/// 参数：kind 为已转小写的转发类型
/// 核心流程：只认 local、remote、dynamic 三种，其余返回空串，让关键词不会误命中。
fn tunnel_kind_alias(kind: &str) -> &'static str {
    match kind {
        "local" => "本地",
        "remote" => "远程",
        "dynamic" => "动态",
        _ => "",
    }
}

/// 判断一条隧道配置是否命中关键词。
///
/// 参数：tunnel 为待判断的隧道配置，keyword 为已去首尾空白并转小写的关键词
/// 核心流程：空关键词直接通过；否则把名称、类型英文与中文别名、监听连接串与端口、
/// 目标连接串与端口、会话标识都转小写后逐个做包含匹配，任一项命中即通过。
fn tunnel_matches(tunnel: &TunnelConfig, keyword: &str) -> bool {
    if keyword.is_empty() {
        return true;
    }
    let kind = tunnel.kind.to_lowercase();
    let listen_port = tunnel.listen_port.to_string();
    let target_port = tunnel.target_port.to_string();
    let candidates = [
        tunnel.name.to_lowercase(),
        kind.clone(),
        tunnel_kind_alias(&kind).to_string(),
        format!("{}:{listen_port}", tunnel.listen_host.to_lowercase()),
        listen_port,
        format!("{}:{target_port}", tunnel.target_host.to_lowercase()),
        target_port,
        tunnel.session_id.to_lowercase(),
    ];

    candidates
        .iter()
        .any(|candidate| candidate.contains(keyword))
}

/// 把结构化输出包装成工具结果。
///
/// 参数：value 为待返回的结构体
/// 核心流程：先序列化成 JSON 值，保持结构化内容与文本内容一致；序列化失败属于服务端
/// 问题，转成 JSON-RPC 内部错误。
fn structured_output<T: Serialize>(value: T) -> Result<CallToolResult, ErrorData> {
    let value = serde_json::to_value(value)
        .map_err(|error| ErrorData::internal_error(format!("结果序列化失败：{error}"), None))?;
    Ok(CallToolResult::structured(value))
}

/// 把宿主错误映射成工具返回值。
///
/// 参数：error 为宿主返回的错误
/// 核心流程：入参在存储层被否掉属于参数问题，返回 JSON-RPC 的 invalid_params；会话不存在
/// 属于业务问题，返回 is_error 的工具结果，正文用中文写明原因。两类不混用，业务失败不占
/// 协议错误码，客户端仍能拿到一次正常的工具响应。
fn host_error(error: CoreError) -> Result<CallToolResult, ErrorData> {
    match error {
        CoreError::Validation(message) => Err(ErrorData::invalid_params(message, None)),
        CoreError::NotFound(id) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
            "会话不存在：{id}"
        ))])),
        other => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
            "宿主操作失败：{other}"
        ))])),
    }
}

/// 把终端相关的宿主错误映射成工具返回值。
///
/// 参数：error 为宿主返回的错误
/// 核心流程：入参不合法仍走 invalid_params；终端不存在与终端操作失败都返回 is_error
/// 的工具结果，正文用中文写明是哪个终端，客户端不需要解析错误码。
fn terminal_error(error: CoreError) -> Result<CallToolResult, ErrorData> {
    match error {
        CoreError::Validation(message) => Err(ErrorData::invalid_params(message, None)),
        CoreError::NotFound(id) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
            "终端不存在：{id}"
        ))])),
        other => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
            "终端操作失败：{other}"
        ))])),
    }
}

/// 把分组相关的宿主错误映射成工具返回值。
///
/// 参数：error 为宿主返回的错误
/// 核心流程：与终端同构，只是文案换成分组；入参非法仍走 invalid_params，
/// 分组不存在与其余失败返回 is_error 的工具结果。
fn group_error(error: CoreError) -> Result<CallToolResult, ErrorData> {
    match error {
        CoreError::Validation(message) => Err(ErrorData::invalid_params(message, None)),
        CoreError::NotFound(id) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
            "分组不存在：{id}"
        ))])),
        other => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
            "分组操作失败：{other}"
        ))])),
    }
}

/// 把字符串协议解析成 core 的枚举。
///
/// 参数：raw 为客户端传来的协议字符串
/// 核心流程：大小写敏感地按落库字符串匹配，失败时把合法取值一并写进错误正文，
/// 让调用方一次就能改对，而不是反复试错。
fn parse_protocol(raw: &str) -> Result<Protocol, ErrorData> {
    Protocol::parse(raw.trim()).ok_or_else(|| {
        ErrorData::invalid_params(
            format!(
                "protocol 取值非法：{raw}，合法取值为 ssh、sftp、local、telnet、rdp、vnc、serial、wsl"
            ),
            None,
        )
    })
}

/// 把字符串凭据方式解析成 core 的枚举。
///
/// 参数：raw 为客户端传来的凭据方式字符串
/// 核心流程：按落库字符串匹配，失败时把 password、key、agent 三个合法取值写进
/// 错误正文。
fn parse_auth_kind(raw: &str) -> Result<AuthKind, ErrorData> {
    AuthKind::parse(raw.trim()).ok_or_else(|| {
        ErrorData::invalid_params(
            format!("authKind 取值非法：{raw}，合法取值为 password、key、agent"),
            None,
        )
    })
}

/// 把可选字符串去掉首尾空白并把空串归一成 None。
///
/// 参数：value 为原始可选字符串
/// 核心流程：先借用再裁剪，空串按「没填」处理，避免把空白写进库里。
fn tidy(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// 用工具层的扁平字段拼出 core 的会话入参。
///
/// 参数：name 为会话名，protocol 与 auth_kind 为落库字符串，host、port、username、
/// group_id、extra 为可选连接信息
/// 核心流程：协议与凭据方式先解析，非法值立刻回 invalid_params；其余字符串去空白后
/// 空串归一成 None，端口原样透传，最终字段与 [`SessionInput`] 一一对应。
fn session_input(
    name: &str,
    protocol: &str,
    host: Option<String>,
    port: Option<u16>,
    username: Option<String>,
    auth_kind: &str,
    group_id: Option<String>,
    extra: Option<String>,
) -> Result<SessionInput, ErrorData> {
    Ok(SessionInput {
        name: name.trim().to_string(),
        group_id: tidy(group_id),
        protocol: parse_protocol(protocol)?,
        host: tidy(host),
        port,
        username: tidy(username),
        auth_kind: parse_auth_kind(auth_kind)?,
        extra: tidy(extra),
    })
}

/// RustMobaX 的 MCP 服务端。
///
/// 无参数
/// 核心流程：持有宿主与工具路由表。宿主是 trait 对象，桌面壳与无头模式各注入一份
/// 实现；路由表在构造时由宏生成，之后只读，因此每个连接重建一个实例的代价很小。
pub struct MobaxServer {
    host: Arc<dyn Host>,
    audit: Arc<dyn McpAudit>,
    tool_router: ToolRouter<Self>,
}

impl MobaxServer {
    /// 用给定宿主与审计实现构造服务端。
    ///
    /// 参数：host 为宿主能力，通常是 `Arc<StoreHost>` 或桌面壳的适配实现；
    /// audit 为审计落地方式，桌面壳传落库实现，测试传内存实现
    /// 核心流程：保存两个引用并生成工具路由表，不发起任何 IO。
    pub fn new(host: Arc<dyn Host>, audit: Arc<dyn McpAudit>) -> Self {
        Self {
            host,
            audit,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl MobaxServer {
    /// 探测工具：证明传输层、服务端与宿主三段是连通的。
    ///
    /// 参数：无入参
    /// 核心流程：向宿主要一次不带关键词的会话列表，取长度作为会话总数，连同固定
    /// 文案一起作为结构化结果返回。宿主报错时原样转成 MCP 的内部错误。
    #[tool(
        description = "探测 MCP 链路是否连通，返回固定文案与当前会话总数",
        annotations(title = "探测服务", read_only_hint = true)
    )]
    async fn server_ping(&self) -> Result<Json<PingOutput>, ErrorData> {
        let sessions = self
            .host
            .list_sessions("")
            .await
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;

        Ok(Json(PingOutput {
            message: "pong from RustMobaX MCP".to_string(),
            session_count: sessions.len(),
        }))
    }

    /// 会话列表工具：按关键词列出已保存的会话。
    ///
    /// 参数：keyword 为可选关键词，缺省视为空串
    /// 核心流程：把关键词交给宿主做模糊匹配，结果整体转成对外视图后连同总数一起返回；
    /// 宿主报错交给统一的错误映射处理。
    #[tool(
        description = "查询已保存的会话，返回会话总数与每条会话的连接信息",
        output_schema = rmcp::handler::server::tool::schema_for_output::<SessionListOutput>(),
        annotations(
            title = "列出会话",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn session_list(
        &self,
        Parameters(params): Parameters<SessionListParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let keyword = params.keyword.unwrap_or_default();
        let sessions = match self.host.list_sessions(&keyword).await {
            Ok(sessions) => sessions,
            Err(error) => return host_error(error),
        };

        let sessions: Vec<SessionView> = sessions.into_iter().map(SessionView::from).collect();
        let count = sessions.len();
        structured_output(SessionListOutput { count, sessions })
    }

    /// 会话详情工具：按标识取单个会话。
    ///
    /// 参数：id 为会话标识
    /// 核心流程：空标识直接按参数错误拒绝，不打扰宿主；其余情况转交宿主，命中后返回
    /// 单条会话，未命中由统一映射转成业务错误。
    #[tool(
        description = "按标识查询单个会话，返回该会话的完整连接信息",
        output_schema = rmcp::handler::server::tool::schema_for_output::<SessionView>(),
        annotations(
            title = "查询会话",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn session_get(
        &self,
        Parameters(params): Parameters<SessionGetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = params.id.trim();
        if id.is_empty() {
            return Err(ErrorData::invalid_params("会话标识不能为空", None));
        }

        let session = match self.host.get_session(id).await {
            Ok(session) => session,
            Err(error) => return host_error(error),
        };

        structured_output(SessionView::from(session))
    }

    /// 隧道列表工具：按关键词过滤并分页列出隧道配置。
    ///
    /// 参数：keyword 为可选关键词，limit 为每页条数，offset 为起始偏移
    /// 核心流程：向宿主取全部配置后在内存里按名称、类型、监听、目标与会话过滤，
    /// 过滤结果再按 offset 与 limit 切片；total 是过滤后的总数，count 是本页条数，
    /// offset 越过尾部时本页为空但不报错；运行状态不在配置里，工具也不去猜。
    #[tool(
        description = "查询已配置的隧道转发，支持按名称/类型/监听/目标/会话过滤，并支持分页；返回 total 为过滤后总数，count 为本页条数，offset 与 limit 回显实际生效值",
        output_schema = rmcp::handler::server::tool::schema_for_output::<TunnelListOutput>(),
        annotations(
            title = "列出隧道",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn tunnel_list(
        &self,
        Parameters(params): Parameters<TunnelListParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let tunnels = match self.host.list_tunnel_configs().await {
            Ok(tunnels) => tunnels,
            Err(error) => return host_error(error),
        };

        let keyword = params.keyword.unwrap_or_default().trim().to_lowercase();
        let limit = tunnel_page_limit(params.limit);
        let offset = params.offset.unwrap_or(0);

        let tunnels: Vec<TunnelView> = tunnels
            .into_iter()
            .filter(|tunnel| tunnel_matches(tunnel, &keyword))
            .map(TunnelView::from)
            .collect();
        let total = tunnels.len();
        let page: Vec<TunnelView> = tunnels
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect();
        let count = page.len();

        structured_output(TunnelListOutput {
            total,
            count,
            offset,
            limit,
            tunnels: page,
        })
    }

    /// 打开终端工具：按参数拉起本机 PTY 或 SSH shell。
    ///
    /// 参数：spec 为终端种类、尺寸与连接参数
    /// 核心流程：原样交给宿主落地，成功返回终端标识与生效尺寸；
    /// 后续读写缩放关都拿这个标识定位。
    #[tool(
        description = "打开一个终端（本机 PTY、显式 SSH 或已保存会话），session 形态由桌面宿主解析已保存会话的凭据，返回终端标识与生效的列数行数",
        annotations(
            title = "打开终端",
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn terminal_open(
        &self,
        Parameters(spec): Parameters<TerminalSpec>,
    ) -> Result<CallToolResult, ErrorData> {
        let opened = match self.host.open_terminal(spec).await {
            Ok(opened) => opened,
            Err(error) => return terminal_error(error),
        };

        structured_output(opened)
    }

    /// 读取终端工具：按游标增量取回输出。
    ///
    /// 参数：id 为终端标识，cursor 为上次返回的 next，max 为单次最多字节数，plain 为是否剥离控制序列
    /// 核心流程：游标缺省从 0 开始，max 夹在 1 到 65536 之间；返回文本、起止游标、
    /// 是否截断与进程是否已退出，调用方拿 next 当下一轮的 cursor。
    #[tool(
        description = "读取终端的增量输出，返回本次文本、起始与下一个游标、是否截断以及进程是否已退出",
        annotations(
            title = "读取终端",
            read_only_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn terminal_read(
        &self,
        Parameters(params): Parameters<TerminalReadParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = params.id.trim();
        if id.is_empty() {
            return Err(ErrorData::invalid_params("终端标识不能为空", None));
        }
        let max = params.max.clamp(1, 65536);

        let chunk = match self
            .host
            .terminal_read(id, params.cursor, max, params.plain)
            .await
        {
            Ok(chunk) => chunk,
            Err(error) => return terminal_error(error),
        };

        structured_output(chunk)
    }

    /// 写入终端工具：把文本送进终端。
    ///
    /// 参数：id 为终端标识，data 为要写入的文本
    /// 核心流程：不做转义也不补回车，需要回车时 data 自带 `\n`；
    /// Windows 上的本机 PTY 由宿主把裸换行补成回车换行，远端 shell 原样发送；
    /// 成功后返回一句确认文案，正文里带上终端标识。
    #[tool(
        description = "向终端写入文本并回车时须自行带换行，返回写入结果；Windows 本地终端会自动把裸换行转为回车换行；向远端 shell 发送多行内容时行为不变",
        annotations(
            title = "写入终端",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn terminal_write(
        &self,
        Parameters(params): Parameters<TerminalWriteParams>,
    ) -> Result<CallToolResult, ErrorData> {
        if params.id.trim().is_empty() {
            return Err(ErrorData::invalid_params("终端标识不能为空", None));
        }

        match self.host.terminal_write(&params.id, &params.data).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "已写入终端：{}",
                params.id
            ))])),
            Err(error) => terminal_error(error),
        }
    }

    /// 缩放终端工具：调整列数与行数。
    ///
    /// 参数：id 为终端标识，cols 为列数，rows 为行数
    /// 核心流程：把尺寸交给宿主，终端会据此重排并通知底层进程；
    /// 成功后返回一句确认文案。
    #[tool(
        description = "调整终端的列数与行数，返回调整结果",
        annotations(
            title = "缩放终端",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn terminal_resize(
        &self,
        Parameters(params): Parameters<TerminalResizeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        if params.id.trim().is_empty() {
            return Err(ErrorData::invalid_params("终端标识不能为空", None));
        }

        match self
            .host
            .terminal_resize(&params.id, params.cols, params.rows)
            .await
        {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "已调整终端尺寸：{}",
                params.id
            ))])),
            Err(error) => terminal_error(error),
        }
    }

    /// 关闭终端工具：结束进程或连接并注销标识。
    ///
    /// 参数：id 为终端标识
    /// 核心流程：通知宿主关闭，成功后该标识再从注册表消失，
    /// 之后的读取会明确报「终端不存在」。
    #[tool(
        description = "关闭终端并注销其标识，返回关闭结果",
        annotations(
            title = "关闭终端",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn terminal_close(
        &self,
        Parameters(params): Parameters<TerminalCloseParams>,
    ) -> Result<CallToolResult, ErrorData> {
        if params.id.trim().is_empty() {
            return Err(ErrorData::invalid_params("终端标识不能为空", None));
        }

        match self.host.terminal_close(&params.id).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "已关闭终端：{}",
                params.id
            ))])),
            Err(error) => terminal_error(error),
        }
    }

    /// 执行命令工具：在已打开的 SSH 终端上跑一次性命令。
    ///
    /// 参数：id 为终端标识，command 为要执行的命令，maxBytes 为最多取回的字节数
    /// 核心流程：输出上限夹在 1 到 1 MiB 之间后交给宿主；命令走独立通道，
    /// 不干扰交互式 shell，stdout 原样作为文本结果返回，非 0 退出码由宿主报错。
    #[tool(
        description = "在已打开的 SSH 终端上执行一次性命令并返回 stdout 文本，非 0 退出码会报错",
        annotations(
            title = "执行命令",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn terminal_exec(
        &self,
        Parameters(params): Parameters<TerminalExecParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = params.id.trim();
        if id.is_empty() {
            return Err(ErrorData::invalid_params("终端标识不能为空", None));
        }
        let max = params.max_bytes.clamp(1, 1_048_576);

        match self.host.terminal_exec(id, &params.command, max).await {
            Ok(output) => Ok(CallToolResult::success(vec![ContentBlock::text(output)])),
            Err(error) => terminal_error(error),
        }
    }

    /// 新建分组工具：在根下或指定父分组下建一个空分组。
    ///
    /// 参数：name 为分组名，parentId 为父分组标识，缺省表示根分组
    /// 核心流程：名称去空白后为空直接按参数错误拒绝，其余交给宿主插入；
    /// 成功后返回新分组视图，标识可直接用于会话的 groupId。
    #[tool(
        description = "新建一个会话分组，parentId 缺省表示建在根下，返回新分组的标识与排序值",
        output_schema = rmcp::handler::server::tool::schema_for_output::<GroupView>(),
        annotations(
            title = "新建分组",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn group_create(
        &self,
        Parameters(params): Parameters<GroupCreateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let name = params.name.trim();
        if name.is_empty() {
            return Err(ErrorData::invalid_params("分组名称不能为空", None));
        }
        let parent_id = params
            .parent_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());

        let group = match self.host.create_group(name, parent_id).await {
            Ok(group) => group,
            Err(error) => return group_error(error),
        };

        structured_output(GroupView::from(group))
    }

    /// 重命名分组工具：只改名字，不动层级与组内会话。
    ///
    /// 参数：id 为分组标识，name 为新名称
    /// 核心流程：标识与名称都做非空校验后交宿主更新；同名重复执行结果一致，
    /// 成功后返回一句确认文案。
    #[tool(
        description = "重命名一个会话分组，不改变层级与组内会话，返回重命名结果",
        annotations(
            title = "重命名分组",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn group_rename(
        &self,
        Parameters(params): Parameters<GroupRenameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = params.id.trim();
        let name = params.name.trim();
        if id.is_empty() {
            return Err(ErrorData::invalid_params("分组标识不能为空", None));
        }
        if name.is_empty() {
            return Err(ErrorData::invalid_params("分组名称不能为空", None));
        }

        match self.host.rename_group(id, name).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "已重命名分组：{id} -> {name}"
            ))])),
            Err(error) => group_error(error),
        }
    }

    /// 删除分组工具：删掉分组本身，组内会话与子分组按存储层规则安置。
    ///
    /// 参数：id 为分组标识
    /// 核心流程：先把空标识挡掉，再交宿主删除；删除分组不可恢复，但组内会话不会被
    /// 连带删除（存储层会把它们上移到父分组），这一点写进描述避免调用方误判。
    #[tool(
        description = "删除一个会话分组，分组本身不可恢复；组内会话会被移到其父分组、子分组上提一级，不会连带删除会话",
        annotations(
            title = "删除分组",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn group_delete(
        &self,
        Parameters(params): Parameters<GroupDeleteParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = params.id.trim();
        if id.is_empty() {
            return Err(ErrorData::invalid_params("分组标识不能为空", None));
        }

        match self.host.delete_group(id).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "已删除分组：{id}"
            ))])),
            Err(error) => group_error(error),
        }
    }

    /// 移动分组工具：把分组挂到另一个父分组下。
    ///
    /// 参数：id 为分组标识，parentId 为新的父分组，缺省表示根分组
    /// 核心流程：空标识挡掉后交宿主更新父子关系，成环与目标不存在由存储层拒绝；
    /// 同一目标重复执行结果一致。
    #[tool(
        description = "把一个会话分组移动到新的父分组下，parentId 缺省表示移到根下，返回移动结果",
        annotations(
            title = "移动分组",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn group_move(
        &self,
        Parameters(params): Parameters<GroupMoveParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = params.id.trim();
        if id.is_empty() {
            return Err(ErrorData::invalid_params("分组标识不能为空", None));
        }
        let parent_id = params
            .parent_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());

        match self.host.move_group(id, parent_id).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "已移动分组：{id}"
            ))])),
            Err(error) => group_error(error),
        }
    }

    /// 新建会话工具：把一条连接配置写进会话库。
    ///
    /// 参数：name 为会话名，protocol 与 authKind 为落库字符串，host、port、username、
    /// groupId、extra 为可选连接信息
    /// 核心流程：协议与凭据方式先解析，非法取值回 invalid_params 并列出合法项；
    /// 其余字段去空白后交宿主校验入库，成功后返回带标识的会话视图。
    #[tool(
        description = "新建一个会话，protocol 取 ssh/sftp/local/telnet/rdp/vnc/serial/wsl，authKind 取 password/key/agent，返回新会话的完整信息",
        output_schema = rmcp::handler::server::tool::schema_for_output::<SessionView>(),
        annotations(
            title = "新建会话",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn session_create(
        &self,
        Parameters(params): Parameters<SessionCreateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        if params.name.trim().is_empty() {
            return Err(ErrorData::invalid_params("会话名称不能为空", None));
        }
        let input = session_input(
            &params.name,
            &params.protocol,
            params.host,
            params.port,
            params.username,
            &params.auth_kind,
            params.group_id,
            params.extra,
        )?;

        let session = match self.host.create_session(&input).await {
            Ok(session) => session,
            Err(error) => return host_error(error),
        };

        structured_output(SessionView::from(session))
    }

    /// 更新会话工具：整份字段覆盖一个已有会话。
    ///
    /// 参数：id 为会话标识，其余字段与新建会话一致
    /// 核心流程：解析规则与新建一致，未填的可选字段会被写成空（不做字段级合并）；
    /// 成功返回更新后的会话视图，同样的入参重复执行结果一致。
    #[tool(
        description = "更新一个已有会话的全部字段，未填的可选字段会被清空（不是字段级合并），返回更新后的会话信息",
        output_schema = rmcp::handler::server::tool::schema_for_output::<SessionView>(),
        annotations(
            title = "更新会话",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn session_update(
        &self,
        Parameters(params): Parameters<SessionUpdateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = params.id.trim();
        if id.is_empty() {
            return Err(ErrorData::invalid_params("会话标识不能为空", None));
        }
        if params.name.trim().is_empty() {
            return Err(ErrorData::invalid_params("会话名称不能为空", None));
        }
        let input = session_input(
            &params.name,
            &params.protocol,
            params.host,
            params.port,
            params.username,
            &params.auth_kind,
            params.group_id,
            params.extra,
        )?;

        let session = match self.host.update_session(id, &input).await {
            Ok(session) => session,
            Err(error) => return host_error(error),
        };

        structured_output(SessionView::from(session))
    }

    /// 删除会话工具：把一条会话从库里移除。
    ///
    /// 参数：id 为会话标识
    /// 核心流程：空标识挡掉后交宿主删除，删除不可恢复；
    /// 已不存在的标识回 is_error 结果而不是协议错误。
    #[tool(
        description = "删除一个会话，删除后不可恢复，返回删除结果",
        annotations(
            title = "删除会话",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn session_delete(
        &self,
        Parameters(params): Parameters<SessionDeleteParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = params.id.trim();
        if id.is_empty() {
            return Err(ErrorData::invalid_params("会话标识不能为空", None));
        }

        match self.host.delete_session(id).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "已删除会话：{id}"
            ))])),
            Err(error) => host_error(error),
        }
    }

    /// 移动会话工具：只改会话归属的分组。
    ///
    /// 参数：id 为会话标识，groupId 为目标分组，缺省表示移出分组
    /// 核心流程：空标识挡掉后交宿主改分组，会话其余字段原样保留；
    /// 目标分组不存在由存储层报错，重复移入同一分组结果一致。
    #[tool(
        description = "把一个会话移动到指定分组，groupId 缺省表示移到未分组，只改归属不动其余字段",
        annotations(
            title = "移动会话",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn session_move(
        &self,
        Parameters(params): Parameters<SessionMoveParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = params.id.trim();
        if id.is_empty() {
            return Err(ErrorData::invalid_params("会话标识不能为空", None));
        }
        let group_id = params
            .group_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());

        match self.host.move_session(id, group_id).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "已移动会话：{id}"
            ))])),
            Err(error) => host_error(error),
        }
    }

    /// 复制会话工具：派生出同名加后缀的新会话。
    ///
    /// 参数：id 为被复制的会话标识
    /// 核心流程：交宿主复制，新名称由存储层避开现有名称后派生；
    /// 每次调用都会新增一条会话，因此不标记幂等。
    #[tool(
        description = "复制一个会话，副本名称自动加后缀，每次调用都会新增一条会话，返回副本信息",
        output_schema = rmcp::handler::server::tool::schema_for_output::<SessionView>(),
        annotations(
            title = "复制会话",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn session_copy(
        &self,
        Parameters(params): Parameters<SessionCopyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = params.id.trim();
        if id.is_empty() {
            return Err(ErrorData::invalid_params("会话标识不能为空", None));
        }

        let session = match self.host.copy_session(id).await {
            Ok(session) => session,
            Err(error) => return host_error(error),
        };

        structured_output(SessionView::from(session))
    }

    /// 设置列表工具：读出全部应用设置。
    ///
    /// 参数：无入参
    /// 核心流程：取宿主的键值列表后按 key 升序再排一次，连同总数一起返回；
    /// 库里没有的键不补默认值，由前端用自己的缺省值兜底。
    #[tool(
        description = "查询全部应用设置，返回设置总数与按键名升序排列的键值列表",
        output_schema = rmcp::handler::server::tool::schema_for_output::<SettingsListOutput>(),
        annotations(
            title = "列出设置",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn settings_list(&self) -> Result<CallToolResult, ErrorData> {
        let mut settings = match self.host.settings_get_all().await {
            Ok(settings) => settings,
            Err(error) => return host_error(error),
        };
        settings.sort_by(|left, right| left.0.cmp(&right.0));

        let settings: Vec<SettingView> = settings
            .into_iter()
            .map(|(key, value)| SettingView { key, value })
            .collect();
        let count = settings.len();
        structured_output(SettingsListOutput { count, settings })
    }

    /// 写入设置工具：覆盖一条应用设置。
    ///
    /// 参数：key 为设置键，value 为设置值
    /// 核心流程：空键直接按参数错误拒绝；`mcp.` 开头的键由宿主拦下并回
    /// invalid_params，避免 agent 把自己关掉或改端口后前端误判；其余键 UPSERT 落库。
    #[tool(
        description = "写入一条应用设置（覆盖同名键），不能修改 mcp.* 开头的键，返回写入结果",
        annotations(
            title = "写入设置",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn settings_set(
        &self,
        Parameters(params): Parameters<SettingsSetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let key = params.key.trim();
        if key.is_empty() {
            return Err(ErrorData::invalid_params("设置键名不能为空", None));
        }

        match self.host.settings_set(key, &params.value).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "已写入设置：{key}"
            ))])),
            Err(error) => host_error(error),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MobaxServer {
    /// 处理一次工具调用并落审计。
    ///
    /// 参数：request 为客户端发来的工具调用参数，context 为请求上下文
    /// 核心流程：先用工具名与参数生成不含明文的摘要并记下开始时间，再交给路由表执行；
    /// 结果统一转给 [`MobaxServer::record_call`] 写审计，最后原样返回给客户端。
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tool = request.name.to_string();
        let digest = digest_tool_arguments(&tool, request.arguments.as_ref());
        let started = Instant::now();
        let result = self
            .tool_router
            .call(ToolCallContext::new(self, request, context))
            .await;
        self.record_call(&tool, &digest, started, &result).await;
        result
    }

    /// 向客户端申明服务端身份与能力。
    ///
    /// 参数：无
    /// 核心流程：同时打开工具、资源与提示模板三项能力，服务名与版本取自包元数据，
    /// 说明里写明监听范围、审计与配置拦截三条边界，让客户端一开始就知道规矩。
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_server_info(Implementation::new(
            "rustmobax-mcp",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(
            "这是 RustMobaX 的 MCP 服务，提供会话库、隧道配置与终端控制三类能力。\
             服务只监听本机回环地址，外部机器连不上。\
             危险操作（删除、写入、执行命令等）都会记入审计日志，事后可查。\
             修改 MCP 自身配置（mcp. 开头的设置键）会被拒绝。",
        )
    }

    /// 列出静态资源。
    ///
    /// 参数：request 为可选分页参数，context 为请求上下文
    /// 核心流程：资源只有五个，分页参数无意义，直接整份包成一页返回。
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(self.resource_list()))
    }

    /// 列出资源模板。
    ///
    /// 参数：request 为可选分页参数，context 为请求上下文
    /// 核心流程：两个模板一次性返回，客户端据此拼出带标识的资源地址。
    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(
            self.resource_template_list(),
        ))
    }

    /// 读一个资源。
    ///
    /// 参数：request 为带 URI 的读请求，context 为请求上下文
    /// 核心流程：把 URI 交给 [`MobaxServer::read_uri`] 分派，结果包成协议要求的
    /// 完成态响应；未知 URI 与取不到的数据都以 resource_not_found 报错。
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        Ok(self.read_uri(&request.uri).await?.into())
    }

    /// 列出提示模板。
    ///
    /// 参数：request 为可选分页参数，context 为请求上下文
    /// 核心流程：三个模板一次性返回，不带分页。
    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(self.prompt_list()))
    }

    /// 取一个提示模板的正文。
    ///
    /// 参数：request 为模板名与入参，context 为请求上下文
    /// 核心流程：把模板名与 sessionId 交给 [`MobaxServer::prompt_get`] 渲染，
    /// 未知模板名回 invalid_params。
    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        Ok(self
            .prompt_get(&request.name, request.arguments.as_ref())?
            .into())
    }
}

/// 全部会话的资源地址。
const RES_URI_SESSIONS: &str = "mobax://sessions";
/// 全部分组的资源地址。
const RES_URI_GROUPS: &str = "mobax://groups";
/// 全部隧道配置的资源地址。
const RES_URI_TUNNELS: &str = "mobax://tunnels";
/// 全部设置的资源地址。
const RES_URI_SETTINGS: &str = "mobax://settings";
/// 活动终端的资源地址。
const RES_URI_TERMINALS: &str = "mobax://terminals";
/// 单条会话资源的前缀。
const RES_URI_SESSION_PREFIX: &str = "mobax://sessions/";
/// 终端屏幕资源的前缀。
const RES_URI_SCREEN_PREFIX: &str = "mobax://terminals/";
/// 终端屏幕资源的后缀。
const RES_URI_SCREEN_SUFFIX: &str = "/screen";
/// JSON 资源的 MIME 类型。
const MIME_JSON: &str = "application/json";
/// 纯文本资源的 MIME 类型。
const MIME_TEXT: &str = "text/plain";
/// 设置里绝不出现在资源正文中的键。
const MCP_TOKEN_KEY: &str = "mcp.token";
/// 排查主机的提示模板名。
const PROMPT_TRIAGE: &str = "triage-host";
/// 只读检查会话的提示模板名。
const PROMPT_INSPECT: &str = "inspect-session";
/// 修改前行为约束的提示模板名。
const PROMPT_SAFE: &str = "safe-change";
/// 提示模板里会话标识的入参名。
const SESSION_ARG: &str = "sessionId";
/// 入参缺省时填进模板正文的占位文案。
const SESSION_ARG_MISSING: &str = "未提供";

impl MobaxServer {
    /// 列出五个静态资源。
    ///
    /// 参数：无
    /// 核心流程：把会话、分组、隧道、设置、终端五项各写成一条资源描述，
    /// 名称控制在一行内，MIME 类型与正文一致，客户端不必再猜。
    fn resource_list(&self) -> Vec<Resource> {
        vec![
            Resource::new(RES_URI_SESSIONS, "全部会话")
                .with_description("全部已保存会话的连接配置，JSON 数组，字段为 camelCase")
                .with_mime_type(MIME_JSON),
            Resource::new(RES_URI_GROUPS, "全部分组")
                .with_description("全部会话分组，JSON 数组，含父级与同级排序值")
                .with_mime_type(MIME_JSON),
            Resource::new(RES_URI_TUNNELS, "全部隧道配置")
                .with_description("全部隧道转发配置，JSON 数组，只含配置不含运行状态")
                .with_mime_type(MIME_JSON),
            Resource::new(RES_URI_SETTINGS, "全部设置")
                .with_description("全部应用设置的键值对象，已剔除 mcp.token")
                .with_mime_type(MIME_JSON),
            Resource::new(RES_URI_TERMINALS, "活动终端")
                .with_description("当前活动终端列表，JSON 数组，摸不到的字段会省略")
                .with_mime_type(MIME_JSON),
        ]
    }

    /// 列出两个资源模板。
    ///
    /// 参数：无
    /// 核心流程：模板只描述 URI 形状与取值含义，具体地址由客户端把标识填进去。
    fn resource_template_list(&self) -> Vec<ResourceTemplate> {
        vec![
            ResourceTemplate::new("mobax://sessions/{id}", "单个会话")
                .with_description("按会话标识读出一条会话的完整配置")
                .with_mime_type(MIME_JSON),
            ResourceTemplate::new("mobax://terminals/{id}/screen", "终端屏幕")
                .with_description("按终端标识读出当前屏幕的纯文本，最多 65536 字节")
                .with_mime_type(MIME_TEXT),
        ]
    }

    /// 按地址读出一个资源。
    ///
    /// 参数：uri 为客户端请求的资源地址
    /// 核心流程：先按五个静态地址精确匹配，未命中的再按单会话与终端屏幕两个模板
    /// 拆出标识；未知地址、拆不出标识、会话或终端不存在都回 resource_not_found，
    /// 宿主内部故障才回 internal_error，两类错误不混用。
    async fn read_uri(&self, uri: &str) -> Result<ReadResourceResult, ErrorData> {
        let uri = uri.trim();
        let contents = match uri {
            RES_URI_SESSIONS => {
                let sessions = self
                    .host
                    .list_sessions("")
                    .await
                    .map_err(|error| Self::resource_error(error, uri))?;
                let views: Vec<SessionView> = sessions.into_iter().map(SessionView::from).collect();
                json_contents(uri, &views)?
            }
            RES_URI_GROUPS => {
                let groups = self
                    .host
                    .list_groups()
                    .await
                    .map_err(|error| Self::resource_error(error, uri))?;
                let views: Vec<GroupView> = groups.into_iter().map(GroupView::from).collect();
                json_contents(uri, &views)?
            }
            RES_URI_TUNNELS => {
                let tunnels = self
                    .host
                    .list_tunnel_configs()
                    .await
                    .map_err(|error| Self::resource_error(error, uri))?;
                let views: Vec<TunnelView> = tunnels.into_iter().map(TunnelView::from).collect();
                json_contents(uri, &views)?
            }
            RES_URI_SETTINGS => {
                let settings = self
                    .host
                    .settings_get_all()
                    .await
                    .map_err(|error| Self::resource_error(error, uri))?;
                json_contents(uri, &settings_object(settings))?
            }
            RES_URI_TERMINALS => {
                let terminals: Vec<TerminalInfo> = self.host.list_terminals().await;
                json_contents(uri, &terminals)?
            }
            _ => {
                if let Some(id) = uri.strip_prefix(RES_URI_SESSION_PREFIX) {
                    let id = id.trim();
                    if id.is_empty() || id.contains('/') {
                        return Err(resource_missing(uri));
                    }
                    let session = self
                        .host
                        .get_session(id)
                        .await
                        .map_err(|error| Self::resource_error(error, uri))?;
                    json_contents(uri, &SessionView::from(session))?
                } else if let Some(id) = uri
                    .strip_prefix(RES_URI_SCREEN_PREFIX)
                    .and_then(|rest| rest.strip_suffix(RES_URI_SCREEN_SUFFIX))
                {
                    let id = id.trim();
                    if id.is_empty() || id.contains('/') {
                        return Err(resource_missing(uri));
                    }
                    // 从头读满 64 KiB 再剥控制序列，拿到的就是一屏干净文本
                    let chunk = self
                        .host
                        .terminal_read(id, 0, 65536, true)
                        .await
                        .map_err(|_| resource_missing(uri))?;
                    ResourceContents::text(chunk.text, uri).with_mime_type(MIME_TEXT)
                } else {
                    return Err(resource_missing(uri));
                }
            }
        };

        Ok(ReadResourceResult::new(vec![contents]))
    }

    /// 列出三个提示模板。
    ///
    /// 参数：无
    /// 核心流程：前两个模板各带一个必填的 sessionId，第三个不带入参；
    /// 描述用中文写清用途，客户端菜单里能直接看懂。
    fn prompt_list(&self) -> Vec<Prompt> {
        vec![
            Prompt::new(
                PROMPT_TRIAGE,
                Some("按会话排查主机负载、磁盘与内存，并汇总成表格"),
                Some(vec![session_argument("要排查的会话标识")]),
            ),
            Prompt::new(
                PROMPT_INSPECT,
                Some("只读检查一条会话的配置与隧道，指出可疑项"),
                Some(vec![session_argument("要检查的会话标识")]),
            ),
            Prompt::new(PROMPT_SAFE, Some("修改与删除类操作前的行为约束"), None),
        ]
    }

    /// 渲染一个提示模板。
    ///
    /// 参数：name 为模板名，arguments 为客户端传来的入参对象
    /// 核心流程：取出 sessionId（缺省填占位文案）后按模板名拼正文，
    /// 未知模板名回 invalid_params 并带上是哪个名字。
    fn prompt_get(
        &self,
        name: &str,
        arguments: Option<&JsonObject>,
    ) -> Result<GetPromptResult, ErrorData> {
        let session_id = arguments
            .and_then(|args| args.get(SESSION_ARG))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(SESSION_ARG_MISSING);

        let (description, body) = match name {
            PROMPT_TRIAGE => (
                "按会话排查主机负载、磁盘与内存，并汇总成表格",
                triage_body(session_id),
            ),
            PROMPT_INSPECT => (
                "只读检查一条会话的配置与隧道，指出可疑项",
                inspect_body(session_id),
            ),
            PROMPT_SAFE => ("修改与删除类操作前的行为约束", safe_change_body()),
            other => {
                return Err(ErrorData::invalid_params(
                    format!("未知的提示模板：{other}"),
                    None,
                ));
            }
        };

        Ok(
            GetPromptResult::new(vec![PromptMessage::new_text(Role::User, body)])
                .with_description(description),
        )
    }

    /// 把宿主错误映射成资源层协议错误。
    ///
    /// 参数：error 为宿主返回的错误，uri 为正在读的资源地址
    /// 核心流程：未找到统一回 resource_not_found，其余按内部错误回；
    /// 两类正文都带上资源地址，客户端一眼看出是哪一条读不到。
    fn resource_error(error: CoreError, uri: &str) -> ErrorData {
        match error {
            CoreError::NotFound(id) => resource_missing(&format!("{uri}，标识：{id}")),
            other => ErrorData::internal_error(format!("读取资源失败：{uri}：{other}"), None),
        }
    }
}

/// 造一个 resource_not_found 协议错误。
///
/// 参数：what 为找不到的地址或描述
/// 核心流程：固定中文文案，带上请求的地址，客户端不需要再拼一遍。
fn resource_missing(what: &str) -> ErrorData {
    ErrorData::resource_not_found(format!("资源不存在：{what}"), None)
}

/// 把可序列化的内容包成 JSON 资源正文。
///
/// 参数：uri 为资源地址，value 为待序列化的内容
/// 核心流程：直接按美化格式输出，客户端读上下文时不必再排版；
/// 序列化失败属于服务端问题，转成内部错误而不是伪装成资源不存在。
fn json_contents<T: Serialize>(uri: &str, value: &T) -> Result<ResourceContents, ErrorData> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| ErrorData::internal_error(format!("资源序列化失败：{error}"), None))?;
    Ok(ResourceContents::text(text, uri).with_mime_type(MIME_JSON))
}

/// 把设置键值列表转成 JSON 对象，并剔除 MCP 自身的令牌。
///
/// 参数：settings 为宿主给出的键值列表
/// 核心流程：按键升序逐个塞进对象，遇到 mcp.token 直接跳过，
/// 令牌值一个字都不进资源正文。
fn settings_object(settings: Vec<(String, String)>) -> serde_json::Value {
    let mut settings = settings;
    settings.sort_by(|left, right| left.0.cmp(&right.0));
    let mut object = serde_json::Map::new();
    for (key, value) in settings {
        if key == MCP_TOKEN_KEY {
            continue;
        }
        object.insert(key, serde_json::Value::String(value));
    }
    serde_json::Value::Object(object)
}

/// 造一个 sessionId 入参描述。
///
/// 参数：description 为给客户端看的入参说明
/// 核心流程：名字固定为 sessionId，必填为真，说明原样带上。
fn session_argument(description: &str) -> PromptArgument {
    PromptArgument::new(SESSION_ARG)
        .with_description(description)
        .with_required(true)
}

/// 排查主机模板的正文。
///
/// 参数：session_id 为要排查的会话标识
/// 核心流程：固定六步，先看配置再开终端，三条命令分次执行分次读取，
/// 最后要求汇总成表格，避免模型把步骤合并成一句「已排查」。
fn triage_body(session_id: &str) -> String {
    format!(
        "请按下面的步骤排查会话 {session_id} 所在的主机，不要跳步，也不要在命令之间混读输出：\n\
         1. 调用 session_get 读出该会话的连接配置，确认协议是 ssh、主机与用户名都已填写；\n\
         2. 调用 terminal_open 以 kind=session 打开该会话的终端，列数与行数取 120 与 30；\n\
         3. 在终端里依次执行 uptime、df -h、free -m 三条命令，每条命令等回显出现后再执行下一条；\n\
         4. 用 terminal_read 按游标把三条命令的输出读回，游标取上一次返回的 next；\n\
         5. 把结果汇总成一张中文表格，列出 1 分钟负载、根分区占用率、内存已用与总量，并标出任何超过 80% 的指标；\n\
         6. 排查结束后不要关闭终端，留给用户继续查看。"
    )
}

/// 只读检查会话模板的正文。
///
/// 参数：session_id 为要检查的会话标识
/// 核心流程：限定两项只读调用，再要求按常见配置错误逐条给结论，
/// 没问题也要明确写出来，避免空回答。
fn inspect_body(session_id: &str) -> String {
    format!(
        "请对会话 {session_id} 做一次只读检查，全程不要执行任何写操作：\n\
         1. 调用 session_get 读出该会话的完整配置；\n\
         2. 调用 tunnel_list 列出全部隧道配置，筛出 sessionId 等于该会话的条目；\n\
         3. 结合两份结果指出配置里可能的问题，例如主机或端口为空、协议与凭据方式不匹配、隧道监听端口与其他隧道冲突、动态转发却填了目标端口；\n\
         4. 用中文分条列出发现的问题与修改建议，没有问题就明确写「未发现明显问题」。"
    )
}

/// 修改前行为约束模板的正文。
///
/// 参数：无
/// 核心流程：四条约束分别覆盖先查后改、删除前确认、不碰 mcp 配置、失败不盲目重试，
/// 都是不依赖具体会话的通用规矩。
fn safe_change_body() -> String {
    "在动手修改之前，请遵守以下约束：\n\
     1. 修改一条记录之前，先用 settings_list 或 session_list 确认现状，并在回答里复述将要改变的值；\n\
     2. 删除类操作（session_delete、group_delete、terminal_close）必须先说明影响范围，等用户明确同意后再执行；\n\
     3. 不要尝试修改 mcp. 开头的设置，被拒绝时说明这是策略拦截，而不是服务故障；\n\
     4. 一次只做一件事，失败后先读错误正文再决定是否重试，不要连续重试同一条命令。"
        .to_string()
}

impl MobaxServer {
    /// 把一次工具调用的结果写进审计。
    ///
    /// 参数：tool 为工具名，digest 为参数摘要，started 为调用开始时刻，
    /// result 为路由表的返回值
    /// 核心流程：工具结果看 `is_error` 判定成败，协议错误取错误正文；失败正文统一截到
    /// 200 字，避免把长输出整段写进库；审计写失败由实现自己吞掉，不影响本次调用。
    async fn record_call(
        &self,
        tool: &str,
        digest: &str,
        started: Instant,
        result: &Result<CallToolResponse, ErrorData>,
    ) {
        let duration_ms = started.elapsed().as_millis() as i64;
        let (ok, error) = match result {
            Ok(CallToolResponse::Complete(result)) if result.is_error.unwrap_or(false) => {
                (false, Some(truncate_chars(&tool_text(result), 200)))
            }
            Ok(_) => (true, None),
            Err(error) => (false, Some(truncate_chars(&error.message, 200))),
        };
        self.audit
            .record(tool, digest, ok, error.as_deref(), duration_ms)
            .await;
    }
}

/// 把工具结果里的文本块拼起来。
///
/// 参数：result 为工具返回值
/// 核心流程：只取文本块并用换行连接，图片、资源等非文本内容不参与审计正文。
fn tool_text(result: &CallToolResult) -> String {
    let mut text = String::new();
    for block in &result.content {
        if let ContentBlock::Text(content) = block {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&content.text);
        }
    }
    text
}

/// 按字符数截断文本。
///
/// 参数：text 为原始文本，max 为最多保留的字符数
/// 核心流程：按 char 边界取前 max 个字符，中文不会被截成半个；不追加省略号，
/// 避免审计正文与真实错误混淆。
fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use rustmobax_core::{Store, TunnelConfigInput};

    use super::*;
    use crate::TerminalHost;
    use crate::host::{OpenedTerminal, SshSpec, StoreHost, TerminalChunk};
    use rmcp::model::ErrorCode;

    /// 一次被记下来的调用。
    ///
    /// 无参数
    /// 核心流程：逐字段保存审计入参，测试按字段断言，不经过数据库。
    #[derive(Debug, Clone)]
    struct RecordedCall {
        tool: String,
        digest: String,
        ok: bool,
        error: Option<String>,
        duration_ms: i64,
    }

    /// 内存审计实现，供单测断言记录内容。
    ///
    /// 无参数
    /// 核心流程：用一个 `Mutex<Vec<...>>` 收集每次调用，锁中毒直接按约定 panic。
    #[derive(Default)]
    struct RecordingAudit {
        calls: Mutex<Vec<RecordedCall>>,
    }

    impl RecordingAudit {
        /// 取出全部记录。
        ///
        /// 参数：无
        /// 核心流程：加锁后克隆一份，调用方拿到快照后可以随意断言。
        fn calls(&self) -> Vec<RecordedCall> {
            self.calls
                .lock()
                .expect("审计记录锁被污染")
                .clone()
        }
    }

    #[async_trait::async_trait]
    impl McpAudit for RecordingAudit {
        async fn record(
            &self,
            tool: &str,
            digest: &str,
            ok: bool,
            error: Option<&str>,
            duration_ms: i64,
        ) {
            self.calls
                .lock()
                .expect("审计记录锁被污染")
                .push(RecordedCall {
                    tool: tool.to_string(),
                    digest: digest.to_string(),
                    ok,
                    error: error.map(str::to_string),
                    duration_ms,
                });
        }
    }

    /// 造一个内存审计，给不关心审计内容的用例用。
    ///
    /// 参数：无
    /// 核心流程：新建空的 [`RecordingAudit`] 并按 trait 对象返回。
    fn audit() -> Arc<dyn McpAudit> {
        Arc::new(RecordingAudit::default())
    }

    /// 造一个带真终端的宿主。
    ///
    /// 参数：无
    /// 核心流程：开内存库即可，终端不落库，注册表随宿主一起建好。
    async fn host() -> Arc<TerminalHost> {
        Arc::new(TerminalHost::new(
            Store::open_in_memory().await.expect("内存库应能打开"),
        ))
    }

    /// 造一个只架在内存库上的宿主。
    ///
    /// 参数：无
    /// 核心流程：开内存库包成 [`StoreHost`] 并返回句柄，测试既能走工具，
    /// 也能直接查库确认落库结果。
    async fn store_host() -> Arc<StoreHost> {
        Arc::new(StoreHost::new(
            Store::open_in_memory().await.expect("内存库应能打开"),
        ))
    }

    /// 从工具结果里取出结构化 JSON。
    ///
    /// 参数：result 为工具返回值
    /// 核心流程：先断言不是错误结果，再取走结构化内容；
    /// 断言消息带上正文，失败时能直接看出原因。
    fn structured(result: CallToolResult) -> serde_json::Value {
        assert_eq!(
            result.is_error,
            Some(false),
            "工具不应报错：{}",
            text_of(&result)
        );
        result.structured_content.expect("应含结构化结果")
    }

    /// 通过新建会话工具建一条最小可用的 SSH 会话。
    ///
    /// 参数：server 为服务端；name 为会话名
    /// 核心流程：协议固定 ssh、凭据方式固定 agent，返回结构化结果供调用方取标识。
    async fn make_session(server: &MobaxServer, name: &str) -> serde_json::Value {
        let result = server
            .session_create(Parameters(SessionCreateParams {
                name: name.to_string(),
                protocol: "ssh".to_string(),
                host: Some("10.0.0.1".to_string()),
                port: Some(22),
                username: Some("root".to_string()),
                auth_kind: "agent".to_string(),
                group_id: None,
                extra: None,
            }))
            .await
            .expect("新建会话工具应返回结果");
        structured(result)
    }

    /// 往内存库里塞一条隧道配置。
    ///
    /// 参数：host 为只架在内存库上的宿主；name 为隧道名；kind 为转发类型；
    /// listen_port 为监听端口；session_id 为借用的会话标识
    /// 核心流程：直接调存储层插入接口绕过工具层，监听地址固定 0.0.0.0、
    /// 目标固定 10.0.0.9:5432，返回落库结果供调用方断言。
    async fn make_tunnel(
        host: &StoreHost,
        name: &str,
        kind: &str,
        listen_port: u16,
        session_id: &str,
    ) -> TunnelConfig {
        host.store()
            .tunnel_config_insert(&TunnelConfigInput {
                name: name.to_string(),
                session_id: session_id.to_string(),
                kind: kind.to_string(),
                listen_host: "0.0.0.0".to_string(),
                listen_port,
                target_host: "10.0.0.9".to_string(),
                target_port: 5432,
                auto_start: false,
            })
            .await
            .expect("应能插入隧道配置")
    }

    /// 造一个装着三条隧道配置的服务端。
    ///
    /// 参数：无
    /// 核心流程：先建一条会话给三条隧道共用，再按 alpha-db、beta-redis、gamma-web
    /// 的顺序插入，每条之间留两毫秒，保证列表顺序与插入顺序一致。
    async fn tunnel_fixture() -> (MobaxServer, Arc<StoreHost>) {
        let host = store_host().await;
        let server = MobaxServer::new(host.clone(), audit());
        let session = make_session(&server, "tunnel-base").await;
        let session_id = session["id"].as_str().expect("会话应含标识").to_string();

        make_tunnel(&host, "alpha-db", "local", 8001, &session_id).await;
        tokio::time::sleep(Duration::from_millis(2)).await;
        make_tunnel(&host, "beta-redis", "remote", 8002, &session_id).await;
        tokio::time::sleep(Duration::from_millis(2)).await;
        make_tunnel(&host, "gamma-web", "dynamic", 8003, &session_id).await;

        (server, host)
    }

    /// 调一次隧道列表工具并取出结构化结果。
    ///
    /// 参数：server 为服务端；keyword、limit、offset 为三个可选项
    /// 核心流程：三个字段原样塞进入参后调用工具，断言不是错误结果并返回 JSON。
    async fn list_tunnels(
        server: &MobaxServer,
        keyword: Option<&str>,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> serde_json::Value {
        structured(
            server
                .tunnel_list(Parameters(TunnelListParams {
                    keyword: keyword.map(str::to_string),
                    limit,
                    offset,
                }))
                .await
                .expect("隧道列表工具应返回结果"),
        )
    }

    /// 把工具结果里的文本拼起来。
    ///
    /// 参数：result 为工具返回值
    /// 核心流程：只取文本块，命中的拼成一串，便于断言中文错误文案。
    fn text_of(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|block| block.as_text())
            .map(|text| text.text.clone())
            .collect::<Vec<_>>()
            .join("")
    }

    /// 从工具结果里解出终端标识与尺寸。
    ///
    /// 参数：result 为打开终端的结果
    /// 核心流程：先确认不是错误，再从结构化内容反序列化，字段名由 camelCase 约定保证。
    fn opened_of(result: CallToolResult) -> OpenedTerminal {
        assert_eq!(
            result.is_error,
            Some(false),
            "打开终端不应报错：{}",
            text_of(&result)
        );
        serde_json::from_value(result.structured_content.expect("应含结构化结果"))
            .expect("应能解出打开结果")
    }

    /// 从工具结果里解出一段终端输出。
    ///
    /// 参数：result 为读取终端的结果
    /// 核心流程：断言成功后再反序列化，失败时把正文带进断言消息。
    fn chunk_of(result: CallToolResult) -> TerminalChunk {
        assert_eq!(
            result.is_error,
            Some(false),
            "读取终端不应报错：{}",
            text_of(&result)
        );
        serde_json::from_value(result.structured_content.expect("应含结构化结果"))
            .expect("应能解出终端片段")
    }

    /// 用给定壳程序打开一个本机终端。
    ///
    /// 参数：server 为服务端；shell 为壳程序路径
    /// 核心流程：走一遍 terminal_open 工具，返回解析后的打开结果。
    async fn open_local(server: &MobaxServer, shell: &str) -> OpenedTerminal {
        let spec = TerminalSpec::Local {
            shell: Some(shell.to_string()),
            cwd: None,
            cols: 100,
            rows: 30,
        };
        let result = server
            .terminal_open(Parameters(spec))
            .await
            .expect("打开终端工具应返回结果");
        opened_of(result)
    }

    /// 读一次终端。
    ///
    /// 参数：server 为服务端；id 为终端标识；cursor 为游标；plain 为是否剥离控制序列
    /// 核心流程：把三个可选字段填满后调用工具，解出终端片段。
    async fn read_once(
        server: &MobaxServer,
        id: &str,
        cursor: u64,
        plain: bool,
    ) -> TerminalChunk {
        let params = TerminalReadParams {
            id: id.to_string(),
            cursor,
            max: 65536,
            plain,
        };
        let result = server
            .terminal_read(Parameters(params))
            .await
            .expect("读取终端工具应返回结果");
        chunk_of(result)
    }

    /// 往终端写一段文本。
    ///
    /// 参数：server 为服务端；id 为终端标识；data 为要写入的文本
    /// 核心流程：调用写入工具并断言成功，失败时把正文带进断言消息。
    async fn write_text(server: &MobaxServer, id: &str, data: &str) {
        let result = server
            .terminal_write(Parameters(TerminalWriteParams {
                id: id.to_string(),
                data: data.to_string(),
            }))
            .await
            .expect("写入终端工具应返回结果");
        assert_eq!(
            result.is_error,
            Some(false),
            "写入终端不应报错：{}",
            text_of(&result)
        );
    }

    /// 真 PTY 端到端：开终端、写命令、按游标读回显。
    #[tokio::test]
    async fn pty_round_trip_echoes_command_output() {
        let server = MobaxServer::new(host().await, audit());
        let opened = open_local(&server, "/bin/sh").await;

        write_text(&server, &opened.id, "echo MOBAX_OK\n").await;

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut cursor = 0u64;
        let mut collected = String::new();
        loop {
            let chunk = read_once(&server, &opened.id, cursor, true).await;
            assert_eq!(
                chunk.start, cursor,
                "每轮起点应等于上一轮的 next，实际片段：{chunk:?}"
            );
            if !chunk.text.is_empty() {
                assert!(
                    chunk.next > chunk.start,
                    "有内容时 next 必须严格递增，实际片段：{chunk:?}"
                );
            }
            collected.push_str(&chunk.text);
            cursor = chunk.next;
            if collected.contains("MOBAX_OK") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "5 秒内应读到命令回显，实际收到：{collected:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        assert!(collected.contains("MOBAX_OK"), "应读到命令输出：{collected:?}");

        let closed = server
            .terminal_close(Parameters(TerminalCloseParams {
                id: opened.id.clone(),
            }))
            .await
            .expect("关闭终端工具应返回结果");
        assert_eq!(
            closed.is_error,
            Some(false),
            "关闭终端不应报错：{}",
            text_of(&closed)
        );
    }

    /// 默认的 plain 读取应把 ANSI 颜色码剥干净。
    #[tokio::test]
    async fn plain_read_strips_ansi_color_codes() {
        let server = MobaxServer::new(host().await, audit());
        let opened = open_local(&server, "/bin/sh").await;

        write_text(
            &server,
            &opened.id,
            "printf '\\033[31mRED\\033[0m\\n'\n",
        )
        .await;

        // 先等原始字节里出现真正的颜色序列 -> 说明命令输出已经到了
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let raw = read_once(&server, &opened.id, 0, false).await;
            if raw.text.contains("\u{1b}[31m") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "5 秒内应读到颜色序列，实际收到：{:?}",
                raw.text
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let plain = read_once(&server, &opened.id, 0, true).await;
        assert!(plain.text.contains("RED"), "应保留正文：{:?}", plain.text);
        assert!(
            !plain.text.contains('\u{1b}'),
            "剥离后不应残留转义符：{:?}",
            plain.text
        );
    }

    /// 关闭之后的读取必须报错，且正文里带终端标识。
    #[tokio::test]
    async fn read_after_close_reports_missing_terminal() {
        let server = MobaxServer::new(host().await, audit());
        let opened = open_local(&server, "/bin/sh").await;

        let closed = server
            .terminal_close(Parameters(TerminalCloseParams {
                id: opened.id.clone(),
            }))
            .await
            .expect("关闭终端工具应返回结果");
        assert_eq!(
            closed.is_error,
            Some(false),
            "关闭终端不应报错：{}",
            text_of(&closed)
        );

        let result = server
            .terminal_read(Parameters(TerminalReadParams {
                id: opened.id.clone(),
                cursor: 0,
                max: 8192,
                plain: true,
            }))
            .await
            .expect("读取工具应返回结果而不是协议错误");

        assert_eq!(result.is_error, Some(true), "应返回 is_error 结果");
        let text = text_of(&result);
        assert!(text.contains("终端不存在"), "错误正文应说明终端不存在：{text}");
        assert!(text.contains(&opened.id), "错误正文应含终端标识：{text}");
    }

    /// 缩放之后读取仍然可用，不 panic。
    #[tokio::test]
    async fn resize_keeps_terminal_readable() {
        let server = MobaxServer::new(host().await, audit());
        let opened = open_local(&server, "/bin/sh").await;

        let resized = server
            .terminal_resize(Parameters(TerminalResizeParams {
                id: opened.id.clone(),
                cols: 120,
                rows: 40,
            }))
            .await
            .expect("缩放终端工具应返回结果");
        assert_eq!(
            resized.is_error,
            Some(false),
            "缩放终端不应报错：{}",
            text_of(&resized)
        );

        write_text(&server, &opened.id, "echo AFTER_RESIZE\n").await;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut cursor = 0u64;
        let mut collected = String::new();
        loop {
            let chunk = read_once(&server, &opened.id, cursor, true).await;
            assert_eq!(chunk.start, cursor, "缩放后游标仍应连续");
            collected.push_str(&chunk.text);
            cursor = chunk.next;
            if collected.contains("AFTER_RESIZE") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "缩放后 5 秒内应能读到输出，实际收到：{collected:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// SSH 参数转 core 选项时，agent 认证要明确报不支持，而不是静默兜底。
    #[tokio::test]
    async fn ssh_agent_auth_is_rejected() {
        let server = MobaxServer::new(host().await, audit());
        let spec = TerminalSpec::Ssh {
            cols: 80,
            rows: 24,
            ssh: SshSpec {
                host: "127.0.0.1".into(),
                port: 22,
                username: "nobody".into(),
                cols: 80,
                rows: 24,
                term: "xterm-256color".into(),
                accept_unknown_host_key: false,
                agent_forward: None,
                auth: crate::SshAuthSpec::Agent,
            },
        };

        // 校验错误走协议层 invalid_params，工具层不会当成正常结果
        let error = server
            .terminal_open(Parameters(spec))
            .await
            .expect_err("agent 认证应被拒绝");
        assert!(
            format!("{error:?}").contains("agent"),
            "错误应说明不支持 agent 认证，实际：{error:?}"
        );
    }

    /// 新建会话后能在列表里查到，且协议与凭据方式落成 camelCase 的小写字符串。
    #[tokio::test]
    async fn session_create_shows_up_in_list_with_camel_case_enums() {
        let server = MobaxServer::new(store_host().await, audit());

        let created = make_session(&server, "prod-web").await;
        assert_eq!(created["protocol"], "ssh", "协议应是小写字符串");
        assert_eq!(created["authKind"], "agent", "凭据方式应是小写字符串");

        let listed = structured(
            server
                .session_list(Parameters(SessionListParams {
                    keyword: Some("prod".to_string()),
                }))
                .await
                .expect("会话列表工具应返回结果"),
        );
        assert_eq!(listed["count"], 1, "应查到刚建的会话：{listed}");
        assert_eq!(listed["sessions"][0]["id"], created["id"]);
        assert_eq!(listed["sessions"][0]["protocol"], "ssh");
        assert_eq!(listed["sessions"][0]["authKind"], "agent");
    }

    /// 非法协议要回 invalid_params，正文里列出全部合法取值。
    #[tokio::test]
    async fn session_create_rejects_unknown_protocol() {
        let server = MobaxServer::new(store_host().await, audit());

        let error = server
            .session_create(Parameters(SessionCreateParams {
                name: "bad".to_string(),
                protocol: "ftp".to_string(),
                host: Some("10.0.0.1".to_string()),
                port: None,
                username: None,
                auth_kind: "password".to_string(),
                group_id: None,
                extra: None,
            }))
            .await
            .expect_err("非法协议应被拒绝");

        assert_eq!(error.code, ErrorCode::INVALID_PARAMS, "应回 invalid params");
        let message = error.message.to_string();
        assert!(
            message.contains("ssh、sftp、local、telnet、rdp、vnc、serial、wsl"),
            "错误正文应列出协议合法取值：{message}"
        );
    }

    /// 更新会话改名字后，按标识查到的是新名字。
    #[tokio::test]
    async fn session_update_renames_session() {
        let server = MobaxServer::new(store_host().await, audit());
        let created = make_session(&server, "old-name").await;
        let id = created["id"].as_str().expect("应含标识").to_string();

        let updated = structured(
            server
                .session_update(Parameters(SessionUpdateParams {
                    id: id.clone(),
                    name: "new-name".to_string(),
                    protocol: "ssh".to_string(),
                    host: Some("10.0.0.1".to_string()),
                    port: Some(22),
                    username: Some("root".to_string()),
                    auth_kind: "agent".to_string(),
                    group_id: None,
                    extra: None,
                }))
                .await
                .expect("更新会话工具应返回结果"),
        );
        assert_eq!(updated["name"], "new-name");

        let fetched = structured(
            server
                .session_get(Parameters(SessionGetParams { id }))
                .await
                .expect("会话详情工具应返回结果"),
        );
        assert_eq!(fetched["name"], "new-name", "按标识应查到新名字");
    }

    /// 删除会话之后按标识再查必须拿到 is_error 结果。
    #[tokio::test]
    async fn session_delete_makes_session_get_fail() {
        let server = MobaxServer::new(store_host().await, audit());
        let created = make_session(&server, "to-delete").await;
        let id = created["id"].as_str().expect("应含标识").to_string();

        let deleted = server
            .session_delete(Parameters(SessionDeleteParams { id: id.clone() }))
            .await
            .expect("删除会话工具应返回结果");
        assert_eq!(
            deleted.is_error,
            Some(false),
            "删除不应报错：{}",
            text_of(&deleted)
        );

        let fetched = server
            .session_get(Parameters(SessionGetParams { id }))
            .await
            .expect("查询工具应返回结果而不是协议错误");
        assert_eq!(fetched.is_error, Some(true), "删除后应返回 is_error");
        assert!(
            text_of(&fetched).contains("会话不存在"),
            "错误正文应说明会话不存在：{}",
            text_of(&fetched)
        );
    }

    /// 建分组并把会话移进去之后，会话的 groupId 应变成新分组标识。
    #[tokio::test]
    async fn group_create_and_session_move_updates_group_id() {
        let server = MobaxServer::new(store_host().await, audit());
        let created = make_session(&server, "movable").await;
        let id = created["id"].as_str().expect("应含标识").to_string();

        let group = structured(
            server
                .group_create(Parameters(GroupCreateParams {
                    name: "生产".to_string(),
                    parent_id: None,
                }))
                .await
                .expect("新建分组工具应返回结果"),
        );
        let group_id = group["id"].as_str().expect("分组应含标识").to_string();

        let moved = server
            .session_move(Parameters(SessionMoveParams {
                id: id.clone(),
                group_id: Some(group_id.clone()),
            }))
            .await
            .expect("移动会话工具应返回结果");
        assert_eq!(
            moved.is_error,
            Some(false),
            "移动会话不应报错：{}",
            text_of(&moved)
        );

        let fetched = structured(
            server
                .session_get(Parameters(SessionGetParams { id }))
                .await
                .expect("会话详情工具应返回结果"),
        );
        assert_eq!(fetched["groupId"], group_id, "会话应归到新分组下");
    }

    /// 删除分组之后分组列表里不再有它。
    #[tokio::test]
    async fn group_delete_removes_group() {
        let host = store_host().await;
        let server = MobaxServer::new(host.clone(), audit());

        let group = structured(
            server
                .group_create(Parameters(GroupCreateParams {
                    name: "临时".to_string(),
                    parent_id: None,
                }))
                .await
                .expect("新建分组工具应返回结果"),
        );
        let group_id = group["id"].as_str().expect("分组应含标识").to_string();

        let deleted = server
            .group_delete(Parameters(GroupDeleteParams {
                id: group_id.clone(),
            }))
            .await
            .expect("删除分组工具应返回结果");
        assert_eq!(
            deleted.is_error,
            Some(false),
            "删除分组不应报错：{}",
            text_of(&deleted)
        );

        let groups = host.store().list_groups().await.expect("应能列分组");
        assert!(
            !groups.iter().any(|item| item.id == group_id),
            "分组应已删除：{groups:?}"
        );
    }

    /// theme 能写能读；mcp.* 必须被拦下且不落库。
    #[tokio::test]
    async fn settings_round_trip_and_mcp_keys_are_blocked() {
        let host = store_host().await;
        let server = MobaxServer::new(host.clone(), audit());

        let written = server
            .settings_set(Parameters(SettingsSetParams {
                key: "theme".to_string(),
                value: "dark".to_string(),
            }))
            .await
            .expect("写入设置工具应返回结果");
        assert_eq!(
            written.is_error,
            Some(false),
            "普通设置应能写入：{}",
            text_of(&written)
        );

        let listed = structured(
            server
                .settings_list()
                .await
                .expect("列出设置工具应返回结果"),
        );
        assert_eq!(listed["count"], 1, "应只有一条设置：{listed}");
        assert_eq!(listed["settings"][0]["key"], "theme");
        assert_eq!(listed["settings"][0]["value"], "dark");

        let denied = server
            .settings_set(Parameters(SettingsSetParams {
                key: "mcp.enabled".to_string(),
                value: "false".to_string(),
            }))
            .await
            .expect_err("mcp.* 开头的键必须被拦住");
        assert_eq!(denied.code, ErrorCode::INVALID_PARAMS, "应回 invalid params");
        assert!(
            denied
                .message
                .contains("不允许通过 MCP 修改 MCP 自身配置"),
            "错误正文应说明这是策略拦截：{}",
            denied.message
        );

        let settings = host.store().settings_get_all().await.expect("应能读设置");
        assert!(
            !settings.iter().any(|(key, _)| key.as_str() == "mcp.enabled"),
            "被拦下的键不应落库：{settings:?}"
        );
    }

    /// 真终端宿主同样要拦 mcp.* 设置键，拦截不能只落在其中一份实现里。
    #[tokio::test]
    async fn terminal_host_blocks_mcp_settings_too() {
        let server = MobaxServer::new(host().await, audit());

        let denied = server
            .settings_set(Parameters(SettingsSetParams {
                key: "mcp.port".to_string(),
                value: "1".to_string(),
            }))
            .await
            .expect_err("终端宿主也必须拦住 mcp.*");

        assert_eq!(denied.code, ErrorCode::INVALID_PARAMS, "应回 invalid params");
        assert!(
            denied
                .message
                .contains("不允许通过 MCP 修改 MCP 自身配置"),
            "错误正文应说明这是策略拦截：{}",
            denied.message
        );
    }

    /// 工具结果为错误时，审计要记下失败正文而不是当成成功。
    #[tokio::test]
    async fn record_call_marks_error_result_and_keeps_text() {
        let audit = Arc::new(RecordingAudit::default());
        let server = MobaxServer::new(host().await, audit.clone());
        let result: Result<CallToolResponse, ErrorData> = Ok(CallToolResponse::Complete(
            CallToolResult::error(vec![ContentBlock::text("终端不存在：term-1")]),
        ));

        server
            .record_call(
                "terminal_write",
                "terminal_write{id:6B, data:4B}",
                Instant::now(),
                &result,
            )
            .await;

        let calls = audit.calls();
        assert_eq!(calls.len(), 1, "一次调用应只记一条审计");
        assert_eq!(calls[0].tool, "terminal_write");
        assert_eq!(calls[0].digest, "terminal_write{id:6B, data:4B}");
        assert!(!calls[0].ok, "is_error 结果必须记成失败");
        assert_eq!(calls[0].error.as_deref(), Some("终端不存在：term-1"));
        assert!(calls[0].duration_ms >= 0, "耗时应是非负毫秒数");
    }

    /// 成功结果不写错误正文；失败正文超过 200 字时按字符截断。
    #[tokio::test]
    async fn record_call_marks_success_and_truncates_long_error() {
        let audit = Arc::new(RecordingAudit::default());
        let server = MobaxServer::new(host().await, audit.clone());

        let ok: Result<CallToolResponse, ErrorData> = Ok(CallToolResponse::Complete(
            CallToolResult::success(vec![ContentBlock::text("已写入设置：theme")]),
        ));
        server
            .record_call("settings_set", "settings_set{key:5B, value:4B}", Instant::now(), &ok)
            .await;

        let long = "错".repeat(300);
        let failed: Result<CallToolResponse, ErrorData> =
            Err(ErrorData::invalid_params(long, None));
        server
            .record_call("session_get", "session_get{id:3B}", Instant::now(), &failed)
            .await;

        let calls = audit.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].ok, "成功结果应记成 ok");
        assert!(calls[0].error.is_none(), "成功结果不应带错误正文");
        assert!(!calls[1].ok, "协议错误应记成失败");
        let error = calls[1].error.as_deref().expect("失败应带错误正文");
        assert_eq!(error.chars().count(), 200, "错误正文应截到 200 字");
        assert!(!calls[0].digest.contains("theme"), "摘要里不应出现设置值");
    }

    /// 从资源读取结果里抠出第一条文本正文。
    ///
    /// 参数：result 为读资源的结果
    /// 核心流程：断言恰好有一条内容，再按文本变体取出正文；
    /// 拿到别的变体直接 panic，避免测试悄悄放过类型错误。
    fn resource_text(result: &ReadResourceResult) -> String {
        assert_eq!(result.contents.len(), 1, "一次读取应只回一条内容");
        match &result.contents[0] {
            ResourceContents::TextResourceContents { text, .. } => text.clone(),
            other => panic!("应返回文本内容：{other:?}"),
        }
    }

    /// 读一个资源并返回正文。
    ///
    /// 参数：server 为服务端；uri 为资源地址
    /// 核心流程：走与服务端同一套分派逻辑，失败时把错误带进 panic 消息。
    async fn read_text(server: &MobaxServer, uri: &str) -> String {
        let result = server.read_uri(uri).await.expect("资源应能读出");
        resource_text(&result)
    }

    /// 静态资源应正好五个，且会话与设置两项在列。
    #[tokio::test]
    async fn list_resources_returns_five_static_resources() {
        let server = MobaxServer::new(store_host().await, audit());

        let resources = server.resource_list();

        assert_eq!(resources.len(), 5, "静态资源应正好五个");
        let uris: Vec<&str> = resources.iter().map(|item| item.uri.as_str()).collect();
        assert!(uris.contains(&"mobax://sessions"), "应含会话资源：{uris:?}");
        assert!(uris.contains(&"mobax://settings"), "应含设置资源：{uris:?}");
        for item in &resources {
            assert!(
                item.mime_type.is_some(),
                "每条资源都要有 MIME 类型：{item:?}"
            );
            assert!(
                item.name.chars().count() <= 30,
                "资源名称不超过 30 字：{}",
                item.name
            );
        }
    }

    /// 会话资源要能读出刚建的两条会话。
    #[tokio::test]
    async fn read_sessions_resource_lists_both_sessions() {
        let server = MobaxServer::new(store_host().await, audit());
        make_session(&server, "prod-web").await;
        make_session(&server, "test-db").await;

        let body = read_text(&server, "mobax://sessions").await;

        assert!(body.contains("prod-web"), "正文应含第一条会话名：{body}");
        assert!(body.contains("test-db"), "正文应含第二条会话名：{body}");
    }

    /// 设置资源绝不能把 MCP 令牌带出去。
    #[tokio::test]
    async fn read_settings_resource_hides_mcp_token() {
        let host = store_host().await;
        host.store()
            .settings_set(MCP_TOKEN_KEY, "SEKRET-TOKEN-VALUE")
            .await
            .expect("应能直接写令牌");
        host.store()
            .settings_set("theme", "dark")
            .await
            .expect("应能直接写主题");
        let server = MobaxServer::new(host, audit());

        let body = read_text(&server, "mobax://settings").await;

        assert!(body.contains("dark"), "普通设置应出现：{body}");
        assert!(
            !body.contains("SEKRET-TOKEN-VALUE"),
            "令牌值不得出现在资源里：{body}"
        );
        assert!(!body.contains(MCP_TOKEN_KEY), "令牌键也不应出现：{body}");
    }

    /// 不存在的会话要回 resource_not_found 协议错误。
    #[tokio::test]
    async fn read_missing_session_reports_resource_not_found() {
        let server = MobaxServer::new(store_host().await, audit());

        let error = server
            .read_uri("mobax://sessions/不存在的id")
            .await
            .expect_err("不存在的会话必须报错");

        assert_eq!(
            error.code,
            ErrorCode::RESOURCE_NOT_FOUND,
            "应回 resource not found：{error:?}"
        );
    }

    /// 资源模板应正好两个，指向单会话与终端屏幕。
    #[tokio::test]
    async fn list_resource_templates_returns_two_templates() {
        let server = MobaxServer::new(store_host().await, audit());

        let templates = server.resource_template_list();

        assert_eq!(templates.len(), 2, "资源模板应正好两个");
        let templates_uri: Vec<&str> = templates
            .iter()
            .map(|item| item.uri_template.as_str())
            .collect();
        assert!(
            templates_uri.contains(&"mobax://sessions/{id}"),
            "应含单会话模板：{templates_uri:?}"
        );
        assert!(
            templates_uri.contains(&"mobax://terminals/{id}/screen"),
            "应含终端屏幕模板：{templates_uri:?}"
        );
    }

    /// 三个提示模板都能取到，排查模板正文要写清开终端的步骤。
    #[tokio::test]
    async fn prompts_are_listed_and_triage_mentions_terminal_open() {
        let server = MobaxServer::new(store_host().await, audit());
        let arguments = serde_json::json!({ "sessionId": "s-1" })
            .as_object()
            .cloned()
            .expect("入参应是对象");

        let prompts = server.prompt_list();
        assert_eq!(prompts.len(), 3, "提示模板应正好三个");

        for name in [PROMPT_TRIAGE, PROMPT_INSPECT, PROMPT_SAFE] {
            let result = server
                .prompt_get(name, Some(&arguments))
                .expect("模板应能取到");
            assert_eq!(result.messages.len(), 1, "每个模板只回一条消息：{name}");
        }

        let triage = server
            .prompt_get(PROMPT_TRIAGE, Some(&arguments))
            .expect("排查模板应能取到");
        let text = triage.messages[0]
            .content
            .as_text()
            .map(|content| content.text.clone())
            .expect("应含文本正文");
        assert!(
            text.contains("terminal_open"),
            "正文应含 terminal_open：{text}"
        );
        assert!(text.contains("s-1"), "正文应带上会话标识：{text}");
    }

    /// 无关键词时隧道列表返回全部配置，total 与 count 都是三条。
    #[tokio::test]
    async fn tunnel_list_without_keyword_returns_everything() {
        let (server, _host) = tunnel_fixture().await;

        let listed = list_tunnels(&server, None, None, None).await;

        assert_eq!(listed["total"], 3, "过滤后总数应是三条：{listed}");
        assert_eq!(listed["count"], 3, "本页也应返回三条：{listed}");
        assert_eq!(listed["offset"], 0, "缺省偏移应为 0：{listed}");
        assert_eq!(listed["limit"], 20, "缺省条数应为 20：{listed}");
    }

    /// 关键词按名称、类型、监听、目标与会话过滤。
    #[tokio::test]
    async fn tunnel_list_filters_by_name_kind_listen_target_and_session() {
        let (server, _host) = tunnel_fixture().await;
        let all = list_tunnels(&server, None, None, None).await;
        let session_id = all["tunnels"][0]["sessionId"]
            .as_str()
            .expect("应含会话标识")
            .to_string();

        let by_name = list_tunnels(&server, Some("redis"), None, None).await;
        assert_eq!(by_name["total"], 1, "名称应命中一条：{by_name}");
        assert_eq!(by_name["count"], 1, "本页也应是一条：{by_name}");
        assert_eq!(by_name["tunnels"][0]["name"], "beta-redis");

        let by_port = list_tunnels(&server, Some("8002"), None, None).await;
        assert_eq!(by_port["total"], 1, "监听端口应参与匹配：{by_port}");
        assert_eq!(by_port["tunnels"][0]["name"], "beta-redis");

        let by_listen = list_tunnels(&server, Some("0.0.0.1"), None, None).await;
        assert_eq!(
            by_listen["total"], 0,
            "不存在的监听地址不应命中：{by_listen}"
        );

        let by_target = list_tunnels(&server, Some("10.0.0.9:5432"), None, None).await;
        assert_eq!(by_target["total"], 3, "目标连接串应命中三条：{by_target}");

        let by_kind = list_tunnels(&server, Some("远程"), None, None).await;
        assert_eq!(by_kind["total"], 1, "中文类型应命中 remote：{by_kind}");
        assert_eq!(by_kind["tunnels"][0]["name"], "beta-redis");

        let by_kind_en = list_tunnels(&server, Some("dynamic"), None, None).await;
        assert_eq!(
            by_kind_en["total"], 1,
            "英文类型应命中 dynamic：{by_kind_en}"
        );
        assert_eq!(by_kind_en["tunnels"][0]["name"], "gamma-web");

        let by_session = list_tunnels(&server, Some(&session_id), None, None).await;
        assert_eq!(by_session["total"], 3, "会话标识应命中三条：{by_session}");

        let blank = list_tunnels(&server, Some("   "), None, None).await;
        assert_eq!(blank["total"], 3, "空白关键词应按不过滤处理：{blank}");
    }

    /// 分页按原顺序切片，offset 越过尾部返回空页且不报错。
    #[tokio::test]
    async fn tunnel_list_paginates_and_tolerates_out_of_range_offset() {
        let (server, _host) = tunnel_fixture().await;

        let page = list_tunnels(&server, None, Some(1), Some(1)).await;
        assert_eq!(page["total"], 3, "总数不受分页影响：{page}");
        assert_eq!(page["count"], 1, "每页一条：{page}");
        assert_eq!(page["offset"], 1, "偏移应回显：{page}");
        assert_eq!(page["limit"], 1, "条数应回显：{page}");
        assert_eq!(
            page["tunnels"][0]["name"], "beta-redis",
            "应返回原顺序第二条：{page}"
        );

        let beyond = list_tunnels(&server, None, None, Some(99)).await;
        assert_eq!(beyond["total"], 3, "总数仍应是三条：{beyond}");
        assert_eq!(beyond["count"], 0, "越过尾部应返回空页：{beyond}");
        assert!(
            beyond["tunnels"]
                .as_array()
                .expect("应含隧道数组")
                .is_empty(),
            "空页的数组应为空：{beyond}"
        );
    }

    /// limit 为 0 时按默认 20 处理，超过上限按 200 截断。
    #[tokio::test]
    async fn tunnel_list_limit_zero_uses_default_and_caps_at_max() {
        let (server, _host) = tunnel_fixture().await;

        let zero = list_tunnels(&server, None, Some(0), None).await;
        assert_eq!(zero["limit"], 20, "limit 为 0 应按默认 20 处理：{zero}");
        assert_eq!(zero["count"], 3, "三条应全部返回：{zero}");

        let huge = list_tunnels(&server, None, Some(500), None).await;
        assert_eq!(huge["limit"], 200, "超过上限应按 200 截断：{huge}");
    }

    /// 资源与提示模板的加入不能改动工具清单。
    #[test]
    fn tool_router_still_has_twenty_one_tools() {
        let router = MobaxServer::tool_router();

        assert_eq!(router.list_all().len(), 21, "工具数量应保持 21 个");
    }
}
