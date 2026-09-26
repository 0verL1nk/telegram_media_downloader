use crate::task_store::TaskStore;
use anyhow::{Context, Result, bail};
use std::time::{Duration, Instant};
use std::{collections::BTreeMap, path::Path};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom},
    sync::{broadcast, mpsc},
};

/// 速率采样的固定时间窗:窗口内累计字节除以窗口时长。
///
/// 逐块采样会把突发到达(两块间隔可低至毫秒级)当成瞬时速率,UI 速度/ETA
/// 会突然飙高几个数量级;固定窗口 + EMA 平滑后读数才稳定可信。
const RATE_WINDOW: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub struct IncomingChunk {
    pub offset: u64,
    pub data: Vec<u8>,
}

pub async fn configure_chunk_map(
    store: &TaskStore,
    task_id: &str,
    total_bytes: u64,
    chunk_size: u64,
) -> Result<()> {
    if total_bytes == 0 {
        bail!("零长度媒体不支持分块下载");
    }
    if !(1024..=1_048_576).contains(&chunk_size)
        || chunk_size % 1024 != 0
        || 1_048_576 % chunk_size != 0
    {
        bail!("Telegram 分块大小必须是可整除 1 MiB 的 1 KiB 倍数，且不超过 1 MiB");
    }
    let existing = store.chunk_map(task_id).await?;
    if existing.is_empty() {
        return store.set_chunks(task_id, total_bytes, chunk_size).await;
    }
    let mut offset = 0_u64;
    for (stored_offset, length, _, _) in &existing {
        if *stored_offset != offset || *length == 0 {
            bail!("已保存的下载分块映射无效，无法安全续传");
        }
        offset = offset.checked_add(*length).context("分块偏移溢出")?;
    }
    if offset != total_bytes {
        bail!("媒体长度已变化，已有分块映射无法用于续传");
    }
    Ok(())
}

pub async fn verify_resumable_chunks(
    store: &TaskStore,
    task_id: &str,
    temp_path: &Path,
) -> Result<u64> {
    let mut file = match File::open(temp_path).await {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // 临时文件已丢失(取消时的清理、外部删除等):已标记完成的分块不再有本地数据
            // 支撑,必须全部退回待下载。否则缺块计划会给出空清单,页面不抓取任何分块,
            // 提交时才发现无文件可提交,任务永久卡死。
            for (offset, _, _, complete) in store.chunk_map(task_id).await? {
                if complete {
                    store.reset_chunk(task_id, offset).await?;
                }
            }
            return Ok(0);
        }
        Err(err) => return Err(err.into()),
    };
    let len = file.metadata().await?.len();
    let mut confirmed = 0_u64;
    for (offset, size, digest, complete) in store.chunk_map(task_id).await? {
        if !complete {
            continue;
        }
        let mut bytes = vec![0_u8; size as usize];
        // 分块必须完整落在临时文件实际长度内,且与记录的摘要一致;否则退回待下载
        // (文件被截断时,超出实际长度的分块不能计入续传进度)。
        let valid = if offset.saturating_add(size) <= len && digest.is_some() {
            file.seek(SeekFrom::Start(offset)).await?;
            file.read_exact(&mut bytes).await.is_ok()
                && blake3::hash(&bytes).to_hex().as_str() == digest.as_deref().unwrap_or_default()
        } else {
            false
        };
        if valid {
            confirmed += size;
        } else {
            store.reset_chunk(task_id, offset).await?;
        }
    }
    Ok(confirmed)
}

pub async fn write_chunks_bounded(
    store: TaskStore,
    task_id: String,
    temp_path: &Path,
    total_bytes: u64,
    mut receiver: mpsc::Receiver<IncomingChunk>,
    progress_events: broadcast::Sender<()>,
) -> Result<u64> {
    if let Some(parent) = temp_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(temp_path)
        .await?;
    file.set_len(total_bytes).await?;
    let map: BTreeMap<u64, u64> = store
        .chunk_map(&task_id)
        .await?
        .into_iter()
        .map(|(offset, len, _, _)| (offset, len))
        .collect();
    let mut already = verify_resumable_chunks(&store, &task_id, temp_path).await?;
    store.update_progress(&task_id, already, 0).await?;
    let _ = progress_events.send(());
    let mut rate_window_started = Instant::now();
    let mut rate_window_bytes = already;
    let mut smoothed_speed = 0.0_f64;
    let mut completed_offsets = store
        .chunk_map(&task_id)
        .await?
        .into_iter()
        .filter_map(|(offset, _, _, complete)| complete.then_some(offset))
        .collect::<std::collections::HashSet<_>>();
    while let Some(chunk) = receiver.recv().await {
        let expected = map
            .get(&chunk.offset)
            .copied()
            .context("收到任务分块表中不存在的偏移")?;
        if chunk.data.len() as u64 != expected {
            bail!(
                "分块长度不匹配：偏移 {}，期望 {}，实际 {}",
                chunk.offset,
                expected,
                chunk.data.len()
            );
        }
        if chunk.offset.saturating_add(expected) > total_bytes {
            bail!("分块超出媒体大小");
        }
        if completed_offsets.contains(&chunk.offset) {
            continue;
        }
        file.seek(SeekFrom::Start(chunk.offset)).await?;
        file.write_all(&chunk.data).await?;
        file.flush().await?;
        let digest = blake3::hash(&chunk.data).to_hex().to_string();
        store
            .complete_chunk(&task_id, chunk.offset, &digest)
            .await?;
        completed_offsets.insert(chunk.offset);
        already = already.saturating_add(expected);
        let elapsed = rate_window_started.elapsed();
        if elapsed >= RATE_WINDOW {
            let window_speed =
                already.saturating_sub(rate_window_bytes) as f64 / elapsed.as_secs_f64();
            smoothed_speed = if smoothed_speed == 0.0 {
                window_speed
            } else {
                smoothed_speed * 0.6 + window_speed * 0.4
            };
            rate_window_started = Instant::now();
            rate_window_bytes = already;
        }
        store
            .update_progress(
                &task_id,
                already,
                smoothed_speed.min(u64::MAX as f64) as u64,
            )
            .await?;
        let _ = progress_events.send(());
    }
    file.sync_all().await?;
    let chunks = store.chunk_map(&task_id).await?;
    if chunks.iter().any(|(_, _, _, complete)| !complete) {
        bail!("仍有未完成的下载分块");
    }
    if chunks.iter().map(|(_, len, _, _)| len).sum::<u64>() != total_bytes {
        bail!("分块映射与预期文件长度不一致");
    }
    Ok(total_bytes)
}

pub fn commit_completed_file(temp_path: &Path, destination: &Path, overwrite: bool) -> Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let metadata = std::fs::metadata(temp_path)
        .with_context(|| format!("临时文件丢失：{}", temp_path.display()))?;
    if metadata.len() == 0 {
        bail!("拒绝提交空文件");
    }
    if destination.exists() && !overwrite {
        bail!("目标文件已存在；请先应用重复文件策略");
    }
    atomic_replace(temp_path, destination, overwrite)
        .with_context(|| "无法将已验证的临时文件原子移动到目标位置")?;
    Ok(())
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path, overwrite: bool) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let flags = MOVEFILE_WRITE_THROUGH
        | if overwrite {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    // SAFETY: both UTF-16 paths are nul-terminated for this call and remain alive until it returns.
    let result = unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), flags) };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path, _overwrite: bool) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TaskRecord;
    use std::fs;
    use tokio::sync::mpsc;

    fn record(path: &Path) -> TaskRecord {
        TaskRecord {
            task_id: "chunk-task".into(),
            chat_id: "-1001".into(),
            chat_title: Some("test".into()),
            message_id: Some(7),
            media_type: Some("document".into()),
            file_name: Some("data.bin".into()),
            status: "queued".into(),
            progress: 0.0,
            downloaded_bytes: 0,
            total_bytes: Some(2048),
            speed_bytes_per_second: 0,
            remaining_bytes: Some(2048),
            started_at: None,
            updated_at: None,
            completed_at: None,
            output_path: Some(path.join("final.bin").to_string_lossy().into()),
            error: None,
            retry_count: 0,
            group_id: None,
        }
    }

    #[tokio::test]
    async fn out_of_order_chunks_are_written_at_offsets_and_detect_gaps() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path().to_path_buf();
        let store = TaskStore::open(&root.join("db.sqlite")).await.unwrap();
        store.create(&record(&root)).await.unwrap();
        configure_chunk_map(&store, "chunk-task", 2048, 1024)
            .await
            .unwrap();
        let (tx, rx) = mpsc::channel(2);
        let temp = root.join("part.tmp");
        let (progress, _events) = broadcast::channel(8);
        let task_path = temp.clone();
        let writer = tokio::spawn(async move {
            write_chunks_bounded(
                store.clone(),
                "chunk-task".into(),
                &task_path,
                2048,
                rx,
                progress,
            )
            .await
        });
        tx.send(IncomingChunk {
            offset: 1024,
            data: vec![2; 1024],
        })
        .await
        .unwrap();
        tx.send(IncomingChunk {
            offset: 0,
            data: vec![1; 1024],
        })
        .await
        .unwrap();
        drop(tx);
        assert_eq!(writer.await.unwrap().unwrap(), 2048);
        let bytes = fs::read(temp).unwrap();
        assert!(bytes[..1024].iter().all(|b| *b == 1));
        assert!(bytes[1024..].iter().all(|b| *b == 2));
    }

    #[tokio::test]
    async fn incomplete_chunks_are_not_success_and_corruption_invalidates_resume() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path().to_path_buf();
        let store = TaskStore::open(&root.join("db.sqlite")).await.unwrap();
        store.create(&record(&root)).await.unwrap();
        configure_chunk_map(&store, "chunk-task", 2048, 1024)
            .await
            .unwrap();
        let temp = root.join("part.tmp");
        fs::write(&temp, vec![0_u8; 2048]).unwrap();
        assert_eq!(
            verify_resumable_chunks(&store, "chunk-task", &temp)
                .await
                .unwrap(),
            0
        );
        assert!(
            store
                .chunk_map("chunk-task")
                .await
                .unwrap()
                .iter()
                .all(|(_, _, _, done)| !done)
        );
    }

    #[tokio::test]
    async fn rejects_incomplete_chunk_maps_after_writer_exits() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path().to_path_buf();
        let store = TaskStore::open(&root.join("db.sqlite")).await.unwrap();
        store.create(&record(&root)).await.unwrap();
        configure_chunk_map(&store, "chunk-task", 2048, 1024)
            .await
            .unwrap();
        let (tx, rx) = mpsc::channel(1);
        drop(tx);
        let task_store = store.clone();
        let temp = root.join("part.tmp");
        let (progress, _events) = broadcast::channel(8);
        let result =
            write_chunks_bounded(task_store, "chunk-task".into(), &temp, 2048, rx, progress).await;
        assert!(result.is_err());
    }

    /// 写满一个任务的全部块,返回临时文件路径。
    async fn write_full_task(store: &TaskStore, root: &Path) -> std::path::PathBuf {
        store.create(&record(root)).await.unwrap();
        configure_chunk_map(store, "chunk-task", 2048, 1024)
            .await
            .unwrap();
        let (tx, rx) = mpsc::channel(2);
        let temp = root.join("part.tmp");
        let (progress, _events) = broadcast::channel(8);
        let task_path = temp.clone();
        let writer_store = store.clone();
        let writer = tokio::spawn(async move {
            write_chunks_bounded(
                writer_store,
                "chunk-task".into(),
                &task_path,
                2048,
                rx,
                progress,
            )
            .await
        });
        tx.send(IncomingChunk {
            offset: 0,
            data: vec![1; 1024],
        })
        .await
        .unwrap();
        tx.send(IncomingChunk {
            offset: 1024,
            data: vec![2; 1024],
        })
        .await
        .unwrap();
        drop(tx);
        assert_eq!(writer.await.unwrap().unwrap(), 2048);
        assert!(
            store
                .chunk_map("chunk-task")
                .await
                .unwrap()
                .iter()
                .all(|(_, _, _, done)| *done)
        );
        temp
    }

    #[tokio::test]
    async fn missing_temp_file_returns_completed_chunks_to_pending() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path().to_path_buf();
        let store = TaskStore::open(&root.join("db.sqlite")).await.unwrap();
        let temp = write_full_task(&store, &root).await;

        fs::remove_file(&temp).unwrap();
        assert_eq!(
            verify_resumable_chunks(&store, "chunk-task", &temp)
                .await
                .unwrap(),
            0
        );
        assert!(
            store
                .chunk_map("chunk-task")
                .await
                .unwrap()
                .iter()
                .all(|(_, _, _, done)| !done),
            "临时文件丢失后所有分块必须重新变为待下载,否则缺块计划会误报已完成"
        );
    }

    #[tokio::test]
    async fn truncated_temp_file_returns_orphaned_chunks_to_pending() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path().to_path_buf();
        let store = TaskStore::open(&root.join("db.sqlite")).await.unwrap();
        let temp = write_full_task(&store, &root).await;

        let file = OpenOptions::new().write(true).open(&temp).await.unwrap();
        file.set_len(1024).await.unwrap();
        drop(file);

        assert_eq!(
            verify_resumable_chunks(&store, "chunk-task", &temp)
                .await
                .unwrap(),
            1024
        );
        let map = store.chunk_map("chunk-task").await.unwrap();
        assert!(map[0].3, "文件长度内的分块保持完成");
        assert!(!map[1].3, "超出文件实际长度的分块必须退回待下载");
    }
}
