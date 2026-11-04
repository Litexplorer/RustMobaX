use rustmobax_core::{CustomTheme, ErrorPayload};
use serde::Deserialize;
use tauri::State;

use crate::state::AppState;

/// 前端保存自定义主题时传入的载荷。
///
/// 无参数
/// 核心流程：字段按 camelCase 反序列化，id 缺省表示新建、给出已存在的 id
/// 表示覆盖；payload 是主题 JSON 原始字符串，语义由前端负责。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomThemeInput {
    /// 主题 id，缺省时由后端生成 UUID v4。
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    /// 主题类型，只接受 dark / light。
    pub kind: String,
    /// 主题 JSON 原始字符串，后端只校验它不超过上限且能解析为对象。
    pub payload: String,
}

/// 列出全部自定义主题。
///
/// 参数：state 为应用状态
/// 核心流程：直接透传核心层的列表结果，按创建时间升序返回，
/// payload 原样带回供前端解析。
#[tauri::command]
pub async fn theme_list_custom(
    state: State<'_, AppState>,
) -> Result<Vec<CustomTheme>, ErrorPayload> {
    state
        .store
        .theme_list_custom()
        .await
        .map_err(ErrorPayload::from)
}

/// 保存（新增或覆盖）一个自定义主题。
///
/// 参数：theme 为前端提交的主题载荷
/// 核心流程：结构校验与 UPSERT 都在核心层完成，命令只做参数转发，
/// 返回实际使用的 id 供前端回填。
#[tauri::command]
pub async fn theme_save_custom(
    state: State<'_, AppState>,
    theme: CustomThemeInput,
) -> Result<String, ErrorPayload> {
    state
        .store
        .theme_save_custom(&theme.name, &theme.kind, &theme.payload, theme.id.as_deref())
        .await
        .map_err(ErrorPayload::from)
}

/// 删除一个自定义主题。
///
/// 参数：id 为主题 id
/// 核心流程：返回受影响行数，0 表示该主题本来就不存在，前端按幂等处理即可。
#[tauri::command]
pub async fn theme_delete_custom(
    state: State<'_, AppState>,
    id: String,
) -> Result<u64, ErrorPayload> {
    state
        .store
        .theme_delete_custom(&id)
        .await
        .map_err(ErrorPayload::from)
}
