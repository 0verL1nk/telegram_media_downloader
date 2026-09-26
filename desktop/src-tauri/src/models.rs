use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub data_root: String,
    pub download_root: String,
    #[serde(alias = "api_id")]
    pub api_id: String,
    #[serde(default, serialize_with = "serialize_secret_blank")]
    pub api_hash: String,
    pub api_hash_configured: bool,
    pub media_types: Vec<String>,
    pub file_formats: BTreeMap<String, Vec<String>>,
    pub chat_filters: BTreeMap<String, String>,
    pub path_template: String,
    pub file_name_template: String,
    pub date_format: String,
    pub duplicate_policy: String,
    pub caption_sidecar: bool,
    pub text_sidecar: bool,
    pub telegram_upload_enabled: bool,
    pub telegram_upload_target_chat_id: String,
    pub telegram_upload_chat_targets: BTreeMap<String, String>,
    pub telegram_upload_delete_local_after_upload: bool,
    pub concurrency: ConcurrencySettings,
    pub proxy: ProxySettings,
    pub cloud_upload: CloudUploadSettings,
    pub language: String,
    pub auto_resume: bool,
    pub preserve_partial_files: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            data_root: String::new(),
            download_root: String::new(),
            api_id: String::new(),
            api_hash: String::new(),
            api_hash_configured: false,
            media_types: vec![
                "photo".into(),
                "video".into(),
                "document".into(),
                "audio".into(),
            ],
            file_formats: BTreeMap::new(),
            chat_filters: BTreeMap::new(),
            path_template: "{chat}/{year}/{month}".into(),
            file_name_template: "{date}_{message_id}_{name}".into(),
            date_format: "%Y_%m".into(),
            duplicate_policy: "rename".into(),
            caption_sidecar: false,
            text_sidecar: true,
            telegram_upload_enabled: false,
            telegram_upload_target_chat_id: String::new(),
            telegram_upload_chat_targets: BTreeMap::new(),
            telegram_upload_delete_local_after_upload: false,
            concurrency: ConcurrencySettings::default(),
            proxy: ProxySettings::default(),
            cloud_upload: CloudUploadSettings::default(),
            language: "zh-CN".into(),
            auto_resume: true,
            preserve_partial_files: true,
        }
    }
}

pub fn valid_date_format(format: &str) -> bool {
    use chrono::format::{Item, StrftimeItems};

    !format.trim().is_empty()
        && format.len() <= 128
        && !format.chars().any(char::is_control)
        && !StrftimeItems::new(format).any(|item| matches!(item, Item::Error))
}

#[cfg(test)]
mod date_format_tests {
    use super::valid_date_format;

    #[test]
    fn accepts_chrono_strftime_patterns_and_rejects_invalid_or_unsafe_values() {
        assert!(valid_date_format("%Y_%m"));
        assert!(valid_date_format("%Y-%m-%d %H:%M"));
        assert!(!valid_date_format("%Q"));
        assert!(!valid_date_format("   "));
        assert!(!valid_date_format("%Y\n%m"));
        assert!(!valid_date_format(&"x".repeat(129)));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ConcurrencySettings {
    pub max_files: usize,
    pub per_file_chunks: usize,
    pub chunk_size_kib: usize,
    pub request_timeout_seconds: u64,
    pub retries: u32,
    pub max_bandwidth_kib: u64,
}

impl Default for ConcurrencySettings {
    fn default() -> Self {
        Self {
            max_files: 3,
            per_file_chunks: 2,
            chunk_size_kib: 512,
            request_timeout_seconds: 45,
            retries: 5,
            max_bandwidth_kib: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProxySettings {
    pub enabled: bool,
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default, serialize_with = "serialize_secret_blank")]
    pub password: String,
    pub password_configured: bool,
}

impl Default for ProxySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            scheme: "socks5".into(),
            host: String::new(),
            port: 1080,
            username: String::new(),
            password: String::new(),
            password_configured: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CloudUploadSettings {
    pub enabled: bool,
    pub adapter: String,
    pub executable_path: String,
    pub remote_dir: String,
    pub zip_before_upload: bool,
    pub delete_local_after_upload: bool,
}

impl Default for CloudUploadSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            adapter: "rclone".into(),
            executable_path: String::new(),
            remote_dir: String::new(),
            zip_before_upload: false,
            delete_local_after_upload: false,
        }
    }
}

/// Stable UI DTO. Database-only names stay inside the ORM adapter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskRecord {
    pub task_id: String,
    pub chat_id: String,
    pub chat_title: Option<String>,
    pub message_id: Option<i64>,
    pub media_type: Option<String>,
    pub file_name: Option<String>,
    pub status: String,
    pub progress: f64,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub speed_bytes_per_second: u64,
    pub remaining_bytes: Option<u64>,
    pub started_at: Option<String>,
    pub updated_at: Option<String>,
    pub completed_at: Option<String>,
    pub output_path: Option<String>,
    pub error: Option<String>,
    pub retry_count: u32,
    pub group_id: Option<String>,
}

/// Answer to the filename-based task-state query that drives the injected Telegram Web viewer
/// button. `state` is "queued" | "downloading" | "completed" | "failed" | "none"; `progress` is a
/// 0..1 ratio only while downloading.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStateMatch {
    pub state: String,
    pub task_id: Option<String>,
    pub progress: Option<f64>,
    pub file_size: Option<u64>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LegacyRetryRecord {
    pub chat_id: String,
    pub message_id: i64,
    pub imported_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UploadTaskRecord {
    pub upload_task_id: String,
    pub source_task_id: Option<String>,
    pub source_path: String,
    pub remote_path: String,
    pub executable_path: String,
    pub zip_before_upload: bool,
    pub delete_local_after_upload: bool,
    pub status: String,
    pub progress: f64,
    pub uploaded_bytes: u64,
    pub total_bytes: u64,
    pub speed_bytes_per_second: u64,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub error: Option<String>,
    pub retry_count: u32,
}

/// Durable ordinary-user MTProto upload/forward job. Numeric progress is deliberately absent:
/// grammers-client 0.10 does not expose a public upload progress callback for upload_stream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TelegramTransferRecord {
    pub transfer_task_id: String,
    pub kind: String,
    pub source_task_id: Option<String>,
    pub source_chat_id: Option<String>,
    pub source_message_id: Option<i64>,
    pub target_chat_id: String,
    pub delete_local_after_upload: bool,
    pub status: String,
    pub total_bytes: Option<u64>,
    pub sent_bytes: Option<u64>,
    pub result_message_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub error: Option<String>,
    pub retry_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageOption {
    pub path: String,
    pub label: Option<String>,
    pub available_bytes: u64,
    pub total_bytes: Option<u64>,
    pub is_system: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStats {
    pub active_downloads: usize,
    pub queued_downloads: usize,
    pub completed_downloads: usize,
    pub failed_downloads: usize,
    pub total_downloaded_bytes: u64,
    pub current_speed_bytes_per_second: u64,
    pub estimated_remaining_seconds: Option<u64>,
    pub cloud_uploads_active: usize,
}

impl Default for RuntimeStats {
    fn default() -> Self {
        Self {
            active_downloads: 0,
            queued_downloads: 0,
            completed_downloads: 0,
            failed_downloads: 0,
            total_downloaded_bytes: 0,
            current_speed_bytes_per_second: 0,
            estimated_remaining_seconds: None,
            cloud_uploads_active: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatInfo {
    pub chat_id: String,
    pub title: String,
    pub username: Option<String>,
    pub kind: Option<String>,
    pub unread_count: Option<i32>,
    pub last_message_at: Option<String>,
    pub avatar_color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageInfo {
    pub chat_id: String,
    pub message_id: i64,
    pub media_type: Option<String>,
    pub file_name: Option<String>,
    pub caption: Option<String>,
    pub text: Option<String>,
    pub size_bytes: Option<u64>,
    pub media_width: Option<i32>,
    pub media_height: Option<i32>,
    pub media_duration: Option<i64>,
    pub file_extension: Option<String>,
    pub sender_id: Option<i64>,
    pub sender_name: Option<String>,
    pub reply_to_message_id: Option<i64>,
    pub message_thread_id: Option<i64>,
    pub date: Option<String>,
    pub group_id: Option<String>,
    pub downloadable: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub target: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStateDto {
    pub app_version: String,
    pub settings: Settings,
    pub session: SessionSummary,
    pub storage_options: Vec<StorageOption>,
    pub stats: RuntimeStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub authorized: bool,
    pub status: String,
    pub display_name: Option<String>,
    pub phone: Option<String>,
    pub user_id: Option<String>,
    pub error: Option<String>,
}

fn serialize_secret_blank<S>(_: &String, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str("")
}
