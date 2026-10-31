//! Base URLs of the providers upstream hard-codes (Cloudflare, Doppler, Have I Been Pwned).
//!
//! Production always uses the real URL. Differential tests set the test-only setting
//! [`TEST_ONLY_FAKE_PROVIDERS`] to the origin of a fake on this machine: every hosted provider
//! then lives under `{origin}/{provider}`. Anything but a loopback `http` origin is ignored, so
//! the setting can never send a token to another host.

use std::net::IpAddr;
use std::time::Duration;

use coolify_db::Db;
use reqwest::Client;

use crate::databases::s3::{UrlPolicy, endpoint_error, safe_url_pin};

/// Test-only setting. Never set this on an instance that manages anything real.
pub const TEST_ONLY_FAKE_PROVIDERS: &str = "COOLIFY_RS_TEST_ONLY_FAKE_PROVIDERS";

/// `http://127.0.0.1:port`, `http://localhost:port` or `http://[::1]:port`, without a path.
fn loopback_origin(value: &str) -> Option<String> {
    let origin = value.trim().trim_end_matches('/');
    let authority = origin.strip_prefix("http://")?;
    if authority.contains(['/', '?', '#', '@']) {
        return None;
    }
    let host = match authority.strip_prefix('[') {
        Some(rest) => rest.split(']').next()?,
        None => authority.split(':').next()?,
    };
    (host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()))
        .then(|| origin.to_owned())
}

/// The fake's origin, when the test-only setting holds a loopback origin.
pub fn fake_origin() -> Option<String> {
    std::env::var(TEST_ONLY_FAKE_PROVIDERS)
        .ok()
        .and_then(|v| loopback_origin(&v))
}

/// `production`, or `{fake}/{provider}` under the test-only setting.
pub fn hosted(provider: &str, production: &str) -> String {
    match fake_origin() {
        Some(origin) => format!("{origin}/{provider}"),
        None => production.to_owned(),
    }
}

/// Whether a user-entered base URL is the fake of the test-only setting.
pub fn is_fake(url: &str) -> bool {
    fake_origin().is_some_and(|origin| url == origin || url.starts_with(&format!("{origin}/")))
}

/// A client for a hard-coded provider: `->connectTimeout(5)->timeout($seconds)`.
pub fn hosted_client(seconds: u64) -> Result<Client, String> {
    Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(seconds))
        .user_agent("coolify-rs")
        .build()
        .map_err(|e| e.to_string())
}

/// `Validator::make(['base_url' => $url], ['base_url' => new SafeExternalUrl])->validate()`.
pub async fn base_url_error(db: &Db, url: &str, settings_url: &str) -> Option<String> {
    if is_fake(url) {
        return None;
    }
    let policy = UrlPolicy::load(db).await;
    endpoint_error(url, "base_url", &policy, settings_url).await
}

/// `SafeExternalUrl::httpClientOptions($url)`: the request is pinned to the validated addresses.
pub async fn pinned_client(
    db: &Db,
    url: &str,
    connect: Duration,
    total: Duration,
) -> Result<Client, String> {
    let mut builder = Client::builder()
        .connect_timeout(connect)
        .timeout(total)
        .user_agent("coolify-rs");
    if !is_fake(url) {
        let policy = UrlPolicy::load(db).await;
        let (host, ips, port) = safe_url_pin(url, &policy).await?;
        if !ips.is_empty() {
            let addrs: Vec<std::net::SocketAddr> = ips
                .iter()
                .map(|ip| std::net::SocketAddr::new(*ip, port))
                .collect();
            builder = builder.resolve_to_addrs(&host, &addrs);
        }
    }
    builder.build().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_http_origins_count() {
        for good in [
            "http://127.0.0.1:18191",
            "http://127.0.0.1:18191/",
            "http://localhost:9",
            "http://[::1]:9",
        ] {
            assert!(loopback_origin(good).is_some(), "{good}");
        }
        for bad in [
            "https://127.0.0.1:18191",
            "http://example.com",
            "http://127.0.0.1.example.com",
            "http://user@127.0.0.1",
            "http://127.0.0.1:1/x",
            "http://10.0.0.1:1",
            "",
        ] {
            assert!(loopback_origin(bad).is_none(), "{bad}");
        }
    }
}
