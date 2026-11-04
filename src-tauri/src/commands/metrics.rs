use rustmobax_core::{
    disk_usage, sample_remote, DiskUsage, ErrorPayload, RemoteMetrics, SystemMetrics,
};
use tauri::State;

use crate::state::AppState;

/// 采一次本机系统指标。
///
/// 参数：state 为应用状态
/// 核心流程：加锁取共享采样器 -> 用与上次采样的差值算 CPU 与网速 ->
/// 立即释放锁返回；前端按固定间隔轮询即可得到实时曲线。
#[tauri::command]
pub fn system_metrics(state: State<'_, AppState>) -> Result<SystemMetrics, ErrorPayload> {
    let mut sampler = state
        .metrics
        .lock()
        .map_err(|_| ErrorPayload::from(rustmobax_core::CoreError::Validation("指标采样器不可用".into())))?;
    Ok(sampler.sample())
}

/// 采一次远端主机的系统指标。
///
/// 参数：terminal_id 为 SSH 终端的标识（本地终端会返回校验错误）
/// 核心流程：从终端注册表取会话 -> 借出底层 SSH 连接 -> 在阻塞线程里
/// 执行一次远端脚本采样；采不到就报错，绝不用本机数据顶替。
#[tauri::command]
pub async fn remote_metrics(
    state: State<'_, AppState>,
    terminal_id: String,
) -> Result<RemoteMetrics, ErrorPayload> {
    let terminal = state.terminals.get(&terminal_id).ok_or_else(|| {
        ErrorPayload::from(rustmobax_core::CoreError::NotFound(format!(
            "终端 {terminal_id}"
        )))
    })?;

    // 本地 PTY 没有 SSH 连接 -> 这里就是「本机 / 远端」两种口径的分界
    let connection = terminal.connection().cloned().ok_or_else(|| {
        ErrorPayload::from(rustmobax_core::CoreError::Validation(
            "本地终端没有远端指标".into(),
        ))
    })?;
    let host = connection.host().to_string();

    // 采样是同步等待远端回包 -> 放阻塞线程池，避免占住 async 工作线程
    tauri::async_runtime::spawn_blocking(move || sample_remote(&connection, &host))
        .await
        .map_err(|err| {
            ErrorPayload::from(rustmobax_core::CoreError::Ssh(format!("任务异常：{err}")))
        })?
        .map_err(ErrorPayload::from)
}

/// 查询远端某个路径所在分区的磁盘占用。
///
/// 参数：terminal_id 为 SSH 终端的标识（不是 SFTP 会话标识），path 为远端路径
/// 核心流程：从终端注册表取会话 -> 借出底层 SSH 连接 -> 在阻塞线程里跑一次 df 并解析；
/// 本地 PTY 没有 SSH 连接，直接返回校验错误，绝不拿本机磁盘数字冒充远端。
#[tauri::command]
pub async fn remote_disk(
    state: State<'_, AppState>,
    terminal_id: String,
    path: String,
) -> Result<DiskUsage, ErrorPayload> {
    let terminal = state.terminals.get(&terminal_id).ok_or_else(|| {
        ErrorPayload::from(rustmobax_core::CoreError::NotFound(format!(
            "终端 {terminal_id}"
        )))
    })?;

    // 本地 PTY 没有 SSH 连接 -> 与远端指标同一条分界线
    let connection = terminal
        .connection()
        .cloned()
        .ok_or_else(|| {
            ErrorPayload::from(rustmobax_core::CoreError::Validation(
                "本地终端没有远端磁盘信息".into(),
            ))
        })?;

    // 查磁盘要等远端回包 -> 同样放阻塞线程池
    tauri::async_runtime::spawn_blocking(move || disk_usage(&connection, &path))
        .await
        .map_err(|err| {
            ErrorPayload::from(rustmobax_core::CoreError::Ssh(format!("任务异常：{err}")))
        })?
        .map_err(ErrorPayload::from)
}
