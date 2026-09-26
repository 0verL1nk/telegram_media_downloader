//! Durable, bounded local application log storage.
//!
//! Records are JSON Lines so a partial write or an older malformed entry does not prevent later
//! entries from being recovered. The active log rotates at 10 MiB and rotated logs older than ten
//! days are pruned, matching the original application's practical retention window.

use crate::models::LogEntry;
use anyhow::{Context, Result};
use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::sync::Mutex;

const ACTIVE_FILE: &str = "client.jsonl";
const ROTATED_PREFIX: &str = "client-";
const ROTATION_BYTES: u64 = 10 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 16 * 1024;
const MAX_RECENT_FILES: usize = 64;
const RETENTION: Duration = Duration::from_secs(10 * 24 * 60 * 60);

#[derive(Clone)]
pub struct PersistentLogs {
    active_path: Arc<PathBuf>,
    writer: Arc<Mutex<()>>,
}

impl PersistentLogs {
    /// Open the store and return the recent persisted records for the UI's in-memory ring buffer.
    pub async fn open(directory: &Path, recent_limit: usize) -> Result<(Self, Vec<LogEntry>)> {
        let directory = directory.to_path_buf();
        let (active_path, recent) = tokio::task::spawn_blocking(move || {
            fs::create_dir_all(&directory)
                .with_context(|| format!("无法创建日志目录：{}", directory.display()))?;
            prune_expired(&directory, SystemTime::now())?;
            let active_path = directory.join(ACTIVE_FILE);
            let recent = load_recent(&directory, recent_limit.min(2_000))?;
            Ok::<_, anyhow::Error>((active_path, recent))
        })
        .await
        .context("恢复本地日志失败")??;
        Ok((
            Self {
                active_path: Arc::new(active_path),
                writer: Arc::new(Mutex::new(())),
            },
            recent,
        ))
    }

    /// Append one sanitized JSONL record and flush it to the local data volume.
    pub async fn append(&self, entry: &LogEntry) -> Result<()> {
        let mut entry = sanitize(entry.clone());
        let mut line = serde_json::to_vec(&entry).context("无法编码日志记录")?;
        line.push(b'\n');
        if line.len() > MAX_RECORD_BYTES {
            entry.message = truncate_chars(&entry.message, MAX_RECORD_BYTES / 8);
            line = serde_json::to_vec(&entry).context("无法编码截断后的日志记录")?;
            line.push(b'\n');
        }

        let _guard = self.writer.lock().await;
        let active_path = Arc::clone(&self.active_path);
        tokio::task::spawn_blocking(move || append_record(&active_path, &line))
            .await
            .context("写入本地日志任务失败")??;
        Ok(())
    }
}

fn append_record(active_path: &Path, line: &[u8]) -> Result<()> {
    let directory = active_path.parent().context("日志文件路径没有父目录")?;
    fs::create_dir_all(directory)
        .with_context(|| format!("无法创建日志目录：{}", directory.display()))?;

    match fs::symlink_metadata(active_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!("活动日志不能是符号链接")
        }
        Ok(metadata)
            if metadata.len() > 0
                && metadata.len().saturating_add(line.len() as u64) > ROTATION_BYTES =>
        {
            let rotated_path = next_rotated_path(directory)?;
            fs::rename(active_path, &rotated_path)
                .with_context(|| format!("无法轮转活动日志：{}", active_path.display()))?;
            prune_expired(directory, SystemTime::now())?;
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("无法检查活动日志文件"),
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(active_path)
        .with_context(|| format!("无法打开活动日志：{}", active_path.display()))?;
    file.write_all(line).context("无法追加本地日志记录")?;
    file.sync_data().context("无法持久化本地日志记录")?;
    Ok(())
}

fn next_rotated_path(directory: &Path) -> Result<PathBuf> {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    for suffix in 0_u32..10_000 {
        let name = format!("{ROTATED_PREFIX}{stamp}-{suffix:04}.jsonl");
        let path = directory.join(name);
        if !path.exists() {
            return Ok(path);
        }
    }
    anyhow::bail!("无法为日志轮转生成唯一文件名")
}

fn prune_expired(directory: &Path, now: SystemTime) -> Result<()> {
    let cutoff = now.checked_sub(RETENTION).unwrap_or(SystemTime::UNIX_EPOCH);
    for entry in fs::read_dir(directory)
        .with_context(|| format!("无法读取日志目录：{}", directory.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(ROTATED_PREFIX) || !name.ends_with(".jsonl") {
            continue;
        }
        let metadata = entry.file_type()?;
        if metadata.is_symlink() || !metadata.is_file() {
            continue;
        }
        if entry.metadata()?.modified().unwrap_or(now) < cutoff {
            fs::remove_file(entry.path())
                .with_context(|| format!("无法清理过期日志：{}", entry.path().display()))?;
        }
    }
    Ok(())
}

fn load_recent(directory: &Path, limit: usize) -> Result<Vec<LogEntry>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)
        .with_context(|| format!("无法读取日志目录：{}", directory.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name != ACTIVE_FILE && !(name.starts_with(ROTATED_PREFIX) && name.ends_with(".jsonl")) {
            continue;
        }
        let file_type = entry.file_type()?;
        if !file_type.is_file() {
            continue;
        }
        let metadata = entry.metadata()?;
        files.push((
            metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            entry.path(),
        ));
    }
    files.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    if files.len() > MAX_RECENT_FILES {
        files.drain(..files.len() - MAX_RECENT_FILES);
    }

    let mut recent = VecDeque::with_capacity(limit);
    for (_, path) in files {
        read_records(&path, limit, &mut recent)?;
    }
    Ok(recent.into_iter().collect())
}

fn read_records(path: &Path, limit: usize, recent: &mut VecDeque<LogEntry>) -> Result<()> {
    let file = File::open(path).with_context(|| format!("无法读取日志：{}", path.display()))?;
    // An app-generated segment is at most 10 MiB plus one bounded record. This `take` cap also
    // prevents a damaged file with a missing newline from allocating without a limit.
    let mut reader = BufReader::new(file.take(ROTATION_BYTES + MAX_RECORD_BYTES as u64));
    let mut line = Vec::with_capacity(512);
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        if line.len() > MAX_RECORD_BYTES {
            continue;
        }
        if let Ok(mut entry) = serde_json::from_slice::<LogEntry>(&line) {
            entry = sanitize(entry);
            if recent.len() == limit {
                recent.pop_front();
            }
            recent.push_back(entry);
        }
    }
    Ok(())
}

pub(crate) fn sanitize_entry(mut entry: LogEntry) -> LogEntry {
    entry.timestamp = truncate_chars(&remove_controls(&entry.timestamp), 80);
    entry.level = truncate_chars(&remove_controls(&entry.level), 16);
    entry.target = entry
        .target
        .map(|value| truncate_chars(&redact_secrets(&remove_controls(&value)), 128));
    entry.message = truncate_chars(&redact_secrets(&remove_controls(&entry.message)), 4_096);
    entry
}

fn sanitize(entry: LogEntry) -> LogEntry {
    sanitize_entry(entry)
}

fn remove_controls(value: &str) -> String {
    value.chars().filter(|ch| !ch.is_control()).collect()
}

fn truncate_chars(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

fn redact_secrets(value: &str) -> String {
    const KEYS: &[&str] = &[
        "api_hash",
        "api hash",
        "password",
        "token",
        "cookie",
        "authorization",
        "session",
        "session_key",
        "session key",
    ];
    let mut redactions = Vec::<(usize, usize)>::new();
    let lower = value.to_ascii_lowercase();
    for key in KEYS {
        let mut from = 0;
        while let Some(relative) = lower[from..].find(key) {
            let key_start = from + relative;
            let mut cursor = key_start + key.len();
            while lower
                .as_bytes()
                .get(cursor)
                .is_some_and(u8::is_ascii_whitespace)
            {
                cursor += 1;
            }
            if !matches!(lower.as_bytes().get(cursor), Some(b'=' | b':')) {
                from = key_start + key.len();
                continue;
            }
            cursor += 1;
            while lower
                .as_bytes()
                .get(cursor)
                .is_some_and(u8::is_ascii_whitespace)
            {
                cursor += 1;
            }
            let quoted = matches!(lower.as_bytes().get(cursor), Some(b'\'' | b'"'));
            let quote = lower.as_bytes().get(cursor).copied();
            if quoted {
                cursor += 1;
            }
            let secret_start = cursor;
            let credential_prefix_len = if lower[secret_start..].starts_with("bearer ") {
                "bearer ".len()
            } else if lower[secret_start..].starts_with("basic ") {
                "basic ".len()
            } else if lower[secret_start..].starts_with("token ") {
                "token ".len()
            } else {
                0
            };
            while let Some(byte) = lower.as_bytes().get(cursor) {
                if !quoted && *key == "cookie" {
                    if matches!(*byte, b']' | b'}') {
                        break;
                    }
                    cursor += 1;
                    continue;
                }
                if (quoted && Some(*byte) == quote)
                    || (!quoted
                        && (byte.is_ascii_whitespace()
                            || matches!(*byte, b',' | b';' | b'&' | b']' | b'}')))
                {
                    if credential_prefix_len > 0
                        && *byte == b' '
                        && cursor < secret_start + credential_prefix_len
                    {
                        cursor += 1;
                        continue;
                    }
                    break;
                }
                cursor += 1;
            }
            if secret_start < cursor {
                redactions.push((secret_start, cursor));
            }
            from = cursor.max(key_start + key.len());
        }
    }

    // Mask URL user-info such as `socks5://user:password@host`.
    let mut from = 0;
    while let Some(relative) = lower[from..].find("://") {
        let authority_start = from + relative + 3;
        let authority_end = lower[authority_start..]
            .find(|ch: char| matches!(ch, '/' | '?' | '#' | ' ' | '\t'))
            .map(|offset| authority_start + offset)
            .unwrap_or(lower.len());
        if let Some(at) = lower[authority_start..authority_end]
            .find('@')
            .map(|offset| authority_start + offset)
        {
            if lower[authority_start..at].contains(':') {
                redactions.push((authority_start, at));
            }
        }
        from = authority_end.max(authority_start);
    }

    redactions.sort_unstable();
    let mut merged = Vec::<(usize, usize)>::new();
    for (start, end) in redactions {
        if let Some(last) = merged.last_mut() {
            if start <= last.1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        merged.push((start, end));
    }
    let mut output = String::with_capacity(value.len());
    let mut previous = 0;
    for (start, end) in merged {
        if value.is_char_boundary(start) && value.is_char_boundary(end) {
            output.push_str(&value[previous..start]);
            output.push_str("[REDACTED]");
            previous = end;
        }
    }
    output.push_str(&value[previous..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn entry(message: &str) -> LogEntry {
        LogEntry {
            timestamp: "2026-09-26T12:00:00Z".into(),
            level: "info".into(),
            target: Some("test".into()),
            message: message.into(),
        }
    }

    #[tokio::test]
    async fn records_reload_and_rotate_without_losing_recent_entries() {
        let directory = tempdir().unwrap();
        let active = directory.path().join(ACTIVE_FILE);
        fs::write(&active, vec![b'x'; ROTATION_BYTES as usize]).unwrap();
        let (store, _) = PersistentLogs::open(directory.path(), 10).await.unwrap();
        store.append(&entry("saved after restart")).await.unwrap();

        let (_, recovered) = PersistentLogs::open(directory.path(), 10).await.unwrap();
        assert_eq!(recovered.last().unwrap().message, "saved after restart");
        assert!(fs::read_dir(directory.path()).unwrap().any(|item| {
            item.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(ROTATED_PREFIX)
        }));
    }

    #[test]
    fn sanitization_masks_secrets_and_url_credentials() {
        let sanitized = sanitize_entry(entry(
            "api_hash=abcdef password: 'secret words' token=Bearer bearersecret authorization: Bearer topsecret session=abc cookie=SID=secret; auth=second socks5://user:pass@proxy:1080",
        ));
        assert!(!sanitized.message.contains("abcdef"));
        assert!(!sanitized.message.contains("secret words"));
        assert!(!sanitized.message.contains("xyz"));
        assert!(!sanitized.message.contains("bearersecret"));
        assert!(!sanitized.message.contains("topsecret"));
        assert!(!sanitized.message.contains("session=abc"));
        assert!(!sanitized.message.contains("SID=secret"));
        assert!(!sanitized.message.contains("auth=second"));
        assert!(!sanitized.message.contains("user:pass"));
        assert!(sanitized.message.contains("[REDACTED]"));
    }
}
