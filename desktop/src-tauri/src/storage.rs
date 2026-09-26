use crate::models::{Settings, StorageOption};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

pub const APP_FOLDER: &str = "TelegramMediaDownloader";
const SETTINGS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct StorageLayout {
    pub root: PathBuf,
    pub settings_file: PathBuf,
    pub database_file: PathBuf,
    pub downloads: PathBuf,
    pub parts: PathBuf,
    pub logs: PathBuf,
    pub webview: PathBuf,
}

impl StorageLayout {
    pub fn under(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            settings_file: root.join("client-settings.json"),
            database_file: root.join("tasks.sqlite3"),
            downloads: root.join("Downloads"),
            parts: root.join("TaskData"),
            logs: root.join("Logs"),
            webview: root.join("WebView"),
            root,
        }
    }

    pub fn ensure(&self) -> Result<()> {
        for path in [
            &self.root,
            &self.downloads,
            &self.parts,
            &self.logs,
            &self.webview,
            &self.root.join("Cache"),
        ] {
            fs::create_dir_all(path)
                .with_context(|| format!("无法创建数据目录：{}", display_path(path)))?;
        }
        Ok(())
    }
}

/// 去掉 Windows canonicalize 产生的 `\\?\` 前缀;仅用于展示/持久化,不用于文件 IO。
pub fn display_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        raw.into_owned()
    }
}

pub fn resolve_active_root(pointer_file: &Path, fallback: &Path) -> Result<PathBuf> {
    match fs::read_to_string(pointer_file) {
        Ok(text) if !text.trim().is_empty() => {
            let configured = PathBuf::from(text.trim());
            if !configured.is_absolute() {
                bail!("客户端数据目录记录无效；为保护旧数据，没有自动改写目录记录");
            }
            if !configured.is_dir() {
                bail!(
                    "客户端数据目录当前不可用：{}。请连接该磁盘后重试，或通过恢复流程选择数据目录。",
                    display_path(&configured)
                );
            }
            return Ok(configured);
        }
        Ok(_) => bail!("客户端数据目录记录为空；为保护旧数据，没有自动重置目录"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("无法读取客户端数据目录记录"),
    }
    let chosen = choose_default_root(fallback);
    fs::create_dir_all(&chosen)
        .with_context(|| format!("无法创建数据目录：{}", display_path(&chosen)))?;
    if let Some(parent) = pointer_file.parent() {
        fs::create_dir_all(parent)?;
    }
    crate::atomic_file::write(pointer_file, chosen.to_string_lossy().as_bytes())?;
    Ok(chosen)
}

pub fn choose_default_root(fallback: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let system = system_drive
            .trim_end_matches(['\\', '/'])
            .to_ascii_lowercase();
        let mut choices = Vec::new();
        for letter in b'D'..=b'Z' {
            let drive = format!("{}:\\", letter as char);
            let path = PathBuf::from(&drive);
            if !path.exists() {
                continue;
            }
            if let Ok(free) = fs2::available_space(&path) {
                choices.push((free, path.join(APP_FOLDER)));
            }
        }
        if let Some((_, path)) = choices.into_iter().max_by_key(|(free, _)| *free) {
            return path;
        }
        if let Some(current) = std::env::current_dir()
            .ok()
            .and_then(|p| p.ancestors().last().map(Path::to_path_buf))
        {
            let drive = current.to_string_lossy().to_ascii_lowercase();
            if !drive.starts_with(&system) {
                return current.join(APP_FOLDER);
            }
        }
    }
    fallback.join(APP_FOLDER)
}

pub fn enumerate_storage(fallback: &Path) -> Vec<StorageOption> {
    #[cfg(windows)]
    {
        let system = std::env::var("SystemDrive")
            .unwrap_or_else(|_| "C:".into())
            .to_ascii_lowercase();
        let mut result = Vec::new();
        for letter in b'C'..=b'Z' {
            let drive = PathBuf::from(format!("{}:\\", letter as char));
            if !drive.exists() {
                continue;
            }
            let available = fs2::available_space(&drive).unwrap_or(0);
            result.push(StorageOption {
                path: drive.join(APP_FOLDER).to_string_lossy().into_owned(),
                label: Some(format!("{} 盘", letter as char)),
                available_bytes: available,
                total_bytes: fs2::total_space(&drive).ok(),
                is_system: drive
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .starts_with(&system),
            });
        }
        if !result.is_empty() {
            return result;
        }
    }
    vec![StorageOption {
        path: fallback.join(APP_FOLDER).to_string_lossy().into_owned(),
        label: fallback
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
        available_bytes: fs2::available_space(fallback).unwrap_or(0),
        total_bytes: fs2::total_space(fallback).ok(),
        is_system: true,
    }]
}

pub fn load_settings(layout: &StorageLayout) -> Result<Settings> {
    if !layout.settings_file.exists() {
        return Ok(Settings::default());
    }
    let data = fs::read(&layout.settings_file)?;
    let value: serde_json::Value = serde_json::from_slice(&data).context("设置文件格式无法读取")?;
    let mut settings: Settings = if let Some(version) = value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
    {
        if version > u64::from(SETTINGS_SCHEMA_VERSION) {
            bail!("设置文件来自更新版本的客户端，当前版本不会覆盖它");
        }
        serde_json::from_value(
            value
                .get("settings")
                .cloned()
                .context("设置文件缺少 settings 字段")?,
        )
        .context("设置文件版本或内容无法读取")?
    } else {
        // Desktop versions before schemaVersion stored Settings as a flat JSON object.
        serde_json::from_value(value).context("旧版设置格式无法读取")?
    };
    settings.data_root = display_path(&layout.root);
    if settings.download_root.trim().is_empty() {
        settings.download_root = display_path(&layout.downloads);
    }
    Ok(settings)
}

pub fn save_settings(layout: &StorageLayout, settings: &Settings) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(
        &serde_json::json!({"schemaVersion": SETTINGS_SCHEMA_VERSION, "settings": settings}),
    )?;
    crate::atomic_file::write(&layout.settings_file, &bytes)
}

pub fn copy_tree_verified(source: &Path, destination: &Path) -> Result<()> {
    if source == destination {
        return Ok(());
    }
    if destination.starts_with(source) || source.starts_with(destination) {
        bail!("新旧数据目录不能互相包含");
    }
    fs::create_dir_all(destination)?;
    for entry in walkdir::WalkDir::new(source).follow_links(false) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target)?;
            if fs::metadata(entry.path())?.len() != fs::metadata(&target)?.len()
                || file_digest(entry.path())? != file_digest(&target)?
            {
                bail!("数据迁移校验失败：{}", relative.display());
            }
        }
    }
    Ok(())
}

fn file_digest(path: &Path) -> Result<[u8; 32]> {
    let mut file = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hasher.update(&buffer[..size]);
    }
    Ok(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_path_strips_windows_verbatim_prefixes() {
        let cases = [
            (r"E:\Media\Downloads", r"E:\Media\Downloads"),
            (r"\\?\E:\Media\Downloads", r"E:\Media\Downloads"),
            (r"\\?\UNC\server\share\dir", r"\\server\share\dir"),
        ];
        for (input, expected) in cases {
            assert_eq!(display_path(Path::new(input)), expected);
        }
    }

    #[test]
    fn flat_legacy_json_loads_and_next_save_is_versioned() {
        let temp = tempfile::tempdir().unwrap();
        let layout = StorageLayout::under(temp.path().join("data"));
        layout.ensure().unwrap();
        fs::write(
            &layout.settings_file,
            br#"{"api_id":"123","apiHash":"deadbeef","pathTemplate":"{chat}","captionSidecar":true}"#,
        )
        .unwrap();
        let settings = load_settings(&layout).unwrap();
        // 旧设置文件里的 Telegram 字段已被 serde 忽略,不再进入新的设置结构。
        assert_eq!(settings.path_template, "{chat}");
        save_settings(&layout, &settings).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&layout.settings_file).unwrap()).unwrap();
        assert_eq!(value["schemaVersion"], SETTINGS_SCHEMA_VERSION);
        assert!(value.get("settings").is_some());
        assert!(value["settings"].get("apiHash").is_none());
        assert!(value["settings"].get("captionSidecar").is_none());
    }

    #[test]
    fn refuses_to_downgrade_settings_from_a_newer_schema() {
        let temp = tempfile::tempdir().unwrap();
        let layout = StorageLayout::under(temp.path().join("data"));
        layout.ensure().unwrap();
        fs::write(
            &layout.settings_file,
            br#"{"schemaVersion":999,"settings":{}}"#,
        )
        .unwrap();
        assert!(load_settings(&layout).is_err());
    }

    #[test]
    fn unavailable_configured_data_root_is_not_silently_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let pointer = temp.path().join("active-data-root.txt");
        let missing_root = temp.path().join("offline-volume").join(APP_FOLDER);
        fs::write(&pointer, missing_root.to_string_lossy().as_bytes()).unwrap();
        let fallback = temp.path().join("fallback");
        assert!(resolve_active_root(&pointer, &fallback).is_err());
        assert_eq!(
            fs::read_to_string(&pointer).unwrap(),
            missing_root.to_string_lossy()
        );
        assert!(!fallback.exists());
    }
}
