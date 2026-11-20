//! MCP 工具调用的审计：摘要生成、审计落地与测试用的内存实现。
//!
//! 这一层只关心「调用了什么、成没成、花了多久」，参数一律压成不含明文的摘要。
//! 摘要只保留键名、数字与字符串字节数，终端输入、设置值、密码口令都不会落库。

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use rmcp::model::JsonObject;
use rustmobax_core::{McpAuditEntry, Store};
use serde_json::Value;

/// 只记键名与长度的工具。
///
/// 这两个工具的入参可能是用户输入或终端里的原文，额外收紧到连数字与布尔的取值都不记，
/// 保证无论如何都看不到任何明文片段。
const LENGTH_ONLY_TOOLS: &[&str] = &["settings_set", "terminal_write"];

/// 审计落地的抽象。
///
/// 无参数
/// 核心流程：服务端只依赖这个 trait，桌面壳注入落库实现，测试注入内存实现；
/// `record` 不返回错误，写失败由实现自己吞掉，不能反过来影响工具调用的结果。
#[async_trait]
pub trait McpAudit: Send + Sync {
    /// 记录一次工具调用。
    ///
    /// 参数：tool 为工具名；digest 为不含明文的参数摘要；ok 为是否成功；
    /// error 为失败正文（成功时为空）；duration_ms 为耗时毫秒数
    /// 核心流程：实现方自行决定落盘或存内存，调用方只保证参数已经在调用前定型。
    async fn record(
        &self,
        tool: &str,
        digest: &str,
        ok: bool,
        error: Option<&str>,
        duration_ms: i64,
    );
}

/// 基于会话库的审计实现。
///
/// 无参数
/// 核心流程：持有 [`Store`] 的克隆句柄，每次调用写一行 `mcp_audit`；
/// 写库失败只打一行 stderr，绝不把错误抛回工具调用路径。
pub struct StoreAudit {
    store: Store,
}

impl StoreAudit {
    /// 用给定的会话库构造审计实现。
    ///
    /// 参数：store 为已打开的会话库
    /// 核心流程：只保存克隆，不建表也不发起 IO，建表由 `Store::open` 完成。
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

#[async_trait]
impl McpAudit for StoreAudit {
    async fn record(
        &self,
        tool: &str,
        digest: &str,
        ok: bool,
        error: Option<&str>,
        duration_ms: i64,
    ) {
        let entry = McpAuditEntry {
            id: 0,
            ts: now_ms(),
            tool: tool.to_string(),
            args_digest: digest.to_string(),
            ok,
            error: error.map(str::to_string),
            duration_ms,
        };
        if let Err(error) = self.store.mcp_audit_insert(&entry).await {
            eprintln!("MCP 审计写入失败：{error}");
        }
    }
}

/// 不落任何地方的审计实现。
///
/// 无参数
/// 核心流程：给传输层与控制器的默认路径用，避免为了满足参数而构造临时数据库；
/// 单测需要断言内容时改用内存记录实现。
pub struct NoopAudit;

#[async_trait]
impl McpAudit for NoopAudit {
    async fn record(
        &self,
        _tool: &str,
        _digest: &str,
        _ok: bool,
        _error: Option<&str>,
        _duration_ms: i64,
    ) {
    }
}

/// 把一次工具调用的入参压成摘要。
///
/// 参数：tool 为工具名；arguments 为客户端传来的参数对象，缺省表示无参
/// 核心流程：先按工具名决定是否进入「只记键名与长度」的严格模式，再交给
/// [`digest_arguments`] 渲染，最后拼上工具名前缀，得到 `session_list{}` 这类短串。
pub(crate) fn digest_tool_arguments(tool: &str, arguments: Option<&JsonObject>) -> String {
    let strict = LENGTH_ONLY_TOOLS.contains(&tool);
    let body = if strict {
        render_arguments(arguments, true)
    } else {
        digest_arguments(arguments)
    };
    format!("{tool}{body}")
}

/// 把参数对象压成不含明文的摘要。
///
/// 参数：arguments 为客户端传来的参数对象，缺省或空对象都渲染成 `{}`
/// 核心流程：键名照抄并按字典序排序；字符串只记字节数（`data:32B`），数字与布尔记原值；
/// 嵌套对象只列下一层键名，数组只记元素个数，任何字符串内容都不会出现在结果里。
pub(crate) fn digest_arguments(arguments: Option<&JsonObject>) -> String {
    render_arguments(arguments, false)
}

/// 渲染参数摘要的主体。
///
/// 参数：arguments 为参数对象；strict 为真时连数字、布尔与 null 的取值都不记
/// 核心流程：顶层键排序后逐个渲染，用逗号加空格连接，外层套大括号。
fn render_arguments(arguments: Option<&JsonObject>, strict: bool) -> String {
    let Some(arguments) = arguments else {
        return "{}".to_string();
    };
    let mut keys: Vec<&String> = arguments.keys().collect();
    keys.sort();
    let fields: Vec<String> = keys
        .iter()
        .map(|key| render_field(key, &arguments[*key], strict))
        .collect();
    format!("{{{}}}", fields.join(", "))
}

/// 渲染单个字段。
///
/// 参数：key 为字段名；value 为字段值；strict 为真时只输出键名与字符串长度
/// 核心流程：字符串永远只留字节数；数字与布尔在非严格模式下保留原值；
/// 嵌套对象递归只列键名，数组只留元素个数。
fn render_field(key: &str, value: &Value, strict: bool) -> String {
    match value {
        Value::String(text) => format!("{key}:{}B", text.len()),
        Value::Number(number) if !strict => format!("{key}:{number}"),
        Value::Bool(flag) if !strict => format!("{key}:{flag}"),
        Value::Null if !strict => format!("{key}:null"),
        Value::Object(map) => {
            let mut nested: Vec<&String> = map.keys().collect();
            nested.sort();
            let names: Vec<&str> = nested.iter().map(|name| name.as_str()).collect();
            format!("{key}{{{}}}", names.join(", "))
        }
        Value::Array(items) => format!("{key}[{}]", items.len()),
        _ => key.to_string(),
    }
}

/// 取当前毫秒时间戳。
///
/// 参数：无
/// 核心流程：系统时钟早于 UNIX 纪元时返回 0，避免负数时间戳写进库里。
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// 把 JSON 对象转成摘要，省掉每个用例各自取引用的样板。
    ///
    /// 参数：value 为任意 JSON 对象
    /// 核心流程：先断言它确实是对象，再取引用交给 digest_arguments。
    fn digest_of(value: serde_json::Value) -> String {
        let object = value.as_object().expect("用例必须传 JSON 对象");
        digest_arguments(Some(object))
    }

    /// 无参与空对象都只渲染成一对空大括号。
    #[test]
    fn digest_keeps_only_key_names_for_empty_arguments() {
        assert_eq!(digest_arguments(None), "{}");
        assert_eq!(digest_of(json!({})), "{}");
        assert_eq!(
            digest_tool_arguments("session_list", None),
            "session_list{}"
        );
    }

    /// 字符串只留字节数，原文与键名以外的内容都不得出现。
    #[test]
    fn digest_keeps_string_length_instead_of_content() {
        let digest = digest_of(json!({ "id": "abc", "data": "hunter2" }));
        assert_eq!(digest, "{data:7B, id:3B}");
        assert!(!digest.contains("abc"), "摘要里不应出现字符串原文：{digest}");
        assert!(!digest.contains("hunter2"), "摘要里不应出现字符串原文：{digest}");

        let strict = digest_tool_arguments(
            "terminal_write",
            json!({ "id": "term-1", "data": "rm -rf /" }).as_object(),
        );
        assert_eq!(strict, "terminal_write{data:8B, id:6B}");
        assert!(!strict.contains("rm -rf"), "终端输入不得进入审计：{strict}");

        let settings = digest_tool_arguments(
            "settings_set",
            json!({ "key": "theme", "value": "dark" }).as_object(),
        );
        assert_eq!(settings, "settings_set{key:5B, value:4B}");
        assert!(!settings.contains("theme"), "设置键名不得进入审计：{settings}");
    }

    /// 数字与布尔保留原值，排序按键名的字典序。
    #[test]
    fn digest_keeps_numeric_and_boolean_values() {
        assert_eq!(
            digest_of(json!({ "port": 22, "plain": true, "cursor": 0 })),
            "{cursor:0, plain:true, port:22}"
        );
    }

    /// 嵌套对象只列下一层键名，值里的主机名与端口都不出现。
    #[test]
    fn digest_lists_nested_object_keys_only() {
        let digest = digest_of(json!({
            "spec": { "host": "secret.example", "port": 22, "cols": 80 },
            "id": "term-1"
        }));
        assert_eq!(digest, "{id:6B, spec{cols, host, port}}");
        assert!(!digest.contains("secret.example"), "嵌套值不得出现在摘要里：{digest}");
        assert!(!digest.contains("22"), "嵌套值不得出现在摘要里：{digest}");
    }
}
