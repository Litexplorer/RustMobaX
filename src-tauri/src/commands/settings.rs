use std::collections::HashMap;

use rustmobax_core::{CoreError, ErrorPayload};
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
/// 参数：state 为应用状态，app 为应用句柄，key 为键名，value 为值，语义由前端约定
/// 核心流程：长度校验与 UPSERT 都在核心层完成，命令只做参数转发；
/// 同一 key 重复写入即覆盖，不会产生多行。写库成功后，`mcp.` 开头的键会立刻应用到
/// MCP 服务上；此时服务起不来不回滚设置，但会把原因并进返回的错误里，避免前端以为
/// 写入失败而重复提交。
#[tauri::command]
pub async fn settings_set(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    key: String,
    value: String,
) -> Result<(), ErrorPayload> {
    state
        .store
        .settings_set(&key, &value)
        .await
        .map_err(ErrorPayload::from)?;

    if key.starts_with("mcp.") {
        if let Err(error) = crate::mcp::apply_from_settings(&app).await {
            return Err(ErrorPayload::from(CoreError::Validation(format!(
                "设置已保存，但服务启动失败：{}",
                error.message
            ))));
        }
    }
    Ok(())
}
