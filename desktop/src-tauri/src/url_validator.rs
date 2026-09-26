//! 媒体 URL 白名单校验。仅允许 https + Telegram 域;拒绝私网地址(SSRF 兜底)。

use anyhow::{Result, bail};
use url::Url;

/// 白名单根域:裸域与任意子域均允许。
const ALLOWED_DOMAINS: &[&str] = &["telegram.org", "cdn-telegram.org"];

pub fn validate_media_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|e| anyhow::anyhow!("URL 无法解析:{e}"))?;
    if url.scheme() != "https" {
        bail!("仅允许 https URL");
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("URL 缺少主机名"))?;
    let host_lower = host.to_ascii_lowercase();
    let allowed = ALLOWED_DOMAINS
        .iter()
        .any(|d| host_lower == *d || host_lower.ends_with(&format!(".{d}")));
    if !allowed {
        bail!("URL 域名不在 Telegram 白名单内:{host}");
    }
    if is_private_host(&host_lower) {
        bail!("拒绝私网地址");
    }
    Ok(url)
}

fn is_private_host(host: &str) -> bool {
    if host == "localhost" || host == "::1" {
        return true;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
            std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
        };
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_telegram_https() {
        assert!(validate_media_url("https://web.telegram.org/file/x").is_ok());
        assert!(validate_media_url("https://cdn-telegram.org/a/b").is_ok());
        assert!(validate_media_url("https://abc.cdn-telegram.org/a/b").is_ok());
        assert!(validate_media_url("https://webk.telegram.org/k/").is_ok());
    }

    #[test]
    fn rejects_non_telegram() {
        assert!(validate_media_url("https://evil.com/x").is_err());
        assert!(validate_media_url("https://telegram.org.evil.com/x").is_err());
        assert!(validate_media_url("https://nottelegram.org/x").is_err());
    }

    #[test]
    fn rejects_non_https_and_private() {
        assert!(validate_media_url("http://web.telegram.org/x").is_err());
        assert!(validate_media_url("https://127.0.0.1/x").is_err());
        assert!(validate_media_url("file:///etc/passwd").is_err());
    }
}
