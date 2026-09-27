use crate::{
    db_migration::Migrator,
    entities::{chunk, task},
    models::{RuntimeStats, TaskRecord, TaskStateMatch},
};
use anyhow::{Context, Result, bail};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectOptions, Database, DatabaseConnection,
    DbErr, EntityTrait, QueryFilter, QueryOrder, QuerySelect, TransactionTrait, sea_query::Expr,
};
use sea_orm_migration::MigratorTrait;
use std::{path::Path, sync::Arc, time::Duration};
use tokio::sync::Mutex as AsyncMutex;

/// 对应用内的写操作排队。SQLite WAL 允许读并发,但仍只有一个 writer;多个下载任务
/// 同时更新 chunks/tasks 时,依赖连接池的 busy_timeout 仍会在事务升级或写锁竞争时
/// 返回 SQLITE_BUSY。这个锁由所有 TaskStore clone 共享,并覆盖完整事务生命周期。
/// 外部进程造成的短暂冲突仍由 SQLite busy_timeout 和下方退避重试处理。
const BUSY_BACKOFFS: [Duration; 3] = [
    Duration::from_millis(5),
    Duration::from_millis(25),
    Duration::from_millis(125),
];

/// 判 SQLITE_BUSY:sea-orm 2.x 的 DbErr 没有公开 variant,直接看字符串 + 错误码。
fn is_sqlite_busy(error: &DbErr) -> bool {
    if let DbErr::Custom(msg) = error {
        return msg.contains("database is locked");
    }
    let s = error.to_string();
    s.contains("database is locked") || s.contains("(code: 5)")
}

fn log_busy_retry(op: &'static str, attempt: usize, delay: Duration) {
    tracing::debug!(
        target: "desktop::download",
        op = op,
        attempt = attempt,
        delay_ms = delay.as_millis() as u64,
        "SQLITE_BUSY,backing off and retrying"
    );
}

#[derive(Clone)]
pub struct TaskStore {
    db: DatabaseConnection,
    write_gate: Arc<AsyncMutex<()>>,
}

impl TaskStore {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let db_path = path
            .to_string_lossy()
            .replace('\\', "/")
            .replace(' ', "%20")
            .replace('#', "%23");
        let mut options = ConnectOptions::new(format!("sqlite://{db_path}?mode=rwc"));
        options
            .max_connections(8)
            .min_connections(1)
            .connect_timeout(Duration::from_secs(20))
            .sqlx_logging(false);
        options.map_sqlx_sqlite_opts(|sqlite| {
            sqlite
                .foreign_keys(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .busy_timeout(Duration::from_secs(8))
        });
        let db = Database::connect(options)
            .await
            .context("无法打开任务数据库")?;
        // SQLite migrations default to non-transactional execution, so each DDL statement may be
        // handed a different pooled connection. A connection that loaded its schema before an
        // earlier migration added a column will reject a later `DROP COLUMN` with "no such
        // column". Pinning the whole migration run to one connection keeps the schema consistent.
        let migration = db.begin().await.context("无法开启迁移事务")?;
        Migrator::up(&migration, None)
            .await
            .context("任务数据库迁移失败")?;
        migration.commit().await.context("无法提交迁移事务")?;
        let store = Self {
            db,
            write_gate: Arc::new(AsyncMutex::new(())),
        };
        store.recover_interrupted().await?;
        Ok(store)
    }

    /// Close all cloned ORM handles before copying a live SQLite database and its WAL sidecars.
    /// The app restarts immediately after a data-root migration, so this pool is not reused.
    pub async fn close(&self) -> Result<()> {
        self.db
            .close_by_ref()
            .await
            .context("无法安全关闭任务数据库")
    }

    pub async fn create(&self, record: &TaskRecord) -> Result<()> {
        let total = record.total_bytes.unwrap_or(0);
        if total > i64::MAX as u64 {
            bail!("媒体文件长度超出任务数据库可表示范围");
        }
        let now = chrono::Utc::now().to_rfc3339();
        let _write = self.write_gate.lock().await;
        task::ActiveModel {
            id: Set(record.task_id.clone()),
            chat_id: Set(record.chat_id.clone()),
            message_id: Set(record.message_id.unwrap_or_default()),
            title: Set(record.chat_title.clone().unwrap_or_default()),
            media_type: Set(record.media_type.clone().unwrap_or_default()),
            file_name: Set(record.file_name.clone().unwrap_or_default()),
            target_path: Set(record.output_path.clone().unwrap_or_default()),
            total_bytes: Set(total as i64),
            completed_bytes: Set(0),
            speed_bytes_per_second: Set(0),
            status: Set("queued".to_owned()),
            error: Set(None),
            created_at: Set(now.clone()),
            updated_at: Set(now),
            started_at: Set(None),
            completed_at: Set(None),
            group_id: Set(record.group_id.clone()),
            retry_count: Set(0),
        }
        .insert(&self.db)
        .await
        .context("无法创建下载任务")?;
        Ok(())
    }

    pub async fn get(&self, id: &str) -> Result<Option<TaskRecord>> {
        Ok(task::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .map(Into::into))
    }

    pub async fn list(&self, status: Option<&str>, limit: usize) -> Result<Vec<TaskRecord>> {
        let mut query = task::Entity::find()
            .order_by_desc(task::Column::CreatedAt)
            .limit(limit.clamp(1, 2000) as u64);
        if let Some(status) = status {
            query = query.filter(task::Column::Status.eq(status));
        }
        Ok(query
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Latest task for a file name, in the shape the injected Telegram Web viewer understands.
    /// Cancelled rows are treated as absent, so a file whose only task was cancelled resolves to
    /// "none". A paused task is reported as "queued": the viewer button only distinguishes
    /// queued / downloading / completed / failed.
    pub async fn find_latest_by_file_name(&self, file_name: &str) -> Result<TaskStateMatch> {
        let row = task::Entity::find()
            .filter(task::Column::FileName.eq(file_name))
            .filter(task::Column::Status.ne("cancelled"))
            .order_by_desc(task::Column::UpdatedAt)
            .one(&self.db)
            .await?;
        let Some(row) = row else {
            return Ok(TaskStateMatch {
                state: "none".to_owned(),
                task_id: None,
                progress: None,
                file_size: None,
                completed_at: None,
            });
        };
        let total = row.total_bytes.max(0) as u64;
        let done = row.completed_bytes.max(0) as u64;
        let downloading = row.status == "downloading";
        let state = if row.status == "paused" {
            "queued".to_owned()
        } else {
            row.status
        };
        Ok(TaskStateMatch {
            state,
            task_id: Some(row.id),
            progress: (downloading && total > 0)
                .then(|| (done as f64 / total as f64).clamp(0.0, 1.0)),
            file_size: (total > 0).then_some(total),
            completed_at: row.completed_at,
        })
    }

    pub async fn set_status(&self, id: &str, next: &str, error: Option<&str>) -> Result<()> {
        const ALLOWED: &[(&str, &[&str])] = &[
            ("queued", &["downloading", "paused", "cancelled", "failed"]),
            (
                "downloading",
                &["paused", "completed", "failed", "cancelled", "queued"],
            ),
            ("paused", &["queued", "cancelled", "failed"]),
            ("failed", &["queued", "cancelled"]),
            ("completed", &["queued"]),
            ("cancelled", &["queued"]),
        ];
        // Hold the shared writer gate from before BEGIN through COMMIT. This method reads the
        // current status before updating it, so concurrent deferred transactions can otherwise
        // race while upgrading their SQLite read snapshots to writers.
        let _write = self.write_gate.lock().await;
        let transaction = self.db.begin().await?;
        let model = task::Entity::find_by_id(id)
            .one(&transaction)
            .await?
            .with_context(|| format!("未找到下载任务：{id}"))?;
        if !ALLOWED
            .iter()
            .any(|(from, to)| *from == model.status && to.contains(&next))
        {
            bail!("任务状态转换不允许：{} → {}", model.status, next);
        }
        let now = chrono::Utc::now().to_rfc3339();
        let first_start = model.started_at.is_none();
        let should_increment_retry = next == "queued"
            && matches!(model.status.as_str(), "failed" | "cancelled" | "completed");
        let retry_count = model.retry_count;
        let mut active: task::ActiveModel = model.into();
        active.status = Set(next.to_owned());
        active.error = Set(error.map(str::to_owned));
        active.updated_at = Set(now.clone());
        if next == "downloading" && first_start {
            active.started_at = Set(Some(now.clone()));
        }
        if matches!(next, "completed" | "failed" | "cancelled") {
            active.completed_at = Set(Some(now));
        }
        if next == "queued" {
            active.completed_at = Set(None);
        }
        if should_increment_retry {
            active.retry_count = Set(retry_count.saturating_add(1));
        }
        if next != "downloading" {
            active.speed_bytes_per_second = Set(0);
        }
        active.update(&transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn update_progress(&self, id: &str, completed: u64, speed: u64) -> Result<()> {
        let completed = completed.min(i64::MAX as u64) as i64;
        let speed = speed.min(i64::MAX as u64) as i64;
        let id = id.to_owned();
        // 高频进度写走共享 writer gate;额外保留短退避,处理应用外部持有数据库写锁的情况。
        let mut attempt = 0usize;
        loop {
            let now = chrono::Utc::now().to_rfc3339();
            let result = {
                let _write = self.write_gate.lock().await;
                task::Entity::update_many()
                    .filter(task::Column::Id.eq(&id))
                    .col_expr(task::Column::CompletedBytes, Expr::value(completed))
                    .col_expr(task::Column::SpeedBytesPerSecond, Expr::value(speed))
                    .col_expr(task::Column::UpdatedAt, Expr::value(now))
                    .exec(&self.db)
                    .await
            };
            match result {
                Ok(_) => return Ok(()),
                Err(error) if is_sqlite_busy(&error) && attempt < BUSY_BACKOFFS.len() => {
                    let delay = BUSY_BACKOFFS[attempt];
                    log_busy_retry("update_progress", attempt + 1, delay);
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(error) => {
                    return Err(error).context(format!("update_progress ({id})"));
                }
            }
        }
    }

    /// Persist the file size discovered by the page-side probe. WebView tasks are created
    /// without a total (the page learns it from `Content-Range` only when the viewer opens),
    /// so `plan_chunks` records it here before the chunk map is used for resume decisions.
    pub async fn set_total_bytes(&self, id: &str, total_bytes: u64) -> Result<()> {
        if total_bytes > i64::MAX as u64 {
            bail!("媒体文件长度超出任务数据库可表示范围");
        }
        let total = total_bytes as i64;
        let mut attempt = 0usize;
        loop {
            let now = chrono::Utc::now().to_rfc3339();
            let result = {
                let _write = self.write_gate.lock().await;
                task::Entity::update_many()
                    .filter(task::Column::Id.eq(id))
                    .col_expr(task::Column::TotalBytes, Expr::value(total))
                    .col_expr(task::Column::UpdatedAt, Expr::value(now))
                    .exec(&self.db)
                    .await
            };
            match result {
                Ok(_) => return Ok(()),
                Err(error) if is_sqlite_busy(&error) && attempt < BUSY_BACKOFFS.len() => {
                    let delay = BUSY_BACKOFFS[attempt];
                    log_busy_retry("set_total_bytes", attempt + 1, delay);
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(error) => {
                    return Err(error).context(format!("set_total_bytes ({id})"));
                }
            }
        }
    }

    pub async fn set_chunks(&self, id: &str, total_bytes: u64, chunk_size: u64) -> Result<()> {
        if total_bytes > i64::MAX as u64 || chunk_size == 0 || chunk_size > i64::MAX as u64 {
            bail!("任务分块尺寸无效");
        }
        let _write = self.write_gate.lock().await;
        let transaction = self.db.begin().await?;
        chunk::Entity::delete_many()
            .filter(chunk::Column::TaskId.eq(id))
            .exec(&transaction)
            .await?;
        let mut offset = 0_u64;
        while offset < total_bytes {
            let length = chunk_size.min(total_bytes - offset);
            chunk::ActiveModel {
                task_id: Set(id.to_owned()),
                offset_bytes: Set(offset as i64),
                length_bytes: Set(length as i64),
                digest: Set(None),
                complete: Set(false),
                ..Default::default()
            }
            .insert(&transaction)
            .await?;
            offset += length;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn complete_chunk(&self, id: &str, offset: u64, digest: &str) -> Result<()> {
        let model = chunk::Entity::find()
            .filter(chunk::Column::TaskId.eq(id))
            .filter(chunk::Column::OffsetBytes.eq(offset as i64))
            .one(&self.db)
            .await?
            .with_context(|| "分块不在当前任务映射中")?;
        let mut active: chunk::ActiveModel = model.into();
        active.complete = Set(true);
        active.digest = Set(Some(digest.to_owned()));
        // UPDATE 阶段排队并重试,SELECT 阶段不重试(读操作不该放大锁时间)。
        // `ActiveModel::update` 按值消费,所以每次重试前克隆一次。
        let mut attempt = 0usize;
        loop {
            let attempt_model = active.clone();
            let result = {
                let _write = self.write_gate.lock().await;
                attempt_model.update(&self.db).await
            };
            match result {
                Ok(_) => return Ok(()),
                Err(error) if is_sqlite_busy(&error) && attempt < BUSY_BACKOFFS.len() => {
                    let delay = BUSY_BACKOFFS[attempt];
                    log_busy_retry("complete_chunk", attempt + 1, delay);
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(error) => {
                    return Err(error).context(format!("complete_chunk ({id}@{offset})"));
                }
            }
        }
    }

    pub async fn reset_chunk(&self, id: &str, offset: u64) -> Result<()> {
        if let Some(model) = chunk::Entity::find()
            .filter(chunk::Column::TaskId.eq(id))
            .filter(chunk::Column::OffsetBytes.eq(offset as i64))
            .one(&self.db)
            .await?
        {
            let mut active: chunk::ActiveModel = model.into();
            active.complete = Set(false);
            active.digest = Set(None);
            let _write = self.write_gate.lock().await;
            active.update(&self.db).await?;
        }
        Ok(())
    }

    /// 删除任务及其全部分块记录(不可恢复;文件由调用方决定是否一并删除)。
    pub async fn delete(&self, id: &str) -> Result<()> {
        // 整个事务作为一次重试单元;失败后先释放 writer gate,再退避。
        let mut attempt = 0usize;
        loop {
            let result = {
                let _write = self.write_gate.lock().await;
                async {
                    let transaction = self.db.begin().await?;
                    chunk::Entity::delete_many()
                        .filter(chunk::Column::TaskId.eq(id))
                        .exec(&transaction)
                        .await?;
                    task::Entity::delete_by_id(id).exec(&transaction).await?;
                    transaction.commit().await?;
                    Ok::<_, DbErr>(())
                }
                .await
            };
            match result {
                Ok(_) => return Ok(()),
                Err(error) if is_sqlite_busy(&error) && attempt < BUSY_BACKOFFS.len() => {
                    let delay = BUSY_BACKOFFS[attempt];
                    log_busy_retry("delete", attempt + 1, delay);
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(error) => {
                    return Err(error).context(format!("delete ({id})"));
                }
            }
        }
    }

    pub async fn chunk_map(&self, id: &str) -> Result<Vec<(u64, u64, Option<String>, bool)>> {
        let rows = chunk::Entity::find()
            .filter(chunk::Column::TaskId.eq(id))
            .order_by_asc(chunk::Column::OffsetBytes)
            .all(&self.db)
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                (
                    row.offset_bytes.max(0) as u64,
                    row.length_bytes.max(0) as u64,
                    row.digest,
                    row.complete,
                )
            })
            .collect())
    }

    pub async fn stats(&self) -> Result<RuntimeStats> {
        let rows = task::Entity::find()
            .select_only()
            .column(task::Column::Status)
            .column_as(task::Column::Id.count(), "task_count")
            .column_as(task::Column::CompletedBytes.sum(), "completed_sum")
            .column_as(task::Column::SpeedBytesPerSecond.sum(), "speed_sum")
            .group_by(task::Column::Status)
            .into_tuple::<(String, i64, Option<i64>, Option<i64>)>()
            .all(&self.db)
            .await?;
        let mut stats = RuntimeStats::default();
        for (status, count, bytes, speed) in rows {
            match status.as_str() {
                "queued" | "paused" => stats.queued_downloads += count.max(0) as usize,
                "downloading" => stats.active_downloads += count.max(0) as usize,
                "completed" => stats.completed_downloads += count.max(0) as usize,
                "failed" => stats.failed_downloads += count.max(0) as usize,
                _ => {}
            }
            stats.total_downloaded_bytes = stats
                .total_downloaded_bytes
                .saturating_add(bytes.unwrap_or(0).max(0) as u64);
            stats.current_speed_bytes_per_second = stats
                .current_speed_bytes_per_second
                .saturating_add(speed.unwrap_or(0).max(0) as u64);
        }
        let active = task::Entity::find()
            .filter(task::Column::Status.eq("downloading"))
            .all(&self.db)
            .await?;
        let remaining = active
            .iter()
            .map(|row| row.total_bytes.saturating_sub(row.completed_bytes).max(0) as u64)
            .sum::<u64>();
        if stats.current_speed_bytes_per_second > 0 {
            stats.estimated_remaining_seconds =
                Some(remaining.div_ceil(stats.current_speed_bytes_per_second));
        }
        Ok(stats)
    }

    async fn recover_interrupted(&self) -> Result<()> {
        let interrupted = task::Entity::find()
            .filter(task::Column::Status.eq("downloading"))
            .all(&self.db)
            .await?;
        if interrupted.is_empty() {
            return Ok(());
        }
        let _write = self.write_gate.lock().await;
        let transaction = self.db.begin().await?;
        for model in interrupted {
            let mut active: task::ActiveModel = model.into();
            active.status = Set("queued".into());
            active.error = Set(Some("应用意外退出，任务已恢复".into()));
            active.speed_bytes_per_second = Set(0);
            active.updated_at = Set(chrono::Utc::now().to_rfc3339());
            active.update(&transaction).await?;
        }
        transaction.commit().await?;
        Ok(())
    }
}

impl From<task::Model> for TaskRecord {
    fn from(row: task::Model) -> Self {
        let total = row.total_bytes.max(0) as u64;
        let done = row.completed_bytes.max(0) as u64;
        let progress = if total == 0 {
            0.0
        } else {
            ((done as f64 / total as f64) * 100.0).min(100.0)
        };
        Self {
            task_id: row.id,
            chat_id: row.chat_id,
            chat_title: Some(row.title),
            message_id: Some(row.message_id),
            media_type: Some(row.media_type),
            file_name: Some(row.file_name),
            status: row.status,
            progress,
            downloaded_bytes: done,
            total_bytes: (total > 0).then_some(total),
            speed_bytes_per_second: row.speed_bytes_per_second.max(0) as u64,
            remaining_bytes: (total > 0).then_some(total.saturating_sub(done)),
            started_at: row.started_at,
            updated_at: Some(row.updated_at),
            completed_at: row.completed_at,
            output_path: Some(row.target_path),
            cover_path: None,
            error: row.error,
            retry_count: row.retry_count.max(0) as u32,
            group_id: row.group_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn record(id: &str) -> TaskRecord {
        TaskRecord {
            task_id: id.into(),
            chat_id: "-1".into(),
            chat_title: Some("chat".into()),
            message_id: Some(1),
            media_type: Some("video".into()),
            file_name: Some("a.mp4".into()),
            status: "queued".into(),
            progress: 0.0,
            downloaded_bytes: 0,
            total_bytes: Some(1024),
            speed_bytes_per_second: 0,
            remaining_bytes: Some(1024),
            started_at: None,
            updated_at: None,
            completed_at: None,
            output_path: Some("test/a.mp4".into()),
            cover_path: None,
            error: None,
            retry_count: 0,
            group_id: None,
        }
    }

    #[tokio::test]
    async fn task_state_transitions_reject_impossible_success() {
        let dir = tempdir().unwrap();
        let store = TaskStore::open(&dir.path().join("db.sqlite"))
            .await
            .unwrap();
        store.create(&record("t1")).await.unwrap();
        assert!(store.set_status("t1", "completed", None).await.is_err());
        store.set_status("t1", "downloading", None).await.unwrap();
        store.set_status("t1", "completed", None).await.unwrap();
        assert_eq!(store.get("t1").await.unwrap().unwrap().status, "completed");
    }

    #[tokio::test]
    async fn webview_task_records_the_probed_total_after_creation() {
        let dir = tempdir().unwrap();
        let store = TaskStore::open(&dir.path().join("db.sqlite"))
            .await
            .unwrap();
        let mut probe_task = record("w1");
        probe_task.total_bytes = None;
        probe_task.remaining_bytes = None;
        store.create(&probe_task).await.unwrap();
        assert!(
            store
                .get("w1")
                .await
                .unwrap()
                .unwrap()
                .total_bytes
                .is_none()
        );

        store.set_total_bytes("w1", 4096).await.unwrap();

        let stored = store.get("w1").await.unwrap().unwrap();
        assert_eq!(stored.total_bytes, Some(4096));
        assert_eq!(stored.remaining_bytes, Some(4096));
    }

    #[tokio::test]
    async fn interrupted_tasks_requeue_and_chunk_map_is_durable() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db.sqlite");
        let store = TaskStore::open(&path).await.unwrap();
        store.create(&record("t2")).await.unwrap();
        store.set_status("t2", "downloading", None).await.unwrap();
        store.set_chunks("t2", 1024, 512).await.unwrap();
        drop(store);
        let store = TaskStore::open(&path).await.unwrap();
        assert_eq!(store.get("t2").await.unwrap().unwrap().status, "queued");
        assert_eq!(store.chunk_map("t2").await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn latest_task_state_by_file_name_resolves_none_completed_and_downloading() {
        let dir = tempdir().unwrap();
        let store = TaskStore::open(&dir.path().join("db.sqlite"))
            .await
            .unwrap();

        let missing = store.find_latest_by_file_name("missing.mp4").await.unwrap();
        assert_eq!(missing.state, "none");
        assert!(missing.task_id.is_none());
        assert!(missing.progress.is_none());
        assert!(missing.file_size.is_none());
        assert!(missing.completed_at.is_none());

        store.create(&record("t1")).await.unwrap();
        store.set_status("t1", "downloading", None).await.unwrap();
        store.set_status("t1", "completed", None).await.unwrap();
        let completed = store.find_latest_by_file_name("a.mp4").await.unwrap();
        assert_eq!(completed.state, "completed");
        assert_eq!(completed.task_id.as_deref(), Some("t1"));
        assert!(completed.progress.is_none());
        assert_eq!(completed.file_size, Some(1024));
        assert!(completed.completed_at.is_some());

        let mut second = record("t2");
        second.file_name = Some("b.mp4".into());
        store.create(&second).await.unwrap();
        store.set_status("t2", "downloading", None).await.unwrap();
        store.update_progress("t2", 256, 0).await.unwrap();
        let downloading = store.find_latest_by_file_name("b.mp4").await.unwrap();
        assert_eq!(downloading.state, "downloading");
        assert_eq!(downloading.task_id.as_deref(), Some("t2"));
        assert_eq!(downloading.progress, Some(0.25));
        assert_eq!(downloading.file_size, Some(1024));
        assert!(downloading.completed_at.is_none());
    }

    #[tokio::test]
    async fn delete_removes_the_task_and_its_chunk_map() {
        let dir = tempdir().unwrap();
        let store = TaskStore::open(&dir.path().join("db.sqlite"))
            .await
            .unwrap();
        store.create(&record("t1")).await.unwrap();
        store.set_chunks("t1", 2048, 1024).await.unwrap();
        store.complete_chunk("t1", 0, "digest").await.unwrap();
        assert_eq!(store.chunk_map("t1").await.unwrap().len(), 2);

        store.delete("t1").await.unwrap();

        assert!(store.get("t1").await.unwrap().is_none());
        assert!(store.chunk_map("t1").await.unwrap().is_empty());
        assert!(store.list(None, 10).await.unwrap().is_empty());
        // 删除不存在的任务不报错(幂等)。
        store.delete("t1").await.unwrap();
    }

    /// SQLITE_BUSY 字符串匹配能识别真锁。生产中我们靠 sea-orm 把 SQLx 错包成
    /// "(code: 5) database is locked",这里直接走字符串路径,避免依赖 sea-orm 内部枚举。
    #[test]
    fn busy_string_match_recognizes_code_5() {
        let cases = [
            DbErr::Custom("Query Error: error returned from database: (code: 5) database is locked".into()),
            DbErr::Custom("error returned from database: (code: 5) database is locked: error returned from database: (code: 5) database is locked".into()),
        ];
        for case in cases {
            assert!(is_sqlite_busy(&case), "should recognize: {case}");
        }
        let not_busy = DbErr::Custom("no such column: foo".into());
        assert!(!is_sqlite_busy(&not_busy));
    }

    /// 写路径在偶发 SQLITE_BUSY 下应当退避重试并最终成功 —— 用一个短超时(1ms)
    /// 模拟"第一次被撞、第二次让出来",验证 `update_progress` 不再向上抛 code: 5。
    #[tokio::test]
    async fn update_progress_survives_an_occasional_busy_retry() {
        let dir = tempdir().unwrap();
        let store = TaskStore::open(&dir.path().join("db.sqlite"))
            .await
            .unwrap();
        store.create(&record("t1")).await.unwrap();
        // busy_timeout 设到 0,海面立刻撞锁 → 让我们的退避自己吃掉。
        sqlx::query("PRAGMA busy_timeout = 0")
            .execute(&store.db)
            .await
            .unwrap();
        // 启动一个长事务,持有 task 表的写锁,等另一线程 update_progress 重试一次。
        let busy_conn = sqlx::query("BEGIN IMMEDIATE")
            .execute(&store.db)
            .await
            .unwrap();
        let store2 = store.clone();
        let updater = tokio::spawn(async move {
            // 第一次会拿到 SQLITE_BUSY → 退避 → 等我们 COMMIT → 第二次成功。
            tokio::time::sleep(Duration::from_millis(40)).await;
            let store2_cloned = store2.clone();
            tokio::spawn(async move {
                sqlx::query("COMMIT")
                    .execute(&store2_cloned.db)
                    .await
                    .unwrap();
            })
            .await
            .unwrap();
        });
        let result = store.update_progress("t1", 42, 7).await;
        updater.await.unwrap();
        drop(busy_conn);
        assert!(
            result.is_ok(),
            "update_progress should retry past a transient busy: {result:?}"
        );
        let row = store.get("t1").await.unwrap().unwrap();
        assert_eq!(row.completed_bytes, 42);
        assert_eq!(row.speed_bytes_per_second, 7);
    }
}
