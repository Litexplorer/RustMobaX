//! 宿主抽象：MCP 工具层与具体运行环境之间的唯一接口。
//!
//! 桌面壳把 Tauri 的共享状态适配到 [`Host`]，无头模式与单元测试用
//! [`StoreHost`] 直接架在会话库上。工具层只认这个 trait，不认 Tauri 也不认 SQL。

use async_trait::async_trait;
use rustmobax_core::{
    CoreError, CoreResult, Session, SessionGroup, SessionInput, Store, TunnelConfig,
};
use serde::{Deserialize, Serialize};

/// 打开 SSH 终端时的认证方式。
///
/// 无参数
/// 核心流程：用内部标签 `kind` 区分为密码、私钥、agent 三种形态；
/// agent 目前没有实现，留作协议占位，宿主遇到它会直接回错误。
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SshAuthSpec {
    /// 密码认证，密码为明文。
    Password {
        /// 登录密码。
        password: String,
    },
    /// 私钥认证。
    PrivateKey {
        /// 私钥文件路径。
        path: String,
        /// 私钥口令，无口令时为空。
        passphrase: Option<String>,
    },
    /// 代理认证，当前不支持。
    Agent,
}

/// 打开 SSH 终端所需的连接参数。
///
/// 无参数
/// 核心流程：连接三要素 + 终端尺寸 + 主机指纹策略，
/// 尺寸与 TERM 各自独立，便于同一主机开多个不同大小的窗口。
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SshSpec {
    /// 主机名或 IP。
    pub host: String,
    /// SSH 端口。
    pub port: u16,
    /// 登录用户名。
    pub username: String,
    /// 初始列数。
    pub cols: u16,
    /// 初始行数。
    pub rows: u16,
    /// 传给远端的 TERM。
    pub term: String,
    /// 用户是否已确认接受未知主机指纹。
    pub accept_unknown_host_key: bool,
    /// 是否向远端申请 agent 转发，缺省为真（堡垒机准入常见要求）。
    pub agent_forward: Option<bool>,
    /// 认证方式。
    pub auth: SshAuthSpec,
}

/// 打开一个终端的入参。
///
/// 无参数
/// 核心流程：`kind` 决定走本机、显式 SSH 还是已保存会话；本机只认壳程序与工作目录，
/// 显式 SSH 要求 `ssh` 字段齐全，会话形态则由桌面宿主自己解析已保存的凭据。
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[schemars(extend("type" = "object"))]
pub enum TerminalSpec {
    /// 本机 PTY。
    Local {
        /// 本机壳程序，缺省跟随系统。
        shell: Option<String>,
        /// 启动目录，缺省跟随壳程序。
        cwd: Option<String>,
        /// 初始列数。
        cols: u16,
        /// 初始行数。
        rows: u16,
    },
    /// 显式给出连接参数的 SSH 终端。
    Ssh {
        /// 初始列数。
        cols: u16,
        /// 初始行数。
        rows: u16,
        /// SSH 连接参数。
        ssh: SshSpec,
    },
    /// 按已保存会话打开的 SSH 终端。
    Session {
        /// 会话标识。
        #[serde(rename = "sessionId")]
        session_id: String,
        /// 初始列数。
        cols: u16,
        /// 初始行数。
        rows: u16,
    },
}

/// 一次成功打开终端的结果。
///
/// 无参数
/// 核心流程：只回标识与生效尺寸，后续读写都用这个标识定位。
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OpenedTerminal {
    /// 终端标识。
    pub id: String,
    /// 实际生效的列数。
    pub cols: u16,
    /// 实际生效的行数。
    pub rows: u16,
}

/// 一次增量读取终端输出的结果。
///
/// 无参数
/// 核心流程：`start` / `next` 是会话建立以来的绝对字节序号，
/// 文本已经按 UTF-8 lossy 解码，`plain` 为真时控制序列已被剥离。
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalChunk {
    /// 本次片段在绝对字节流里的起始偏移。
    pub start: u64,
    /// 下一次读取应使用的游标。
    pub next: u64,
    /// 为真表示中间有字节已被回滚缓冲挤掉。
    pub truncated: bool,
    /// 解码后的文本，已按调用方要求剥离控制序列。
    pub text: String,
    /// 底层进程或连接是否已结束。
    pub exited: bool,
}

/// 一个活动终端的概要。
///
/// 无参数
/// 核心流程：只承载资源层要给客户端看的三个数字，拿不到的字段留空而不是瞎填，
/// 序列化时空值直接省略，客户端看到的就是「这一项未知」。
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInfo {
    /// 终端标识。
    pub id: String,
    /// 生效的列数，宿主不记录时为空。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cols: Option<u16>,
    /// 生效的行数，宿主不记录时为空。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
    /// 累计写入终端的字节数，宿主不记录时为空。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub written: Option<u64>,
}

/// MCP 工具层需要的宿主能力。
///
/// 无参数
/// 核心流程：只声明工具真正用到的动作，出入参一律是 core 的强类型，
/// 这样新增一个工具不需要碰任何传输代码，换宿主也不用改工具。
#[async_trait]
pub trait Host: Send + Sync {
    /// 列出全部会话分组。
    ///
    /// 参数：无
    /// 核心流程：转交存储层，按存储层既定顺序返回，不做二次排序。
    async fn list_groups(&self) -> CoreResult<Vec<SessionGroup>>;

    /// 列出会话，可按关键词过滤。
    ///
    /// 参数：keyword 为空串表示不过滤
    /// 核心流程：转交存储层的模糊匹配，命名与协议字段的匹配规则由存储层决定。
    async fn list_sessions(&self, keyword: &str) -> CoreResult<Vec<Session>>;

    /// 按标识取单个会话。
    ///
    /// 参数：id 为会话标识
    /// 核心流程：直接取，不存在时由存储层返回 NotFound 错误。
    async fn get_session(&self, id: &str) -> CoreResult<Session>;

    /// 列出全部隧道配置。
    ///
    /// 参数：无
    /// 核心流程：只读配置，不含运行状态；运行状态由隧道注册表另外提供。
    async fn list_tunnel_configs(&self) -> CoreResult<Vec<TunnelConfig>>;

    /// 打开一个终端。
    ///
    /// 参数：spec 为终端种类与连接参数
    /// 核心流程：由宿主选择本机 PTY 或 SSH 实现，成功即登记到注册表，
    /// 返回标识与生效尺寸；失败原因保持中文原文。
    async fn open_terminal(&self, spec: TerminalSpec) -> CoreResult<OpenedTerminal>;

    /// 按游标增量读取终端输出。
    ///
    /// 参数：id 为终端标识，cursor 为上次返回的 next，max 为本次最多取的字节数，
    /// plain 为真时剥离 ANSI 控制序列
    /// 核心流程：从注册表取句柄，读回滚缓冲；不支持回滚的会话回明确错误，
    /// 字符集按 UTF-8 lossy 解码，不因半个多字节字符而失败。
    async fn terminal_read(
        &self,
        id: &str,
        cursor: u64,
        max: usize,
        plain: bool,
    ) -> CoreResult<TerminalChunk>;

    /// 向终端写入文本。
    ///
    /// 参数：id 为终端标识，data 为要写入的文本，需要回车时由调用方自行带换行
    /// 核心流程：转发到终端的写入端，不做转义；Windows 上的本机 PTY 由实现把裸换行
    /// 补成回车换行，远端 shell 一律原样发送。
    async fn terminal_write(&self, id: &str, data: &str) -> CoreResult<()>;

    /// 调整终端尺寸。
    ///
    /// 参数：id 为终端标识，cols 为列数，rows 为行数
    /// 核心流程：转发给终端的尺寸调整动作，内核或远端会据此发窗口变更通知。
    async fn terminal_resize(&self, id: &str, cols: u16, rows: u16) -> CoreResult<()>;

    /// 关闭终端。
    ///
    /// 参数：id 为终端标识
    /// 核心流程：先让终端自行收尾，再从注册表移除，关闭不存在的标识按未找到处理。
    async fn terminal_close(&self, id: &str) -> CoreResult<()>;

    /// 在已打开的 SSH 终端上执行一次性命令。
    ///
    /// 参数：id 为终端标识，command 为要执行的命令，max_bytes 为最多取回的字节数
    /// 核心流程：只对 SSH 会话有效，命令走独立通道执行，不占用交互式 shell；
    /// 非 SSH 终端明确报错，执行本身放在阻塞线程里做，避免占住异步执行器。
    async fn terminal_exec(&self, id: &str, command: &str, max_bytes: usize) -> CoreResult<String>;

    /// 列出当前活动终端。
    ///
    /// 参数：无
    /// 核心流程：给资源层一份终端概要，默认实现回空表，这样不记录终端表的宿主
    /// 不必改一行也能编过；能枚举终端的宿主覆盖它，多给点信息就多给点。
    async fn list_terminals(&self) -> Vec<TerminalInfo> {
        Vec::new()
    }

    /// 新建一个会话分组。
    ///
    /// 参数：name 为分组名，parent_id 为空表示建在根下
    /// 核心流程：转交存储层插入，顺序值由存储层按同级最大顺序加一算出，
    /// 名称与父分组的校验也在存储层做，工具层不重复判断。
    async fn create_group(&self, name: &str, parent_id: Option<&str>) -> CoreResult<SessionGroup>;

    /// 重命名会话分组。
    ///
    /// 参数：id 为分组标识，name 为新名称
    /// 核心流程：转交存储层更新名称，分组不存在时由存储层返回未找到错误。
    async fn rename_group(&self, id: &str, name: &str) -> CoreResult<()>;

    /// 删除会话分组。
    ///
    /// 参数：id 为分组标识
    /// 核心流程：转交存储层删除；存储层会先把组内会话移到父分组、子分组上提一级，
    /// 因此删除分组不会连带删掉会话，这里也不额外做级联。
    async fn delete_group(&self, id: &str) -> CoreResult<()>;

    /// 把会话分组挂到新的父级下。
    ///
    /// 参数：id 为分组标识，parent_id 为空表示移到根下
    /// 核心流程：转交存储层更新父级，成环与目标存在性由存储层拒绝。
    async fn move_group(&self, id: &str, parent_id: Option<&str>) -> CoreResult<()>;

    /// 新建一个会话。
    ///
    /// 参数：input 为会话全部可编辑字段
    /// 核心流程：转交存储层校验并插入，返回带标识与时间戳的完整会话。
    async fn create_session(&self, input: &SessionInput) -> CoreResult<Session>;

    /// 覆盖更新一个已有会话。
    ///
    /// 参数：id 为会话标识，input 为新的字段值
    /// 核心流程：整份字段覆盖，不做字段级合并；会话不存在时由存储层报未找到。
    async fn update_session(&self, id: &str, input: &SessionInput) -> CoreResult<Session>;

    /// 删除一个会话。
    ///
    /// 参数：id 为会话标识
    /// 核心流程：转交存储层删除，先校验存在性再删，已不存在的标识报未找到。
    async fn delete_session(&self, id: &str) -> CoreResult<()>;

    /// 把会话移动到另一个分组。
    ///
    /// 参数：id 为会话标识，group_id 为空表示移到未分组
    /// 核心流程：只改归属，不动会话其余字段；目标分组不存在时由存储层报错。
    async fn move_session(&self, id: &str, group_id: Option<&str>) -> CoreResult<()>;

    /// 复制一个会话。
    ///
    /// 参数：id 为被复制的会话标识
    /// 核心流程：转交存储层复制，新名称由存储层派生以避免与现有名冲突，
    /// 返回的是副本而不是原会话。
    async fn copy_session(&self, id: &str) -> CoreResult<Session>;

    /// 读出全部应用设置。
    ///
    /// 参数：无
    /// 核心流程：原样转交存储层的键值列表，按 key 升序返回，
    /// 库里没有的键不会凭空补默认值。
    async fn settings_get_all(&self) -> CoreResult<Vec<(String, String)>>;

    /// 写入一条应用设置。
    ///
    /// 参数：key 为设置键，value 为设置值
    /// 核心流程：先拦掉 `mcp.` 开头的键，避免 agent 把自己关掉或改端口后前端误判；
    /// 其余键转交存储层的 UPSERT，长度上限由存储层把关。
    async fn settings_set(&self, key: &str, value: &str) -> CoreResult<()>;
}

/// 判断设置键是否属于 MCP 自身的配置。
///
/// 参数：key 为设置键名
/// 核心流程：只按 `mcp.` 前缀判断，前缀后的内容不参与判断；
/// 这些键由桌面壳维护，MCP 工具一律不许写。
fn is_mcp_setting(key: &str) -> bool {
    key.starts_with("mcp.")
}

/// 构造拒绝写入 MCP 自身配置时使用的错误。
///
/// 参数：无
/// 核心流程：固定中文文案，走 Validation 分支让工具层回 invalid_params，
/// 调用方一眼能看出这是策略拦截而不是库坏了。
fn mcp_setting_denied() -> CoreError {
    CoreError::Validation("不允许通过 MCP 修改 MCP 自身配置".into())
}

/// 直接架在会话库上的宿主实现。
///
/// 无参数
/// 核心流程：持有 [`Store`]，每个方法一对一转发。单元测试与将来的无头模式
/// 共用这一份实现，桌面壳则另有一份带 Tauri 状态的适配。
pub struct StoreHost {
    store: Store,
}

impl StoreHost {
    /// 用给定的会话库构造宿主。
    ///
    /// 参数：store 为已打开的会话库
    /// 核心流程：仅保存所有权，不建连接也不做迁移。
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    /// 取回内部会话库。
    ///
    /// 参数：无
    /// 核心流程：借出引用，供构造测试数据或调用 trait 之外的存储方法。
    pub fn store(&self) -> &Store {
        &self.store
    }
}

#[async_trait]
impl Host for StoreHost {
    async fn list_groups(&self) -> CoreResult<Vec<SessionGroup>> {
        self.store.list_groups().await
    }

    async fn list_sessions(&self, keyword: &str) -> CoreResult<Vec<Session>> {
        self.store.list_sessions(keyword).await
    }

    async fn get_session(&self, id: &str) -> CoreResult<Session> {
        self.store.get_session(id).await
    }

    async fn list_tunnel_configs(&self) -> CoreResult<Vec<TunnelConfig>> {
        self.store.tunnel_config_list().await
    }

    async fn open_terminal(&self, spec: TerminalSpec) -> CoreResult<OpenedTerminal> {
        let _ = spec;
        Err(CoreError::Terminal("当前宿主不支持终端操作".into()))
    }

    async fn terminal_read(
        &self,
        id: &str,
        cursor: u64,
        max: usize,
        plain: bool,
    ) -> CoreResult<TerminalChunk> {
        let _ = (id, cursor, max, plain);
        Err(CoreError::Terminal("当前宿主不支持终端操作".into()))
    }

    async fn terminal_write(&self, id: &str, data: &str) -> CoreResult<()> {
        let _ = (id, data);
        Err(CoreError::Terminal("当前宿主不支持终端操作".into()))
    }

    async fn terminal_resize(&self, id: &str, cols: u16, rows: u16) -> CoreResult<()> {
        let _ = (id, cols, rows);
        Err(CoreError::Terminal("当前宿主不支持终端操作".into()))
    }

    async fn terminal_close(&self, id: &str) -> CoreResult<()> {
        let _ = id;
        Err(CoreError::Terminal("当前宿主不支持终端操作".into()))
    }

    async fn terminal_exec(&self, id: &str, command: &str, max_bytes: usize) -> CoreResult<String> {
        let _ = (id, command, max_bytes);
        Err(CoreError::Terminal("当前宿主不支持终端操作".into()))
    }

    async fn create_group(&self, name: &str, parent_id: Option<&str>) -> CoreResult<SessionGroup> {
        self.store.create_group(name, parent_id).await
    }

    async fn rename_group(&self, id: &str, name: &str) -> CoreResult<()> {
        self.store.rename_group(id, name).await
    }

    async fn delete_group(&self, id: &str) -> CoreResult<()> {
        self.store.delete_group(id).await
    }

    async fn move_group(&self, id: &str, parent_id: Option<&str>) -> CoreResult<()> {
        self.store.move_group(id, parent_id).await
    }

    async fn create_session(&self, input: &SessionInput) -> CoreResult<Session> {
        self.store.create_session(input).await
    }

    async fn update_session(&self, id: &str, input: &SessionInput) -> CoreResult<Session> {
        self.store.update_session(id, input).await
    }

    async fn delete_session(&self, id: &str) -> CoreResult<()> {
        self.store.delete_session(id).await
    }

    async fn move_session(&self, id: &str, group_id: Option<&str>) -> CoreResult<()> {
        self.store.move_session(id, group_id).await
    }

    async fn copy_session(&self, id: &str) -> CoreResult<Session> {
        self.store.copy_session(id).await
    }

    async fn settings_get_all(&self) -> CoreResult<Vec<(String, String)>> {
        self.store.settings_get_all().await
    }

    async fn settings_set(&self, key: &str, value: &str) -> CoreResult<()> {
        if is_mcp_setting(key) {
            return Err(mcp_setting_denied());
        }
        self.store.settings_set(key, value).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rustmobax_core::{AuthKind, Protocol, SessionInput};

    use super::*;

    /// 构造一个架在内存库上的宿主。
    ///
    /// 参数：无
    /// 核心流程：开内存库并直接返回，调用方自己灌测试数据。
    async fn host() -> StoreHost {
        StoreHost::new(Store::open_in_memory().await.expect("内存库应能打开"))
    }

    /// 构造一条最小可用的 SSH 会话入参。
    ///
    /// 参数：name 为会话名
    /// 核心流程：主机填固定值，其余字段留空，满足存储层校验即可。
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

    #[tokio::test]
    async fn list_sessions_reads_through_store() {
        let host = host().await;
        host.store()
            .create_session(&ssh_input("prod-web"))
            .await
            .expect("应能建会话");

        let sessions = host.list_sessions("").await.expect("应能列会话");

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "prod-web");
    }

    #[tokio::test]
    async fn list_sessions_honours_keyword() {
        let host = host().await;
        host.store()
            .create_session(&ssh_input("prod-web"))
            .await
            .expect("应能建会话");
        host.store()
            .create_session(&ssh_input("test-db"))
            .await
            .expect("应能建会话");

        let sessions = host.list_sessions("prod").await.expect("应能列会话");

        assert_eq!(sessions.len(), 1, "关键词应过滤掉 test-db");
    }

    #[tokio::test]
    async fn get_session_reports_missing_id() {
        let host = host().await;

        let error = host.get_session("not-exist").await.expect_err("应报错");

        assert!(!error.to_string().is_empty());
    }

    #[tokio::test]
    async fn list_tunnel_configs_starts_empty() {
        let host = host().await;

        let configs = host.list_tunnel_configs().await.expect("应能列隧道配置");

        assert!(configs.is_empty());
    }

    /// 会话形态的入参按 kind=session 解析，序列化与 schema 都保持 camelCase 会话标识。
    #[test]
    fn session_spec_uses_kind_and_camel_case_id() {
        let raw = serde_json::json!({
            "kind": "session",
            "sessionId": "s-1",
            "cols": 80,
            "rows": 24
        });
        let spec: TerminalSpec = serde_json::from_value(raw).expect("应能解出会话形态");
        let TerminalSpec::Session {
            session_id,
            cols,
            rows,
        } = spec
        else {
            panic!("kind=session 应解成 Session 变体");
        };
        assert_eq!(session_id, "s-1");
        assert_eq!((cols, rows), (80, 24));

        let back = serde_json::to_value(TerminalSpec::Session {
            session_id: "s-1".to_string(),
            cols: 80,
            rows: 24,
        })
        .expect("应能序列化");
        assert_eq!(back["kind"], "session");
        assert_eq!(back["sessionId"], "s-1");

        let schema = serde_json::to_value(schemars::schema_for!(TerminalSpec))
            .expect("应能生成入参 schema");
        assert!(
            schema.to_string().contains("sessionId"),
            "schema 应含 sessionId：{schema}"
        );
    }

    #[tokio::test]
    async fn host_is_usable_as_trait_object() {
        let host: Arc<dyn Host> = Arc::new(host().await);

        let groups = host.list_groups().await.expect("应能列分组");

        assert!(groups.is_empty());
    }
}
