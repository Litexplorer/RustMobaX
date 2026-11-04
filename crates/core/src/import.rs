use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::CoreResult;
use crate::model::{AuthKind, Protocol, SessionInput};
use crate::store::Store;

/// 已知但本轮不支持的会话类型标识键。
///
/// 无参数
/// 核心流程：命中这些键的段计入 skipped 并说明具体类型，其余无连接信息的段按元数据处理。
const UNSUPPORTED_MARKERS: [&str; 6] = [
    "RdpHost",
    "RDPHost",
    "VncHost",
    "VNCHost",
    "TelnetHost",
    "SerialPort",
];

/// 导入预览中的一条会话。
///
/// 无参数
/// 核心流程：只描述将要写库的内容，不触碰数据库，便于先预览后确认。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingSession {
    pub name: String,
    /// 从根到叶的分组名链，空表示未分组。
    pub group_path: Vec<String>,
    pub protocol: Protocol,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub auth_kind: AuthKind,
}

/// 被跳过的会话段。
///
/// 无参数
/// 核心流程：保留段名与原因，让用户知道哪几条没导进来、为什么。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedEntry {
    pub section: String,
    pub reason: String,
}

/// 导入预览结果。
///
/// 无参数
/// 核心流程：同时给出可导入的会话、涉及的分组链与被跳过项，供前端弹确认框。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub sessions: Vec<PendingSession>,
    pub skipped: Vec<SkippedEntry>,
}

/// 导入执行结果。
///
/// 无参数
/// 核心流程：返回实际写入数量与新建分组数，便于前端给出结果反馈。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub imported: usize,
    pub groups_created: usize,
    pub skipped: Vec<SkippedEntry>,
}

/// 解析后的单个 ini 段。///
/// 无参数
/// 核心流程：记录段路径与键值，路径形如 ["1", "2"]，用于推导分组嵌套。
struct RawSection {
    path: Vec<String>,
    key: String,
    entries: HashMap<String, String>,
}

/// 解析 MobaXterm 会话文件。
///
/// 参数：content 为 .mxtsessions 文件全文
/// 核心流程：逐行切成 ini 段 → 先用 SubRep 段建立"路径到分组名"的映射 →
/// 再按最近的祖先分组把会话段归位，无法识别的类型计入 skipped 而不是静默丢弃。
pub fn parse_mxtsessions(content: &str) -> ImportPreview {
    let sections = split_sections(content);

    // 先收集分组 -> 拿到 path 与名称的对应关系
    let mut group_names: HashMap<Vec<String>, String> = HashMap::new();
    for section in &sections {
        let sub_rep = section
            .entries
            .get("SubRep")
            .map(|v| v.trim())
            .unwrap_or("");
        if !sub_rep.is_empty() {
            group_names.insert(section.path.clone(), sub_rep.to_string());
        }
    }

    let mut sessions = Vec::new();
    let mut skipped = Vec::new();
    for section in &sections {
        let has_sub_rep = section
            .entries
            .get("SubRep")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        if has_sub_rep {
            continue;
        }

        let ssh_host = section
            .entries
            .get("SSHHost")
            .map(|v| v.trim().to_string())
            .unwrap_or_default();
        let kind = section.entries.get("Type").map(|v| v.trim()).unwrap_or("");

        match (ssh_host.is_empty(), kind) {
            (false, _) => {
                let port = section
                    .entries
                    .get("SSHPort")
                    .and_then(|v| v.trim().parse::<u16>().ok());
                let username = section
                    .entries
                    .get("SSHUserName")
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty());
                sessions.push(PendingSession {
                    name: session_name(section, &ssh_host),
                    group_path: resolve_group_path(&section.path, &group_names),
                    protocol: Protocol::Ssh,
                    host: Some(ssh_host),
                    port: Some(port.unwrap_or(22)),
                    username,
                    auth_kind: AuthKind::Password,
                });
            }
            // Type=2 在 MobaXterm 中表示本地终端
            (true, "2") => {
                sessions.push(PendingSession {
                    name: session_name(section, "本地终端"),
                    group_path: resolve_group_path(&section.path, &group_names),
                    protocol: Protocol::Local,
                    host: None,
                    port: None,
                    username: None,
                    auth_kind: AuthKind::Password,
                });
            }
            _ => {
                // 只有带其它连接协议的段才算"跳过"，纯元数据段（如根段只有 ImgNum）静默忽略
                if let Some(marker) = UNSUPPORTED_MARKERS
                    .iter()
                    .find(|marker| section.entries.contains_key(**marker))
                {
                    skipped.push(SkippedEntry {
                        section: section.key.clone(),
                        reason: format!("暂不支持的会话类型（{marker}）"),
                    });
                }
            }
        }
    }

    ImportPreview { sessions, skipped }
}

/// 把预览内容写入会话库。
///
/// 参数：store 为目标会话库，preview 为解析结果
/// 核心流程：按分组的层级链逐级查找或创建分组 → 逐条创建会话，跳过项原样回传。
pub async fn apply_import(store: &Store, preview: &ImportPreview) -> CoreResult<ImportReport> {
    let mut groups_created = 0;
    let mut imported = 0;
    let mut skipped = preview.skipped.clone();

    for pending in &preview.sessions {
        let input = SessionInput {
            name: pending.name.clone(),
            group_id: None,
            protocol: pending.protocol,
            host: pending.host.clone(),
            port: pending.port,
            username: pending.username.clone(),
            auth_kind: pending.auth_kind,
            extra: None,
        };
        // 先校验再建分组 -> 不合格的会话不留下空分组
        if let Err(err) = input.validate() {
            skipped.push(SkippedEntry {
                section: pending.name.clone(),
                reason: format!("字段不完整：{err}"),
            });
            continue;
        }

        // 沿分组链逐级查找或创建 -> 复用已有分组
        let mut parent: Option<String> = None;
        for name in &pending.group_path {
            let group = match store.find_child_group(parent.as_deref(), name).await? {
                Some(existing) => existing,
                None => {
                    let created = store.create_group(name, parent.as_deref()).await?;
                    groups_created += 1;
                    created
                }
            };
            parent = Some(group.id);
        }

        let input = SessionInput {
            group_id: parent,
            ..input
        };
        store.create_session(&input).await?;
        imported += 1;
    }

    Ok(ImportReport {
        imported,
        groups_created,
        skipped,
    })
}

/// 按行切分 ini 内容。
///
/// 参数：content 为文件全文
/// 核心流程：跳过空行与注释行，遇到 [xxx] 开新段，其余按 key=value 归入当前段。
fn split_sections(content: &str) -> Vec<RawSection> {
    let mut sections: Vec<RawSection> = Vec::new();
    let mut current: Option<RawSection> = None;

    for raw_line in content.lines() {
        let line = raw_line.trim().trim_start_matches('\u{feff}').trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            if let Some(section) = current.take() {
                sections.push(section);
            }
            let key = line[1..line.len() - 1].trim().to_string();
            current = Some(RawSection {
                path: section_path(&key),
                key,
                entries: HashMap::new(),
            });
            continue;
        }
        if let Some(section) = current.as_mut()
            && let Some((key, value)) = line.split_once('=')
        {
            section
                .entries
                .insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    if let Some(section) = current {
        sections.push(section);
    }
    sections
}

/// 把段名拆成层级路径。
///
/// 参数：key 为段名，如 Bookmarks_1_2
/// 核心流程：按 '_' 切分并丢弃首段 Bookmarks，得到 ["1", "2"] 供嵌套推导复用。
fn section_path(key: &str) -> Vec<String> {
    key.split('_')
        .skip(1)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// 沿祖先路径推导会话所属的分组名链。
///
/// 参数：section_path 为会话段路径，group_names 为分组路径到名称的映射
/// 核心流程：从最深的祖先开始向上找最近的分组段，再沿其路径补齐完整名称链。
fn resolve_group_path(
    section_path: &[String],
    group_names: &HashMap<Vec<String>, String>,
) -> Vec<String> {
    for end in (0..section_path.len()).rev() {
        let candidate = &section_path[..end];
        if group_names.contains_key(candidate) {
            return (1..=candidate.len())
                .filter_map(|depth| group_names.get(&candidate[..depth]).cloned())
                .collect();
        }
    }
    Vec::new()
}

/// 取会话显示名。
///
/// 参数：section 为原始段，fallback 为兜底名
/// 核心流程：优先用 Name/SessionName 字段，其次用兜底名（主机或协议名）。
fn session_name(section: &RawSection, fallback: &str) -> String {
    for key in ["Name", "SessionName"] {
        if let Some(name) = section.entries.get(key) {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    fallback.to_string()
}
