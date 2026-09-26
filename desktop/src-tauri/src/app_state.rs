use crate::{
    cloud_upload::CloudUploadManager,
    credentials,
    downloader::DownloadManager,
    log_store::{self, PersistentLogs},
    models::{LogEntry, Settings},
    storage::{self, StorageLayout},
    task_store::TaskStore,
    telegram::TelegramAdapter,
    transfers::TelegramTransferManager,
};
use anyhow::{Context, Result, bail};
use std::{collections::VecDeque, path::PathBuf, sync::Arc};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{Mutex, RwLock, mpsc};

pub struct SharedState {
    pub app: AppHandle,
    pub root_pointer: PathBuf,
    pub layout: RwLock<StorageLayout>,
    pub settings: RwLock<Settings>,
    pub store: TaskStore,
    pub telegram: RwLock<Option<Arc<TelegramAdapter>>>,
    pub upload_sender: RwLock<Option<mpsc::Sender<String>>>,
    pub webview_init: Mutex<()>,
    pub logs: Mutex<VecDeque<LogEntry>>,
    pub persistent_logs: PersistentLogs,
}

pub struct AppState {
    pub shared: Arc<SharedState>,
    pub downloads: DownloadManager,
    pub uploads: CloudUploadManager,
    pub telegram_transfers: TelegramTransferManager,
}

impl AppState {
    pub async fn initialize(app: AppHandle) -> Result<Self> {
        let config_dir = app
            .path()
            .app_config_dir()
            .context("无法读取客户端配置目录")?;
        tokio::fs::create_dir_all(&config_dir).await?;
        let root_pointer = config_dir.join("active-data-root.txt");
        let fallback = app
            .path()
            .app_data_dir()
            .context("无法读取客户端默认数据目录")?;
        let root = storage::resolve_active_root(&root_pointer, &fallback)?;
        let layout = StorageLayout::under(root);
        layout.ensure()?;
        let mut settings = storage::load_settings(&layout)?;
        settings.api_hash_configured = credentials::read("telegram-api-hash")?.is_some();
        settings.proxy.password_configured =
            credentials::read("telegram-proxy-password")?.is_some();
        settings.data_root = layout.root.to_string_lossy().into_owned();
        if settings.download_root.trim().is_empty() {
            settings.download_root = layout.downloads.to_string_lossy().into_owned();
        }
        let store = TaskStore::open(&layout.database_file).await?;
        let (persistent_logs, recovered_logs) = PersistentLogs::open(&layout.logs, 500).await?;
        let shared = Arc::new(SharedState {
            app,
            root_pointer,
            layout: RwLock::new(layout),
            settings: RwLock::new(settings),
            store,
            telegram: RwLock::new(None),
            upload_sender: RwLock::new(None),
            webview_init: Mutex::new(()),
            logs: Mutex::new(VecDeque::from(recovered_logs)),
            persistent_logs,
        });
        let downloads = DownloadManager::start(Arc::clone(&shared)).await?;
        let uploads = CloudUploadManager::start(Arc::clone(&shared)).await?;
        let telegram_transfers = TelegramTransferManager::start(Arc::clone(&shared)).await?;
        shared
            .log(
                "info",
                "startup",
                "客户端就绪；任务数据库通过 SeaORM 迁移并恢复待续传任务",
            )
            .await;
        Ok(Self {
            shared,
            downloads,
            uploads,
            telegram_transfers,
        })
    }

    pub async fn telegram(&self) -> Result<Arc<TelegramAdapter>> {
        self.shared.telegram().await
    }
}

impl SharedState {
    pub async fn telegram(&self) -> Result<Arc<TelegramAdapter>> {
        if let Some(client) = self.telegram.read().await.as_ref() {
            return Ok(Arc::clone(client));
        }
        let settings = self.settings.read().await.clone();
        let api_id = settings
            .api_id
            .trim()
            .parse::<i32>()
            .context("请先在设置中填写有效的 Telegram API ID")?;
        if api_id <= 0 {
            bail!("Telegram API ID 必须为正整数");
        }
        let api_hash = credentials::read("telegram-api-hash")?
            .filter(|value| !value.trim().is_empty())
            .context("请先在设置中填写 Telegram API Hash")?;
        let mut guard = self.telegram.write().await;
        if let Some(client) = guard.as_ref() {
            return Ok(Arc::clone(client));
        }
        let layout = self.layout.read().await.clone();
        let client = Arc::new(TelegramAdapter::open(&layout, api_id, &settings.proxy)?);
        *guard = Some(Arc::clone(&client));
        drop(api_hash);
        Ok(client)
    }

    pub async fn log(&self, level: &str, target: &str, message: impl Into<String>) {
        let entry = log_store::sanitize_entry(LogEntry {
            timestamp: chrono::Utc::now().to_rfc3339(),
            level: level.to_owned(),
            target: Some(target.to_owned()),
            message: message.into(),
        });
        let mut logs = self.logs.lock().await;
        if let Err(error) = self.persistent_logs.append(&entry).await {
            tracing::error!(target: "desktop::logging", "无法持久化日志记录：{error:#}");
        }
        if logs.len() >= 500 {
            logs.pop_front();
        }
        logs.push_back(entry.clone());
        drop(logs);
        match level {
            "error" => {
                tracing::error!(target: "desktop", component = target, message = %entry.message)
            }
            "warn" => {
                tracing::warn!(target: "desktop", component = target, message = %entry.message)
            }
            _ => tracing::info!(target: "desktop", component = target, message = %entry.message),
        }
    }

    pub async fn publish_task(&self, task_id: &str) {
        match self.store.get(task_id).await {
            Ok(Some(task)) => {
                let _ = self.app.emit("task-updated", task);
                if let Ok(stats) = self.store.stats().await {
                    let _ = self
                        .app
                        .emit("stats-updated", serde_json::json!({"stats":stats}));
                }
            }
            Ok(None) => {}
            Err(error) => {
                self.log("error", "database", format!("读取任务状态失败：{error:#}"))
                    .await
            }
        }
    }
}
