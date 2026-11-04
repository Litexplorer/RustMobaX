use rustmobax_core::{Credential, ErrorPayload};
use serde::Serialize;
use tauri::State;

use crate::state::AppState;

/// 凭据库的当前状态。
///
/// 无参数
/// 核心流程：区分"还没设主密码"与"设了但没解锁"，前端据此弹不同对话框。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultStatus {
    pub initialized: bool,
    pub unlocked: bool,
}

/// 查询凭据库状态。
///
/// 参数：state 为应用状态
/// 核心流程：一次返回是否设置过主密码与当前是否解锁。
#[tauri::command]
pub async fn vault_status(state: State<'_, AppState>) -> Result<VaultStatus, ErrorPayload> {
    let initialized = state.vault.is_initialized().await?;
    Ok(VaultStatus {
        initialized,
        unlocked: state.vault.is_unlocked(),
    })
}

/// 首次设置主密码。
///
/// 参数：password 为用户设置的主密码
/// 核心流程：核心层生成盐与校验块落库，成功后立即处于解锁态。
#[tauri::command]
pub async fn vault_setup(
    state: State<'_, AppState>,
    password: String,
) -> Result<(), ErrorPayload> {
    state.vault.setup(&password).await.map_err(ErrorPayload::from)
}

/// 解锁凭据库。
///
/// 参数：password 为用户输入的主密码
/// 核心流程：核心层用盐重新派生密钥并试解校验块，失败保持锁定。
#[tauri::command]
pub async fn vault_unlock(
    state: State<'_, AppState>,
    password: String,
) -> Result<(), ErrorPayload> {
    state.vault.unlock(&password).await.map_err(ErrorPayload::from)
}

/// 锁定凭据库。
///
/// 参数：state 为应用状态
/// 核心流程：丢弃内存密钥，后续读写都会要求重新解锁。
#[tauri::command]
pub fn vault_lock(state: State<'_, AppState>) -> Result<(), ErrorPayload> {
    state.vault.lock().map_err(ErrorPayload::from)
}

/// 保存一条凭据。
///
/// 参数：kind 为用途分类，label 为展示名，secret 为明文
/// 核心流程：要求已解锁，加密后只写密文，返回凭据标识供会话引用。
#[tauri::command]
pub async fn credential_save(
    state: State<'_, AppState>,
    kind: String,
    label: String,
    secret: String,
) -> Result<String, ErrorPayload> {
    state
        .vault
        .save_secret(&kind, &label, &secret)
        .await
        .map_err(ErrorPayload::from)
}

/// 取出单条凭据的明文。
///
/// 参数：state 为应用状态，id 为凭据标识
/// 核心流程：要求凭据库已解锁 → 复用核心层既有的解密路径取出明文 → 经 IPC 回前端；
/// 前端只把它交给本次认证（私钥口令），不写日志、不落库；
/// 未解锁（"凭据库已锁定，请先解锁"）与 id 不存在（NotFound）都是明确错误。
#[tauri::command]
pub async fn credential_reveal(
    state: State<'_, AppState>,
    id: String,
) -> Result<String, ErrorPayload> {
    state.vault.reveal(&id).await.map_err(ErrorPayload::from)
}

/// 列出全部凭据。
///
/// 参数：state 为应用状态
/// 核心流程：只回可展示的元信息，密文不出后端。
#[tauri::command]
pub async fn credential_list(
    state: State<'_, AppState>,
) -> Result<Vec<Credential>, ErrorPayload> {
    state
        .store
        .credential_list()
        .await
        .map_err(ErrorPayload::from)
}

/// 删除一条凭据。
///
/// 参数：id 为凭据标识
/// 核心流程：直接删记录，引用该凭据的会话下次连接时会退回手动输入。
#[tauri::command]
pub async fn credential_delete(
    state: State<'_, AppState>,
    id: String,
) -> Result<u64, ErrorPayload> {
    state
        .store
        .credential_delete(&id)
        .await
        .map_err(ErrorPayload::from)
}
