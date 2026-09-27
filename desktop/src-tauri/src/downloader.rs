//! WebView 页面抓取下载管理器。
//!
//! 下载的网络请求全部在 Telegram Web 页面上下文里执行(与脚本 #446342 一致),Rust 只负责
//! 任务登记、分块落盘(`chunk_writer`)、进度事件、槽位/背压、看门狗兜底与原子提交。
//! URL 与 cookie 永不进入 Rust。
//!
//! 数据流(见 `docs/superpowers/specs/2026-09-26-webview-http-downloader-design.md`):
//! `create` 建记录 → `plan`(占文件槽位、配置分块表、启动 writer,返回缺块清单) →
//! `push`(页面 fetch 到的分块经 IPC 入队,mpsc 满时自然背压) →
//! `finish`(全块校验 + 原子提交)/ `fail`(保留已校验分块以便续传)。
//!
//! 看门狗每 10 秒巡检一次:下载中且 30 秒既无 push 也无 finish 的任务会被暂停并发出
//! `webview-download-abort`(覆盖 WebView 关闭/刷新/网络掉线等 JS 侧消失的情形)。

use crate::{
    adaptive::AdaptiveConcurrency,
    app_state::SharedState,
    chunk_writer::{self, IncomingChunk},
    models::TaskRecord,
    storage,
    task_store::TaskStore,
};
use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::Emitter;
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt, SeekFrom},
    sync::{Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore, broadcast, mpsc},
    task::JoinHandle,
};

/// 进度事件节流间隔。
const PROGRESS_TICK: Duration = Duration::from_millis(350);
/// 分块入队等待超过该时长即告警(写入端停滞的早期信号)。
const SLOW_PUSH_WARN: Duration = Duration::from_secs(5);
/// 看门狗巡检间隔。
const WATCHDOG_TICK: Duration = Duration::from_secs(10);
/// 活动任务超过此间隔既无 push 也无 finish,即判定页面侧已消失。
const ACTIVITY_TIMEOUT: Duration = Duration::from_secs(30);
/// 单个 IPC 分块字节数硬上限。分块设置上界是 1 MiB,这里留出余量以拒绝异常调用方。
const CHUNK_HARD_CAP: usize = 4 * 1024 * 1024;
/// 分块映射允许的单块最大字节数(与设置页 1 MiB 上界一致)。
const MAX_CHUNK_BYTES: u64 = 1024 * 1024;
/// 文件名最大字符数。
const MAX_FILE_NAME_CHARS: usize = 512;
/// 媒体类型标记最大字符数。
const MAX_FILE_TYPE_CHARS: usize = 64;
/// 任务来源摘要最大字符数(只用于日志)。
const MAX_SOURCE_SUMMARY_CHARS: usize = 200;
/// 重复文件命名候选上限。
const MAX_DUPLICATE_NAME_ATTEMPTS: u32 = 100_000;
/// 分块大小候选(设置页 64–1024 KiB;HTTP Range 没有 Telegram 的 1 MiB 对齐约束)。
const CHUNK_SIZE_KIB_CHOICES: [usize; 5] = [64, 128, 256, 512, 1024];
const MIN_CHUNK_SIZE_KIB: usize = 64;
const MAX_CHUNK_SIZE_KIB: usize = 1024;
/// 每个文件的并发抓取路数(随计划返回给页面 JS)。
const MIN_PER_FILE_CHUNKS: usize = 1;
const MAX_PER_FILE_CHUNKS: usize = 16;
/// 自适应模式下的初始并发(从较小值起步,由控制器按投递率爬升)。
const INITIAL_CONCURRENCY: usize = 4;
/// 自适应模式下的并发下限。
const MIN_ADAPTIVE_CONCURRENCY: usize = 2;
/// 自适应并发的投递率采样间隔。
const ADAPT_SAMPLE: Duration = Duration::from_secs(2);
/// 自动重试的首次退避与上限(指数增长:5s → 10s → 20s → …封顶 5 分钟)。
const AUTO_RETRY_BASE: Duration = Duration::from_secs(5);
const AUTO_RETRY_MAX: Duration = Duration::from_secs(300);
/// 自动重试的次数上限(超过后停在失败态,等用户手动处理)。
const AUTO_RETRY_LIMIT: u32 = 20;
/// 全局页面抓取流预算:多文件并行时避免 3×16=48 条流触发 CDN 侧限流。
const GLOBAL_MAX_STREAMS: usize = 24;
/// BDP 分块的档位上下限(KiB):下限摊薄请求开销,上限兼顾页面内存、IPC 拷贝
/// 与末段进度粒度。
const MIN_ADAPTIVE_CHUNK_KIB: usize = 256;
const MAX_ADAPTIVE_CHUNK_KIB: usize = 1024;
/// 请求超时(秒),随计划返回给页面 JS。
const MIN_TIMEOUT_SECONDS: u64 = 5;
const MAX_TIMEOUT_SECONDS: u64 = 600;
/// 页面侧块级重试次数上限。
const MAX_RETRIES: u32 = 20;
/// 进度广播缓冲(只用来置脏标记,容量小即可)。
const PROGRESS_EVENT_BUFFER: usize = 8;
/// 同时下载的文件数硬上限。
const MAX_FILE_SLOTS: usize = 12;
/// 管理动作的日志目标。
const DOWNLOAD_LOG_TARGET: &str = "download";

/// 页面需要补齐的一个分块区间。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkRange {
    pub offset: u64,
    pub length: u64,
}

/// `plan_chunks` 的返回:除缺块清单外,并发/超时/重试参数决定页面侧抓取行为。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanInfo {
    pub chunk_size_bytes: u64,
    /// 页面线程池的初始宽度(自适应模式下从较小值起步爬升)。
    pub concurrency: usize,
    /// 并发上限(自适应模式下控制器不会越过它;页面用它兜底钳制事件值)。
    pub max_concurrency: usize,
    pub timeout_seconds: u64,
    pub retries: u32,
    pub missing: Vec<ChunkRange>,
}

/// 全局页面抓取流预算(所有任务在途分块流的总和上限)。
///
/// 每个文件的自适应控制器只看得见自己:三个文件各自爬满 16 路就是 48 条流,
/// 足以触发服务端限流,之后再集体退避来回震荡。这里做一个进程内的总闸,
/// 增长申请超预算时不放行(控制器会在下一轮无增益采样里自行撤回探测)。
struct StreamBudget {
    cap: usize,
    used: AtomicUsize,
}

impl StreamBudget {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            used: AtomicUsize::new(0),
        }
    }

    /// 最多占用 `n` 条流,返回实际占用数(可能少于请求,甚至为 0)。
    fn acquire_up_to(&self, n: usize) -> usize {
        let mut current = self.used.load(Ordering::Relaxed);
        loop {
            let take = n.min(self.cap.saturating_sub(current));
            if take == 0 {
                return 0;
            }
            match self.used.compare_exchange_weak(
                current,
                current + take,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return take,
                Err(actual) => current = actual,
            }
        }
    }

    fn release(&self, n: usize) {
        let mut current = self.used.load(Ordering::Acquire);
        loop {
            let next = current.saturating_sub(n);
            match self.used.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }
}

/// 进度监视器使用的自适应运行时:控制器 + 全局流预算 + 本任务已占用的流数。
struct AdaptiveRuntime {
    controller: AdaptiveConcurrency,
    budget: Arc<StreamBudget>,
    reserved: usize,
}

impl AdaptiveRuntime {
    /// 应用控制器给出的新宽度:增长需向全局预算申请额度,退避即释放。
    /// 申请不到时不放行(控制器会在下一轮无增益采样里自行撤回探测)。
    fn apply(&mut self, width: usize) -> Option<usize> {
        if width > self.reserved {
            let granted = self.budget.acquire_up_to(width - self.reserved);
            if granted == 0 {
                return None;
            }
            self.reserved += granted;
            Some(self.reserved)
        } else if width < self.reserved {
            let freed = self.reserved - width;
            self.budget.release(freed);
            self.reserved = width;
            Some(width)
        } else {
            Some(width)
        }
    }

    /// 任务结束(完成/暂停/失败):把占用的流额度还给全局预算。
    fn release_all(&mut self) {
        self.budget.release(self.reserved);
        self.reserved = 0;
    }
}

/// 一个正在下载中的任务:分块入队通道、writer/进度监视句柄、活动时间与占用的文件槽位。
struct ActiveDownload {
    sender: mpsc::Sender<IncomingChunk>,
    writer: JoinHandle<Result<u64>>,
    watcher: JoinHandle<()>,
    /// 最后活动时间(std Mutex:只在同步短临界区内读写)。
    last_activity: Arc<Mutex<Instant>>,
    total: u64,
    /// `plan_chunks` 获取的文件槽位;随本结构体一并释放。
    _slot: OwnedSemaphorePermit,
}

pub struct DownloadManager {
    shared: Arc<SharedState>,
    slots: Arc<Semaphore>,
    active: Arc<AsyncMutex<HashMap<String, ActiveDownload>>>,
    budget: Arc<StreamBudget>,
}

impl DownloadManager {
    /// 构造管理器并启动看门狗。不自动入队:WebView 任务由页面探明大小后调用 `plan`。
    pub async fn start(shared: Arc<SharedState>) -> Result<Self> {
        let limit = {
            let settings = shared.settings.read().await;
            settings.concurrency.max_files.clamp(1, MAX_FILE_SLOTS)
        };
        let manager = Self {
            shared,
            slots: Arc::new(Semaphore::new(limit)),
            active: Arc::new(AsyncMutex::new(HashMap::new())),
            budget: Arc::new(StreamBudget::new(GLOBAL_MAX_STREAMS)),
        };
        manager.spawn_watchdog();
        Ok(manager)
    }

    /// 登记一个 WebView 下载任务:校验文件名、应用重复策略、建立 `queued` 记录。
    pub async fn create(
        &self,
        file_name: &str,
        file_type: &str,
        source: &str,
    ) -> Result<TaskRecord> {
        let name = validate_file_name(file_name)?;
        let sanitized = safe_component(&name);
        let settings = self.shared.settings.read().await.clone();
        if settings.download_root.trim().is_empty() {
            bail!("请先在设置中配置下载目录");
        }
        let root = PathBuf::from(&settings.download_root);
        let target = resolve_download_target(&root, &sanitized, &settings.duplicate_policy)?;
        let record = TaskRecord {
            task_id: uuid::Uuid::new_v4().to_string(),
            // 页面抓取的任务没有 Telegram 消息上下文;列约束要求非空,写入空串。
            chat_id: String::new(),
            chat_title: None,
            message_id: None,
            media_type: Some(normalize_file_type(file_type)),
            file_name: Some(sanitized.clone()),
            status: "queued".into(),
            progress: 0.0,
            downloaded_bytes: 0,
            total_bytes: None,
            speed_bytes_per_second: 0,
            remaining_bytes: None,
            started_at: None,
            updated_at: Some(chrono::Utc::now().to_rfc3339()),
            completed_at: None,
            output_path: Some(target.to_string_lossy().into_owned()),
            error: None,
            retry_count: 0,
            group_id: None,
        };
        self.shared.store.create(&record).await?;
        self.shared.publish_task(&record.task_id).await;
        self.emit(
            "webview-task-submitted",
            serde_json::json!({ "taskId": record.task_id }),
        );
        self.shared
            .log(
                "info",
                DOWNLOAD_LOG_TARGET,
                format!(
                    "已创建网页下载任务 {task_id}:{sanitized}(来源:{summary})",
                    task_id = record.task_id,
                    summary = summarize_source(source)
                ),
            )
            .await;
        Ok(record)
    }

    /// 页面探明文件大小后排定分块计划:占文件槽位、配置分块表、启动 writer,返回缺块清单。
    ///
    /// 幂等:任务已在下载中时只返回当前缺块清单,不重复占槽位或启动 writer。
    pub async fn plan(
        &self,
        task_id: &str,
        total_bytes: u64,
        probe_rtt_ms: Option<u64>,
    ) -> Result<PlanInfo> {
        if total_bytes == 0 {
            bail!("文件大小无效,无法规划分块下载");
        }
        let record = self.require_record(task_id).await?;
        if !matches!(record.status.as_str(), "queued" | "paused" | "downloading") {
            bail!("任务当前状态不允许开始下载:{}", record.status);
        }
        ensure_total_matches(record.total_bytes, total_bytes)?;
        let settings = self.shared.settings.read().await.clone();
        // 分块大小:自适应模式按 BDP(单路峰值速率 × 探测 RTT)选档,无学习数据时
        // 退回设置值;固定模式始终用设置值。分块小于单路 BDP 时,一条流会在
        // "等下一块"的空档里丢掉带宽。
        let adaptive_chunk_kib = if settings.concurrency.adaptive {
            bdp_chunk_size_kib(
                settings.concurrency.learned_per_stream_bytes_per_second,
                probe_rtt_ms.unwrap_or(0),
            )
        } else {
            0
        };
        let chunk_size_kib = if adaptive_chunk_kib > 0 {
            adaptive_chunk_kib
        } else {
            supported_chunk_size_kib(settings.concurrency.chunk_size_kib)
        };
        let chunk_size_bytes = chunk_size_kib as u64 * 1024;
        let max_concurrency = settings
            .concurrency
            .per_file_chunks
            .clamp(MIN_PER_FILE_CHUNKS, MAX_PER_FILE_CHUNKS);
        let adaptive_enabled = settings.concurrency.adaptive;
        // 自适应模式从较小并发起步,由控制器按投递率爬升;固定模式直接用设置值。
        let concurrency = if adaptive_enabled {
            max_concurrency.min(INITIAL_CONCURRENCY)
        } else {
            max_concurrency
        };
        let parameters = PlanInfo {
            chunk_size_bytes,
            concurrency,
            max_concurrency,
            timeout_seconds: settings
                .concurrency
                .request_timeout_seconds
                .clamp(MIN_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS),
            retries: settings.concurrency.retries.clamp(0, MAX_RETRIES),
            missing: Vec::new(),
        };
        let active_activity = {
            let active = self.active.lock().await;
            active
                .get(task_id)
                .map(|entry| Arc::clone(&entry.last_activity))
        };
        if let Some(last_activity) = active_activity {
            // 已在下载中:刷新活动时间并返回当前缺块,由既有 writer 落盘。
            *last_activity
                .lock()
                .map_err(|_| anyhow!("任务活动时间读取失败"))? = Instant::now();
            let missing = missing_chunk_ranges(&self.shared.store, task_id).await?;
            return Ok(PlanInfo {
                missing,
                ..parameters
            });
        }
        // 排队中的任务在这里等待空闲槽位。
        let slot = Arc::clone(&self.slots)
            .acquire_owned()
            .await
            .map_err(|_| anyhow!("下载槽位已关闭;任务保留在本机,可重启客户端恢复"))?;
        let requested_resume = record.status == "paused";
        let record = self.require_record(task_id).await?;
        // 等槽位期间任务可能已被暂停/取消;只有“调用前就是 paused”的续传重入才继续,
        // 否则释放槽位干净退出,不再偷偷启动一个用户已经停下的任务。
        let startable = match record.status.as_str() {
            "queued" | "downloading" => true,
            "paused" => requested_resume,
            _ => false,
        };
        if !startable {
            bail!("任务在等待下载槽位期间已暂停或取消:{}", record.status);
        }
        ensure_total_matches(record.total_bytes, total_bytes)?;
        let output = PathBuf::from(record.output_path.as_deref().context("任务目标路径为空")?);
        let temp = temporary_output_path(&output, task_id);
        if let Some(parent) = temp.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        chunk_writer::configure_chunk_map(
            &self.shared.store,
            task_id,
            total_bytes,
            chunk_size_bytes,
        )
        .await?;
        chunk_writer::verify_resumable_chunks(&self.shared.store, task_id, &temp).await?;
        let missing = missing_chunk_ranges(&self.shared.store, task_id).await?;
        self.shared
            .store
            .set_total_bytes(task_id, total_bytes)
            .await?;
        match record.status.as_str() {
            "paused" => {
                // 状态机只允许 paused → queued → downloading。
                self.shared
                    .store
                    .set_status(task_id, "queued", None)
                    .await?;
                self.shared
                    .store
                    .set_status(task_id, "downloading", None)
                    .await?;
            }
            "downloading" => {}
            _ => {
                self.shared
                    .store
                    .set_status(task_id, "downloading", None)
                    .await?;
            }
        }
        // 检查与插入必须一次持锁完成:并发的第二次 plan 若在此前通过检查,会启动第二个
        // writer 覆盖同一个临时文件,并让槽位重复释放。锁只覆盖同步的 spawn 与插入,
        // 槽位获取在锁外。
        let mut active = self.active.lock().await;
        if active.contains_key(task_id) {
            drop(active);
            let missing = missing_chunk_ranges(&self.shared.store, task_id).await?;
            return Ok(PlanInfo {
                missing,
                ..parameters
            });
        }
        let (sender, receiver) =
            mpsc::channel(chunk_queue_depth(settings.concurrency.per_file_chunks));
        let (progress_events, progress_receiver) = broadcast::channel(PROGRESS_EVENT_BUFFER);
        let writer_store = self.shared.store.clone();
        let writer_task_id = task_id.to_owned();
        let writer_temp = temp;
        let writer = tokio::spawn(async move {
            chunk_writer::write_chunks_bounded(
                writer_store,
                writer_task_id,
                &writer_temp,
                total_bytes,
                receiver,
                progress_events,
            )
            .await
        });
        // 全局流预算:按占用到的额度决定页面初始宽度(至少 1 路,避免任务卡死)。
        let reserved_streams = self.budget.acquire_up_to(concurrency);
        let initial_width = reserved_streams.max(1);
        let adaptive_runtime = adaptive_enabled.then(|| AdaptiveRuntime {
            controller: AdaptiveConcurrency::new(
                MIN_ADAPTIVE_CONCURRENCY,
                initial_width,
                max_concurrency,
            ),
            budget: Arc::clone(&self.budget),
            reserved: reserved_streams,
        });
        let watcher = spawn_progress_watcher(
            Arc::clone(&self.shared),
            task_id.to_owned(),
            total_bytes,
            progress_receiver,
            adaptive_runtime,
        );
        active.insert(
            task_id.to_owned(),
            ActiveDownload {
                sender,
                writer,
                watcher,
                last_activity: Arc::new(Mutex::new(Instant::now())),
                total: total_bytes,
                _slot: slot,
            },
        );
        drop(active);
        // writer 首轮刷新与 350ms 节流器会立即补上 `webview-task-updated`。
        self.shared.publish_task(task_id).await;
        self.shared
            .log(
                "info",
                DOWNLOAD_LOG_TARGET,
                format!(
                    "任务 {task_id} 开始分块下载,共 {total_bytes} 字节,缺 {missing_len} 块;分块 {chunk_size_kib} KiB × 并发 {initial_width}(上限 {max_concurrency})",
                    missing_len = missing.len()
                ),
            )
            .await;
        Ok(PlanInfo {
            concurrency: initial_width,
            missing,
            ..parameters
        })
    }

    /// 页面抓取到的分块入队。mpsc 满时 `send` 阻塞,形成自然背压。
    pub async fn push(&self, task_id: &str, offset: u64, bytes: Vec<u8>) -> Result<()> {
        if bytes.is_empty() {
            bail!("分块数据为空");
        }
        if bytes.len() > CHUNK_HARD_CAP {
            bail!("单个分块超过 {CHUNK_HARD_CAP} 字节上限");
        }
        let (sender, last_activity, total) = {
            let active = self.active.lock().await;
            let entry = active.get(task_id).context("任务不在活动状态")?;
            (
                entry.sender.clone(),
                Arc::clone(&entry.last_activity),
                entry.total,
            )
        };
        let end = offset
            .checked_add(bytes.len() as u64)
            .context("分块偏移溢出")?;
        if end > total {
            bail!("分块超出文件大小");
        }
        *last_activity
            .lock()
            .map_err(|_| anyhow!("任务活动时间读取失败"))? = Instant::now();
        let queued_at = Instant::now();
        sender
            .send(IncomingChunk {
                offset,
                data: bytes,
            })
            .await
            .map_err(|_| anyhow!("下载已结束或中断"))?;
        let waited = queued_at.elapsed();
        if waited >= SLOW_PUSH_WARN {
            self.shared
                .log(
                    "warn",
                    DOWNLOAD_LOG_TARGET,
                    format!(
                        "任务 {task_id} 分块入队等待 {:.1}s:写入端疑似停滞",
                        waited.as_secs_f64()
                    ),
                )
                .await;
        }
        Ok(())
    }

    /// 页面侧心跳:抓取在途(可能长时间收不到完整分块)时由页面定期调用。
    ///
    /// 看门狗把"30 秒无 push"当作页面已消失;慢而健康的连接会因此被误暂停,
    /// 心跳把"页面仍在工作"这一事实补充给它。任务已结束时不报错(心跳是尽力而为)。
    pub async fn heartbeat(&self, task_id: &str) -> Result<()> {
        let active = self.active.lock().await;
        let Some(entry) = active.get(task_id) else {
            return Ok(());
        };
        *entry
            .last_activity
            .lock()
            .map_err(|_| anyhow!("任务活动时间读取失败"))? = Instant::now();
        Ok(())
    }

    /// 全部块抓取完成后调用:校验分块完整并原子提交到目标路径。
    ///
    /// 缺块或提交失败时走与 [`Self::fail`] 相同的失败路径(保留临时文件以便续传)。
    pub async fn finish(&self, task_id: &str) -> Result<TaskRecord> {
        let Some(active) = self.detach(task_id).await else {
            // 幂等:重复 finish(典型是成功后重放)直接返回当前记录。
            let record = self.require_record(task_id).await?;
            if record.status == "completed" {
                return Ok(record);
            }
            bail!("任务不在活动状态,无法完成:{task_id}");
        };
        let total = active.total;
        let writer_result = drain_active(active).await;
        let output = match self.output_path(task_id).await {
            Ok(output) => output,
            Err(error) => {
                return self
                    .mark_failed(task_id, &safe_error(&format!("{error:#}")))
                    .await;
            }
        };
        let temp = temporary_output_path(&output, task_id);
        let chunks = self.shared.store.chunk_map(task_id).await?;
        let failure = if !chunk_map_is_complete(&chunks, total) {
            Some("分块未完成,请重新打开媒体补齐".to_owned())
        } else {
            writer_result
                .err()
                .map(|error| safe_error(&format!("{error:#}")))
        };
        if let Some(reason) = failure {
            return self.mark_failed(task_id, &reason).await;
        }
        let overwrite = self
            .shared
            .settings
            .read()
            .await
            .duplicate_policy
            .eq("overwrite");
        if let Err(error) = complete_download(
            &self.shared.store,
            task_id,
            total,
            &temp,
            &output,
            overwrite,
        )
        .await
        {
            return self
                .mark_failed(task_id, &safe_error(&format!("{error:#}")))
                .await;
        }
        self.shared.store.update_progress(task_id, total, 0).await?;
        self.shared
            .store
            .set_status(task_id, "completed", None)
            .await?;
        self.shared.publish_task(task_id).await;
        self.emit(
            "webview-task-completed",
            serde_json::json!({
                "taskId": task_id,
                "outputPath": output.to_string_lossy(),
            }),
        );
        let record = self.require_record(task_id).await?;
        self.shared
            .log(
                "info",
                DOWNLOAD_LOG_TARGET,
                format!(
                    "任务 {task_id} 已校验并原子提交:{}{}",
                    output.display(),
                    completion_summary(record.started_at.as_deref(), total)
                ),
            )
            .await;
        Ok(record)
    }

    /// 页面侧失败(URL 失效、网络中断、抓取停滞等):保留已校验分块,标记 `failed`。
    ///
    /// `permanent` 为真表示重试不会有帮助(URL 过期/文件已删除),此时不做自动重试。
    pub async fn fail(&self, task_id: &str, error: &str, permanent: bool) -> Result<TaskRecord> {
        if let Some(active) = self.detach(task_id).await {
            // writer 的错误只反映本地收尾细节;页面侧已给出更准确的失败原因。
            let _ = drain_active(active).await;
        }
        let record = self.mark_failed(task_id, error).await?;
        self.maybe_schedule_auto_retry(&record, permanent);
        Ok(record)
    }

    /// 失败任务按指数退避自动重新排队(网络抖动恢复后无需用户干预)。
    ///
    /// 到点前任务若被取消/删除/手动重试,状态不再是 `failed`,本次调度自然作废。
    fn maybe_schedule_auto_retry(&self, record: &TaskRecord, permanent: bool) {
        if permanent || record.status != "failed" {
            return;
        }
        let attempt = record.retry_count.saturating_add(1);
        if attempt > AUTO_RETRY_LIMIT {
            return;
        }
        let shared = Arc::clone(&self.shared);
        let task_id = record.task_id.clone();
        let file_name = record.file_name.clone();
        tokio::spawn(async move {
            let enabled = shared.settings.read().await.concurrency.auto_retry;
            if !enabled {
                return;
            }
            let delay = auto_retry_delay(attempt);
            shared
                .log(
                    "info",
                    DOWNLOAD_LOG_TARGET,
                    format!(
                        "任务 {task_id} 将在 {:.0} 秒后自动重试(第 {attempt} 次)",
                        delay.as_secs_f64()
                    ),
                )
                .await;
            tokio::time::sleep(delay).await;
            let Ok(Some(current)) = shared.store.get(&task_id).await else {
                return;
            };
            if current.status != "failed" {
                return; // 用户已取消/删除/手动重试
            }
            if let Err(error) = shared.store.set_status(&task_id, "queued", None).await {
                shared
                    .log(
                        "warn",
                        DOWNLOAD_LOG_TARGET,
                        format!("任务 {task_id} 自动重试排队失败:{error:#}"),
                    )
                    .await;
                return;
            }
            shared.publish_task(&task_id).await;
            if let Some(file_name) = file_name.filter(|name| !name.is_empty()) {
                let _ = shared.app.emit(
                    "webview-resume-request",
                    serde_json::json!({ "taskId": task_id, "fileName": file_name }),
                );
            }
        });
    }

    /// 删除任务记录:活动任务先按取消路径停住,再清理临时分块,最后删库。
    ///
    /// `delete_file` 只对已提交的输出文件生效,且要求文件位于下载目录内(防止任务记录
    /// 被篡改后越界删除);临时分块文件总是清理 —— 删除的语义是"这条任务不再存在"。
    pub async fn delete(&self, task_id: &str, delete_file: bool) -> Result<()> {
        let record = self.require_record(task_id).await?;
        if let Some(active) = self.detach(task_id).await {
            announce_abort(&self.shared, task_id);
            if let Err(error) = drain_active(active).await {
                self.shared
                    .log(
                        "warn",
                        DOWNLOAD_LOG_TARGET,
                        format!("任务 {task_id} 写出器退出:{error:#}"),
                    )
                    .await;
            }
        }
        if let Some(output) = record.output_path.as_deref().map(PathBuf::from) {
            let temp = temporary_output_path(&output, task_id);
            if let Err(error) = tokio::fs::remove_file(&temp).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                self.shared
                    .log(
                        "warn",
                        DOWNLOAD_LOG_TARGET,
                        format!("任务 {task_id} 临时分块清理失败:{error}"),
                    )
                    .await;
            }
            if delete_file && output.exists() {
                if !self.output_within_download_root(&output).await? {
                    bail!("任务目标不在下载目录内,已拒绝删除文件");
                }
                tokio::fs::remove_file(&output)
                    .await
                    .with_context(|| format!("删除文件失败:{}", output.display()))?;
            }
        }
        self.shared.store.delete(task_id).await?;
        if let Ok(stats) = self.shared.store.stats().await {
            let _ = self
                .shared
                .app
                .emit("stats-updated", serde_json::json!({ "stats": stats }));
        }
        self.shared
            .log(
                "info",
                DOWNLOAD_LOG_TARGET,
                format!(
                    "任务 {task_id} 已删除{file_note}",
                    file_note = if delete_file {
                        "(含已下载文件)"
                    } else {
                        ""
                    }
                ),
            )
            .await;
        Ok(())
    }

    /// 输出文件是否位于下载目录内(删除文件前的安全检查)。
    async fn output_within_download_root(&self, output: &Path) -> Result<bool> {
        let settings = self.shared.settings.read().await.clone();
        let Ok(root) = PathBuf::from(&settings.download_root).canonicalize() else {
            return Ok(false);
        };
        let Some(parent) = output.parent() else {
            return Ok(false);
        };
        let Ok(parent) = parent.canonicalize() else {
            return Ok(false);
        };
        Ok(parent.starts_with(&root))
    }

    /// 任务页操作:`pause` / `resume` / `cancel` / `retry`。
    pub async fn action(&self, task_id: &str, action: &str) -> Result<TaskRecord> {
        match action {
            "pause" => pause_task(&self.shared, &self.active, task_id, "用户暂停").await,
            "resume" => {
                let record = self.require_record(task_id).await?;
                if record.status != "paused" {
                    bail!("只有已暂停的任务可以继续");
                }
                self.shared
                    .store
                    .set_status(task_id, "queued", None)
                    .await?;
                self.shared.publish_task(task_id).await;
                self.shared
                    .log(
                        "info",
                        DOWNLOAD_LOG_TARGET,
                        format!("任务 {task_id} 已排队;重新打开媒体后按缺块续传"),
                    )
                    .await;
                self.request_page_resume(task_id, record.file_name.as_deref());
                self.require_record(task_id).await
            }
            "cancel" => cancel_task(&self.shared, &self.active, task_id).await,
            "retry" => {
                let record = self.require_record(task_id).await?;
                if !matches!(record.status.as_str(), "failed" | "cancelled") {
                    bail!("只有失败或已取消的任务可以重试");
                }
                self.shared
                    .store
                    .set_status(task_id, "queued", None)
                    .await?;
                self.shared.publish_task(task_id).await;
                self.shared
                    .log(
                        "info",
                        DOWNLOAD_LOG_TARGET,
                        format!("任务 {task_id} 已重新排队"),
                    )
                    .await;
                self.request_page_resume(task_id, record.file_name.as_deref());
                self.require_record(task_id).await
            }
            _ => bail!("不支持的任务操作"),
        }
    }

    /// 每 10 秒巡检:下载中且长时间无 push/finish 的任务按“暂停”处理。
    fn spawn_watchdog(&self) {
        let shared = Arc::clone(&self.shared);
        let active = Arc::clone(&self.active);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(WATCHDOG_TICK);
            loop {
                tick.tick().await;
                let stale = {
                    let active_map = active.lock().await;
                    active_map
                        .iter()
                        .filter(|(_, entry)| {
                            entry
                                .last_activity
                                .lock()
                                .map(|at| at.elapsed() >= ACTIVITY_TIMEOUT)
                                .unwrap_or(false)
                        })
                        .map(|(task_id, _)| task_id.clone())
                        .collect::<Vec<_>>()
                };
                for task_id in stale {
                    if let Err(error) =
                        pause_task(&shared, &active, &task_id, "超过 30 秒无下载活动").await
                    {
                        shared
                            .log(
                                "warn",
                                DOWNLOAD_LOG_TARGET,
                                format!("看门狗暂停任务 {task_id} 失败:{error:#}"),
                            )
                            .await;
                    }
                }
            }
        });
    }

    async fn detach(&self, task_id: &str) -> Option<ActiveDownload> {
        self.active.lock().await.remove(task_id)
    }

    async fn require_record(&self, task_id: &str) -> Result<TaskRecord> {
        self.shared
            .store
            .get(task_id)
            .await?
            .with_context(|| format!("未找到下载任务:{task_id}"))
    }

    async fn output_path(&self, task_id: &str) -> Result<PathBuf> {
        let record = self.require_record(task_id).await?;
        record
            .output_path
            .as_deref()
            .map(PathBuf::from)
            .context("任务目标路径为空")
    }

    /// 标记失败:幂等,已失败的任务直接返回,其它终态拒绝。
    async fn mark_failed(&self, task_id: &str, error: &str) -> Result<TaskRecord> {
        let detail = safe_error(error);
        let record = self.require_record(task_id).await?;
        match record.status.as_str() {
            "failed" => {}
            "queued" | "downloading" | "paused" => {
                self.shared
                    .store
                    .set_status(task_id, "failed", Some(&detail))
                    .await?;
            }
            other => bail!("任务当前状态无法标记失败:{other}"),
        }
        self.shared.publish_task(task_id).await;
        self.emit(
            "webview-task-failed",
            serde_json::json!({ "taskId": task_id, "error": detail }),
        );
        self.shared
            .log(
                "error",
                DOWNLOAD_LOG_TARGET,
                format!("任务 {task_id} 失败:{detail}"),
            )
            .await;
        self.require_record(task_id).await
    }

    fn emit(&self, event: &str, payload: serde_json::Value) {
        let _ = self.shared.app.emit(event, payload);
    }

    /// 通知页面"这个文件可以继续抓了":媒体仍打开、或本会话还缓存着该文件的 URL 时,
    /// 页面会直接续传;两者都不满足则页面保持安静(客户端提示用户重新打开媒体)。
    fn request_page_resume(&self, task_id: &str, file_name: Option<&str>) {
        let Some(file_name) = file_name.filter(|name| !name.is_empty()) else {
            return;
        };
        self.emit(
            "webview-resume-request",
            serde_json::json!({ "taskId": task_id, "fileName": file_name }),
        );
    }
}

/// 在活动表上执行暂停:停止 writer/进度监视,状态置 `paused`,并通知页面中止抓取。
async fn pause_task(
    shared: &SharedState,
    active: &AsyncMutex<HashMap<String, ActiveDownload>>,
    task_id: &str,
    reason: &str,
) -> Result<TaskRecord> {
    let detached = active.lock().await.remove(task_id);
    if let Some(entry) = detached {
        // 写出器的错误以前在这里被丢弃;它是"任务为何停住"的关键证据,必须留痕。
        if let Err(error) = drain_active(entry).await {
            shared
                .log(
                    "warn",
                    DOWNLOAD_LOG_TARGET,
                    format!("任务 {task_id} 写出器退出:{error:#}"),
                )
                .await;
        }
    }
    let record = require_record(shared, task_id).await?;
    match record.status.as_str() {
        "queued" | "downloading" => {
            shared.store.set_status(task_id, "paused", None).await?;
        }
        "paused" => {}
        other => bail!("该任务当前状态不能暂停:{other}"),
    }
    announce_abort(shared, task_id);
    shared.publish_task(task_id).await;
    shared
        .log(
            "info",
            DOWNLOAD_LOG_TARGET,
            format!("任务 {task_id} 已暂停({reason})"),
        )
        .await;
    require_record(shared, task_id).await
}

/// 取消任务:与暂停相同的收尾,但状态置 `cancelled`,并按设置清理临时分块文件。
async fn cancel_task(
    shared: &SharedState,
    active: &AsyncMutex<HashMap<String, ActiveDownload>>,
    task_id: &str,
) -> Result<TaskRecord> {
    let record = require_record(shared, task_id).await?;
    if !matches!(record.status.as_str(), "queued" | "downloading" | "paused") {
        bail!("该任务当前状态不能取消:{}", record.status);
    }
    let detached = active.lock().await.remove(task_id);
    if let Some(entry) = detached
        && let Err(error) = drain_active(entry).await
    {
        shared
            .log(
                "warn",
                DOWNLOAD_LOG_TARGET,
                format!("任务 {task_id} 写出器退出:{error:#}"),
            )
            .await;
    }
    shared.store.set_status(task_id, "cancelled", None).await?;
    announce_abort(shared, task_id);
    let settings = shared.settings.read().await.clone();
    if !settings.preserve_partial_files
        && let Some(output) = record.output_path.as_deref()
    {
        let temp = temporary_output_path(Path::new(output), task_id);
        let _ = tokio::fs::remove_file(temp).await;
    }
    shared.publish_task(task_id).await;
    shared
        .log(
            "info",
            DOWNLOAD_LOG_TARGET,
            format!("任务 {task_id} 已取消"),
        )
        .await;
    require_record(shared, task_id).await
}

async fn require_record(shared: &SharedState, task_id: &str) -> Result<TaskRecord> {
    shared
        .store
        .get(task_id)
        .await?
        .with_context(|| format!("未找到下载任务:{task_id}"))
}

/// 分离在途下载:先丢弃发送端让 writer 观察到通道关闭,再等 writer 落盘完毕;
/// 槽位保持到收尾结束才随结构体释放。
///
/// 不变式:sender 必须在 join writer 之前丢弃,且每条分离路径(finish/fail/pause/
/// cancel/看门狗)都只经过这里 —— 否则 writer 会一直阻塞在 `recv()` 上,join 永不返回。
async fn drain_active(active: ActiveDownload) -> Result<u64> {
    let ActiveDownload {
        sender,
        writer,
        watcher,
        _slot,
        ..
    } = active;
    drop(sender);
    join_download_children(writer, watcher).await
}

fn announce_abort(shared: &SharedState, task_id: &str) {
    let _ = shared.app.emit(
        "webview-download-abort",
        serde_json::json!({ "taskId": task_id }),
    );
}

/// 节流后的进度广播:刷新任务页并通知页面按钮;同时驱动自适应并发采样。
///
/// 自适应采样用"窗口内新增完成字节 / 窗口时长"作为投递率(BBR 式模型:看速率本身,
/// 不看丢包/重试)。决策在 Rust,页面只按 `webview-task-concurrency` 事件调整线程池宽度。
fn spawn_progress_watcher(
    shared: Arc<SharedState>,
    task_id: String,
    total_bytes: u64,
    mut progress_events: broadcast::Receiver<()>,
    mut adaptive: Option<AdaptiveRuntime>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut dirty = false;
        let mut tick = tokio::time::interval(PROGRESS_TICK);
        let mut sample_started = Instant::now();
        let mut sample_bytes = shared
            .store
            .get(&task_id)
            .await
            .ok()
            .flatten()
            .map(|record| record.downloaded_bytes)
            .unwrap_or(0);
        let mut active_width = adaptive.as_ref().map(|rt| rt.reserved.max(1)).unwrap_or(0);
        // 单路峰值速率:峰值的 per-stream 投递率,作为下一个任务 BDP 分块的输入。
        let mut peak_per_stream = 0.0_f64;
        loop {
            tokio::select! {
                event = progress_events.recv() => match event {
                    Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => dirty = true,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = tick.tick() => {
                    if dirty {
                        dirty = false;
                        publish_progress(&shared, &task_id, total_bytes).await;
                    }
                    if let Some(runtime) = adaptive.as_mut() {
                        if sample_started.elapsed() >= ADAPT_SAMPLE {
                            let elapsed = sample_started.elapsed();
                            sample_started = Instant::now();
                            if let Ok(Some(record)) = shared.store.get(&task_id).await {
                                let delta = record.downloaded_bytes.saturating_sub(sample_bytes);
                                sample_bytes = record.downloaded_bytes;
                                let rate = delta as f64 / elapsed.as_secs_f64();
                                if rate > 0.0 && active_width > 0 {
                                    peak_per_stream = peak_per_stream.max(rate / active_width as f64);
                                }
                                if let Some(width) = runtime.controller.sample(Instant::now(), rate)
                                    && let Some(applied) = runtime.apply(width)
                                {
                                    active_width = applied;
                                    let _ = shared.app.emit(
                                        "webview-task-concurrency",
                                        serde_json::json!({
                                            "taskId": task_id,
                                            "concurrency": applied,
                                        }),
                                    );
                                    shared
                                        .log(
                                            "info",
                                            DOWNLOAD_LOG_TARGET,
                                            format!(
                                                "任务 {task_id} 自适应并发调整为 {applied} 路(窗口速率 {:.1} MB/s)",
                                                rate / 1_048_576.0
                                            ),
                                        )
                                        .await;
                                }
                            }
                        }
                    }
                },
            }
        }
        if let Some(runtime) = adaptive.as_mut() {
            runtime.release_all();
            if peak_per_stream > 0.0 {
                persist_learned_rate(&shared, peak_per_stream as u64).await;
            }
        }
        if let Ok(Some(record)) = shared.store.get(&task_id).await {
            let progress = progress_ratio(record.downloaded_bytes, total_bytes);
            emit_webview_progress(&shared, &task_id, progress).await;
        }
    })
}

/// 把实测的单路峰值速率写回设置,作为下一个任务 BDP 分块的输入。
async fn persist_learned_rate(shared: &Arc<SharedState>, bytes_per_second: u64) {
    let mut settings = shared.settings.write().await;
    if settings.concurrency.learned_per_stream_bytes_per_second == bytes_per_second {
        return;
    }
    settings.concurrency.learned_per_stream_bytes_per_second = bytes_per_second;
    let layout = shared.layout.read().await.clone();
    if let Err(error) = storage::save_settings(&layout, &settings) {
        tracing::warn!(target: "desktop::download", "学习速率持久化失败:{error:#}");
    }
}

async fn publish_progress(shared: &SharedState, task_id: &str, total_bytes: u64) {
    shared.publish_task(task_id).await;
    if let Ok(Some(record)) = shared.store.get(task_id).await {
        let progress = progress_ratio(record.downloaded_bytes, total_bytes);
        emit_webview_progress(shared, task_id, progress).await;
    }
}

async fn emit_webview_progress(shared: &SharedState, task_id: &str, progress: f64) {
    let _ = shared.app.emit(
        "webview-task-updated",
        serde_json::json!({ "taskId": task_id, "progress": progress }),
    );
}

fn progress_ratio(downloaded: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (downloaded as f64 / total as f64).clamp(0.0, 1.0)
    }
}

/// 完成日志后缀:";共 X MB,用时 Y 秒,平均 Z MB/s"。
/// 供用户判断是否跑满链路(无开始时间时返回空串)。
fn completion_summary(started_at: Option<&str>, total_bytes: u64) -> String {
    let Some(started) =
        started_at.and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
    else {
        return String::new();
    };
    let seconds = (chrono::Utc::now() - started.with_timezone(&chrono::Utc))
        .num_milliseconds()
        .max(0) as f64
        / 1000.0;
    if seconds <= 0.0 {
        return String::new();
    }
    let megabytes = total_bytes as f64 / 1_048_576.0;
    format!(
        ";共 {megabytes:.0} MB,用时 {seconds:.0} 秒,平均 {:.1} MB/s",
        megabytes / seconds
    )
}

async fn missing_chunk_ranges(store: &TaskStore, task_id: &str) -> Result<Vec<ChunkRange>> {
    Ok(store
        .chunk_map(task_id)
        .await?
        .into_iter()
        .filter(|(_, _, _, complete)| !complete)
        .map(|(offset, length, _, _)| ChunkRange { offset, length })
        .collect())
}

/// 提交已完成的分块:先做“目标已是本任务产物”的恢复检查,再原子移动临时文件。
///
/// 独立于管理器的自由函数,便于在无 Tauri 运行时的单测里验证提交语义。
async fn complete_download(
    store: &TaskStore,
    task_id: &str,
    total_bytes: u64,
    temp_path: &Path,
    output: &Path,
    overwrite: bool,
) -> Result<()> {
    let chunks = store.chunk_map(task_id).await?;
    if !chunk_map_is_complete(&chunks, total_bytes) {
        bail!("分块未完成,请重新打开媒体补齐");
    }
    if verified_final_file_matches_chunks(store, task_id, output, total_bytes).await? {
        // 目标文件已是本任务的完整产物(提交后中断重入):无需再次覆盖。
        let _ = tokio::fs::remove_file(temp_path).await;
        return Ok(());
    }
    if !overwrite && tokio::fs::try_exists(output).await? {
        bail!(
            "目标文件已存在,且不匹配此任务已校验的分块;为避免覆盖文件,任务已停止:{}",
            output.display()
        );
    }
    chunk_writer::commit_completed_file(temp_path, output, overwrite)
}

async fn verified_final_file_matches_chunks(
    store: &TaskStore,
    task_id: &str,
    path: &Path,
    total_bytes: u64,
) -> Result<bool> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.len() != total_bytes {
        return Ok(false);
    }
    let chunks = store.chunk_map(task_id).await?;
    if !chunk_map_is_complete(&chunks, total_bytes) {
        return Ok(false);
    }
    let mut file = File::open(path).await?;
    for (offset, length, digest, _) in chunks {
        let Some(digest) = digest else {
            return Ok(false);
        };
        let mut bytes = vec![0_u8; length as usize];
        file.seek(SeekFrom::Start(offset)).await?;
        match file.read_exact(&mut bytes).await {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(false),
            Err(error) => return Err(error.into()),
        }
        if blake3::hash(&bytes).to_hex().as_str() != digest {
            return Ok(false);
        }
    }
    Ok(true)
}

fn chunk_map_is_complete(chunks: &[(u64, u64, Option<String>, bool)], total_bytes: u64) -> bool {
    if chunks.is_empty() {
        return false;
    }
    let mut expected_offset = 0_u64;
    for (offset, length, digest, complete) in chunks {
        if *offset != expected_offset
            || *length == 0
            || *length > MAX_CHUNK_BYTES
            || digest.is_none()
            || !complete
        {
            return false;
        }
        let Some(next) = expected_offset.checked_add(*length) else {
            return false;
        };
        expected_offset = next;
    }
    expected_offset == total_bytes
}

async fn join_download_children<T>(
    writer: JoinHandle<Result<T>>,
    watcher: JoinHandle<()>,
) -> Result<T> {
    watcher.abort();
    let _ = watcher.await;
    writer.await.context("分块写入线程异常退出")?
}

/// 校验页面提交的文件名:去空白,限制长度,拒绝控制字符。
fn validate_file_name(raw: &str) -> Result<String> {
    let name = raw.trim();
    if name.is_empty() {
        bail!("文件名不能为空");
    }
    if name.chars().count() > MAX_FILE_NAME_CHARS {
        bail!("文件名超过 {MAX_FILE_NAME_CHARS} 个字符");
    }
    if name.chars().any(char::is_control) {
        bail!("文件名包含控制字符");
    }
    Ok(name.to_owned())
}

fn normalize_file_type(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > MAX_FILE_TYPE_CHARS
        || trimmed.chars().any(char::is_control)
    {
        return "media".to_owned();
    }
    trimmed.to_owned()
}

fn summarize_source(source: &str) -> String {
    safe_error(source)
        .chars()
        .take(MAX_SOURCE_SUMMARY_CHARS)
        .collect()
}

/// 目标路径 = `download_root` + 净化后的文件名,再按重复策略决定跳过/改名/覆盖。
fn resolve_download_target(root: &Path, sanitized_name: &str, policy: &str) -> Result<PathBuf> {
    let target = root.join(sanitized_name);
    let Some(resolved) = apply_duplicate_policy(&target, policy)? else {
        bail!("目标文件已存在(按跳过策略)");
    };
    Ok(resolved)
}

fn apply_duplicate_policy(path: &Path, policy: &str) -> Result<Option<PathBuf>> {
    if !path.exists() || policy == "overwrite" {
        return Ok(Some(path.to_owned()));
    }
    if policy == "skip" {
        return Ok(None);
    }
    let parent = path.parent().context("目标路径无父目录")?;
    let name = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path
        .extension()
        .map(|value| value.to_string_lossy().into_owned());
    for suffix in 1..=MAX_DUPLICATE_NAME_ATTEMPTS {
        let candidate = parent.join(match &ext {
            Some(ext) => format!("{name} ({suffix}).{ext}"),
            None => format!("{name} ({suffix})"),
        });
        if !candidate.exists() {
            return Ok(Some(candidate));
        }
    }
    bail!("无法为重复文件生成不冲突的名称")
}

/// 页面探明的大小必须与本地记录一致,否则已有分块无法安全续传。
fn ensure_total_matches(recorded: Option<u64>, probed: u64) -> Result<()> {
    if let Some(recorded) = recorded
        && recorded != probed
    {
        bail!("文件大小与此前记录不符,停止续传以保护数据");
    }
    Ok(())
}

fn safe_component(value: &str) -> String {
    let mut clean = value
        .chars()
        .map(|ch| {
            if ch.is_control() || "<>:\"/\\|?*".contains(ch) {
                '_'
            } else {
                ch
            }
        })
        .collect::<String>();
    clean = clean.trim().trim_end_matches('.').to_owned();
    if clean.is_empty() || clean == "." || clean == ".." {
        "_".into()
    } else {
        clean.chars().take(180).collect()
    }
}

fn temporary_output_path(output: &Path, task_id: &str) -> PathBuf {
    output.with_file_name(format!(".telegram-media-{task_id}.part"))
}

fn chunk_queue_depth(per_file_chunks: usize) -> usize {
    per_file_chunks.clamp(MIN_PER_FILE_CHUNKS, MAX_PER_FILE_CHUNKS) * 2
}

fn supported_chunk_size_kib(requested: usize) -> usize {
    CHUNK_SIZE_KIB_CHOICES
        .into_iter()
        .min_by_key(|candidate| {
            candidate.abs_diff(requested.clamp(MIN_CHUNK_SIZE_KIB, MAX_CHUNK_SIZE_KIB))
        })
        .unwrap_or(512)
}

/// 由 BDP(单路峰值速率 × 探测 RTT)估算下个任务的分块大小,并就近对齐到受支持档位。///
/// 分块应不小于单路 BDP,否则一条流会在"等下一块"的空档里丢掉带宽;上限 1 MiB
/// 兼顾页面内存、IPC 拷贝开销与末段进度粒度。返回 0 表示没有学习数据可用。
fn bdp_chunk_size_kib(per_stream_bytes_per_second: u64, rtt_ms: u64) -> usize {
    if per_stream_bytes_per_second == 0 || rtt_ms == 0 {
        return 0;
    }
    let bdp_kib = (per_stream_bytes_per_second as f64 * rtt_ms as f64 / 1000.0 / 1024.0) as usize;
    supported_chunk_size_kib(bdp_kib.clamp(MIN_ADAPTIVE_CHUNK_KIB, MAX_ADAPTIVE_CHUNK_KIB))
}

/// 自动重试的退避时长:5s、10s、20s、40s… 封顶 5 分钟(attempt 从 1 开始)。
fn auto_retry_delay(attempt: u32) -> Duration {
    let factor = 1_u32 << attempt.saturating_sub(1).min(16);
    (AUTO_RETRY_BASE * factor).min(AUTO_RETRY_MAX)
}

fn safe_error(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control())
        .take(700)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn record(id: &str, output_path: &Path) -> TaskRecord {
        TaskRecord {
            task_id: id.into(),
            chat_id: String::new(),
            chat_title: None,
            message_id: None,
            media_type: Some("document".into()),
            file_name: Some("media.bin".into()),
            status: "queued".into(),
            progress: 0.0,
            downloaded_bytes: 0,
            total_bytes: None,
            speed_bytes_per_second: 0,
            remaining_bytes: None,
            started_at: None,
            updated_at: None,
            completed_at: None,
            output_path: Some(output_path.to_string_lossy().into_owned()),
            error: None,
            retry_count: 0,
            group_id: None,
        }
    }

    /// 写入内容并为每个分块登记 BLAKE3 摘要,模拟 writer 正常收尾后的现场。
    async fn seed_complete_task(
        store: &TaskStore,
        task_id: &str,
        output: &Path,
        contents: &[u8],
        chunk_size: u64,
    ) -> PathBuf {
        store.create(&record(task_id, output)).await.unwrap();
        chunk_writer::configure_chunk_map(store, task_id, contents.len() as u64, chunk_size)
            .await
            .unwrap();
        let temp = temporary_output_path(output, task_id);
        tokio::fs::write(&temp, contents).await.unwrap();
        for (index, bytes) in contents.chunks(chunk_size as usize).enumerate() {
            store
                .complete_chunk(
                    task_id,
                    index as u64 * chunk_size,
                    blake3::hash(bytes).to_hex().as_str(),
                )
                .await
                .unwrap();
        }
        temp
    }

    #[test]
    fn file_name_validation_rejects_empty_oversized_and_control_names() {
        assert!(validate_file_name("   ").is_err());
        assert!(validate_file_name("").is_err());
        assert_eq!(validate_file_name("  clip.mp4  ").unwrap(), "clip.mp4");
        assert!(validate_file_name(&"x".repeat(MAX_FILE_NAME_CHARS)).is_ok());
        assert!(validate_file_name(&"x".repeat(MAX_FILE_NAME_CHARS + 1)).is_err());
        assert!(validate_file_name("bad\u{7}name.bin").is_err());
        assert!(validate_file_name("line\nbreak.bin").is_err());
        // 路径分隔符由 safe_component 净化,不会逃出下载目录。
        assert_eq!(safe_component("a/b\\c.bin"), "a_b_c.bin");
    }

    #[test]
    fn create_time_duplicate_policy_skips_renames_or_overwrites() {
        let dir = tempdir().unwrap();
        let existing = dir.path().join("clip.mp4");
        std::fs::write(&existing, b"old").unwrap();

        assert!(resolve_download_target(dir.path(), "clip.mp4", "skip").is_err());
        assert_eq!(
            resolve_download_target(dir.path(), "clip.mp4", "overwrite").unwrap(),
            existing
        );
        let renamed = resolve_download_target(dir.path(), "clip.mp4", "rename").unwrap();
        assert_eq!(
            renamed.file_name().unwrap().to_str().unwrap(),
            "clip (1).mp4"
        );
        assert_eq!(
            resolve_download_target(dir.path(), "new.bin", "skip").unwrap(),
            dir.path().join("new.bin")
        );
    }

    #[test]
    fn probed_total_must_match_the_recorded_total() {
        assert!(ensure_total_matches(None, 10).is_ok());
        assert!(ensure_total_matches(Some(10), 10).is_ok());
        let mismatch = ensure_total_matches(Some(10), 11).unwrap_err();
        assert!(mismatch.to_string().contains("文件大小与此前记录不符"));
    }

    #[test]
    fn chunk_sizes_pick_the_closest_supported_candidate() {
        for requested in [64, 80, 127, 192, 300, 600, 1024, 2048, 4096] {
            let chosen = supported_chunk_size_kib(requested);
            assert!(CHUNK_SIZE_KIB_CHOICES.contains(&chosen));
        }
        assert_eq!(supported_chunk_size_kib(1), 64);
        assert_eq!(supported_chunk_size_kib(4096), 1024);
        assert_eq!(supported_chunk_size_kib(300), 256);
        assert_eq!(supported_chunk_size_kib(192), 128);
    }

    #[test]
    fn auto_retry_backoff_grows_and_caps() {
        assert_eq!(auto_retry_delay(1), Duration::from_secs(5));
        assert_eq!(auto_retry_delay(2), Duration::from_secs(10));
        assert_eq!(auto_retry_delay(3), Duration::from_secs(20));
        assert_eq!(auto_retry_delay(20), AUTO_RETRY_MAX);
    }

    #[test]
    fn bdp_chunk_size_tracks_rate_times_rtt() {
        // 无学习数据 → 交给调用方回退设置值
        assert_eq!(bdp_chunk_size_kib(0, 120), 0);
        assert_eq!(bdp_chunk_size_kib(1_048_576, 0), 0);
        // 1 MB/s × 200 ms ≈ 205 KiB → 抬到下限 256
        assert_eq!(bdp_chunk_size_kib(1_048_576, 200), 256);
        // 3 MB/s × 100 ms ≈ 300 KiB → 就近 256
        assert_eq!(bdp_chunk_size_kib(3 * 1_048_576, 100), 256);
        // 3 MB/s × 300 ms ≈ 900 KiB → 就近 1024
        assert_eq!(bdp_chunk_size_kib(3 * 1_048_576, 300), 1024);
        // 20 MB/s × 400 ms ≈ 8 MiB → 封顶 1024
        assert_eq!(bdp_chunk_size_kib(20 * 1_048_576, 400), 1024);
    }

    #[test]
    fn resumable_temporary_file_stays_next_to_final_target_for_atomic_commit() {
        let root = tempdir().unwrap();
        let output = root.path().join("nested").join("media.bin");
        let partial = temporary_output_path(&output, "task-123");
        assert_eq!(partial.parent(), output.parent());
        assert_eq!(
            partial.file_name().unwrap().to_str().unwrap(),
            ".telegram-media-task-123.part"
        );
    }

    /// 回归测试:finish/fail/pause 共用的分离路径必须先丢弃发送端,否则 writer 会一直
    /// 阻塞在 `recv()` 上,`drain_active` 永不返回(整个下载队列随之挂死)。
    #[tokio::test]
    async fn drain_active_closes_the_chunk_channel_before_joining_the_writer() {
        let (sender, mut receiver) = mpsc::channel::<IncomingChunk>(1);
        let writer = tokio::spawn(async move {
            // 与 chunk_writer 相同:只有发送端全部丢弃后 writer 才会退出。
            while receiver.recv().await.is_some() {}
            Ok::<u64, anyhow::Error>(2048)
        });
        let watcher = tokio::spawn(async { std::future::pending::<()>().await });
        let slots = Arc::new(Semaphore::new(1));
        let slot = Arc::clone(&slots).acquire_owned().await.unwrap();
        let active = ActiveDownload {
            sender,
            writer,
            watcher,
            last_activity: Arc::new(Mutex::new(Instant::now())),
            total: 2048,
            _slot: slot,
        };

        let drained = tokio::time::timeout(Duration::from_secs(5), drain_active(active))
            .await
            .expect("drain_active 必须先丢弃发送端让 writer 退出,而不是等满超时");

        assert_eq!(drained.unwrap(), 2048);
        assert_eq!(slots.available_permits(), 1, "槽位必须随分离一起释放");
    }

    #[tokio::test]
    async fn missing_chunk_ranges_only_lists_incomplete_chunks() {
        let dir = tempdir().unwrap();
        let store = TaskStore::open(&dir.path().join("tasks.sqlite"))
            .await
            .unwrap();
        let output = dir.path().join("out.bin");
        store.create(&record("plan-task", &output)).await.unwrap();
        chunk_writer::configure_chunk_map(&store, "plan-task", 3072, 1024)
            .await
            .unwrap();
        store
            .complete_chunk(
                "plan-task",
                1024,
                blake3::hash(&[7_u8; 1024]).to_hex().as_str(),
            )
            .await
            .unwrap();

        let missing = missing_chunk_ranges(&store, "plan-task").await.unwrap();
        assert_eq!(
            missing,
            vec![
                ChunkRange {
                    offset: 0,
                    length: 1024
                },
                ChunkRange {
                    offset: 2048,
                    length: 1024
                }
            ]
        );
    }

    #[tokio::test]
    async fn complete_download_commits_verified_chunks_and_removes_the_temp_file() {
        let dir = tempdir().unwrap();
        let store = TaskStore::open(&dir.path().join("tasks.sqlite"))
            .await
            .unwrap();
        let output = dir.path().join("final.bin");
        let contents = (0..2048)
            .map(|value| (value % 251) as u8)
            .collect::<Vec<_>>();
        let temp = seed_complete_task(&store, "commit-task", &output, &contents, 1024).await;

        complete_download(&store, "commit-task", 2048, &temp, &output, false)
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&output).await.unwrap(), contents);
        assert!(!temp.exists());
    }

    #[tokio::test]
    async fn complete_download_rejects_incomplete_chunk_maps() {
        let dir = tempdir().unwrap();
        let store = TaskStore::open(&dir.path().join("tasks.sqlite"))
            .await
            .unwrap();
        let output = dir.path().join("partial.bin");
        store
            .create(&record("partial-task", &output))
            .await
            .unwrap();
        chunk_writer::configure_chunk_map(&store, "partial-task", 2048, 1024)
            .await
            .unwrap();
        let temp = temporary_output_path(&output, "partial-task");
        tokio::fs::write(&temp, vec![1_u8; 2048]).await.unwrap();
        store
            .complete_chunk(
                "partial-task",
                0,
                blake3::hash(&[1_u8; 1024]).to_hex().as_str(),
            )
            .await
            .unwrap();

        let error = complete_download(&store, "partial-task", 2048, &temp, &output, false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("分块未完成"));
        assert!(!output.exists());
        assert!(temp.exists());
    }

    #[tokio::test]
    async fn complete_download_recovers_an_already_committed_target() {
        let dir = tempdir().unwrap();
        let store = TaskStore::open(&dir.path().join("tasks.sqlite"))
            .await
            .unwrap();
        let output = dir.path().join("recovered.bin");
        let contents = (0..2048)
            .map(|value| (value % 97) as u8)
            .collect::<Vec<_>>();
        let temp = seed_complete_task(&store, "recover-task", &output, &contents, 1024).await;
        // 上一次已完成提交(目标文件就位),但状态尚未落库时重入。
        tokio::fs::write(&output, &contents).await.unwrap();

        complete_download(&store, "recover-task", 2048, &temp, &output, false)
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&output).await.unwrap(), contents);
        assert!(!temp.exists());
    }

    #[tokio::test]
    async fn complete_download_refuses_to_overwrite_an_unrelated_target() {
        let dir = tempdir().unwrap();
        let store = TaskStore::open(&dir.path().join("tasks.sqlite"))
            .await
            .unwrap();
        let output = dir.path().join("occupied.bin");
        let contents = (0..2048)
            .map(|value| (value % 31) as u8)
            .collect::<Vec<_>>();
        let temp = seed_complete_task(&store, "occupied-task", &output, &contents, 1024).await;
        tokio::fs::write(&output, b"someone else's file")
            .await
            .unwrap();

        let error = complete_download(&store, "occupied-task", 2048, &temp, &output, false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("目标文件已存在"));
        assert_eq!(
            tokio::fs::read(&output).await.unwrap(),
            b"someone else's file"
        );
        assert!(temp.exists());
    }
}
