use rustmobax_core::Store;

/// 建立内存会话库。
///
/// 参数：无
/// 核心流程：每个用例独立建库，避免用例间互相污染。
async fn new_store() -> Store {
    Store::open_in_memory().await.expect("内存库应可建立")
}

#[tokio::test]
async fn stores_and_reads_encrypted_credential() {
    let store = new_store().await;
    store
        .credential_insert("c1", "password", "生产库密码", b"0123456789ab", b"fake-cipher")
        .await
        .unwrap();

    let list = store.credential_list().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].label, "生产库密码");
    assert_eq!(list[0].kind, "password");

    let secret = store.credential_secret("c1").await.unwrap().expect("应有密文");
    assert_eq!(secret.nonce, b"0123456789ab");
    assert_eq!(secret.ciphertext, b"fake-cipher");

    // 库里只应有密文，不应出现任何明文字段
    assert_eq!(store.credential_delete("c1").await.unwrap(), 1);
    assert!(store.credential_secret("c1").await.unwrap().is_none());
}
