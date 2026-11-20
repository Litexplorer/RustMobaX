//! MCP 的 Streamable HTTP 传输层：Axum 路由、Bearer 鉴权与 rmcp 服务的接线。
//!
//! 这一层只做三件事：在回环地址上监听、把 `/mcp` 上的请求交给 rmcp 的流式 HTTP
//! 服务、在请求进入服务之前校验 `Authorization` 头。工具定义全在
//! [`crate::server`]，传输层不认识任何具体工具。

use std::{io, net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio::task::JoinHandle;

use crate::{Host, McpAudit, server::MobaxServer};

/// MCP 服务端的启动参数。
///
/// 无参数
/// 核心流程：地址决定监听面，令牌决定谁能进。两者都必填，不提供默认值，避免
/// 无鉴权监听被当成默认行为。
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    /// 监听地址，必须是回环地址，端口填 0 时由内核分配空闲端口。
    pub bind_addr: SocketAddr,
    /// 客户端必须携带的 Bearer 令牌。
    pub token: String,
}

/// 启动 MCP 服务并带回实际监听地址。
///
/// 参数：host 为宿主能力；audit 为工具调用审计；config 为监听地址与访问令牌
/// 核心流程：先确认地址是回环地址，不是就直接拒绝，绝不把 MCP 暴露到外部网卡；
/// 再绑监听套接字并读取真实地址，端口填 0 时也能拿到内核分配的端口；随后在后台任务
/// 里跑 Axum，返回地址与任务句柄。调用方丢弃句柄即可停止服务。
pub async fn serve_on(
    host: Arc<dyn Host>,
    audit: Arc<dyn McpAudit>,
    config: McpServerConfig,
) -> io::Result<(SocketAddr, JoinHandle<()>)> {
    if !config.bind_addr.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "MCP 服务只允许监听回环地址",
        ));
    }

    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    let bound_addr = listener.local_addr()?;
    let app = build_router(host, audit, &config);

    let handle = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            eprintln!("MCP 服务已退出：{error}");
        }
    });

    Ok((bound_addr, handle))
}

/// 启动 MCP 服务并丢弃实际监听地址。
///
/// 参数：host 为宿主能力；audit 为工具调用审计；config 为监听地址与访问令牌
/// 核心流程：转交 [`serve_on`]，只在调用方指定固定端口、不关心实际绑定地址时使用。
pub async fn serve(
    host: Arc<dyn Host>,
    audit: Arc<dyn McpAudit>,
    config: McpServerConfig,
) -> io::Result<JoinHandle<()>> {
    let (_, handle) = serve_on(host, audit, config).await?;
    Ok(handle)
}

/// 组装路由：`/mcp` 上的全部方法交给 rmcp，外层统一做鉴权。
///
/// 参数：host 为宿主能力；audit 为工具调用审计；config 为监听地址与访问令牌
/// 核心流程：用工厂闭包为每个连接生成一个服务端实例，会话管理器用内存实现；
/// `nest_service` 让 POST、GET、DELETE 都落到 rmcp 的传输服务上，再由
/// `layer` 把鉴权中间件套在整张路由表外面，业务路径上不再有绕过鉴权的分支。
fn build_router(host: Arc<dyn Host>, audit: Arc<dyn McpAudit>, config: &McpServerConfig) -> Router {
    let service: StreamableHttpService<MobaxServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(MobaxServer::new(host.clone(), audit.clone())),
            Arc::new(LocalSessionManager::default()),
            transport_config(),
        );

    Router::new().nest_service("/mcp", service).layer(
        middleware::from_fn_with_state(Arc::new(config.token.clone()), require_bearer),
    )
}

/// 生成 rmcp 的传输配置。
///
/// 参数：无
/// 核心流程：开会话模式并保留下行事件流。无状态模式虽然更简单，但 rmcp 在无状态分支
/// 里只允许 POST（`tower.rs:1967` 的方法分派表），客户端发来的通知也会被直接丢弃
/// （`tower.rs:2492`），于是 `notifications/tools/list_changed`、资源变更通知都没有投递
/// 通道，长任务也收不到取消。因此这里显式打开 legacy 会话模式，让 POST 响应走 SSE、
/// GET 作为常驻下行流；`json_response` 在会话模式下不生效，显式设为 false 以免误导后来人；
/// 心跳给 15 秒，长连接空闲时不至于被中间设备当成死连接回收。
fn transport_config() -> StreamableHttpServerConfig {
    StreamableHttpServerConfig::default()
        .with_legacy_session_mode(true)
        .with_json_response(false)
        .with_stateless_protocol_metadata_required(false)
        .with_sse_keep_alive(Some(Duration::from_secs(15)))
}

/// 鉴权中间件：校验 `Authorization: Bearer` 头。
///
/// 参数：expected 为配置里的令牌；request 为入站请求；next 为下游路由
/// 核心流程：取出 Authorization 头，按空格切成方案与令牌两段，方案不区分大小写；
/// 令牌比对走等长比较，不匹配或缺失都返回 401，且响应体只写结论，不回显期望值。
async fn require_bearer(
    State(expected): State<Arc<String>>,
    request: Request,
    next: Next,
) -> Response {
    let provided = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_bearer);

    match provided {
        Some(token) if token_matches(&expected, token) => next.run(request).await,
        _ => unauthorized(),
    }
}

/// 从 Authorization 头里取出令牌部分。
///
/// 参数：value 为头的原始字符串
/// 核心流程：按第一个空格切开，方案名忽略大小写与 Bearer 比较，令牌去掉首尾空白；
/// 方案不对返回 None，交给调用方统一按未授权处理。
fn parse_bearer(value: &str) -> Option<&str> {
    let (scheme, token) = value.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    Some(token)
}

/// 等长比较两个令牌。
///
/// 参数：expected 为配置里的令牌；provided 为请求带来的令牌
/// 核心流程：长度不同直接判否；长度相同时逐字节异或累加，比较次数与内容无关，
/// 避免按比较耗时猜出令牌前缀。配置为空令牌时一律判否，防止配错后变成人人可进。
fn token_matches(expected: &str, provided: &str) -> bool {
    let expected = expected.as_bytes();
    let provided = provided.as_bytes();
    if expected.is_empty() || expected.len() != provided.len() {
        return false;
    }

    let mut diff = 0u8;
    for (left, right) in expected.iter().zip(provided.iter()) {
        diff |= left ^ right;
    }
    diff == 0
}

/// 构造 401 响应。
///
/// 参数：无
/// 核心流程：状态码 401，附 `WWW-Authenticate: Bearer` 提示客户端应当怎么带令牌，
/// 正文只写结论，不含令牌本身。
fn unauthorized() -> Response {
    let mut response = (StatusCode::UNAUTHORIZED, "未授权的请求").into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}
