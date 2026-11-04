use rustmobax_core::{CoreError, Store};

/// 建立内存会话库。
///
/// 参数：无
/// 核心流程：每个用例独立建库，避免用例间互相污染。
async fn new_store() -> Store {
    Store::open_in_memory().await.expect("内存库应可建立")
}

#[tokio::test]
async fn settings_round_trip() {
    let store = new_store().await;
    store.settings_set("ui.theme", "dark").await.unwrap();
    store.settings_set("terminal.fontSize", "14").await.unwrap();

    let all = store.settings_get_all().await.unwrap();
    // 按 key 升序返回，且写入的键都能原样读回
    assert_eq!(
        all,
        vec![
            ("terminal.fontSize".to_string(), "14".to_string()),
            ("ui.theme".to_string(), "dark".to_string()),
        ]
    );
    // 未写入的键不落库，由前端兜底
    assert!(!all.iter().any(|(k, _)| k == "ui.unknown"));
}

#[tokio::test]
async fn settings_overwrite_keeps_single_row() {
    let store = new_store().await;
    store.settings_set("ui.theme", "dark").await.unwrap();
    store.settings_set("ui.theme", "light").await.unwrap();

    let all = store.settings_get_all().await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].1, "light");
}

#[tokio::test]
async fn settings_reject_oversized_input() {
    let store = new_store().await;

    // 键名超过 64 字节
    let long_key = "k".repeat(65);
    let err = store.settings_set(&long_key, "v").await.unwrap_err();
    assert!(matches!(err, CoreError::Validation(_)), "应为校验错误");

    // 值超过 4096 字节
    let long_value = "v".repeat(4097);
    let err = store.settings_set("ui.theme", &long_value).await.unwrap_err();
    assert!(matches!(err, CoreError::Validation(_)), "应为校验错误");

    // 边界值恰好合法，且超限输入不应落库
    store.settings_set(&"k".repeat(64), &"v".repeat(4096)).await.unwrap();
    assert_eq!(store.settings_get_all().await.unwrap().len(), 1);
}
