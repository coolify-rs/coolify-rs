//! Running scripts on a server through the system `ssh` binary.
//!
//! Port of upstream `instant_remote_process` with the controls from ADR 0009:
//! no local shell, a cleared environment, ephemeral key files, pinned host
//! keys, deadlines and output caps.

use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

use crate::retry::{DEFAULT_MAX_ATTEMPTS, is_retryable_failure, retry_delay};
use crate::sudo::parse_commands_by_line_for_sudo;
use crate::text::{command_output, php_trim, sanitize_utf8};

/// sshd runs this with the user's login shell, which may not be POSIX; the
/// script itself goes to stdin.
const REMOTE_SHELL: &str =
    r#"sh -c "if command -v bash >/dev/null 2>&1; then exec bash -se; else exec sh -se; fi""#;
/// Upstream `constants.ssh.connection_timeout` and `server_interval`.
const DEFAULT_CONNECT_TIMEOUT: i64 = 10;
const SERVER_ALIVE_INTERVAL: u32 = 20;
const STDOUT_CAP: usize = 16 * 1024 * 1024;
const STDERR_CAP: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyPolicy {
    /// Trust on first use, pinned per server (default).
    Pin,
    /// Upstream behaviour: `StrictHostKeyChecking=no`, no known_hosts.
    Ignore,
}

#[derive(Debug, Clone)]
pub struct RemoteConfig {
    pub data_dir: PathBuf,
    pub ssh_binary: PathBuf,
    pub host_keys: HostKeyPolicy,
    pub max_concurrency: usize,
    /// Upstream `constants.ssh.command_timeout` (`SSH_COMMAND_TIMEOUT`).
    pub command_timeout: Duration,
}

impl RemoteConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            ssh_binary: PathBuf::from("ssh"),
            host_keys: HostKeyPolicy::Pin,
            max_concurrency: 32,
            command_timeout: Duration::from_secs(3600),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("the data directory may only contain letters, digits, '/', '.', '_' and '-': {0}")]
    UnsafeDataDir(PathBuf),
    #[error("cannot prepare {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// A decrypted private key. Zeroized on drop; never printed.
pub struct PrivateKey {
    uuid: String,
    pem: Zeroizing<String>,
}

impl PrivateKey {
    /// `uuid` is the `private_keys.uuid`; errors name the key `ssh_key@<uuid>` as upstream's file is named.
    pub fn new(uuid: impl Into<String>, pem: String) -> Self {
        Self {
            uuid: uuid.into(),
            pem: Zeroizing::new(pem),
        }
    }
}

impl fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PrivateKey(<redacted>)")
    }
}

/// Where and how to connect. Mirrors the `Server` fields upstream reads.
#[derive(Debug)]
pub struct Target {
    pub server_uuid: String,
    pub host: String,
    pub port: i64,
    pub user: String,
    pub key: PrivateKey,
    /// `server_settings.connection_timeout`.
    pub connection_timeout: Option<i64>,
    pub cloudflare_tunnel: bool,
    pub force_disabled: bool,
}

impl Target {
    pub fn is_non_root(&self) -> bool {
        self.user != "root"
    }
}

/// Per-call options of `instant_remote_process`.
#[derive(Debug, Clone, Copy)]
pub struct RunOptions {
    /// Fixed name for the audit log; scripts are never logged.
    pub op: &'static str,
    pub timeout: Option<Duration>,
    pub max_attempts: Option<u32>,
    pub no_sudo: bool,
}

impl RunOptions {
    pub const fn op(op: &'static str) -> Self {
        Self {
            op,
            timeout: None,
            max_attempts: None,
            no_sudo: false,
        }
    }
    pub const fn attempts(mut self, n: u32) -> Self {
        self.max_attempts = Some(n);
        self
    }
    pub const fn no_sudo(mut self) -> Self {
        self.no_sudo = true;
        self
    }
    pub const fn timeout(mut self, t: Duration) -> Self {
        self.timeout = Some(t);
        self
    }
}

/// What upstream throws: the message users see and the exit code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct RemoteError {
    pub message: String,
    pub code: Option<i32>,
}

impl RemoteError {
    fn new(message: impl Into<String>, code: Option<i32>) -> Self {
        Self {
            message: message.into(),
            code,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Remote {
    cfg: Arc<RemoteConfig>,
    permits: Arc<Semaphore>,
}

fn safe_path_chars(p: &Path) -> bool {
    p.to_str().is_some_and(|s| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
    })
}

fn private_dir(path: PathBuf) -> Result<PathBuf, SetupError> {
    use std::os::unix::fs::PermissionsExt;
    let io = |source| SetupError::Io {
        path: path.clone(),
        source,
    };
    std::fs::create_dir_all(&path).map_err(io)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).map_err(io)?;
    Ok(path)
}

impl Remote {
    /// Prepares the private directories and removes key files a crash left behind.
    pub fn new(cfg: RemoteConfig) -> Result<Self, SetupError> {
        // ssh expands `%` tokens and splits on whitespace in option values.
        if !safe_path_chars(&cfg.data_dir) {
            return Err(SetupError::UnsafeDataDir(cfg.data_dir.clone()));
        }
        let ssh = private_dir(cfg.data_dir.join("ssh"))?;
        let keys = private_dir(ssh.join("keys"))?;
        private_dir(ssh.join("known_hosts"))?;
        private_dir(ssh.join("home"))?;
        if let Ok(entries) = std::fs::read_dir(&keys) {
            for e in entries.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
        let permits = Arc::new(Semaphore::new(cfg.max_concurrency.max(1)));
        Ok(Self {
            cfg: Arc::new(cfg),
            permits,
        })
    }

    fn ssh_dir(&self) -> PathBuf {
        self.cfg.data_dir.join("ssh")
    }

    fn known_hosts(&self, server_uuid: &str) -> Option<PathBuf> {
        let ok = !server_uuid.is_empty()
            && server_uuid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        ok.then(|| self.ssh_dir().join("known_hosts").join(server_uuid))
    }

    /// Drops a server's pinned host key (IP or port changed, server deleted).
    pub fn forget_host_key(&self, server_uuid: &str) {
        if let Some(p) = self.known_hosts(server_uuid) {
            let _ = std::fs::remove_file(p);
        }
    }

    /// Drops the pins of servers that no longer exist, for example servers
    /// deleted by upstream Coolify on a shared database. Returns how many.
    pub fn retain_host_keys(&self, live_server_uuids: &std::collections::HashSet<String>) -> usize {
        let Ok(entries) = std::fs::read_dir(self.ssh_dir().join("known_hosts")) else {
            return 0;
        };
        entries
            .flatten()
            .filter(|e| !live_server_uuids.contains(e.file_name().to_string_lossy().as_ref()))
            .filter(|e| std::fs::remove_file(e.path()).is_ok())
            .count()
    }

    /// `instant_remote_process($commands, $server, throwError: true, ...)`.
    pub async fn run(
        &self,
        target: &Target,
        commands: &[&str],
        opts: RunOptions,
    ) -> Result<Option<String>, RemoteError> {
        let mut lines: Vec<String> = commands.iter().map(|c| (*c).to_owned()).collect();
        if target.is_non_root() && !opts.no_sudo {
            lines = parse_commands_by_line_for_sudo(&lines, &target.user);
        }
        let script = Zeroizing::new(lines.join("\n"));
        let timeout = opts.timeout.unwrap_or(self.cfg.command_timeout);
        let attempts = opts.max_attempts.unwrap_or(DEFAULT_MAX_ATTEMPTS);

        let mut last = RemoteError::new("SSH command failed with exit code: ", None);
        for attempt in 0..attempts {
            match self
                .run_once(target, &script, timeout, opts.op, attempt + 1)
                .await
            {
                Ok(stdout) => return Ok(command_output(&stdout)),
                Err(e) => {
                    // A changed host key does not fix itself between attempts.
                    let retry = is_retryable_failure(e.code, &e.message)
                        && !e.message.starts_with(HOST_KEY_CHANGED)
                        && attempt + 1 < attempts;
                    last = e;
                    if !retry {
                        break;
                    }
                    tokio::time::sleep(retry_delay(attempt)).await;
                }
            }
        }
        Err(last)
    }

    /// `instant_remote_process(..., throwError: false)`: failures become no output.
    pub async fn run_quiet(
        &self,
        target: &Target,
        commands: &[&str],
        opts: RunOptions,
    ) -> Option<String> {
        self.run(target, commands, opts).await.ok().flatten()
    }

    fn check_target(&self, t: &Target) -> Result<PathBuf, RemoteError> {
        if t.force_disabled {
            return Err(RemoteError::new("Server is disabled.", Some(0)));
        }
        let bad = |s: &str| {
            s.is_empty()
                || s.starts_with('-')
                || s.chars().any(|c| c.is_whitespace() || c.is_control())
        };
        if bad(&t.host)
            || !t
                .host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".:-_%".contains(c))
        {
            return Err(RemoteError::new("Invalid server address.", None));
        }
        // Upstream's API rule for the user; the name also ends up in remote `chown` commands.
        if bad(&t.user)
            || !t
                .user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(RemoteError::new("Invalid SSH user.", None));
        }
        if !(1..=65535).contains(&t.port) {
            return Err(RemoteError::new("Invalid SSH port.", None));
        }
        self.known_hosts(&t.server_uuid)
            .ok_or_else(|| RemoteError::new("Invalid server.", None))
    }

    fn write_key(&self, key: &PrivateKey) -> Result<tempfile::NamedTempFile, RemoteError> {
        let fail = |e: std::io::Error| {
            RemoteError::new(format!("Failed to write SSH key to filesystem: {e}"), None)
        };
        // NamedTempFile creates the file with O_EXCL and mode 0600 on Unix.
        let mut file = tempfile::Builder::new()
            .prefix("key-")
            .rand_bytes(16)
            .tempfile_in(self.ssh_dir().join("keys"))
            .map_err(fail)?;
        file.write_all(key.pem.as_bytes()).map_err(fail)?;
        if !key.pem.ends_with('\n') {
            file.write_all(b"\n").map_err(fail)?;
        }
        file.flush().map_err(fail)?;
        Ok(file)
    }

    fn command(&self, t: &Target, key_path: &Path, known_hosts: &Path) -> Command {
        let connect_timeout = t
            .connection_timeout
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_CONNECT_TIMEOUT);
        let mut cmd = Command::new(&self.cfg.ssh_binary);
        cmd.env_clear()
            .env(
                "PATH",
                std::env::var_os("PATH").unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".into()),
            )
            .env("HOME", self.ssh_dir().join("home"))
            .arg("-F")
            .arg("/dev/null")
            .arg("-i")
            .arg(key_path);
        let mut opt = |o: String| {
            cmd.arg("-o").arg(o);
        };
        match self.cfg.host_keys {
            HostKeyPolicy::Pin => {
                opt("StrictHostKeyChecking=accept-new".into());
                opt(format!("UserKnownHostsFile={}", known_hosts.display()));
            }
            HostKeyPolicy::Ignore => {
                opt("StrictHostKeyChecking=no".into());
                opt("UserKnownHostsFile=/dev/null".into());
            }
        }
        for o in [
            "GlobalKnownHostsFile=/dev/null",
            "UpdateHostKeys=no",
            "PasswordAuthentication=no",
            "BatchMode=yes",
            "IdentitiesOnly=yes",
            "IdentityAgent=none",
            "ForwardAgent=no",
            "ForwardX11=no",
            "ClearAllForwardings=yes",
            "PermitLocalCommand=no",
            "ControlMaster=no",
            "ControlPath=none",
            "RequestTTY=no",
            "LogLevel=ERROR",
        ] {
            opt(o.into());
        }
        opt(format!("ConnectTimeout={connect_timeout}"));
        opt(format!("ServerAliveInterval={SERVER_ALIVE_INTERVAL}"));
        if t.cloudflare_tunnel {
            opt("ProxyCommand=cloudflared access ssh --hostname %h".into());
        }
        cmd.arg("-l")
            .arg(&t.user)
            .arg("-p")
            .arg(t.port.to_string())
            .arg("--")
            .arg(&t.host)
            .arg(REMOTE_SHELL)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }

    async fn run_once(
        &self,
        t: &Target,
        script: &str,
        timeout: Duration,
        op: &'static str,
        attempt: u32,
    ) -> Result<Vec<u8>, RemoteError> {
        let known_hosts = self.check_target(t)?;
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| RemoteError::new("Remote execution is shutting down.", None))?;
        let key = self.write_key(&t.key)?;
        let started = Instant::now();

        let mut child = self
            .command(t, key.path(), &known_hosts)
            .spawn()
            .map_err(|e| RemoteError::new(format!("Failed to start ssh: {e}"), None))?;
        let (Some(mut stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(RemoteError::new("Failed to start ssh: missing pipes", None));
        };

        let io = async {
            let write = async {
                // The remote side may exit before reading everything.
                let _ = stdin.write_all(script.as_bytes()).await;
                let _ = stdin.write_all(b"\n").await;
                drop(stdin);
            };
            let (_, out, err) = tokio::join!(
                write,
                read_capped(stdout, STDOUT_CAP),
                read_capped(stderr, STDERR_CAP)
            );
            let status = child.wait().await;
            (status, out, err)
        };
        let outcome = tokio::time::timeout(timeout, io).await;
        let key_path = key.path().display().to_string();
        drop(key);

        let result = match outcome {
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                Err(RemoteError::new(
                    format!(
                        "The command on the server timed out after {} seconds.",
                        timeout.as_secs()
                    ),
                    Some(124),
                ))
            }
            Ok((Err(e), _, _)) => Err(RemoteError::new(format!("Failed to run ssh: {e}"), None)),
            Ok((Ok(status), out, err)) => {
                let code = exit_code(status);
                if code == Some(0) {
                    Ok(out)
                } else {
                    // ssh names the identity file in some errors; it is a temporary path here.
                    let stderr =
                        sanitize_utf8(&err).replace(&key_path, &format!("ssh_key@{}", t.key.uuid));
                    let msg = php_trim(&stderr);
                    let msg = if let Some(changed) = host_key_changed(msg) {
                        changed
                    } else if msg.is_empty() {
                        format!(
                            "SSH command failed with exit code: {}",
                            code.map(|c| c.to_string()).unwrap_or_default()
                        )
                    } else {
                        msg.to_owned()
                    };
                    Err(RemoteError::new(msg, code))
                }
            }
        };
        tracing::info!(
            target: "audit",
            server_uuid = %t.server_uuid,
            op,
            attempt,
            exit_code = result.as_ref().map_or_else(|e| e.code, |_| Some(0)),
            duration_ms = started.elapsed().as_millis() as u64,
            "remote command"
        );
        result
    }
}

pub(crate) const HOST_KEY_CHANGED: &str =
    "Host key verification failed: the server's SSH host key changed";

/// OpenSSH's "REMOTE HOST IDENTIFICATION HAS CHANGED" banner names the pin
/// file (a path on the control plane) and gives no way forward in Coolify.
/// Upstream never pins keys, so this message is coolify-rs's own.
fn host_key_changed(stderr: &str) -> Option<String> {
    if !stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED") {
        return None;
    }
    let fingerprint = stderr
        .lines()
        .skip_while(|l| !l.starts_with("The fingerprint for the "))
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    let offered = fingerprint
        .strip_prefix("The fingerprint for the ")
        .and_then(|rest| {
            let (kind, after) = rest.split_once(" key sent by the remote host is")?;
            Some(format!(
                " It now offers this {kind} key: {}",
                after.trim().trim_end_matches('.')
            ))
        })
        .unwrap_or_default();
    Some(format!(
        "{HOST_KEY_CHANGED} since coolify-rs first connected to it.{offered}. If you reinstalled the server or replaced its SSH host keys, re-enter its IP address or port to accept the new key. Otherwise someone may be intercepting the connection."
    ))
}

/// Symfony Process reports a signal as `128 + signal`.
fn exit_code(status: std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.code().or_else(|| status.signal().map(|s| 128 + s))
}

/// Reads up to `cap` bytes and discards the rest, so the child never blocks on a full pipe.
async fn read_capped(mut r: impl AsyncRead + Unpin, cap: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => return out,
            Ok(n) => {
                let room = cap.saturating_sub(out.len());
                out.extend_from_slice(&buf[..n.min(room)]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::host_key_changed;

    #[test]
    fn rewrites_the_changed_host_key_banner() {
        // OpenSSH 10.3 output for a pinned key that changed.
        let stderr = "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\nIT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!\nSomeone could be eavesdropping on you right now (man-in-the-middle attack)!\nIt is also possible that a host key has just been changed.\nThe fingerprint for the ED25519 key sent by the remote host is\nSHA256:P/QnH16tWXGbHRqp4+IUWllIA/aw5lP/GZvdfsdVilk.\nPlease contact your system administrator.\nAdd correct host key in /data/coolify-rs/ssh/known_hosts/abc to get rid of this message.\nOffending ED25519 key in /data/coolify-rs/ssh/known_hosts/abc:1\nHost key for h.example has changed and you have requested strict checking.\nHost key verification failed.";
        let msg = host_key_changed(stderr).unwrap_or_default();
        assert_eq!(
            msg,
            "Host key verification failed: the server's SSH host key changed since coolify-rs first connected to it. It now offers this ED25519 key: SHA256:P/QnH16tWXGbHRqp4+IUWllIA/aw5lP/GZvdfsdVilk. If you reinstalled the server or replaced its SSH host keys, re-enter its IP address or port to accept the new key. Otherwise someone may be intercepting the connection."
        );
        assert!(!msg.contains("/data/"));
        assert_eq!(host_key_changed("Permission denied (publickey)."), None);
    }
}
