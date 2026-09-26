//! Tauri command boundary for the desktop UI.
//!
//! Every value crossing this module is treated as untrusted. Telegram
//! messages are re-read through the MTProto adapter before a task is created;
//! the remote Telegram WebView may only start page-fetch download tasks and
//! push the bytes it fetched itself — it can never choose a filesystem path or
//! a download URL.

use crate::{
    app_state::AppState,
    credentials,
    downloader::PlanInfo,
    legacy_config::{self, MigrationReport},
    models::{
        AppStateDto, ChatInfo, LogEntry, MessageInfo, SessionSummary, Settings, TaskRecord,
        TaskStateMatch, TelegramTransferRecord, UploadTaskRecord,
    },
    secure_session::EncryptedSession,
    storage::{self, StorageLayout},
    task_store::TaskStore,
    telegram::TelegramAdapter,
    webview_bridge::{self, TelegramWebviewBounds},
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Display,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::{
    AppHandle, Manager, State, Webview, Window, Wry,
    ipc::{InvokeBody, Request},
};

/// 单个 IPC 分块请求头中的任务 ID 与偏移。
const CHUNK_TASK_ID_HEADER: &str = "x-task-id";
const CHUNK_OFFSET_HEADER: &str = "x-offset";

const MAX_PAGE_SIZE: usize = 100;
const MAX_TASKS_PER_LIST: usize = 2_000;
const MAX_LOG_ENTRIES: usize = 500;
/// 注入脚本按文件名查询状态时允许的最大字符数(与下载文件名校验一致)。
const MAX_WEBVIEW_FILE_NAME_CHARS: usize = 512;
const MAX_LEGACY_CONFIG_BYTES: u64 = 1024 * 1024;
const CHAT_ID_MAX_CHARS: usize = 21;
const MAX_TEMPLATE_LENGTH: usize = 256;
const ALLOWED_MEDIA_TYPES: &[&str] = &[
    "photo",
    "video",
    "document",
    "audio",
    "voice",
    "video_note",
    "animation",
    "sticker",
];
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
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_offset: Option<i64>,
    pub has_more: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ChatIdInput {
    String(String),
    Number(i64),
}

impl ChatIdInput {
    fn into_validated(self) -> Result<String, String> {
        let value = match self {
            Self::String(value) => value,
            Self::Number(value) => value.to_string(),
        };
        validate_chat_id(&value)?;
        Ok(value)
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
    settings.data_root = layout.root.to_string_lossy().into_owned();
    settings.api_hash.clear();
    settings.proxy.password.clear();
    settings.api_hash_configured = credentials::read("telegram-api-hash")
        .map_err(command_error)?
        .is_some_and(|secret| !secret.trim().is_empty());
    settings.proxy.password_configured = credentials::read("telegram-proxy-password")
        .map_err(command_error)?
        .is_some_and(|secret| !secret.trim().is_empty());

    let session = current_session_summary(&state, &layout).await;
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
        session,
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
    let mut service_slot = state.shared.telegram.write().await;
    let previous = current_settings.clone();
    let old_api_hash = credentials::read("telegram-api-hash").map_err(command_error)?;
    let old_proxy_password = credentials::read("telegram-proxy-password").map_err(command_error)?;

    let submitted_api_hash = settings.api_hash.trim().to_owned();
    let next_api_hash = if submitted_api_hash.is_empty() {
        old_api_hash.clone()
    } else {
        validate_api_hash(&submitted_api_hash)?;
        Some(submitted_api_hash.clone())
    };
    let submitted_proxy_password = settings.proxy.password.clone();
    let next_proxy_password = if submitted_proxy_password.trim().is_empty() {
        old_proxy_password.clone()
    } else {
        Some(submitted_proxy_password.clone())
    };

    settings.data_root = layout.root.to_string_lossy().into_owned();
    settings.api_hash.clear();
    settings.proxy.password = next_proxy_password.clone().unwrap_or_default();
    settings.api_hash_configured = next_api_hash.is_some();
    settings.proxy.password_configured = next_proxy_password.is_some();
    normalize_and_validate_settings(&mut settings)?;

    let old_proxy_password_value = old_proxy_password.clone().unwrap_or_default();
    let new_proxy_password_value = next_proxy_password.clone().unwrap_or_default();
    let connection_changed = previous.api_id.trim() != settings.api_id.trim()
        || old_api_hash.as_deref().unwrap_or_default()
            != next_api_hash.as_deref().unwrap_or_default()
        || previous.proxy.enabled != settings.proxy.enabled
        || previous.proxy.scheme != settings.proxy.scheme
        || previous.proxy.host != settings.proxy.host
        || previous.proxy.port != settings.proxy.port
        || previous.proxy.username != settings.proxy.username
        || old_proxy_password_value != new_proxy_password_value;

    if connection_changed {
        ensure_no_active_or_queued_downloads(&state.shared.store).await?;
        if service_slot
            .as_ref()
            .is_some_and(|service| Arc::strong_count(service) > 1)
        {
            return Err("Telegram 连接仍有操作正在进行；请稍后再修改 API 或代理设置".into());
        }
        if telegram_session_is_authorized_in_slot(&service_slot, &layout).await? {
            return Err("请先注销 Telegram MTProto 会话，再修改 API 或代理连接设置".into());
        }
        if let Some(service) = service_slot.as_ref() {
            service.shutdown().await.map_err(command_error)?;
        }
        service_slot.take();
    }

    let mut wrote_api_hash = false;
    let mut wrote_proxy_password = false;
    if !submitted_api_hash.is_empty() {
        credentials::write("telegram-api-hash", &submitted_api_hash).map_err(command_error)?;
        wrote_api_hash = true;
    }
    if !submitted_proxy_password.trim().is_empty() {
        if settings.proxy.username.trim().is_empty() {
            if wrote_api_hash {
                restore_credential("telegram-api-hash", old_api_hash.as_deref());
            }
            return Err("填写代理密码时也必须填写代理用户名".into());
        }
        if let Err(error) = credentials::write("telegram-proxy-password", &submitted_proxy_password)
        {
            if wrote_api_hash {
                restore_credential("telegram-api-hash", old_api_hash.as_deref());
            }
            return Err(command_error(error));
        }
        wrote_proxy_password = true;
    }

    if let Err(error) = storage::save_settings(&layout, &settings) {
        if wrote_api_hash {
            restore_credential("telegram-api-hash", old_api_hash.as_deref());
        }
        if wrote_proxy_password {
            restore_credential("telegram-proxy-password", old_proxy_password.as_deref());
        }
        return Err(command_error(error));
    }

    *current_settings = settings;
    drop(service_slot);
    drop(current_settings);
    state
        .shared
        .log(
            "info",
            "settings",
            "设置已保存；敏感凭据保存在系统凭据管理器",
        )
        .await;
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn import_legacy_config(
    state: State<'_, AppState>,
    path: String,
) -> Result<MigrationReport, String> {
    let requested = PathBuf::from(path.trim());
    if !requested.is_absolute() {
        return Err("旧配置文件路径必须是绝对路径".into());
    }
    let path = requested.canonicalize().map_err(command_error)?;
    if !path.is_file() {
        return Err("请选择现有的 config.yaml 或 data.yaml 文件".into());
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if extension != "yaml" && extension != "yml" {
        return Err("旧配置只接受 YAML 文件（.yaml 或 .yml）".into());
    }
    if fs::metadata(&path).map_err(command_error)?.len() > MAX_LEGACY_CONFIG_BYTES {
        return Err("旧配置文件超过 1 MiB，已拒绝导入".into());
    }

    let layout = state.shared.layout.read().await.clone();
    ensure_no_active_or_queued_downloads(&state.shared.store).await?;
    let mut current_settings = state.shared.settings.write().await;
    let mut service_slot = state.shared.telegram.write().await;
    if service_slot
        .as_ref()
        .is_some_and(|service| Arc::strong_count(service) > 1)
    {
        return Err("Telegram 连接仍有操作正在进行；请稍后再导入旧配置".into());
    }
    let prior_settings = current_settings.clone();
    let prior_api_hash = credentials::read("telegram-api-hash").map_err(command_error)?;
    let prior_proxy_password =
        credentials::read("telegram-proxy-password").map_err(command_error)?;

    let mut report =
        match legacy_config::import_legacy_config(&path, &layout, &state.shared.store).await {
            Ok(report) => report,
            Err(error) => {
                rollback_import(
                    &layout,
                    &prior_settings,
                    prior_api_hash.as_deref(),
                    prior_proxy_password.as_deref(),
                );
                return Err(command_error(error));
            }
        };
    let imported_api_hash = match credentials::read("telegram-api-hash") {
        Ok(secret) => secret,
        Err(error) => {
            rollback_import(
                &layout,
                &prior_settings,
                prior_api_hash.as_deref(),
                prior_proxy_password.as_deref(),
            );
            return Err(command_error(error));
        }
    };
    let imported_proxy_password = match credentials::read("telegram-proxy-password") {
        Ok(secret) => secret,
        Err(error) => {
            rollback_import(
                &layout,
                &prior_settings,
                prior_api_hash.as_deref(),
                prior_proxy_password.as_deref(),
            );
            return Err(command_error(error));
        }
    };
    let mut imported_settings = report.settings;
    imported_settings.data_root = layout.root.to_string_lossy().into_owned();
    imported_settings.api_hash.clear();
    imported_settings.proxy.password = imported_proxy_password.clone().unwrap_or_default();
    imported_settings.api_hash_configured = imported_api_hash.is_some();
    imported_settings.proxy.password_configured = imported_proxy_password.is_some();

    let validation = imported_api_hash
        .as_deref()
        .map(validate_api_hash)
        .unwrap_or(Ok(()))
        .and_then(|_| normalize_and_validate_settings(&mut imported_settings));
    if let Err(error) = validation {
        rollback_import(
            &layout,
            &prior_settings,
            prior_api_hash.as_deref(),
            prior_proxy_password.as_deref(),
        );
        return Err(error);
    }
    let connection_changed = prior_settings.api_id.trim() != imported_settings.api_id.trim()
        || prior_api_hash.as_deref().unwrap_or_default()
            != imported_api_hash.as_deref().unwrap_or_default()
        || prior_settings.proxy.enabled != imported_settings.proxy.enabled
        || prior_settings.proxy.scheme != imported_settings.proxy.scheme
        || prior_settings.proxy.host != imported_settings.proxy.host
        || prior_settings.proxy.port != imported_settings.proxy.port
        || prior_settings.proxy.username != imported_settings.proxy.username
        || prior_proxy_password.as_deref().unwrap_or_default()
            != imported_proxy_password.as_deref().unwrap_or_default();
    if connection_changed {
        let no_active_tasks = ensure_no_active_or_queued_downloads(&state.shared.store)
            .await
            .is_ok();
        let no_live_operations = service_slot
            .as_ref()
            .is_none_or(|service| Arc::strong_count(service) == 1);
        let session_authorized = telegram_session_is_authorized_in_slot(&service_slot, &layout)
            .await
            .unwrap_or(true);
        if !no_active_tasks || !no_live_operations || session_authorized {
            rollback_import(
                &layout,
                &prior_settings,
                prior_api_hash.as_deref(),
                prior_proxy_password.as_deref(),
            );
            return Err(
                "活动任务或已登录 Session 阻止导入新的 API/代理连接设置；请先停止任务并注销".into(),
            );
        }
        if let Some(service) = service_slot.as_ref() {
            if let Err(error) = service.shutdown().await {
                rollback_import(
                    &layout,
                    &prior_settings,
                    prior_api_hash.as_deref(),
                    prior_proxy_password.as_deref(),
                );
                return Err(command_error(error));
            }
        }
        service_slot.take();
    }

    imported_settings.api_hash.clear();
    imported_settings.proxy.password = imported_proxy_password.clone().unwrap_or_default();
    imported_settings.api_hash_configured = imported_api_hash.is_some();
    imported_settings.proxy.password_configured = imported_proxy_password.is_some();
    if let Err(error) = storage::save_settings(&layout, &imported_settings) {
        rollback_import(
            &layout,
            &prior_settings,
            prior_api_hash.as_deref(),
            prior_proxy_password.as_deref(),
        );
        return Err(command_error(error));
    }
    *current_settings = imported_settings.clone();
    drop(service_slot);
    drop(current_settings);
    report.settings = imported_settings;
    state
        .shared
        .log(
            "info",
            "migration",
            "旧版 YAML 设置已导入；Bot 专属字段已忽略",
        )
        .await;
    Ok(report)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn login_request_code(
    state: State<'_, AppState>,
    phone: String,
) -> Result<SessionSummary, String> {
    let phone = phone.trim();
    if phone.is_empty() || phone.len() > 40 || phone.chars().any(char::is_control) {
        return Err("手机号格式无效；请包含国家/地区代码".into());
    }
    let api_hash = credentials::read("telegram-api-hash")
        .map_err(command_error)?
        .filter(|value| valid_api_hash(value))
        .ok_or_else(|| "请先在设置中填写有效的 Telegram API Hash".to_owned())?;
    let service = telegram_adapter(&state).await?;
    service
        .request_code(phone, &api_hash)
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn login_submit_code(
    state: State<'_, AppState>,
    code: String,
) -> Result<SessionSummary, String> {
    if code.trim().is_empty() || code.len() > 32 || code.chars().any(char::is_control) {
        return Err("验证码格式无效".into());
    }
    telegram_adapter(&state)
        .await?
        .submit_code(&code)
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn login_submit_password(
    state: State<'_, AppState>,
    password: String,
) -> Result<SessionSummary, String> {
    if password.is_empty() || password.len() > 1024 || password.chars().any(char::is_control) {
        return Err("两步验证密码格式无效".into());
    }
    telegram_adapter(&state)
        .await?
        .submit_password(&password)
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn logout_session(
    state: State<'_, AppState>,
    remove_session: bool,
) -> Result<SessionSummary, String> {
    ensure_no_active_or_queued_downloads(&state.shared.store).await?;
    let layout = state.shared.layout.read().await.clone();
    let settings_guard = state.shared.settings.read().await;
    let current_settings = settings_guard.clone();
    let mut service_slot = state.shared.telegram.write().await;

    if let Some(service) = service_slot.as_ref() {
        if Arc::strong_count(service) != 1 {
            return Err("Telegram 会话仍有操作正在进行；请稍后再注销".into());
        }
        service.logout().await.map_err(command_error)?;
        service.shutdown().await.map_err(command_error)?;
    } else if layout.session_file.exists() {
        let api_id = current_settings
            .api_id
            .trim()
            .parse::<i32>()
            .map_err(|_| "注销已有 Session 前需要有效的 Telegram API ID".to_owned())?;
        if api_id <= 0 {
            return Err("注销已有 Session 前需要有效的 Telegram API ID".into());
        }
        let mut proxy = current_settings.proxy.clone();
        proxy.password = credentials::read("telegram-proxy-password")
            .map_err(command_error)?
            .unwrap_or_default();
        let service = TelegramAdapter::open(&layout, api_id, &proxy).map_err(command_error)?;
        service.logout().await.map_err(command_error)?;
        service.shutdown().await.map_err(command_error)?;
    }

    let stopped_service = service_slot.take();
    drop(stopped_service);
    drop(service_slot);
    drop(settings_guard);

    if remove_session {
        if layout.session_file.exists() {
            fs::remove_file(&layout.session_file).map_err(command_error)?;
        }
        credentials::remove("mtproto-session-encryption-key").map_err(command_error)?;
    }
    state
        .shared
        .log(
            "info",
            "session",
            if remove_session {
                "Telegram Session 已注销并清理"
            } else {
                "Telegram Session 已注销"
            },
        )
        .await;
    Ok(signed_out_summary())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn list_chats(
    state: State<'_, AppState>,
    query: String,
    offset: usize,
    limit: usize,
) -> Result<Page<ChatInfo>, String> {
    if query.len() > 256 || query.chars().any(char::is_control) {
        return Err("聊天搜索内容过长或包含控制字符".into());
    }
    if offset > 1_000_000 {
        return Err("聊天分页偏移超出允许范围".into());
    }
    let limit = validate_page_limit(limit)?;
    let service = telegram_adapter(&state).await?;
    let (items, has_more) = service
        .chats(&query, offset, limit)
        .await
        .map_err(command_error)?;
    let next_offset = has_more.then(|| offset.saturating_add(items.len()) as i64);
    Ok(Page {
        items,
        next_offset,
        has_more,
    })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_chat_messages(
    state: State<'_, AppState>,
    chat_id: ChatIdInput,
    offset: i64,
    limit: usize,
) -> Result<Page<MessageInfo>, String> {
    let chat_id = chat_id.into_validated()?;
    if offset < 0 || offset > i64::from(i32::MAX) {
        return Err("消息分页游标必须是有效的 Telegram 消息 ID".into());
    }
    let limit = validate_page_limit(limit)?;
    let service = telegram_adapter(&state).await?;
    let (items, next_offset, has_more) = service
        .messages(&chat_id, offset, limit)
        .await
        .map_err(command_error)?;
    Ok(Page {
        items,
        next_offset,
        has_more,
    })
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
    state
        .shared
        .store
        .list(status.as_deref(), limit)
        .await
        .map_err(command_error)
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
    if !target.starts_with(&download_root) {
        return Err("任务路径不在配置的下载目录中，已拒绝打开".into());
    }

    let reveal = if target.exists() {
        let canonical = target.canonicalize().map_err(command_error)?;
        if !canonical.starts_with(&download_root) {
            return Err("任务文件解析到下载目录之外，已拒绝打开".into());
        }
        canonical
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
        let parent = parent.canonicalize().map_err(command_error)?;
        if !parent.starts_with(&download_root) {
            return Err("任务目录解析到下载目录之外，已拒绝打开".into());
        }
        parent
    };
    tauri_plugin_opener::reveal_item_in_dir(reveal).map_err(command_error)
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
pub async fn list_cloud_uploads(
    state: State<'_, AppState>,
    status: Option<String>,
    limit: usize,
) -> Result<Vec<UploadTaskRecord>, String> {
    if let Some(status) = status.as_deref() {
        if !["queued", "uploading", "completed", "failed", "cancelled"].contains(&status) {
            return Err("云上传状态筛选值无效".into());
        }
    }
    state
        .uploads
        .list(status.as_deref(), limit.min(MAX_TASKS_PER_LIST))
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn queue_cloud_upload(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<UploadTaskRecord, String> {
    if task_id.len() > 64 || task_id.chars().any(char::is_control) {
        return Err("下载任务 ID 格式无效".into());
    }
    state
        .uploads
        .queue_completed_download(&task_id)
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn cloud_upload_action(
    state: State<'_, AppState>,
    upload_task_id: String,
    action: String,
) -> Result<UploadTaskRecord, String> {
    if upload_task_id.len() > 64 || upload_task_id.chars().any(char::is_control) {
        return Err("云上传任务 ID 格式无效".into());
    }
    state
        .uploads
        .action(&upload_task_id, &action)
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn upload_completed_download(
    state: State<'_, AppState>,
    task_id: String,
    target_chat_id: Option<String>,
) -> Result<TelegramTransferRecord, String> {
    validate_task_id(&task_id)?;
    let target_chat_id = target_chat_id
        .map(|value| validate_chat_id(value.trim()))
        .transpose()?;
    state
        .telegram_transfers
        .queue_upload(&task_id, target_chat_id.as_deref())
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn forward_telegram_message(
    state: State<'_, AppState>,
    source_chat_id: ChatIdInput,
    source_message_id: i64,
    target_chat_id: Option<String>,
) -> Result<TelegramTransferRecord, String> {
    let source_chat_id = source_chat_id.into_validated()?;
    validate_message_id(source_message_id)?;
    let target_chat_id = target_chat_id
        .map(|value| validate_chat_id(value.trim()))
        .transpose()?;
    state
        .telegram_transfers
        .queue_forward(
            &source_chat_id,
            source_message_id,
            target_chat_id.as_deref(),
        )
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn list_telegram_transfers(
    state: State<'_, AppState>,
    status: Option<String>,
    limit: usize,
) -> Result<Vec<TelegramTransferRecord>, String> {
    if let Some(status) = status.as_deref()
        && ![
            "queued",
            "preparing",
            "uploading",
            "sending",
            "completed",
            "failed",
            "cancelled",
            "uncertain",
        ]
        .contains(&status)
    {
        return Err("Telegram 传输任务状态筛选值无效".into());
    }
    state
        .telegram_transfers
        .list(status.as_deref(), limit.min(MAX_TASKS_PER_LIST))
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn telegram_transfer_action(
    state: State<'_, AppState>,
    transfer_task_id: String,
    action: String,
) -> Result<TelegramTransferRecord, String> {
    if transfer_task_id.len() > 64 || transfer_task_id.chars().any(char::is_control) {
        return Err("Telegram 传输任务 ID 格式无效".into());
    }
    state
        .telegram_transfers
        .action(&transfer_task_id, &action)
        .await
        .map_err(command_error)
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
            data_root: old_root.to_string_lossy().into_owned(),
            download_root: settings.download_root.clone(),
            restart_required: false,
        });
    }
    if new_root.starts_with(&old_root) || old_root.starts_with(&new_root) {
        return Err("新旧数据目录不能相互包含".into());
    }
    ensure_no_active_or_queued_downloads(&state.shared.store).await?;
    ensure_no_active_or_queued_uploads(&state).await?;
    if fs::read_dir(&new_root)
        .map_err(command_error)?
        .next()
        .is_some()
    {
        return Err("目标数据目录必须为空".into());
    }

    let mut settings_guard = state.shared.settings.write().await;
    let mut service_slot = state.shared.telegram.write().await;
    ensure_no_active_or_queued_downloads(&state.shared.store).await?;
    ensure_no_active_or_queued_uploads(&state).await?;
    if let Some(service) = service_slot.as_ref() {
        if Arc::strong_count(service) != 1 {
            return Err("Telegram 会话仍有请求在运行；请稍后重试数据迁移".into());
        }
        service.shutdown().await.map_err(command_error)?;
        service_slot.take();
    }

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
    let mapped_download_root = if previous_download_root.starts_with(&old_root) {
        Some(
            new_root.join(
                previous_download_root
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
        migrated_settings.download_root = download_root.to_string_lossy().into_owned();
    }
    migrated_settings.data_root = new_root.to_string_lossy().into_owned();
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
    drop(service_slot);
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
        data_root: new_root.to_string_lossy().into_owned(),
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
#[tauri::command(rename_all = "camelCase")]
pub async fn plan_chunks(
    webview: Webview<Wry>,
    state: State<'_, AppState>,
    task_id: String,
    total_bytes: u64,
) -> Result<PlanInfo, String> {
    ensure_trusted_telegram_webview(&webview)?;
    validate_task_id(&task_id)?;
    state
        .downloads
        .plan(&task_id, total_bytes)
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
) -> Result<TaskRecord, String> {
    ensure_trusted_telegram_webview(&webview)?;
    validate_task_id(&task_id)?;
    state
        .downloads
        .fail(&task_id, &error)
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

async fn telegram_adapter(state: &AppState) -> Result<Arc<TelegramAdapter>, String> {
    let proxy_password = credentials::read("telegram-proxy-password").map_err(command_error)?;
    {
        let mut settings = state.shared.settings.write().await;
        settings.proxy.password = proxy_password.unwrap_or_default();
        settings.proxy.password_configured = !settings.proxy.password.is_empty();
    }
    state.telegram().await.map_err(command_error)
}

async fn current_session_summary(state: &AppState, layout: &StorageLayout) -> SessionSummary {
    if let Some(service) = state.shared.telegram.read().await.as_ref() {
        return service.summary().await;
    }
    if !layout.session_file.exists() {
        return signed_out_summary();
    }
    match EncryptedSession::open(&layout.session_file) {
        Ok(session) => {
            let authorized = session.is_authorized();
            SessionSummary {
                authorized,
                status: if authorized {
                    "storedSession"
                } else {
                    "signedOut"
                }
                .into(),
                display_name: None,
                phone: None,
                user_id: None,
                error: None,
            }
        }
        Err(error) => SessionSummary {
            authorized: false,
            status: "error".into(),
            display_name: None,
            phone: None,
            user_id: None,
            error: Some(command_error(error)),
        },
    }
}

async fn telegram_session_is_authorized_in_slot(
    service_slot: &Option<Arc<TelegramAdapter>>,
    layout: &StorageLayout,
) -> Result<bool, String> {
    if let Some(service) = service_slot.as_ref() {
        return service.is_authorized().await.map_err(command_error);
    }
    if !layout.session_file.exists() {
        return Ok(false);
    }
    EncryptedSession::open(&layout.session_file)
        .map(|session| session.is_authorized())
        .map_err(command_error)
}

async fn ensure_no_active_or_queued_downloads(store: &TaskStore) -> Result<(), String> {
    let stats = store.stats().await.map_err(command_error)?;
    if stats.active_downloads > 0 || stats.queued_downloads > 0 {
        return Err("请先暂停或完成活动/排队任务，再执行此操作".into());
    }
    Ok(())
}

async fn ensure_no_active_or_queued_uploads(state: &AppState) -> Result<(), String> {
    for status in ["queued", "uploading"] {
        if !state
            .uploads
            .list(Some(status), MAX_TASKS_PER_LIST)
            .await
            .map_err(command_error)?
            .is_empty()
        {
            return Err("请先完成或取消云端上传任务，再迁移数据目录".into());
        }
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

fn validate_page_limit(limit: usize) -> Result<usize, String> {
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(format!("分页大小必须在 1 到 {MAX_PAGE_SIZE} 之间"));
    }
    Ok(limit)
}

fn validate_chat_id(value: &str) -> Result<String, String> {
    if value.is_empty() || value.len() > CHAT_ID_MAX_CHARS || value == "0" {
        return Err("Telegram 聊天 ID 格式无效".into());
    }
    let digits = value.strip_prefix('-').unwrap_or(value);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("Telegram 聊天 ID 只能包含可选负号和十进制数字".into());
    }
    let parsed = value
        .parse::<i64>()
        .map_err(|_| "Telegram 聊天 ID 超出允许范围".to_owned())?;
    if parsed == 0 {
        return Err("Telegram 聊天 ID 不能为 0".into());
    }
    Ok(parsed.to_string())
}

fn validate_message_id(message_id: i64) -> Result<(), String> {
    if !(1..=i64::from(i32::MAX)).contains(&message_id) {
        return Err("Telegram 消息 ID 必须是正整数且不超过协议范围".into());
    }
    Ok(())
}

fn validate_media_type(media_type: &str) -> Result<(), String> {
    if !ALLOWED_MEDIA_TYPES.contains(&media_type) {
        return Err("此媒体类型不受支持".into());
    }
    Ok(())
}

fn validate_media_filters(
    media_types: &[String],
    file_formats: &BTreeMap<String, Vec<String>>,
) -> Result<(), String> {
    if media_types.len() > ALLOWED_MEDIA_TYPES.len() {
        return Err("媒体类型筛选项过多".into());
    }
    let mut unique_types = BTreeSet::new();
    for media_type in media_types {
        validate_media_type(media_type)?;
        if !unique_types.insert(media_type) {
            return Err("媒体类型筛选项不能重复".into());
        }
    }
    if file_formats.len() > ALLOWED_MEDIA_TYPES.len() {
        return Err("文件格式筛选项过多".into());
    }
    for (media_type, formats) in file_formats {
        validate_media_type(media_type)?;
        if formats.len() > 64 {
            return Err("单种媒体的文件格式筛选项过多".into());
        }
        for extension in formats {
            let extension = extension.trim().trim_start_matches('.');
            if extension.is_empty()
                || extension.len() > 16
                || !extension
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+'))
            {
                return Err("文件扩展名筛选格式无效".into());
            }
        }
    }
    Ok(())
}

fn validate_task_id(task_id: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(task_id)
        .map(|_| ())
        .map_err(|_| "下载任务 ID 格式无效".to_owned())
}

fn validate_api_hash(api_hash: &str) -> Result<(), String> {
    if !valid_api_hash(api_hash) {
        return Err("Telegram API Hash 必须是 32 位十六进制字符串".into());
    }
    Ok(())
}

fn valid_api_hash(api_hash: &str) -> bool {
    api_hash.len() == 32 && api_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn normalize_and_validate_settings(settings: &mut Settings) -> Result<(), String> {
    let api_id = settings.api_id.trim();
    if !api_id.is_empty() {
        let parsed = api_id
            .parse::<i32>()
            .map_err(|_| "Telegram API ID 必须是正整数".to_owned())?;
        if parsed <= 0 {
            return Err("Telegram API ID 必须是正整数".into());
        }
        settings.api_id = parsed.to_string();
    } else {
        settings.api_id.clear();
    }
    if !settings.api_hash.is_empty() {
        validate_api_hash(&settings.api_hash)?;
    }

    validate_media_filters(&settings.media_types, &settings.file_formats)?;
    if settings.chat_filters.len() > 10_000 {
        return Err("每聊天筛选规则最多支持 10000 项".into());
    }
    for (chat_id, expression) in &settings.chat_filters {
        validate_chat_id(chat_id)?;
        if expression.len() > 4096 || expression.chars().any(char::is_control) {
            return Err(format!("聊天 {chat_id} 的筛选表达式超过限制"));
        }
        crate::filter::validate(expression)
            .map_err(|error| format!("聊天 {chat_id} 的筛选表达式无效：{error}"))?;
    }
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

    if settings.proxy.scheme != "socks5" {
        return Err("当前 MTProto 下载核心只支持 SOCKS5 代理".into());
    }
    if settings.proxy.enabled {
        if settings.proxy.host.trim().is_empty() || settings.proxy.host.len() > 253 {
            return Err("请填写有效的 SOCKS5 代理主机".into());
        }
        if settings.proxy.port == 0 {
            return Err("SOCKS5 代理端口必须在 1 到 65535 之间".into());
        }
    }
    if settings.proxy.host.chars().any(char::is_control)
        || settings.proxy.username.len() > 255
        || settings.proxy.username.chars().any(char::is_control)
        || settings.proxy.password.len() > 1024
        || settings.proxy.password.chars().any(char::is_control)
    {
        return Err("代理主机或凭据格式无效".into());
    }

    if settings.cloud_upload.adapter != "rclone" {
        return Err("当前桌面客户端只支持 rclone 云盘上传；请选择 rclone".into());
    }
    if settings.cloud_upload.executable_path.len() > 4096
        || settings.cloud_upload.remote_dir.len() > 1024
        || settings
            .cloud_upload
            .remote_dir
            .chars()
            .any(char::is_control)
    {
        return Err("云端上传路径或远端目录无效".into());
    }
    if !settings.cloud_upload.executable_path.trim().is_empty()
        && !Path::new(&settings.cloud_upload.executable_path).is_absolute()
    {
        return Err("rclone 可执行文件路径必须是绝对路径".into());
    }
    if settings.telegram_upload_enabled {
        validate_chat_id(&settings.telegram_upload_target_chat_id)?;
    } else if !settings.telegram_upload_target_chat_id.is_empty() {
        validate_chat_id(&settings.telegram_upload_target_chat_id)?;
    }
    if settings.telegram_upload_chat_targets.len() > 2_000 {
        return Err("Telegram 来源聊天目标映射最多保存 2000 条".into());
    }
    for (source_chat_id, target_chat_id) in &settings.telegram_upload_chat_targets {
        validate_chat_id(source_chat_id)?;
        validate_chat_id(target_chat_id)?;
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
    settings.download_root = root
        .canonicalize()
        .map_err(command_error)?
        .to_string_lossy()
        .into_owned();
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

fn restore_credential(name: &str, value: Option<&str>) {
    let result = match value {
        Some(value) => credentials::write(name, value),
        None => credentials::remove(name),
    };
    if let Err(error) = result {
        tracing::error!(target: "desktop::credentials", "无法回滚凭据变更：{error:#}");
    }
}

fn rollback_import(
    layout: &StorageLayout,
    previous_settings: &Settings,
    previous_api_hash: Option<&str>,
    previous_proxy_password: Option<&str>,
) {
    restore_credential("telegram-api-hash", previous_api_hash);
    restore_credential("telegram-proxy-password", previous_proxy_password);
    if let Err(error) = storage::save_settings(layout, previous_settings) {
        tracing::error!(target: "desktop::migration", "无法回滚旧设置：{error:#}");
    }
}

fn signed_out_summary() -> SessionSummary {
    SessionSummary {
        authorized: false,
        status: "signedOut".into(),
        display_name: None,
        phone: None,
        user_id: None,
        error: None,
    }
}

fn command_error(error: impl Display) -> String {
    let mut message = error.to_string();
    if message.len() > 1_000 {
        message.truncate(1_000);
        message.push_str("…");
    }
    message
}
