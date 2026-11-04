use rustmobax_core::{
    apply_import, export_json, parse_mxtsessions, AuthKind, Protocol, SessionInput, Store,
};

/// 构造一个合法的 SSH 会话入参。
///
/// 参数：name 为会话名，host 为主机
/// 核心流程：填齐 E1 校验要求的字段，供各用例按需改写。
fn ssh_input(name: &str, host: &str) -> SessionInput {
    SessionInput {
        name: name.to_string(),
        group_id: None,
        protocol: Protocol::Ssh,
        host: Some(host.to_string()),
        port: Some(22),
        username: Some("root".to_string()),
        auth_kind: AuthKind::Password,
        extra: None,
    }
}

/// 建立内存会话库。
///
/// 参数：无
/// 核心流程：每个用例独立建库，避免用例间互相污染。
async fn new_store() -> Store {
    Store::open_in_memory().await.expect("内存库应可建立")
}

#[tokio::test]
async fn creates_session_and_lists_it() {
    let store = new_store().await;
    let created = store.create_session(&ssh_input("web-01", "10.0.0.1")).await.unwrap();
    assert_eq!(created.name, "web-01");
    assert_eq!(created.port, Some(22));

    let all = store.list_sessions("").await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, created.id);
}

#[tokio::test]
async fn rejects_session_missing_host() {
    let store = new_store().await;

    let mut no_host = ssh_input("no-host", "");
    no_host.host = None;
    let err = store.create_session(&no_host).await.unwrap_err();
    assert!(err.to_string().contains("必须填写主机"));
}

#[tokio::test]
async fn allows_session_without_username() {
    let store = new_store().await;

    // 用户名留空表示沿用远端登录名，对齐 MobaXterm 的可选语义
    let mut no_user = ssh_input("no-user", "10.0.0.2");
    no_user.username = None;
    let created = store.create_session(&no_user).await.unwrap();
    assert_eq!(created.username, None);
    assert_eq!(created.host.as_deref(), Some("10.0.0.2"));
}

#[tokio::test]
async fn local_session_does_not_require_host() {
    let store = new_store().await;
    let local = SessionInput {
        name: "本地终端".into(),
        group_id: None,
        protocol: Protocol::Local,
        host: None,
        port: None,
        username: None,
        auth_kind: AuthKind::Password,
        extra: None,
    };
    let created = store.create_session(&local).await.unwrap();
    assert_eq!(created.protocol, Protocol::Local);
    assert_eq!(created.port, None);
}

#[tokio::test]
async fn search_matches_name_host_username_and_group() {
    let store = new_store().await;
    let prod = store.create_group("生产", None).await.unwrap();

    let mut web = ssh_input("web-01", "10.0.0.1");
    web.group_id = Some(prod.id.clone());
    web.username = Some("deploy".into());
    store.create_session(&web).await.unwrap();
    store.create_session(&ssh_input("db-01", "10.0.0.2")).await.unwrap();

    assert_eq!(store.list_sessions("web").await.unwrap().len(), 1);
    assert_eq!(store.list_sessions("10.0.0.2").await.unwrap().len(), 1);
    assert_eq!(store.list_sessions("DEPLOY").await.unwrap().len(), 1);
    assert_eq!(store.list_sessions("生产").await.unwrap().len(), 1);
    assert_eq!(store.list_sessions("").await.unwrap().len(), 2);
    assert_eq!(store.list_sessions("nothing").await.unwrap().len(), 0);
}

#[tokio::test]
async fn deleting_group_moves_sessions_to_parent_instead_of_dropping_them() {
    let store = new_store().await;
    let parent = store.create_group("根组", None).await.unwrap();
    let child = store.create_group("子组", Some(&parent.id)).await.unwrap();

    let mut session = ssh_input("web-01", "10.0.0.1");
    session.group_id = Some(child.id.clone());
    let created = store.create_session(&session).await.unwrap();

    store.delete_group(&child.id).await.unwrap();

    let survivor = store.get_session(&created.id).await.unwrap();
    assert_eq!(survivor.group_id, Some(parent.id));
    assert_eq!(store.list_sessions("").await.unwrap().len(), 1);
}

#[tokio::test]
async fn moving_group_into_its_own_subtree_is_rejected() {
    let store = new_store().await;
    let root = store.create_group("A", None).await.unwrap();
    let child = store.create_group("B", Some(&root.id)).await.unwrap();

    let err = store.move_group(&root.id, Some(&child.id)).await.unwrap_err();
    assert!(err.to_string().contains("子分组"));

    store.move_group(&child.id, None).await.unwrap();
    let groups = store.list_groups().await.unwrap();
    let moved = groups.iter().find(|g| g.id == child.id).unwrap();
    assert_eq!(moved.parent_id, None);
}

#[tokio::test]
async fn copy_session_derives_unique_names() {
    let store = new_store().await;
    let source = store.create_session(&ssh_input("web-01", "10.0.0.1")).await.unwrap();

    let first = store.copy_session(&source.id).await.unwrap();
    let second = store.copy_session(&source.id).await.unwrap();

    assert_eq!(first.name, "web-01 副本");
    assert_eq!(second.name, "web-01 副本 2");
    assert_ne!(first.id, source.id);
    assert_eq!(store.list_sessions("").await.unwrap().len(), 3);
}

const SAMPLE_MXTSESSIONS: &str = "\
[Bookmarks]
SubRep=
ImgNum=41

[Bookmarks_1]
SubRep=Production
ImgNum=41

[Bookmarks_1_1]
SubRep=
ImgNum=41
SSHHost=10.0.0.1
SSHPort=2222
SSHUserName=deploy

[Bookmarks_1_2]
SubRep=
ImgNum=41
SSHHost=10.0.0.2

[Bookmarks_1_3]
SubRep=Staging
ImgNum=41

[Bookmarks_1_3_1]
SubRep=
ImgNum=41
SSHHost=10.0.1.1
SSHUserName=stage

[Bookmarks_2]
SubRep=
ImgNum=41
Type=2

[Bookmarks_3]
SubRep=
ImgNum=41
RdpHost=10.0.0.9
";

#[tokio::test]
async fn parses_mxtsessions_into_sessions_groups_and_skips() {
    let preview = parse_mxtsessions(SAMPLE_MXTSESSIONS);

    assert_eq!(preview.sessions.len(), 4, "三条 SSH 加一条本地终端");

    let first = &preview.sessions[0];
    assert_eq!(first.host.as_deref(), Some("10.0.0.1"));
    assert_eq!(first.port, Some(2222));
    assert_eq!(first.username.as_deref(), Some("deploy"));
    assert_eq!(first.group_path, vec!["Production".to_string()]);

    let nested = preview
        .sessions
        .iter()
        .find(|s| s.host.as_deref() == Some("10.0.1.1"))
        .expect("应解析出嵌套分组下的会话");
    assert_eq!(
        nested.group_path,
        vec!["Production".to_string(), "Staging".to_string()]
    );

    let local = preview
        .sessions
        .iter()
        .find(|s| s.protocol == Protocol::Local)
        .expect("应识别 Type=2 为本地终端");
    assert_eq!(local.name, "本地终端");

    assert_eq!(preview.skipped.len(), 1);
    assert_eq!(preview.skipped[0].section, "Bookmarks_3");
    assert!(preview.skipped[0].reason.contains("RdpHost"));
}

#[tokio::test]
async fn applies_import_and_reuses_existing_groups() {
    let store = new_store().await;
    // 先造一个同名分组 -> 导入时应复用而不是重复建
    store.create_group("Production", None).await.unwrap();

    let preview = parse_mxtsessions(SAMPLE_MXTSESSIONS);
    let report = apply_import(&store, &preview).await.unwrap();

    assert_eq!(report.imported, 4, "缺用户名的 SSH 段现在也能导入");
    assert_eq!(report.groups_created, 1, "只应新建 Staging");
    assert_eq!(report.skipped.len(), 1, "只有 RDP 段应被跳过");
    assert_eq!(store.list_sessions("").await.unwrap().len(), 4);

    let groups = store.list_groups().await.unwrap();
    assert_eq!(
        groups.iter().filter(|g| g.name == "Production").count(),
        1,
        "同名根分组不应重复创建"
    );
}

#[tokio::test]
async fn exports_groups_and_sessions_without_credentials() {
    let store = new_store().await;
    let group = store.create_group("生产", None).await.unwrap();
    let mut session = ssh_input("web-01", "10.0.0.1");
    session.group_id = Some(group.id.clone());
    store.create_session(&session).await.unwrap();

    let json = export_json(&store).await.unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(value["formatVersion"], 1);
    assert_eq!(value["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(value["groups"].as_array().unwrap().len(), 1);
    assert_eq!(value["sessions"][0]["host"], "10.0.0.1");

    // 认证方式 authKind="password" 是合法字段，这里只禁止出现凭据类键
    let keys: Vec<&str> = value["sessions"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert!(
        !keys
            .iter()
            .any(|key| key.contains("password") || key.contains("secret") || key.contains("credential")),
        "导出对象不得包含凭据字段，实际键：{keys:?}"
    );
}
