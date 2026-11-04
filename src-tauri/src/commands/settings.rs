use std::collections::HashMap;

use rustmobax_core::ErrorPayload;
use tauri::State;

use crate::state::AppState;

/// 读取全部应用设置。
///
/// 参数：state 为应用状态
/// 核心流程：一次性取出库中所有键值并转成前端可直接使用的对象；
/// 库里没有的键不出现在结果中，默认值由前端兜底。
#[tauri::command]
pub async fn settings_get_all(
    state: State<'_, AppState>,
) -> Result<HashMap<String, String>, ErrorPayload> {
    let pairs = state
        .store
        .settings_get_all()
        .await
        .map_err(ErrorPayload::from)?;
    Ok(pairs.into_iter().collect())
}

/// 写入一条应用设置。
///
/// 参数：key 为键名，value 为值，语义由前端约定
/// 核心流程：长度校验与 UPSERT 都在核心层完成，命令只做参数转发；
/// 同一 key 重复写入即覆盖，不会产生多行。
#[tauri::command]
pub async fn settings_set(
    state: State<'_, AppState>,
    key: String,
    value: String,
) -> Result<(), ErrorPayload> {
    state
        .store
        .settings_set(&key, &value)
        .await
        .map_err(ErrorPayload::from)
}
