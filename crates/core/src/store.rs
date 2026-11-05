use std::path::Path;
use std::str::FromStr;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::error::{CoreError, CoreResult};
use crate::model::{
    now_ms, AuthKind, Protocol, Session, SessionGroup, SessionInput, TunnelConfig,
    TunnelConfigInput,
};

const DDL_GROUP: &str = "
CREATE TABLE IF NOT EXISTS session_group (
    id         TEXT PRIMARY KEY,
    parent_id  TEXT,
    name       TEXT NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0
)";

const DDL_SESSION: &str = "
CREATE TABLE IF NOT EXISTS session (
    id         TEXT PRIMARY KEY,
    group_id   TEXT,
    name       TEXT NOT NULL,
    protocol   TEXT NOT NULL,
    host       TEXT,
    port       INTEGER,
    username   TEXT,
    auth_kind  TEXT NOT NULL,
    extra      TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
)";

const DDL_SESSION_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_session_group_id ON session(group_id)";

const DDL_KNOWN_HOST: &str = "
CREATE TABLE IF NOT EXISTS known_host (
    host        TEXT NOT NULL,
    port        INTEGER NOT NULL,
    key_type    TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (host, port, key_type)
)";

const DDL_CREDENTIAL: &str = "
CREATE TABLE IF NOT EXISTS credential (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL,
    label      TEXT NOT NULL,
    nonce      BLOB NOT NULL,
    ciphertext BLOB NOT NULL,
    updated_at INTEGER NOT NULL
)";

const DDL_VAULT_META: &str = "
CREATE TABLE IF NOT EXISTS vault_meta (
    k TEXT PRIMARY KEY,
    v TEXT NOT NULL
)";

const DDL_APP_SETTING: &str = "
CREATE TABLE IF NOT EXISTS app_setting (
    k TEXT PRIMARY KEY,
    v TEXT NOT NULL
)";

const DDL_CUSTOM_THEME: &str = "
CREATE TABLE IF NOT EXISTS custom_theme (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL
)";

const DDL_TUNNEL: &str = "
CREATE TABLE IF NOT EXISTS tunnel_config (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    session_id  TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    listen_host TEXT NOT NULL DEFAULT '',
    listen_port INTEGER NOT NULL DEFAULT 0,
    target_host TEXT NOT NULL DEFAULT '',
    target_port INTEGER NOT NULL DEFAULT 0,
    auto_start  INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
)";

const DDL_TUNNEL_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_tunnel_config_session_id ON tunnel_config(session_id)";

/// 给旧库补列：tunnel_config 早先没有 auto_start。
const DDL_TUNNEL_ADD_AUTO_START: &str =
    "ALTER TABLE tunnel_config ADD COLUMN auto_start INTEGER NOT NULL DEFAULT 0";

const TUNNEL_COLUMNS: &str = "id, name, session_id, kind, listen_host, listen_port, \
     target_host, target_port, auto_start, created_at, updated_at";

const SESSION_COLUMNS: &str = "s.id, s.group_id, s.name, s.protocol, s.host, s.port, \
     s.username, s.auth_kind, s.extra, s.created_at, s.updated_at";

/// 设置键名的字节上限。
const SETTING_KEY_MAX_BYTES: usize = 64;

/// 设置值的字节上限。
const SETTING_VALUE_MAX_BYTES: usize = 4096;

/// 自定义主题名称的字节上限。
pub const THEME_NAME_MAX_BYTES: usize = 64;

/// 自定义主题 payload 的字节上限。
pub const THEME_PAYLOAD_MAX_BYTES: usize = 16384;

/// 会话库，封装全部 SQLite 访问。
///
/// 无参数
/// 核心流程：持有连接池，向上提供分组树与会话的增删改查，屏蔽 SQL 细节。
#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

/// 数据库行的中间表示。
///
/// 无参数
/// 核心流程：协议与认证方式以字符串出入库，再由 TryFrom 转成强类型，避免
/// sqlx 直接解码枚举时丢错误信息。
#[derive(sqlx::FromRow)]
struct SessionRow {
    id: String,
    group_id: Option<String>,
    name: String,
    protocol: String,
    host: Option<String>,
    port: Option<i64>,
    username: Option<String>,
    auth_kind: String,
    extra: Option<String>,
    created_at: i64,
    updated_at: i64,
}

impl TryFrom<SessionRow> for Session {
    type Error = CoreError;

    /// 把数据库行还原成会话。
    ///
    /// 参数：row 为查询结果行
    /// 核心流程：解析协议与认证方式，未知枚举值直接报错而不是静默兜底。
    fn try_from(row: SessionRow) -> CoreResult<Self> {
        let protocol = Protocol::parse(&row.protocol)
            .ok_or_else(|| CoreError::Validation(format!("未知协议：{}", row.protocol)))?;
        let auth_kind = AuthKind::parse(&row.auth_kind)
            .ok_or_else(|| CoreError::Validation(format!("未知认证方式：{}", row.auth_kind)))?;
        Ok(Session {
            id: row.id,
            group_id: row.group_id,
            name: row.name,
            protocol,
            host: row.host,
            port: row.port.map(|p| p as u16),
            username: row.username,
            auth_kind,
            extra: row.extra,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

/// 隧道配置行的中间表示。
///
/// 无参数
/// 核心流程：端口在库里是 INTEGER，先按 i64 取出再转 u16，
/// 越界直接报校验错误，避免静默截断成另一个端口。
#[derive(sqlx::FromRow)]
struct TunnelConfigRow {
    id: String,
    name: String,
    session_id: String,
    kind: String,
    listen_host: String,
    listen_port: i64,
    target_host: String,
    target_port: i64,
    /// 库里存 INTEGER，0 表示关闭。
    auto_start: i64,
    created_at: i64,
    updated_at: i64,
}

impl TryFrom<TunnelConfigRow> for TunnelConfig {
    type Error = CoreError;

    /// 把数据库行还原成隧道配置。
    ///
    /// 参数：row 为查询结果行
    /// 核心流程：端口做范围检查，其余字段原样搬运。
    fn try_from(row: TunnelConfigRow) -> CoreResult<Self> {
        let listen_port = u16::try_from(row.listen_port)
            .map_err(|_| CoreError::Validation(format!("监听端口超出范围：{}", row.listen_port)))?;
        let target_port = u16::try_from(row.target_port)
            .map_err(|_| CoreError::Validation(format!("目标端口超出范围：{}", row.target_port)))?;
        Ok(TunnelConfig {
            id: row.id,
            name: row.name,
            session_id: row.session_id,
            kind: row.kind,
            listen_host: row.listen_host,
            listen_port,
            target_host: row.target_host,
            target_port,
            auto_start: row.auto_start != 0,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

impl Store {
    /// 打开磁盘上的会话库。
    ///
    /// 参数：path 为数据库文件路径
    /// 核心流程：确保父目录存在 → 建立连接池 → 执行幂等建表。
    pub async fn open(path: impl AsRef<Path>) -> CoreResult<Self> {
        let path = path.as_ref();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    /// 打开内存会话库，供测试使用。
    ///
    /// 参数：无
    /// 核心流程：以 sqlite::memory: 建库，连接数固定为 1，保证内存库不被多条连接分裂；
    /// 同样打开外键约束，让测试与线上（open）走一样的级联删除行为。
    pub async fn open_in_memory() -> CoreResult<Self> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    /// 建表。
    ///
    /// 参数：无
    /// 核心流程：顺序执行幂等 DDL，重复调用无副作用；隧道配置在会话之后建表，
    /// 它带 session(id) 外键，会话删除时配置随之清掉。
    async fn migrate(&self) -> CoreResult<()> {
        sqlx::query(DDL_GROUP).execute(&self.pool).await?;
        sqlx::query(DDL_SESSION).execute(&self.pool).await?;
        sqlx::query(DDL_SESSION_INDEX).execute(&self.pool).await?;
        sqlx::query(DDL_TUNNEL).execute(&self.pool).await?;
        self.migrate_tunnel_auto_start().await?;
        sqlx::query(DDL_TUNNEL_INDEX).execute(&self.pool).await?;
        sqlx::query(DDL_KNOWN_HOST).execute(&self.pool).await?;
        sqlx::query(DDL_CREDENTIAL).execute(&self.pool).await?;
        sqlx::query(DDL_VAULT_META).execute(&self.pool).await?;
        sqlx::query(DDL_APP_SETTING).execute(&self.pool).await?;
        sqlx::query(DDL_CUSTOM_THEME).execute(&self.pool).await?;
        Ok(())
    }

    /// 给旧库的 tunnel_config 补上 auto_start 列。
    ///
    /// 无参数
    /// 核心流程：先查 pragma_table_info 看列在不在，不在才 ALTER；
    /// 新库建表时已经带上该列，重复执行 migrate 也不会重复 ALTER 报错。
    async fn migrate_tunnel_auto_start(&self) -> CoreResult<()> {
        let present: i64 = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM pragma_table_info('tunnel_config') WHERE name = 'auto_start'",
        )
        .fetch_one(&self.pool)
        .await?;
        if present == 0 {
            sqlx::query(DDL_TUNNEL_ADD_AUTO_START)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// 列出全部分组。
    ///
    /// 参数：无
    /// 核心流程：按同级顺序值排序返回，前端自行组装成树。
    pub async fn list_groups(&self) -> CoreResult<Vec<SessionGroup>> {
        let rows = sqlx::query(
            "SELECT id, parent_id, name, sort_order FROM session_group \
             ORDER BY sort_order, name COLLATE NOCASE",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| SessionGroup {
                id: row.get("id"),
                parent_id: row.get("parent_id"),
                name: row.get("name"),
                sort_order: row.get("sort_order"),
            })
            .collect())
    }

    /// 新建分组。
    ///
    /// 参数：name 为分组名，parent_id 为空表示根分组
    /// 核心流程：校验名称与父分组 → 取同级最大顺序值加一 → 插入并返回。
    pub async fn create_group(
        &self,
        name: &str,
        parent_id: Option<&str>,
    ) -> CoreResult<SessionGroup> {
        let name = name.trim();
        if name.is_empty() {
            return Err(CoreError::Validation("分组名称不能为空".into()));
        }
        if let Some(parent) = parent_id {
            self.require_group(parent).await?;
        }
        let next: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM session_group WHERE parent_id IS ?1",
        )
        .bind(parent_id)
        .fetch_one(&self.pool)
        .await?;
        let group = SessionGroup {
            id: Uuid::new_v4().to_string(),
            parent_id: parent_id.map(str::to_string),
            name: name.to_string(),
            sort_order: next,
        };
        sqlx::query(
            "INSERT INTO session_group (id, parent_id, name, sort_order) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(&group.id)
        .bind(&group.parent_id)
        .bind(&group.name)
        .bind(group.sort_order)
        .execute(&self.pool)
        .await?;
        Ok(group)
    }

    /// 重命名分组。
    ///
    /// 参数：id 为分组标识，name 为新名称
    /// 核心流程：校验存在性与非空名称 → 更新。
    pub async fn rename_group(&self, id: &str, name: &str) -> CoreResult<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(CoreError::Validation("分组名称不能为空".into()));
        }
        self.require_group(id).await?;
        sqlx::query("UPDATE session_group SET name = ?1 WHERE id = ?2")
            .bind(name)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 删除分组。
    ///
    /// 参数：id 为分组标识
    /// 核心流程：在事务内把该组的会话移到未分组、子分组上提到被删分组的父级，
    /// 最后删除分组本身，保证不会连带删掉用户的会话。
    pub async fn delete_group(&self, id: &str) -> CoreResult<()> {
        let parent_id: Option<String> =
            sqlx::query_scalar("SELECT parent_id FROM session_group WHERE id = ?1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?
                .ok_or_else(|| CoreError::NotFound(format!("分组 {id}")))?;

        let mut tx = self.pool.begin().await?;
        // 把会话挂到父分组 -> 避免删组时误删会话
        sqlx::query("UPDATE session SET group_id = ?1 WHERE group_id = ?2")
            .bind(&parent_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        // 把子分组上提一级 -> 保留嵌套结构
        sqlx::query("UPDATE session_group SET parent_id = ?1 WHERE parent_id = ?2")
            .bind(&parent_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM session_group WHERE id = ?1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// 移动分组到新的父级。
    ///
    /// 参数：id 为待移动分组，parent_id 为新父级（空表示根）
    /// 核心流程：拒绝把自己挂到自己的子树下 → 校验目标存在 → 更新 parent_id。
    pub async fn move_group(&self, id: &str, parent_id: Option<&str>) -> CoreResult<()> {
        self.require_group(id).await?;
        if let Some(target) = parent_id {
            self.require_group(target).await?;
            if self.is_in_subtree(target, id).await? {
                return Err(CoreError::Validation(
                    "不能把分组移动到它自己的子分组里".into(),
                ));
            }
        }
        sqlx::query("UPDATE session_group SET parent_id = ?1 WHERE id = ?2")
            .bind(parent_id)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 判断 candidate 是否位于 root 的子树中。
    ///
    /// 参数：candidate 为被检查的分组，root 为子树根
    /// 核心流程：沿 parent 链向上回溯，命中 root 即判定为子节点。
    async fn is_in_subtree(&self, candidate: &str, root: &str) -> CoreResult<bool> {
        let mut current = Some(candidate.to_string());
        let mut guard = 0;
        while let Some(id) = current {
            if id == root {
                return Ok(true);
            }
            guard += 1;
            if guard > 256 {
                return Err(CoreError::Validation("分组层级异常，疑似存在环".into()));
            }
            current = self.group_parent(&id).await?;
        }
        Ok(false)
    }

    /// 查询分组的父级。
    ///
    /// 参数：id 为分组标识
    /// 核心流程：直接读 parent_id，不存在返回 None。
    async fn group_parent(&self, id: &str) -> CoreResult<Option<String>> {
        let found: Option<Option<String>> =
            sqlx::query_scalar("SELECT parent_id FROM session_group WHERE id = ?1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(found.flatten())
    }

    /// 确认分组存在。
    ///
    /// 参数：id 为分组标识
    /// 核心流程：计数为零则报未找到，供写入前的前置校验复用。
    async fn require_group(&self, id: &str) -> CoreResult<()> {
        let exists: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM session_group WHERE id = ?1")
                .bind(id)
                .fetch_one(&self.pool)
                .await?;
        if exists == 0 {
            return Err(CoreError::NotFound(format!("分组 {id}")));
        }
        Ok(())
    }

    /// 列出会话，可按关键字过滤。
    ///
    /// 参数：keyword 为搜索词，空表示不过滤
    /// 核心流程：关键字统一转小写后拼 LIKE，匹配会话名、主机、用户名与所属分组名；
    /// 空关键字时 WHERE 恒真，走全量列表。
    pub async fn list_sessions(&self, keyword: &str) -> CoreResult<Vec<Session>> {
        let trimmed = keyword.trim().to_lowercase();
        let pattern = if trimmed.is_empty() {
            String::new()
        } else {
            format!("%{trimmed}%")
        };
        let sql = format!(
            "SELECT {SESSION_COLUMNS} FROM session s \
             LEFT JOIN session_group g ON g.id = s.group_id \
             WHERE ?1 = '' \
                OR lower(s.name) LIKE ?1 \
                OR lower(COALESCE(s.host, '')) LIKE ?1 \
                OR lower(COALESCE(s.username, '')) LIKE ?1 \
                OR lower(COALESCE(g.name, '')) LIKE ?1 \
             ORDER BY s.name COLLATE NOCASE"
        );
        let rows = sqlx::query_as::<_, SessionRow>(&sql)
            .bind(&pattern)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(Session::try_from).collect()
    }

    /// 按标识取单个会话。
    ///
    /// 参数：id 为会话标识
    /// 核心流程：查不到即报未找到，避免上层拿到空壳。
    pub async fn get_session(&self, id: &str) -> CoreResult<Session> {
        let sql = format!("SELECT {SESSION_COLUMNS} FROM session s WHERE s.id = ?1");
        let row = sqlx::query_as::<_, SessionRow>(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("会话 {id}")))?;
        Session::try_from(row)
    }

    /// 新建会话。
    ///
    /// 参数：input 为用户填写的会话信息
    /// 核心流程：校验必填与分组存在性 → 生成标识与时间戳 → 插入并回读。
    pub async fn create_session(&self, input: &SessionInput) -> CoreResult<Session> {
        input.validate()?;
        if let Some(group) = &input.group_id {
            self.require_group(group).await?;
        }
        let now = now_ms();
        let session = Session {
            id: Uuid::new_v4().to_string(),
            group_id: input.group_id.clone(),
            name: input.name.trim().to_string(),
            protocol: input.protocol,
            host: normalize(&input.host),
            port: input.effective_port(),
            username: normalize(&input.username),
            auth_kind: input.auth_kind,
            extra: normalize(&input.extra),
            created_at: now,
            updated_at: now,
        };
        sqlx::query(
            "INSERT INTO session \
             (id, group_id, name, protocol, host, port, username, auth_kind, extra, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(&session.id)
        .bind(&session.group_id)
        .bind(&session.name)
        .bind(session.protocol.as_str())
        .bind(&session.host)
        .bind(session.port.map(i64::from))
        .bind(&session.username)
        .bind(session.auth_kind.as_str())
        .bind(&session.extra)
        .bind(session.created_at)
        .bind(session.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(session)
    }

    /// 更新会话。
    ///
    /// 参数：id 为会话标识，input 为新内容
    /// 核心流程：校验必填与分组存在性 → 覆盖可编辑字段并刷新更新时间，保留创建时间。
    pub async fn update_session(&self, id: &str, input: &SessionInput) -> CoreResult<Session> {
        input.validate()?;
        let existing = self.get_session(id).await?;
        if let Some(group) = &input.group_id {
            self.require_group(group).await?;
        }
        let updated_at = now_ms();
        sqlx::query(
            "UPDATE session SET group_id = ?1, name = ?2, protocol = ?3, host = ?4, port = ?5, \
             username = ?6, auth_kind = ?7, extra = ?8, updated_at = ?9 WHERE id = ?10",
        )
        .bind(&input.group_id)
        .bind(input.name.trim())
        .bind(input.protocol.as_str())
        .bind(normalize(&input.host))
        .bind(input.effective_port().map(i64::from))
        .bind(normalize(&input.username))
        .bind(input.auth_kind.as_str())
        .bind(normalize(&input.extra))
        .bind(updated_at)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(Session {
            id: existing.id,
            group_id: input.group_id.clone(),
            name: input.name.trim().to_string(),
            protocol: input.protocol,
            host: normalize(&input.host),
            port: input.effective_port(),
            username: normalize(&input.username),
            auth_kind: input.auth_kind,
            extra: normalize(&input.extra),
            created_at: existing.created_at,
            updated_at,
        })
    }

    /// 删除会话。
    ///
    /// 参数：id 为会话标识
    /// 核心流程：先确认存在再删除，删除不存在的会话返回未找到。
    pub async fn delete_session(&self, id: &str) -> CoreResult<()> {
        self.get_session(id).await?;
        sqlx::query("DELETE FROM session WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 移动会话到目标分组。
    ///
    /// 参数：id 为会话标识，group_id 为目标分组（空表示未分组）
    /// 核心流程：校验会话与目标分组 → 更新 group_id 与更新时间。
    pub async fn move_session(&self, id: &str, group_id: Option<&str>) -> CoreResult<()> {
        self.get_session(id).await?;
        if let Some(group) = group_id {
            self.require_group(group).await?;
        }
        sqlx::query("UPDATE session SET group_id = ?1, updated_at = ?2 WHERE id = ?3")
            .bind(group_id)
            .bind(now_ms())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 复制会话并派生新名称。
    ///
    /// 参数：id 为被复制的会话标识
    /// 核心流程：读取原会话 → 生成不与现有名冲突的后缀名 → 以新标识插入副本。
    pub async fn copy_session(&self, id: &str) -> CoreResult<Session> {
        let source = self.get_session(id).await?;
        let existing: Vec<String> = sqlx::query_scalar("SELECT name FROM session")
            .fetch_all(&self.pool)
            .await?;
        let name = derive_copy_name(&source.name, &existing);
        let input = SessionInput {
            name,
            group_id: source.group_id.clone(),
            protocol: source.protocol,
            host: source.host.clone(),
            port: source.port,
            username: source.username.clone(),
            auth_kind: source.auth_kind,
            extra: source.extra.clone(),
        };
        self.create_session(&input).await
    }

    /// 查找指定父分组下同名的子分组。
    ///
    /// 参数：parent_id 为父分组（空表示根），name 为分组名
    /// 核心流程：按父级与名称精确匹配，命中返回分组，否则返回 None，
    /// 供导入时复用已有分组而不是重复创建。
    pub async fn find_child_group(
        &self,
        parent_id: Option<&str>,
        name: &str,
    ) -> CoreResult<Option<SessionGroup>> {
        let row = sqlx::query(
            "SELECT id, parent_id, name, sort_order FROM session_group \
             WHERE parent_id IS ?1 AND name = ?2 LIMIT 1",
        )
        .bind(parent_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| SessionGroup {
            id: row.get("id"),
            parent_id: row.get("parent_id"),
            name: row.get("name"),
            sort_order: row.get("sort_order"),
        }))
    }

    /// 查询已保存的主机指纹。
    ///
    /// 参数：host、port、key_type 定位一条记录
    /// 核心流程：命中返回指纹字符串，未命中返回 None（表示"未知主机"）。
    pub async fn known_host_get(
        &self,
        host: &str,
        port: u16,
        key_type: &str,
    ) -> CoreResult<Option<String>> {
        let found: Option<String> = sqlx::query_scalar(
            "SELECT fingerprint FROM known_host WHERE host = ?1 AND port = ?2 AND key_type = ?3",
        )
        .bind(host)
        .bind(i64::from(port))
        .bind(key_type)
        .fetch_optional(&self.pool)
        .await?;
        Ok(found)
    }

    /// 记录或更新主机指纹。
    ///
    /// 参数：host、port、key_type 定位，fingerprint 为本次观察到值
    /// 核心流程：主键冲突时覆盖旧指纹，用户确认过才会调用。
    pub async fn known_host_upsert(
        &self,
        host: &str,
        port: u16,
        key_type: &str,
        fingerprint: &str,
    ) -> CoreResult<()> {
        sqlx::query(
            "INSERT INTO known_host (host, port, key_type, fingerprint, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(host, port, key_type) DO UPDATE SET \
                 fingerprint = excluded.fingerprint, updated_at = excluded.updated_at",
        )
        .bind(host)
        .bind(i64::from(port))
        .bind(key_type)
        .bind(fingerprint)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 删除某台主机的指纹记录。
    ///
    /// 参数：host、port 定位，key_type 为空表示该主机全部类型
    /// 核心流程：指纹变更时要求用户显式删除旧记录，避免静默覆盖。
    pub async fn known_host_delete(
        &self,
        host: &str,
        port: u16,
        key_type: Option<&str>,
    ) -> CoreResult<u64> {
        let affected = match key_type {
            Some(kind) => sqlx::query(
                "DELETE FROM known_host WHERE host = ?1 AND port = ?2 AND key_type = ?3",
            )
            .bind(host)
            .bind(i64::from(port))
            .bind(kind)
            .execute(&self.pool)
            .await?,
            None => sqlx::query("DELETE FROM known_host WHERE host = ?1 AND port = ?2")
                .bind(host)
                .bind(i64::from(port))
                .execute(&self.pool)
                .await?,
        };
        Ok(affected.rows_affected())
    }

    /// 列出全部已知主机。
    ///
    /// 参数：无
    /// 核心流程：按主机与端口排序返回，供界面展示与导出。
    pub async fn known_host_list(&self) -> CoreResult<Vec<KnownHost>> {
        let rows = sqlx::query(
            "SELECT host, port, key_type, fingerprint FROM known_host \
             ORDER BY host, port, key_type",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| KnownHost {
                host: row.get("host"),
                port: u16::try_from(row.get::<i64, _>("port")).unwrap_or(22),
                key_type: row.get("key_type"),
                fingerprint: row.get("fingerprint"),
            })
            .collect())
    }

    /// 保存一条加密后的凭据。
    ///
    /// 参数：id 为凭据标识，kind/label 为分类与展示名，nonce/ciphertext 为密文数据
    /// 核心流程：明文永远不进这一层，只把加密结果写库，因此库里搜不到密码。
    pub async fn credential_insert(
        &self,
        id: &str,
        kind: &str,
        label: &str,
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> CoreResult<()> {
        sqlx::query(
            "INSERT INTO credential (id, kind, label, nonce, ciphertext, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(id)
        .bind(kind)
        .bind(label)
        .bind(nonce)
        .bind(ciphertext)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 列出全部凭据的元信息。
    ///
    /// 参数：无
    /// 核心流程：只回 id/kind/label，供界面做选择，不返回任何密文字段。
    pub async fn credential_list(&self) -> CoreResult<Vec<Credential>> {
        let rows = sqlx::query(
            "SELECT id, kind, label, updated_at FROM credential ORDER BY label, id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| Credential {
                id: row.get("id"),
                kind: row.get("kind"),
                label: row.get("label"),
                updated_at: row.get("updated_at"),
            })
            .collect())
    }

    /// 读取一条凭据的密文。
    ///
    /// 参数：id 为凭据标识
    /// 核心流程：命中返回密文与 nonce，未命中返回 None，由上层决定报"不存在"。
    pub async fn credential_secret(&self, id: &str) -> CoreResult<Option<StoredSecret>> {
        let row = sqlx::query("SELECT kind, nonce, ciphertext FROM credential WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|row| StoredSecret {
            kind: row.get("kind"),
            nonce: row.get("nonce"),
            ciphertext: row.get("ciphertext"),
        }))
    }

    /// 删除一条凭据。
    ///
    /// 参数：id 为凭据标识
    /// 核心流程：返回受影响行数，0 表示本来就不存在。
    pub async fn credential_delete(&self, id: &str) -> CoreResult<u64> {
        let affected = sqlx::query("DELETE FROM credential WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(affected.rows_affected())
    }

    /// 读取凭据库的元数据。
    ///
    /// 参数：k 为键名（salt / check_nonce / check_ciphertext）
    /// 核心流程：键值稀疏存储，主密码设置前返回 None。
    pub async fn vault_meta_get(&self, k: &str) -> CoreResult<Option<String>> {
        let found: Option<String> =
            sqlx::query_scalar("SELECT v FROM vault_meta WHERE k = ?1")
                .bind(k)
                .fetch_optional(&self.pool)
                .await?;
        Ok(found)
    }

    /// 写入凭据库的元数据。
    ///
    /// 参数：k 为键名，v 为值
    /// 核心流程：主键冲突时覆盖，用于初始化主密码时写入盐与校验块。
    pub async fn vault_meta_set(&self, k: &str, v: &str) -> CoreResult<()> {
        sqlx::query(
            "INSERT INTO vault_meta (k, v) VALUES (?1, ?2) \
             ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        )
        .bind(k)
        .bind(v)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 读取全部应用设置。
    ///
    /// 无参数
    /// 核心流程：按 key 升序整表取出，返回 (key, value) 列表；库里没有的键
    /// 不会出现在结果中，由前端用自己的默认值兜底。
    pub async fn settings_get_all(&self) -> CoreResult<Vec<(String, String)>> {
        let rows = sqlx::query("SELECT k, v FROM app_setting ORDER BY k")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|row| (row.get("k"), row.get("v")))
            .collect())
    }

    /// 写入（或覆盖）一条应用设置。
    ///
    /// 参数：key 为键名，value 为值，均由前端约定语义
    /// 核心流程：先做字节长度校验（key 1 到 64 字节、value 不超过 4096 字节），
    /// 再用 UPSERT 落库；同一 key 只保留一行。
    pub async fn settings_set(&self, key: &str, value: &str) -> CoreResult<()> {
        if key.is_empty() {
            return Err(CoreError::Validation("设置键名不能为空".into()));
        }
        if key.len() > SETTING_KEY_MAX_BYTES {
            return Err(CoreError::Validation(format!(
                "设置键名超过 {SETTING_KEY_MAX_BYTES} 字节上限"
            )));
        }
        if value.len() > SETTING_VALUE_MAX_BYTES {
            return Err(CoreError::Validation(format!(
                "设置值超过 {SETTING_VALUE_MAX_BYTES} 字节上限"
            )));
        }
        sqlx::query(
            "INSERT INTO app_setting (k, v) VALUES (?1, ?2) \
             ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 列出全部自定义主题。
    ///
    /// 无参数
    /// 核心流程：按创建时间升序整表取出，payload 原样带回，主题 JSON 的语义
    /// 解析交给前端，后端只保证结构与体积合法。
    pub async fn theme_list_custom(&self) -> CoreResult<Vec<CustomTheme>> {
        let rows = sqlx::query(
            "SELECT id, name, kind, payload FROM custom_theme \
             ORDER BY created_at, id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| CustomTheme {
                id: row.get("id"),
                name: row.get("name"),
                kind: row.get("kind"),
                payload: row.get("payload"),
            })
            .collect())
    }

    /// 保存（新增或覆盖）一个自定义主题。
    ///
    /// 参数：name 为主题名称，kind 为 dark 或 light，payload 为前端主题 JSON 的
    /// 原始字符串，id 为 Some 时按该 id 覆盖、为 None 时由后端生成
    /// 核心流程：先做结构校验（名称非空且不超 64 字节、kind 取值合法、payload
    /// 不超 16384 字节且能解析成 JSON 对象），再以 UPSERT 落库并刷新 created_at，
    /// 最后返回实际使用的 id。
    pub async fn theme_save_custom(
        &self,
        name: &str,
        kind: &str,
        payload: &str,
        id: Option<&str>,
    ) -> CoreResult<String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(CoreError::Validation("主题名称不能为空".into()));
        }
        if name.len() > THEME_NAME_MAX_BYTES {
            return Err(CoreError::Validation(format!(
                "主题名称超过 {THEME_NAME_MAX_BYTES} 字节上限"
            )));
        }
        if !matches!(kind, "dark" | "light") {
            return Err(CoreError::Validation(format!(
                "主题类型只能是 dark 或 light，收到 {kind}"
            )));
        }
        if payload.len() > THEME_PAYLOAD_MAX_BYTES {
            return Err(CoreError::Validation(format!(
                "主题内容超过 {THEME_PAYLOAD_MAX_BYTES} 字节上限"
            )));
        }
        // 只要求能解析成 JSON 对象：字段含义由前端负责，后端不做语义兜底
        match serde_json::from_str::<serde_json::Value>(payload) {
            Ok(serde_json::Value::Object(_)) => {}
            Ok(_) => {
                return Err(CoreError::Validation("主题内容必须是 JSON 对象".into()));
            }
            Err(err) => {
                return Err(CoreError::Validation(format!(
                    "主题内容不是合法 JSON：{err}"
                )));
            }
        }
        // 空字符串视为未提供 id，统一由后端生成，避免出现空主键
        let theme_id = match id.map(str::trim).filter(|value| !value.is_empty()) {
            Some(existing) => existing.to_string(),
            None => Uuid::new_v4().to_string(),
        };
        sqlx::query(
            "INSERT INTO custom_theme (id, name, kind, payload, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, kind = excluded.kind, \
             payload = excluded.payload, created_at = excluded.created_at",
        )
        .bind(&theme_id)
        .bind(name)
        .bind(kind)
        .bind(payload)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(theme_id)
    }

    /// 删除一个自定义主题。
    ///
    /// 参数：id 为主题 id
    /// 核心流程：按主键删除，返回受影响行数，前端据此区分"删掉了"与"本来就不存在"。
    pub async fn theme_delete_custom(&self, id: &str) -> CoreResult<u64> {
        let result = sqlx::query("DELETE FROM custom_theme WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// 列出全部隧道配置。
    ///
    /// 无参数
    /// 核心流程：按创建时间升序整表取出，前端列表顺序与用户新增顺序一致。
    pub async fn tunnel_config_list(&self) -> CoreResult<Vec<TunnelConfig>> {
        let sql = format!("SELECT {TUNNEL_COLUMNS} FROM tunnel_config ORDER BY created_at, id");
        let rows = sqlx::query_as::<_, TunnelConfigRow>(&sql)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(TunnelConfig::try_from).collect()
    }

    /// 按标识取单条隧道配置。
    ///
    /// 参数：id 为配置标识
    /// 核心流程：查不到返回 None，由命令层决定是报未找到还是回退到运行态快照。
    pub async fn tunnel_config_get(&self, id: &str) -> CoreResult<Option<TunnelConfig>> {
        let sql = format!("SELECT {TUNNEL_COLUMNS} FROM tunnel_config WHERE id = ?1");
        let row = sqlx::query_as::<_, TunnelConfigRow>(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(TunnelConfig::try_from).transpose()
    }

    /// 新建隧道配置。
    ///
    /// 参数：input 为用户填写的转发参数
    /// 核心流程：校验类型与必填 → 确认所属会话存在（外键也会兜底，但先给出
    /// 可读错误）→ 名称为空时按转发关系自动生成 → 插入并返回落库结果。
    pub async fn tunnel_config_insert(
        &self,
        input: &TunnelConfigInput,
    ) -> CoreResult<TunnelConfig> {
        input.validate()?;
        self.get_session(input.session_id.trim()).await?;
        let now = now_ms();
        let config = TunnelConfig {
            id: Uuid::new_v4().to_string(),
            name: input.effective_name(),
            session_id: input.session_id.trim().to_string(),
            kind: input.kind.trim().to_string(),
            listen_host: input.listen_host.trim().to_string(),
            listen_port: input.listen_port,
            target_host: input.target_host.trim().to_string(),
            target_port: input.target_port,
            auto_start: input.auto_start,
            created_at: now,
            updated_at: now,
        };
        sqlx::query(
            "INSERT INTO tunnel_config \
             (id, name, session_id, kind, listen_host, listen_port, target_host, target_port, \
             auto_start, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(&config.id)
        .bind(&config.name)
        .bind(&config.session_id)
        .bind(&config.kind)
        .bind(&config.listen_host)
        .bind(i64::from(config.listen_port))
        .bind(&config.target_host)
        .bind(i64::from(config.target_port))
        .bind(i64::from(config.auto_start))
        .bind(config.created_at)
        .bind(config.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(config)
    }

    /// 更新隧道配置。
    ///
    /// 参数：id 为配置标识，input 为新内容
    /// 核心流程：校验 → 确认配置与所属会话都存在 → 覆盖可编辑字段并刷新更新时间，
    /// 保留创建时间；更新时间取「当前时间与上次更新 +1」的较大值，
    /// 保证同一毫秒内连续更新也能看出先后。
    pub async fn tunnel_config_update(
        &self,
        id: &str,
        input: &TunnelConfigInput,
    ) -> CoreResult<TunnelConfig> {
        input.validate()?;
        let existing = self
            .tunnel_config_get(id)
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("隧道配置 {id}")))?;
        self.get_session(input.session_id.trim()).await?;
        let updated_at = now_ms().max(existing.updated_at + 1);
        let config = TunnelConfig {
            id: existing.id,
            name: input.effective_name(),
            session_id: input.session_id.trim().to_string(),
            kind: input.kind.trim().to_string(),
            listen_host: input.listen_host.trim().to_string(),
            listen_port: input.listen_port,
            target_host: input.target_host.trim().to_string(),
            target_port: input.target_port,
            auto_start: input.auto_start,
            created_at: existing.created_at,
            updated_at,
        };
        sqlx::query(
            "UPDATE tunnel_config SET name = ?1, session_id = ?2, kind = ?3, listen_host = ?4, \
             listen_port = ?5, target_host = ?6, target_port = ?7, auto_start = ?8, \
             updated_at = ?9 \
             WHERE id = ?10",
        )
        .bind(&config.name)
        .bind(&config.session_id)
        .bind(&config.kind)
        .bind(&config.listen_host)
        .bind(i64::from(config.listen_port))
        .bind(&config.target_host)
        .bind(i64::from(config.target_port))
        .bind(i64::from(config.auto_start))
        .bind(config.updated_at)
        .bind(&config.id)
        .execute(&self.pool)
        .await?;
        Ok(config)
    }

    /// 删除隧道配置。
    ///
    /// 参数：id 为配置标识
    /// 核心流程：直接按主键删除且不报"不存在"——前端关窗时可能删一个已被
    /// 级联清掉的配置，重复删除应当是幂等的。
    pub async fn tunnel_config_delete(&self, id: &str) -> CoreResult<()> {
        sqlx::query("DELETE FROM tunnel_config WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// 一个用户导入的自定义主题。
///
/// 无参数
/// 核心流程：只承载前端可直接消费的四个字段，payload 是原始主题 JSON 字符串，
/// 后端不做语义解释，便于后续原样导出给他人复用。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomTheme {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub payload: String,
}

/// 一条凭据的元信息。
///
/// 无参数
/// 核心流程：只承载可展示字段，密文与 nonce 不经过前端。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Credential {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub updated_at: i64,
}

/// 一条凭据的密文数据。
///
/// 无参数
/// 核心流程：交给加密层解密，明文只在这一层之上短暂存在。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSecret {
    pub kind: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

/// 一条已知主机记录。
///
/// 无参数
/// 核心流程：承载主机的固定指纹，用于比对与展示。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownHost {
    pub host: String,
    pub port: u16,
    pub key_type: String,
    pub fingerprint: String,
}

/// 去除首尾空白，空串归一为 None。
///
/// 参数：value 为原始可选字符串
/// 核心流程：避免把空字符串写进库导致"有值但是空"的歧义。
fn normalize(value: &Option<String>) -> Option<String> {
    value
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// 生成不与现有名称冲突的副本名。
///
/// 参数：base 为原名，existing 为库中已有名称
/// 核心流程：先试 `<原名> 副本`，重复则依次追加序号，确保不覆盖任何已有会话。
fn derive_copy_name(base: &str, existing: &[String]) -> String {
    let pattern = format!("{base} 副本");
    if !existing.iter().any(|name| name == &pattern) {
        return pattern;
    }
    let mut index = 2;
    loop {
        let candidate = format!("{pattern} {index}");
        if !existing.iter().any(|name| name == &candidate) {
            return candidate;
        }
        index += 1;
    }
}
