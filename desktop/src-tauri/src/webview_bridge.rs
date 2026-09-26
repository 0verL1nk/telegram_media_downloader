//! Restricted Telegram Web child-webview integration.
//!
//! Tauri's child [`WebviewBuilder`] API is still marked `unstable` in Tauri
//! 2.11.6. Keep all use of that API and the remote-page boundary in this
//! module. Every host command exposed to the `telegram` capability must still
//! re-check the invoking webview label and current URL; see
//! `commands::start_webview_download` for the pattern. Those commands must
//! never resolve media through a Telegram API and never accept URLs, cookies,
//! or filesystem paths from the remote document: the page fetches the bytes
//! itself and pushes them to `push_chunk`.
//! Tauri 2.11.6 has no per-`WebviewBuilder` `with_global_tauri` method;
//! `app.withGlobalTauri` is an app-level config option. Enable it in the
//! application config if this script is to use the public `window.__TAURI__`
//! API, and keep the remote webview's capability restricted to the download
//! pipeline commands.
//!
//! The injected script (built from `webview-inject/` and embedded below)
//! polls Telegram Web for the media viewer / story viewer / pinned audio,
//! injects a download button into the native toolbar, and on click runs the
//! page-fetch download pipeline (`start_webview_download` → `plan_chunks` →
//! `push_chunk` → `finish_download`), handing the fetched bytes to the Rust
//! task manager while the media URL stays in the page. The script never reads
//! cookies or storage; it reads only the media element the user opened.

use std::path::PathBuf;
use tauri::{
    LogicalPosition, LogicalSize, Webview, Window, Wry,
    utils::config::WebviewUrl as ConfigWebviewUrl,
    webview::{NewWindowResponse, WebviewBuilder},
};
use url::Url;

pub const TELEGRAM_WEBVIEW_LABEL: &str = "telegram";
pub const TELEGRAM_WEBVIEW_START_URL: &str = "https://web.telegram.org/";

/// Logical-pixel bounds within the native main window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TelegramWebviewBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl TelegramWebviewBounds {
    fn validate(self) -> Result<Self, String> {
        let values = [self.x, self.y, self.width, self.height];
        if values.iter().any(|value| !value.is_finite()) {
            return Err("Telegram WebView bounds must be finite logical pixels".into());
        }
        if self.x < 0.0 || self.y < 0.0 || self.width <= 0.0 || self.height <= 0.0 {
            return Err(
                "Telegram WebView bounds must have a non-negative origin and positive size".into(),
            );
        }
        Ok(self)
    }
}

/// Return whether a navigation URL is on Telegram Web's exact HTTPS origin.
///
/// This intentionally rejects subdomains, alternate ports, credentials, and
/// non-HTTPS schemes. Telegram's `/a` and `/k` routes remain allowed.
pub fn is_telegram_web_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("web.telegram.org")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
}

/// Build the remote child webview used inside the native main window.
///
/// Tauri 2.11.6 documents `WebviewBuilder`/`Window::add_child` as unstable.
/// New windows are denied so Telegram cannot escape into a second native
/// window. The parent UI owns the child bounds and visibility.
pub fn telegram_webview_builder(data_directory: PathBuf) -> WebviewBuilder<Wry> {
    let url = Url::parse(TELEGRAM_WEBVIEW_START_URL)
        .expect("the compiled-in Telegram Web start URL must be valid");

    WebviewBuilder::new(TELEGRAM_WEBVIEW_LABEL, ConfigWebviewUrl::External(url))
        .data_directory(data_directory)
        .initialization_script(TELEGRAM_WEBVIEW_INIT_SCRIPT)
        .on_navigation(is_telegram_web_url)
        .on_new_window(|_url, _features| NewWindowResponse::Deny)
        .focused(false)
}

/// Create Telegram Web as a child of the supplied Tauri window.
///
/// Call from the application's asynchronous setup path on Windows: Tauri
/// 2.11.6 documents a WebView2 deadlock when `Window::add_child` is used from
/// synchronous commands or event handlers.
pub fn create_telegram_webview(
    window: &Window,
    bounds: TelegramWebviewBounds,
    data_directory: PathBuf,
) -> Result<Webview, String> {
    let bounds = bounds.validate()?;
    window
        .add_child(
            telegram_webview_builder(data_directory),
            LogicalPosition::new(bounds.x, bounds.y),
            LogicalSize::new(bounds.width, bounds.height),
        )
        .map_err(|error| format!("create Telegram WebView: {error}"))
}

/// Update the child view's logical-pixel layout without resizing the native
/// window. The caller should recompute these bounds when its own layout or
/// the window size changes.
pub fn update_telegram_webview_bounds(
    webview: &Webview,
    bounds: TelegramWebviewBounds,
) -> Result<(), String> {
    let bounds = bounds.validate()?;
    webview
        .set_position(LogicalPosition::new(bounds.x, bounds.y))
        .map_err(|error| format!("move Telegram WebView: {error}"))?;
    webview
        .set_size(LogicalSize::new(bounds.width, bounds.height))
        .map_err(|error| format!("resize Telegram WebView: {error}"))
}

/// Show or hide the Telegram child view while keeping its login state alive.
pub fn set_telegram_webview_visible(webview: &Webview, visible: bool) -> Result<(), String> {
    let result = if visible {
        webview.show()
    } else {
        webview.hide()
    };
    result.map_err(|error| {
        format!(
            "{} Telegram WebView: {error}",
            if visible { "show" } else { "hide" }
        )
    })
}

/// Check both the child label and its current location before accepting an
/// invocation from a Tauri command. Tauri capabilities are the first gate;
/// command handlers should call this as an additional runtime check.
pub fn is_trusted_telegram_webview(webview: &Webview) -> bool {
    webview.label() == TELEGRAM_WEBVIEW_LABEL
        && webview.url().is_ok_and(|url| is_telegram_web_url(&url))
}

/// Built from `webview-inject/src` by `webview-inject/build.mjs` (invoked
/// from `build.rs`); `dist/inject.js` is generated and git-ignored.
pub const TELEGRAM_WEBVIEW_INIT_SCRIPT: &str = include_str!("../webview-inject/dist/inject.js");
