//! Tauri command boundary for the desktop UI.
//!
//! Every value crossing this module is treated as untrusted. The trusted local
//! interface manages download tasks, settings, logs, and the Telegram Web child
//! view; the remote Telegram WebView may only start page-fetch download tasks
//! and push the bytes it fetched itself — it can never choose a filesystem path
//! or a download URL.

use crate::{
    app_state::AppState,
    downloader::PlanInfo,
    models::{AppStateDto, LogEntry, Settings, TaskRecord, TaskStateMatch},
    storage::{self, StorageLayout},
    task_store::TaskStore,
    webview_bridge::{self, TelegramWebviewBounds},
};
use anyhow::Result;
use serde::Serialize;
use std::{fmt::Display, fs, path::PathBuf};
use tauri::{
    AppHandle, Manager, State, Webview, Window, Wry,
    ipc::{InvokeBody, Request},
};

/// 单个 IPC 分块请求头中的任务 ID 与偏移。
const CHUNK_TASK_ID_HEADER: &str = "x-task-id";
const CHUNK_OFFSET_HEADER: &str = "x-offset";

const MAX_TASKS_PER_LIST: usize = 2_000;
const MAX_LOG_ENTRIES: usize = 500;
/// 注入脚本按文件名查询状态时允许的最大字符数(与下载文件名校验一致)。
const MAX_WEBVIEW_FILE_NAME_CHARS: usize = 512;
const MAX_TEMPLATE_LENGTH: usize = 256;
const ALLOWED_TASK_STATUSES: &[&str] = &[
    "queued",
    "downloading",
    "paused",
    "completed",
    "failed",
    "cancelled",
];
const ALLOWED_TEMPLATE_FIELDS: &[&str] = &[
    "chat",
    "year",
    "month",
    "date",
    "message_id",
    "name",
    "caption",
    "media_type",
];

struct RestartAfterStorageClose(AppHandle);

impl Drop for RestartAfterStorageClose {
    fn drop(&mut self) {
        let app = self.0.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            app.request_restart();
        });
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageChangeResult {
    pub data_root: String,
    pub download_root: String,
    pub restart_required: bool,
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_app_state(state: State<'_, AppState>) -> Result<AppStateDto, String> {
    let layout = state.shared.layout.read().await.clone();
    let mut settings = state.shared.settings.read().await.clone();
    settings.data_root = storage::display_path(&layout.root);
    let stats = state.shared.store.stats().await.map_err(command_error)?;
    let fallback = state
        .shared
        .app
        .path()
        .app_data_dir()
        .map_err(command_error)?;

    Ok(AppStateDto {
        app_version: state.shared.app.package_info().version.to_string(),
        settings,
        storage_options: storage::enumerate_storage(&fallback),
        stats,
    })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn save_settings(
    state: State<'_, AppState>,
    mut settings: Settings,
) -> Result<(), String> {
    let layout = state.shared.layout.read().await.clone();
    let mut current_settings = state.shared.settings.write().await;
    settings.data_root = storage::display_path(&layout.root);
    normalize_and_validate_settings(&mut settings)?;
    // 学习值是下载器的测量结果,不受设置表单回传影响(表单可能是几分钟前加载的),
    // 否则一次"保存设置"就会把刚学到的链路数据抹掉。
    settings.concurrency.learned_per_stream_bytes_per_second = current_settings
        .concurrency
        .learned_per_stream_bytes_per_second;
    storage::save_settings(&layout, &settings).map_err(command_error)?;
    *current_settings = settings;
    drop(current_settings);
    state.shared.log("info", "settings", "设置已保存").await;
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn list_tasks(
    state: State<'_, AppState>,
    status: Option<String>,
    limit: usize,
) -> Result<Vec<TaskRecord>, String> {
    if status
        .as_deref()
        .is_some_and(|value| !ALLOWED_TASK_STATUSES.contains(&value))
    {
        return Err("未知的下载任务状态".into());
    }
    let limit = limit.clamp(1, MAX_TASKS_PER_LIST);
    let mut tasks = state
        .shared
        .store
        .list(status.as_deref(), limit)
        .await
        .map_err(command_error)?;
    let layout = state.shared.layout.read().await.clone();
    crate::task_cover::attach_cover_paths(&layout.root, &mut tasks);
    Ok(tasks)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn task_action(
    state: State<'_, AppState>,
    task_id: String,
    action: String,
) -> Result<TaskRecord, String> {
    validate_task_id(&task_id)?;
    if !["pause", "resume", "cancel", "retry"].contains(&action.as_str()) {
        return Err("未知的下载任务操作".into());
    }
    state
        .downloads
        .action(&task_id, &action)
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn open_task_location(state: State<'_, AppState>, task_id: String) -> Result<(), String> {
    validate_task_id(&task_id)?;
    let task = state
        .shared
        .store
        .get(&task_id)
        .await
        .map_err(command_error)?
        .ok_or_else(|| "未找到下载任务".to_owned())?;
    let raw_path = task
        .output_path
        .as_deref()
        .ok_or_else(|| "任务尚无目标文件位置".to_owned())?;
    let target = PathBuf::from(raw_path);
    if !target.is_absolute() {
        return Err("任务目标路径无效，已拒绝打开".into());
    }
    let settings = state.shared.settings.read().await.clone();
    let download_root = PathBuf::from(settings.download_root)
        .canonicalize()
        .map_err(command_error)?;
    // 任务记录里的 output_path 可能来自旧版本(带 `\\?\` 前缀),而设置里的下载目录
    // 现在是干净形式;比较前统一解析为 canonical:文件已存在时解析文件本身,否则
    // 解析最近的已存在父目录(reveal 目标即该父目录)。
    let (resolved, outside_message) = if target.exists() {
        (
            target.canonicalize().map_err(command_error)?,
            "任务文件解析到下载目录之外，已拒绝打开",
        )
    } else {
        let mut parent = target
            .parent()
            .ok_or_else(|| "任务目标目录无效".to_owned())?
            .to_path_buf();
        while !parent.exists() {
            parent = parent
                .parent()
                .ok_or_else(|| "任务目标目录不存在".to_owned())?
                .to_path_buf();
        }
        (
            parent.canonicalize().map_err(command_error)?,
            "任务目录解析到下载目录之外，已拒绝打开",
        )
    };
    if !resolved.starts_with(&download_root) {
        return Err(outside_message.into());
    }
    tauri_plugin_opener::reveal_item_in_dir(resolved).map_err(command_error)
}

/// 删除任务记录(可选一并删除已下载文件);下载中的任务会先被停止。
#[tauri::command(rename_all = "camelCase")]
pub async fn delete_task(
    state: State<'_, AppState>,
    task_id: String,
    delete_file: bool,
) -> Result<(), String> {
    validate_task_id(&task_id)?;
    state
        .downloads
        .delete(&task_id, delete_file)
        .await
        .map_err(command_error)?;
    let layout = state.shared.layout.read().await.clone();
    let cover = layout
        .root
        .join("Cache")
        .join("video-covers")
        .join(format!("{task_id}.jpg"));
    if let Err(error) = fs::remove_file(cover)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        state
            .shared
            .log(
                "warn",
                "video-cover",
                format!("清理任务封面缓存失败：{error}"),
            )
            .await;
    }
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_logs(state: State<'_, AppState>, limit: usize) -> Result<Vec<LogEntry>, String> {
    let limit = limit.min(MAX_LOG_ENTRIES);
    let logs = state.shared.logs.lock().await;
    Ok(logs
        .iter()
        .skip(logs.len().saturating_sub(limit))
        .cloned()
        .collect())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn ensure_telegram_webview(
    window: Window<Wry>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Telegram WebView 只能由主窗口创建".into());
    }
    let _guard = state.shared.webview_init.lock().await;
    if let Some(_webview) = state
        .shared
        .app
        .get_webview(webview_bridge::TELEGRAM_WEBVIEW_LABEL)
    {
        return Ok(());
    }
    let profile = state.shared.layout.read().await.webview.clone();
    let webview = webview_bridge::create_telegram_webview(
        &window,
        TelegramWebviewBounds {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        },
        profile,
    )?;
    webview_bridge::set_telegram_webview_visible(&webview, false)?;
    state
        .shared
        .log(
            "info",
            "telegram-webview",
            "已在主窗口创建 Telegram WebView",
        )
        .await;
    Ok(())
}

/// 诊断:向 Telegram WebView 注入一次性探测脚本,回报注入脚本标记与页面状态。
///
/// 由本地主窗口(或托盘菜单)调用,而非远程页面;探测结果经 `webview_log`
/// 写入应用日志的 `inject` 目标,用于区分「脚本没跑」与「脚本跑了但检测失败」。
#[tauri::command(rename_all = "camelCase")]
pub async fn probe_telegram_inject(state: State<'_, AppState>) -> Result<(), String> {
    let webview = state
        .shared
        .app
        .get_webview(webview_bridge::TELEGRAM_WEBVIEW_LABEL)
        .ok_or_else(|| "Telegram WebView 尚未创建".to_owned())?;
    webview
        .eval(
            r#"
      (function () {
        try {
          var invoke = window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke;
          var marker = document.documentElement.getAttribute('data-tmd-inject') || 'none';
          var hasTauri = typeof window.__TAURI__ !== 'undefined';
          var viewerK = !!document.querySelector('.media-viewer-whole');
          var viewerZ = !!document.querySelector('#MediaViewer');
          var msg = 'probe: marker=' + marker + ' hasTauri=' + hasTauri + ' origin=' + location.origin + ' path=' + location.pathname + ' viewerK=' + viewerK + ' viewerZ=' + viewerZ;
          if (invoke) { invoke('webview_log', { message: msg }).catch(function () {}); }
        } catch (e) {
          try { window.__TAURI__.core.invoke('webview_log', { message: 'probe error: ' + (e && e.message) }).catch(function(){}); } catch (_e2) {}
        }
      })();
    "#,
        )
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub fn set_telegram_webview_visible(
    state: State<'_, AppState>,
    visible: bool,
) -> Result<(), String> {
    let webview = state
        .shared
        .app
        .get_webview(webview_bridge::TELEGRAM_WEBVIEW_LABEL)
        .ok_or_else(|| "Telegram WebView 尚未创建".to_owned())?;
    webview_bridge::set_telegram_webview_visible(&webview, visible)
}

#[tauri::command(rename_all = "camelCase")]
pub fn set_telegram_webview_bounds(
    window: Window<Wry>,
    state: State<'_, AppState>,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let bounds = TelegramWebviewBounds {
        x,
        y,
        width,
        height,
    };
    validate_bounds_in_main_window(&window, bounds)?;
    let webview = state
        .shared
        .app
        .get_webview(webview_bridge::TELEGRAM_WEBVIEW_LABEL)
        .ok_or_else(|| "Telegram WebView 尚未创建".to_owned())?;
    webview_bridge::update_telegram_webview_bounds(&webview, bounds)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn change_storage_root(
    state: State<'_, AppState>,
    path: String,
) -> Result<StorageChangeResult, String> {
    let requested = PathBuf::from(path.trim());
    if !requested.is_absolute() || requested.file_name().is_none() {
        return Err("数据目录必须是绝对路径，且不能是磁盘根目录".into());
    }
    let old_layout = state.shared.layout.read().await.clone();
    let old_root = old_layout.root.canonicalize().map_err(command_error)?;
    fs::create_dir_all(&requested).map_err(command_error)?;
    let new_root = requested.canonicalize().map_err(command_error)?;
    if new_root == old_root {
        let settings = state.shared.settings.read().await;
        return Ok(StorageChangeResult {
            data_root: storage::display_path(&old_root),
            download_root: settings.download_root.clone(),
            restart_required: false,
        });
    }
    if new_root.starts_with(&old_root) || old_root.starts_with(&new_root) {
        return Err("新旧数据目录不能相互包含".into());
    }
    ensure_no_active_or_queued_downloads(&state.shared.store).await?;
    if fs::read_dir(&new_root)
        .map_err(command_error)?
        .next()
        .is_some()
    {
        return Err("目标数据目录必须为空".into());
    }

    let mut settings_guard = state.shared.settings.write().await;
    ensure_no_active_or_queued_downloads(&state.shared.store).await?;

    let child_webview = state
        .shared
        .app
        .get_webview(webview_bridge::TELEGRAM_WEBVIEW_LABEL);
    if let Some(webview) = child_webview {
        webview
            .close()
            .map_err(|error| format!("关闭 Telegram WebView 配置目录失败，未迁移数据：{error}"))?;
    }
    state.shared.store.close().await.map_err(command_error)?;
    // The ORM pool is now closed so the SQLite main file and WAL can be copied
    // consistently. Restart after this command returns, on both success and
    // failure, to reopen the active (old or newly switched) data directory.
    let _restart_after_storage_close = RestartAfterStorageClose(state.shared.app.clone());

    let mut migrated_settings = settings_guard.clone();
    let previous_download_root = PathBuf::from(&migrated_settings.download_root);
    // 设置里的下载目录是干净形式,old_root 是 canonical(带 `\\?\` 前缀);统一解析
    // 后再比较/截取,解析失败(目录不存在)时退回词法比较。
    let previous_canonical = previous_download_root
        .canonicalize()
        .unwrap_or_else(|_| previous_download_root.clone());
    let mapped_download_root = if previous_canonical.starts_with(&old_root) {
        Some(
            new_root.join(
                previous_canonical
                    .strip_prefix(&old_root)
                    .map_err(command_error)?,
            ),
        )
    } else {
        None
    };
    let next_layout = StorageLayout::under(new_root.clone());
    let source = old_root.clone();
    let destination = new_root.clone();
    tokio::task::spawn_blocking(move || storage::copy_tree_verified(&source, &destination))
        .await
        .map_err(command_error)?
        .map_err(|error| {
            format!(
                "复制数据并校验失败；活动数据目录指针未切换。请检查目标磁盘空间和权限后重启应用：{error:#}"
            )
        })?;

    if let Some(download_root) = mapped_download_root {
        migrated_settings.download_root = storage::display_path(&download_root);
    }
    migrated_settings.data_root = storage::display_path(&new_root);
    if let Some(parent) = state.shared.root_pointer.parent() {
        fs::create_dir_all(parent).map_err(command_error)?;
    }
    storage::save_settings(&next_layout, &migrated_settings).map_err(command_error)?;
    crate::atomic_file::write(
        &state.shared.root_pointer,
        new_root.to_string_lossy().as_bytes(),
    )
    .map_err(command_error)?;
    *settings_guard = migrated_settings.clone();
    drop(settings_guard);

    state
        .shared
        .log(
            "info",
            "storage",
            "数据目录已复制并校验；重启后切换到新目录",
        )
        .await;
    Ok(StorageChangeResult {
        data_root: storage::display_path(&new_root),
        download_root: migrated_settings.download_root,
        restart_required: true,
    })
}

/// 远程命令的运行时二次校验:能力(capability)之外再确认调用方确实是 `telegram`
/// 子视图,且当前地址仍在 `https://web.telegram.org` 上;见 `webview_bridge`。
fn ensure_trusted_telegram_webview(webview: &Webview<Wry>) -> Result<(), String> {
    if webview_bridge::is_trusted_telegram_webview(webview) {
        Ok(())
    } else {
        Err("下载请求必须来自受信任的 Telegram WebView 页面".into())
    }
}

/// WebView 页面抓取下载的命令面。这些命令只授予 `telegram` 远程能力:
/// 页面自己发起 fetch,把分块字节经 IPC 交给 Rust 落盘;Rust 端不接收 URL,
/// 也从不读取页面凭据。
#[tauri::command(rename_all = "camelCase")]
pub async fn start_webview_download(
    webview: Webview<Wry>,
    state: State<'_, AppState>,
    file_name: String,
    file_type: String,
    source: String,
) -> Result<TaskRecord, String> {
    ensure_trusted_telegram_webview(&webview)?;
    state
        .downloads
        .create(&file_name, &file_type, &source)
        .await
        .map_err(command_error)
}

/// 页面探明文件大小后排定分块计划;幂等,可重复调用以续传。
/// `probe_rtt_ms` 是页面探测请求的首字节时间(≈RTT),用于 BDP 分块估算。
#[tauri::command(rename_all = "camelCase")]
pub async fn plan_chunks(
    webview: Webview<Wry>,
    state: State<'_, AppState>,
    task_id: String,
    total_bytes: u64,
    probe_rtt_ms: Option<u64>,
) -> Result<PlanInfo, String> {
    ensure_trusted_telegram_webview(&webview)?;
    validate_task_id(&task_id)?;
    state
        .downloads
        .plan(&task_id, total_bytes, probe_rtt_ms)
        .await
        .map_err(command_error)
}

/// 接收页面 fetch 到的分块(`taskId`/`offset` 走请求头,body 是分块字节)。
///
/// 首选通道是 Tauri 的 `ipc://localhost` 自定义协议,body 以 raw bytes 到达
/// (`InvokeBody::Raw`);若页面侧该通道被拦截,Tauri 会回退到 postMessage,
/// body 变成 JSON 数字数组,这里同样接受(仅作兼容,吞吐按前者设计)。
#[tauri::command]
pub async fn push_chunk(
    webview: Webview<Wry>,
    state: State<'_, AppState>,
    request: Request<'_>,
) -> Result<(), String> {
    ensure_trusted_telegram_webview(&webview)?;
    let task_id = required_header(&request, CHUNK_TASK_ID_HEADER)?;
    let offset = required_header(&request, CHUNK_OFFSET_HEADER)?
        .parse::<u64>()
        .map_err(|_| format!("{CHUNK_OFFSET_HEADER} 必须是非负整数"))?;
    validate_task_id(&task_id)?;
    let bytes = match request.body() {
        InvokeBody::Raw(bytes) => bytes.clone(),
        InvokeBody::Json(value) => decode_json_chunk_body(value)?,
    };
    state
        .downloads
        .push(&task_id, offset, bytes)
        .await
        .map_err(command_error)
}

/// 页面侧心跳:抓取在途但暂无完整分块时调用,避免看门狗把慢连接误判成页面消失。
#[tauri::command(rename_all = "camelCase")]
pub async fn webview_download_heartbeat(
    webview: Webview<Wry>,
    state: State<'_, AppState>,
    task_id: String,
) -> Result<(), String> {
    ensure_trusted_telegram_webview(&webview)?;
    validate_task_id(&task_id)?;
    state
        .downloads
        .heartbeat(&task_id)
        .await
        .map_err(command_error)
}

/// 全部分块接收完成:校验分块完整并原子提交。
#[tauri::command(rename_all = "camelCase")]
pub async fn finish_download(
    webview: Webview<Wry>,
    state: State<'_, AppState>,
    task_id: String,
) -> Result<TaskRecord, String> {
    ensure_trusted_telegram_webview(&webview)?;
    validate_task_id(&task_id)?;
    state
        .downloads
        .finish(&task_id)
        .await
        .map_err(command_error)
}

/// 页面侧失败(URL 过期、块级重试耗尽等):保留已校验分块并标记 `failed`。
#[tauri::command(rename_all = "camelCase")]
pub async fn fail_download(
    webview: Webview<Wry>,
    state: State<'_, AppState>,
    task_id: String,
    error: String,
    permanent: Option<bool>,
) -> Result<TaskRecord, String> {
    ensure_trusted_telegram_webview(&webview)?;
    validate_task_id(&task_id)?;
    state
        .downloads
        .fail(&task_id, &error, permanent.unwrap_or(false))
        .await
        .map_err(command_error)
}

/// 注入脚本按文件名查询任务状态,驱动查看器里的下载按钮。
///
/// `resumable` 为真表示已有任务记录了分块(queued/downloading/paused),
/// 页面重开媒体时应走 `plan_chunks` 只补缺块而不是重新开始。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebviewTaskState {
    pub state: String,
    pub task_id: Option<String>,
    pub progress: Option<f64>,
    pub file_size: Option<u64>,
    pub completed_at: Option<String>,
    pub resumable: bool,
}

/// 诊断通道:注入脚本把运行日志写入应用日志(client.jsonl 的 `inject` 目标)。
///
/// 只接受受信任 Telegram WebView 页面发来的消息;消息截断到 500 字符,
/// 避免页面故障时刷爆日志。
#[tauri::command(rename_all = "camelCase")]
pub async fn webview_log(
    webview: Webview<Wry>,
    state: State<'_, AppState>,
    message: String,
) -> Result<(), String> {
    if !webview_bridge::is_trusted_telegram_webview(&webview) {
        return Err("诊断日志必须来自受信任的 Telegram WebView 页面".into());
    }
    let message: String = message.chars().take(500).collect();
    state.shared.log("info", "inject", message).await;
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn webview_query_task_state(
    webview: Webview<Wry>,
    state: State<'_, AppState>,
    file_name: String,
) -> Result<WebviewTaskState, String> {
    ensure_trusted_telegram_webview(&webview)?;
    let file_name = file_name.trim();
    if file_name.is_empty()
        || file_name.chars().count() > MAX_WEBVIEW_FILE_NAME_CHARS
        || file_name.chars().any(char::is_control)
    {
        return Err("文件名无效".into());
    }
    let found: TaskStateMatch = state
        .shared
        .store
        .find_latest_by_file_name(file_name)
        .await
        .map_err(command_error)?;
    let resumable = matches!(found.state.as_str(), "queued" | "downloading" | "paused");
    Ok(WebviewTaskState {
        state: found.state,
        task_id: found.task_id,
        progress: found.progress,
        file_size: found.file_size,
        completed_at: found.completed_at,
        resumable,
    })
}

fn required_header(request: &Request<'_>, name: &str) -> Result<String, String> {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| format!("缺少 {name} 请求头"))
}

/// postMessage 回退通道把字节数组序列化成 JSON 数字数组,这里还原成字节。
fn decode_json_chunk_body(value: &serde_json::Value) -> Result<Vec<u8>, String> {
    let items = value
        .as_array()
        .ok_or_else(|| "push_chunk 需要二进制请求体".to_owned())?;
    let mut bytes = Vec::with_capacity(items.len());
    for item in items {
        let number = item
            .as_u64()
            .ok_or_else(|| "push_chunk 分块数据格式无效".to_owned())?;
        bytes.push(u8::try_from(number).map_err(|_| "push_chunk 分块数据格式无效".to_owned())?);
    }
    Ok(bytes)
}

async fn ensure_no_active_or_queued_downloads(store: &TaskStore) -> Result<(), String> {
    let stats = store.stats().await.map_err(command_error)?;
    if stats.active_downloads > 0 || stats.queued_downloads > 0 {
        return Err("请先暂停或完成活动/排队任务，再执行此操作".into());
    }
    Ok(())
}

fn validate_bounds_in_main_window(
    window: &Window<Wry>,
    bounds: TelegramWebviewBounds,
) -> Result<(), String> {
    let values = [bounds.x, bounds.y, bounds.width, bounds.height];
    if values.iter().any(|value| !value.is_finite())
        || bounds.x < 0.0
        || bounds.y < 0.0
        || bounds.width <= 0.0
        || bounds.height <= 0.0
    {
        return Err("Telegram WebView bounds 必须是窗口内的有限正值".into());
    }
    let physical = window.inner_size().map_err(command_error)?;
    let scale = window.scale_factor().map_err(command_error)?;
    let logical_width = f64::from(physical.width) / scale;
    let logical_height = f64::from(physical.height) / scale;
    if !scale.is_finite()
        || scale <= 0.0
        || !(bounds.x + bounds.width).is_finite()
        || !(bounds.y + bounds.height).is_finite()
        || bounds.x + bounds.width > logical_width
        || bounds.y + bounds.height > logical_height
    {
        return Err("Telegram WebView bounds 超出主窗口可见范围".into());
    }
    Ok(())
}

fn validate_task_id(task_id: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(task_id)
        .map(|_| ())
        .map_err(|_| "下载任务 ID 格式无效".to_owned())
}

fn normalize_and_validate_settings(settings: &mut Settings) -> Result<(), String> {
    if !["skip", "rename", "overwrite"].contains(&settings.duplicate_policy.as_str()) {
        return Err("重复文件策略必须是跳过、重命名或覆盖".into());
    }
    if !crate::models::valid_date_format(&settings.date_format) {
        return Err("日期格式无效；请使用有效的 chrono/strftime 格式（最多 128 字节）".into());
    }
    validate_template(&settings.path_template, true)?;
    validate_template(&settings.file_name_template, false)?;

    let concurrency = &settings.concurrency;
    if !(1..=64).contains(&concurrency.max_files)
        || !(1..=32).contains(&concurrency.per_file_chunks)
        || !(64..=4096).contains(&concurrency.chunk_size_kib)
        || !(5..=600).contains(&concurrency.request_timeout_seconds)
        || concurrency.retries > 20
    {
        return Err("并发、分块、超时或重试设置超出允许范围".into());
    }

    if settings.language.trim().is_empty()
        || settings.language.len() > 16
        || settings.language.chars().any(char::is_control)
    {
        return Err("界面语言标识无效".into());
    }

    let root = PathBuf::from(settings.download_root.trim());
    if !root.is_absolute() {
        return Err("下载目录必须是绝对路径".into());
    }
    fs::create_dir_all(&root).map_err(command_error)?;
    // 校验用 canonical 路径,但持久化的是去掉 Windows `\\?\` 前缀的干净形式。
    settings.download_root = storage::display_path(&root.canonicalize().map_err(command_error)?);
    Ok(())
}

fn validate_template(template: &str, allow_directories: bool) -> Result<(), String> {
    if template.is_empty()
        || template.len() > MAX_TEMPLATE_LENGTH
        || template.chars().any(char::is_control)
    {
        return Err("文件命名规则不能为空、不能过长或包含控制字符".into());
    }
    if !allow_directories
        && (template.contains('/') || template.contains('\\') || template.contains(':'))
    {
        return Err("文件名模板不能包含目录分隔符或盘符".into());
    }
    if allow_directories
        && (template.starts_with('/')
            || template.starts_with('\\')
            || template.contains(':')
            || template.split(['/', '\\']).any(|part| part == ".."))
    {
        return Err("文件夹模板必须是相对路径，且不能包含上级目录".into());
    }
    let mut chars = template.chars();
    while let Some(character) = chars.next() {
        match character {
            '{' => {
                let mut field = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some('{') | None => {
                            return Err("文件命名规则包含无效或未闭合的模板字段".into());
                        }
                        Some(character) => field.push(character),
                    }
                }
                if !ALLOWED_TEMPLATE_FIELDS.contains(&field.as_str()) {
                    return Err(format!("文件命名规则包含不支持的字段：{{{field}}}"));
                }
            }
            '}' => return Err("文件命名规则包含未匹配的模板字段".into()),
            _ => {}
        }
    }
    Ok(())
}

fn command_error(error: impl Display) -> String {
    let mut message = error.to_string();
    if message.len() > 1_000 {
        message.truncate(1_000);
        message.push_str("…");
    }
    message
}
