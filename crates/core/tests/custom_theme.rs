use std::path::{Path, PathBuf};

use rustmobax_core::{CoreError, Store};

/// 建立内存会话库。
///
/// 参数：无
/// 核心流程：每个用例独立建库，避免用例间互相污染。
async fn new_store() -> Store {
    Store::open_in_memory().await.expect("内存库应可建立")
}

/// 造一个磁盘库路径。
///
/// 参数：tag 为用例标识，便于排查残留文件
/// 核心流程：把随机目录放在临时目录下，需要直查 created_at 等内部列时用它建库。
fn temp_db_path(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rmx-theme-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("临时目录应可建立");
    dir.join("sessions.db")
}

/// 直接连上测试库。
///
/// 参数：path 为会话库文件路径
/// 核心流程：绕开 Store 的公开方法，用于断言 created_at 这类不对外暴露的列。
async fn connect_direct(path: &Path) -> sqlx::SqlitePool {
    let url = format!("sqlite://{}", path.display());
    sqlx::SqlitePool::connect(&url).await.expect("直连测试库应成功")
}

/// 读取某条主题的创建时间。
///
/// 参数：path 为会话库文件路径，id 为主题 id
/// 核心流程：新开一条连接查一行，读完即关闭，避免与 Store 的连接池互相阻塞。
async fn created_at_of(path: &Path, id: &str) -> i64 {
    let pool = connect_direct(path).await;
    let value: i64 = sqlx::query_scalar("SELECT created_at FROM custom_theme WHERE id = ?1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("主题行应存在");
    pool.close().await;
    value
}

#[tokio::test]
async fn theme_list_starts_empty() {
    let store = new_store().await;
    // 新建库没有任何自定义主题，列表应为空而不是报错
    let themes = store.theme_list_custom().await.unwrap();
    assert!(themes.is_empty(), "初始列表应为空");
}

#[tokio::test]
async fn theme_save_then_list_keeps_fields() {
    let store = new_store().await;
    let payload = r##"{"background":"#101010","accent":"#4f9cf9"}"##;

    let id = store
        .theme_save_custom("晨雾", "dark", payload, None)
        .await
        .unwrap();
    assert!(!id.is_empty(), "缺省 id 时应由后端生成");

    let themes = store.theme_list_custom().await.unwrap();
    assert_eq!(themes.len(), 1);
    assert_eq!(themes[0].id, id);
    assert_eq!(themes[0].name, "晨雾");
    assert_eq!(themes[0].kind, "dark");
    assert_eq!(themes[0].payload, payload, "payload 应原样存取");
}

#[tokio::test]
async fn theme_save_same_id_overwrites_single_row() {
    let path = temp_db_path("overwrite");
    let store = Store::open(&path).await.unwrap();

    let id = store
        .theme_save_custom("晨雾", "dark", r##"{"background":"#101010"}"##, None)
        .await
        .unwrap();

    // 把创建时间压到 1，覆盖写入后必然刷新成当前时间，断言不受毫秒精度影响
    let pool = connect_direct(&path).await;
    sqlx::query("UPDATE custom_theme SET created_at = 1 WHERE id = ?1")
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let same_id = store
        .theme_save_custom("晨雾改", "light", r##"{"background":"#fafafa"}"##, Some(&id))
        .await
        .unwrap();
    assert_eq!(same_id, id, "覆盖写入应沿用传入的 id");

    let themes = store.theme_list_custom().await.unwrap();
    assert_eq!(themes.len(), 1, "同 id 再保存不应新增行");
    assert_eq!(themes[0].name, "晨雾改");
    assert_eq!(themes[0].kind, "light");
    assert!(created_at_of(&path, &id).await > 1, "覆盖更新应刷新 created_at");

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn theme_save_rejects_invalid_input() {
    let store = new_store().await;
    let payload = r##"{"background":"#101010"}"##;

    // 非法 kind
    let err = store.theme_save_custom("晨雾", "sepia", payload, None).await.unwrap_err();
    assert!(matches!(err, CoreError::Validation(_)), "非法 kind 应为校验错误");

    // 空名称（含纯空白）
    let err = store.theme_save_custom("", "dark", payload, None).await.unwrap_err();
    assert!(matches!(err, CoreError::Validation(_)), "空名称应为校验错误");
    let err = store.theme_save_custom("   ", "dark", payload, None).await.unwrap_err();
    assert!(matches!(err, CoreError::Validation(_)), "纯空白名称应为校验错误");

    // 名称超过 64 字节
    let err = store
        .theme_save_custom(&"名".repeat(22), "dark", payload, None)
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Validation(_)), "超长名称应为校验错误");

    // payload 超过 16384 字节
    let big = format!(r#"{{"blob":"{}"}}"#, "x".repeat(16384));
    let err = store.theme_save_custom("晨雾", "dark", &big, None).await.unwrap_err();
    assert!(matches!(err, CoreError::Validation(_)), "超长 payload 应为校验错误");

    // payload 不是合法 JSON，或合法但不是对象
    let err = store.theme_save_custom("晨雾", "dark", "not-json", None).await.unwrap_err();
    assert!(matches!(err, CoreError::Validation(_)), "非 JSON 应为校验错误");
    let err = store.theme_save_custom("晨雾", "dark", "[1,2,3]", None).await.unwrap_err();
    assert!(matches!(err, CoreError::Validation(_)), "顶层非对象应为校验错误");

    // 边界值恰好合法，且被拒绝的输入一行都不应落库
    store
        .theme_save_custom(&"名".repeat(21), "light", payload, None)
        .await
        .unwrap();
    assert_eq!(store.theme_list_custom().await.unwrap().len(), 1);
}

#[tokio::test]
async fn theme_delete_returns_affected_rows() {
    let store = new_store().await;
    let payload = r##"{"background":"#101010"}"##;
    let id = store
        .theme_save_custom("晨雾", "dark", payload, None)
        .await
        .unwrap();

    // 第一次删除命中一行，第二次已不存在应返回 0 而不是报错
    assert_eq!(store.theme_delete_custom(&id).await.unwrap(), 1);
    assert_eq!(store.theme_delete_custom(&id).await.unwrap(), 0);
    assert!(store.theme_list_custom().await.unwrap().is_empty());

    // 删除不存在的 id 同样返回 0，前端可当作幂等处理
    assert_eq!(store.theme_delete_custom("missing-id").await.unwrap(), 0);
}

#[tokio::test]
async fn theme_list_orders_by_created_at() {
    let path = temp_db_path("order");
    let store = Store::open(&path).await.unwrap();
    let payload = r##"{"background":"#101010"}"##;

    let later = store
        .theme_save_custom("后建", "dark", payload, None)
        .await
        .unwrap();
    let earlier = store
        .theme_save_custom("先建", "light", payload, None)
        .await
        .unwrap();

    // 直属库固定创建时间，避免同一毫秒写入导致排序断言不稳定
    let pool = connect_direct(&path).await;
    for (id, created_at) in [(&later, 200_i64), (&earlier, 100_i64)] {
        sqlx::query("UPDATE custom_theme SET created_at = ?1 WHERE id = ?2")
            .bind(created_at)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
    pool.close().await;

    let names: Vec<String> = store
        .theme_list_custom()
        .await
        .unwrap()
        .into_iter()
        .map(|theme| theme.name)
        .collect();
    assert_eq!(names, vec!["先建".to_string(), "后建".to_string()]);

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
