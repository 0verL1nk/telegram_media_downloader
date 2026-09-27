use crate::{app_state::AppState, atomic_file, storage};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    process::Command,
};
use tauri::{AppHandle, Manager, State};
use tokio::sync::Semaphore;

const MAX_COVER_BYTES: usize = 4 * 1024 * 1024;
const VIDEO_EXTENSIONS: &[&str] = &[
    "3g2", "3gp", "asf", "avi", "divx", "f4v", "flv", "m2ts", "m2v", "m4v", "mkv", "mov", "mp4",
    "mpe", "mpeg", "mpg", "mts", "m2ts", "mxf", "ogv", "ogg", "qt", "rm", "rmvb", "ts", "vob",
    "webm", "wmv",
];
static THUMBNAIL_JOBS: Semaphore = Semaphore::const_new(2);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoPreviewInfo {
    pub video_path: String,
    pub duration_seconds: f64,
    pub cover_path: Option<String>,
    pub download_complete: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverSaveResult {
    pub cover_path: String,
    pub embedded: bool,
}

fn command_error(error: impl std::fmt::Display) -> String {
    let mut message = error.to_string();
    if message.len() > 1_000 {
        message.truncate(1_000);
        message.push_str("…");
    }
    message
}

fn cover_path(root: &Path, task_id: &str) -> PathBuf {
    root.join("Cache")
        .join("video-covers")
        .join(format!("{task_id}.jpg"))
}

fn preview_path(root: &Path, task_id: &str) -> PathBuf {
    root.join("Cache")
        .join("video-thumbnails")
        .join(format!("{task_id}.jpg"))
}

fn ffmpeg_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .resource_dir()
        .map(|directory| directory.join("ffmpeg.exe"))
        .map_err(command_error)
}

fn ffmpeg_command(binary: &Path) -> Command {
    let mut command = Command::new(binary);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

async fn video_for_task(
    state: &AppState,
    task_id: &str,
) -> Result<(PathBuf, bool, String), String> {
    if uuid::Uuid::parse_str(task_id).is_err() {
        return Err("任务编号无效".into());
    }
    let task = state
        .shared
        .store
        .get(task_id)
        .await
        .map_err(command_error)?
        .ok_or_else(|| "未找到下载任务".to_owned())?;
    let complete = task.status.eq_ignore_ascii_case("completed");
    let raw_path = task
        .output_path
        .as_deref()
        .ok_or_else(|| "任务没有本地文件".to_owned())?;
    let output = PathBuf::from(raw_path);
    let settings = state.shared.settings.read().await.clone();
    let download_root = PathBuf::from(settings.download_root)
        .canonicalize()
        .map_err(command_error)?;
    let extension = output
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !VIDEO_EXTENSIONS.contains(&extension.as_str()) {
        return Err("当前文件格式不是可识别的视频格式".into());
    }
    let target = if complete {
        output.canonicalize().map_err(command_error)?
    } else {
        let temp = output.with_file_name(format!(".telegram-media-{task_id}.part"));
        temp.canonicalize()
            .map_err(|_| "该任务还没有可读取的视频分块".to_owned())?
    };
    if !target.starts_with(&download_root) {
        return Err("任务文件位于下载目录之外，已拒绝访问".into());
    }
    Ok((target, complete, extension))
}

fn container_family(video: &Path) -> Result<&'static str, String> {
    let extension = video
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    container_family_from_extension(&extension)
}

fn container_family_from_extension(extension: &str) -> Result<&'static str, String> {
    match extension {
        "mp4" | "m4v" | "mov" | "3gp" | "3g2" | "f4v" | "qt" => Ok("quicktime"),
        "mkv" => Ok("matroska"),
        _ => Err("当前容器没有通用的内嵌封面轨道；目前支持 MP4、MOV、M4V、3GP 和 MKV".into()),
    }
}

fn embed_cover(ffmpeg: &Path, video: &Path, image: &Path) -> Result<(), String> {
    let family = container_family(video)?;
    let extension = video
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let output = video.with_file_name(format!(".cover-{}.{}", uuid::Uuid::new_v4(), extension));
    let mut args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-i".into(),
        video.to_string_lossy().into_owned(),
    ];
    if family == "matroska" {
        args.extend([
            "-map".into(),
            "0".into(),
            "-c".into(),
            "copy".into(),
            "-attach".into(),
            image.to_string_lossy().into_owned(),
            "-metadata:s:t:0".into(),
            "mimetype=image/jpeg".into(),
            "-metadata:s:t:0".into(),
            "filename=cover.jpg".into(),
        ]);
    } else {
        args.extend([
            "-i".into(),
            image.to_string_lossy().into_owned(),
            "-map".into(),
            "0".into(),
            "-map".into(),
            "1:v:0".into(),
            "-c".into(),
            "copy".into(),
            "-disposition:v:1".into(),
            "attached_pic".into(),
            "-metadata:s:v:1".into(),
            "title=Cover (front)".into(),
            "-metadata:s:v:1".into(),
            "comment=Cover (front)".into(),
            "-movflags".into(),
            "+faststart".into(),
        ]);
    }
    args.push(output.to_string_lossy().into_owned());
    let result = run_output(ffmpeg, &args);
    if let Err(error) = result {
        let _ = std::fs::remove_file(&output);
        return Err(error);
    }
    if !output.is_file() {
        return Err("视频封面写入未生成输出文件".into());
    }
    atomic_file::replace(&output, video).map_err(command_error)
}

fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xff, 0xd8, 0xff])
}

fn attached_picture_streams(stderr: &str) -> Vec<usize> {
    stderr
        .lines()
        .filter(|line| line.contains("(attached pic)"))
        .filter_map(|line| {
            let (_, tail) = line.split_once("Stream #0:")?;
            let digits = tail
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            digits.parse().ok()
        })
        .collect()
}

fn image_to_jpeg(ffmpeg: &Path, image_path: &Path) -> Result<Vec<u8>, String> {
    run_output(
        ffmpeg,
        &[
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-i".into(),
            image_path.to_string_lossy().into_owned(),
            "-frames:v".into(),
            "1".into(),
            "-vf".into(),
            "scale='min(1280,iw)':-2".into(),
            "-f".into(),
            "image2pipe".into(),
            "-vcodec".into(),
            "mjpeg".into(),
            "pipe:1".into(),
        ],
    )
}

fn extract_attached_cover(ffmpeg: &Path, video: &Path) -> Option<Vec<u8>> {
    let input = ffmpeg_command(ffmpeg)
        .args(["-hide_banner", "-i"])
        .arg(video)
        .output()
        .ok()?;
    let stderr = String::from_utf8_lossy(&input.stderr);
    for index in attached_picture_streams(&stderr) {
        let Ok(bytes) = run_output(
            ffmpeg,
            &[
                "-hide_banner".into(),
                "-loglevel".into(),
                "error".into(),
                "-i".into(),
                video.to_string_lossy().into_owned(),
                "-map".into(),
                format!("0:{index}"),
                "-frames:v".into(),
                "1".into(),
                "-vf".into(),
                "scale='min(1280,iw)':-2".into(),
                "-f".into(),
                "image2pipe".into(),
                "-vcodec".into(),
                "mjpeg".into(),
                "pipe:1".into(),
            ],
        ) else {
            continue;
        };
        if is_jpeg(&bytes) {
            return Some(bytes);
        }
    }

    // Matroska stores cover art as an attachment instead of an attached-picture video stream.
    let attachment_count = stderr
        .lines()
        .filter(|line| line.contains("Attachment:"))
        .count();
    for index in 0..attachment_count.min(16) {
        let temp = std::env::temp_dir().join(format!("tmd-cover-{}.bin", uuid::Uuid::new_v4()));
        let result = ffmpeg_command(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error"])
            .arg(format!("-dump_attachment:t:{index}"))
            .arg(&temp)
            .arg("-i")
            .arg(video)
            .args(["-f", "null", "-"])
            .output();
        let Ok(result) = result else {
            let _ = std::fs::remove_file(&temp);
            continue;
        };
        if result.status.success() && temp.is_file() {
            let jpeg = image_to_jpeg(ffmpeg, &temp).ok();
            let _ = std::fs::remove_file(&temp);
            if let Some(bytes) = jpeg.filter(|bytes| is_jpeg(bytes)) {
                return Some(bytes);
            }
        } else {
            let _ = std::fs::remove_file(&temp);
        }
    }
    None
}

fn extract_video_frame_from_path(
    ffmpeg: &Path,
    video: &Path,
    timestamp_seconds: f64,
) -> Result<Vec<u8>, String> {
    let time_arg = format!("{timestamp_seconds:.3}");
    run_output(
        ffmpeg,
        &[
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-ss".into(),
            time_arg,
            "-i".into(),
            video.to_string_lossy().into_owned(),
            "-map".into(),
            "0:V:0".into(),
            "-frames:v".into(),
            "1".into(),
            "-vf".into(),
            "scale='min(1280,iw)':-2".into(),
            "-f".into(),
            "image2pipe".into(),
            "-vcodec".into(),
            "mjpeg".into(),
            "pipe:1".into(),
        ],
    )
}

fn generate_preview_jpeg(ffmpeg: &Path, video: &Path) -> Result<Vec<u8>, String> {
    if let Some(cover) = extract_attached_cover(ffmpeg, video) {
        return Ok(cover);
    }
    extract_video_frame_from_path(ffmpeg, video, 1.0)
        .or_else(|_| extract_video_frame_from_path(ffmpeg, video, 0.0))
}

pub async fn embed_cached_cover(app: &AppHandle, video: &Path, cover: &Path) -> Result<(), String> {
    let ffmpeg = ffmpeg_path(app)?;
    let video = video.to_path_buf();
    let cover = cover.to_path_buf();
    tokio::task::spawn_blocking(move || embed_cover(&ffmpeg, &video, &cover))
        .await
        .map_err(command_error)?
}

pub async fn embed_task_cover_if_selected(
    app: &AppHandle,
    layout_root: &Path,
    task_id: &str,
    video: &Path,
) -> Result<bool, String> {
    let cover = cover_path(layout_root, task_id);
    if !cover.is_file() {
        return Ok(false);
    }
    embed_cached_cover(app, video, &cover).await?;
    Ok(true)
}

fn run_output(binary: &Path, args: &[String]) -> Result<Vec<u8>, String> {
    let output = ffmpeg_command(binary)
        .args(args)
        .output()
        .map_err(|error| format!("无法启动视频处理工具：{error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        return Err(if detail.is_empty() {
            "视频文件无法读取或封面格式不受支持".into()
        } else {
            detail.chars().take(900).collect()
        });
    }
    Ok(output.stdout)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn probe_task_video(
    app: AppHandle,
    state: State<'_, AppState>,
    task_id: String,
) -> Result<VideoPreviewInfo, String> {
    let (video_path, download_complete, _) = video_for_task(&state, &task_id).await?;
    app.asset_protocol_scope()
        .allow_file(&video_path)
        .map_err(command_error)?;
    let ffmpeg = ffmpeg_path(&app)?;
    let path_arg = video_path.to_string_lossy().into_owned();
    let duration_seconds = tokio::task::spawn_blocking(move || {
        let output = ffmpeg_command(&ffmpeg)
            .args(["-hide_banner", "-i"])
            .arg(path_arg)
            .output()
            .map_err(|error| format!("无法启动视频处理工具：{error}"))?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stamp = stderr
            .split_once("Duration: ")
            .and_then(|(_, tail)| tail.split([',', '\r', '\n']).next())
            .ok_or_else(|| "无法读取视频时长；请确认文件包含有效的视频流".to_owned())?;
        let mut components = stamp.trim().split(':');
        let hours = components.next().and_then(|part| part.parse::<f64>().ok());
        let minutes = components.next().and_then(|part| part.parse::<f64>().ok());
        let seconds = components.next().and_then(|part| part.parse::<f64>().ok());
        let duration = match (hours, minutes, seconds) {
            (Some(hours), Some(minutes), Some(seconds)) => {
                hours * 3600.0 + minutes * 60.0 + seconds
            }
            _ => return Err("无法读取视频时长；请确认文件包含有效的视频流".to_owned()),
        };
        if duration.is_finite() && duration > 0.0 {
            Ok(duration)
        } else {
            Err("视频时长无效".to_owned())
        }
    })
    .await
    .map_err(command_error)??;
    let layout = state.shared.layout.read().await.clone();
    let cover = cover_path(&layout.root, &task_id);
    let cover_path = if cover.is_file() {
        app.asset_protocol_scope()
            .allow_file(&cover)
            .map_err(command_error)?;
        Some(cover.to_string_lossy().into_owned())
    } else {
        None
    };
    Ok(VideoPreviewInfo {
        video_path: video_path.to_string_lossy().into_owned(),
        duration_seconds,
        cover_path,
        download_complete,
    })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn extract_video_frame(
    app: AppHandle,
    state: State<'_, AppState>,
    task_id: String,
    timestamp_seconds: f64,
) -> Result<String, String> {
    if !timestamp_seconds.is_finite() || timestamp_seconds < 0.0 {
        return Err("帧位置无效".into());
    }
    let (video, _, _) = video_for_task(&state, &task_id).await?;
    let ffmpeg = ffmpeg_path(&app)?;
    let ffmpeg_for_work = ffmpeg.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        extract_video_frame_from_path(&ffmpeg_for_work, &video, timestamp_seconds)
    })
    .await
    .map_err(command_error)??;
    if bytes.is_empty() || bytes.len() > MAX_COVER_BYTES {
        return Err("提取的视频帧为空或尺寸过大".into());
    }
    Ok(STANDARD.encode(bytes))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_task_video_thumbnail(
    app: AppHandle,
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Option<String>, String> {
    let (video, _, _) = video_for_task(&state, &task_id).await?;
    let layout = state.shared.layout.read().await.clone();
    let custom = cover_path(&layout.root, &task_id);
    let generated = preview_path(&layout.root, &task_id);
    let cached = if custom.is_file() {
        Some(custom)
    } else if generated.is_file() {
        Some(generated.clone())
    } else {
        None
    };
    if let Some(path) = cached {
        if let Ok(bytes) = tokio::fs::read(&path).await
            && !bytes.is_empty()
            && bytes.len() <= MAX_COVER_BYTES
            && is_jpeg(&bytes)
        {
            return Ok(Some(format!(
                "data:image/jpeg;base64,{}",
                STANDARD.encode(bytes)
            )));
        }
    }

    let _permit = THUMBNAIL_JOBS.acquire().await.map_err(command_error)?;
    // Another visible row or list refresh may have generated the same image while this command
    // waited for the thumbnail worker slot.
    if generated.is_file() {
        if let Ok(bytes) = tokio::fs::read(&generated).await
            && !bytes.is_empty()
            && bytes.len() <= MAX_COVER_BYTES
            && is_jpeg(&bytes)
        {
            return Ok(Some(format!(
                "data:image/jpeg;base64,{}",
                STANDARD.encode(bytes)
            )));
        }
    }
    let ffmpeg = ffmpeg_path(&app)?;
    let video_for_work = video.clone();
    let bytes =
        tokio::task::spawn_blocking(move || generate_preview_jpeg(&ffmpeg, &video_for_work))
            .await
            .map_err(command_error)??;
    if bytes.is_empty() || bytes.len() > MAX_COVER_BYTES || !is_jpeg(&bytes) {
        return Ok(None);
    }
    atomic_file::write(&generated, &bytes).map_err(command_error)?;
    Ok(Some(format!(
        "data:image/jpeg;base64,{}",
        STANDARD.encode(bytes)
    )))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn set_task_video_cover(
    app: AppHandle,
    state: State<'_, AppState>,
    task_id: String,
    jpeg_base64: String,
) -> Result<CoverSaveResult, String> {
    let (_, _, output_extension) = video_for_task(&state, &task_id).await?;
    if jpeg_base64.len() > MAX_COVER_BYTES * 2 {
        return Err("封面图过大".into());
    }
    let image = STANDARD
        .decode(jpeg_base64.as_bytes())
        .map_err(|_| "封面图数据无效".to_owned())?;
    if image.len() < 4 || image.len() > MAX_COVER_BYTES || !image.starts_with(&[0xff, 0xd8, 0xff]) {
        return Err("封面必须是有效的 JPEG 图片，且不能超过 4 MB".into());
    }
    container_family_from_extension(&output_extension)?;
    let layout = state.shared.layout.read().await.clone();
    let cached_cover = cover_path(&layout.root, &task_id);
    atomic_file::write(&cached_cover, &image).map_err(command_error)?;
    let current = state
        .shared
        .store
        .get(&task_id)
        .await
        .map_err(command_error)?
        .ok_or_else(|| "任务已不存在".to_owned())?;
    let embedded = if current.status.eq_ignore_ascii_case("completed") {
        let source = PathBuf::from(
            current
                .output_path
                .ok_or_else(|| "任务没有本地文件".to_owned())?,
        )
        .canonicalize()
        .map_err(command_error)?;
        embed_cached_cover(&app, &source, &cached_cover).await?;
        true
    } else {
        false
    };
    app.asset_protocol_scope()
        .allow_file(&cached_cover)
        .map_err(command_error)?;
    Ok(CoverSaveResult {
        cover_path: cached_cover.to_string_lossy().into_owned(),
        embedded,
    })
}

pub fn attach_cover_paths(layout_root: &Path, tasks: &mut [crate::models::TaskRecord]) {
    for task in tasks {
        let cover = cover_path(layout_root, &task.task_id);
        if cover.is_file() {
            task.cover_path = Some(storage::display_path(&cover));
        }
        let preview = preview_path(layout_root, &task.task_id);
        if preview.is_file() {
            task.preview_path = Some(storage::display_path(&preview));
        }
    }
}
