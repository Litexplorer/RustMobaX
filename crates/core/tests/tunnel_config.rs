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
    }
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
