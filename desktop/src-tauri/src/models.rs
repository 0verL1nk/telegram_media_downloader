use serde::{Deserialize, Serialize};

/// Persisted client preferences.
///
/// The client is a page-fetch downloader: URLs and Telegram credentials never
/// reach this struct, so it stores only local paths, naming rules, queue
/// limits, and cosmetic preferences. Unknown keys from older settings files
/// are ignored by serde on deserialize, which keeps upgrades safe.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub data_root: String,
    pub download_root: String,
    pub path_template: String,
    pub file_name_template: String,
    pub date_format: String,
    pub duplicate_policy: String,
    pub concurrency: ConcurrencySettings,
    pub language: String,
    pub preserve_partial_files: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            data_root: String::new(),
            download_root: String::new(),
            path_template: "{chat}/{year}/{month}".into(),
            file_name_template: "{date}_{message_id}_{name}".into(),
            date_format: "%Y_%m".into(),
            duplicate_policy: "rename".into(),
            concurrency: ConcurrencySettings::default(),
            language: "zh-CN".into(),
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
    /// 单文件分块并发数。adaptive 开启时是自适应上限;关闭时即固定并发。
    pub per_file_chunks: usize,
    /// 自适应并发(BBR 式投递率探测 + 乘性退避),默认开启。
    pub adaptive: bool,
    pub chunk_size_kib: usize,
    pub request_timeout_seconds: u64,
    pub retries: u32,
    pub max_bandwidth_kib: u64,
    /// 后端学习值:实测单路峰值速率(字节/秒),用于 BDP 分块估算。0 表示尚无数据。
    /// 只由下载器写入,设置表单回传时不覆盖(见 commands::save_settings)。
    pub learned_per_stream_bytes_per_second: u64,
}

impl Default for ConcurrencySettings {
    fn default() -> Self {
        Self {
            max_files: 3,
            per_file_chunks: 16,
            adaptive: true,
            chunk_size_kib: 512,
            request_timeout_seconds: 45,
            retries: 5,
            max_bandwidth_kib: 0,
            learned_per_stream_bytes_per_second: 0,
        }
    }
}

/// Stable UI DTO. Database-only names stay inside the ORM adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
        }
    }
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
    pub storage_options: Vec<StorageOption>,
    pub stats: RuntimeStats,
}
