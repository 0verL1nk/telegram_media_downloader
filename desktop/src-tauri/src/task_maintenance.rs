use crate::{app_state::SharedState, models::TaskRecord};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tauri::Emitter;

const CHECK_INTERVAL: Duration = Duration::from_secs(30);
const TASK_SCAN_LIMIT: usize = 2000;

fn output_is_missing_under_root(task: &TaskRecord, download_root: &Path) -> bool {
    if !task.status.eq_ignore_ascii_case("completed") {
        return false;
    }
    let Some(raw_path) = task.output_path.as_deref() else {
        return false;
    };
    let output = PathBuf::from(raw_path);
    if !output.is_absolute() {
        return false;
    }
    match output.try_exists() {
        Ok(true) | Err(_) => return false,
        Ok(false) => {}
    }

    let mut ancestor = output.parent();
    while let Some(path) = ancestor {
        if let Ok(canonical) = path.canonicalize() {
            return canonical.starts_with(download_root);
        }
        ancestor = path.parent();
    }
    false
}

pub fn start(shared: Arc<SharedState>) {
    tauri::async_runtime::spawn(async move {
        loop {
            reconcile_missing_outputs(&shared).await;
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

async fn reconcile_missing_outputs(shared: &SharedState) {
    let settings = shared.settings.read().await.clone();
    let configured_root = PathBuf::from(settings.download_root);
    // If the disk or configured folder is unavailable, keep task records until it is reachable.
    let Ok(download_root) = tokio::fs::canonicalize(&configured_root).await else {
        return;
    };
    if !download_root.is_dir() {
        return;
    }

    let tasks = match shared.store.list(Some("completed"), TASK_SCAN_LIMIT).await {
        Ok(tasks) => tasks,
        Err(error) => {
            shared
                .log(
                    "warn",
                    "task-maintenance",
                    format!("检查已完成文件失败：{error:#}"),
                )
                .await;
            return;
        }
    };
    for task in tasks {
        if !output_is_missing_under_root(&task, &download_root) {
            continue;
        }
        // Re-read before deleting so a task retried while the scan was running is preserved.
        let Ok(Some(current)) = shared.store.get(&task.task_id).await else {
            continue;
        };
        if current.status != task.status
            || current.output_path != task.output_path
            || !output_is_missing_under_root(&current, &download_root)
        {
            continue;
        }
        if let Err(error) = shared.store.delete(&current.task_id).await {
            shared
                .log(
                    "warn",
                    "task-maintenance",
                    format!("清理缺失文件对应的任务 {} 失败：{error:#}", current.task_id),
                )
                .await;
            continue;
        }
        for folder in ["video-covers", "video-thumbnails"] {
            let cached = shared
                .layout
                .read()
                .await
                .root
                .join("Cache")
                .join(folder)
                .join(format!("{}.jpg", current.task_id));
            if let Err(error) = tokio::fs::remove_file(&cached).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                shared
                    .log(
                        "warn",
                        "task-maintenance",
                        format!("清理任务封面缓存失败：{error}"),
                    )
                    .await;
            }
        }
        let _ = shared.app.emit(
            "task-deleted",
            serde_json::json!({ "taskId": current.task_id, "reason": "missing-output" }),
        );
        if let Ok(stats) = shared.store.stats().await {
            let _ = shared
                .app
                .emit("stats-updated", serde_json::json!({ "stats": stats }));
        }
        shared
            .log(
                "info",
                "task-maintenance",
                format!("本地文件已不存在，自动移除任务 {}", current.task_id),
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::output_is_missing_under_root;
    use crate::models::TaskRecord;
    use std::path::Path;

    fn task(path: &Path, status: &str) -> TaskRecord {
        TaskRecord {
            task_id: "task".into(),
            chat_id: String::new(),
            chat_title: None,
            message_id: None,
            media_type: Some("video/mp4".into()),
            file_name: Some("video.mp4".into()),
            status: status.into(),
            progress: 1.0,
            downloaded_bytes: 10,
            total_bytes: Some(10),
            speed_bytes_per_second: 0,
            remaining_bytes: Some(0),
            started_at: None,
            updated_at: None,
            completed_at: None,
            output_path: Some(path.to_string_lossy().into_owned()),
            cover_path: None,
            preview_path: None,
            error: None,
            retry_count: 0,
            group_id: None,
        }
    }

    #[test]
    fn only_missing_completed_files_under_the_download_root_are_removed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("downloads");
        std::fs::create_dir_all(&root).unwrap();
        let absent = root.join("video.mp4");
        assert!(output_is_missing_under_root(
            &task(&absent, "completed"),
            &root.canonicalize().unwrap()
        ));
        assert!(!output_is_missing_under_root(
            &task(&absent, "downloading"),
            &root.canonicalize().unwrap()
        ));
        assert!(!output_is_missing_under_root(
            &task(&temp.path().join("outside.mp4"), "completed"),
            &root.canonicalize().unwrap()
        ));
        std::fs::write(&absent, b"video").unwrap();
        assert!(!output_is_missing_under_root(
            &task(&absent, "completed"),
            &root.canonicalize().unwrap()
        ));
    }
}
