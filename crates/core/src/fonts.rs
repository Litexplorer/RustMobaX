//! 系统字体枚举：只把「族名 + 是否等宽」交给前端，字体文件本身不碰。
//!
//! 本模块刻意不依赖 Tauri：桌面壳只做一次缓存转发，
//! 因此字体枚举逻辑可以在无 GUI 环境里直接测。

use std::collections::BTreeMap;

use serde::Serialize;

/// 一个可选的字体族。
///
/// name 是字族名（如 "JetBrains Mono"），前端据此拼 CSS 字体栈；
/// monospaced 来自字体自身的等宽标记，供界面把等宽字体排在前面。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FontFamily {
    pub name: String,
    pub monospaced: bool,
}

/// 枚举本机安装的字体族。
///
/// 无参数
/// 核心流程：先让 fontdb 扫系统字体目录，再把结果压成「族名 -> 是否等宽」：
/// 同一族名的多个字重/字形合并成一条，其中任意一个是等宽就算等宽；
/// BTreeMap 的键序天然是族名升序，所以输出顺序稳定且可复现。
/// 读不到任何字体（无 fontconfig、容器里没装字体）时返回空 Vec：
/// 前端仍然允许手输族名，不该在这里报错中断设置页。
pub fn system_fonts() -> Vec<FontFamily> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    collect_families(&db)
}

/// 从已加载的字体库里提取字体族列表。
///
/// 参数：db 为已经 load 过的字体库
/// 核心流程：与 system_fonts 相同的去重/排序逻辑，单独拆出来是为了让单测
/// 能在空库和伪造库上验证，不必依赖本机装了哪些字体。
fn collect_families(db: &fontdb::Database) -> Vec<FontFamily> {
    let mut families: BTreeMap<String, bool> = BTreeMap::new();
    for face in db.faces() {
        for (family, _language) in &face.families {
            let name = family.trim();
            // 有些字体把空族名写进表里，这种条目对用户没有意义
            if name.is_empty() {
                continue;
            }
            let entry = families.entry(name.to_string()).or_insert(false);
            *entry = *entry || face.monospaced;
        }
    }
    families
        .into_iter()
        .map(|(name, monospaced)| FontFamily { name, monospaced })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_fonts_are_non_empty_unique_and_sorted() {
        // 无字体环境下允许为空，但只要有就必须满足契约
        let fonts = system_fonts();
        for font in &fonts {
            assert!(!font.name.is_empty(), "族名不能是空串");
            assert_eq!(font.name.trim(), font.name, "族名两侧不应有空白");
        }
        let mut names: Vec<&str> = fonts.iter().map(|f| f.name.as_str()).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "同一族名只能出现一次");
        let mut ordered: Vec<&str> = fonts.iter().map(|f| f.name.as_str()).collect();
        ordered.sort_unstable();
        let actual: Vec<&str> = fonts.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(actual, ordered, "输出必须按族名升序，顺序才稳定");
    }

    #[test]
    fn empty_database_yields_empty_list() {
        // 扫不到字体时不能 panic，返回空列表把兜底交给前端
        let db = fontdb::Database::new();
        assert!(collect_families(&db).is_empty());
    }
}
