//! SSH host key pinning: the parts of upstream `SshMultiplexingHelper` and
//! `ServerSetting` (v4.7.5) that need no database.
//!
//! The pinned key of a server lives in `server_settings.ssh_host_key`. Before a
//! connection the key is written to a known_hosts file of its own, and ssh gets
//! `HostKeyAlias`, `UserKnownHostsFile` and `StrictHostKeyChecking` for it.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use base64::Engine as _;
use regex::Regex;
use sha2::{Digest, Sha256};

/// `SshMultiplexingHelper::HOST_KEY_CHANGED_MESSAGE`.
pub const HOST_KEY_CHANGED_MESSAGE: &str = "The SSH host key of this server changed, so Coolify did not connect. An admin must check the new key on the Security page of the server.";

/// `HOST_KEY_SCAN_MAX_CONNECT_TIMEOUT_SECONDS`.
pub const HOST_KEY_SCAN_MAX_CONNECT_TIMEOUT_SECONDS: i64 = 30;

/// `HOST_KEY_CHANGED_WARNING_MAX_BYTES`.
const HOST_KEY_CHANGED_WARNING_MAX_BYTES: usize = 16384;

/// `HOST_KEY_CHANGED_WARNING_PATTERN`: the warning OpenSSH prints at the start of its own stderr
/// when the server sends a host key that is not the pinned key.
static HOST_KEY_CHANGED_WARNING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\A@+\r?\n@\s+WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\s+@\r?\n@+\r?\n",
        r"(?:[^@\r\n].*\r?\n){0,20}?Offending .+ key in .+\r?\n(?:  remove with:\r?\n  ssh-keygen .+\r?\n)?",
        r"(?:(?:.+ is disabled to avoid man-in-the-middle attacks\.|UpdateHostkeys is disabled because the host key is not trusted\.)\r?\n)*",
    ))
    .unwrap_or_else(|e| unreachable!("host key warning pattern: {e}"))
});

/// `constants.ssh.host_key_checking` and the result of `hostKeyCheckingMode()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyMode {
    /// Connect also when the host key changed, and report it.
    Warn,
    /// Refuse a changed host key.
    Enforce,
    /// No check.
    Off,
}

impl HostKeyMode {
    /// `strtolower(trim(env('SSH_HOST_KEY_CHECKING', 'warn')))`; `None` for an unknown value,
    /// which upstream treats as `warn` and logs.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "warn" => Some(Self::Warn),
            "enforce" => Some(Self::Enforce),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Enforce => "enforce",
            Self::Off => "off",
        }
    }

    /// `hostKeyCheckingMode($server)`: a team can make its own server stricter than the instance,
    /// never weaker than an instance "off".
    pub fn for_server(self, check_disabled: bool, check_enforced: bool) -> Self {
        if self == Self::Off || check_disabled {
            return Self::Off;
        }
        if self == Self::Enforce || check_enforced {
            Self::Enforce
        } else {
            Self::Warn
        }
    }
}

/// How one connection checks the host key (`hostKeyOptions()`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HostKeyCheck {
    /// Mode `off`, or `warn` before the first key is pinned:
    /// `-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null`.
    #[default]
    Off,
    /// A pinned key (`server_settings.ssh_host_key`); `enforce` refuses another key.
    Pinned { keys: String, enforce: bool },
}

/// `hostKeyAlias()`.
pub fn host_key_alias(server_uuid: &str) -> String {
    format!("coolify-{server_uuid}")
}

/// `isSshHostKeyType()`.
fn is_ssh_host_key_type(kind: &str) -> bool {
    matches!(
        kind,
        "ssh-ed25519"
            | "ssh-rsa"
            | "ssh-dss"
            | "ecdsa-sha2-nistp256"
            | "ecdsa-sha2-nistp384"
            | "ecdsa-sha2-nistp521"
            | "sk-ssh-ed25519@openssh.com"
            | "sk-ecdsa-sha2-nistp256@openssh.com"
    )
}

/// PHP `base64_decode($blob, true)`: the standard alphabet, padding optional.
fn decode_strict(blob: &str) -> Option<Vec<u8>> {
    const ENGINE: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    ENGINE.decode(blob).ok()
}

/// `sshKeyBlobType()`: the key type that is encoded in the first field of an SSH public key blob.
fn ssh_key_blob_type(blob: &str) -> Option<String> {
    let decoded = decode_strict(blob)?;
    let head: [u8; 4] = decoded.get(..4)?.try_into().ok()?;
    let length = u32::from_be_bytes(head) as usize;
    let kind = decoded.get(4..4usize.checked_add(length)?)?;
    Some(String::from_utf8_lossy(kind).into_owned())
}

/// `ServerSetting::normalizeSshHostKeys()`: reads SSH host keys from public key lines
/// ("type base64 [comment]") or known_hosts lines ("host type base64"). Returns the keys as
/// "type base64" lines, or `None` when there is no valid key.
pub fn normalize_ssh_host_keys(input: Option<&str>) -> Option<String> {
    let mut keys: Vec<String> = Vec::new();
    for line in input.unwrap_or_default().lines() {
        let mut fields: Vec<&str> = line.split_whitespace().collect();
        let Some(first) = fields.first() else {
            continue;
        };
        if first.starts_with('#') || first.starts_with('@') {
            continue;
        }
        if !is_ssh_host_key_type(first) {
            fields.remove(0);
        }
        let kind = fields.first().copied().unwrap_or_default();
        let blob = fields.get(1).copied().unwrap_or_default();
        if is_ssh_host_key_type(kind) && ssh_key_blob_type(blob).as_deref() == Some(kind) {
            let key = format!("{kind} {blob}");
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    (!keys.is_empty()).then(|| keys.join("\n"))
}

/// `ServerSetting::sshHostKeyFingerprints()`: the SHA256 fingerprints of host key lines, in the
/// format of `ssh-keygen -lf`.
pub fn ssh_host_key_fingerprints(keys: Option<&str>) -> Vec<String> {
    keys.unwrap_or_default()
        .split('\n')
        .filter(|line| !line.is_empty() && *line != "0")
        .map(|line| {
            let (kind, blob) = line.split_once(' ').unwrap_or((line, ""));
            let digest = Sha256::digest(decode_strict(blob).unwrap_or_default());
            format!(
                "SHA256:{} ({kind})",
                base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
            )
        })
        .collect()
}

/// `hash('sha256', $text)`.
pub fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `knownHostsFile()`: the file name has a hash of the content, so a process that loaded an older
/// key cannot overwrite the file of the current key.
pub fn known_hosts_file(
    directory: &Path,
    server_uuid: &str,
    pinned_key: &str,
) -> (PathBuf, String) {
    let alias = host_key_alias(server_uuid);
    let content: String = pinned_key
        .split('\n')
        .filter(|key| !key.is_empty() && *key != "0")
        .map(|key| format!("{alias} {key}\n"))
        .collect();
    let hash = sha256_hex(&content);
    (
        directory.join(format!("known_hosts_{server_uuid}_{}", &hash[..16])),
        content,
    )
}

/// `syncKnownHostsFile()`: writes the known_hosts file of the server from the pinned key.
pub fn sync_known_hosts_file(
    directory: &Path,
    server_uuid: &str,
    pinned_key: &str,
) -> std::io::Result<PathBuf> {
    use std::io::Write as _;
    let (path, content) = known_hosts_file(directory, server_uuid, pinned_key);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(content.as_str()) {
        // NamedTempFile creates the file with mode 0600; the rename replaces the old file at once.
        let mut temporary = tempfile::Builder::new()
            .prefix(&format!(
                "{}.",
                path.file_name().unwrap_or_default().to_string_lossy()
            ))
            .rand_bytes(8)
            .tempfile_in(directory)?;
        temporary.write_all(content.as_bytes())?;
        temporary.flush()?;
        temporary.persist(&path).map_err(|e| e.error)?;
    }
    Ok(path)
}

/// The server uuid of a `known_hosts_<uuid>_<16 hex>` file name.
pub(crate) fn known_hosts_file_owner(file_name: &str) -> Option<&str> {
    let rest = file_name.strip_prefix("known_hosts_")?;
    let (uuid, hash) = rest.rsplit_once('_')?;
    (hash.len() == 16
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && !uuid.is_empty())
    .then_some(uuid)
}

/// `deleteStaleKnownHostsFiles()`: deletes the known_hosts files of old host keys of a server.
/// Pass `None` to delete every file of the server.
pub fn delete_stale_known_hosts_files(
    directory: &Path,
    server_uuid: &str,
    pinned_key: Option<&str>,
) {
    let keep = pinned_key
        .filter(|k| !k.is_empty())
        .map(|k| known_hosts_file(directory, server_uuid, k).0);
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if known_hosts_file_owner(&name.to_string_lossy()) == Some(server_uuid)
            && Some(entry.path()) != keep
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// `isHostKeyMismatch()`: true when ssh reports that the server sent a host key that does not
/// match the pinned key.
pub fn is_host_key_mismatch(error_output: &str) -> bool {
    error_output.contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
        || error_output.contains("Host key verification failed")
}

/// The condition of `noticeHostKeyMismatch()`: the message must name the host key alias or the
/// known_hosts file of the server, because a program on the server (git in a deployment) can
/// print the same text for its own SSH connection.
pub fn mismatch_names_server(error_output: &str, server_uuid: &str) -> bool {
    is_host_key_mismatch(error_output)
        && (error_output.contains(&host_key_alias(server_uuid))
            || error_output.contains(&format!("/known_hosts_{server_uuid}_")))
}

/// `stripHostKeyChangedWarning()`: removes the changed host key warning of OpenSSH from the start
/// of the stderr of an SSH process. A warning later in the output is from a program on the
/// server and stays.
pub fn strip_host_key_changed_warning(output: &str) -> String {
    match HOST_KEY_CHANGED_WARNING.find(output) {
        Some(m) => output[m.end()..].to_owned(),
        None => output.to_owned(),
    }
}

/// `mayBeHostKeyChangedWarning()`.
fn may_be_host_key_changed_warning(output: &str) -> bool {
    if output.is_empty() {
        return true;
    }
    if !output.starts_with('@') || output.len() > HOST_KEY_CHANGED_WARNING_MAX_BYTES {
        return false;
    }
    let Some(m) = HOST_KEY_CHANGED_WARNING.find(output) else {
        return true;
    };
    // The rest of the warning can follow until a complete line arrives that is not part of it.
    let rest = &output[m.end()..];
    !(rest.contains('\n') && !rest.starts_with("  remove with:"))
}

/// `filterHostKeyChangedWarning()`: filters streamed stderr of an SSH process. It holds the first
/// chunks while they can still be the changed host key warning, then returns them without it.
#[derive(Debug, Default)]
pub struct HostKeyWarningFilter {
    /// `None` when the start of the output is passed.
    held: Option<String>,
}

impl HostKeyWarningFilter {
    pub fn new() -> Self {
        Self {
            held: Some(String::new()),
        }
    }

    /// Pass `last` when the warning must be over (the process ended).
    pub fn push(&mut self, chunk: &str, last: bool) -> String {
        let Some(held) = self.held.as_mut() else {
            return chunk.to_owned();
        };
        held.push_str(chunk);
        if !last && may_be_host_key_changed_warning(held) {
            return String::new();
        }
        let output = strip_host_key_changed_warning(held);
        self.held = None;
        output
    }
}

/// `Str::limit($value, $limit)`: the first `limit` characters and `...` when the text is longer.
pub fn str_limit(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    let cut: String = value.chars().take(limit).collect();
    format!("{}...", cut.trim_end())
}

/// The result of `readHostKey()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyScan {
    /// The keys as "type base64" lines; `None` when the server sent none.
    pub key: Option<String>,
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    // An Ed25519 public key blob: the length-prefixed type name and 32 key bytes.
    fn blob(kind: &str, fill: u8) -> String {
        let mut raw = Vec::new();
        raw.extend_from_slice(&(kind.len() as u32).to_be_bytes());
        raw.extend_from_slice(kind.as_bytes());
        raw.extend_from_slice(&32u32.to_be_bytes());
        raw.extend_from_slice(&[fill; 32]);
        base64::engine::general_purpose::STANDARD.encode(raw)
    }

    const WARNING: &str = "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\nIT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!\nSomeone could be eavesdropping on you right now (man-in-the-middle attack)!\nIt is also possible that a host key has just been changed.\nThe fingerprint for the ED25519 key sent by the remote host is\nSHA256:P/QnH16tWXGbHRqp4+IUWllIA/aw5lP/GZvdfsdVilk.\nPlease contact your system administrator.\nAdd correct host key in /data/ssh/known_hosts/known_hosts_abc_0123456789abcdef to get rid of this message.\nOffending ED25519 key in /data/ssh/known_hosts/known_hosts_abc_0123456789abcdef:1\n";
    const DISABLED: &str = "Password authentication is disabled to avoid man-in-the-middle attacks.\nKeyboard-interactive authentication is disabled to avoid man-in-the-middle attacks.\nUpdateHostkeys is disabled because the host key is not trusted.\n";

    #[test]
    fn instance_and_server_settings_decide_the_mode() {
        assert_eq!(HostKeyMode::parse(" Enforce "), Some(HostKeyMode::Enforce));
        assert_eq!(HostKeyMode::parse("strict"), None);
        assert_eq!(
            HostKeyMode::Warn.for_server(false, false),
            HostKeyMode::Warn
        );
        assert_eq!(
            HostKeyMode::Warn.for_server(false, true),
            HostKeyMode::Enforce
        );
        assert_eq!(HostKeyMode::Warn.for_server(true, true), HostKeyMode::Off);
        assert_eq!(
            HostKeyMode::Enforce.for_server(false, false),
            HostKeyMode::Enforce
        );
        assert_eq!(
            HostKeyMode::Enforce.for_server(true, false),
            HostKeyMode::Off
        );
        // A server cannot be stricter than an instance "off".
        assert_eq!(HostKeyMode::Off.for_server(false, true), HostKeyMode::Off);
    }

    #[test]
    fn normalizes_public_key_and_known_hosts_lines() {
        let ed = blob("ssh-ed25519", 1);
        let rsa = blob("ssh-rsa", 2);
        let input = format!(
            "# comment\n@revoked * ssh-ed25519 {ed}\n\nssh-ed25519 {ed} root@host\r\n[203.0.113.7]:2222 ssh-rsa {rsa}\ncoolify-abc ssh-ed25519 {ed}\nssh-rsa {ed}\nssh-ed25519 not-base64!\nnothing here\n"
        );
        assert_eq!(
            normalize_ssh_host_keys(Some(&input)),
            Some(format!("ssh-ed25519 {ed}\nssh-rsa {rsa}"))
        );
        assert_eq!(normalize_ssh_host_keys(Some("")), None);
        assert_eq!(normalize_ssh_host_keys(None), None);
        // The type in the blob must be the type of the line.
        assert_eq!(
            normalize_ssh_host_keys(Some(&format!("ssh-rsa {ed}"))),
            None
        );
    }

    #[test]
    fn fingerprints_look_like_ssh_keygen() {
        let ed = blob("ssh-ed25519", 1);
        let prints = ssh_host_key_fingerprints(Some(&format!("ssh-ed25519 {ed}")));
        assert_eq!(prints.len(), 1);
        assert!(prints[0].starts_with("SHA256:"), "{prints:?}");
        assert!(prints[0].ends_with(" (ssh-ed25519)"), "{prints:?}");
        // 32 bytes are 43 base64 characters without padding.
        assert_eq!(
            prints[0].len(),
            "SHA256:".len() + 43 + " (ssh-ed25519)".len()
        );
        assert!(!prints[0].contains('='));
        assert!(ssh_host_key_fingerprints(None).is_empty());
    }

    #[test]
    fn known_hosts_file_is_named_by_content() {
        let dir = Path::new("/d/ssh/known_hosts");
        let (path, content) = known_hosts_file(dir, "abc", "ssh-ed25519 AAAA\nssh-rsa BBBB");
        assert_eq!(
            content,
            "coolify-abc ssh-ed25519 AAAA\ncoolify-abc ssh-rsa BBBB\n"
        );
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        assert_eq!(known_hosts_file_owner(&name), Some("abc"));
        assert_eq!(
            name,
            format!("known_hosts_abc_{}", &sha256_hex(&content)[..16])
        );
        let (other, _) = known_hosts_file(dir, "abc", "ssh-ed25519 CCCC");
        assert_ne!(path, other);
        assert_eq!(known_hosts_file_owner("scan_abc_x1y2z3a4"), None);
        assert_eq!(known_hosts_file_owner("known_hosts_abc_XYZ"), None);
    }

    #[test]
    fn syncs_and_deletes_known_hosts_files() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let old = sync_known_hosts_file(dir.path(), "abc", "ssh-ed25519 AAAA")
            .unwrap_or_else(|e| panic!("{e}"));
        let new = sync_known_hosts_file(dir.path(), "abc", "ssh-ed25519 BBBB")
            .unwrap_or_else(|e| panic!("{e}"));
        let other = sync_known_hosts_file(dir.path(), "abcd", "ssh-ed25519 AAAA")
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            std::fs::read_to_string(&new).unwrap_or_default(),
            "coolify-abc ssh-ed25519 BBBB\n"
        );
        delete_stale_known_hosts_files(dir.path(), "abc", Some("ssh-ed25519 BBBB"));
        assert!(!old.exists());
        assert!(new.exists());
        assert!(other.exists());
        delete_stale_known_hosts_files(dir.path(), "abc", None);
        assert!(!new.exists());
        assert!(other.exists());
    }

    #[test]
    fn detects_a_mismatch_of_this_server_only() {
        let warn = format!("{WARNING}{DISABLED}");
        assert!(is_host_key_mismatch(&warn));
        assert!(mismatch_names_server(&warn, "abc"));
        assert!(!mismatch_names_server(&warn, "other"));
        assert!(mismatch_names_server(
            "No ED25519 host key is known for coolify-abc and you have requested strict checking.\nHost key verification failed.",
            "abc"
        ));
        // git on the server prints the same text for its own connection.
        assert!(!mismatch_names_server(
            "Host key verification failed.\nfatal: Could not read from remote repository.",
            "abc"
        ));
        assert!(!is_host_key_mismatch("Permission denied (publickey)."));
    }

    #[test]
    fn strips_the_warning_only_at_the_start() {
        let warn = format!("{WARNING}{DISABLED}real error\n");
        assert_eq!(strip_host_key_changed_warning(&warn), "real error\n");
        let with_hint = format!(
            "{WARNING}  remove with:\n  ssh-keygen -f '/x/known_hosts' -R 'coolify-abc'\n{DISABLED}after"
        );
        assert_eq!(strip_host_key_changed_warning(&with_hint), "after");
        let later = format!("first line\n{WARNING}");
        assert_eq!(strip_host_key_changed_warning(&later), later);
        let crlf = WARNING.replace('\n', "\r\n");
        assert_eq!(strip_host_key_changed_warning(&crlf), "");
        // Enforce mode: the lines after the warning stay.
        let enforce = format!(
            "{WARNING}Host key for coolify-abc has changed and you have requested strict checking.\nHost key verification failed.\n"
        );
        assert_eq!(
            strip_host_key_changed_warning(&enforce),
            "Host key for coolify-abc has changed and you have requested strict checking.\nHost key verification failed.\n"
        );
    }

    #[test]
    fn filter_holds_the_start_of_stderr_until_it_is_decided() {
        // Other output passes at once.
        let mut f = HostKeyWarningFilter::new();
        assert_eq!(f.push("plain error\n", false), "plain error\n");
        assert_eq!(f.push("@ later\n", false), "@ later\n");

        // The warning arrives in pieces and is removed.
        let mut f = HostKeyWarningFilter::new();
        let (a, b) = WARNING.split_at(100);
        assert_eq!(f.push(a, false), "");
        assert_eq!(f.push(b, false), "");
        assert_eq!(f.push(DISABLED, false), "");
        assert_eq!(f.push("docker: not found\n", false), "docker: not found\n");
        assert_eq!(f.push("more\n", false), "more\n");

        // Output that only starts like the warning is released when the process ends.
        let mut f = HostKeyWarningFilter::new();
        assert_eq!(f.push("@@@@\n", false), "");
        assert_eq!(f.push("", true), "@@@@\n");

        // Nothing is held without limit.
        let mut f = HostKeyWarningFilter::new();
        let long = "@".repeat(HOST_KEY_CHANGED_WARNING_MAX_BYTES + 1);
        assert_eq!(f.push(&long, false), long);

        // A held warning without more output ends as nothing.
        let mut f = HostKeyWarningFilter::new();
        assert_eq!(f.push(WARNING, false), "");
        assert_eq!(f.push("", true), "");
    }

    #[test]
    fn limits_text_like_laravel() {
        assert_eq!(str_limit("short", 300), "short");
        assert_eq!(str_limit("abcdef", 3), "abc...");
        assert_eq!(str_limit("ab cdef", 3), "ab...");
    }
}
