use rustmobax_core::{AuthKind, Protocol, SessionInput, Store, TunnelConfigInput};

/// 建立内存会话库。
///
/// 参数：无
/// 核心流程：每个用例独立建库，避免用例间互相污染；
/// 内存库同样打开了外键约束，级联删除行为与线上一致。
async fn new_store() -> Store {
    Store::open_in_memory().await.expect("内存库应可建立")
}

/// 新建一个可用于挂隧道的会话。
///
/// 参数：store 为会话库
/// 核心流程：填齐校验要求的最小字段，返回会话标识供隧道配置引用。
async fn new_session(store: &Store) -> String {
    let input = SessionInput {
        name: "web-01".to_string(),
        group_id: None,
        protocol: Protocol::Ssh,
        host: Some("10.0.0.1".to_string()),
        port: Some(22),
        username: Some("root".to_string()),
        auth_kind: AuthKind::Password,
        extra: None,
    };
    store.create_session(&input).await.expect("会话应可建立").id
}

/// 构造一条本地转发配置入参。
///
/// 参数：session_id 为所属会话
/// 核心流程：给出本地 8080 -> db.internal:5432 的典型形态，用例按需改写。
fn local_input(session_id: &str) -> TunnelConfigInput {
    TunnelConfigInput {
        name: "数据库".to_string(),
        session_id: session_id.to_string(),
        kind: "local".to_string(),
        listen_host: "127.0.0.1".to_string(),
        listen_port: 8080,
        target_host: "db.internal".to_string(),
        target_port: 5432,
        auto_start: false,
    }
}

/// 造一个只有旧版 tunnel_config（没有 auto_start 列）的库文件。
///
/// 参数：tag 为临时目录后缀，便于区分用例
/// 核心流程：建临时目录 → 用 sqlx 直接建旧表并插一行 → 关连接 → 返回目录与库路径，
/// 交给 Store::open 走一次真实迁移。
async fn legacy_db(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let (dir, path) = temp_db(tag);
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let pool = sqlx::sqlite::SqlitePool::connect_with(options)
        .await
        .expect("旧库应可建立");
    sqlx::query(
        "CREATE TABLE tunnel_config (\
         id TEXT PRIMARY KEY, name TEXT NOT NULL, session_id TEXT NOT NULL, \
         kind TEXT NOT NULL, listen_host TEXT NOT NULL DEFAULT '', \
         listen_port INTEGER NOT NULL DEFAULT 0, target_host TEXT NOT NULL DEFAULT '', \
         target_port INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL, \
         updated_at INTEGER NOT NULL)",
    )
    .execute(&pool)
    .await
    .expect("旧表应可建立");
    sqlx::query(
        "INSERT INTO tunnel_config \
         (id, name, session_id, kind, listen_host, listen_port, target_host, target_port, \
         created_at, updated_at) \
         VALUES ('legacy-1', '老配置', 'sess-1', 'local', '127.0.0.1', 9000, 'db', 5432, 1, 2)",
    )
    .execute(&pool)
    .await
    .expect("旧数据应可插入");
    pool.close().await;
    (dir, path)
}

/// 判读某列是否已在表里。
///
/// 参数：path 为库文件路径，column 为列名
/// 核心流程：用 pragma_table_info 数一下命中数，>0 即存在。
async fn column_exists(path: &std::path::Path, column: &str) -> bool {
    let options = sqlx::sqlite::SqliteConnectOptions::new().filename(path);
    let pool = sqlx::sqlite::SqlitePool::connect_with(options)
        .await
        .expect("库应可打开");
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('tunnel_config') WHERE name = ?1",
    )
    .bind(column)
    .fetch_one(&pool)
    .await
    .expect("列信息应可查询");
    pool.close().await;
    count > 0
}

/// 准备一个空的磁盘库位置。
///
/// 参数：tag 为临时目录后缀
/// 核心流程：只建目录并拼路径，建库交给 Store::open。
fn temp_db(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("rmx-tunnel-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("临时目录应可建立");
    let path = dir.join("sessions.db");
    (dir, path)
}

/// 清掉临时目录。
///
/// 参数：dir 为临时目录
/// 核心流程：尽力删除，失败不影响断言结果。
fn cleanup(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn creates_tunnel_config_and_reads_it_back() {
    let store = new_store().await;
    let session_id = new_session(&store).await;

    let created = store
        .tunnel_config_insert(&local_input(&session_id))
        .await
        .unwrap();
    assert_eq!(created.name, "数据库");
    assert_eq!(created.kind, "local");
    assert!(created.created_at > 0);
    assert_eq!(created.created_at, created.updated_at);

    let read = store
        .tunnel_config_get(&created.id)
        .await
        .unwrap()
        .expect("刚建的配置应能读回");
    assert_eq!(read, created);
    assert_eq!(read.session_id, session_id);
    assert_eq!(read.listen_port, 8080);
    assert_eq!(read.target_host, "db.internal");
    assert_eq!(read.target_port, 5432);

    let all = store.tunnel_config_list().await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, created.id);
}

#[tokio::test]
async fn unknown_tunnel_config_reads_none() {
    let store = new_store().await;
    assert!(store.tunnel_config_get("missing").await.unwrap().is_none());
}

#[tokio::test]
async fn updates_tunnel_config_and_bumps_updated_at() {
    let store = new_store().await;
    let session_id = new_session(&store).await;
    let created = store
        .tunnel_config_insert(&local_input(&session_id))
        .await
        .unwrap();

    let mut changed = local_input(&session_id);
    changed.name = "数据库（备用）".to_string();
    changed.listen_port = 18080;
    changed.target_host = "db2.internal".to_string();
    changed.target_port = 1521;
    let updated = store
        .tunnel_config_update(&created.id, &changed)
        .await
        .unwrap();
    assert_eq!(updated.id, created.id);
    assert_eq!(updated.created_at, created.created_at);
    assert!(
        updated.updated_at > created.updated_at,
        "更新时间必须严格变大：{} -> {}",
        created.updated_at,
        updated.updated_at
    );

    let read = store.tunnel_config_get(&created.id).await.unwrap().unwrap();
    assert_eq!(read, updated);
    assert_eq!(read.name, "数据库（备用）");
    assert_eq!(read.listen_port, 18080);
    assert_eq!(read.target_host, "db2.internal");
    assert_eq!(read.target_port, 1521);
}

#[tokio::test]
async fn deletes_tunnel_config() {
    let store = new_store().await;
    let session_id = new_session(&store).await;
    let created = store
        .tunnel_config_insert(&local_input(&session_id))
        .await
        .unwrap();

    store.tunnel_config_delete(&created.id).await.unwrap();
    assert!(store
        .tunnel_config_get(&created.id)
        .await
        .unwrap()
        .is_none());
    assert!(store.tunnel_config_list().await.unwrap().is_empty());

    // 重复删除是幂等的 -> 前端关窗时重复调用不该报错
    store.tunnel_config_delete(&created.id).await.unwrap();
}

#[tokio::test]
async fn deleting_session_cascades_tunnel_configs() {
    let store = new_store().await;
    let session_id = new_session(&store).await;
    let created = store
        .tunnel_config_insert(&local_input(&session_id))
        .await
        .unwrap();

    store.delete_session(&session_id).await.unwrap();

    assert!(store
        .tunnel_config_get(&created.id)
        .await
        .unwrap()
        .is_none());
    assert!(store.tunnel_config_list().await.unwrap().is_empty());
}

#[tokio::test]
async fn generates_name_when_missing() {
    let store = new_store().await;
    let session_id = new_session(&store).await;

    let mut unnamed = local_input(&session_id);
    unnamed.name = "  ".to_string();
    let created = store.tunnel_config_insert(&unnamed).await.unwrap();
    assert!(!created.name.trim().is_empty(), "名称不能为空");
    assert_eq!(created.name, "本地 8080 → db.internal:5432");

    // 动态转发没有目标 -> 生成 SOCKS5 监听点描述
    let mut dynamic = local_input(&session_id);
    dynamic.name = String::new();
    dynamic.kind = "dynamic".to_string();
    dynamic.listen_host = String::new();
    dynamic.listen_port = 0;
    dynamic.target_host = String::new();
    dynamic.target_port = 0;
    let created = store.tunnel_config_insert(&dynamic).await.unwrap();
    assert_eq!(created.name, "动态 SOCKS5 127.0.0.1:自动");

    // 更新时留空同样重新生成，避免列表里出现空名字
    let mut renamed = dynamic.clone();
    renamed.name = String::new();
    renamed.listen_port = 1080;
    let updated = store
        .tunnel_config_update(&created.id, &renamed)
        .await
        .unwrap();
    assert_eq!(updated.name, "动态 SOCKS5 127.0.0.1:1080");
}

#[tokio::test]
async fn rejects_tunnel_config_for_missing_session() {
    let store = new_store().await;

    let err = store
        .tunnel_config_insert(&local_input("no-such-session"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("未找到"), "应报会话不存在：{err}");
}

#[tokio::test]
async fn rejects_unknown_tunnel_kind() {
    let store = new_store().await;
    let session_id = new_session(&store).await;

    let mut bad = local_input(&session_id);
    bad.kind = "socks".to_string();
    let err = store.tunnel_config_insert(&bad).await.unwrap_err();
    assert!(err.to_string().contains("隧道类型"), "应报类型非法：{err}");
}
#[tokio::test]
async fn stores_auto_start_true_on_create() {
    let store = new_store().await;
    let session_id = new_session(&store).await;

    let mut input = local_input(&session_id);
    input.auto_start = true;
    let created = store.tunnel_config_insert(&input).await.unwrap();
    assert!(created.auto_start, "入参为 true 时应落库为 true");

    let read = store.tunnel_config_get(&created.id).await.unwrap().unwrap();
    assert!(read.auto_start, "读回时仍应为 true");
    assert_eq!(read, created);

    let all = store.tunnel_config_list().await.unwrap();
    assert_eq!(all.len(), 1);
    assert!(all[0].auto_start);
}

#[tokio::test]
async fn update_can_turn_auto_start_off() {
    let store = new_store().await;
    let session_id = new_session(&store).await;

    let mut input = local_input(&session_id);
    input.auto_start = true;
    let created = store.tunnel_config_insert(&input).await.unwrap();
    assert!(created.auto_start);

    let mut off = local_input(&session_id);
    off.auto_start = false;
    let updated = store
        .tunnel_config_update(&created.id, &off)
        .await
        .unwrap();
    assert!(!updated.auto_start, "更新应能把 true 改回 false");

    let read = store.tunnel_config_get(&created.id).await.unwrap().unwrap();
    assert!(!read.auto_start, "落库值也必须是 false");
    assert_eq!(read, updated);
}

#[tokio::test]
async fn auto_start_defaults_to_false_when_not_given() {
    let store = new_store().await;
    let session_id = new_session(&store).await;

    // 老前端不发 autoStart，走 serde 默认值
    let raw = serde_json::json!({
        "name": "旧调用方",
        "sessionId": session_id,
        "kind": "local",
        "listenHost": "127.0.0.1",
        "listenPort": 8081,
        "targetHost": "db.internal",
        "targetPort": 5432,
    });
    let input: TunnelConfigInput = serde_json::from_value(raw).unwrap();
    assert!(!input.auto_start, "缺省入参应为 false");

    let created = store.tunnel_config_insert(&input).await.unwrap();
    assert!(!created.auto_start);
    let read = store.tunnel_config_get(&created.id).await.unwrap().unwrap();
    assert!(!read.auto_start);
}

#[tokio::test]
async fn migrate_is_idempotent_across_reopen() {
    let (dir, path) = temp_db("idempotent");

    let store = Store::open(&path).await.expect("首次建库应成功");
    let session_id = new_session(&store).await;
    let mut input = local_input(&session_id);
    input.auto_start = true;
    let created = store.tunnel_config_insert(&input).await.unwrap();
    drop(store);

    // 第二次打开 = 第二次 migrate，不能因重复 ALTER 报错
    let again = Store::open(&path).await.expect("重复 migrate 应成功");
    let list = again.tunnel_config_list().await.unwrap();
    assert_eq!(list.len(), 1, "重复迁移后数据仍在");
    assert!(list[0].auto_start);

    // 第三次同样稳定，且删除后重开仍为空
    let third = Store::open(&path).await.expect("第三次 migrate 应成功");
    assert_eq!(third.tunnel_config_list().await.unwrap().len(), 1);
    third.tunnel_config_delete(&created.id).await.unwrap();
    drop(third);
    drop(again);

    let last = Store::open(&path).await.expect("删除后重开应成功");
    assert!(last.tunnel_config_list().await.unwrap().is_empty());
    drop(last);
    cleanup(&dir);
}

#[tokio::test]
async fn migrate_adds_auto_start_to_legacy_table() {
    let (dir, path) = legacy_db("legacy").await;
    assert!(
        !column_exists(&path, "auto_start").await,
        "构造的旧表不应有 auto_start 列"
    );

    let store = Store::open(&path).await.expect("旧库升级应成功");
    assert!(
        column_exists(&path, "auto_start").await,
        "migrate 应把 auto_start 列补上"
    );

    let list = store.tunnel_config_list().await.unwrap();
    assert_eq!(list.len(), 1, "旧数据不能被丢掉");
    assert_eq!(list[0].id, "legacy-1");
    assert_eq!(list[0].name, "老配置");
    assert!(!list[0].auto_start, "补列后旧行的默认值应为 false");
    drop(store);

    cleanup(&dir);
}
