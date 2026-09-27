mod adaptive;
mod app_state;
mod atomic_file;
mod chunk_writer;
mod commands;
mod db_migration;
mod downloader;
mod entities;
mod log_store;
mod models;
mod storage;
mod task_cover;
mod task_maintenance;
mod task_store;
mod tray;
mod video_processing;
mod webview_bridge;

use app_state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .try_init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let state = tauri::async_runtime::block_on(AppState::initialize(handle.clone()))
                .map_err(|error| std::io::Error::other(format!("初始化客户端失败：{error:#}")))?;
            app.manage(state);
            tray::build_tray(app.handle())?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭主窗口只隐藏到托盘，后台下载继续；真正的退出走托盘菜单「退出」。
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && window.label() == tray::MAIN_WINDOW_LABEL
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_app_state,
            commands::save_settings,
            commands::start_webview_download,
            commands::plan_chunks,
            commands::push_chunk,
            commands::webview_download_heartbeat,
            commands::finish_download,
            commands::fail_download,
            commands::webview_query_task_state,
            commands::webview_log,
            commands::list_tasks,
            task_cover::probe_task_video,
            task_cover::extract_video_frame,
            task_cover::get_task_video_thumbnail,
            task_cover::set_task_video_cover,
            commands::task_action,
            commands::process_video,
            commands::delete_task,
            commands::open_task_location,
            commands::get_logs,
            commands::ensure_telegram_webview,
            commands::probe_telegram_inject,
            commands::set_telegram_webview_visible,
            commands::set_telegram_webview_bounds,
            commands::change_storage_root,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
