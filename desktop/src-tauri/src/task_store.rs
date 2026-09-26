use crate::{
    db_migration::Migrator,
    entities::{chunk, legacy_retry, task, telegram_transfer_task, upload_task},
    models::{
        LegacyRetryRecord, RuntimeStats, TaskRecord, TaskStateMatch, TelegramTransferRecord,
        UploadTaskRecord,
    },
};
use anyhow::{Context, Result, bail};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectOptions, Database, DatabaseConnection,
    EntityTrait, QueryFilter, QueryOrder, QuerySelect, TransactionTrait, sea_query::Expr,
};
use sea_orm_migration::MigratorTrait;
use std::{path::Path, time::Duration};

#[derive(Clone)]
pub struct TaskStore {
    db: DatabaseConnection,
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
        let store = Self { db };
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

    pub async fn create_upload_task(&self, record: &UploadTaskRecord) -> Result<()> {
        if record.total_bytes > i64::MAX as u64 {
            bail!("上传文件长度超出数据库可表示范围");
        }
        upload_task::ActiveModel {
            id: Set(record.upload_task_id.clone()),
            source_task_id: Set(record.source_task_id.clone()),
            source_path: Set(record.source_path.clone()),
            remote_path: Set(record.remote_path.clone()),
            executable_path: Set(record.executable_path.clone()),
            zip_before_upload: Set(record.zip_before_upload),
            delete_local_after_upload: Set(record.delete_local_after_upload),
            status: Set("queued".into()),
            uploaded_bytes: Set(0),
            total_bytes: Set(record.total_bytes as i64),
            speed_bytes_per_second: Set(0),
            error: Set(None),
            created_at: Set(record.created_at.clone()),
            updated_at: Set(record.updated_at.clone()),
            completed_at: Set(None),
            retry_count: Set(0),
        }
        .insert(&self.db)
        .await
        .context("无法创建云上传任务")?;
        Ok(())
    }

    pub async fn create_telegram_transfer_task(
        &self,
        record: &TelegramTransferRecord,
    ) -> Result<()> {
        if record
            .total_bytes
            .is_some_and(|bytes| bytes > i64::MAX as u64)
        {
            bail!("Telegram 传输文件长度超出数据库可表示范围");
        }
        telegram_transfer_task::ActiveModel {
            id: Set(record.transfer_task_id.clone()),
            kind: Set(record.kind.clone()),
            source_task_id: Set(record.source_task_id.clone()),
            source_chat_id: Set(record.source_chat_id.clone()),
            source_message_id: Set(record.source_message_id),
            target_chat_id: Set(record.target_chat_id.clone()),
            delete_local_after_upload: Set(record.delete_local_after_upload),
            status: Set("queued".to_owned()),
            total_bytes: Set(record.total_bytes.map(|bytes| bytes as i64)),
            sent_bytes: Set(None),
            result_message_id: Set(None),
            error: Set(None),
            created_at: Set(record.created_at.clone()),
            updated_at: Set(record.updated_at.clone()),
            started_at: Set(None),
            completed_at: Set(None),
            retry_count: Set(0),
        }
        .insert(&self.db)
        .await
        .context("无法创建 Telegram 传输任务")?;
        Ok(())
    }

    pub async fn get_telegram_transfer_task(
        &self,
        id: &str,
    ) -> Result<Option<TelegramTransferRecord>> {
        Ok(telegram_transfer_task::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .map(Into::into))
    }

    pub async fn list_telegram_transfer_tasks(
        &self,
        status: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TelegramTransferRecord>> {
        let mut query = telegram_transfer_task::Entity::find()
            .order_by_desc(telegram_transfer_task::Column::CreatedAt)
            .limit(limit.clamp(1, 2000) as u64);
        if let Some(status) = status {
            query = query.filter(telegram_transfer_task::Column::Status.eq(status));
        }
        Ok(query
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Update transfer phase and optional confirmed receipt atomically. In particular, `sending`
    /// is committed before the Telegram RPC so recovery can distinguish a safe requeue from an
    /// ambiguous server-side result.
    pub async fn set_telegram_transfer_status(
        &self,
        id: &str,
        next: &str,
        error: Option<&str>,
        result_message_id: Option<i64>,
        sent_bytes: Option<u64>,
    ) -> Result<()> {
        const ALLOWED: &[(&str, &[&str])] = &[
            ("queued", &["preparing", "uploading", "cancelled"]),
            ("preparing", &["sending", "failed", "queued"]),
            ("uploading", &["sending", "failed", "queued"]),
            ("sending", &["completed", "uncertain"]),
            ("failed", &["queued", "cancelled"]),
            ("cancelled", &["queued"]),
        ];
        let transaction = self.db.begin().await?;
        let model = telegram_transfer_task::Entity::find_by_id(id)
            .one(&transaction)
            .await?
            .with_context(|| format!("未找到 Telegram 传输任务：{id}"))?;
        if !ALLOWED
            .iter()
            .any(|(from, to)| *from == model.status && to.contains(&next))
        {
            bail!("Telegram 传输状态转换不允许：{} → {next}", model.status);
        }
        if next == "completed" && result_message_id.is_none_or(|message_id| message_id <= 0) {
            bail!("Telegram 传输缺少可确认的消息 ID，不能标记成功");
        }
        if let Some(bytes) = sent_bytes
            && bytes > i64::MAX as u64
        {
            bail!("Telegram 发送文件长度超出数据库可表示范围");
        }

        let now = chrono::Utc::now().to_rfc3339();
        let mut active: telegram_transfer_task::ActiveModel = model.clone().into();
        active.status = Set(next.to_owned());
        active.error = Set(error.map(str::to_owned));
        active.updated_at = Set(now.clone());
        if matches!(next, "preparing" | "uploading" | "sending") && model.started_at.is_none() {
            active.started_at = Set(Some(now.clone()));
        }
        if matches!(next, "completed" | "failed" | "cancelled" | "uncertain") {
            active.completed_at = Set(Some(now));
        }
        if next == "queued" {
            active.completed_at = Set(None);
            active.error = Set(None);
            active.result_message_id = Set(None);
            active.sent_bytes = Set(None);
            if matches!(model.status.as_str(), "failed" | "cancelled") {
                active.retry_count = Set(model.retry_count.saturating_add(1));
            }
        }
        if let Some(message_id) = result_message_id {
            active.result_message_id = Set(Some(message_id));
        }
        if let Some(bytes) = sent_bytes {
            active.sent_bytes = Set(Some(bytes as i64));
        }
        active.update(&transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn get_upload_task(&self, id: &str) -> Result<Option<UploadTaskRecord>> {
        Ok(upload_task::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .map(Into::into))
    }

    pub async fn list_upload_tasks(
        &self,
        status: Option<&str>,
        limit: usize,
    ) -> Result<Vec<UploadTaskRecord>> {
        let mut query = upload_task::Entity::find()
            .order_by_desc(upload_task::Column::CreatedAt)
            .limit(limit.clamp(1, 2000) as u64);
        if let Some(status) = status {
            query = query.filter(upload_task::Column::Status.eq(status));
        }
        Ok(query
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub async fn set_upload_status(&self, id: &str, next: &str, error: Option<&str>) -> Result<()> {
        const ALLOWED: &[(&str, &[&str])] = &[
            ("queued", &["uploading", "cancelled"]),
            ("uploading", &["completed", "failed", "cancelled", "queued"]),
            ("failed", &["queued", "cancelled"]),
            ("cancelled", &["queued"]),
            ("completed", &["queued"]),
        ];
        let transaction = self.db.begin().await?;
        let model = upload_task::Entity::find_by_id(id)
            .one(&transaction)
            .await?
            .with_context(|| format!("未找到云上传任务：{id}"))?;
        if !ALLOWED
            .iter()
            .any(|(from, to)| *from == model.status && to.contains(&next))
        {
            bail!("云上传状态转换不允许：{} → {next}", model.status);
        }
        let now = chrono::Utc::now().to_rfc3339();
        let mut active: upload_task::ActiveModel = model.clone().into();
        active.status = Set(next.to_owned());
        active.error = Set(error.map(str::to_owned));
        active.updated_at = Set(now.clone());
        if matches!(next, "completed" | "failed" | "cancelled") {
            active.completed_at = Set(Some(now));
        }
        if next == "queued" {
            active.completed_at = Set(None);
            if matches!(model.status.as_str(), "failed" | "cancelled" | "completed") {
                active.retry_count = Set(model.retry_count.saturating_add(1));
            }
        }
        if next != "uploading" {
            active.speed_bytes_per_second = Set(0);
        }
        active.update(&transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn update_upload_progress(&self, id: &str, uploaded: u64, speed: u64) -> Result<()> {
        let model = upload_task::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .with_context(|| format!("未找到云上传任务：{id}"))?;
        let mut active: upload_task::ActiveModel = model.clone().into();
        active.uploaded_bytes = Set(uploaded
            .min(model.total_bytes.max(0) as u64)
            .min(i64::MAX as u64) as i64);
        active.speed_bytes_per_second = Set(speed.min(i64::MAX as u64) as i64);
        active.updated_at = Set(chrono::Utc::now().to_rfc3339());
        active.update(&self.db).await?;
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
        task::Entity::update_many()
            .filter(task::Column::Id.eq(id))
            .col_expr(
                task::Column::CompletedBytes,
                Expr::value(completed.min(i64::MAX as u64) as i64),
            )
            .col_expr(
                task::Column::SpeedBytesPerSecond,
                Expr::value(speed.min(i64::MAX as u64) as i64),
            )
            .col_expr(
                task::Column::UpdatedAt,
                Expr::value(chrono::Utc::now().to_rfc3339()),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }

    pub async fn set_chunks(&self, id: &str, total_bytes: u64, chunk_size: u64) -> Result<()> {
        if total_bytes > i64::MAX as u64 || chunk_size == 0 || chunk_size > i64::MAX as u64 {
            bail!("任务分块尺寸无效");
        }
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
        active.update(&self.db).await?;
        Ok(())
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
            active.update(&self.db).await?;
        }
        Ok(())
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

    pub async fn import_retry_ids(&self, chat_id: &str, ids: &[i64]) -> Result<usize> {
        let transaction = self.db.begin().await?;
        let now = chrono::Utc::now().to_rfc3339();
        let mut inserted = 0;
        for id in ids.iter().copied().filter(|id| *id > 0) {
            let key = format!("{chat_id}:{id}");
            if legacy_retry::Entity::find_by_id(&key)
                .one(&transaction)
                .await?
                .is_none()
            {
                legacy_retry::ActiveModel {
                    id: Set(key),
                    chat_id: Set(chat_id.to_owned()),
                    message_id: Set(id),
                    imported_at: Set(now.clone()),
                    claimed: Set(false),
                }
                .insert(&transaction)
                .await?;
                inserted += 1;
            }
        }
        transaction.commit().await?;
        Ok(inserted)
    }

    pub async fn list_pending_retry_ids(&self, limit: usize) -> Result<Vec<LegacyRetryRecord>> {
        let rows = legacy_retry::Entity::find()
            .filter(legacy_retry::Column::Claimed.eq(false))
            .order_by_asc(legacy_retry::Column::ImportedAt)
            .limit(limit.clamp(1, 5000) as u64)
            .all(&self.db)
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| LegacyRetryRecord {
                chat_id: row.chat_id,
                message_id: row.message_id,
                imported_at: row.imported_at,
            })
            .collect())
    }

    /// Mark an imported retry ID consumed only after an equivalent task exists
    /// or its source has been intentionally skipped as a duplicate.
    pub async fn mark_retry_id_claimed(&self, chat_id: &str, message_id: i64) -> Result<bool> {
        let key = format!("{chat_id}:{message_id}");
        let Some(row) = legacy_retry::Entity::find_by_id(&key).one(&self.db).await? else {
            return Ok(false);
        };
        if row.claimed {
            return Ok(false);
        }
        let mut active: legacy_retry::ActiveModel = row.into();
        active.claimed = Set(true);
        active.update(&self.db).await?;
        Ok(true)
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
        let interrupted_uploads = upload_task::Entity::find()
            .filter(upload_task::Column::Status.eq("uploading"))
            .all(&self.db)
            .await?;
        let interrupted_transfers = telegram_transfer_task::Entity::find()
            .filter(telegram_transfer_task::Column::Status.is_in([
                "preparing",
                "uploading",
                "sending",
            ]))
            .all(&self.db)
            .await?;
        if interrupted.is_empty()
            && interrupted_uploads.is_empty()
            && interrupted_transfers.is_empty()
        {
            return Ok(());
        }
        let transaction = self.db.begin().await?;
        for model in interrupted {
            let mut active: task::ActiveModel = model.into();
            active.status = Set("queued".into());
            active.error = Set(Some("应用意外退出，任务已恢复".into()));
            active.speed_bytes_per_second = Set(0);
            active.updated_at = Set(chrono::Utc::now().to_rfc3339());
            active.update(&transaction).await?;
        }
        for model in interrupted_uploads {
            let mut active: upload_task::ActiveModel = model.into();
            active.status = Set("queued".into());
            active.error = Set(Some("应用意外退出，云上传任务已恢复".into()));
            active.speed_bytes_per_second = Set(0);
            active.updated_at = Set(chrono::Utc::now().to_rfc3339());
            active.update(&transaction).await?;
        }
        for model in interrupted_transfers {
            let was_sending = model.status == "sending";
            let mut active: telegram_transfer_task::ActiveModel = model.into();
            active.status = Set(if was_sending { "uncertain" } else { "queued" }.into());
            active.error = Set(Some(if was_sending {
                "应用在 Telegram 发送请求期间退出；服务端是否已接收无法确认，请先检查目标聊天，避免重复发送".into()
            } else {
                "应用意外退出，Telegram 传输尚未进入发送请求阶段，已安全重新排队".into()
            }));
            active.updated_at = Set(chrono::Utc::now().to_rfc3339());
            if was_sending {
                active.completed_at = Set(Some(chrono::Utc::now().to_rfc3339()));
            } else {
                active.completed_at = Set(None);
            }
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
            error: row.error,
            retry_count: row.retry_count.max(0) as u32,
            group_id: row.group_id,
        }
    }
}

impl From<upload_task::Model> for UploadTaskRecord {
    fn from(row: upload_task::Model) -> Self {
        let total = row.total_bytes.max(0) as u64;
        let uploaded = (row.uploaded_bytes.max(0) as u64).min(total);
        Self {
            upload_task_id: row.id,
            source_task_id: row.source_task_id,
            source_path: row.source_path,
            remote_path: row.remote_path,
            executable_path: row.executable_path,
            zip_before_upload: row.zip_before_upload,
            delete_local_after_upload: row.delete_local_after_upload,
            status: row.status,
            progress: if total == 0 {
                0.0
            } else {
                uploaded as f64 * 100.0 / total as f64
            },
            uploaded_bytes: uploaded,
            total_bytes: total,
            speed_bytes_per_second: row.speed_bytes_per_second.max(0) as u64,
            created_at: row.created_at,
            updated_at: row.updated_at,
            completed_at: row.completed_at,
            error: row.error,
            retry_count: row.retry_count.max(0) as u32,
        }
    }
}

impl From<telegram_transfer_task::Model> for TelegramTransferRecord {
    fn from(row: telegram_transfer_task::Model) -> Self {
        Self {
            transfer_task_id: row.id,
            kind: row.kind,
            source_task_id: row.source_task_id,
            source_chat_id: row.source_chat_id,
            source_message_id: row.source_message_id,
            target_chat_id: row.target_chat_id,
            delete_local_after_upload: row.delete_local_after_upload,
            status: row.status,
            total_bytes: row.total_bytes.and_then(|value| u64::try_from(value).ok()),
            sent_bytes: row.sent_bytes.and_then(|value| u64::try_from(value).ok()),
            result_message_id: row.result_message_id,
            created_at: row.created_at,
            updated_at: row.updated_at,
            started_at: row.started_at,
            completed_at: row.completed_at,
            error: row.error,
            retry_count: row.retry_count.max(0) as u32,
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
            error: None,
            retry_count: 0,
            group_id: None,
        }
    }

    fn telegram_transfer_record(id: &str) -> TelegramTransferRecord {
        TelegramTransferRecord {
            transfer_task_id: id.into(),
            kind: "upload_file".into(),
            source_task_id: Some("download-1".into()),
            source_chat_id: Some("-100123".into()),
            source_message_id: Some(23),
            target_chat_id: "-100456".into(),
            delete_local_after_upload: false,
            status: "queued".into(),
            total_bytes: Some(1024),
            sent_bytes: None,
            result_message_id: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            started_at: None,
            completed_at: None,
            error: None,
            retry_count: 0,
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
    async fn imported_retry_ids_remain_pending_until_explicitly_claimed() {
        let dir = tempdir().unwrap();
        let store = TaskStore::open(&dir.path().join("db.sqlite"))
            .await
            .unwrap();
        assert_eq!(store.import_retry_ids("-1001", &[17, 18]).await.unwrap(), 2);
        assert_eq!(store.import_retry_ids("-1001", &[17, 18]).await.unwrap(), 0);

        let pending = store.list_pending_retry_ids(100).await.unwrap();
        assert_eq!(pending.len(), 2);
        assert!(store.mark_retry_id_claimed("-1001", 17).await.unwrap());
        assert!(!store.mark_retry_id_claimed("-1001", 17).await.unwrap());
        let pending = store.list_pending_retry_ids(100).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].message_id, 18);
    }

    #[tokio::test]
    async fn telegram_transfer_rows_persist_and_recover_ambiguous_send_safely() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("tasks.sqlite3");
        let store = TaskStore::open(&db).await.unwrap();
        store
            .create_telegram_transfer_task(&telegram_transfer_record("upload-running"))
            .await
            .unwrap();
        store
            .create_telegram_transfer_task(&telegram_transfer_record("send-running"))
            .await
            .unwrap();
        store
            .create_telegram_transfer_task(&telegram_transfer_record("send-failed"))
            .await
            .unwrap();
        store
            .set_telegram_transfer_status("upload-running", "uploading", None, None, None)
            .await
            .unwrap();
        store
            .set_telegram_transfer_status("send-running", "uploading", None, None, None)
            .await
            .unwrap();
        store
            .set_telegram_transfer_status("send-running", "sending", None, None, None)
            .await
            .unwrap();
        store
            .set_telegram_transfer_status("send-failed", "uploading", None, None, None)
            .await
            .unwrap();
        store
            .set_telegram_transfer_status(
                "send-failed",
                "failed",
                Some("simulated upload failure"),
                None,
                None,
            )
            .await
            .unwrap();
        store.close().await.unwrap();

        let reopened = TaskStore::open(&db).await.unwrap();
        let upload = reopened
            .get_telegram_transfer_task("upload-running")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(upload.status, "queued");
        assert_eq!(upload.sent_bytes, None, "no fake progress is stored");

        let sending = reopened
            .get_telegram_transfer_task("send-running")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sending.status, "uncertain");
        assert!(sending.error.as_deref().unwrap().contains("检查目标聊天"));
        assert!(
            reopened
                .set_telegram_transfer_status("send-running", "queued", None, None, None)
                .await
                .is_err()
        );

        let failed = reopened
            .get_telegram_transfer_task("send-failed")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(failed.status, "failed");
        reopened
            .set_telegram_transfer_status("send-failed", "queued", None, None, None)
            .await
            .unwrap();
        let retried = reopened
            .get_telegram_transfer_task("send-failed")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retried.status, "queued");
        assert_eq!(retried.retry_count, 1);
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
}
