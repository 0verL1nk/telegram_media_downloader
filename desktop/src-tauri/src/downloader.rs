use crate::{
    app_state::SharedState,
    chunk_writer::{self, IncomingChunk},
    filter::{FilterMetadata, evaluate},
    models::{MessageInfo, Settings, TaskRecord},
    telegram::{self, TelegramAdapter},
};
use anyhow::{Context, Result, bail};
use futures_util::{StreamExt, stream};
use grammers_client::{InvocationError, media::Downloadable, tl};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom},
    sync::{Mutex, Notify, RwLock, Semaphore, broadcast, mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

const MAX_TASK_WORKERS: usize = 12;
const JOB_QUEUE_CAPACITY: usize = 256;
const MAX_BATCH_TASKS: usize = 20_000;

#[derive(Clone)]
struct TaskControl {
    token: CancellationToken,
    join: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    dispatch_ready: Arc<AtomicBool>,
    dispatch_changed: Arc<Notify>,
}

pub struct DownloadManager {
    shared: Arc<SharedState>,
    sender: mpsc::Sender<String>,
    controls: Arc<Mutex<HashMap<String, TaskControl>>>,
}

impl DownloadManager {
    pub async fn start(shared: Arc<SharedState>) -> Result<Self> {
        let (sender, receiver) = mpsc::channel(JOB_QUEUE_CAPACITY);
        let controls = Arc::new(Mutex::new(HashMap::new()));
        let manager = Self {
            shared: Arc::clone(&shared),
            sender,
            controls: Arc::clone(&controls),
        };
        tokio::spawn(dispatch(Arc::clone(&shared), receiver, controls));
        let queued = shared.store.list(Some("queued"), 2000).await?;
        for task in queued {
            manager.enqueue(task.task_id).await?;
        }
        Ok(manager)
    }

    pub async fn enqueue(&self, task_id: String) -> Result<()> {
        let mut controls = self.controls.lock().await;
        if controls
            .get(&task_id)
            .is_some_and(|control| !control.token.is_cancelled())
        {
            return Ok(());
        }
        let control = TaskControl {
            token: CancellationToken::new(),
            join: Arc::new(Mutex::new(None)),
            dispatch_ready: Arc::new(AtomicBool::new(false)),
            dispatch_changed: Arc::new(Notify::new()),
        };
        controls.insert(task_id.clone(), control.clone());
        drop(controls);
        if self.sender.send(task_id.clone()).await.is_err() {
            mark_dispatch_ready(&control);
            self.controls.lock().await.remove(&task_id);
            bail!("下载任务队列已关闭；任务已保留在本机，可稍后重启恢复");
        }
        Ok(())
    }

    pub async fn action(&self, task_id: &str, action: &str) -> Result<TaskRecord> {
        let record = self
            .shared
            .store
            .get(task_id)
            .await?
            .with_context(|| format!("未找到下载任务：{task_id}"))?;
        match action {
            "pause" | "cancel" => {
                if !matches!(record.status.as_str(), "queued" | "downloading") {
                    bail!("该任务当前状态不能暂停或取消");
                }
                if let Some(control) = self.controls.lock().await.get(task_id).cloned() {
                    control.token.cancel();
                    wait_for_dispatch(&control).await;
                    let join = { control.join.lock().await.take() };
                    if let Some(join) = join {
                        let _ = join.await;
                    }
                }
                let latest = self
                    .shared
                    .store
                    .get(task_id)
                    .await?
                    .with_context(|| format!("未找到下载任务：{task_id}"))?;
                if !matches!(latest.status.as_str(), "queued" | "downloading") {
                    return Ok(latest);
                }
                self.shared
                    .store
                    .set_status(
                        task_id,
                        if action == "pause" {
                            "paused"
                        } else {
                            "cancelled"
                        },
                        None,
                    )
                    .await?;
                self.controls.lock().await.remove(task_id);
                if action == "cancel" {
                    let settings = self.shared.settings.read().await.clone();
                    if !settings.preserve_partial_files
                        && let Some(output) = record.output_path.as_deref()
                    {
                        let temp = temporary_output_path(Path::new(output), task_id);
                        let _ = tokio::fs::remove_file(temp).await;
                    }
                }
                self.shared.publish_task(task_id).await;
                self.shared
                    .log(
                        "info",
                        "download",
                        format!(
                            "任务 {task_id} 已{}",
                            if action == "pause" {
                                "暂停"
                            } else {
                                "取消"
                            }
                        ),
                    )
                    .await;
            }
            "resume" => {
                if record.status != "paused" {
                    bail!("只有已暂停的任务可以继续");
                }
                self.shared
                    .store
                    .set_status(task_id, "queued", None)
                    .await?;
                self.enqueue(task_id.to_owned()).await?;
                self.shared.publish_task(task_id).await;
            }
            "retry" => {
                if !matches!(record.status.as_str(), "failed" | "cancelled") {
                    bail!("只有失败或已取消的任务可以重试");
                }
                self.shared
                    .store
                    .set_status(task_id, "queued", None)
                    .await?;
                self.enqueue(task_id.to_owned()).await?;
                self.shared.publish_task(task_id).await;
            }
            _ => bail!("不支持的任务操作"),
        }
        self.shared
            .store
            .get(task_id)
            .await?
            .context("任务状态更新后无法读取")
    }
}

async fn dispatch(
    shared: Arc<SharedState>,
    mut receiver: mpsc::Receiver<String>,
    controls: Arc<Mutex<HashMap<String, TaskControl>>>,
) {
    let slots = Arc::new(Semaphore::new(MAX_TASK_WORKERS));
    let active_count = Arc::new(AtomicUsize::new(0));
    let capacity_changed = Arc::new(Notify::new());
    let limiter = Arc::new(RequestLimiter::new(6));
    let bandwidth = Arc::new(BandwidthLimiter::default());
    let flood_gate = Arc::new(FloodGate::default());
    while let Some(task_id) = receiver.recv().await {
        let Some(control) = controls.lock().await.get(&task_id).cloned() else {
            continue;
        };
        let token = control.token.clone();
        let permit = tokio::select! {
            _ = token.cancelled() => {
                mark_dispatch_ready(&control);
                remove_control_if_current(&controls, &task_id, &control).await;
                continue;
            },
            permit = Arc::clone(&slots).acquire_owned() => match permit {
                Ok(p) => p,
                Err(_) => {
                    mark_dispatch_ready(&control);
                    remove_control_if_current(&controls, &task_id, &control).await;
                    break;
                }
            },
        };
        let task_shared = Arc::clone(&shared);
        let task_controls = Arc::clone(&controls);
        let task_active = Arc::clone(&active_count);
        let task_capacity = Arc::clone(&capacity_changed);
        let task_limiter = Arc::clone(&limiter);
        let task_bandwidth = Arc::clone(&bandwidth);
        let task_gate = Arc::clone(&flood_gate);
        let id = task_id.clone();
        let task_control = control.clone();
        let (start_tx, start_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            let _ = start_rx.await;
            run_task(
                task_shared,
                id.clone(),
                token,
                task_active,
                task_capacity,
                task_limiter,
                task_bandwidth,
                task_gate,
            )
            .await;
            drop(permit);
            remove_control_if_current(&task_controls, &id, &task_control).await;
        });
        *control.join.lock().await = Some(handle);
        mark_dispatch_ready(&control);
        let _ = start_tx.send(());
    }
}

fn mark_dispatch_ready(control: &TaskControl) {
    control.dispatch_ready.store(true, Ordering::Release);
    control.dispatch_changed.notify_waiters();
}

async fn wait_for_dispatch(control: &TaskControl) {
    loop {
        let notified = control.dispatch_changed.notified();
        if control.dispatch_ready.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }
}

async fn remove_control_if_current(
    controls: &Mutex<HashMap<String, TaskControl>>,
    task_id: &str,
    control: &TaskControl,
) {
    let mut controls = controls.lock().await;
    if controls
        .get(task_id)
        .is_some_and(|current| Arc::ptr_eq(&current.join, &control.join))
    {
        controls.remove(task_id);
    }
}

async fn wait_for_file_slot(
    shared: &SharedState,
    token: &CancellationToken,
    active: &AtomicUsize,
    changed: &Notify,
) -> bool {
    loop {
        if token.is_cancelled() {
            return false;
        }
        let limit = shared
            .settings
            .read()
            .await
            .concurrency
            .max_files
            .clamp(1, MAX_TASK_WORKERS);
        let current = active.load(Ordering::Acquire);
        if current < limit
            && active
                .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            return true;
        }
        tokio::select! { _ = token.cancelled() => return false, _ = changed.notified() => {}, _ = tokio::time::sleep(Duration::from_millis(250)) => {} }
    }
}

async fn run_task(
    shared: Arc<SharedState>,
    task_id: String,
    token: CancellationToken,
    active: Arc<AtomicUsize>,
    changed: Arc<Notify>,
    limiter: Arc<RequestLimiter>,
    bandwidth: Arc<BandwidthLimiter>,
    flood_gate: Arc<FloodGate>,
) {
    if !wait_for_file_slot(&shared, &token, &active, &changed).await {
        return;
    }
    if token.is_cancelled() {
        active.fetch_sub(1, Ordering::AcqRel);
        changed.notify_waiters();
        return;
    }
    let _active_guard = ActiveGuard { active, changed };
    if let Err(error) = shared.store.set_status(&task_id, "downloading", None).await {
        shared
            .log(
                "error",
                "database",
                format!("任务 {task_id} 无法开始：{error:#}"),
            )
            .await;
        return;
    }
    shared.publish_task(&task_id).await;
    let result = download_task(&shared, &task_id, &token, &limiter, &bandwidth, &flood_gate).await;
    if token.is_cancelled() {
        return;
    }
    match result {
        Ok(()) => {
            if let Err(error) = shared.store.set_status(&task_id, "completed", None).await {
                shared
                    .log(
                        "error",
                        "database",
                        format!("任务 {task_id} 完成状态保存失败：{error:#}"),
                    )
                    .await;
            } else if shared.settings.read().await.cloud_upload.enabled {
                if let Err(error) =
                    crate::cloud_upload::queue_completed_download(&shared, &task_id).await
                {
                    shared
                        .log(
                            "warn",
                            "cloud-upload",
                            format!("下载已完成，但自动云上传未能排队：{error:#}"),
                        )
                        .await;
                }
            }
            shared
                .log("info", "download", format!("任务 {task_id} 下载并校验完成"))
                .await;
        }
        Err(error) => {
            let detail = safe_error(&error.to_string());
            let _ = shared
                .store
                .set_status(&task_id, "failed", Some(&detail))
                .await;
            shared
                .log(
                    "error",
                    "download",
                    format!("任务 {task_id} 失败：{detail}"),
                )
                .await;
        }
    }
    shared.publish_task(&task_id).await;
}

struct ActiveGuard {
    active: Arc<AtomicUsize>,
    changed: Arc<Notify>,
}
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
        self.changed.notify_waiters();
    }
}

async fn download_task(
    shared: &SharedState,
    task_id: &str,
    token: &CancellationToken,
    limiter: &Arc<RequestLimiter>,
    bandwidth: &Arc<BandwidthLimiter>,
    flood_gate: &Arc<FloodGate>,
) -> Result<()> {
    let record = shared
        .store
        .get(task_id)
        .await?
        .with_context(|| format!("任务记录不存在：{task_id}"))?;
    if record.media_type.as_deref() == Some("text") {
        return save_text_task(shared, &record, token).await;
    }
    let service = shared.telegram().await?;
    let message = service
        .message_by_id(&record.chat_id, record.message_id.unwrap_or_default())
        .await?
        .with_context(|| "Telegram 消息已删除、不可访问或当前账号没有权限")?;
    let metadata = telegram::message_info(&record.chat_id, &message);
    if !metadata.downloadable {
        bail!(
            "{}",
            metadata
                .unavailable_reason
                .unwrap_or_else(|| "Telegram 媒体不可下载".into())
        );
    }
    let media = message.media().context("Telegram 消息不再包含可下载媒体")?;
    let location = media
        .to_raw_input_location()
        .context("当前媒体没有可用的 Telegram 文件位置")?;
    let total = media.size().context("Telegram 未返回文件大小")? as u64;
    if total == 0 {
        bail!("Telegram 返回了空媒体文件");
    }
    if record.total_bytes.is_some_and(|expected| expected != total) {
        bail!("消息媒体大小已变化；为保护续传文件，任务已停止，请重试任务");
    }
    let output = PathBuf::from(record.output_path.clone().context("下载目标路径为空")?);
    let temp = temporary_output_path(&output, task_id);
    let settings = shared.settings.read().await.clone();
    bandwidth.set_limit(settings.concurrency.max_bandwidth_kib.saturating_mul(1024));
    let chunk_size = (supported_chunk_size_kib(settings.concurrency.chunk_size_kib) as u64) * 1024;
    if tokio::fs::try_exists(&output).await? {
        if verified_final_file_matches_chunks(&shared.store, task_id, &output, total).await? {
            shared.store.update_progress(task_id, total, 0).await?;
            write_sidecars(&output, &metadata, &settings).await?;
            return Ok(());
        }
        if settings.duplicate_policy != "overwrite" {
            bail!(
                "目标文件已存在，且不匹配此任务已校验的分块；为避免覆盖文件，任务已停止：{}",
                output.display()
            );
        }
    }
    chunk_writer::configure_chunk_map(&shared.store, task_id, total, chunk_size).await?;
    let temp_parent = temp.parent().context("临时文件目录无效")?;
    tokio::fs::create_dir_all(temp_parent).await?;
    // Validate persisted bytes before spawning child tasks. Any error here then
    // returns without detaching a writer or progress-event task.
    chunk_writer::verify_resumable_chunks(&shared.store, task_id, &temp).await?;
    let rows = shared.store.chunk_map(task_id).await?;
    let missing = rows
        .into_iter()
        .filter(|(_, _, _, complete)| !complete)
        .map(|(offset, length, _, _)| (offset, length))
        .collect::<Vec<_>>();
    let (chunk_tx, chunk_rx) = mpsc::channel(settings.concurrency.per_file_chunks.clamp(1, 16) * 2);
    let (progress_tx, mut progress_rx) = broadcast::channel(8);
    let writer_store = shared.store.clone();
    let writer_id = task_id.to_owned();
    let writer_path = temp.clone();
    let writer = tokio::spawn(async move {
        chunk_writer::write_chunks_bounded(
            writer_store,
            writer_id,
            &writer_path,
            total,
            chunk_rx,
            progress_tx,
        )
        .await
    });
    let update_shared = shared.app.clone();
    let store_for_events = shared.store.clone();
    let id_for_events = task_id.to_owned();
    let event_watcher = tokio::spawn(async move {
        let mut dirty = false;
        let mut tick = tokio::time::interval(Duration::from_millis(350));
        loop {
            tokio::select! {
                event = progress_rx.recv() => match event {
                    Ok(()) => dirty = true,
                    Err(broadcast::error::RecvError::Lagged(_)) => dirty = true,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = tick.tick() => if dirty {
                    dirty = false;
                    if let Ok(Some(task)) = store_for_events.get(&id_for_events).await {
                        use tauri::Emitter;
                        let _ = update_shared.emit("task-updated", task);
                    }
                },
            }
        }
        if let Ok(Some(task)) = store_for_events.get(&id_for_events).await {
            use tauri::Emitter;
            let _ = update_shared.emit("task-updated", task);
        }
    });

    let location = Arc::new(RwLock::new(location));
    limiter.set_ceiling(
        settings.concurrency.max_files.clamp(1, 12)
            * settings.concurrency.per_file_chunks.clamp(1, 16),
    );
    let client = service.client().clone();
    let file_limit = settings.concurrency.per_file_chunks.clamp(1, 16);
    let timeout = Duration::from_secs(settings.concurrency.request_timeout_seconds.clamp(5, 600));
    let retries = settings.concurrency.retries.clamp(0, 20);
    let token_for_chunks = token.clone();
    let service_for_refresh = Arc::clone(&service);
    let location_for_chunks = Arc::clone(&location);
    let bandwidth_for_chunks = Arc::clone(bandwidth);
    let chat_id = record.chat_id.clone();
    let message_id = record.message_id.unwrap_or_default();
    let chunk_stream = stream::iter(missing)
        .map(|(offset, length)| {
            let client = client.clone();
            let token = token_for_chunks.clone();
            let location = Arc::clone(&location_for_chunks);
            let service = Arc::clone(&service_for_refresh);
            let gate = Arc::clone(flood_gate);
            let limiter = Arc::clone(limiter);
            let bandwidth = Arc::clone(&bandwidth_for_chunks);
            let chat_id = chat_id.clone();
            async move {
                let data = fetch_chunk(
                    &client, &service, &chat_id, message_id, total, &location, offset, length,
                    retries, timeout, &token, &gate, &limiter,
                )
                .await?;
                bandwidth.consume(data.len() as u64, &token).await?;
                Ok::<IncomingChunk, anyhow::Error>(IncomingChunk { offset, data })
            }
        })
        .buffer_unordered(file_limit);
    tokio::pin!(chunk_stream);
    let mut result: Result<()> = Ok(());
    loop {
        let chunk = tokio::select! {
            _ = token.cancelled() => break,
            chunk = chunk_stream.next() => chunk,
        };
        let Some(chunk) = chunk else {
            break;
        };
        match chunk {
            Ok(chunk) => {
                if let Err(error) = chunk_tx.send(chunk).await {
                    result = Err(error.into());
                    break;
                }
            }
            Err(error) => {
                result = Err(error);
                break;
            }
        }
    }
    drop(chunk_tx);
    let writer_result = join_download_children(writer, event_watcher).await;
    if token.is_cancelled() {
        let _ = writer_result;
        return Ok(());
    }
    if let Err(error) = result {
        let _ = writer_result;
        return Err(error);
    }
    writer_result?;
    if token.is_cancelled() {
        return Ok(());
    }
    verify_telegram_hashes(
        service.client(),
        &service,
        &record.chat_id,
        record.message_id.unwrap_or_default(),
        &location,
        &temp,
        total,
        limiter,
        flood_gate,
        token,
        timeout,
        retries,
    )
    .await?;
    if token.is_cancelled() {
        return Ok(());
    }
    let overwrite = settings.duplicate_policy == "overwrite";
    if !overwrite && tokio::fs::try_exists(&output).await? {
        if verified_final_file_matches_chunks(&shared.store, task_id, &output, total).await? {
            let _ = tokio::fs::remove_file(&temp).await;
            write_sidecars(&output, &metadata, &settings).await?;
            return Ok(());
        }
        bail!(
            "目标文件在下载期间被其他文件占用；已验证的临时文件已保留，请移走冲突文件后重试：{}",
            output.display()
        );
    }
    chunk_writer::commit_completed_file(&temp, &output, overwrite)?;
    write_sidecars(&output, &metadata, &settings).await?;
    Ok(())
}

async fn save_text_task(
    shared: &SharedState,
    record: &TaskRecord,
    token: &CancellationToken,
) -> Result<()> {
    let service = shared.telegram().await?;
    let message = service
        .message_by_id(&record.chat_id, record.message_id.unwrap_or_default())
        .await?
        .with_context(|| "Telegram 文本消息已删除或当前账号没有权限")?;
    if message.media().is_some() {
        bail!("此任务对应的消息已不再是纯文本消息");
    }
    let text = message.text();
    if text.trim().is_empty() {
        bail!("Telegram 文本消息内容为空");
    }
    if record
        .total_bytes
        .is_some_and(|expected| expected != text.len() as u64)
    {
        bail!("文本消息内容已变化；为避免静默替换旧内容，任务已停止");
    }
    let output = PathBuf::from(
        record
            .output_path
            .as_deref()
            .context("文本任务目标路径为空")?,
    );
    let temp = temporary_output_path(&output, &record.task_id);
    if let Some(parent) = output.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if let Some(parent) = temp.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = File::create(&temp).await?;
    tokio::select! {
        _ = token.cancelled() => bail!("文本保存已取消"),
        result = file.write_all(text.as_bytes()) => result.context("无法写入 Telegram 文本消息")?,
    }
    file.sync_all().await?;
    drop(file);
    if token.is_cancelled() {
        bail!("文本保存已取消");
    }
    let settings = shared.settings.read().await.clone();
    if settings.duplicate_policy == "overwrite" && tokio::fs::try_exists(&output).await? {
        tokio::fs::remove_file(&output).await?;
    }
    tokio::fs::rename(&temp, &output)
        .await
        .with_context(|| format!("无法提交文本文件：{}", output.display()))?;
    shared
        .store
        .update_progress(&record.task_id, text.len() as u64, 0)
        .await?;
    Ok(())
}

async fn fetch_chunk(
    client: &grammers_client::Client,
    service: &TelegramAdapter,
    chat_id: &str,
    message_id: i64,
    total_size: u64,
    location: &RwLock<tl::enums::InputFileLocation>,
    offset: u64,
    length: u64,
    retries: u32,
    timeout: Duration,
    token: &CancellationToken,
    gate: &Arc<FloodGate>,
    limiter: &Arc<RequestLimiter>,
) -> Result<Vec<u8>> {
    let mut attempt = 0_u32;
    loop {
        if token.is_cancelled() {
            bail!("任务已取消");
        }
        gate.wait(token).await?;
        let _permit = limiter.acquire(token).await?;
        let location_snapshot = location.read().await.clone();
        let request = tl::functions::upload::GetFile {
            precise: true,
            cdn_supported: false,
            location: location_snapshot,
            offset: offset as i64,
            limit: i32::try_from(length).context("分块长度超出 Telegram API 范围")?,
        };
        let response = tokio::select! {
            _ = token.cancelled() => bail!("任务已取消"),
            response = tokio::time::timeout(timeout, client.invoke(&request)) => match response {
                Ok(result) => result,
                Err(_) => { attempt += 1; if attempt > retries { bail!("Telegram 分块请求超时（偏移 {offset}）"); } tokio::time::sleep(backoff(attempt)).await; continue; }
            }
        };
        match response {
            Ok(tl::enums::upload::File::File(file)) => {
                if file.bytes.len() as u64 != length {
                    attempt += 1;
                    if attempt > retries {
                        bail!(
                            "Telegram 返回了不完整分块，偏移 {offset}，期望 {length} 字节，收到 {} 字节",
                            file.bytes.len()
                        );
                    }
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
                limiter.success();
                return Ok(file.bytes);
            }
            Ok(tl::enums::upload::File::CdnRedirect(_)) => bail!(
                "Telegram 将文件重定向到 CDN；当前版本尚未实现 CDN 加密与哈希验证，任务保持失败以避免保存未验证内容"
            ),
            Err(error) if is_file_reference_error(&error) => {
                attempt += 1;
                if attempt > retries {
                    bail!("Telegram 文件引用已失效，重新读取消息后仍无法恢复：{error}");
                }
                refresh_location(service, chat_id, message_id, total_size, location).await?;
            }
            Err(error) if flood_wait_seconds(&error).is_some() => {
                let wait = flood_wait_seconds(&error).unwrap_or(1).clamp(1, 86_400);
                gate.extend(Duration::from_secs(u64::from(wait))).await;
                limiter.back_off();
                attempt += 1;
                if attempt > retries {
                    bail!("Telegram 限流，等待 {wait} 秒后重试次数已用尽");
                }
            }
            Err(error) => {
                attempt += 1;
                if attempt > retries {
                    bail!("Telegram 分块读取失败（偏移 {offset}）：{error}");
                }
                tokio::time::sleep(backoff(attempt)).await;
            }
        }
    }
}

async fn refresh_location(
    service: &TelegramAdapter,
    chat_id: &str,
    message_id: i64,
    total_size: u64,
    location: &RwLock<tl::enums::InputFileLocation>,
) -> Result<()> {
    let message = service
        .message_by_id(chat_id, message_id)
        .await?
        .with_context(|| "Telegram 原消息已不可访问，无法刷新文件引用")?;
    let metadata = telegram::message_info(chat_id, &message);
    if !metadata.downloadable {
        bail!(
            "文件引用失效，且原消息当前不可下载：{}",
            metadata.unavailable_reason.unwrap_or_default()
        );
    }
    let media = message.media().context("原消息已不包含媒体")?;
    if media.size().is_none_or(|size| size as u64 != total_size) {
        bail!("刷新文件引用时发现媒体大小已变化，停止续传以保护文件完整性");
    }
    let new_location = media
        .to_raw_input_location()
        .context("无法从原消息取得新的 Telegram 文件引用")?;
    *location.write().await = new_location;
    Ok(())
}

async fn verify_telegram_hashes(
    client: &grammers_client::Client,
    service: &TelegramAdapter,
    chat_id: &str,
    message_id: i64,
    location: &RwLock<tl::enums::InputFileLocation>,
    path: &Path,
    total: u64,
    limiter: &Arc<RequestLimiter>,
    gate: &Arc<FloodGate>,
    token: &CancellationToken,
    timeout: Duration,
    retries: u32,
) -> Result<()> {
    let mut file = File::open(path).await.context("无法打开待校验临时文件")?;
    if file.metadata().await?.len() != total {
        bail!("临时文件长度与 Telegram 元数据不一致");
    }
    let mut offset = 0_u64;
    let mut checked_any = false;
    let mut rounds = 0;
    while offset < total && rounds < 4096 {
        if token.is_cancelled() {
            bail!("任务已取消");
        }
        gate.wait(token).await?;
        let _permit = limiter.acquire(token).await?;
        let mut attempt = 0_u32;
        let hashes: Vec<tl::enums::FileHash> = loop {
            let _permit = limiter.acquire(token).await?;
            let request = tl::functions::upload::GetFileHashes {
                location: location.read().await.clone(),
                offset: offset as i64,
            };
            let response = tokio::select! {
                _ = token.cancelled() => bail!("任务已取消"),
                response = tokio::time::timeout(timeout, client.invoke(&request)) => response,
            };
            match response {
                Ok(Ok(hashes)) => {
                    limiter.success();
                    break hashes;
                }
                Err(_) if attempt < retries => {
                    attempt += 1;
                    tokio::select! { _ = token.cancelled() => bail!("任务已取消"), _ = tokio::time::sleep(backoff(attempt)) => {} }
                }
                Err(_) => bail!("读取 Telegram 文件校验信息超时（偏移 {offset}）"),
                Ok(Err(error)) if is_file_reference_error(&error) && attempt < retries => {
                    attempt += 1;
                    refresh_location(service, chat_id, message_id, total, location).await?;
                }
                Ok(Err(error)) if flood_wait_seconds(&error).is_some() && attempt < retries => {
                    let wait = flood_wait_seconds(&error).unwrap_or(1).clamp(1, 86_400);
                    gate.extend(Duration::from_secs(u64::from(wait))).await;
                    limiter.back_off();
                    attempt += 1;
                }
                Ok(Err(error)) => return Err(error).context("Telegram 分块哈希读取失败"),
            }
        };
        if hashes.is_empty() {
            break;
        }
        let previous = offset;
        for item in hashes {
            let tl::enums::FileHash::Hash(item) = item;
            if item.offset < 0 || item.limit <= 0 {
                bail!("Telegram 返回无效的文件校验范围");
            }
            let part_offset = item.offset as u64;
            let part_len = item.limit as u64;
            if part_offset.saturating_add(part_len) > total {
                bail!("Telegram 返回的文件校验范围超出文件长度");
            }
            let mut bytes = vec![0_u8; part_len as usize];
            file.seek(SeekFrom::Start(part_offset)).await?;
            file.read_exact(&mut bytes).await?;
            let digest = Sha256::digest(&bytes);
            if digest.as_slice() != item.hash.as_slice() {
                bail!("Telegram SHA-256 文件分块校验失败（偏移 {part_offset}）");
            }
            checked_any = true;
            offset = offset.max(part_offset.saturating_add(part_len));
        }
        rounds += 1;
        if offset <= previous {
            break;
        }
    }
    if rounds >= 4096 {
        bail!("Telegram 文件哈希响应数量超出保护上限");
    }
    if !checked_any {
        tracing::warn!(
            "Telegram 未为本文件提供 SHA-256 哈希；任务仍按完整长度及本地持久分块摘要校验"
        );
    }
    Ok(())
}

fn flood_wait_seconds(error: &InvocationError) -> Option<u32> {
    match error {
        InvocationError::Rpc(rpc) if rpc.is("FLOOD_WAIT_*") => rpc.value,
        _ => None,
    }
}
fn is_file_reference_error(error: &InvocationError) -> bool {
    matches!(error, InvocationError::Rpc(rpc) if rpc.is("FILE_REFERENCE_EXPIRED") || rpc.is("FILE_REFERENCE_INVALID") || rpc.is("FILE_REFERENCE_*"))
}
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(350_u64.saturating_mul(1_u64 << attempt.min(5)))
}
fn safe_error(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control())
        .take(700)
        .collect()
}
fn supported_chunk_size_kib(requested: usize) -> usize {
    [64_usize, 128, 256, 512, 1024]
        .into_iter()
        .min_by_key(|candidate| candidate.abs_diff(requested.clamp(64, 1024)))
        .unwrap_or(512)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn record(id: &str, output_path: &Path, total_bytes: u64) -> TaskRecord {
        TaskRecord {
            task_id: id.into(),
            chat_id: "-1".into(),
            chat_title: Some("chat".into()),
            message_id: Some(1),
            media_type: Some("document".into()),
            file_name: Some("media.bin".into()),
            status: "queued".into(),
            progress: 0.0,
            downloaded_bytes: 0,
            total_bytes: Some(total_bytes),
            speed_bytes_per_second: 0,
            remaining_bytes: Some(total_bytes),
            started_at: None,
            updated_at: None,
            completed_at: None,
            output_path: Some(output_path.to_string_lossy().into_owned()),
            error: None,
            retry_count: 0,
            group_id: None,
        }
    }

    #[test]
    fn chunk_sizes_stay_aligned_to_telegram_megabyte_boundaries() {
        for requested in [64, 80, 127, 192, 300, 600, 1024, 2048] {
            let chosen = supported_chunk_size_kib(requested);
            assert!([64, 128, 256, 512, 1024].contains(&chosen));
            assert_eq!(1024 % chosen, 0);
        }
    }

    #[test]
    fn batch_limit_allows_exactly_the_configured_cap() {
        let mut created = 0;
        while !batch_limit_reached(created) {
            created += 1;
        }
        assert_eq!(created, MAX_BATCH_TASKS);
        assert!(batch_limit_reached(created));
    }

    #[test]
    fn duplicate_skip_policy_returns_a_skip_decision() {
        let dir = tempdir().unwrap();
        let existing = dir.path().join("existing.bin");
        std::fs::write(&existing, b"already here").unwrap();

        assert!(
            apply_duplicate_policy(&existing, "skip", 12)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            apply_duplicate_policy(&existing, "overwrite", 12).unwrap(),
            Some(existing.clone())
        );
        assert!(
            apply_duplicate_policy(&dir.path().join("new.bin"), "skip", 12)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn legacy_all_format_is_a_wildcard_instead_of_filtering_every_file() {
        assert!(matches_file_format("clip.mp4", &["all".into()]));
        assert!(matches_file_format("clip.mp4", &["*".into()]));
        assert!(matches_file_format("clip.MP4", &[".mp4".into()]));
        assert!(!matches_file_format("clip.mkv", &["mp4".into()]));
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

    #[test]
    fn output_path_applies_user_date_format_to_folder_and_filename_tokens() {
        let root = tempdir().unwrap();
        let mut settings = Settings::default();
        settings.download_root = root.path().to_string_lossy().into_owned();
        settings.path_template = "{chat}/{media_datetime}".into();
        settings.file_name_template = "{media_datetime}_{message_id}_{name}".into();
        settings.date_format = "%Y_%m".into();
        let info = MessageInfo {
            chat_id: "-1".into(),
            message_id: 17,
            media_type: Some("document".into()),
            file_name: Some("report.pdf".into()),
            caption: None,
            text: None,
            size_bytes: None,
            media_width: None,
            media_height: None,
            media_duration: None,
            file_extension: Some("pdf".into()),
            sender_id: None,
            sender_name: None,
            reply_to_message_id: None,
            message_thread_id: None,
            date: None,
            group_id: None,
            downloadable: true,
            unavailable_reason: None,
        };
        let date = chrono::DateTime::parse_from_rfc3339("2024-03-09T00:00:00Z")
            .unwrap()
            .to_utc();

        let path = output_path(&settings, "sample-chat", "report.pdf", &info, date);
        assert_eq!(path.parent().unwrap().file_name().unwrap(), "2024_03");
        assert_eq!(path.file_name().unwrap(), "2024_03_17_report.pdf");
    }

    #[tokio::test]
    async fn existing_final_file_is_recovered_only_when_all_chunk_hashes_match() {
        let dir = tempdir().unwrap();
        let store = crate::task_store::TaskStore::open(&dir.path().join("tasks.sqlite"))
            .await
            .unwrap();
        let output = dir.path().join("recovered.bin");
        let contents = (0..2048)
            .map(|value| (value % 251) as u8)
            .collect::<Vec<_>>();
        store
            .create(&record("recovery-task", &output, contents.len() as u64))
            .await
            .unwrap();
        store
            .set_chunks("recovery-task", contents.len() as u64, 1024)
            .await
            .unwrap();
        for (offset, bytes) in contents.chunks(1024).enumerate() {
            store
                .complete_chunk(
                    "recovery-task",
                    (offset * 1024) as u64,
                    &blake3::hash(bytes).to_hex().to_string(),
                )
                .await
                .unwrap();
        }
        tokio::fs::write(&output, &contents).await.unwrap();

        assert!(
            verified_final_file_matches_chunks(
                &store,
                "recovery-task",
                &output,
                contents.len() as u64
            )
            .await
            .unwrap()
        );

        tokio::fs::write(&output, vec![0; contents.len()])
            .await
            .unwrap();
        assert!(
            !verified_final_file_matches_chunks(
                &store,
                "recovery-task",
                &output,
                contents.len() as u64
            )
            .await
            .unwrap()
        );
        assert!(
            store
                .chunk_map("recovery-task")
                .await
                .unwrap()
                .iter()
                .all(|(_, _, _, complete)| *complete)
        );
    }

    #[tokio::test]
    async fn cancellation_cleanup_joins_writer_and_aborted_event_watcher() {
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let watcher_dropped = Arc::new(AtomicBool::new(false));
        let watcher_flag = Arc::clone(&watcher_dropped);
        let (watcher_started_tx, watcher_started_rx) = oneshot::channel();
        let event_watcher = tokio::spawn(async move {
            let _drop_flag = DropFlag(watcher_flag);
            let _ = watcher_started_tx.send(());
            futures_util::future::pending::<()>().await;
        });
        watcher_started_rx.await.unwrap();
        let (sender, mut receiver) = mpsc::channel::<()>(1);
        let writer = tokio::spawn(async move {
            while receiver.recv().await.is_some() {}
            Ok::<_, anyhow::Error>(17_u8)
        });
        drop(sender);

        assert_eq!(
            join_download_children(writer, event_watcher).await.unwrap(),
            17
        );
        assert!(watcher_dropped.load(Ordering::Acquire));
    }
}

#[derive(Default)]
struct FloodGate {
    blocked_until: Mutex<Option<Instant>>,
}
impl FloodGate {
    async fn extend(&self, duration: Duration) {
        let until = Instant::now() + duration;
        let mut current = self.blocked_until.lock().await;
        if current.is_none_or(|saved| saved < until) {
            *current = Some(until);
        }
    }
    async fn wait(&self, token: &CancellationToken) -> Result<()> {
        loop {
            let remaining = self
                .blocked_until
                .lock()
                .await
                .and_then(|until| until.checked_duration_since(Instant::now()));
            let Some(remaining) = remaining.filter(|duration| !duration.is_zero()) else {
                return Ok(());
            };
            tokio::select! { _ = token.cancelled() => bail!("任务已取消"), _ = tokio::time::sleep(remaining) => {} }
        }
    }
}

struct RequestLimiter {
    active: AtomicUsize,
    current: AtomicUsize,
    ceiling: AtomicUsize,
    consecutive_successes: AtomicUsize,
    changed: Notify,
}
impl RequestLimiter {
    fn new(initial: usize) -> Self {
        Self {
            active: AtomicUsize::new(0),
            current: AtomicUsize::new(initial),
            ceiling: AtomicUsize::new(initial),
            consecutive_successes: AtomicUsize::new(0),
            changed: Notify::new(),
        }
    }
    fn set_ceiling(&self, value: usize) {
        let value = value.clamp(1, 60);
        self.ceiling.store(value, Ordering::Release);
        self.current.fetch_min(value, Ordering::AcqRel);
        self.changed.notify_waiters();
    }
    fn back_off(&self) {
        let now = self.current.load(Ordering::Acquire);
        self.current.store((now / 2).max(1), Ordering::Release);
        self.consecutive_successes.store(0, Ordering::Release);
        self.changed.notify_waiters();
    }
    fn success(&self) {
        if self.consecutive_successes.fetch_add(1, Ordering::AcqRel) >= 31 {
            self.consecutive_successes.store(0, Ordering::Release);
            let ceiling = self.ceiling.load(Ordering::Acquire);
            let _ = self
                .current
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    Some((current + 1).min(ceiling))
                });
            self.changed.notify_waiters();
        }
    }
    async fn acquire(self: &Arc<Self>, token: &CancellationToken) -> Result<LimiterPermit> {
        loop {
            if token.is_cancelled() {
                bail!("任务已取消");
            }
            let max = self.current.load(Ordering::Acquire);
            let current = self.active.load(Ordering::Acquire);
            if current < max
                && self
                    .active
                    .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                return Ok(LimiterPermit(Arc::clone(self)));
            }
            tokio::select! { _ = token.cancelled() => bail!("任务已取消"), _ = self.changed.notified() => {}, _ = tokio::time::sleep(Duration::from_millis(100)) => {} }
        }
    }
}
struct LimiterPermit(Arc<RequestLimiter>);
impl Drop for LimiterPermit {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
}

struct BandwidthLimiter {
    bytes_per_second: AtomicUsize,
    state: Mutex<BandwidthBucket>,
}
struct BandwidthBucket {
    tokens: f64,
    updated: Instant,
}
impl Default for BandwidthLimiter {
    fn default() -> Self {
        Self {
            bytes_per_second: AtomicUsize::new(0),
            state: Mutex::new(BandwidthBucket {
                tokens: 0.0,
                updated: Instant::now(),
            }),
        }
    }
}
impl BandwidthLimiter {
    fn set_limit(&self, bytes_per_second: u64) {
        self.bytes_per_second.store(
            bytes_per_second.min(usize::MAX as u64) as usize,
            Ordering::Release,
        );
    }
    async fn consume(&self, bytes: u64, token: &CancellationToken) -> Result<()> {
        let mut remaining = bytes as f64;
        loop {
            if token.is_cancelled() {
                bail!("任务已取消");
            }
            let rate = self.bytes_per_second.load(Ordering::Acquire) as f64;
            if rate <= 0.0 {
                return Ok(());
            }
            let mut bucket = self.state.lock().await;
            let now = Instant::now();
            let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
            bucket.tokens = (bucket.tokens + elapsed * rate).min(rate);
            bucket.updated = now;
            let take = bucket.tokens.min(remaining);
            bucket.tokens -= take;
            remaining -= take;
            if remaining <= 0.0 {
                return Ok(());
            }
            let wait = Duration::from_secs_f64(
                ((1.0 - bucket.tokens).max(0.001) / rate).clamp(0.001, 0.25),
            );
            drop(bucket);
            tokio::select! { _ = token.cancelled() => bail!("任务已取消"), _ = tokio::time::sleep(wait) => {} }
        }
    }
}

pub async fn create_task_for_message(
    shared: &SharedState,
    manager: &DownloadManager,
    chat_id: &str,
    message_id: i64,
    requested_type: Option<&str>,
) -> Result<TaskRecord> {
    create_task_for_message_if_needed(shared, manager, chat_id, message_id, requested_type)
        .await?
        .ok_or_else(|| anyhow::anyhow!("目标文件已存在，按“跳过”策略未创建下载任务"))
}

async fn create_task_for_message_if_needed(
    shared: &SharedState,
    manager: &DownloadManager,
    chat_id: &str,
    message_id: i64,
    requested_type: Option<&str>,
) -> Result<Option<TaskRecord>> {
    if requested_type == Some("text") {
        return create_text_task_for_message_if_needed(shared, manager, chat_id, message_id).await;
    }
    let service = shared.telegram().await?;
    let message = service
        .message_by_id(chat_id, message_id)
        .await?
        .with_context(|| "消息已删除或当前账号没有权限")?;
    let info = telegram::message_info(chat_id, &message);
    if !info.downloadable {
        bail!(
            "{}",
            info.unavailable_reason
                .unwrap_or_else(|| "此消息不可下载".into())
        );
    }
    if requested_type.is_some_and(|kind| info.media_type.as_deref() != Some(kind)) {
        bail!("页面提供的媒体类型与 Telegram 消息实际类型不一致");
    }
    let settings = shared.settings.read().await.clone();
    let title = message
        .peer()
        .and_then(|peer| peer.name())
        .unwrap_or("Telegram 聊天")
        .to_owned();
    let file_name = info
        .file_name
        .clone()
        .unwrap_or_else(|| format!("telegram_{}", message.id()));
    let size = info.size_bytes.context("Telegram 未提供媒体大小")?;
    let target = output_path(&settings, &title, &file_name, &info, message.date());
    let Some(target) = apply_duplicate_policy(&target, &settings.duplicate_policy, size)? else {
        return Ok(None);
    };
    let record = TaskRecord {
        task_id: uuid::Uuid::new_v4().to_string(),
        chat_id: chat_id.to_owned(),
        chat_title: Some(title),
        message_id: Some(message_id),
        media_type: info.media_type.clone(),
        file_name: Some(
            target
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ),
        status: "queued".into(),
        progress: 0.0,
        downloaded_bytes: 0,
        total_bytes: Some(size),
        speed_bytes_per_second: 0,
        remaining_bytes: Some(size),
        started_at: None,
        updated_at: Some(chrono::Utc::now().to_rfc3339()),
        completed_at: None,
        output_path: Some(target.to_string_lossy().into_owned()),
        error: None,
        retry_count: 0,
        group_id: info.group_id,
    };
    shared.store.create(&record).await?;
    manager.enqueue(record.task_id.clone()).await?;
    shared.publish_task(&record.task_id).await;
    shared
        .log(
            "info",
            "download",
            format!(
                "已排入下载：{}",
                record.file_name.as_deref().unwrap_or("媒体文件")
            ),
        )
        .await;
    Ok(Some(record))
}

async fn create_text_task_for_message_if_needed(
    shared: &SharedState,
    manager: &DownloadManager,
    chat_id: &str,
    message_id: i64,
) -> Result<Option<TaskRecord>> {
    let service = shared.telegram().await?;
    let message = service
        .message_by_id(chat_id, message_id)
        .await?
        .with_context(|| "Telegram 文本消息已删除或当前账号没有权限")?;
    if message.media().is_some() {
        bail!("此消息不是纯文本消息，不能作为文本文件保存");
    }
    let text = message.text();
    if text.trim().is_empty() {
        bail!("Telegram 文本消息内容为空");
    }
    let source_info = telegram::message_info(chat_id, &message);
    if source_info
        .unavailable_reason
        .as_deref()
        .is_some_and(|reason| reason.contains("受保护") || reason.contains("限时"))
    {
        bail!("此消息属于受保护或限时内容，不能保存");
    }
    let mut path_info = source_info;
    path_info.media_type = Some("msg".into());
    path_info.file_name = Some(format!("message_{message_id}.txt"));
    path_info.caption = None;
    let settings = shared.settings.read().await.clone();
    if !settings.text_sidecar {
        bail!("请先在设置中启用纯文本消息保存");
    }
    let title = message
        .peer()
        .and_then(|peer| peer.name())
        .unwrap_or("Telegram 聊天")
        .to_owned();
    let file_name = path_info.file_name.as_deref().unwrap_or("message.txt");
    let target = output_path(&settings, &title, file_name, &path_info, message.date());
    let Some(target) =
        apply_duplicate_policy(&target, &settings.duplicate_policy, text.len() as u64)?
    else {
        return Ok(None);
    };
    let record = TaskRecord {
        task_id: uuid::Uuid::new_v4().to_string(),
        chat_id: chat_id.to_owned(),
        chat_title: Some(title),
        message_id: Some(message_id),
        media_type: Some("text".into()),
        file_name: Some(
            target
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ),
        status: "queued".into(),
        progress: 0.0,
        downloaded_bytes: 0,
        total_bytes: Some(text.len() as u64),
        speed_bytes_per_second: 0,
        remaining_bytes: Some(text.len() as u64),
        started_at: None,
        updated_at: Some(chrono::Utc::now().to_rfc3339()),
        completed_at: None,
        output_path: Some(target.to_string_lossy().into_owned()),
        error: None,
        retry_count: 0,
        group_id: None,
    };
    shared.store.create(&record).await?;
    manager.enqueue(record.task_id.clone()).await?;
    shared.publish_task(&record.task_id).await;
    Ok(Some(record))
}

pub async fn create_chat_tasks(
    shared: &SharedState,
    manager: &DownloadManager,
    chat_id: &str,
    start_id: Option<i64>,
    end_id: Option<i64>,
    media_types: &[String],
    file_formats: &std::collections::BTreeMap<String, Vec<String>>,
    filter: Option<&str>,
) -> Result<usize> {
    let service = shared.telegram().await?;
    let mut cursor = 0_i64;
    let mut created = 0_usize;
    let mut skipped = 0_usize;
    let mut limit_reached = false;
    let lower = start_id.unwrap_or(1).min(end_id.unwrap_or(i64::MAX));
    let upper = start_id.unwrap_or(i64::MAX).max(end_id.unwrap_or(1));
    let wanted = media_types
        .iter()
        .map(|item| item.to_lowercase())
        .collect::<Vec<_>>();
    let query = filter.unwrap_or_default().trim();
    let settings = shared.settings.read().await.clone();
    if !query.is_empty() {
        crate::filter::validate(query).context("下载范围中的筛选表达式无效")?;
    }
    loop {
        let (items, next, more) = service.messages(chat_id, cursor, 100).await?;
        if items.is_empty() {
            break;
        }
        let mut reached_start = false;
        for item in items {
            let id = item.message_id;
            if id < lower {
                reached_start = true;
                break;
            }
            if id > upper {
                continue;
            }
            if item.media_type.is_none() {
                if !item
                    .text
                    .as_deref()
                    .is_some_and(|text| !text.trim().is_empty())
                {
                    continue;
                }
                if !settings.text_sidecar {
                    continue;
                }
                if !query.is_empty()
                    && !evaluate(query, &FilterMetadata::from_message(&item))
                        .context("按旧版消息筛选表达式计算失败")?
                {
                    continue;
                }
                if batch_limit_reached(created) {
                    limit_reached = true;
                    break;
                }
                match create_text_task_for_message_if_needed(shared, manager, chat_id, id).await? {
                    Some(_) => created += 1,
                    None => skipped += 1,
                }
                continue;
            }
            if !item.downloadable {
                continue;
            }
            let Some(kind) = item.media_type.as_deref() else {
                continue;
            };
            if !wanted.is_empty() && !wanted.iter().any(|wanted| wanted == kind) {
                continue;
            }
            let name = item.file_name.as_deref().unwrap_or_default();
            if !query.is_empty()
                && !evaluate(query, &FilterMetadata::from_message(&item))
                    .context("按旧版消息筛选表达式计算失败")?
            {
                continue;
            }
            if let Some(extensions) = file_formats.get(kind)
                && !matches_file_format(name, extensions)
            {
                continue;
            }
            if batch_limit_reached(created) {
                limit_reached = true;
                break;
            }
            match create_task_for_message_if_needed(shared, manager, chat_id, id, Some(kind))
                .await?
            {
                Some(_) => created += 1,
                None => skipped += 1,
            }
        }
        if reached_start || !more || limit_reached {
            break;
        }
        cursor = next.context("Telegram 消息分页没有返回游标")?;
    }
    if skipped > 0 {
        shared
            .log(
                "info",
                "download",
                format!("按重复文件“跳过”策略略过 {skipped} 个媒体"),
            )
            .await;
    }
    if limit_reached {
        shared
            .log(
                "warn",
                "download",
                format!("单次范围达到 {MAX_BATCH_TASKS} 个任务上限；其余匹配媒体未入队"),
            )
            .await;
    }
    Ok(created)
}

fn batch_limit_reached(created: usize) -> bool {
    created >= MAX_BATCH_TASKS
}

fn output_path(
    settings: &Settings,
    chat_title: &str,
    file_name: &str,
    info: &MessageInfo,
    date: chrono::DateTime<chrono::Utc>,
) -> PathBuf {
    let root = PathBuf::from(&settings.download_root);
    let replacements = [
        ("{chat}", chat_title.to_owned()),
        ("{year}", date.format("%Y").to_string()),
        ("{month}", date.format("%m").to_string()),
        ("{date}", date.format("%Y%m%d_%H%M%S").to_string()),
        (
            "{media_datetime}",
            date.format(&settings.date_format).to_string(),
        ),
        ("{message_id}", info.message_id.to_string()),
        ("{name}", file_name.to_owned()),
        ("{caption}", info.caption.clone().unwrap_or_default()),
        (
            "{media_type}",
            info.media_type.clone().unwrap_or_else(|| "media".into()),
        ),
    ];
    let mut relative = settings.path_template.clone();
    for (key, value) in &replacements {
        relative = relative.replace(key, value);
    }
    let segments = relative
        .split(['/', '\\'])
        .filter(|part| !part.trim().is_empty())
        .map(safe_component)
        .collect::<Vec<_>>();
    let mut directory = root;
    for segment in segments {
        directory.push(segment);
    }
    let mut filename = settings.file_name_template.clone();
    for (key, value) in &replacements {
        filename = filename.replace(key, value);
    }
    filename = safe_component(&filename);
    let original_ext = Path::new(file_name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_owned);
    if original_ext.is_some() && Path::new(&filename).extension().is_none() {
        filename.push('.');
        filename.push_str(original_ext.as_deref().unwrap_or_default());
    }
    directory.join(filename)
}

async fn verified_final_file_matches_chunks(
    store: &crate::task_store::TaskStore,
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
        // Chunk sizes are bounded by the Telegram request layer. Keep that
        // bound here too in case the persisted database is damaged.
        if length > 1_048_576 {
            return Ok(false);
        }
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
            || *length > 1_048_576
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
    writer: tokio::task::JoinHandle<Result<T>>,
    event_watcher: tokio::task::JoinHandle<()>,
) -> Result<T> {
    event_watcher.abort();
    let _ = event_watcher.await;
    writer.await.context("分块写入线程异常退出")?
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

fn apply_duplicate_policy(
    path: &Path,
    policy: &str,
    expected_size: u64,
) -> Result<Option<PathBuf>> {
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
    for suffix in 1..=100_000_u32 {
        let candidate = parent.join(match &ext {
            Some(ext) => format!("{name} ({suffix}).{ext}"),
            None => format!("{name} ({suffix})"),
        });
        if !candidate.exists() {
            return Ok(Some(candidate));
        }
        if std::fs::metadata(&candidate).is_ok_and(|metadata| metadata.len() == expected_size) {
            continue;
        }
    }
    bail!("无法为重复文件生成不冲突的名称")
}

async fn write_sidecars(output: &Path, info: &MessageInfo, settings: &Settings) -> Result<()> {
    let text = info
        .caption
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    if settings.caption_sidecar {
        if let Some(text) = text {
            let path = output.with_extension(format!(
                "{}.caption.txt",
                output.extension().and_then(|e| e.to_str()).unwrap_or("")
            ));
            tokio::fs::write(path, text).await?;
        }
    }
    Ok(())
}

fn matches_file_format(file_name: &str, allowed: &[String]) -> bool {
    if allowed.is_empty()
        || allowed.iter().any(|value| {
            matches!(
                value
                    .trim()
                    .trim_start_matches('.')
                    .to_ascii_lowercase()
                    .as_str(),
                "all" | "*"
            )
        })
    {
        return true;
    }
    let extension = Path::new(file_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    allowed.iter().any(|value| {
        value
            .trim()
            .trim_start_matches('.')
            .eq_ignore_ascii_case(extension)
    })
}
