use serde::Serialize;

use crate::error::CoreResult;
use crate::model::{now_ms, Session, SessionGroup};
use crate::store::Store;

/// 导出文件的结构。
///
/// 无参数
/// 核心流程：固定 formatVersion 以便将来升级格式时做兼容判断；
/// 结构内不含任何凭据字段，默认导出即为脱敏结果。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportBundle {
    format_version: u32,
    exported_at: i64,
    groups: Vec<SessionGroup>,
    sessions: Vec<Session>,
}

/// 当前导出格式版本。
const FORMAT_VERSION: u32 = 1;

/// 把会话库导出为 JSON 文本。
///
/// 参数：store 为会话库
/// 核心流程：一次性取全部分组与会话 → 打包成带版本号的文档 → 序列化为 JSON。
/// 注意：凭据导出要等 E6 的凭据库落地，此处不预留空参数。
pub async fn export_json(store: &Store) -> CoreResult<String> {
    let bundle = ExportBundle {
        format_version: FORMAT_VERSION,
        exported_at: now_ms(),
        groups: store.list_groups().await?,
        sessions: store.list_sessions("").await?,
    };
    Ok(serde_json::to_string_pretty(&bundle)?)
}
