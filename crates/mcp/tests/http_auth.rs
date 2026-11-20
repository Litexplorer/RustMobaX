//! 传输层集成测试：真的起 HTTP 服务，真的用 TCP 发请求。
//!
//! 客户端是手写的 HTTP/1.1，只依赖 tokio 的 TCP 与读写，不引入额外依赖，也不走 TLS。
//! 服务端走会话模式，POST 的响应体是 `text/event-stream`，因此响应解析要先解 SSE 帧；
//! 每个请求都带 `Connection: close`，服务端回完就关连接，读到 EOF 即视为一次交互结束。

use std::{net::SocketAddr, sync::Arc, time::Duration};

use rustmobax_core::{AuthKind, Protocol, SessionInput, Store};
use rustmobax_mcp::{Host, McpServerConfig, NoopAudit, StoreHost, serve_on};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    task::JoinHandle,
};

/// 测试用令牌。
const TOKEN: &str = "test-token-0123456789";

/// 握手请求，协议版本取 2025-06-18。
const INIT_BODY: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}"#;

/// 列工具请求。
const LIST_TOOLS_BODY: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;

/// 调探针工具请求。
const CALL_PING_BODY: &str = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"server_ping","arguments":{}}}"#;

/// 一次 HTTP 交互的结果。
///
/// 无参数
/// 核心流程：把状态码、响应头与（已解块的）响应体放在一起，测试按需取用。
struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl HttpResponse {
    /// 按名字取响应头的值。
    ///
    /// 参数：name 为头名，比较时不区分大小写
    /// 核心流程：线性查找第一个同名头，取不到返回 None。
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// 一次握手得到的会话信息。
///
/// 无参数
/// 核心流程：会话标识与协商到的协议版本后续要原样带在请求头上，所以在这里一并存下。
struct Session {
    id: String,
    protocol_version: String,
}

/// 拼出带令牌的 Authorization 头值。
///
/// 参数：无
/// 核心流程：固定拼接，避免每个用例各自写一遍带空格的字符串。
fn bearer() -> String {
    format!("Bearer {TOKEN}")
}

/// 起一个只监听回环、端口由内核分配、库为空的测试服务。
///
/// 参数：无
/// 核心流程：转交 [`start_server_with`]，不预置任何会话。
async fn start_server() -> (SocketAddr, JoinHandle<()>) {
    start_server_with(&[]).await
}

/// 起一个测试服务并按名字预置若干会话。
///
/// 参数：names 为会话名列表
/// 核心流程：先往内存库里逐条建会话，再把它包成宿主交给服务端，最后启动监听；
/// 会话名即查询时用来区分的数据。
async fn start_server_with(names: &[&str]) -> (SocketAddr, JoinHandle<()>) {
    let store = Store::open_in_memory().await.expect("内存库应能打开");
    for name in names {
        store
            .create_session(&ssh_input(name))
            .await
            .expect("应能建会话");
    }

    let host: Arc<dyn Host> = Arc::new(StoreHost::new(store));
    let config = McpServerConfig {
        bind_addr: "127.0.0.1:0".parse().expect("回环地址应能解析"),
        token: TOKEN.to_string(),
    };

    serve_on(host, Arc::new(NoopAudit), config)
        .await
        .expect("服务应能启动")
}

/// 构造一条最小可用的 SSH 会话入参。
///
/// 参数：name 为会话名
/// 核心流程：主机填固定值，其余可选字段留空，满足存储层校验即可。
fn ssh_input(name: &str) -> SessionInput {
    SessionInput {
        name: name.to_string(),
        group_id: None,
        protocol: Protocol::Ssh,
        host: Some("10.0.0.1".into()),
        port: Some(22),
        username: Some("root".into()),
        auth_kind: AuthKind::Agent,
        extra: None,
    }
}

/// 拼一个工具调用请求体。
///
/// 参数：id 为 JSON-RPC 标识；name 为工具名；arguments 为已序列化的参数对象
/// 核心流程：按协议模板拼字符串，参数原样嵌入，方便测试自行构造各种入参。
fn tool_call(id: u32, name: &str, arguments: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{name}","arguments":{arguments}}}}}"#
    )
}

/// 解析 SSE 响应体，取回其中的 JSON-RPC 消息。
///
/// 参数：body 为已解块的 SSE 正文
/// 核心流程：逐行扫描，只认 `data:` 开头的行，取冒号之后的负载逐条反序列化；注释行、
/// 空行、`id:` 与 `retry:` 这类字段行、以及解析不了的负载一律跳过。
fn sse_events(body: &str) -> Vec<serde_json::Value> {
    body.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|payload| serde_json::from_str::<serde_json::Value>(payload.trim()).ok())
        .collect()
}

/// 从响应里挑出 id 匹配的那条 JSON-RPC 消息。
///
/// 参数：response 为一次交互的结果；id 为期望的 JSON-RPC 标识
/// 核心流程：先解 SSE 帧，再按 id 找第一条匹配的消息，找不到返回 None 交给调用方断言。
fn event_by_id(response: &HttpResponse, id: u32) -> Option<serde_json::Value> {
    sse_events(&response.body)
        .into_iter()
        .find(|event| event["id"] == id)
}

/// 发一次 `/mcp` 请求并取回完整响应。
///
/// 参数：addr 为服务地址；method 为 HTTP 方法；headers 为附加请求头；body 为可选请求体
/// 核心流程：连上 TCP 后手写请求行与头，有请求体时按字节数写 Content-Length，并让 Accept
/// 同时声明 JSON 与事件流以满足 rmcp 的入站校验；随后读到 EOF 或已收到一条带 id 的
/// JSON-RPC 事件为止，交给 [`split_response`] 拆包。
async fn send(
    addr: SocketAddr,
    method: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> HttpResponse {
    let mut stream = TcpStream::connect(addr).await.expect("应能连上测试服务");
    let mut request = format!("{method} /mcp HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if body.is_some() {
        request.push_str("Content-Type: application/json\r\n");
        request.push_str("Accept: application/json, text/event-stream\r\n");
    }
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body) = body {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    if let Some(body) = body {
        request.push_str(body);
    }

    stream
        .write_all(request.as_bytes())
        .await
        .expect("应能写出请求");
    stream.flush().await.expect("应能刷出请求");

    read_response(&mut stream).await
}

/// 读一次响应，直到 EOF 或已收到一条带 id 的 JSON-RPC 事件。
///
/// 参数：stream 为已连上的 TCP 流
/// 核心流程：循环读块并累积，每读完一块就尝试解出 SSE 事件，一旦出现带 id 的消息即停止，
/// 这样会话模式下的长连接不会把测试拖住；读到 EOF、出错或超过 15 秒都按已读内容收尾。
async fn read_response(stream: &mut TcpStream) -> HttpResponse {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut raw = Vec::new();
    loop {
        let mut buf = [0u8; 8192];
        match tokio::time::timeout_at(deadline, stream.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(read)) => {
                raw.extend_from_slice(&buf[..read]);
                let text = String::from_utf8_lossy(&raw);
                let done = text.split_once("\r\n\r\n").is_some_and(|(_, body)| {
                    sse_events(body)
                        .iter()
                        .any(|event| event.get("id").is_some())
                });
                if done {
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }

    split_response(&raw)
}

/// 打开下行事件流并读回第一段内容。
///
/// 参数：addr 为服务地址；session 为已握手的会话
/// 核心流程：带会话标识与事件流 Accept 头发 GET，只读一次并在 20 秒内等待首个字节，
/// 读到什么就返回什么，超时或出错返回空串，由调用方断言。
async fn read_stream_head(addr: SocketAddr, session: &Session) -> String {
    let mut stream = TcpStream::connect(addr).await.expect("应能连上测试服务");
    let request = format!(
        "GET /mcp HTTP/1.1\r\nHost: {addr}\r\nAccept: text/event-stream\r\nConnection: close\r\nAuthorization: {}\r\nMcp-Session-Id: {}\r\nMCP-Protocol-Version: {}\r\n\r\n",
        bearer(),
        session.id,
        session.protocol_version
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("应能写出请求");
    stream.flush().await.expect("应能刷出请求");

    let mut buf = [0u8; 4096];
    match tokio::time::timeout(Duration::from_secs(20), stream.read(&mut buf)).await {
        Ok(Ok(read)) => String::from_utf8_lossy(&buf[..read]).to_string(),
        _ => String::new(),
    }
}

/// 完成一次握手并带回会话与原始响应。
///
/// 参数：addr 为服务地址
/// 核心流程：先发 initialize，断言状态码，再从响应头取 `mcp-session-id`、从 SSE 事件里取
/// 协商后的协议版本；两者缺一都算握手失败，直接让用例响亮地挂掉。
async fn handshake_response(addr: SocketAddr) -> (Session, HttpResponse) {
    let authorization = bearer();
    let response = send(
        addr,
        "POST",
        &[("Authorization", authorization.as_str())],
        Some(INIT_BODY),
    )
    .await;

    assert_eq!(
        response.status, 200,
        "握手应成功，响应体为：{}",
        response.body
    );
    let id = response
        .header("mcp-session-id")
        .expect("握手响应应含 mcp-session-id")
        .to_string();
    let negotiated = event_by_id(&response, 1).expect("握手响应应含 id 为 1 的消息");
    let protocol_version = negotiated["result"]["protocolVersion"]
        .as_str()
        .expect("握手结果应含协商后的协议版本")
        .to_string();

    (
        Session {
            id,
            protocol_version,
        },
        response,
    )
}

/// 完成一次握手，只带回会话信息。
///
/// 参数：addr 为服务地址
/// 核心流程：转交 [`handshake_response`] 并丢弃原始响应，供只关心会话头的用例使用。
async fn handshake(addr: SocketAddr) -> Session {
    handshake_response(addr).await.0
}

/// 带着令牌、会话标识与协议版本发一次 POST。
///
/// 参数：addr 为服务地址；session 为已握手的会话；body 为 JSON-RPC 请求体
/// 核心流程：把会话模式要求的三个头一次带齐，避免各用例各写一份。
async fn post_with_session(addr: SocketAddr, session: &Session, body: &str) -> HttpResponse {
    let authorization = bearer();
    send(
        addr,
        "POST",
        &[
            ("Authorization", authorization.as_str()),
            ("Mcp-Session-Id", session.id.as_str()),
            ("MCP-Protocol-Version", session.protocol_version.as_str()),
        ],
        Some(body),
    )
    .await
}

/// 把原始响应字节拆成状态码、响应头与响应体。
///
/// 参数：raw 为读到的完整响应
/// 核心流程：按首个空行切开头部与正文，状态行取第二段解析成数字，头部逐行按冒号切分；
/// 正文若被标记为分块传输就先解块，否则原样返回。
fn split_response(raw: &[u8]) -> HttpResponse {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text.split_once("\r\n\r\n").expect("响应应含头体分隔");

    let mut lines = head.split("\r\n");
    let status_line = lines.next().expect("响应应有状态行");
    let status = status_line
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .expect("状态行应含数字状态码");

    let mut headers = Vec::new();
    let mut chunked = false;
    for line in lines {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().to_string();
        if key.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
        {
            chunked = true;
        }
        headers.push((key.to_string(), value));
    }

    let body = if chunked {
        dechunk(body)
    } else {
        body.to_string()
    };

    HttpResponse {
        status,
        headers,
        body,
    }
}

/// 解开 HTTP 分块传输编码。
///
/// 参数：body 为分块编码的正文
/// 核心流程：每轮读一行十六进制长度，按长度切出数据块后跳过行尾，遇到零长度块即停；
/// 长度行不合法或数据块不完整时提前收尾，已解出的部分照常返回。
fn dechunk(body: &str) -> String {
    let mut rest = body;
    let mut out = String::new();
    loop {
        let Some((size_line, after)) = rest.split_once("\r\n") else {
            break;
        };
        let hex = size_line.split(';').next().unwrap_or_default().trim();
        let size = match usize::from_str_radix(hex, 16) {
            Ok(size) => size,
            Err(_) => break,
        };
        if size == 0 {
            break;
        }
        let Some(chunk) = after.get(..size) else {
            break;
        };
        out.push_str(chunk);
        rest = after
            .get(size..)
            .unwrap_or_default()
            .trim_start_matches("\r\n");
    }
    out
}

#[tokio::test]
async fn missing_authorization_is_rejected() {
    let (addr, handle) = start_server().await;

    let response = send(addr, "POST", &[], Some(INIT_BODY)).await;

    assert_eq!(response.status, 401, "缺 Authorization 头应返回 401");
    assert!(
        !response.body.contains(TOKEN),
        "401 响应体不得回显期望的令牌，实际为：{}",
        response.body
    );
    handle.abort();
}

#[tokio::test]
async fn wrong_token_is_rejected() {
    let (addr, handle) = start_server().await;

    let response = send(
        addr,
        "POST",
        &[("Authorization", "Bearer wrong-token")],
        Some(INIT_BODY),
    )
    .await;

    assert_eq!(response.status, 401, "令牌不匹配应返回 401");
    assert!(
        !response.body.contains(TOKEN),
        "401 响应体不得回显期望的令牌，实际为：{}",
        response.body
    );
    handle.abort();
}

#[tokio::test]
async fn initialize_succeeds_with_token() {
    let (addr, handle) = start_server().await;

    let (session, response) = handshake_response(addr).await;

    assert!(!session.id.is_empty(), "握手应带回非空的会话标识");
    assert!(
        !session.protocol_version.is_empty(),
        "握手应协商出协议版本"
    );
    let initialized = event_by_id(&response, 1).expect("握手响应应含 id 为 1 的消息");
    assert!(
        initialized["result"].is_object(),
        "响应应含 result 对象，实际为：{}",
        response.body
    );
    assert_eq!(
        initialized["result"]["serverInfo"]["name"], "rustmobax-mcp",
        "握手结果应带上服务端身份"
    );
    handle.abort();
}

#[tokio::test]
async fn missing_session_id_is_rejected() {
    let (addr, handle) = start_server().await;

    let authorization = bearer();
    let response = send(
        addr,
        "POST",
        &[("Authorization", authorization.as_str())],
        Some(LIST_TOOLS_BODY),
    )
    .await;

    assert_eq!(
        response.status, 400,
        "会话模式下缺会话头应被拒，响应体为：{}",
        response.body
    );
    assert!(
        !(200..300).contains(&response.status),
        "缺会话头不能拿到 2xx，实际为：{}",
        response.status
    );
    handle.abort();
}

#[tokio::test]
async fn get_stream_requires_session_id() {
    let (addr, handle) = start_server().await;

    let authorization = bearer();
    let response = send(
        addr,
        "GET",
        &[
            ("Authorization", authorization.as_str()),
            ("Accept", "text/event-stream"),
        ],
        None,
    )
    .await;

    assert_eq!(
        response.status, 400,
        "下行事件流必须带会话标识，响应体为：{}",
        response.body
    );
    handle.abort();
}

#[tokio::test]
async fn get_stream_delivers_keep_alive() {
    let (addr, handle) = start_server().await;
    let session = handshake(addr).await;

    let head = read_stream_head(addr, &session).await;

    assert!(
        head.starts_with("HTTP/1.1 200"),
        "下行事件流应建立成功，实际为：{head}"
    );
    assert!(
        head.contains("id:") || head.contains("data:") || head.contains("retry:"),
        "20 秒内应至少收到一条 SSE 帧或字段行，实际为：{head}"
    );
    handle.abort();
}

#[tokio::test]
async fn tools_list_reports_twenty_one_tools() {
    let (addr, handle) = start_server().await;
    let session = handshake(addr).await;

    let response = post_with_session(addr, &session, LIST_TOOLS_BODY).await;

    assert_eq!(
        response.status, 200,
        "列工具应成功，响应体为：{}",
        response.body
    );
    let listed = event_by_id(&response, 2).expect("列工具响应应含 id 为 2 的消息");
    let tools = listed["result"]["tools"]
        .as_array()
        .expect("列工具结果应含 tools 数组");
    assert_eq!(tools.len(), 21, "当前应有二十一个工具：{}", response.body);

    for name in [
        "server_ping",
        "session_list",
        "session_get",
        "tunnel_list",
        "terminal_open",
        "terminal_read",
        "terminal_write",
        "terminal_resize",
        "terminal_close",
        "terminal_exec",
        "group_create",
        "group_rename",
        "group_delete",
        "group_move",
        "session_create",
        "session_update",
        "session_delete",
        "session_move",
        "session_copy",
        "settings_list",
        "settings_set",
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == name),
            "工具清单应含 {name}，实际为：{}",
            response.body
        );
    }
    handle.abort();
}

#[tokio::test]
async fn ping_tool_reads_through_host() {
    let (addr, handle) = start_server_with(&["probe-host"]).await;
    let session = handshake(addr).await;

    let response = post_with_session(addr, &session, CALL_PING_BODY).await;

    assert_eq!(
        response.status, 200,
        "调探针工具应成功，响应体为：{}",
        response.body
    );
    let called = event_by_id(&response, 3).expect("探针响应应含 id 为 3 的消息");
    assert_eq!(
        called["result"]["structuredContent"]["session_count"], 1,
        "探针应读到宿主里的那一条会话，实际为：{}",
        response.body
    );
    assert!(
        response.body.contains("pong from RustMobaX MCP"),
        "探针应返回固定文案，实际为：{}",
        response.body
    );
    handle.abort();
}

#[tokio::test]
async fn session_list_returns_zero_on_empty_database() {
    let (addr, handle) = start_server().await;
    let session = handshake(addr).await;

    let response = post_with_session(addr, &session, &tool_call(4, "session_list", "{}")).await;

    assert_eq!(
        response.status, 200,
        "列会话应成功，响应体为：{}",
        response.body
    );
    let called = event_by_id(&response, 4).expect("列会话响应应含 id 为 4 的消息");
    assert_eq!(
        called["result"]["isError"], false,
        "空库不是错误，实际为：{}",
        response.body
    );
    assert_eq!(
        called["result"]["structuredContent"]["count"], 0,
        "空库应返回零条，实际为：{}",
        response.body
    );
    handle.abort();
}

#[tokio::test]
async fn session_list_returns_sessions_in_camel_case() {
    let (addr, handle) = start_server_with(&["prod-web", "test-db"]).await;
    let session = handshake(addr).await;

    let response = post_with_session(addr, &session, &tool_call(5, "session_list", "{}")).await;

    assert_eq!(
        response.status, 200,
        "列会话应成功，响应体为：{}",
        response.body
    );
    let called = event_by_id(&response, 5).expect("列会话响应应含 id 为 5 的消息");
    let sessions = called["result"]["structuredContent"]["sessions"]
        .as_array()
        .expect("列会话结果应含 sessions 数组");
    assert_eq!(sessions.len(), 2, "应返回两条会话：{}", response.body);
    assert_eq!(
        called["result"]["structuredContent"]["count"], 2,
        "总数应与数组长度一致：{}",
        response.body
    );
    assert!(
        response.body.contains("\"authKind\""),
        "字段名应是 camelCase 的 authKind，实际为：{}",
        response.body
    );
    assert!(
        !response.body.contains("auth_kind"),
        "不应出现 snake_case 字段名，实际为：{}",
        response.body
    );
    handle.abort();
}

#[tokio::test]
async fn session_list_filters_by_keyword() {
    let (addr, handle) = start_server_with(&["prod-web", "test-db"]).await;
    let session = handshake(addr).await;

    let response = post_with_session(
        addr,
        &session,
        &tool_call(6, "session_list", r#"{"keyword":"prod"}"#),
    )
    .await;

    assert_eq!(
        response.status, 200,
        "带关键词查询应成功，响应体为：{}",
        response.body
    );
    let called = event_by_id(&response, 6).expect("列会话响应应含 id 为 6 的消息");
    assert_eq!(
        called["result"]["structuredContent"]["count"], 1,
        "关键词应过滤掉 test-db，实际为：{}",
        response.body
    );
    assert!(
        response.body.contains("prod-web") && !response.body.contains("test-db"),
        "只应留下 prod-web，实际为：{}",
        response.body
    );
    handle.abort();
}

#[tokio::test]
async fn session_get_returns_seeded_session() {
    let (addr, handle) = start_server_with(&["prod-web"]).await;
    let session = handshake(addr).await;

    let listed = post_with_session(addr, &session, &tool_call(7, "session_list", "{}")).await;
    let listed_event = event_by_id(&listed, 7).expect("列会话响应应含 id 为 7 的消息");
    let id = listed_event["result"]["structuredContent"]["sessions"][0]["id"]
        .as_str()
        .expect("列表结果应含会话标识")
        .to_string();

    let response = post_with_session(
        addr,
        &session,
        &tool_call(8, "session_get", &format!(r#"{{"id":"{id}"}}"#)),
    )
    .await;

    assert_eq!(
        response.status, 200,
        "查详情应成功，响应体为：{}",
        response.body
    );
    let called = event_by_id(&response, 8).expect("查详情响应应含 id 为 8 的消息");
    assert_eq!(
        called["result"]["isError"], false,
        "命中的查询不是错误，实际为：{}",
        response.body
    );
    assert_eq!(
        called["result"]["structuredContent"]["name"], "prod-web",
        "应返回该会话，实际为：{}",
        response.body
    );
    assert_eq!(
        called["result"]["structuredContent"]["id"],
        id.as_str(),
        "标识应与列表里的一致，实际为：{}",
        response.body
    );
    handle.abort();
}

#[tokio::test]
async fn session_get_reports_missing_id() {
    let (addr, handle) = start_server().await;
    let session = handshake(addr).await;

    let response = post_with_session(
        addr,
        &session,
        &tool_call(9, "session_get", r#"{"id":"no-such-session"}"#),
    )
    .await;

    assert_eq!(
        response.status, 200,
        "业务错误仍应是正常的工具响应，响应体为：{}",
        response.body
    );
    let called = event_by_id(&response, 9).expect("查详情响应应含 id 为 9 的消息");
    assert_eq!(
        called["result"]["isError"], true,
        "会话不存在应标记为工具级错误，实际为：{}",
        response.body
    );
    assert!(
        response.body.contains("no-such-session"),
        "错误正文应带上会话标识，实际为：{}",
        response.body
    );
    handle.abort();
}

#[tokio::test]
async fn session_get_rejects_blank_id_as_invalid_params() {
    let (addr, handle) = start_server().await;
    let session = handshake(addr).await;

    let response = post_with_session(
        addr,
        &session,
        &tool_call(11, "session_get", r#"{"id":"   "}"#),
    )
    .await;

    assert_eq!(
        response.status, 200,
        "参数错误仍是一次正常响应，响应体为：{}",
        response.body
    );
    let called = event_by_id(&response, 11).expect("查详情响应应含 id 为 11 的消息");
    assert_eq!(
        called["error"]["code"], -32602,
        "空标识应走 invalid_params 而不是工具级错误，实际为：{}",
        response.body
    );
    assert!(
        called["result"].is_null(),
        "参数错误不应带回工具结果，实际为：{}",
        response.body
    );
    handle.abort();
}

#[tokio::test]
async fn tunnel_list_returns_zero_on_empty_database() {
    let (addr, handle) = start_server().await;
    let session = handshake(addr).await;

    let response = post_with_session(addr, &session, &tool_call(10, "tunnel_list", "{}")).await;

    assert_eq!(
        response.status, 200,
        "列隧道应成功，响应体为：{}",
        response.body
    );
    let called = event_by_id(&response, 10).expect("列隧道响应应含 id 为 10 的消息");
    assert_eq!(
        called["result"]["isError"], false,
        "空库不是错误，实际为：{}",
        response.body
    );
    assert_eq!(
        called["result"]["structuredContent"]["count"], 0,
        "空库应返回零条，实际为：{}",
        response.body
    );
    handle.abort();
}

#[tokio::test]
async fn non_loopback_bind_is_rejected() {
    let host: Arc<dyn Host> = Arc::new(StoreHost::new(
        Store::open_in_memory().await.expect("内存库应能打开"),
    ));
    let config = McpServerConfig {
        bind_addr: "0.0.0.0:0".parse().expect("通配地址应能解析"),
        token: TOKEN.to_string(),
    };

    let error = serve_on(host, Arc::new(NoopAudit), config)
        .await
        .expect_err("非回环地址应被拒绝");

    assert_eq!(
        error.kind(),
        std::io::ErrorKind::InvalidInput,
        "应报参数错误，实际为：{error}"
    );
    assert!(
        error.to_string().contains("回环"),
        "错误文案应说明只允许回环地址，实际为：{error}"
    );
}
