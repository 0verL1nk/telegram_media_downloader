mod app_state;
mod atomic_file;
mod chunk_writer;
mod cloud_upload;
mod commands;
mod credentials;
mod db_migration;
mod downloader;
mod entities;
pub(crate) mod filter;
mod legacy_config;
mod log_store;
mod models;
mod secure_session;
mod storage;
mod task_store;
mod telegram;
mod transfers;
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
        .setup(|app| {
            let handle = app.handle().clone();
            let state = tauri::async_runtime::block_on(AppState::initialize(handle.clone()))
                .map_err(|error| std::io::Error::other(format!("初始化客户端失败：{error:#}")))?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_app_state,
            commands::save_settings,
            commands::import_legacy_config,
            commands::login_request_code,
            commands::login_submit_code,
            commands::login_submit_password,
            commands::logout_session,
            commands::list_chats,
            commands::get_chat_messages,
            commands::create_download_task,
            commands::create_chat_download,
            commands::list_tasks,
            commands::task_action,
            commands::open_task_location,
            commands::get_logs,
            commands::list_cloud_uploads,
            commands::queue_cloud_upload,
            commands::cloud_upload_action,
            commands::upload_completed_download,
            commands::forward_telegram_message,
            commands::list_telegram_transfers,
            commands::telegram_transfer_action,
            commands::ensure_telegram_webview,
            commands::set_telegram_webview_visible,
            commands::set_telegram_webview_bounds,
            commands::change_storage_root,
            commands::submit_download_from_webview,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
