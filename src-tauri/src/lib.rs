mod commands;
mod state;

use tauri::Manager;

use commands::session::build_state;

/// 返回当前运行时的应用元信息。
///
/// 无参数
/// 核心流程：读取编译期常量与平台常量，供设置页与诊断使用。
#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        name: env!("CARGO_PKG_NAME").to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
    }
}

/// 应用元信息。
///
/// 无参数
/// 核心流程：承载包信息与平台信息，序列化后经 IPC 返回前端。
#[derive(serde::Serialize)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub os: String,
    pub arch: String,
}

/// 需要跨启动记忆的窗口状态：位置、尺寸、是否最大化。
///
/// 无参数
/// 核心流程：把三个 flag 取并集交给窗口状态插件；不含 VISIBLE / DECORATIONS /
/// FULLSCREEN，避免恢复无关状态时打扰窗口。
fn window_state_flags() -> tauri_plugin_window_state::StateFlags {
    use tauri_plugin_window_state::StateFlags;
    StateFlags::POSITION | StateFlags::SIZE | StateFlags::MAXIMIZED
}

/// 把窗口尺寸夹紧到配置里的最小尺寸之上。
///
/// 参数：`window` 目标窗口，`size` 变化后的内尺寸。
/// 核心流程：按窗口 label 找回 tauri.conf.json 的定义 → 读 minWidth/minHeight →
/// 换算成逻辑尺寸与当前尺寸比对 → 小于下限时按较大者重新 set_size。
///
/// 插件按保存的绝对尺寸恢复，自身不夹紧；Linux 下 tao 的 set_inner_size 也只
/// 转发请求，所以最小尺寸只能由这里兜底。
fn clamp_to_min_size<R: tauri::Runtime>(window: &tauri::Window<R>, size: &tauri::PhysicalSize<u32>) {
    let config = window.app_handle().config();
    let Some(target) = config
        .app
        .windows
        .iter()
        .find(|def| def.label == window.label())
    else {
        return;
    };
    let (Some(min_width), Some(min_height)) = (target.min_width, target.min_height) else {
        return;
    };

    let scale_factor = window.scale_factor().unwrap_or(1.0).max(0.5);
    let current = size.to_logical::<f64>(scale_factor);
    if current.width >= min_width && current.height >= min_height {
        return;
    }

    let clamped = tauri::LogicalSize::new(
        current.width.max(min_width),
        current.height.max(min_height),
    );
    let _ = window.set_size(clamped);
}

/// 启动应用主循环。
///
/// 无参数
/// 核心流程：注册文件对话框插件 → 注册窗口位置尺寸记忆插件 →
/// 兜底夹紧最小尺寸 → 打开会话库并注册为共享状态 → 注册全部 IPC 命令 →
/// 进入事件循环。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // 保存的位置不与任何显示器相交时（例如外接屏被拔掉），插件会跳过
        // set_position，窗口留在 tauri.conf.json 的默认位置，不会跑到屏幕外。
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(window_state_flags())
                .build(),
        )
        // 恢复出来的尺寸可能比 minWidth/minHeight 还小，这里兜底夹紧一次。
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Resized(size) = event {
                clamp_to_min_size(window, size);
            }
        })
        .setup(|app| {
            // 会话库落在应用数据目录 -> 关闭应用后会话仍在
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|err| format!("无法定位应用数据目录：{err}"))?;
            let db_path = data_dir.join("sessions.db");
            let state = tauri::async_runtime::block_on(build_state(&db_path))?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            commands::session::list_groups,
            commands::session::create_group,
            commands::session::rename_group,
            commands::session::delete_group,
            commands::session::move_group,
            commands::session::list_sessions,
            commands::session::create_session,
            commands::session::update_session,
            commands::session::delete_session,
            commands::session::move_session,
            commands::session::copy_session,
            commands::session::preview_import,
            commands::session::apply_import_preview,
            commands::session::export_sessions,
            commands::settings::settings_get_all,
            commands::settings::settings_set,
            commands::theme::theme_list_custom,
            commands::theme::theme_save_custom,
            commands::theme::theme_delete_custom,
            commands::files::read_text_file,
            commands::files::write_text_file,
            commands::files::read_binary_file,
            commands::files::write_binary_file,
            commands::files::list_local_dir,
            commands::fonts::system_fonts,
            commands::metrics::system_metrics,
            commands::metrics::remote_metrics,
            commands::metrics::remote_disk,
            commands::sftp::sftp_connect,
            commands::sftp::sftp_list,
            commands::sftp::sftp_read_text,
            commands::sftp::sftp_write_text,
            commands::sftp::sftp_read_file_base64,
            commands::sftp::sftp_write_file_base64,
            commands::sftp::sftp_upload_local_file,
            commands::sftp::sftp_download_remote_file,
            commands::sftp::sftp_cancel_transfer,
            commands::sftp::sftp_mkdir,
            commands::sftp::sftp_remove,
            commands::sftp::sftp_rename,
            commands::sftp::sftp_close,
            commands::tunnel::tunnel_config_list,
            commands::tunnel::tunnel_config_create,
            commands::tunnel::tunnel_config_update,
            commands::tunnel::tunnel_config_delete,
            commands::tunnel::tunnel_start,
            commands::tunnel::tunnel_stop,
            commands::tunnel::tunnel_list,
            commands::terminal::start_local_terminal,
            commands::terminal::start_ssh_terminal,
            commands::terminal::start_telnet_terminal,
            commands::terminal::serial_list_ports,
            commands::terminal::start_serial_terminal,
            commands::terminal::start_wsl_terminal,
            commands::terminal::wsl_list_distros,
            commands::terminal::remote_desktop_clients,
            commands::terminal::launch_remote_desktop,
            commands::terminal::trust_host_key,
            commands::terminal::forget_host_key,
            commands::terminal::list_known_hosts,
            commands::terminal::write_local_terminal,
            commands::terminal::resize_local_terminal,
            commands::terminal::close_local_terminal,
            commands::terminal::default_shell_name,
            commands::terminal::terminal_cwd,
            commands::vault::vault_status,
            commands::vault::vault_setup,
            commands::vault::vault_unlock,
            commands::vault::vault_lock,
            commands::vault::credential_save,
            commands::vault::credential_reveal,
            commands::vault::credential_list,
            commands::vault::credential_delete,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
