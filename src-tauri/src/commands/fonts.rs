use std::sync::OnceLock;

use rustmobax_core::FontFamily;

/// 枚举本机字体族供设置页搜索。
///
/// 无参数
/// 核心流程：扫系统字体目录要读盘且耗时随字体数量增长，但结果在进程生命周期里
/// 不会变，所以用 OnceLock 缓存第一次的扫描结果：设置页反复打开也只扫一次。
/// 首次调用丢进阻塞线程池，避免占住异步运行时；缓存空表时也照旧返回空表，
/// 前端会退化成「手输字体名」模式，这不是错误。
#[tauri::command]
pub async fn system_fonts() -> Vec<FontFamily> {
    static CACHE: OnceLock<Vec<FontFamily>> = OnceLock::new();
    if let Some(fonts) = CACHE.get() {
        return fonts.clone();
    }
    // 扫描失败（线程池不可用）按「没读到字体」处理，不让设置页报错
    let scanned = tauri::async_runtime::spawn_blocking(rustmobax_core::system_fonts)
        .await
        .unwrap_or_default();
    CACHE.get_or_init(|| scanned).clone()
}
