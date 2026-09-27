//! Safe, in-place AV1 compression for downloaded videos.
//!
//! FFmpeg/ffprobe are external runtime tools. The original is only replaced after the
//! candidate has been encoded, decoded for validation, probed, and confirmed smaller.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::{
    env,
    ffi::OsStr,
    fs as std_fs,
    path::{Path, PathBuf},
    process::Stdio,
};
use tauri::{AppHandle, Manager};
use tokio::{fs, process::Command};
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub struct Replacement {
    pub old_size: u64,
    pub new_size: u64,
}

const SEGMENT_SECONDS: f64 = 30.0;

#[derive(Debug, Deserialize, Serialize)]
struct EncodeManifest {
    source_size: u64,
    source_modified_nanos: u128,
    duration_seconds: f64,
    width: u32,
    height: u32,
    profile: String,
}

#[derive(Debug, Deserialize)]
struct Probe {
    streams: Vec<ProbeStream>,
    format: ProbeFormat,
}

#[derive(Debug, Deserialize)]
struct ProbeStream {
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
}

/// Types currently produced by the Telegram Web media detector, plus common video containers.
pub fn is_video(media_type: Option<&str>, path: &Path) -> bool {
    let kind = media_type.unwrap_or_default().to_ascii_lowercase();
    if matches!(kind.as_str(), "video" | "animation") {
        return true;
    }
    matches!(
        path.extension()
            .and_then(OsStr::to_str)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "mp4" | "m4v" | "mov" | "mkv" | "webm" | "avi"
    )
}

/// Recover the tiny rename window if the app exits while replacing the source.
pub fn recover_interrupted_replace(source: &Path) -> Result<()> {
    let Some(parent) = source.parent() else {
        return Ok(());
    };
    let Some(stem) = source.file_stem().and_then(OsStr::to_str) else {
        return Ok(());
    };
    let Some(extension) = source.extension().and_then(OsStr::to_str) else {
        return Ok(());
    };
    let backup_prefix = format!(".{stem}.tmd-backup-");
    let candidate_prefix = format!(".{stem}.tmd-");
    let mut backups = Vec::new();
    let mut candidates = Vec::new();
    for entry in std_fs::read_dir(parent)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(OsStr::to_str) != Some(extension) {
            continue;
        }
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        if name.starts_with(&backup_prefix) {
            backups.push(path);
        } else if name.starts_with(&candidate_prefix) {
            candidates.push(path);
        }
    }
    if !source.exists() {
        let backup = backups
            .pop()
            .context("原视频路径不存在，且未找到可恢复的处理备份")?;
        std_fs::rename(&backup, source).context("无法恢复视频处理前的原文件")?;
    }
    for path in backups.into_iter().chain(candidates) {
        let _ = std_fs::remove_file(path);
    }
    Ok(())
}

pub async fn compress_replace(app: &AppHandle, source: &Path) -> Result<Option<Replacement>> {
    if !source.is_file() {
        bail!("原视频文件不存在");
    }
    let (ffmpeg, ffprobe) = resolve_tools(app)?;
    let before = probe(&ffprobe, source).await?;
    let video = before.streams.first().context("原视频没有可读取的视频流")?;
    if video.codec_name.as_deref() == Some("av1") {
        let work_dir = work_directory(source)?;
        if work_dir.exists() {
            fs::remove_dir_all(&work_dir)
                .await
                .context("视频已是 AV1，但无法清理上次处理留下的临时分段")?;
        }
        return Ok(None);
    }
    let width = video.width.context("无法读取原视频宽度")?;
    let height = video.height.context("无法读取原视频高度")?;
    let duration_seconds = before
        .format
        .duration
        .as_deref()
        .and_then(|duration| duration.parse::<f64>().ok())
        .filter(|duration| duration.is_finite() && *duration > 0.0)
        .context("无法读取原视频时长，不能创建可恢复的编码分段")?;
    let old_size = fs::metadata(source).await?.len();
    let modified = fs::metadata(source)
        .await?
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let work_dir = work_directory(source)?;
    fs::create_dir_all(&work_dir).await?;
    let manifest_path = work_dir.join("manifest.json");
    let manifest = EncodeManifest {
        source_size: old_size,
        source_modified_nanos: modified,
        duration_seconds,
        width,
        height,
        profile: "svtav1-p8-crf20-tune0-v1".to_owned(),
    };
    let can_resume = match fs::read(&manifest_path).await {
        Ok(bytes) => serde_json::from_slice::<EncodeManifest>(&bytes)
            .map(|saved| {
                saved.source_size == manifest.source_size
                    && saved.source_modified_nanos == manifest.source_modified_nanos
                    && saved.duration_seconds == manifest.duration_seconds
                    && saved.width == manifest.width
                    && saved.height == manifest.height
                    && saved.profile == manifest.profile
            })
            .unwrap_or(false),
        Err(_) => false,
    };
    if !can_resume {
        fs::remove_dir_all(&work_dir).await?;
        fs::create_dir_all(&work_dir).await?;
        fs::write(&manifest_path, serde_json::to_vec(&manifest)?).await?;
    }
    let segment_count = (duration_seconds / SEGMENT_SECONDS).ceil() as usize;
    if segment_count == 0 || segment_count > 100_000 {
        bail!("视频时长超出可处理范围");
    }

    // Encode fixed-size, independently decodable segments. Completed segments survive
    // app exits and are reused; only the segment interrupted in progress is repeated.
    for index in 0..segment_count {
        let start = index as f64 * SEGMENT_SECONDS;
        let segment_duration = (duration_seconds - start).min(SEGMENT_SECONDS);
        let segment = work_dir.join(format!("segment-{index:06}.mkv"));
        if segment_is_valid(&ffmpeg, &ffprobe, &segment, width, height, segment_duration).await {
            continue;
        }
        let _ = fs::remove_file(&segment).await;
        let start_arg = format!("{start:.3}");
        let duration_arg = format!("{segment_duration:.3}");
        let encode = Command::new(&ffmpeg)
            .args([
                OsStr::new("-hide_banner"),
                OsStr::new("-loglevel"),
                OsStr::new("error"),
                OsStr::new("-nostdin"),
                OsStr::new("-y"),
                OsStr::new("-ss"),
                OsStr::new(&start_arg),
                OsStr::new("-i"),
            ])
            .arg(source)
            .args([
                OsStr::new("-t"),
                OsStr::new(&duration_arg),
                OsStr::new("-map"),
                OsStr::new("0:v:0"),
                OsStr::new("-an"),
                OsStr::new("-sn"),
                OsStr::new("-c:v"),
                OsStr::new("libsvtav1"),
                OsStr::new("-preset"),
                OsStr::new("8"),
                OsStr::new("-crf"),
                OsStr::new("20"),
                OsStr::new("-svtav1-params"),
                OsStr::new("tune=0"),
                OsStr::new("-fps_mode"),
                OsStr::new("passthrough"),
                OsStr::new("-f"),
                OsStr::new("matroska"),
            ])
            .arg(&segment)
            .kill_on_drop(true)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("无法启动 FFmpeg 分段编码")?;
        if !encode.status.success() {
            let _ = fs::remove_file(&segment).await;
            let detail = String::from_utf8_lossy(&encode.stderr)
                .trim()
                .chars()
                .take(500)
                .collect::<String>();
            bail!(
                "FFmpeg 分段编码失败{}",
                if detail.is_empty() {
                    String::new()
                } else {
                    format!("：{detail}")
                }
            );
        }
        if !segment_is_valid(&ffmpeg, &ffprobe, &segment, width, height, segment_duration).await {
            let _ = fs::remove_file(&segment).await;
            bail!("编码片段校验失败，原视频已保留");
        }
    }

    let candidate = temporary_candidate_path(source)?;
    let concat_path = work_dir.join("concat.txt");
    let mut concat = String::new();
    for index in 0..segment_count {
        concat.push_str(&format!("file 'segment-{index:06}.mkv'\n"));
    }
    fs::write(&concat_path, concat).await?;
    let mux = Command::new(&ffmpeg)
        .args([
            OsStr::new("-hide_banner"),
            OsStr::new("-loglevel"),
            OsStr::new("error"),
            OsStr::new("-nostdin"),
            OsStr::new("-y"),
            OsStr::new("-f"),
            OsStr::new("concat"),
            OsStr::new("-safe"),
            OsStr::new("0"),
            OsStr::new("-i"),
        ])
        .arg(&concat_path)
        .arg("-i")
        .arg(source)
        .args([
            OsStr::new("-map"),
            OsStr::new("0:v:0"),
            OsStr::new("-map"),
            OsStr::new("1:a?"),
            OsStr::new("-map"),
            OsStr::new("1:s?"),
            OsStr::new("-map_metadata"),
            OsStr::new("1"),
            OsStr::new("-map_chapters"),
            OsStr::new("1"),
            OsStr::new("-c:v"),
            OsStr::new("copy"),
            OsStr::new("-c:a"),
            OsStr::new("copy"),
            OsStr::new("-c:s"),
            OsStr::new("copy"),
        ])
        .arg(&candidate)
        .kill_on_drop(true)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("无法启动 FFmpeg 最终封装")?;
    if !mux.status.success() {
        let _ = fs::remove_file(&candidate).await;
        let detail = String::from_utf8_lossy(&mux.stderr)
            .trim()
            .chars()
            .take(500)
            .collect::<String>();
        bail!(
            "FFmpeg 最终封装失败{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!("：{detail}")
            }
        );
    }

    let new_size = match fs::metadata(&candidate).await {
        Ok(metadata) if metadata.len() > 0 => metadata.len(),
        _ => {
            let _ = fs::remove_file(&candidate).await;
            bail!("FFmpeg 没有生成有效文件");
        }
    };
    if new_size >= old_size {
        let _ = fs::remove_file(&candidate).await;
        fs::remove_dir_all(&work_dir)
            .await
            .context("转码结果未变小，且无法清理临时分段")?;
        return Ok(None);
    }

    let after = match probe(&ffprobe, &candidate).await {
        Ok(probe) => probe,
        Err(error) => {
            let _ = fs::remove_file(&candidate).await;
            return Err(error);
        }
    };
    let encoded_video = after
        .streams
        .first()
        .context("转码结果没有可读取的视频流")?;
    if encoded_video.codec_name.as_deref() != Some("av1")
        || encoded_video.width != Some(width)
        || encoded_video.height != Some(height)
        || !durations_match(
            before.format.duration.as_deref(),
            after.format.duration.as_deref(),
        )
    {
        let _ = fs::remove_file(&candidate).await;
        bail!("转码结果的编码、分辨率或时长校验未通过");
    }

    // Decode the complete candidate before changing the original path.
    let validation = Command::new(&ffmpeg)
        .args([
            OsStr::new("-v"),
            OsStr::new("error"),
            OsStr::new("-xerror"),
            OsStr::new("-nostdin"),
            OsStr::new("-i"),
        ])
        .arg(&candidate)
        .args([
            OsStr::new("-map"),
            OsStr::new("0:v:0"),
            OsStr::new("-f"),
            OsStr::new("null"),
            OsStr::new(if cfg!(windows) { "NUL" } else { "/dev/null" }),
        ])
        .kill_on_drop(true)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("无法校验转码结果")?;
    if !validation.status.success() {
        let _ = fs::remove_file(&candidate).await;
        bail!("转码结果无法完整解码，原视频已保留");
    }

    replace_with_rollback(source, &candidate).await?;
    fs::remove_dir_all(&work_dir)
        .await
        .context("视频已替换，但无法清理临时分段")?;
    Ok(Some(Replacement { old_size, new_size }))
}

async fn segment_is_valid(
    ffmpeg: &Path,
    ffprobe: &Path,
    path: &Path,
    width: u32,
    height: u32,
    expected_duration: f64,
) -> bool {
    let Ok(metadata) = fs::metadata(path).await else {
        return false;
    };
    if metadata.len() == 0 {
        return false;
    }
    let Ok(probe) = probe(ffprobe, path).await else {
        return false;
    };
    let Some(video) = probe.streams.first() else {
        return false;
    };
    let metadata_valid = video.codec_name.as_deref() == Some("av1")
        && video.width == Some(width)
        && video.height == Some(height)
        && probe
            .format
            .duration
            .as_deref()
            .and_then(|v| v.parse::<f64>().ok())
            .is_some_and(|duration| (duration - expected_duration).abs() <= 1.5);
    if !metadata_valid {
        return false;
    }
    Command::new(ffmpeg)
        .args([
            OsStr::new("-v"),
            OsStr::new("error"),
            OsStr::new("-xerror"),
            OsStr::new("-nostdin"),
            OsStr::new("-i"),
        ])
        .arg(path)
        .args([
            OsStr::new("-map"),
            OsStr::new("0:v:0"),
            OsStr::new("-f"),
            OsStr::new("null"),
            OsStr::new(if cfg!(windows) { "NUL" } else { "/dev/null" }),
        ])
        .kill_on_drop(true)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

fn work_directory(source: &Path) -> Result<PathBuf> {
    let parent = source.parent().context("视频没有父目录")?;
    let file_name = source
        .file_name()
        .and_then(OsStr::to_str)
        .context("视频文件名无效")?;
    Ok(parent.join(format!(".{file_name}.tmd-parts")))
}

async fn probe(ffprobe: &Path, path: &Path) -> Result<Probe> {
    let output = Command::new(ffprobe)
        .args([
            OsStr::new("-v"),
            OsStr::new("error"),
            OsStr::new("-select_streams"),
            OsStr::new("v:0"),
            OsStr::new("-show_entries"),
            OsStr::new("stream=codec_name,width,height"),
            OsStr::new("-show_entries"),
            OsStr::new("format=duration"),
            OsStr::new("-of"),
            OsStr::new("json"),
        ])
        .arg(path)
        .kill_on_drop(true)
        .output()
        .await
        .context("无法启动 ffprobe")?;
    if !output.status.success() {
        bail!("ffprobe 无法读取视频文件");
    }
    serde_json::from_slice(&output.stdout).context("ffprobe 返回了无效的视频信息")
}

fn resolve_tools(app: &AppHandle) -> Result<(PathBuf, PathBuf)> {
    let ffmpeg = env::var_os("TMD_FFMPEG_PATH")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| resource_tool(app, "ffmpeg.exe"))
        .or_else(|| sibling_tool("ffmpeg.exe"))
        .or_else(|| find_on_path("ffmpeg.exe"))
        .or_else(|| find_on_path("ffmpeg"))
        .ok_or_else(|| {
            anyhow!("未找到 FFmpeg；请将 ffmpeg.exe 加入 PATH，或设置 TMD_FFMPEG_PATH")
        })?;
    let sibling = ffmpeg.with_file_name("ffprobe.exe");
    let ffprobe = if sibling.is_file() {
        sibling
    } else {
        resource_tool(app, "ffprobe.exe")
            .or_else(|| find_on_path("ffprobe.exe"))
            .or_else(|| find_on_path("ffprobe"))
            .ok_or_else(|| anyhow!("未找到 ffprobe.exe；请与 FFmpeg 一起安装"))?
    };
    Ok((ffmpeg, ffprobe))
}

fn resource_tool(app: &AppHandle, name: &str) -> Option<PathBuf> {
    let root = app.path().resource_dir().ok()?;
    [
        root.join("ffmpeg").join(name),
        root.join("resources").join("ffmpeg").join(name),
        root.join(name),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

fn sibling_tool(name: &str) -> Option<PathBuf> {
    let path = std::env::current_exe().ok()?.parent()?.join(name);
    path.is_file().then_some(path)
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

fn temporary_candidate_path(source: &Path) -> Result<PathBuf> {
    let parent = source.parent().context("视频没有父目录")?;
    let stem = source
        .file_stem()
        .and_then(OsStr::to_str)
        .context("视频文件名无效")?;
    let extension = source
        .extension()
        .and_then(OsStr::to_str)
        .context("视频缺少扩展名")?;
    Ok(parent.join(format!(".{stem}.tmd-{}.{}", Uuid::new_v4(), extension)))
}

async fn replace_with_rollback(source: &Path, candidate: &Path) -> Result<()> {
    let extension = source
        .extension()
        .and_then(OsStr::to_str)
        .context("视频缺少扩展名")?;
    let stem = source
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("video");
    let backup = source.with_file_name(format!(
        ".{stem}.tmd-backup-{}.{}",
        Uuid::new_v4(),
        extension
    ));
    fs::rename(source, &backup)
        .await
        .context("无法安全暂存原视频")?;
    if let Err(error) = fs::rename(candidate, source).await {
        if let Err(restore_error) = fs::rename(&backup, source).await {
            bail!(
                "写入转码结果失败({error})，且恢复原视频失败({restore_error})；原视频保存在 {}",
                backup.display()
            );
        }
        return Err(error).context("无法将转码结果替换到原路径；原视频已恢复");
    }
    if let Err(error) = fs::remove_file(&backup).await {
        // Do not silently leave the original consuming disk beside the replacement.
        // Restore it if cleanup fails, so the task remains safe to retry.
        if fs::remove_file(source).await.is_ok() && fs::rename(&backup, source).await.is_ok() {
            bail!("无法删除旧视频备份，已恢复原视频：{error}");
        }
        bail!(
            "无法删除旧视频备份；新视频位于原路径，旧视频备份保留在 {}：{error}",
            backup.display()
        );
    }
    Ok(())
}

fn durations_match(before: Option<&str>, after: Option<&str>) -> bool {
    match (
        before.and_then(|value| value.parse::<f64>().ok()),
        after.and_then(|value| value.parse::<f64>().ok()),
    ) {
        (Some(before), Some(after)) => (before - after).abs() <= 0.5,
        _ => false,
    }
}
