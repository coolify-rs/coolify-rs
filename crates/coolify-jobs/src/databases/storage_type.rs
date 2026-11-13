//! `App\Enums\BackupStorageType` and the parts of `S3Storage` that depend on it (Coolify v4.7.0):
//! the endpoint URL of an SFTP, FTPS or WebDAV storage, its rclone config, and the transfer of an
//! S3 endpoint with a path (curl with a presigned URL).

use coolify_db::records::Record;
use serde_json::Value;

use super::connection::rawurlencode;
use crate::compose_parser::php;

/// The protocol of a backup storage (`s3_storages.type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupStorageType {
    S3,
    Sftp,
    Ftps,
    Webdav,
}

impl BackupStorageType {
    /// `BackupStorageType::cases()`.
    pub const ALL: [Self; 4] = [Self::S3, Self::Sftp, Self::Ftps, Self::Webdav];

    /// `BackupStorageType::tryFrom()`.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.value() == value)
    }

    /// `S3Storage::storageType()`: a row without the `type` column (an older schema, or a partial
    /// select) is an S3 storage, as every storage was before other types existed.
    pub fn of(row: &Record) -> Self {
        row.get("type")
            .and_then(Value::as_str)
            .and_then(Self::parse)
            .unwrap_or(Self::S3)
    }

    pub fn value(self) -> &'static str {
        match self {
            Self::S3 => "s3",
            Self::Sftp => "sftp",
            Self::Ftps => "ftps",
            Self::Webdav => "webdav",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::S3 => "S3",
            Self::Sftp => "SFTP",
            Self::Ftps => "FTPS",
            Self::Webdav => "WebDAV",
        }
    }

    /// The URL schemes that the endpoint of this type can use.
    pub fn endpoint_schemes(self) -> &'static [&'static str] {
        match self {
            Self::S3 | Self::Webdav => &["https", "http"],
            Self::Sftp => &["sftp"],
            Self::Ftps => &["ftps"],
        }
    }

    pub fn default_port(self) -> Option<u16> {
        match self {
            Self::S3 | Self::Webdav => None,
            Self::Sftp => Some(22),
            Self::Ftps => Some(21),
        }
    }

    /// `BackupStorageType::options()`.
    pub fn options() -> Value {
        Value::Array(
            Self::ALL
                .into_iter()
                .map(|t| serde_json::json!({"value": t.value(), "label": t.label()}))
                .collect(),
        )
    }
}

/// `S3Storage::isS3()`.
pub fn is_s3(row: &Record) -> bool {
    BackupStorageType::of(row) == BackupStorageType::S3
}

/// `S3Storage::RCLONE_REMOTE`: the name of the rclone remote in [`rclone_config`].
pub const RCLONE_REMOTE: &str = "storage";

/// `S3Storage::SFTP_HOST_KEY_MISSING_MESSAGE`.
pub const SFTP_HOST_KEY_MISSING_MESSAGE: &str = "The SFTP host key of this storage is not saved yet. Validate the connection in the storage settings to save it.";

/// `S3Storage::RCLONE_OBSCURE_KEY`: the key of the rclone password obscuring (fs/config/obscure in
/// rclone). It only hides the password from a casual look at the config file; it is not encryption.
const RCLONE_OBSCURE_KEY: [u8; 32] = [
    0x9c, 0x93, 0x5b, 0x48, 0x73, 0x0a, 0x55, 0x4d, 0x6b, 0xfd, 0x7c, 0x63, 0xc8, 0x86, 0xa9, 0x2b,
    0xd3, 0x90, 0x19, 0x8e, 0xb8, 0x12, 0x8a, 0xfb, 0xf4, 0xde, 0x16, 0x2b, 0x8b, 0x95, 0xf6, 0x38,
];

/// PHP `rawurldecode`.
pub fn rawurldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `S3Storage::hasControlCharacters()`.
pub fn has_control_characters(value: &str) -> bool {
    value.bytes().any(|b| b < 0x20 || b == 0x7f)
}

fn trim_brackets(host: &str) -> &str {
    host.trim_matches(|c| c == '[' || c == ']')
}

/// `S3Storage::serverEndpoint()`: the endpoint URL of an SFTP or FTPS storage, from the form fields.
pub fn server_endpoint(kind: BackupStorageType, host: &str, port: &str, path: &str) -> String {
    let mut host = host.trim().to_owned();
    if host.contains(':') && !host.starts_with('[') {
        host = format!("[{host}]");
    }
    let port = port.trim();
    let path = path
        .trim()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(rawurlencode)
        .collect::<Vec<_>>()
        .join("/");
    format!(
        "{}://{host}{}{}",
        kind.endpoint_schemes()[0],
        if port.is_empty() {
            String::new()
        } else {
            format!(":{port}")
        },
        if path.is_empty() {
            String::new()
        } else {
            format!("/{path}")
        }
    )
}

/// `S3Storage::splitServerEndpoint()`: the form fields of an SFTP or FTPS endpoint URL, as
/// `(host, port, path)`.
pub fn split_server_endpoint(endpoint: Option<&str>) -> (String, String, String) {
    let parts = php::parse_url(endpoint.unwrap_or_default()).unwrap_or_default();
    (
        trim_brackets(parts.host.as_deref().unwrap_or_default()).to_owned(),
        parts.port.map(|p| p.to_string()).unwrap_or_default(),
        rawurldecode(parts.path.as_deref().unwrap_or_default())
            .trim_matches('/')
            .to_owned(),
    )
}

/// The host of an endpoint URL without the brackets of an IPv6 address.
pub fn endpoint_host(endpoint: &str) -> String {
    php::parse_url(endpoint)
        .and_then(|u| u.host)
        .map(|h| trim_brackets(&h).to_owned())
        .unwrap_or_default()
}

/// `S3Storage::basePath()`: the directory on an SFTP or FTPS server that holds the backups: the
/// path of the endpoint URL. Empty means the login directory of the user.
pub fn base_path(endpoint: &str) -> Result<String, String> {
    let path = rawurldecode(
        php::parse_url(endpoint)
            .and_then(|u| u.path)
            .as_deref()
            .unwrap_or_default(),
    );
    let path = path.trim_end_matches('/').to_owned();
    if has_control_characters(&path) {
        return Err("The storage path must not contain control characters.".to_owned());
    }
    Ok(path)
}

/// `S3Storage::endpointPort()`.
pub fn endpoint_port(kind: BackupStorageType, endpoint: &str) -> u16 {
    php::parse_url(endpoint)
        .and_then(|u| u.port)
        .filter(|p| *p != 0)
        .or(kind.default_port())
        .unwrap_or(0)
}

/// The server of an endpoint, as the `endpoint` mutator of `S3Storage` compares it: another host
/// or port is another server, so the saved SFTP host key no longer applies.
pub fn endpoint_server(endpoint: Option<&str>) -> (String, Option<u16>) {
    let parts = php::parse_url(endpoint.unwrap_or_default()).unwrap_or_default();
    (
        trim_brackets(parts.host.as_deref().unwrap_or_default()).to_lowercase(),
        parts.port,
    )
}

/// Whether saving `new` over `previous` drops the saved SFTP host key (the `endpoint` mutator).
pub fn endpoint_change_resets_host_key(previous: Option<&str>, new: Option<&str>) -> bool {
    previous.is_some() && endpoint_server(previous) != endpoint_server(new)
}

/// `S3Storage::rcloneRemotePath()`: the path of a backup file in the storage, as rclone reads it.
/// The file keeps its full local path, as the S3 object key does, below the base path (SFTP, FTPS)
/// or the base URL (WebDAV).
pub fn rclone_remote_path(
    kind: BackupStorageType,
    endpoint: &str,
    file: &str,
) -> Result<String, String> {
    let file = file.trim_start_matches('/');
    let base = if kind == BackupStorageType::Webdav {
        String::new()
    } else {
        base_path(endpoint)?
    };
    Ok(if base.is_empty() {
        format!("{RCLONE_REMOTE}:{file}")
    } else {
        format!("{RCLONE_REMOTE}:{base}/{file}")
    })
}

/// What [`rclone_config`] reads from a storage.
#[derive(Clone)]
pub struct RcloneSource<'a> {
    pub kind: BackupStorageType,
    pub endpoint: &'a str,
    /// `key`: the user.
    pub user: &'a str,
    /// `secret`: the password.
    pub password: &'a str,
    /// `sftpPrivateKey()?->private_key`.
    pub private_key: Option<&'a str>,
    pub sftp_host_key: Option<&'a str>,
}

impl std::fmt::Debug for RcloneSource<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RcloneSource")
            .field("kind", &self.kind)
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

/// `S3Storage::requireSftpHostKey()`.
pub fn require_sftp_host_key(host_key: Option<&str>) -> Result<&str, String> {
    host_key
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| SFTP_HOST_KEY_MISSING_MESSAGE.to_owned())
}

/// `S3Storage::rcloneConfig()`: the rclone config of a storage. The server gets it through stdin,
/// so the password is never on a command line. `iv` is the random IV of the obscured password.
pub fn rclone_config(source: &RcloneSource<'_>, iv: [u8; 16]) -> Result<String, String> {
    let host = endpoint_host(source.endpoint);
    let port = endpoint_port(source.kind, source.endpoint).to_string();
    let mut options: Vec<(&str, String)> = match source.kind {
        // host_keys makes rclone refuse another host key, and offer only the algorithm of the saved key.
        BackupStorageType::Sftp => vec![
            ("type", "sftp".to_owned()),
            ("host", host),
            ("port", port),
            ("shell_type", "none".to_owned()),
            (
                "host_keys",
                require_sftp_host_key(source.sftp_host_key)?.to_owned(),
            ),
        ],
        BackupStorageType::Ftps => vec![
            ("type", "ftp".to_owned()),
            ("host", host),
            ("port", port),
            ("explicit_tls", "true".to_owned()),
        ],
        BackupStorageType::Webdav => vec![
            ("type", "webdav".to_owned()),
            ("url", source.endpoint.to_owned()),
            ("vendor", "other".to_owned()),
        ],
        BackupStorageType::S3 => return Err("S3 storages use the MinIO client.".to_owned()),
    };
    options.push(("user", source.user.to_owned()));
    match source.private_key {
        // rclone reads key_pem from one line, with \n for each line break.
        Some(key) => options.push((
            "key_pem",
            key.replace("\r\n", "\n").trim().replace('\n', "\\n"),
        )),
        None => options.push(("pass", rclone_obscure(source.password, iv))),
    }
    let mut lines = vec![format!("[{RCLONE_REMOTE}]")];
    for (name, value) in options {
        if has_control_characters(&value) {
            return Err(format!(
                "The storage {name} must not contain control characters."
            ));
        }
        lines.push(format!("{name} = {value}"));
    }
    Ok(lines.join("\n") + "\n")
}

fn aes256_ctr(key: &[u8; 32], iv: [u8; 16], data: &mut [u8]) {
    use aes::cipher::{BlockEncrypt, KeyInit};
    let cipher = aes::Aes256::new(&(*key).into());
    let mut counter = u128::from_be_bytes(iv);
    for chunk in data.chunks_mut(16) {
        let mut block = counter.to_be_bytes().into();
        cipher.encrypt_block(&mut block);
        for (byte, mask) in chunk.iter_mut().zip(block.iter()) {
            *byte ^= mask;
        }
        counter = counter.wrapping_add(1);
    }
}

/// `S3Storage::rcloneObscure()`: obscures a password as `rclone obscure` does: AES-CTR with the
/// fixed rclone key and a random IV, in URL-safe base64 without padding.
pub fn rclone_obscure(value: &str, iv: [u8; 16]) -> String {
    use base64::Engine as _;
    let mut data = value.as_bytes().to_vec();
    aes256_ctr(&RCLONE_OBSCURE_KEY, iv, &mut data);
    let mut out = iv.to_vec();
    out.extend_from_slice(&data);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(out)
}

/// A random IV for [`rclone_config`].
pub fn random_iv() -> [u8; 16] {
    use rand::Rng as _;
    rand::thread_rng().r#gen()
}

/// `PinnedSftpConnectionProvider::fingerprint()`: the OpenSSH SHA256 fingerprint of a host key, as
/// `ssh-keygen -l` prints it.
pub fn sftp_host_key_fingerprint(host_key: &str) -> String {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};
    let blob = base64::engine::general_purpose::STANDARD
        .decode(host_key.split(' ').nth(1).unwrap_or_default())
        .unwrap_or_default();
    format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(Sha256::digest(&blob))
    )
}

/// `S3Storage::MINIO_CLIENT_MAX_UPLOAD_PARTS`: the most parts that an upload with the MinIO client
/// uses. Most providers accept 10000 parts, but some, like Scaleway, accept only 1000. The MinIO
/// client sizes the parts for 10000 parts (16 MiB for files up to about 156 GiB), so without a part
/// size, files larger than about 15.6 GiB fail on these providers.
const MINIO_CLIENT_MAX_UPLOAD_PARTS: u64 = 1000;

/// `S3Storage::MINIO_CLIENT_MIN_PART_SIZE_BYTES`: the smallest part size of an upload with the
/// MinIO client. It is also the default part size of the MinIO client.
const MINIO_CLIENT_MIN_PART_SIZE_BYTES: u64 = 16 * 1024 * 1024;

/// `S3Storage::minioClientPartSizeOption()`: the `docker exec` option that makes `mc cp` upload a
/// file of `file_size` bytes in at most 1000 parts. `mc cp` has no part size flag, but it reads the
/// part size from `MC_UPLOAD_MULTIPART_SIZE`.
pub fn minio_client_part_size_option(file_size: u64) -> String {
    const MEBIBYTE: u64 = 1024 * 1024;
    let per_part = file_size.div_ceil(MINIO_CLIENT_MAX_UPLOAD_PARTS);
    let part_size = MINIO_CLIENT_MIN_PART_SIZE_BYTES.max(per_part.div_ceil(MEBIBYTE) * MEBIBYTE);
    format!("-e MC_UPLOAD_MULTIPART_SIZE={part_size}")
}

/// `S3Storage::CURL_TRANSFER_URL_TTL_HOURS`: how long the presigned URL of a curl transfer is
/// valid. S3 checks it when the request starts, so it must cover the wait for the earlier transfers
/// and the retries of the backup job.
pub const CURL_TRANSFER_URL_TTL_SECONDS: u64 = 24 * 3600;

/// `S3Storage::CURL_TRANSFER_CONFIG_PATH`.
pub const CURL_TRANSFER_CONFIG_PATH: &str = "/tmp/s3-transfer.curl";

/// `S3Storage::usesCurlTransfer()`: the MinIO client accepts only `scheme://host[:port]` endpoints.
/// Providers like Supabase Storage need a path (`/storage/v1/s3`), so transfers to these endpoints
/// use curl with a presigned URL instead.
pub fn uses_curl_transfer(row: &Record) -> bool {
    is_s3(row)
        && endpoint_has_path(
            row.get("endpoint")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
}

/// Whether an endpoint URL has a path (the test of [`uses_curl_transfer`]).
pub fn endpoint_has_path(endpoint: &str) -> bool {
    php::parse_url(endpoint)
        .and_then(|u| u.path)
        .is_some_and(|p| !p.trim_matches('/').is_empty())
}

/// `S3Storage::curlConfigValue()`: `addcslashes($value, '"\\')` in double quotes.
pub fn curl_config_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The `upload-file` options of `S3Storage::prepareCurlUpload()`. An S3 error is an XML body;
/// curl writes it to stderr, so it becomes the error message of the backup.
pub fn curl_upload_options(local_path: &str) -> Vec<String> {
    vec![
        format!("upload-file = {}", curl_config_value(local_path)),
        "fail-with-body".to_owned(),
        "output = \"/dev/stderr\"".to_owned(),
    ]
}

/// The options of `S3Storage::prepareCurlDownload()`.
pub fn curl_download_options(local_path: &str) -> Vec<String> {
    vec![
        format!("output = {}", curl_config_value(local_path)),
        "fail".to_owned(),
    ]
}

/// The curl config of `S3Storage::prepareCurlTransfer()`. It holds the presigned URL, so the URL is
/// not in the arguments of any process, and the S3 keys never reach the server. The `resolve` line
/// pins the endpoint host to its validated IPs (`resolve_entries` are `host:port=ip`, as
/// `SafeWebhookUrl::minioClientResolveOptions()` returns them).
pub fn curl_transfer_config(
    url: &str,
    options: &[String],
    resolve_entries: &[String],
    skip_tls_verification: bool,
) -> String {
    let mut lines = vec![
        format!("url = {}", curl_config_value(url)),
        "silent".to_owned(),
        "show-error".to_owned(),
    ];
    lines.extend(options.iter().cloned());
    if let Some(first) = resolve_entries.first() {
        let host_port = first.rsplit_once('=').map_or(first.as_str(), |(h, _)| h);
        let ips = resolve_entries
            .iter()
            .map(|entry| entry.rsplit_once('=').map_or(entry.as_str(), |(_, ip)| ip))
            .map(|ip| {
                if ip.contains(':') {
                    format!("[{ip}]")
                } else {
                    ip.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        lines.push(format!(
            "resolve = {}",
            curl_config_value(&format!("{host_port}:{ips}"))
        ));
    }
    if skip_tls_verification {
        lines.push("insecure".to_owned());
    }
    lines.join("\n") + "\n"
}

/// The command that writes `content` to `path` in a container. Upstream pipes the content into the
/// stdin of a separate ssh process; here it travels inside the script, through the shell's builtin
/// `echo`, like every command input of coolify-rs, so it is not in the arguments of any process.
pub fn write_in_container_command(container: &str, path: &str, content: &str) -> String {
    use base64::Engine as _;
    use coolify_remote::shell::quote;
    format!(
        "echo '{}' | base64 -d | docker exec -i {} sh -c {}",
        base64::engine::general_purpose::STANDARD.encode(content.as_bytes()),
        quote(container),
        quote(&format!("umask 077 && cat > {path}"))
    )
}

/// The command that `prepareCurlTransfer()` returns: it runs the transfer of the written config.
pub fn curl_transfer_command(container: &str) -> String {
    format!(
        "docker exec {} curl --config {CURL_TRANSFER_CONFIG_PATH}",
        coolify_remote::shell::quote(container)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: Option<&str>, endpoint: &str) -> Record {
        let mut r = Record::new();
        if let Some(kind) = kind {
            r.insert("type".into(), kind.into());
        }
        r.insert("endpoint".into(), endpoint.into());
        r
    }

    #[test]
    fn a_row_without_a_type_is_an_s3_storage() {
        assert_eq!(BackupStorageType::of(&Record::new()), BackupStorageType::S3);
        assert_eq!(
            BackupStorageType::of(&row(Some("webdav"), "")),
            BackupStorageType::Webdav
        );
        assert_eq!(BackupStorageType::parse("ftp"), None);
        assert_eq!(BackupStorageType::Sftp.endpoint_schemes(), ["sftp"]);
        assert_eq!(BackupStorageType::Ftps.default_port(), Some(21));
        assert_eq!(BackupStorageType::Webdav.default_port(), None);
        assert_eq!(
            BackupStorageType::options(),
            serde_json::json!([
                {"value": "s3", "label": "S3"},
                {"value": "sftp", "label": "SFTP"},
                {"value": "ftps", "label": "FTPS"},
                {"value": "webdav", "label": "WebDAV"},
            ])
        );
    }

    #[test]
    fn server_endpoints_are_built_from_the_form_fields() {
        use BackupStorageType::{Ftps, Sftp};
        assert_eq!(
            server_endpoint(Sftp, " backup.example.com ", "2222", "/srv/my backups/"),
            "sftp://backup.example.com:2222/srv/my%20backups"
        );
        assert_eq!(
            server_endpoint(Ftps, "ftp.example.com", "", ""),
            "ftps://ftp.example.com"
        );
        // IPv6 hosts get brackets.
        assert_eq!(
            server_endpoint(Sftp, "2001:db8::1", "22", "backups"),
            "sftp://[2001:db8::1]:22/backups"
        );
        assert_eq!(
            server_endpoint(Sftp, "[2001:db8::1]", "", ""),
            "sftp://[2001:db8::1]"
        );
        assert_eq!(
            split_server_endpoint(Some("sftp://[2001:db8::1]:22/srv/my%20backups/")),
            (
                "2001:db8::1".to_owned(),
                "22".to_owned(),
                "srv/my backups".to_owned()
            )
        );
        assert_eq!(
            split_server_endpoint(Some("ftps://ftp.example.com")),
            ("ftp.example.com".to_owned(), String::new(), String::new())
        );
        assert_eq!(
            split_server_endpoint(None),
            (String::new(), String::new(), String::new())
        );
    }

    #[test]
    fn the_base_path_is_the_path_of_the_endpoint() {
        assert_eq!(base_path("sftp://h/srv/backups/").unwrap(), "/srv/backups");
        assert_eq!(base_path("sftp://h").unwrap(), "");
        assert_eq!(
            base_path("sftp://h/a%0Ab").unwrap_err(),
            "The storage path must not contain control characters."
        );
        assert_eq!(endpoint_port(BackupStorageType::Sftp, "sftp://h"), 22);
        assert_eq!(
            endpoint_port(BackupStorageType::Ftps, "ftps://h:990/x"),
            990
        );
        assert_eq!(endpoint_port(BackupStorageType::Webdav, "https://h/dav"), 0);
    }

    #[test]
    fn another_host_or_port_resets_the_host_key() {
        let saved = Some("sftp://Backup.example.com/srv");
        assert!(!endpoint_change_resets_host_key(
            saved,
            Some("sftp://backup.example.com/other")
        ));
        assert!(endpoint_change_resets_host_key(
            saved,
            Some("sftp://backup.example.com:2222/srv")
        ));
        assert!(endpoint_change_resets_host_key(
            saved,
            Some("sftp://other.example.com/srv")
        ));
        assert!(!endpoint_change_resets_host_key(None, Some("sftp://h")));
    }

    #[test]
    fn remote_paths_keep_the_local_path_below_the_base() {
        use BackupStorageType::{Sftp, Webdav};
        assert_eq!(
            rclone_remote_path(Sftp, "sftp://h/srv/backups", "/data/coolify/backups/a.dmp")
                .unwrap(),
            "storage:/srv/backups/data/coolify/backups/a.dmp"
        );
        assert_eq!(
            rclone_remote_path(Sftp, "sftp://h", "/data/a.dmp").unwrap(),
            "storage:data/a.dmp"
        );
        assert_eq!(
            rclone_remote_path(Webdav, "https://h/remote.php/dav", "/data/a.dmp").unwrap(),
            "storage:data/a.dmp"
        );
    }

    #[test]
    fn rclone_obscures_the_password_as_rclone_does() {
        // `rclone obscure` output for "potato" has the IV in front; rclone reveals it with the IV.
        let iv = [7u8; 16];
        let obscured = rclone_obscure("potato", iv);
        use base64::Engine as _;
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&obscured)
            .unwrap();
        assert_eq!(raw.len(), 16 + 6);
        assert_eq!(raw[..16], iv);
        let mut revealed = raw[16..].to_vec();
        aes256_ctr(&RCLONE_OBSCURE_KEY, iv, &mut revealed);
        assert_eq!(revealed, b"potato");
        assert!(!obscured.contains('=') && !obscured.contains('+') && !obscured.contains('/'));
        // AES-256-CTR of zero bytes with a zero key and IV is the first AES-256 block of zeros.
        let mut zeros = [0u8; 16];
        aes256_ctr(&[0u8; 32], [0u8; 16], &mut zeros);
        assert_eq!(
            zeros,
            [
                0xdc, 0x95, 0xc0, 0x78, 0xa2, 0x40, 0x89, 0x89, 0xad, 0x48, 0xa2, 0x14, 0x92, 0x84,
                0x20, 0x87
            ]
        );
    }

    #[test]
    fn rclone_configs_per_type() {
        let iv = [0u8; 16];
        let sftp = RcloneSource {
            kind: BackupStorageType::Sftp,
            endpoint: "sftp://[2001:db8::1]/srv",
            user: "backup",
            password: "",
            private_key: Some("-----BEGIN KEY-----\r\nabc\r\n-----END KEY-----\r\n"),
            sftp_host_key: Some("ssh-ed25519 AAAA"),
        };
        assert_eq!(
            rclone_config(&sftp, iv).unwrap(),
            "[storage]\ntype = sftp\nhost = 2001:db8::1\nport = 22\nshell_type = none\nhost_keys = ssh-ed25519 AAAA\nuser = backup\nkey_pem = -----BEGIN KEY-----\\nabc\\n-----END KEY-----\n"
        );
        let unpinned = RcloneSource {
            sftp_host_key: None,
            ..sftp.clone()
        };
        assert_eq!(
            rclone_config(&unpinned, iv).unwrap_err(),
            SFTP_HOST_KEY_MISSING_MESSAGE
        );
        let ftps = RcloneSource {
            kind: BackupStorageType::Ftps,
            endpoint: "ftps://ftp.example.com:2121/backups",
            user: "bob",
            password: "pw",
            private_key: None,
            sftp_host_key: None,
        };
        let config = rclone_config(&ftps, iv).unwrap();
        assert!(config.starts_with(
            "[storage]\ntype = ftp\nhost = ftp.example.com\nport = 2121\nexplicit_tls = true\nuser = bob\npass = "
        ));
        assert!(!config.contains("pw\n"));
        let webdav = RcloneSource {
            kind: BackupStorageType::Webdav,
            endpoint: "https://cloud.example.com/remote.php/dav/files/bob",
            user: "bob\nhost = evil",
            password: "pw",
            private_key: None,
            sftp_host_key: None,
        };
        assert_eq!(
            rclone_config(&webdav, iv).unwrap_err(),
            "The storage user must not contain control characters."
        );
        let webdav = RcloneSource {
            user: "bob",
            ..webdav
        };
        assert!(rclone_config(&webdav, iv).unwrap().starts_with(
            "[storage]\ntype = webdav\nurl = https://cloud.example.com/remote.php/dav/files/bob\nvendor = other\nuser = bob\npass = "
        ));
        let s3 = RcloneSource {
            kind: BackupStorageType::S3,
            ..webdav
        };
        assert_eq!(
            rclone_config(&s3, iv).unwrap_err(),
            "S3 storages use the MinIO client."
        );
    }

    #[test]
    fn host_key_fingerprints_match_ssh_keygen() {
        // SHA256 of the empty blob, as `PinnedSftpConnectionProvider::fingerprint()` computes it.
        assert_eq!(
            sftp_host_key_fingerprint("ssh-ed25519"),
            "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU"
        );
        // base64("abc") = "YWJj"; sha256("abc") = ba7816bf...
        assert_eq!(
            sftp_host_key_fingerprint("ssh-ed25519 YWJj"),
            "SHA256:ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0"
        );
    }

    #[test]
    fn uploads_use_at_most_1000_parts() {
        const GIB: u64 = 1024 * 1024 * 1024;
        assert_eq!(
            minio_client_part_size_option(0),
            "-e MC_UPLOAD_MULTIPART_SIZE=16777216"
        );
        assert_eq!(
            minio_client_part_size_option(15 * GIB),
            "-e MC_UPLOAD_MULTIPART_SIZE=16777216"
        );
        // 20 GiB / 1000 = 20.48 MiB, rounded up to 21 MiB.
        assert_eq!(
            minio_client_part_size_option(20 * GIB),
            "-e MC_UPLOAD_MULTIPART_SIZE=22020096"
        );
        // Exactly 16 MiB parts: 16000 MiB.
        assert_eq!(
            minio_client_part_size_option(16_000 * 1024 * 1024),
            "-e MC_UPLOAD_MULTIPART_SIZE=16777216"
        );
        assert_eq!(
            minio_client_part_size_option(16_000 * 1024 * 1024 + 1),
            "-e MC_UPLOAD_MULTIPART_SIZE=17825792"
        );
    }

    #[test]
    fn endpoints_with_a_path_use_curl() {
        assert!(uses_curl_transfer(&row(
            None,
            "https://project.supabase.co/storage/v1/s3"
        )));
        assert!(!uses_curl_transfer(&row(
            Some("s3"),
            "https://s3.example.com/"
        )));
        assert!(!uses_curl_transfer(&row(
            Some("s3"),
            "https://s3.example.com"
        )));
        assert!(!uses_curl_transfer(&row(
            Some("webdav"),
            "https://cloud.example.com/dav"
        )));
    }

    #[test]
    fn curl_configs_hold_the_url_and_the_pins() {
        let config = curl_transfer_config(
            "https://h.example/storage/v1/s3/b/k?X-Amz-Signature=a\"b",
            &curl_upload_options("/data/a \"b\".gz"),
            &[
                "h.example:443=203.0.113.7".to_owned(),
                "h.example:443=2001:db8::1".to_owned(),
            ],
            true,
        );
        assert_eq!(
            config,
            "url = \"https://h.example/storage/v1/s3/b/k?X-Amz-Signature=a\\\"b\"\nsilent\nshow-error\nupload-file = \"/data/a \\\"b\\\".gz\"\nfail-with-body\noutput = \"/dev/stderr\"\nresolve = \"h.example:443:203.0.113.7,[2001:db8::1]\"\ninsecure\n"
        );
        assert_eq!(
            curl_transfer_config(
                "https://h/k",
                &curl_download_options("/tmp/restore"),
                &[],
                false
            ),
            "url = \"https://h/k\"\nsilent\nshow-error\noutput = \"/tmp/restore\"\nfail\n"
        );
        assert_eq!(
            curl_transfer_command("backup-of-x"),
            "docker exec 'backup-of-x' curl --config /tmp/s3-transfer.curl"
        );
        let write =
            write_in_container_command("backup-of-x", CURL_TRANSFER_CONFIG_PATH, "url = \"u\"\n");
        assert_eq!(
            write,
            "echo 'dXJsID0gInUiCg==' | base64 -d | docker exec -i 'backup-of-x' sh -c 'umask 077 && cat > /tmp/s3-transfer.curl'"
        );
    }

    #[test]
    fn rawurldecode_reads_percent_escapes() {
        assert_eq!(rawurldecode("a%20b%2Fc+d%"), "a b/c+d%");
        assert_eq!(rawurldecode("100%zz"), "100%zz");
    }
}
