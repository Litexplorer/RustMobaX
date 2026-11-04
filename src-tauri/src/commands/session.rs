use rustmobax_core::{
    apply_import, export_json, parse_mxtsessions, ErrorPayload, ImportPreview, ImportReport,
    Session, SessionGroup, SessionInput, Store, Vault,
};
use tauri::State;

use crate::state::AppState;

/// 列出全部分组。
///
/// 参数：state 为应用状态
/// 核心流程：直接转发到会话库，由前端组装成树。
#[tauri::command]
pub async fn list_groups(state: State<'_, AppState>) -> Result<Vec<SessionGroup>, ErrorPayload> {
    state.store.list_groups().await.map_err(ErrorPayload::from)
}

/// 新建分组。
///
/// 参数：name 为分组名，parent_id 为父分组
/// 核心流程：校验与写库交给核心层，命令层只做参数透传。
#[tauri::command]
pub async fn create_group(
    state: State<'_, AppState>,
    name: String,
    parent_id: Option<String>,
) -> Result<SessionGroup, ErrorPayload> {
    state
        .store
        .create_group(&name, parent_id.as_deref())
        .await
        .map_err(ErrorPayload::from)
}

/// 重命名分组。
///
/// 参数：id 为分组标识，name 为新名称
/// 核心流程：透传到核心层并归一化错误。
#[tauri::command]
pub async fn rename_group(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> Result<(), ErrorPayload> {
    state
        .store
        .rename_group(&id, &name)
        .await
        .map_err(ErrorPayload::from)
}

/// 删除分组。
///
/// 参数：id 为分组标识
/// 核心流程：核心层负责把组内会话与子分组上移，命令层不做额外处理。
#[tauri::command]
pub async fn delete_group(state: State<'_, AppState>, id: String) -> Result<(), ErrorPayload> {
    state
        .store
        .delete_group(&id)
        .await
        .map_err(ErrorPayload::from)
}

/// 移动分组。
///
/// 参数：id 为分组标识，parent_id 为新父级
/// 核心流程：核心层做环检测，命令层只转发。
#[tauri::command]
pub async fn move_group(
    state: State<'_, AppState>,
    id: String,
    parent_id: Option<String>,
) -> Result<(), ErrorPayload> {
    state
        .store
        .move_group(&id, parent_id.as_deref())
        .await
        .map_err(ErrorPayload::from)
}

/// 列出会话。
///
/// 参数：keyword 为搜索关键字，空串表示全量
/// 核心流程：透传搜索到会话库。
#[tauri::command]
pub async fn list_sessions(
    state: State<'_, AppState>,
    keyword: String,
) -> Result<Vec<Session>, ErrorPayload> {
    state
        .store
        .list_sessions(&keyword)
        .await
        .map_err(ErrorPayload::from)
}

/// 新建会话。
///
/// 参数：input 为用户填写的会话信息
/// 核心流程：核心层完成校验与落库，返回带标识的会话。
#[tauri::command]
pub async fn create_session(
    state: State<'_, AppState>,
    input: SessionInput,
) -> Result<Session, ErrorPayload> {
    state
        .store
        .create_session(&input)
        .await
        .map_err(ErrorPayload::from)
}

/// 更新会话。
///
/// 参数：id 为会话标识，input 为新内容
/// 核心流程：保留创建时间，刷新更新时间与可编辑字段。
#[tauri::command]
pub async fn update_session(
    state: State<'_, AppState>,
    id: String,
    input: SessionInput,
) -> Result<Session, ErrorPayload> {
    state
        .store
        .update_session(&id, &input)
        .await
        .map_err(ErrorPayload::from)
}

/// 删除会话。
///
/// 参数：id 为会话标识
/// 核心流程：透传到会话库。
#[tauri::command]
pub async fn delete_session(state: State<'_, AppState>, id: String) -> Result<(), ErrorPayload> {
    state
        .store
        .delete_session(&id)
        .await
        .map_err(ErrorPayload::from)
}

/// 移动会话到目标分组。
///
/// 参数：id 为会话标识，group_id 为目标分组
/// 核心流程：透传到会话库。
#[tauri::command]
pub async fn move_session(
    state: State<'_, AppState>,
    id: String,
    group_id: Option<String>,
) -> Result<(), ErrorPayload> {
    state
        .store
        .move_session(&id, group_id.as_deref())
        .await
        .map_err(ErrorPayload::from)
}

/// 复制会话。
///
/// 参数：id 为被复制的会话标识
/// 核心流程：核心层派生不冲突的名称并插入副本。
#[tauri::command]
pub async fn copy_session(state: State<'_, AppState>, id: String) -> Result<Session, ErrorPayload> {
    state
        .store
        .copy_session(&id)
        .await
        .map_err(ErrorPayload::from)
}

/// 解析会话文件并生成导入预览。
///
/// 参数：content 为 .mxtsessions 文件全文
/// 核心流程：只解析不写库，让前端先确认再落地。
#[tauri::command]
pub fn preview_import(content: String) -> ImportPreview {
    parse_mxtsessions(&content)
}

/// 执行导入。
///
/// 参数：preview 为前端确认过的预览结果
/// 核心流程：按预览写库，返回实际导入与跳过明细。
#[tauri::command]
pub async fn apply_import_preview(
    state: State<'_, AppState>,
    preview: ImportPreview,
) -> Result<ImportReport, ErrorPayload> {
    apply_import(&state.store, &preview)
        .await
        .map_err(ErrorPayload::from)
}

/// 导出会话库为 JSON 文本。
///
/// 参数：state 为应用状态
/// 核心流程：核心层产出脱敏 JSON，命令层原样返回给前端保存。
#[tauri::command]
pub async fn export_sessions(state: State<'_, AppState>) -> Result<String, ErrorPayload> {
    export_json(&state.store).await.map_err(ErrorPayload::from)
}

/// 会在首次启动时创建会话库。
///
/// 参数：db_path 为数据库文件绝对路径
/// 核心流程：打开库并完成建表，连同空的终端、SFTP、隧道与传输取消表一起返回。
pub async fn build_state(db_path: &std::path::Path) -> Result<AppState, String> {
    let store = Store::open(db_path)
        .await
        .map_err(|err| format!("会话库初始化失败：{err}"))?;
    Ok(AppState {
        vault: Vault::new(store.clone()),
        store,
        terminals: Default::default(),
        sftp: Default::default(),
        tunnels: Default::default(),
        transfers: Default::default(),
        metrics: Default::default(),
    })
}
