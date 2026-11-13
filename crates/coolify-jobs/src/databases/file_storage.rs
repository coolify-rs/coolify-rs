//! SFTP, FTPS and WebDAV backup storages as Coolify itself reads them (Coolify v4.7.0):
//! `S3Storage::filesystem()` for the types that are not S3, with upstream's
//! `SftpFilesystemAdapter`, `FtpsFilesystemAdapter` and `WebdavFilesystemAdapter`, the connection
//! test of `S3Storage::testConnection()`, and `deleteBackupsFromFileStorage()`.
//!
//! Upstream uses phpseclib, the curl extension and Guzzle. Here SFTP goes through the system `ssh`
//! (`coolify_remote::sftp`), FTPS through the system `curl` with the same options, and WebDAV
//! through reqwest. SFTP and FTPS connect to the validated IP of the host, and WebDAV pins it, so a
//! DNS change cannot send them to another address.

use std::net::{IpAddr, SocketAddr};
use std::process::Stdio;
use std::time::Duration;

use coolify_db::database_parts as parts;
use coolify_db::records::Record;
use coolify_remote::sftp::{SftpError, SftpSession, SftpTarget};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::connection::rawurlencode;
use super::s3::{self, UrlPolicy};
use super::storage_type::{
    self as st, BackupStorageType, has_control_characters, require_sftp_host_key,
};
use crate::Jobs;

/// `S3Storage::CONNECTION_TIMEOUT_SECONDS`.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(15);
/// `FtpsFilesystemAdapter::REQUEST_TIMEOUT_SECONDS`: the limit of a request that transfers no file.
const FTPS_REQUEST_TIMEOUT_SECONDS: u64 = 30;
/// `STALL_SECONDS`: a file transfer fails when it moves no data for this long.
const STALL_SECONDS: u64 = 60;
/// `WebdavFilesystemAdapter::METADATA_TIMEOUT_SECONDS`.
const WEBDAV_METADATA_TIMEOUT: Duration = Duration::from_secs(15);
/// `WebdavFilesystemAdapter::PROPFIND_BODY`.
const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/><d:getcontenttype/></d:prop></d:propfind>"#;

/// `FtpsRequestFailed::NOT_FOUND_ERRORS`: `CURLE_REMOTE_ACCESS_DENIED` (the server refused to change
/// to the directory), `CURLE_FTP_COULDNT_RETR_FILE`, and `CURLE_REMOTE_FILE_NOT_FOUND`.
const FTPS_NOT_FOUND_ERRORS: [i32; 3] = [9, 19, 78];

/// A decrypted SFTP, FTPS or WebDAV storage.
#[derive(Clone)]
pub struct FileStorage {
    pub kind: BackupStorageType,
    pub endpoint: String,
    /// The host of the endpoint, without the brackets of an IPv6 address.
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    /// `sftpPrivateKey()?->private_key`.
    pub private_key: Option<String>,
    pub sftp_host_key: Option<String>,
    /// `basePath()`; empty for WebDAV, whose base is the endpoint URL.
    pub base_path: String,
    /// `pinnedHost()`: the validated IP of the host, or the host when it is an IP.
    pub address: String,
    /// The validated IPs of a WebDAV host (none for an IP host).
    pub ips: Vec<IpAddr>,
}

impl std::fmt::Debug for FileStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStorage")
            .field("kind", &self.kind)
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

/// A failed curl request of the FTPS adapter (`FtpsRequestFailed`). The code is the curl error.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FtpsRequestFailed {
    code: i32,
    message: String,
}

impl FtpsRequestFailed {
    fn is_not_found(&self) -> bool {
        FTPS_NOT_FOUND_ERRORS.contains(&self.code)
    }
}

impl std::fmt::Display for FtpsRequestFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FTPS request failed: {}", self.message)
    }
}

/// What a storage holds at a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub is_dir: bool,
    pub size: Option<u64>,
}

fn bracketed(host: &str) -> String {
    let host = host.trim_matches(|c| c == '[' || c == ']');
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

/// Flysystem's `WhitespacePathNormalizer` for the paths Coolify uses: no leading slash.
fn normalize(path: &str) -> &str {
    path.trim_matches('/')
}

/// PHP `dirname()` of a relative path without a trailing slash.
fn dirname(path: &str) -> &str {
    path.rsplit_once('/').map_or(".", |(dir, _)| dir)
}

/// `PathPrefixer::prefixPath()` with the base path of an SFTP storage.
fn sftp_path(base: &str, path: &str) -> String {
    let path = path.trim_start_matches('/');
    if base.is_empty() {
        path.to_owned()
    } else {
        format!("{}/{path}", base.trim_end_matches('/'))
    }
}

/// `FtpsFilesystemAdapter::fullPath()`. Control characters are refused, as a line break would end
/// the FTP command and start another one.
fn ftps_full_path(root: &str, path: &str) -> Result<String, String> {
    if has_control_characters(path) || has_control_characters(root) {
        return Err("The path contains control characters.".to_owned());
    }
    let path = path.trim_matches('/');
    let trimmed_root = root.trim_end_matches('/');
    if path.is_empty() {
        return Ok(if trimmed_root.is_empty() && root.starts_with('/') {
            "/".to_owned()
        } else {
            trimmed_root.to_owned()
        });
    }
    Ok(if trimmed_root.is_empty() {
        path.to_owned()
    } else {
        format!("{trimmed_root}/{path}")
    })
}

/// `FtpsFilesystemAdapter::url()`, with the login in the URL: the curl tool splits `--user` at the
/// first colon, so a username with a colon would fail (upstream sets `CURLOPT_USERNAME`).
fn ftps_url(storage: &FileStorage, path: &str, directory: bool) -> Result<String, String> {
    let full_path = ftps_full_path(&storage.base_path, path)?;
    // curl reads a URL path as relative to the login directory; %2F at the start makes it absolute.
    let encoded = full_path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(rawurlencode)
        .collect::<Vec<_>>()
        .join("/");
    let url_path = if full_path.starts_with('/') {
        format!("%2F{encoded}")
    } else {
        encoded
    };
    Ok(format!(
        "ftp://{}:{}@{}:{}/{url_path}{}",
        rawurlencode(&storage.user),
        rawurlencode(&storage.password),
        bracketed(&storage.host),
        storage.port,
        if directory && !url_path.is_empty() {
            "/"
        } else {
            ""
        }
    ))
}

/// The curl config of one request of `FtpsFilesystemAdapter::request()`. curl reads it from its
/// standard input, so the password is not an argument of the process.
fn ftps_config(storage: &FileStorage, url: &str, options: &[String], transfer: bool) -> String {
    let mut lines = vec![
        format!("url = {}", st::curl_config_value(url)),
        "ssl-reqd".to_owned(),
        "proto = \"=ftp\"".to_owned(),
        format!("connect-timeout = {}", CONNECTION_TIMEOUT.as_secs()),
        "ftp-skip-pasv-ip".to_owned(),
        "silent".to_owned(),
        "show-error".to_owned(),
    ];
    if transfer {
        lines.push("speed-limit = 1".to_owned());
        lines.push(format!("speed-time = {STALL_SECONDS}"));
    } else {
        lines.push(format!("max-time = {FTPS_REQUEST_TIMEOUT_SECONDS}"));
    }
    lines.push(format!(
        "resolve = {}",
        st::curl_config_value(&format!(
            "{}:{}:{}",
            bracketed(&storage.host),
            storage.port,
            bracketed(&storage.address)
        ))
    ));
    lines.extend(options.iter().cloned());
    lines.join("\n") + "\n"
}

/// `curl_error()` from what the curl tool prints: `curl: (67) Access denied: 530`.
fn curl_error_message(stderr: &str) -> String {
    let line = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim();
    match line.strip_prefix("curl: (") {
        Some(rest) => rest
            .split_once(") ")
            .map_or(rest, |(_, message)| message)
            .to_owned(),
        None => line.to_owned(),
    }
}

/// The size in what `curl --head` prints for an FTP file (`CURLINFO_CONTENT_LENGTH_DOWNLOAD_T`).
fn ftps_head_size(output: &str) -> Option<u64> {
    output.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse().ok())
            .flatten()
    })
}

/// The entries of a PROPFIND answer (`WebdavFilesystemAdapter::propfind()`), as paths relative to
/// the base path. `None` when the body is not a multistatus document.
fn parse_propfind(body: &str, base_path: &str) -> Option<Vec<(String, Entry)>> {
    static TAG: std::sync::LazyLock<Option<regex::Regex>> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?s)<(/?)(?:[A-Za-z_][\w.-]*:)?([A-Za-z_][\w.-]*)(?:\s[^>]*)?/?>").ok()
    });
    let tag = TAG.as_ref()?;
    if !tag
        .captures_iter(body)
        .any(|c| c[1].is_empty() && c[2].eq_ignore_ascii_case("multistatus"))
    {
        return None;
    }
    // The text of the first `name` element in a block, and the blocks of an element.
    let text_of = |block: &str, name: &str| -> Option<String> {
        let mut start = None;
        for c in tag.captures_iter(block) {
            let whole = c.get(0)?;
            if !c[2].eq_ignore_ascii_case(name) {
                continue;
            }
            if c[1].is_empty() {
                if whole.as_str().ends_with("/>") {
                    return Some(String::new());
                }
                start = Some(whole.end());
            } else if let Some(s) = start {
                return Some(xml_text(&block[s..whole.start()]));
            }
        }
        None
    };
    let blocks_of = |block: &str, name: &str| -> Vec<String> {
        let mut out = Vec::new();
        let mut start = None;
        for c in tag.captures_iter(block) {
            let Some(whole) = c.get(0) else { continue };
            if !c[2].eq_ignore_ascii_case(name) {
                continue;
            }
            if c[1].is_empty() {
                start = Some(whole.end());
            } else if let Some(s) = start.take() {
                out.push(block[s..whole.start()].to_owned());
            }
        }
        out
    };
    let base = st::rawurldecode(base_path);
    let mut entries = Vec::new();
    for response in blocks_of(body, "response") {
        let href = text_of(&response, "href").unwrap_or_default();
        let href_path = crate::compose_parser::php::parse_url(href.trim())
            .and_then(|u| u.path)
            .unwrap_or_default();
        let href_path = st::rawurldecode(&href_path);
        let relative = href_path
            .strip_prefix(base.as_str())
            .unwrap_or_else(|| href_path.trim_start_matches('/'))
            .trim_matches('/')
            .to_owned();
        // `d:propstat[contains(d:status, " 200 ")]`.
        let found: Vec<String> = blocks_of(&response, "propstat")
            .into_iter()
            .filter(|p| text_of(p, "status").is_some_and(|s| s.contains(" 200 ")))
            .collect();
        let is_dir = found.iter().any(|p| {
            tag.captures_iter(p)
                .any(|c| c[1].is_empty() && c[2].eq_ignore_ascii_case("collection"))
        });
        let size = found
            .iter()
            .find_map(|p| text_of(p, "getcontentlength"))
            .and_then(|s| s.trim().parse::<u64>().ok());
        entries.push((
            relative,
            Entry {
                is_dir,
                size: if is_dir { None } else { size },
            },
        ));
    }
    Some(entries)
}

/// The text of an XML element: entities and CDATA sections.
fn xml_text(raw: &str) -> String {
    let raw = raw.trim();
    if let Some(inner) = raw
        .strip_prefix("<![CDATA[")
        .and_then(|r| r.strip_suffix("]]>"))
    {
        return inner.to_owned();
    }
    raw.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// `WebdavFilesystemAdapter::uri()`.
fn webdav_uri(path: &str, directory: bool) -> Result<String, String> {
    if has_control_characters(path) {
        return Err("The path contains control characters.".to_owned());
    }
    let encoded = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(rawurlencode)
        .collect::<Vec<_>>()
        .join("/");
    Ok(if encoded.is_empty() {
        String::new()
    } else if directory {
        format!("{encoded}/")
    } else {
        encoded
    })
}

fn reqwest_message(e: &reqwest::Error) -> String {
    let mut message = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(inner) = source {
        message.push_str(": ");
        message.push_str(&inner.to_string());
        source = inner.source();
    }
    if e.is_timeout() {
        message.push_str(" (timed out)");
    }
    message
}

/// `S3Storage::toUserFriendlyConnectionException()` for the types that are not S3.
pub fn friendly_connection_error(kind: BackupStorageType, message: String) -> String {
    // The network error of an SFTP connection is shown as it is.
    if message.starts_with("Could not connect to the ") {
        return message;
    }
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
        format!(
            "Could not connect to the {} server within 15 seconds. Please verify the host, port, credentials, and network/firewall settings.",
            kind.label()
        )
    } else {
        message
    }
}

/// `S3Storage::connectionRules()` as `testConnection()` runs them on a saved storage of a type that
/// is not S3: the first failure, with Laravel's messages.
pub async fn connection_settings_error(
    kind: BackupStorageType,
    endpoint: &str,
    user: &str,
    password: &str,
    has_private_key: bool,
    policy: &UrlPolicy,
    settings_url: &str,
) -> Option<String> {
    if endpoint.trim().is_empty() {
        return Some("The endpoint field is required.".to_owned());
    }
    if endpoint.chars().count() > 255 {
        return Some("The endpoint field must not be greater than 255 characters.".to_owned());
    }
    if let Some(message) = endpoint_path_error(endpoint, "endpoint") {
        return Some(message);
    }
    if let Some(message) = s3::endpoint_error_for(
        endpoint,
        "endpoint",
        policy,
        settings_url,
        kind.endpoint_schemes(),
    )
    .await
    {
        return Some(message);
    }
    if user.is_empty() {
        return Some("The key field is required.".to_owned());
    }
    if user.chars().count() > 255 {
        return Some("The key field must not be greater than 255 characters.".to_owned());
    }
    if !valid_user(user) {
        return Some("The key field format is invalid.".to_owned());
    }
    if password.is_empty() {
        if kind == BackupStorageType::Sftp {
            if !has_private_key {
                return Some(
                    "The secret field is required when private key id is not present.".to_owned(),
                );
            }
        } else {
            return Some("The secret field is required.".to_owned());
        }
    } else if password.chars().count() > 255 {
        return Some("The secret field must not be greater than 255 characters.".to_owned());
    }
    None
}

/// The `key` rule of `connectionRules()`: rclone reads the user from a config file line, so it must
/// not contain line breaks or quotes (`regex:/^[^\x00-\x1f\x7f"]+$/`).
pub fn valid_user(user: &str) -> bool {
    !user.is_empty() && !has_control_characters(user) && !user.contains('"')
}

/// The control character check of the closure in `connectionRules()`: the message for the path of
/// an endpoint, `:attribute` replaced.
pub fn endpoint_path_error(endpoint: &str, attribute: &str) -> Option<String> {
    let path = crate::compose_parser::php::parse_url(endpoint.trim())
        .and_then(|u| u.path)
        .unwrap_or_default();
    has_control_characters(&st::rawurldecode(&path))
        .then(|| format!("The {attribute} path must not contain control characters."))
}

/// The credentials check of the closure in `connectionRules()`: team members and read-only API
/// tokens see the endpoint, so it must not hold credentials. `saved_endpoint` is the endpoint that
/// is saved now: it may keep a username or password from before this check.
pub fn endpoint_credentials_error(
    endpoint: &str,
    saved_endpoint: Option<&str>,
    attribute: &str,
) -> Option<String> {
    let parts = crate::compose_parser::php::parse_url(endpoint.trim()).unwrap_or_default();
    ((parts.user.is_some() || parts.pass.is_some())
        && endpoint.trim() != saved_endpoint.unwrap_or_default().trim())
    .then(|| {
        format!(
            "The {attribute} must not contain a username or password. Enter the credentials in their own fields."
        )
    })
}

/// An open storage: `$storage->filesystem()`. The SFTP connection is opened by the first request
/// and kept.
pub struct Disk {
    pub storage: FileStorage,
    sftp: Option<SftpSession>,
    http: Option<reqwest::Client>,
}

impl std::fmt::Debug for Disk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Disk")
            .field("storage", &self.storage)
            .finish_non_exhaustive()
    }
}

/// A download through Coolify (`StreamsDownloads::download()`).
pub enum Download {
    Sftp {
        session: Box<SftpSession>,
        handle: Vec<u8>,
        offset: u64,
    },
    Http(Box<reqwest::Response>),
    Curl {
        child: tokio::process::Child,
        stdout: tokio::process::ChildStdout,
    },
}

impl std::fmt::Debug for Download {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Download")
    }
}

impl Download {
    /// The next part of the file, or `None` at its end.
    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        match self {
            Self::Sftp {
                session,
                handle,
                offset,
            } => {
                let data = session
                    .read(handle, *offset, coolify_remote::sftp::READ_CHUNK)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(data.filter(|d| !d.is_empty()).inspect(|d| {
                    *offset += d.len() as u64;
                }))
            }
            Self::Http(response) => {
                let next =
                    tokio::time::timeout(Duration::from_secs(STALL_SECONDS), response.chunk())
                        .await
                        .map_err(|_| "The WebDAV server sent no data.".to_owned())?;
                next.map(|b| b.map(|b| b.to_vec()))
                    .map_err(|e| reqwest_message(&e))
            }
            Self::Curl { child, stdout } => {
                let mut buf = vec![0u8; 64 * 1024];
                let n = stdout.read(&mut buf).await.map_err(|e| e.to_string())?;
                if n > 0 {
                    buf.truncate(n);
                    return Ok(Some(buf));
                }
                let status = child.wait().await.map_err(|e| e.to_string())?;
                if status.success() {
                    Ok(None)
                } else {
                    Err("FTPS request failed: the transfer did not finish.".to_owned())
                }
            }
        }
    }
}

impl FileStorage {
    fn sftp_target(&self, pinned: Option<String>) -> SftpTarget {
        SftpTarget {
            host: self.host.clone(),
            address: self.address.clone(),
            port: self.port,
            user: self.user.clone(),
            // `password: $this->sftpPrivateKey() ? null : (string) $this->secret`.
            password: self
                .private_key
                .is_none()
                .then(|| self.password.clone().into()),
            private_key: self.private_key.clone().map(Into::into),
            pinned_host_key: pinned,
            timeout: CONNECTION_TIMEOUT,
        }
    }

    /// `$storage->filesystem()`. An SFTP storage refuses to connect before `testConnection()`
    /// saved its host key.
    pub fn disk(&self) -> Result<Disk, String> {
        if self.kind == BackupStorageType::Sftp {
            require_sftp_host_key(self.sftp_host_key.as_deref())?;
        }
        Ok(Disk {
            storage: self.clone(),
            sftp: None,
            http: None,
        })
    }

    /// `S3Storage::sftpFilesystem()`: without a saved host key, it accepts any key. Only the
    /// connection test uses it so, to save the presented key.
    fn disk_accepting_new_host_key(&self) -> Disk {
        Disk {
            storage: self.clone(),
            sftp: None,
            http: None,
        }
    }
}

impl Disk {
    async fn sftp(&mut self, jobs: &Jobs) -> Result<&mut SftpSession, String> {
        if self.sftp.is_none() {
            let target = self.storage.sftp_target(
                self.storage
                    .sftp_host_key
                    .clone()
                    .filter(|k| !k.trim().is_empty()),
            );
            let session = jobs
                .remote()
                .sftp_connect(&target)
                .await
                .map_err(|e| match e {
                    // Flysystem shows only "Unable to connect to host". The network error is in ssh.
                    SftpError::Connect(message) => {
                        format!("Could not connect to the SFTP server: {message}")
                    }
                    other => other.to_string(),
                })?;
            self.sftp = Some(session);
        }
        self.sftp
            .as_mut()
            .ok_or_else(|| "Unable to connect to host".to_owned())
    }

    fn http(&mut self) -> Result<reqwest::Client, String> {
        if let Some(client) = &self.http {
            return Ok(client.clone());
        }
        let mut builder = reqwest::Client::builder()
            .connect_timeout(CONNECTION_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none());
        if !self.storage.ips.is_empty() {
            let addrs: Vec<SocketAddr> = self
                .storage
                .ips
                .iter()
                .map(|ip| SocketAddr::new(*ip, self.storage.port))
                .collect();
            builder = builder.resolve_to_addrs(&self.storage.host, &addrs);
        }
        let client = builder.build().map_err(|e| e.to_string())?;
        self.http = Some(client.clone());
        Ok(client)
    }

    fn webdav_url(&self, path: &str, directory: bool) -> Result<String, String> {
        Ok(format!(
            "{}/{}",
            self.storage.endpoint.trim_end_matches('/'),
            webdav_uri(path, directory)?
        ))
    }

    async fn webdav(
        &mut self,
        method: &str,
        path: &str,
        directory: bool,
        depth: Option<u8>,
    ) -> Result<reqwest::Response, String> {
        let url = self.webdav_url(path, directory)?;
        let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
        let mut request = self
            .http()?
            .request(method, url)
            .basic_auth(&self.storage.user, Some(&self.storage.password));
        if let Some(depth) = depth {
            request = request
                .timeout(WEBDAV_METADATA_TIMEOUT)
                .header("Depth", depth.to_string())
                .header("Content-Type", "application/xml; charset=utf-8")
                .body(PROPFIND_BODY);
        }
        request.send().await.map_err(|e| reqwest_message(&e))
    }

    /// `WebdavFilesystemAdapter::expectSuccess()`.
    fn expect_success(response: &reqwest::Response) -> Result<(), String> {
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(format!(
                "The WebDAV server answered with HTTP {} {}.",
                status.as_u16(),
                status.canonical_reason().unwrap_or_default()
            ))
        }
    }

    async fn propfind(
        &mut self,
        path: &str,
        depth: u8,
        directory: bool,
    ) -> Result<Vec<(String, Entry)>, String> {
        let response = self
            .webdav("PROPFIND", path, directory, Some(depth))
            .await?;
        if response.status().as_u16() == 404 {
            return Ok(Vec::new());
        }
        Self::expect_success(&response)?;
        let body = response.text().await.map_err(|e| reqwest_message(&e))?;
        let base = crate::compose_parser::php::parse_url(&self.storage.endpoint)
            .and_then(|u| u.path)
            .unwrap_or_default();
        let base = format!("{}/", base.trim_end_matches('/'));
        parse_propfind(&body, &base)
            .ok_or_else(|| "The WebDAV server returned an invalid PROPFIND response.".to_owned())
    }

    /// One request of the FTPS adapter: the output of curl.
    async fn ftps(
        &self,
        path: &str,
        directory: bool,
        options: &[String],
    ) -> Result<String, FtpsRequestFailed> {
        let fail = |message: String| FtpsRequestFailed { code: -1, message };
        let url = ftps_url(&self.storage, path, directory).map_err(fail)?;
        let config = ftps_config(&self.storage, &url, options, false);
        let mut child = curl_command()
            .spawn()
            .map_err(|e| fail(format!("Failed to start curl: {e}")))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(config.as_bytes()).await;
        }
        let output = tokio::time::timeout(
            Duration::from_secs(FTPS_REQUEST_TIMEOUT_SECONDS + 10),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| FtpsRequestFailed {
            code: 28,
            message: "Operation timed out".to_owned(),
        })?
        .map_err(|e| fail(format!("Failed to run curl: {e}")))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(FtpsRequestFailed {
                code: output.status.code().unwrap_or(-1),
                message: curl_error_message(&String::from_utf8_lossy(&output.stderr)),
            })
        }
    }

    /// `$disk->files()`: the names in the base directory. The connection test only needs it to
    /// work.
    pub async fn list_root(&mut self, jobs: &Jobs) -> Result<(), String> {
        let wrap = |reason: String| {
            format!("Unable to list contents for '', shallow listing Reason: {reason}")
        };
        match self.storage.kind {
            BackupStorageType::Sftp => {
                let base = self.storage.base_path.clone();
                let session = self.sftp(jobs).await?;
                // Flysystem's SFTP adapter lists nothing for a directory that it cannot read.
                match session.list(&format!("{base}/")).await {
                    Ok(_) | Err(SftpError::Status { .. }) => Ok(()),
                    Err(e) => Err(e.to_string()),
                }
            }
            BackupStorageType::Ftps => self
                .ftps("", true, &["list-only".to_owned()])
                .await
                .map(|_| ())
                .map_err(|e| wrap(e.to_string())),
            BackupStorageType::Webdav => {
                let entries = self.propfind("", 1, true).await.map_err(wrap)?;
                if entries.is_empty() {
                    return Err(wrap(
                        "The directory does not exist on the WebDAV server.".to_owned(),
                    ));
                }
                Ok(())
            }
            BackupStorageType::S3 => Err("S3 storages use the S3 API.".to_owned()),
        }
    }

    /// `$disk->exists($path)` and `$disk->size($path)`: what is at the path, or `None`.
    pub async fn entry(&mut self, jobs: &Jobs, path: &str) -> Result<Option<Entry>, String> {
        let path = normalize(path);
        match self.storage.kind {
            BackupStorageType::Sftp => {
                let full = sftp_path(&self.storage.base_path, path);
                let attrs = self
                    .sftp(jobs)
                    .await?
                    .stat(&full)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(attrs.map(|a| Entry {
                    is_dir: a.is_dir(),
                    size: if a.is_dir() { None } else { a.size },
                }))
            }
            BackupStorageType::Ftps => {
                match self.ftps(path, false, &["head".to_owned()]).await {
                    Ok(output) => Ok(Some(Entry {
                        is_dir: false,
                        size: ftps_head_size(&output),
                    })),
                    Err(e) if e.is_not_found() => {
                        // `directoryExists()`.
                        match self.ftps(path, true, &["head".to_owned()]).await {
                            Ok(_) => Ok(Some(Entry {
                                is_dir: true,
                                size: None,
                            })),
                            Err(e) if e.is_not_found() => Ok(None),
                            Err(e) => Err(format!("Unable to check existence for: {path}: {e}")),
                        }
                    }
                    Err(e) => Err(format!("Unable to check existence for: {path}: {e}")),
                }
            }
            BackupStorageType::Webdav => Ok(self
                .propfind(path, 0, false)
                .await
                .map_err(|e| format!("Unable to check existence for: {path}: {e}"))?
                .into_iter()
                .next()
                .map(|(_, entry)| entry)),
            BackupStorageType::S3 => Err("S3 storages use the S3 API.".to_owned()),
        }
    }

    /// `$disk->delete($path)`.
    pub async fn delete(&mut self, jobs: &Jobs, path: &str) -> Result<(), String> {
        let path = normalize(path);
        match self.storage.kind {
            BackupStorageType::Sftp => {
                let full = sftp_path(&self.storage.base_path, path);
                match self.sftp(jobs).await?.remove(&full).await {
                    Ok(()) => Ok(()),
                    Err(e) if e.is_not_found() => Ok(()),
                    Err(e) => Err(e.to_string()),
                }
            }
            BackupStorageType::Ftps => {
                let full = ftps_full_path(&self.storage.base_path, path)?;
                self.ftps(
                    "",
                    true,
                    &[
                        "head".to_owned(),
                        format!("quote = {}", st::curl_config_value(&format!("DELE {full}"))),
                    ],
                )
                .await
                .map(|_| ())
                .map_err(|e| format!("Unable to delete file located at: {path}. {e}"))
            }
            BackupStorageType::Webdav => {
                let response = self.webdav("DELETE", path, false, None).await;
                let response = response
                    .map_err(|e| format!("Unable to delete file located at: {path}. {e}"))?;
                if response.status().as_u16() == 404 {
                    return Ok(());
                }
                Self::expect_success(&response)
                    .map_err(|e| format!("Unable to delete file located at: {path}. {e}"))
            }
            BackupStorageType::S3 => Err("S3 storages use the S3 API.".to_owned()),
        }
    }

    /// Whether the adapter implements `DeletesEmptyDirectories` (SFTP and FTPS).
    pub fn deletes_empty_directories(&self) -> bool {
        matches!(
            self.storage.kind,
            BackupStorageType::Sftp | BackupStorageType::Ftps
        )
    }

    /// `DeletesEmptyDirectories::deleteEmptyDirectory()`: deletes a directory only when it is
    /// empty, in one request, so a file that another upload writes at the same time is never
    /// deleted. False when the directory is not empty or could not be deleted.
    pub async fn delete_empty_directory(
        &mut self,
        jobs: &Jobs,
        path: &str,
    ) -> Result<bool, String> {
        let path = normalize(path);
        match self.storage.kind {
            BackupStorageType::Sftp => {
                let full = sftp_path(&self.storage.base_path, path);
                self.sftp(jobs)
                    .await?
                    .rmdir(&full)
                    .await
                    .map_err(|e| e.to_string())
            }
            BackupStorageType::Ftps => {
                let full = ftps_full_path(&self.storage.base_path, path)?;
                Ok(self
                    .ftps(
                        "",
                        true,
                        &[
                            "head".to_owned(),
                            format!("quote = {}", st::curl_config_value(&format!("RMD {full}"))),
                        ],
                    )
                    .await
                    .is_ok())
            }
            _ => Ok(false),
        }
    }

    /// `StreamsDownloads::download()`: the file as a stream, so a download through Coolify buffers
    /// nothing. The caller sends at most the size that the storage reported.
    pub async fn download(mut self, jobs: &Jobs, path: &str) -> Result<Download, String> {
        let path = normalize(path).to_owned();
        let unreadable =
            |reason: String| format!("Unable to read file from location: {path}. {reason}");
        match self.storage.kind {
            BackupStorageType::Sftp => {
                let full = sftp_path(&self.storage.base_path, &path);
                self.sftp(jobs).await.map_err(unreadable)?;
                let Some(mut session) = self.sftp.take() else {
                    return Err(unreadable("Unable to connect to host".to_owned()));
                };
                let handle = session
                    .open(&full)
                    .await
                    .map_err(|e| unreadable(e.to_string()))?;
                Ok(Download::Sftp {
                    session: Box::new(session),
                    handle,
                    offset: 0,
                })
            }
            BackupStorageType::Ftps => {
                let url = ftps_url(&self.storage, &path, false).map_err(unreadable)?;
                let config = ftps_config(&self.storage, &url, &[], true);
                let mut child = curl_command()
                    .spawn()
                    .map_err(|e| unreadable(format!("Failed to start curl: {e}")))?;
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(config.as_bytes()).await;
                }
                let Some(stdout) = child.stdout.take() else {
                    return Err(unreadable("Failed to start curl: missing pipes".to_owned()));
                };
                Ok(Download::Curl { child, stdout })
            }
            BackupStorageType::Webdav => {
                let response = self
                    .webdav("GET", &path, false, None)
                    .await
                    .map_err(unreadable)?;
                Self::expect_success(&response).map_err(unreadable)?;
                Ok(Download::Http(Box::new(response)))
            }
            BackupStorageType::S3 => Err("S3 storages use the S3 API.".to_owned()),
        }
    }
}

/// The curl process of one FTPS request: no local shell, a cleared environment, and the request
/// with the password on its standard input.
fn curl_command() -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("curl");
    cmd.env_clear()
        .env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".into()),
        )
        .args(["-q", "--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}

/// What a connection test of a storage found.
#[derive(Debug, Clone)]
pub struct ConnectionCheck {
    pub result: Result<(), String>,
    /// `usesPathStyleEndpoint()` after the test (S3).
    pub path_style: bool,
    /// The host key that an SFTP server without a saved key presented, to save.
    pub new_sftp_host_key: Option<String>,
}

impl Jobs {
    /// `S3Storage::trustedInternalHosts()` on the instance's URL policy: the bundled MinIO
    /// container is a trusted internal S3 target, not a user-supplied webhook destination.
    pub(crate) async fn storage_url_policy(&self, row: &Record) -> UrlPolicy {
        let mut policy = UrlPolicy::load(&self.0.db).await;
        let endpoint = parts::str_of(row, "endpoint").unwrap_or_default();
        if parts::str_of(row, "uuid") == Some("minio")
            && crate::compose_parser::php::parse_url(endpoint)
                .and_then(|u| u.host)
                .as_deref()
                == Some("coolify-minio")
        {
            policy.allowed.push("coolify-minio".to_owned());
        }
        policy
    }

    /// `S3Storage::sftpPrivateKey()`: the private key of an SFTP storage, only when it belongs to
    /// the team of the storage. `None`: it logs in with the password.
    pub async fn sftp_private_key(&self, row: &Record) -> Result<Option<String>, String> {
        let Some(key_id) = row.get("private_key_id").and_then(Value::as_i64) else {
            return Ok(None);
        };
        if BackupStorageType::of(row) != BackupStorageType::Sftp {
            return Ok(None);
        }
        let team_id = row.get("team_id").and_then(Value::as_i64).unwrap_or(-1);
        let found: Option<(String, bool)> = sqlx::query_as(
            "SELECT private_key, is_git_related FROM private_keys WHERE id = $1 AND team_id = $2",
        )
        .bind(key_id)
        .bind(team_id)
        .fetch_optional(self.0.db.pool())
        .await
        .map_err(|e| e.to_string())?;
        let Some((encrypted, false)) = found else {
            return Err("The private key of this storage does not exist, belongs to another team, or is a Git deploy key.".to_owned());
        };
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        enc.decrypt_string(&encrypted)
            .map(Some)
            .map_err(|_| "The MAC is invalid.".to_owned())
    }

    /// The decrypted storage of a row whose type is not S3, with its pinned address.
    pub async fn file_storage(&self, row: &Record) -> Result<FileStorage, String> {
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let kind = BackupStorageType::of(row);
        let endpoint = parts::str_of(row, "endpoint")
            .unwrap_or_default()
            .to_owned();
        let secret = |k: &str| {
            enc.decrypt_string(parts::str_of(row, k).unwrap_or_default())
                .map_err(|_| "The MAC is invalid.".to_owned())
        };
        let user = secret("key")?;
        let password = secret("secret")?;
        let private_key = self.sftp_private_key(row).await?;
        let base_path = if kind == BackupStorageType::Webdav {
            String::new()
        } else {
            st::base_path(&endpoint)?
        };
        let policy = self.storage_url_policy(row).await;
        let (host, ips, port) =
            s3::safe_url_pin_for(&endpoint, &policy, kind.endpoint_schemes()).await?;
        let address = ips
            .iter()
            .find(|ip| ip.is_ipv4())
            .or(ips.first())
            .map_or_else(|| host.clone(), IpAddr::to_string);
        Ok(FileStorage {
            kind,
            host,
            port,
            user,
            password,
            private_key,
            sftp_host_key: parts::str_of(row, "sftp_host_key")
                .filter(|k| !k.trim().is_empty())
                .map(str::to_owned),
            base_path,
            address,
            ips,
            endpoint,
        })
    }

    /// The branch of `S3Storage::testConnection()` for SFTP, FTPS and WebDAV storages.
    /// `accept_new_host_key` only when an admin validates the storage: an SFTP storage without a
    /// saved host key then reports the presented key. Background tests refuse such a storage, so a
    /// reset or changed key is never trusted without the admin.
    pub(crate) async fn check_file_storage_connection(
        &self,
        row: &Record,
        accept_new_host_key: bool,
    ) -> (Result<(), String>, Option<String>) {
        let kind = BackupStorageType::of(row);
        let mut new_host_key = None;
        let result = self
            .check_file_storage_connection_inner(row, kind, accept_new_host_key, &mut new_host_key)
            .await
            .map_err(|e| friendly_connection_error(kind, e));
        (result, new_host_key)
    }

    async fn check_file_storage_connection_inner(
        &self,
        row: &Record,
        kind: BackupStorageType,
        accept_new_host_key: bool,
        new_host_key: &mut Option<String>,
    ) -> Result<(), String> {
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let endpoint = parts::str_of(row, "endpoint").unwrap_or_default();
        let plain = |k: &str| {
            enc.decrypt_string(parts::str_of(row, k).unwrap_or_default())
                .map_err(|_| "The MAC is invalid.".to_owned())
        };
        let policy = self.storage_url_policy(row).await;
        // `route('settings.advanced')` outside a request: `config('app.url')`.
        let settings_url = format!(
            "{}/settings/advanced",
            crate::notifications::Ctx::load(&self.0.db).await.app_url
        );
        if let Some(error) = connection_settings_error(
            kind,
            endpoint,
            &plain("key")?,
            &plain("secret")?,
            row.get("private_key_id").and_then(Value::as_i64).is_some(),
            &policy,
            &settings_url,
        )
        .await
        {
            return Err(format!("Storage settings are not allowed: {error}"));
        }
        let saved_host_key = parts::str_of(row, "sftp_host_key").filter(|k| !k.trim().is_empty());
        if kind == BackupStorageType::Sftp && !accept_new_host_key {
            require_sftp_host_key(saved_host_key)?;
        }
        let storage = self.file_storage(row).await?;
        let mut disk = if kind == BackupStorageType::Sftp {
            storage.disk_accepting_new_host_key()
        } else {
            storage.disk()?
        };
        disk.list_root(self).await?;
        if kind == BackupStorageType::Sftp && saved_host_key.is_none() {
            // `pinSftpHostKey()`.
            let presented = disk
                .sftp
                .as_ref()
                .and_then(|s| s.presented_host_key())
                .unwrap_or_default()
                .to_owned();
            if presented.is_empty() {
                return Err("The SFTP server did not present a host key.".to_owned());
            }
            *new_host_key = Some(presented);
        }
        Ok(())
    }

    /// `S3Storage::pinSftpHostKey($hostKey, shouldSave: true)`: saves the host key of the first
    /// connection only while the saved storage still has the tested endpoint and no key, so a test
    /// that started before an endpoint change cannot save the key of the old server.
    pub async fn pin_sftp_host_key(&self, row: &Record, host_key: &str) -> Result<(), String> {
        let saved = sqlx::query(
            "UPDATE s3_storages SET sftp_host_key = $1 WHERE id = $2 AND endpoint IS NOT DISTINCT FROM $3 AND sftp_host_key IS NULL",
        )
        .bind(host_key)
        .bind(parts::id_of(row))
        .bind(parts::str_of(row, "endpoint"))
        .execute(self.0.db.pool())
        .await
        .map_err(|e| e.to_string())?;
        if saved.rows_affected() == 0 {
            return Err(
                "The storage settings changed during the connection test. Validate the connection again."
                    .to_owned(),
            );
        }
        Ok(())
    }

    /// `deleteBackupsS3($filenames, $s3)` for every storage type: the object keys of an S3 storage,
    /// or the files of an SFTP, FTPS or WebDAV storage (`deleteBackupsFromFileStorage()`).
    pub async fn delete_backups_from_storage(
        &self,
        row: &Record,
        filenames: &[String],
    ) -> Result<(), String> {
        if filenames.is_empty() {
            return Ok(());
        }
        if st::is_s3(row) {
            let enc = self
                .0
                .encrypter
                .as_ref()
                .ok_or("No application encryption key has been specified.")?;
            return s3::S3Target::from_row(enc, row)?
                .delete_objects(filenames)
                .await;
        }
        let kind = BackupStorageType::of(row);
        let failed = || {
            format!(
                "One or more backup files could not be deleted from the {} storage.",
                kind.label()
            )
        };
        // A file that is gone already counts as deleted, as on S3.
        let mut disk = self
            .file_storage(row)
            .await
            .and_then(|s| s.disk())
            .map_err(|e| {
                tracing::warn!(error = %e, "backup storage is not reachable");
                failed()
            })?;
        for filename in filenames {
            let deleted = match disk.entry(self, filename).await {
                Ok(Some(_)) => disk.delete(self, filename).await,
                Ok(None) => Ok(()),
                Err(e) => Err(e),
            };
            if let Err(e) = deleted {
                tracing::warn!(error = %e, "backup file could not be deleted from the storage");
                return Err(failed());
            }
        }
        // `deleteEmptyFileStorageDirectories()`: the directories of the deleted files that are
        // empty now, and their empty parents, below the base path of the storage. The server
        // refuses a directory with content, so a file that another backup writes now stays.
        if disk.deletes_empty_directories() {
            let mut directories: Vec<&str> = Vec::new();
            for filename in filenames {
                let directory = dirname(filename.trim_matches('/'));
                if !directories.contains(&directory) {
                    directories.push(directory);
                }
            }
            for mut directory in directories {
                while directory != "." && !directory.is_empty() {
                    match disk.delete_empty_directory(self, directory).await {
                        Ok(true) => directory = dirname(directory),
                        Ok(false) => break,
                        Err(e) => {
                            tracing::warn!("Could not delete an empty backup directory: {e}");
                            break;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// `$disk->exists($key)` with `$disk->size($key)` for every storage type: the size of a
    /// backup file, or `None` when the storage has no such file.
    pub async fn storage_file_size(&self, row: &Record, key: &str) -> Result<Option<u64>, String> {
        if st::is_s3(row) {
            let enc = self
                .0
                .encrypter
                .as_ref()
                .ok_or("No application encryption key has been specified.")?;
            return s3::S3Target::from_row(enc, row)?.object_size(key).await;
        }
        let mut disk = self.file_storage(row).await?.disk()?;
        match disk.entry(self, key).await? {
            Some(Entry { is_dir: false, size }) => size.map(Some).ok_or_else(|| {
                format!(
                    "Unable to retrieve the file_size for file at location: {}. The server does not return the file size.",
                    normalize(key)
                )
            }),
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage(kind: BackupStorageType, base: &str) -> FileStorage {
        FileStorage {
            kind,
            endpoint: "ftps://ftp.example.com:2121/backups".into(),
            host: "ftp.example.com".into(),
            port: 2121,
            user: "bob:smith@example.com".into(),
            password: "p@ss/word".into(),
            private_key: None,
            sftp_host_key: None,
            base_path: base.into(),
            address: "203.0.113.7".into(),
            ips: Vec::new(),
        }
    }

    #[test]
    fn ftps_paths_are_below_the_base_path() {
        assert_eq!(
            ftps_full_path("/backups", "/data/a.dmp").unwrap(),
            "/backups/data/a.dmp"
        );
        assert_eq!(ftps_full_path("", "data/a.dmp").unwrap(), "data/a.dmp");
        assert_eq!(ftps_full_path("/backups", "").unwrap(), "/backups");
        assert_eq!(ftps_full_path("/", "").unwrap(), "/");
        assert_eq!(ftps_full_path("", "").unwrap(), "");
        assert_eq!(
            ftps_full_path("/backups", "a\r\nDELE x").unwrap_err(),
            "The path contains control characters."
        );
    }

    #[test]
    fn ftps_urls_are_absolute_with_an_absolute_base() {
        let s = storage(BackupStorageType::Ftps, "/my backups");
        assert_eq!(
            ftps_url(&s, "data/a b.dmp", false).unwrap(),
            "ftp://bob%3Asmith%40example.com:p%40ss%2Fword@ftp.example.com:2121/%2Fmy%20backups/data/a%20b.dmp"
        );
        assert_eq!(
            ftps_url(&s, "", true).unwrap(),
            "ftp://bob%3Asmith%40example.com:p%40ss%2Fword@ftp.example.com:2121/%2Fmy%20backups/"
        );
        let relative = storage(BackupStorageType::Ftps, "");
        assert_eq!(
            ftps_url(&relative, "", true).unwrap(),
            "ftp://bob%3Asmith%40example.com:p%40ss%2Fword@ftp.example.com:2121/"
        );
        let mut v6 = storage(BackupStorageType::Ftps, "");
        v6.host = "2001:db8::1".into();
        assert!(
            ftps_url(&v6, "a", false)
                .unwrap()
                .contains("@[2001:db8::1]:2121/a")
        );
    }

    #[test]
    fn ftps_requests_need_tls_and_the_pinned_ip() {
        let s = storage(BackupStorageType::Ftps, "/backups");
        let config = ftps_config(
            &s,
            "ftp://u:p@ftp.example.com:2121/",
            &["list-only".to_owned()],
            false,
        );
        assert_eq!(
            config,
            "url = \"ftp://u:p@ftp.example.com:2121/\"\nssl-reqd\nproto = \"=ftp\"\nconnect-timeout = 15\nftp-skip-pasv-ip\nsilent\nshow-error\nmax-time = 30\nresolve = \"ftp.example.com:2121:203.0.113.7\"\nlist-only\n"
        );
        let transfer = ftps_config(&s, "ftp://h/a", &[], true);
        assert!(transfer.contains("speed-limit = 1\nspeed-time = 60\n"));
        assert!(!transfer.contains("max-time"));
    }

    #[test]
    fn curl_errors_are_read_from_its_output() {
        assert_eq!(
            curl_error_message("curl: (67) Access denied: 530\n"),
            "Access denied: 530"
        );
        assert_eq!(curl_error_message("something else"), "something else");
        assert!(
            FtpsRequestFailed {
                code: 78,
                message: String::new()
            }
            .is_not_found()
        );
        assert!(
            !FtpsRequestFailed {
                code: 67,
                message: String::new()
            }
            .is_not_found()
        );
        assert_eq!(
            FtpsRequestFailed {
                code: 67,
                message: "Access denied: 530".into()
            }
            .to_string(),
            "FTPS request failed: Access denied: 530"
        );
        assert_eq!(
            ftps_head_size(
                "Last-Modified: Tue, 10 Nov 2026 12:00:00 GMT\r\nContent-Length: 1234\r\nAccept-ranges: bytes\r\n"
            ),
            Some(1234)
        );
        assert_eq!(ftps_head_size(""), None);
    }

    #[test]
    fn sftp_paths_are_below_the_base_path() {
        assert_eq!(
            sftp_path("/srv/backups", "data/a.dmp"),
            "/srv/backups/data/a.dmp"
        );
        assert_eq!(sftp_path("", "/data/a.dmp"), "data/a.dmp");
        assert_eq!(normalize("/data/a.dmp"), "data/a.dmp");
        assert_eq!(dirname("data/coolify/a.dmp"), "data/coolify");
        assert_eq!(dirname("data"), ".");
    }

    #[test]
    fn propfind_answers_list_files_and_directories() {
        let body = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:">
 <d:response>
  <d:href>/remote.php/dav/files/bob/backups/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
 <d:response>
  <d:href>/remote.php/dav/files/bob/backups/data/my%20db.dmp</d:href>
  <d:propstat><d:prop><d:resourcetype/><d:getcontentlength>2048</d:getcontentlength></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  <d:propstat><d:prop><d:getcontenttype/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>
 </d:response>
</d:multistatus>"#;
        let entries = parse_propfind(body, "/remote.php/dav/files/bob/backups/").unwrap();
        assert_eq!(
            entries,
            vec![
                (
                    String::new(),
                    Entry {
                        is_dir: true,
                        size: None
                    }
                ),
                (
                    "data/my db.dmp".to_owned(),
                    Entry {
                        is_dir: false,
                        size: Some(2048)
                    }
                ),
            ]
        );
        // Another prefix and the default namespace.
        let other = r#"<multistatus xmlns="DAV:"><response><href>https://h/dav/a.gz</href><propstat><prop><getcontentlength>7</getcontentlength></prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#;
        assert_eq!(
            parse_propfind(other, "/dav/").unwrap(),
            vec![(
                "a.gz".to_owned(),
                Entry {
                    is_dir: false,
                    size: Some(7)
                }
            )]
        );
        assert_eq!(parse_propfind("<html>Login</html>", "/dav/"), None);
        assert_eq!(
            parse_propfind(r#"<D:multistatus xmlns:D="DAV:"></D:multistatus>"#, "/"),
            Some(Vec::new())
        );
    }

    #[test]
    fn webdav_paths_are_encoded_by_segment() {
        assert_eq!(
            webdav_uri("data/my db.dmp", false).unwrap(),
            "data/my%20db.dmp"
        );
        assert_eq!(webdav_uri("data/", true).unwrap(), "data/");
        assert_eq!(webdav_uri("", true).unwrap(), "");
        assert_eq!(
            webdav_uri("a\nb", false).unwrap_err(),
            "The path contains control characters."
        );
    }

    #[test]
    fn connection_errors_name_the_storage_type() {
        assert_eq!(
            friendly_connection_error(BackupStorageType::Webdav, "operation timed out".into()),
            "Could not connect to the WebDAV server within 15 seconds. Please verify the host, port, credentials, and network/firewall settings."
        );
        // The network error of SFTP is shown as it is.
        let sftp = "Could not connect to the SFTP server: connect to host 203.0.113.7 port 22: Connection refused";
        assert_eq!(
            friendly_connection_error(BackupStorageType::Sftp, sftp.into()),
            sftp
        );
        assert_eq!(
            friendly_connection_error(
                BackupStorageType::Ftps,
                "FTPS request failed: Access denied: 530".into()
            ),
            "FTPS request failed: Access denied: 530"
        );
    }

    #[test]
    fn endpoints_must_not_hold_credentials() {
        assert_eq!(
            endpoint_credentials_error("sftp://bob:pw@h/x", None, "endpoint").unwrap(),
            "The endpoint must not contain a username or password. Enter the credentials in their own fields."
        );
        // The saved endpoint may keep them.
        assert_eq!(
            endpoint_credentials_error(
                "sftp://bob:pw@h/x",
                Some(" sftp://bob:pw@h/x "),
                "endpoint"
            ),
            None
        );
        assert_eq!(
            endpoint_credentials_error("sftp://h/x", None, "endpoint"),
            None
        );
        assert_eq!(
            endpoint_path_error("sftp://h/a%0Ab", "Endpoint").unwrap(),
            "The Endpoint path must not contain control characters."
        );
        assert_eq!(endpoint_path_error("sftp://h/a", "endpoint"), None);
        assert!(valid_user("bob smith@example.com"));
        assert!(!valid_user("bob\"x"));
        assert!(!valid_user("bob\nhost = evil"));
    }

    #[tokio::test]
    async fn settings_are_checked_before_a_connection() {
        let policy = UrlPolicy::default();
        let check = |kind: BackupStorageType,
                     endpoint: &'static str,
                     user: &'static str,
                     password: &'static str,
                     key: bool| {
            let policy = policy.clone();
            async move {
                connection_settings_error(
                    kind,
                    endpoint,
                    user,
                    password,
                    key,
                    &policy,
                    "http://c/settings/advanced",
                )
                .await
            }
        };
        use BackupStorageType::{Ftps, Sftp, Webdav};
        assert_eq!(
            check(Sftp, "", "bob", "pw", false).await.unwrap(),
            "The endpoint field is required."
        );
        assert_eq!(
            check(Sftp, "https://8.8.8.8/x", "bob", "pw", false)
                .await
                .unwrap(),
            "The endpoint must use the sftp scheme."
        );
        assert_eq!(
            check(Webdav, "sftp://8.8.8.8/x", "bob", "pw", false)
                .await
                .unwrap(),
            "The endpoint must use the https or http scheme."
        );
        assert_eq!(
            check(Ftps, "ftps://8.8.8.8", "bo\"b", "pw", false)
                .await
                .unwrap(),
            "The key field format is invalid."
        );
        assert_eq!(
            check(Ftps, "ftps://8.8.8.8", "bob", "", false)
                .await
                .unwrap(),
            "The secret field is required."
        );
        assert_eq!(
            check(Sftp, "sftp://8.8.8.8", "bob", "", false)
                .await
                .unwrap(),
            "The secret field is required when private key id is not present."
        );
        assert_eq!(check(Sftp, "sftp://8.8.8.8", "bob", "", true).await, None);
        assert_eq!(
            check(Sftp, "sftp://8.8.8.8:2222/srv", "bob", "pw", false).await,
            None
        );
        // Private addresses need the instance's allow list, as for S3.
        assert!(
            check(Sftp, "sftp://10.0.0.5", "bob", "pw", false)
                .await
                .unwrap()
                .contains("points to a local or private address")
        );
    }
}
