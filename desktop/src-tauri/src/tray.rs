//! 系统托盘：显示主窗口、打开下载目录、退出。
//!
//! 关闭主窗口时只隐藏到托盘（见 `lib.rs` 的 `on_window_event`），后台下载继续
//! 运行；真正的退出只通过托盘菜单「退出」触发。

use crate::app_state::AppState;
use std::path::PathBuf;
use tauri::{
    AppHandle, Manager,
    menu::{Menu, MenuEvent, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

/// 主窗口标签，与 `tauri.conf.json` 中 `app.windows[0].label` 保持一致。
pub const MAIN_WINDOW_LABEL: &str = "main";

/// 托盘图标 ID 与菜单项 ID。菜单事件是全局监听，ID 必须在整个应用内唯一。
const TRAY_ICON_ID: &str = "main-tray";
const MENU_SHOW_MAIN: &str = "tray-show-main";
const MENU_OPEN_DOWNLOADS: &str = "tray-open-downloads";
const MENU_QUIT: &str = "tray-quit";

const TRAY_TOOLTIP: &str = "Telegram Media Downloader";

/// 构建系统托盘图标：左键显示主窗口，右键弹出中文菜单。
///
/// 图标复用打包配置里的默认窗口图标（`bundle.icon` 由 tauri-build 在编译期嵌入）。
pub fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show_main = MenuItem::with_id(app, MENU_SHOW_MAIN, "显示主窗口", true, None::<&str>)?;
    let open_downloads =
        MenuItem::with_id(app, MENU_OPEN_DOWNLOADS, "打开下载目录", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_main, &open_downloads, &quit])?;

    let builder = TrayIconBuilder::with_id(TRAY_ICON_ID)
        .tooltip(TRAY_TOOLTIP)
        .menu(&menu)
        // 左键留给「显示主窗口」，菜单只在右键弹出。
        .show_menu_on_left_click(false)
        .on_menu_event(handle_menu_event)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });

    let builder = match app.default_window_icon() {
        Some(icon) => builder.icon(icon.clone()),
        None => {
            tracing::warn!(target: "desktop::tray", "未找到默认窗口图标，托盘图标可能不可见");
            builder
        }
    };

    // 托盘图标在构建时会注册到应用资源表，由 Tauri 持有引用，无需保存返回值。
    builder.build(app)?;
    Ok(())
}

/// 处理托盘菜单事件。菜单事件是全局的，这里按 ID 过滤自己注册的菜单项。
fn handle_menu_event(app: &AppHandle, event: MenuEvent) {
    let id: &str = event.id().as_ref();
    match id {
        MENU_SHOW_MAIN => show_main_window(app),
        MENU_OPEN_DOWNLOADS => open_downloads_dir(app),
        MENU_QUIT => app.exit(0),
        _ => {}
    }
}

/// 显示、还原并聚焦主窗口；托盘左键与「显示主窗口」共用。
fn show_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        tracing::error!(target: "desktop::tray", "未找到主窗口，无法显示");
        return;
    };
    if let Err(error) = window.unminimize() {
        tracing::warn!(target: "desktop::tray", "取消最小化主窗口失败：{error}");
    }
    if let Err(error) = window.show() {
        tracing::error!(target: "desktop::tray", "显示主窗口失败：{error}");
        return;
    }
    if let Err(error) = window.set_focus() {
        tracing::warn!(target: "desktop::tray", "聚焦主窗口失败：{error}");
    }
}

/// 在系统文件管理器中打开配置的下载目录。
///
/// 菜单事件回调是同步的，而设置读取是异步的（`AppState.shared.settings` 是
/// 异步 `RwLock`），因此把读取与打开都放到异步运行时里执行。打开方式与
/// `commands::open_task_location` 相同，复用 `tauri_plugin_opener`。
fn open_downloads_dir(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let shared = app.state::<AppState>().shared.clone();
        let configured = shared.settings.read().await.download_root.trim().to_owned();
        // 设置为空时回退到存储布局的 Downloads 目录（初始化时通常已写入，
        // 这里是防御性兜底）；两种情况都不弹错误对话框。
        let directory = if configured.is_empty() {
            shared.layout.read().await.downloads.clone()
        } else {
            PathBuf::from(configured)
        };

        if !directory.is_dir() {
            // 目录可能被外部删除：先尝试重建，失败时只写日志。
            if let Err(error) = tokio::fs::create_dir_all(&directory).await {
                shared
                    .log(
                        "warn",
                        "tray",
                        format!("下载目录不可用，无法打开 {}：{error}", directory.display()),
                    )
                    .await;
                return;
            }
        }

        if let Err(error) = tauri_plugin_opener::open_path(&directory, None::<&str>) {
            shared
                .log(
                    "error",
                    "tray",
                    format!("打开下载目录失败 {}：{error}", directory.display()),
                )
                .await;
            return;
        }
        shared
            .log(
                "info",
                "tray",
                format!("已打开下载目录：{}", directory.display()),
            )
            .await;
    });
}
