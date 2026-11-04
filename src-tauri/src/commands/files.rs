use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use rustmobax_core::{CoreError, ErrorPayload};

/// 读取一个文本文件。
///
/// 参数：path 为绝对路径
/// 核心流程：按 UTF-8 读取全文，失败转成前端可读的错误负载。
#[tauri::command]
pub fn read_text_file(path: String) -> Result<String, ErrorPayload> {
    std::fs::read_to_string(&path)
        .map_err(|err| ErrorPayload::from(rustmobax_core::CoreError::Io(err)))
}

/// 把文本写入文件。
///
/// 参数：path 为绝对路径，content 为文件内容
/// 核心流程：覆盖写入，父目录不存在时直接报错而不是静默创建。
#[tauri::command]
pub fn write_text_file(path: String, content: String) -> Result<(), ErrorPayload> {
    std::fs::write(&path, content)
        .map_err(|err| ErrorPayload::from(rustmobax_core::CoreError::Io(err)))
}

/// 二进制传输的单文件上限。
///
/// 参数：无
/// 核心流程：base64 会按 4/3 放大，32 MiB 原文经 IPC 后约 43 MiB 字符串，
/// 再大就要改成分片传输，本轮先在这里拦截。
const MAX_BINARY_BYTES: u64 = 32 * 1024 * 1024;

/// 读取二进制文件并返回 base64。
///
/// 参数：path 为绝对路径
/// 核心流程：先看文件大小防一次性读爆内存 -> 读字节 -> base64 编码交给前端，
/// 不要用字符串通道，避免 UTF-8 之外的字节被破坏。
#[tauri::command]
pub fn read_binary_file(path: String) -> Result<String, ErrorPayload> {
    let meta = std::fs::metadata(&path)
        .map_err(|err| ErrorPayload::from(CoreError::Io(err)))?;
    if meta.len() > MAX_BINARY_BYTES {
        return Err(ErrorPayload::from(CoreError::Validation(
            "文件超过 32 MiB 上限，暂不支持二进制大文件".into(),
        )));
    }
    let bytes = std::fs::read(&path).map_err(|err| ErrorPayload::from(CoreError::Io(err)))?;
    Ok(BASE64.encode(bytes))
}

/// 把 base64 内容写成二进制文件。
///
/// 参数：path 为绝对路径，data 为 base64 文本
/// 核心流程：解码失败直接拒绝（不能把坏数据落盘）-> 父目录不存在时创建 ->
/// 覆盖写入。
#[tauri::command]
pub fn write_binary_file(path: String, data: String) -> Result<(), ErrorPayload> {
    let bytes = BASE64.decode(data.as_bytes()).map_err(|_| {
        ErrorPayload::from(CoreError::Validation("文件内容不是合法的 base64".into()))
    })?;
    if let Some(parent) = std::path::Path::new(&path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|err| ErrorPayload::from(CoreError::Io(err)))?;
        }
    }
    std::fs::write(&path, bytes).map_err(|err| ErrorPayload::from(CoreError::Io(err)))
}

/// 本地目录中的一项。
///
/// 无参数
/// 核心流程：只回前端做递归上传需要的字段，不读文件内容。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
}

/// 列出本地目录。
///
/// 参数：path 为绝对路径
/// 核心流程：目录在前、再按名称排序；跳过符号链接指向的目录以免递归成环，
/// 单个条目元信息读失败就跳过，不让一个坏文件挡住整次上传。
#[tauri::command]
pub fn list_local_dir(path: String) -> Result<Vec<LocalEntry>, ErrorPayload> {
    let mut entries = Vec::new();
    let dir = std::fs::read_dir(&path).map_err(|err| ErrorPayload::from(CoreError::Io(err)))?;
    for item in dir.flatten() {
        let entry_path = item.path();
        let Ok(meta) = item.metadata() else { continue };
        let name = item.file_name().to_string_lossy().to_string();
        entries.push(LocalEntry {
            name,
            path: entry_path.to_string_lossy().to_string(),
            is_dir: meta.is_dir(),
            size: meta.len(),
        });
    }
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(entries)
}
