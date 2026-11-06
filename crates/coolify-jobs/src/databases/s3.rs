//! `S3Storage`: `testConnection` (Flysystem `files()` = `ListObjectsV2`), object
//! checks and deletes used by backups, signed with AWS SigV4, with path-style or
//! virtual-hosted-style URLs; `SafeWebhookUrl` for the endpoint; `ValidS3BucketName`.

use std::net::IpAddr;
use std::time::Duration;

use chrono::Utc;
use coolify_compat::Encrypter;
use coolify_db::Db;
use coolify_db::database_parts as parts;
use coolify_db::records::Record;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::Jobs;

/// `ValidationPatterns::isValidS3BucketName`.
pub fn valid_bucket_name(v: &str) -> bool {
    let len = v.chars().count();
    if !(3..=63).contains(&len) {
        return false;
    }
    let ok_edge = |c: char| c.is_ascii_alphanumeric();
    if !v.chars().next().is_some_and(ok_edge) || !v.chars().last().is_some_and(ok_edge) {
        return false;
    }
    if !v
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return false;
    }
    if v.contains("..") || v.contains(".-") || v.contains("-.") {
        return false;
    }
    v.parse::<std::net::Ipv4Addr>().is_err()
}

/// The instance settings `SafeWebhookUrl` reads.
#[derive(Debug, Clone, Default)]
pub struct UrlPolicy {
    /// `webhook_allowed_internal_hosts`, normalized.
    pub allowed: Vec<String>,
    pub allow_localhost: bool,
    /// `allowPrivateNetworks` (`SafeWebhookUrl::forGitSource()`).
    pub allow_private: bool,
}

impl UrlPolicy {
    pub async fn load(db: &Db) -> Self {
        let row: Option<(Option<String>, Option<bool>)> = sqlx_settings(db).await;
        let (allowed, allow_localhost) = row.unwrap_or_default();
        let entries: Vec<String> = allowed
            .map(
                |text| match serde_json::from_str::<Vec<serde_json::Value>>(&text) {
                    Ok(list) => list
                        .iter()
                        .map(|v| {
                            v.as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| v.to_string())
                        })
                        .collect(),
                    Err(_) => text.split(',').map(str::to_owned).collect(),
                },
            )
            .unwrap_or_default();
        Self {
            allowed: entries
                .iter()
                .map(|e| e.trim().to_lowercase().trim_end_matches('.').to_owned())
                .filter(|e| !e.is_empty())
                .collect(),
            allow_localhost: allow_localhost.unwrap_or(false),
            allow_private: false,
        }
    }

    /// `SafeWebhookUrl::resolveUrlForRequest()`: the addresses to pin the request to (none for
    /// an IP host) or the exception message.
    pub async fn request_target(&self, url: &str) -> Result<(String, u16, Vec<IpAddr>), String> {
        let parsed = parse_url(url).ok_or("Webhook URL is invalid.")?;
        if parsed.host.ends_with('.') {
            return Err("Webhook URL host must not end with a trailing dot.".into());
        }
        if parsed.scheme != "http" && parsed.scheme != "https" {
            return Err("Webhook URL scheme is unsafe.".into());
        }
        let host = parsed.host.to_lowercase();
        let host = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(&host)
            .trim_end_matches('.')
            .to_owned();
        if let Ok(ip) = host.parse::<IpAddr>() {
            if !self.allowed_ip(ip, &host) {
                return Err("Webhook URL resolved to an unsafe IP address.".into());
            }
            return Ok((host, parsed.port, Vec::new()));
        }
        let ips = resolve(&host, parsed.port).await;
        if ips.is_empty() {
            return Err("Webhook URL host could not be resolved.".into());
        }
        if ips.iter().any(|ip| !self.allowed_ip(*ip, &host)) {
            return Err("Webhook URL resolved to an unsafe IP address.".into());
        }
        if self.blocked_host(&host) && !self.allowed_host(&host) {
            return Err("Webhook URL host is unsafe.".into());
        }
        Ok((host, parsed.port, ips))
    }

    /// `isBlockedHostname`.
    fn blocked_host(&self, host: &str) -> bool {
        host == "localhost" || (!self.allow_private && blocked_hostname(host))
    }

    fn allowed_host(&self, host: &str) -> bool {
        self.allowed.iter().any(|e| !e.contains('/') && e == host)
    }

    fn allowlisted_ip(&self, ip: IpAddr) -> bool {
        self.allowed.iter().any(|e| {
            if let Some((net, prefix)) = e.split_once('/') {
                match (net.parse::<IpAddr>(), prefix.parse::<u32>()) {
                    (Ok(n), Ok(p)) => in_cidr(ip, n, p),
                    _ => false,
                }
            } else {
                e.parse::<IpAddr>().is_ok_and(|a| a == ip)
            }
        })
    }

    /// `isAllowedIp`.
    fn allowed_ip(&self, ip: IpAddr, host: &str) -> bool {
        let ip = mapped_v4(ip);
        if link_local(ip) {
            return false;
        }
        if public_ip(ip) {
            return true;
        }
        if loopback(ip) {
            return self.allow_localhost && (self.allowed_host(host) || self.allowlisted_ip(ip));
        }
        if private_ip(ip) {
            return self.allow_private || self.allowed_host(host) || self.allowlisted_ip(ip);
        }
        if self.allow_private && v4_in(ip, ("100.64.0.0", 10)) {
            return true;
        }
        self.allowlisted_ip(ip)
    }
}

async fn sqlx_settings(db: &Db) -> Option<(Option<String>, Option<bool>)> {
    coolify_db::instance::webhook_policy(db)
        .await
        .ok()
        .flatten()
}

fn mapped_v4(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

fn in_cidr(ip: IpAddr, net: IpAddr, prefix: u32) -> bool {
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(b)) if prefix <= 32 => {
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            u32::from(a) & mask == u32::from(b) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(b)) if prefix <= 128 => {
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            u128::from(a) & mask == u128::from(b) & mask
        }
        _ => false,
    }
}

fn v4_in(ip: IpAddr, cidr: (&str, u32)) -> bool {
    cidr.0
        .parse::<IpAddr>()
        .is_ok_and(|n| in_cidr(ip, n, cidr.1))
}

fn link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(_) => v4_in(ip, ("169.254.0.0", 16)),
        IpAddr::V6(_) => v4_in(ip, ("fe80::", 10)),
    }
}

fn loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(_) => v4_in(ip, ("127.0.0.0", 8)),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// `FILTER_FLAG_NO_PRIV_RANGE` fails for these.
fn private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(_) => {
            v4_in(ip, ("10.0.0.0", 8))
                || v4_in(ip, ("172.16.0.0", 12))
                || v4_in(ip, ("192.168.0.0", 16))
        }
        IpAddr::V6(_) => v4_in(ip, ("fc00::", 7)),
    }
}

/// `isPublicIp`: not private, reserved or special use.
fn public_ip(ip: IpAddr) -> bool {
    if private_ip(ip) {
        return false;
    }
    match ip {
        IpAddr::V4(_) => ![
            ("0.0.0.0", 8),
            ("100.64.0.0", 10),
            ("127.0.0.0", 8),
            ("169.254.0.0", 16),
            ("192.0.0.0", 24),
            ("192.0.2.0", 24),
            ("198.18.0.0", 15),
            ("198.51.100.0", 24),
            ("203.0.113.0", 24),
            ("224.0.0.0", 4),
            ("240.0.0.0", 4),
            ("255.255.255.255", 32),
        ]
        .into_iter()
        .any(|c| v4_in(ip, c)),
        IpAddr::V6(_) => ![
            ("::", 128),
            ("::1", 128),
            ("::ffff:0:0", 96),
            ("64:ff9b::", 96),
            ("64:ff9b:1::", 48),
            ("100::", 64),
            ("2001::", 23),
            ("2001:2::", 48),
            ("2001:db8::", 32),
            ("2002::", 16),
            ("fc00::", 7),
            ("fe80::", 10),
            ("ff00::", 8),
        ]
        .into_iter()
        .any(|c| v4_in(ip, c)),
    }
}

fn blocked_hostname(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".cluster.local")
}

struct ParsedUrl {
    scheme: String,
    host: String,
    port: u16,
    authority: String,
}

/// The parts of `parse_url` the rule reads, after `FILTER_VALIDATE_URL`.
fn parse_url(url: &str) -> Option<ParsedUrl> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let hostport = authority.rsplit('@').next()?;
    if hostport.is_empty() || hostport.chars().any(char::is_whitespace) {
        return None;
    }
    let (host, port) = if let Some(stripped) = hostport.strip_prefix('[') {
        let (h, after) = stripped.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p.parse::<u16>().ok()?),
            None if after.is_empty() => None,
            None => return None,
        };
        (format!("[{h}]"), port)
    } else if let Some((h, p)) = hostport.rsplit_once(':') {
        (h.to_owned(), Some(p.parse::<u16>().ok()?))
    } else {
        (hostport.to_owned(), None)
    };
    if host.is_empty() {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    let port = port.unwrap_or(if scheme == "https" { 443 } else { 80 });
    Some(ParsedUrl {
        scheme,
        authority: hostport.to_owned(),
        host,
        port,
    })
}

async fn resolve(host: &str, port: u16) -> Vec<IpAddr> {
    if host == "localhost" {
        return vec![
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
        ];
    }
    tokio::net::lookup_host((host, port))
        .await
        .map(|a| {
            let mut out: Vec<IpAddr> = Vec::new();
            for s in a {
                if !out.contains(&s.ip()) {
                    out.push(s.ip());
                }
            }
            out
        })
        .unwrap_or_default()
}

/// `SafeWebhookUrl::validate`: the failure message, `:attribute` replaced by the caller's name.
/// `settings_url` is `route('settings.advanced')`.
pub async fn endpoint_error(
    url: &str,
    attribute: &str,
    policy: &UrlPolicy,
    settings_url: &str,
) -> Option<String> {
    let private_message = || {
        format!(
            "The {attribute} points to a local or private address that is not allowed. Configure allowed internal targets: {settings_url}#endpoint-section"
        )
    };
    let Some(parsed) = parse_url(url) else {
        return Some(format!("The {attribute} must be a valid URL."));
    };
    if parsed.scheme != "https" && parsed.scheme != "http" {
        return Some(format!(
            "The {attribute} must use the http or https scheme."
        ));
    }
    if parsed.host.ends_with('.') {
        return Some(format!(
            "The {attribute} host must not end with a trailing dot."
        ));
    }
    let host = parsed.host.to_lowercase();
    let for_ip = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(&host)
        .to_owned();
    if policy.blocked_host(&for_ip) && !policy.allowed_host(&for_ip) {
        return Some(private_message());
    }
    if let Ok(ip) = for_ip.parse::<IpAddr>() {
        if !policy.allowed_ip(ip, &for_ip) {
            return Some(if link_local(mapped_v4(ip)) {
                format!("The {attribute} must not point to link-local addresses.")
            } else {
                private_message()
            });
        }
        return None;
    }
    let ips = resolve(&for_ip, parsed.port).await;
    if ips.is_empty() {
        return Some(format!("The {attribute} host could not be resolved."));
    }
    for ip in ips {
        if !policy.allowed_ip(ip, &for_ip) {
            return Some(if link_local(mapped_v4(ip)) {
                format!("The {attribute} must not resolve to a link-local address.")
            } else {
                private_message()
            });
        }
    }
    None
}

/// `SafeWebhookUrl::httpClientOptions` without the DNS servers of the instance: the host, its
/// validated addresses (none for an IP host) and the port, for pinning the request.
pub async fn safe_url_pin(
    url: &str,
    policy: &UrlPolicy,
) -> Result<(String, Vec<IpAddr>, u16), String> {
    let parsed = parse_url(url).ok_or("Webhook URL is invalid.")?;
    if parsed.host.ends_with('.') {
        return Err("Webhook URL host must not end with a trailing dot.".to_owned());
    }
    if parsed.scheme != "http" && parsed.scheme != "https" {
        return Err("Webhook URL scheme is unsafe.".to_owned());
    }
    let host = parsed.host.to_lowercase();
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(&host)
        .to_owned();
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !policy.allowed_ip(ip, &host) {
            return Err("Webhook URL resolved to an unsafe IP address.".to_owned());
        }
        return Ok((host, Vec::new(), parsed.port));
    }
    let ips = resolve(&host, parsed.port).await;
    if ips.is_empty() {
        return Err("Webhook URL host could not be resolved.".to_owned());
    }
    if ips.iter().any(|ip| !policy.allowed_ip(*ip, &host)) {
        return Err("Webhook URL resolved to an unsafe IP address.".to_owned());
    }
    if blocked_hostname(&host) && !policy.allowed_host(&host) {
        return Err("Webhook URL host is unsafe.".to_owned());
    }
    Ok((host, ips, parsed.port))
}

/// `SafeWebhookUrl::minioClientResolveOptions`: `host:port=ip`, IPv4 first; none for an IP host.
pub async fn minio_resolve_options(url: &str) -> Vec<String> {
    let Some(parsed) = parse_url(url) else {
        return Vec::new();
    };
    let host = parsed.host.to_lowercase();
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(&host)
        .to_owned();
    if host.parse::<IpAddr>().is_ok() {
        return Vec::new();
    }
    let ips = resolve(&host, parsed.port).await;
    let ip = ips.iter().find(|i| i.is_ipv4()).or_else(|| ips.first());
    ip.map(|ip| vec![format!("{host}:{}={ip}", parsed.port)])
        .unwrap_or_default()
}

/// `S3Storage::VIRTUAL_HOSTED_STYLE_HOST_SUFFIXES`: endpoint host suffixes of providers that reject
/// path-style URLs: Alibaba Cloud OSS, Tencent Cloud COS (buckets created after 2024-01-01), and
/// Volcengine / BytePlus TOS.
const VIRTUAL_HOSTED_STYLE_HOST_SUFFIXES: [&str; 4] = [
    "aliyuncs.com",
    "myqcloud.com",
    "volces.com",
    "bytepluses.com",
];

/// `S3Storage::PATH_STYLE_REJECTION_ERRORS`: parts of the errors these providers return for a
/// path-style request.
const PATH_STYLE_REJECTION_ERRORS: [&str; 5] = [
    "secondleveldomainforbidden",
    "pathstyledomainforbidden",
    "virtual hosted style",
    "virtual-hosted style",
    "virtual-styled",
];

/// `S3Storage::MINIO_CLIENT_UPLOAD_RETRY_DELAYS_MS`: waits before the second and the third attempt
/// of an upload with the MinIO client.
pub const MINIO_CLIENT_UPLOAD_RETRY_DELAYS: [Duration; 2] =
    [Duration::from_secs(10), Duration::from_secs(30)];

/// `S3Storage::REMOTE_COMMAND_TIMEOUT_EXIT_CODE`: the code of a remote command that timed out.
pub const REMOTE_COMMAND_TIMEOUT_EXIT_CODE: i32 = 124;

/// `S3Storage::detectPathStyleEndpoint`: the URL style for a new storage: virtual-hosted style for
/// providers that reject path-style URLs, else path-style.
pub fn detect_path_style_endpoint(endpoint: Option<&str>) -> bool {
    let host = crate::compose_parser::php::parse_url(endpoint.unwrap_or_default())
        .and_then(|u| u.host)
        .unwrap_or_default()
        .to_lowercase();
    !VIRTUAL_HOSTED_STYLE_HOST_SUFFIXES
        .iter()
        .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
}

/// `S3Storage::usesPathStyleEndpoint`: a row without the column (older schema) or a storage that
/// is not saved yet uses the default, path-style URLs.
pub fn uses_path_style_endpoint(row: &Record) -> bool {
    row.get("use_path_style_endpoint")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
}

/// `S3Storage::isPathStyleRejection`.
fn is_path_style_rejection(message: &str) -> bool {
    let lower = message.to_lowercase();
    PATH_STYLE_REJECTION_ERRORS
        .iter()
        .any(|e| lower.contains(e))
}

/// `S3Storage::minioClientAliasOptions`: the `mc alias set` options. `--api S3v4` signs as
/// `filesystem()` does; without it, mc probes the signature version on every run and a network
/// error during the probe makes it try S3v2, which most providers reject. `--path off` uses
/// virtual-hosted-style URLs; with path-style URLs mc keeps its own `auto` lookup.
pub fn minio_client_alias_options(path_style: bool) -> &'static str {
    if path_style {
        " --api S3v4"
    } else {
        " --api S3v4 --path off"
    }
}

/// `S3Storage::retryMinioClientUpload`: runs an upload with the MinIO client again when it fails,
/// for example after a short network outage, waiting 10 and then 30 seconds. A timeout is not
/// retried, because another full attempt can pass the timeout of the backup job.
pub async fn retry_minio_client_upload<T, F, Fut>(
    upload: F,
) -> Result<T, coolify_remote::RemoteError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, coolify_remote::RemoteError>>,
{
    retry_upload(&MINIO_CLIENT_UPLOAD_RETRY_DELAYS, upload).await
}

/// Laravel's `retry($delays, $callback, when: ...)` for [`retry_minio_client_upload`].
async fn retry_upload<T, F, Fut>(
    delays: &[Duration],
    mut upload: F,
) -> Result<T, coolify_remote::RemoteError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, coolify_remote::RemoteError>>,
{
    let mut delays = delays.iter();
    loop {
        match upload().await {
            Err(e) if e.code != Some(REMOTE_COMMAND_TIMEOUT_EXIT_CODE) => match delays.next() {
                Some(delay) => tokio::time::sleep(*delay).await,
                None => return Err(e),
            },
            other => return other,
        }
    }
}

/// A decrypted S3 storage.
#[derive(Clone)]
pub struct S3Target {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub key: String,
    pub secret: String,
    /// `usesPathStyleEndpoint()`: `endpoint/bucket` URLs, else `bucket.endpoint`.
    pub path_style: bool,
}

impl std::fmt::Debug for S3Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Target")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .finish_non_exhaustive()
    }
}

impl S3Target {
    pub fn from_row(enc: &Encrypter, row: &Record) -> Result<Self, String> {
        let text = |k: &str| parts::str_of(row, k).unwrap_or_default().to_owned();
        let secret = |k: &str| {
            enc.decrypt_string(parts::str_of(row, k).unwrap_or_default())
                .map_err(|_| "The MAC is invalid.".to_owned())
        };
        Ok(Self {
            endpoint: text("endpoint"),
            bucket: text("bucket"),
            region: text("region"),
            key: secret("key")?,
            secret: secret("secret")?,
            path_style: uses_path_style_endpoint(row),
        })
    }

    /// The host and the path of an object (`key`, already encoded; empty for the bucket): the
    /// bucket is the first path segment, or the first label of the host.
    fn locate(&self, authority: &str, encoded_key: &str) -> (String, String) {
        if self.path_style {
            let path = if encoded_key.is_empty() {
                format!("/{}/", uri_encode(&self.bucket))
            } else {
                format!("/{}/{encoded_key}", uri_encode(&self.bucket))
            };
            (authority.to_owned(), path)
        } else {
            (
                format!("{}.{authority}", self.bucket),
                format!("/{encoded_key}"),
            )
        }
    }

    fn client(&self) -> Result<reqwest::Client, String> {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())
    }

    /// One request signed with SigV4.
    async fn request(
        &self,
        method: reqwest::Method,
        key: &str,
        query: &[(&str, &str)],
    ) -> Result<(reqwest::StatusCode, String, Option<u64>), String> {
        let parsed = parse_url(&self.endpoint).ok_or("S3 endpoint is invalid.")?;
        let encoded_key = if key.is_empty() {
            String::new()
        } else {
            key.split('/').map(uri_encode).collect::<Vec<_>>().join("/")
        };
        let (authority, path) = self.locate(&parsed.authority, &encoded_key);
        let mut pairs: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (uri_encode(k), uri_encode(v)))
            .collect();
        pairs.sort();
        let canonical_query = pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let now = Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let payload = hex(&Sha256::digest(b""));
        let canonical_headers =
            format!("host:{authority}\nx-amz-content-sha256:{payload}\nx-amz-date:{amz_date}\n");
        let signed = "host;x-amz-content-sha256;x-amz-date";
        let canonical = format!(
            "{}\n{path}\n{canonical_query}\n{canonical_headers}\n{signed}\n{payload}",
            method.as_str()
        );
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex(&Sha256::digest(canonical.as_bytes()))
        );
        let k_date = hmac(format!("AWS4{}", self.secret).as_bytes(), date.as_bytes());
        let k_region = hmac(&k_date, self.region.as_bytes());
        let k_service = hmac(&k_region, b"s3");
        let k_signing = hmac(&k_service, b"aws4_request");
        let signature = hex(&hmac(&k_signing, to_sign.as_bytes()));
        let auth = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}",
            self.key
        );
        let url = format!(
            "{}://{authority}{path}{}",
            parsed.scheme,
            if canonical_query.is_empty() {
                String::new()
            } else {
                format!("?{canonical_query}")
            }
        );
        let resp = self
            .client()?
            .request(method, url)
            .header("x-amz-content-sha256", payload)
            .header("x-amz-date", amz_date)
            .header("authorization", auth)
            .send()
            .await
            .map_err(|e| {
                let mut message = e.to_string();
                if e.is_timeout() || e.is_connect() {
                    message.push_str(" connection refused");
                }
                message
            })?;
        let status = resp.status();
        let length = resp
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok());
        let body = resp.text().await.unwrap_or_default();
        Ok((status, body, length))
    }

    /// `$disk->temporaryUrl($key, ...)`: a presigned GET (SigV4 query signing).
    pub fn presigned_get(&self, key: &str, expires_seconds: u64) -> Result<String, String> {
        let parsed = parse_url(&self.endpoint).ok_or("S3 endpoint is invalid.")?;
        let encoded_key = key.split('/').map(uri_encode).collect::<Vec<_>>().join("/");
        let (authority, path) = self.locate(&parsed.authority, &encoded_key);
        let now = Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let query = format!(
            "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential={}&X-Amz-Date={amz_date}&X-Amz-Expires={expires_seconds}&X-Amz-SignedHeaders=host",
            uri_encode(&format!("{}/{scope}", self.key))
        );
        let canonical = format!("GET\n{path}\n{query}\nhost:{authority}\n\nhost\nUNSIGNED-PAYLOAD");
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex(&Sha256::digest(canonical.as_bytes()))
        );
        let k_date = hmac(format!("AWS4{}", self.secret).as_bytes(), date.as_bytes());
        let k_region = hmac(&k_date, self.region.as_bytes());
        let k_service = hmac(&k_region, b"s3");
        let k_signing = hmac(&k_service, b"aws4_request");
        let signature = hex(&hmac(&k_signing, to_sign.as_bytes()));
        Ok(format!(
            "{}://{authority}{path}?{query}&X-Amz-Signature={signature}",
            parsed.scheme
        ))
    }

    /// `$disk->temporaryUploadUrl($key, ..., ['ContentDisposition' => ...])`: a presigned PUT that
    /// signs the header the uploader must send.
    pub fn presigned_put(
        &self,
        key: &str,
        expires_seconds: u64,
        content_disposition: &str,
    ) -> Result<String, String> {
        let parsed = parse_url(&self.endpoint).ok_or("S3 endpoint is invalid.")?;
        let encoded_key = key.split('/').map(uri_encode).collect::<Vec<_>>().join("/");
        let (authority, path) = self.locate(&parsed.authority, &encoded_key);
        let now = Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let query = format!(
            "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential={}&X-Amz-Date={amz_date}&X-Amz-Expires={expires_seconds}&X-Amz-SignedHeaders=content-disposition%3Bhost",
            uri_encode(&format!("{}/{scope}", self.key))
        );
        let canonical = format!(
            "PUT\n{path}\n{query}\ncontent-disposition:{content_disposition}\nhost:{authority}\n\ncontent-disposition;host\nUNSIGNED-PAYLOAD"
        );
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex(&Sha256::digest(canonical.as_bytes()))
        );
        let k_date = hmac(format!("AWS4{}", self.secret).as_bytes(), date.as_bytes());
        let k_region = hmac(&k_date, self.region.as_bytes());
        let k_service = hmac(&k_region, b"s3");
        let k_signing = hmac(&k_service, b"aws4_request");
        let signature = hex(&hmac(&k_signing, to_sign.as_bytes()));
        Ok(format!(
            "{}://{authority}{path}?{query}&X-Amz-Signature={signature}",
            parsed.scheme
        ))
    }

    /// `$disk->files()`: `ListObjectsV2` on the bucket root.
    pub async fn list_root(&self) -> Result<(), String> {
        let (status, body, _) = self
            .request(
                reqwest::Method::GET,
                "",
                &[("list-type", "2"), ("prefix", ""), ("delimiter", "/")],
            )
            .await?;
        if status.is_success() {
            Ok(())
        } else {
            // Flysystem's `UnableToListContents` around the AWS SDK's exception message.
            let code = tag(&body, "Code").unwrap_or_default();
            let message = tag(&body, "Message").unwrap_or_default();
            let kind = if status.is_server_error() {
                "server"
            } else {
                "client"
            };
            let url = match parse_url(&self.endpoint) {
                Some(p) if !self.path_style => {
                    format!("{}://{}.{}/", p.scheme, self.bucket, p.authority)
                }
                _ => format!("{}/{}/", self.endpoint.trim_end_matches('/'), self.bucket),
            };
            Err(format!(
                "Unable to list contents for '', shallow listing Reason: Error executing \"ListObjectsV2\" on \"{url}?list-type=2&prefix=&delimiter=%2F\"; AWS HTTP error: {code} ({kind}): {message}"
            ))
        }
    }

    /// `$disk->exists($key)` and `$disk->size($key)`.
    pub async fn object_size(&self, key: &str) -> Result<Option<u64>, String> {
        let (status, _, length) = self.request(reqwest::Method::HEAD, key, &[]).await?;
        if status.is_success() {
            Ok(Some(length.unwrap_or(0)))
        } else if status.as_u16() == 404 {
            Ok(None)
        } else {
            Err(format!("S3 returned {}", status.as_u16()))
        }
    }

    /// `$disk->delete($filenames)`: the object keys have no leading slash.
    pub async fn delete_objects(&self, filenames: &[String]) -> Result<(), String> {
        for f in filenames {
            let (status, _, _) = self
                .request(reqwest::Method::DELETE, f.trim_start_matches('/'), &[])
                .await
                .map_err(|_| "One or more S3 backup files could not be deleted.".to_owned())?;
            if !status.is_success() {
                return Err("One or more S3 backup files could not be deleted.".to_owned());
            }
        }
        Ok(())
    }
}

fn tag(body: &str, name: &str) -> Option<String> {
    let start = body.find(&format!("<{name}>"))? + name.len() + 2;
    let end = body[start..].find(&format!("</{name}>"))? + start;
    Some(body[start..end].to_owned())
}

fn uri_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let Ok(mut m) = <Hmac<Sha256> as Mac>::new_from_slice(key) else {
        return Vec::new();
    };
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `toUserFriendlyConnectionException`.
fn friendly(message: String) -> String {
    let lower = message.to_lowercase();
    if [
        "timed out",
        "timeout",
        "connection refused",
        "could not resolve",
        "curl error 28",
    ]
    .iter()
    .any(|k| lower.contains(k))
    {
        "Could not connect to the S3 endpoint within 15 seconds. Please verify the endpoint, bucket, credentials, region, and network/firewall settings.".to_owned()
    } else {
        message
    }
}

impl Jobs {
    /// `S3Storage::testConnection(shouldSave: true)`: stores `is_usable`, and the URL style when
    /// the provider rejected path-style URLs. Returns the URL style to use (`usesPathStyleEndpoint`).
    pub async fn test_s3_connection(&self, row: &Record) -> Result<bool, String> {
        let id = parts::id_of(row);
        let (result, path_style) = self.check_s3_connection_style(row).await;
        let _ = parts::s3_set_usable(&self.0.db, id, result.is_ok(), Jobs::now()).await;
        if path_style != uses_path_style_endpoint(row) {
            let _ = parts::update_row(
                &self.0.db,
                "s3_storages",
                id,
                &[(
                    "use_path_style_endpoint",
                    coolify_db::servers::ColumnValue::Bool(path_style),
                )],
                Jobs::now(),
            )
            .await;
        }
        result.map(|()| path_style)
    }

    /// `S3Storage::testConnection()` on a model that is not saved.
    pub async fn check_s3_connection(&self, row: &Record) -> Result<(), String> {
        self.check_s3_connection_style(row).await.0
    }

    /// `S3Storage::testConnection()`, with the URL style the storage has afterwards: a storage
    /// that uses path-style URLs and is rejected for that is tried again with virtual-hosted-style
    /// URLs, which it keeps when they work (`listFilesWithAddressingStyleFallback`).
    pub async fn check_s3_connection_style(&self, row: &Record) -> (Result<(), String>, bool) {
        let mut path_style = uses_path_style_endpoint(row);
        let result = self.check_s3_connection_with(row, &mut path_style).await;
        (result, path_style)
    }

    async fn check_s3_connection_with(
        &self,
        row: &Record,
        path_style: &mut bool,
    ) -> Result<(), String> {
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let mut policy = UrlPolicy::load(&self.0.db).await;
        let endpoint = parts::str_of(row, "endpoint").unwrap_or_default();
        // `trustedInternalHosts()`: the bundled MinIO is not a user-supplied destination.
        if parts::str_of(row, "uuid") == Some("minio")
            && parse_url(endpoint).is_some_and(|u| u.host == "coolify-minio")
        {
            policy.allowed.push("coolify-minio".to_owned());
        }
        let bucket = parts::str_of(row, "bucket").unwrap_or_default();
        let mut result: Result<(), String> = Ok(());
        if endpoint.is_empty() {
            result = Err("S3 endpoint is not allowed: The endpoint field is required.".to_owned());
        } else if let Some(error) =
            // `route('settings.advanced')` outside a request: `config('app.url')`.
            endpoint_error(
                endpoint,
                "endpoint",
                &policy,
                &format!(
                    "{}/settings/advanced",
                    crate::notifications::Ctx::load(&self.0.db).await.app_url
                ),
            )
            .await
        {
            result = Err(format!("S3 endpoint is not allowed: {error}"));
        } else if !valid_bucket_name(bucket) {
            result = Err("S3 bucket name is not allowed: The bucket must be a valid S3 bucket name: 3-63 letters, numbers, dots, or hyphens; start and end with a letter or number; no consecutive dots, dot-hyphen pairs, or IP address format.".to_owned());
        }
        if result.is_ok() {
            result = match S3Target::from_row(enc, row) {
                Ok(mut target) => {
                    target.path_style = *path_style;
                    match target.list_root().await {
                        Err(e) if target.path_style && is_path_style_rejection(&e) => {
                            target.path_style = false;
                            match target.list_root().await {
                                Ok(()) => {
                                    *path_style = false;
                                    Ok(())
                                }
                                Err(_) => Err(e),
                            }
                        }
                        other => other,
                    }
                    .map_err(friendly)
                }
                Err(e) => Err(e),
            };
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_names() {
        assert!(valid_bucket_name("my-bucket.1"));
        assert!(!valid_bucket_name("ab"));
        assert!(!valid_bucket_name("a..b"));
        assert!(!valid_bucket_name("1.2.3.4"));
        assert!(!valid_bucket_name("-abc"));
    }

    #[test]
    fn ip_classes() {
        let p = UrlPolicy::default();
        assert!(!p.allowed_ip("10.1.2.3".parse().unwrap(), "x"));
        assert!(p.allowed_ip("8.8.8.8".parse().unwrap(), "x"));
        assert!(!p.allowed_ip("169.254.169.254".parse().unwrap(), "x"));
        let p = UrlPolicy {
            allowed: vec!["192.168.139.0/24".into()],
            allow_localhost: false,
            allow_private: false,
        };
        assert!(p.allowed_ip("192.168.139.220".parse().unwrap(), "x"));
    }

    #[test]
    fn urls() {
        let u = parse_url("http://minio:9000/x").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("minio", 9000));
        assert!(parse_url("nope").is_none());
    }

    #[test]
    fn path_style_detection() {
        for virtual_hosted in [
            "https://oss-cn-hangzhou.aliyuncs.com",
            "https://aliyuncs.com",
            "https://cos.ap-guangzhou.myqcloud.com",
            "https://tos-cn-beijing.volces.com",
            "https://tos-ap-southeast-1.bytepluses.com:443",
            "HTTPS://OSS-CN-HANGZHOU.ALIYUNCS.COM",
        ] {
            assert!(
                !detect_path_style_endpoint(Some(virtual_hosted)),
                "{virtual_hosted}"
            );
        }
        for path_style in [
            "https://s3.us-east-1.amazonaws.com",
            "http://minio:9000",
            "https://notaliyuncs.com",
            "https://aliyuncs.com.example.com",
            "oss-cn-hangzhou.aliyuncs.com",
            "",
        ] {
            assert!(detect_path_style_endpoint(Some(path_style)), "{path_style}");
        }
        assert!(detect_path_style_endpoint(None));
        let mut row = Record::new();
        assert!(uses_path_style_endpoint(&row));
        row.insert("use_path_style_endpoint".into(), serde_json::Value::Null);
        assert!(uses_path_style_endpoint(&row));
        row.insert("use_path_style_endpoint".into(), serde_json::json!(false));
        assert!(!uses_path_style_endpoint(&row));
    }

    #[test]
    fn path_style_rejections() {
        assert!(is_path_style_rejection(
            "AWS HTTP error: SecondLevelDomainForbidden (client): Please use virtual hosted style to access."
        ));
        assert!(is_path_style_rejection("PathStyleDomainForbidden"));
        assert!(is_path_style_rejection(
            "the bucket must be addressed using virtual-styled requests"
        ));
        assert!(!is_path_style_rejection(
            "AccessDenied (client): Access Denied"
        ));
        assert_eq!(minio_client_alias_options(true), " --api S3v4");
        assert_eq!(minio_client_alias_options(false), " --api S3v4 --path off");
    }

    #[test]
    fn virtual_hosted_urls() {
        let mut t = S3Target {
            endpoint: "https://oss-cn-hangzhou.aliyuncs.com".into(),
            bucket: "backups".into(),
            region: "cn-hangzhou".into(),
            key: "k".into(),
            secret: "s".into(),
            path_style: true,
        };
        let authority = "oss-cn-hangzhou.aliyuncs.com";
        assert_eq!(
            t.locate(authority, "a/b.gz"),
            (authority.to_owned(), "/backups/a/b.gz".to_owned())
        );
        assert_eq!(
            t.locate(authority, ""),
            (authority.to_owned(), "/backups/".to_owned())
        );
        t.path_style = false;
        assert_eq!(
            t.locate(authority, "a/b.gz"),
            (
                "backups.oss-cn-hangzhou.aliyuncs.com".to_owned(),
                "/a/b.gz".to_owned()
            )
        );
        assert_eq!(
            t.locate(authority, ""),
            (
                "backups.oss-cn-hangzhou.aliyuncs.com".to_owned(),
                "/".to_owned()
            )
        );
        let url = t.presigned_get("a/b.gz", 60).unwrap();
        assert!(
            url.starts_with("https://backups.oss-cn-hangzhou.aliyuncs.com/a/b.gz?X-Amz-Algorithm="),
            "{url}"
        );
    }

    #[tokio::test]
    async fn uploads_are_not_retried_after_a_timeout() {
        let mut attempts = 0;
        let result: Result<(), coolify_remote::RemoteError> = retry_minio_client_upload(|| {
            attempts += 1;
            async {
                Err(coolify_remote::RemoteError {
                    message: "The command on the server timed out after 3600 seconds.".into(),
                    code: Some(REMOTE_COMMAND_TIMEOUT_EXIT_CODE),
                })
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(attempts, 1);
        let mut attempts = 0;
        let result = retry_minio_client_upload(|| {
            attempts += 1;
            async { Ok::<_, coolify_remote::RemoteError>(Some("done".to_owned())) }
        })
        .await;
        assert_eq!(result.unwrap().as_deref(), Some("done"));
        assert_eq!(attempts, 1);
    }

    #[tokio::test]
    async fn failed_uploads_are_tried_three_times() {
        assert_eq!(
            MINIO_CLIENT_UPLOAD_RETRY_DELAYS,
            [Duration::from_secs(10), Duration::from_secs(30)]
        );
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let delays = [Duration::from_millis(1), Duration::from_millis(1)];
        let result: Result<(), coolify_remote::RemoteError> = retry_upload(&delays, || {
            attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async {
                Err(coolify_remote::RemoteError {
                    message: "mc: Unable to upload".into(),
                    code: Some(1),
                })
            }
        })
        .await;
        assert_eq!(result.unwrap_err().message, "mc: Unable to upload");
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
    }
}
