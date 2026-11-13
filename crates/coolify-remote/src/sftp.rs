//! SFTP to a backup storage (Coolify v4.7.0).
//!
//! Port of upstream's `PinnedSftpConnectionProvider` and `SftpFilesystemAdapter`, which use
//! phpseclib inside PHP. Here the system `ssh` binary opens the `sftp` subsystem and this module
//! speaks the SFTP protocol (version 3) on its pipes, so the login, the ciphers and the host key
//! check are OpenSSH's. A saved host key is checked on every connection (trust on first use);
//! without one, the key that the server presents is recorded so the caller can save it.

use std::ffi::OsString;
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use zeroize::Zeroizing;

use crate::exec::Remote;
use crate::text::{php_trim, sanitize_utf8};

/// The name the host key is saved under in the temporary known_hosts file (`HostKeyAlias`), so
/// the lookup does not depend on the address or the port.
const HOST_ALIAS: &str = "coolify-backup-storage";
/// The largest packet a server may send: a read of [`READ_CHUNK`] bytes or a directory page.
const MAX_PACKET: usize = 1024 * 1024;
/// The bytes one `SSH_FXP_READ` asks for. Every SFTP server accepts 32 KiB.
pub const READ_CHUNK: u32 = 32 * 1024;

const FXP_INIT: u8 = 1;
const FXP_VERSION: u8 = 2;
const FXP_OPEN: u8 = 3;
const FXP_CLOSE: u8 = 4;
const FXP_READ: u8 = 5;
const FXP_OPENDIR: u8 = 11;
const FXP_READDIR: u8 = 12;
const FXP_REMOVE: u8 = 13;
const FXP_RMDIR: u8 = 15;
const FXP_STAT: u8 = 17;
const FXP_STATUS: u8 = 101;
const FXP_HANDLE: u8 = 102;
const FXP_DATA: u8 = 103;
const FXP_NAME: u8 = 104;
const FXP_ATTRS: u8 = 105;

const FX_OK: u32 = 0;
const FX_EOF: u32 = 1;
/// `SSH_FX_NO_SUCH_FILE`.
pub const FX_NO_SUCH_FILE: u32 = 2;

const ATTR_SIZE: u32 = 0x0000_0001;
const ATTR_UIDGID: u32 = 0x0000_0002;
const ATTR_PERMISSIONS: u32 = 0x0000_0004;
const ATTR_ACMODTIME: u32 = 0x0000_0008;
const ATTR_EXTENDED: u32 = 0x8000_0000;
const S_IFMT: u32 = 0o170_000;
const S_IFDIR: u32 = 0o040_000;

/// Where and how to log in. Mirrors the arguments of `PinnedSftpConnectionProvider::create()`.
pub struct SftpTarget {
    /// The host name, for messages.
    pub host: String,
    /// The IP to connect to (the validated address of the host).
    pub address: String,
    pub port: u16,
    pub user: String,
    /// Used when there is no private key.
    pub password: Option<Zeroizing<String>>,
    pub private_key: Option<Zeroizing<String>>,
    /// The saved host key (`ssh-ed25519 AAAA...`). `None`: any key is accepted and recorded.
    pub pinned_host_key: Option<String>,
    /// `S3Storage::CONNECTION_TIMEOUT_SECONDS`.
    pub timeout: Duration,
}

impl fmt::Debug for SftpTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SftpTarget")
            .field("host", &self.host)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

/// Why an SFTP connection or request failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SftpError {
    /// The network connection failed (refused, timed out, no route): the message of ssh.
    /// Upstream: phpseclib's `UnableToConnectException`.
    Connect(String),
    /// The server presented another key than the saved one (`SftpHostKeyChanged`).
    HostKeyChanged { host: String },
    /// The login was refused (Flysystem's `UnableToAuthenticate`).
    Auth { with_private_key: bool },
    /// The server answered a request with an error status.
    Status { code: u32, message: String },
    /// ssh or the server did something else.
    Other(String),
}

impl SftpError {
    /// `SSH_FX_NO_SUCH_FILE`.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Status { code, .. } if *code == FX_NO_SUCH_FILE)
    }
}

impl fmt::Display for SftpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(message) | Self::Other(message) => f.write_str(message),
            // `SftpHostKeyChanged::atHost()`.
            Self::HostKeyChanged { host } => write!(
                f,
                "The SSH host key of the SFTP server {host} changed. If you expect this change, for example after a reinstall of the server, reset the host key in the storage settings and validate the connection again."
            ),
            // `UnableToAuthenticate::withPrivateKey()` / `withPassword()`.
            Self::Auth {
                with_private_key: true,
            } => f.write_str("Unable to authenticate using a private key."),
            Self::Auth {
                with_private_key: false,
            } => f.write_str("Unable to authenticate using a password."),
            Self::Status { code, message } if message.is_empty() => {
                write!(f, "The SFTP server answered with status {code}.")
            }
            Self::Status { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for SftpError {}

/// The attributes of a file (`SSH_FXP_ATTRS`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SftpAttrs {
    pub size: Option<u64>,
    pub permissions: Option<u32>,
}

impl SftpAttrs {
    pub fn is_dir(&self) -> bool {
        self.permissions.is_some_and(|p| p & S_IFMT == S_IFDIR)
    }
}

/// `PinnedSftpConnectionProvider::hostKeyAlgorithms()`: ask for the type of the saved key, else a
/// server with several keys can present another one.
pub fn host_key_algorithms(host_key: &str) -> Vec<String> {
    let kind = host_key.split(' ').next().unwrap_or_default();
    if kind == "ssh-rsa" {
        vec![
            "rsa-sha2-512".to_owned(),
            "rsa-sha2-256".to_owned(),
            "ssh-rsa".to_owned(),
        ]
    } else {
        vec![kind.to_owned()]
    }
}

/// Whether a saved host key has the form `<key type> <base64>`.
pub fn valid_host_key(host_key: &str) -> bool {
    let mut parts = host_key.split(' ');
    let (Some(kind), Some(blob), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !kind.is_empty()
        && kind
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@.-".contains(&b))
        && !blob.is_empty()
        && blob
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
}

/// The key that ssh saved in the temporary known_hosts file on the first connection, as
/// `<key type> <base64>` (`presentedHostKey()`).
fn presented_host_key(known_hosts: &str) -> Option<String> {
    known_hosts.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let (name, kind, blob) = (parts.next()?, parts.next()?, parts.next()?);
        let key = format!("{kind} {blob}");
        (name == HOST_ALIAS && valid_host_key(&key)).then_some(key)
    })
}

/// What a failed start of ssh means, from its error output.
fn classify_failure(stderr: &str, t: &SftpTarget) -> SftpError {
    let text = php_trim(stderr);
    let lower = text.to_lowercase();
    if lower.contains("remote host identification has changed")
        || lower.contains("host key verification failed")
        || lower.contains("no matching host key type found")
    {
        return if t.pinned_host_key.is_some() {
            SftpError::HostKeyChanged {
                host: t.host.clone(),
            }
        } else {
            SftpError::Other("The SFTP server did not present a host key.".to_owned())
        };
    }
    if lower.contains("permission denied") || lower.contains("too many authentication failures") {
        return SftpError::Auth {
            with_private_key: t.private_key.is_some(),
        };
    }
    // The last line is the error; ssh prints warnings before it.
    let last = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default();
    let last = last.strip_prefix("ssh: ").unwrap_or(last);
    if [
        "connect to host",
        "could not resolve hostname",
        "connection timed out",
        "connection refused",
        "no route to host",
        "network is unreachable",
        "connection closed by",
        "connection reset by",
        "kex_exchange_identification",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return SftpError::Connect(last.to_owned());
    }
    SftpError::Other(if last.is_empty() {
        "Unable to connect to host".to_owned()
    } else {
        last.to_owned()
    })
}

/// The arguments of the `ssh` process that opens the `sftp` subsystem.
fn ssh_args(t: &SftpTarget, key: Option<&Path>, known_hosts: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["-F".into(), "/dev/null".into()];
    if let Some(key) = key {
        args.push("-i".into());
        args.push(key.into());
    }
    let mut opt = |o: String| {
        args.push("-o".into());
        args.push(o.into());
    };
    match &t.pinned_host_key {
        Some(pinned) => {
            opt("StrictHostKeyChecking=yes".into());
            opt(format!(
                "HostKeyAlgorithms={}",
                host_key_algorithms(pinned).join(",")
            ));
        }
        None => opt("StrictHostKeyChecking=accept-new".into()),
    }
    opt(format!("UserKnownHostsFile={}", known_hosts.display()));
    opt(format!("HostKeyAlias={HOST_ALIAS}"));
    for o in [
        "GlobalKnownHostsFile=/dev/null",
        "CheckHostIP=no",
        "HashKnownHosts=no",
        "UpdateHostKeys=no",
    ] {
        opt(o.into());
    }
    if key.is_some() {
        for o in [
            "PreferredAuthentications=publickey",
            "PasswordAuthentication=no",
            "KbdInteractiveAuthentication=no",
            "BatchMode=yes",
        ] {
            opt(o.into());
        }
    } else {
        // The password comes from the askpass program, never from a terminal.
        for o in [
            "PreferredAuthentications=password,keyboard-interactive",
            "PubkeyAuthentication=no",
            "NumberOfPasswordPrompts=1",
            "BatchMode=no",
        ] {
            opt(o.into());
        }
    }
    for o in [
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
        "ConnectionAttempts=1",
        "ServerAliveInterval=20",
    ] {
        opt(o.into());
    }
    opt(format!("ConnectTimeout={}", t.timeout.as_secs().max(1)));
    args.extend([
        "-l".into(),
        (&t.user).into(),
        "-p".into(),
        t.port.to_string().into(),
        "-s".into(),
        "--".into(),
        (&t.address).into(),
        "sftp".into(),
    ]);
    args
}

fn put_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_be_bytes());
}

fn put_bytes(buf: &mut Vec<u8>, v: &[u8]) {
    put_u32(buf, v.len() as u32);
    buf.extend_from_slice(v);
}

/// One packet: length, type, payload.
fn packet(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 5);
    put_u32(&mut out, payload.len() as u32 + 1);
    out.push(kind);
    out.extend_from_slice(payload);
    out
}

struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], SftpError> {
        if self.buf.len() < n {
            return Err(SftpError::Other(
                "The SFTP server sent a malformed packet.".to_owned(),
            ));
        }
        let (head, rest) = self.buf.split_at(n);
        self.buf = rest;
        Ok(head)
    }
    fn u32(&mut self) -> Result<u32, SftpError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, SftpError> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_be_bytes(a))
    }
    fn bytes(&mut self) -> Result<&'a [u8], SftpError> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    fn attrs(&mut self) -> Result<SftpAttrs, SftpError> {
        let flags = self.u32()?;
        let mut attrs = SftpAttrs::default();
        if flags & ATTR_SIZE != 0 {
            attrs.size = Some(self.u64()?);
        }
        if flags & ATTR_UIDGID != 0 {
            self.take(8)?;
        }
        if flags & ATTR_PERMISSIONS != 0 {
            attrs.permissions = Some(self.u32()?);
        }
        if flags & ATTR_ACMODTIME != 0 {
            self.take(8)?;
        }
        if flags & ATTR_EXTENDED != 0 {
            for _ in 0..self.u32()? {
                self.bytes()?;
                self.bytes()?;
            }
        }
        Ok(attrs)
    }
}

/// `SSH_FXP_STATUS` as a result: `Ok(true)` for OK, `Ok(false)` for EOF.
fn status(payload: &[u8]) -> Result<bool, SftpError> {
    let mut r = Reader { buf: payload };
    let code = r.u32()?;
    match code {
        FX_OK => Ok(true),
        FX_EOF => Ok(false),
        _ => Err(SftpError::Status {
            code,
            message: r.bytes().map(sanitize_utf8).unwrap_or_default(),
        }),
    }
}

/// The names of one `SSH_FXP_NAME` page.
fn names(payload: &[u8]) -> Result<Vec<String>, SftpError> {
    let mut r = Reader { buf: payload };
    let count = r.u32()?;
    let mut out = Vec::new();
    for _ in 0..count {
        let name = sanitize_utf8(r.bytes()?);
        r.bytes()?;
        r.attrs()?;
        out.push(name);
    }
    Ok(out)
}

/// An open SFTP connection. Dropping it ends the ssh process and removes its temporary files.
pub struct SftpSession {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    next_id: u32,
    timeout: Duration,
    presented_host_key: Option<String>,
    _files: Vec<tempfile::TempPath>,
}

impl fmt::Debug for SftpSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SftpSession")
    }
}

impl Remote {
    fn sftp_temp_file(
        &self,
        prefix: &str,
        content: &[u8],
    ) -> Result<tempfile::NamedTempFile, SftpError> {
        let fail = |e: std::io::Error| {
            SftpError::Other(format!("Failed to prepare the SFTP connection: {e}"))
        };
        // NamedTempFile creates the file with O_EXCL and mode 0600 on Unix.
        let mut file = tempfile::Builder::new()
            .prefix(prefix)
            .rand_bytes(16)
            .tempfile_in(self.key_dir())
            .map_err(fail)?;
        file.write_all(content).map_err(fail)?;
        file.flush().map_err(fail)?;
        Ok(file)
    }

    /// `PinnedSftpConnectionProvider::provideConnection()`: connects, checks or records the host
    /// key, logs in and opens the `sftp` subsystem.
    pub async fn sftp_connect(&self, t: &SftpTarget) -> Result<SftpSession, SftpError> {
        let bad = |s: &str| s.is_empty() || s.starts_with('-') || s.chars().any(|c| c.is_control());
        if bad(&t.address)
            || !t
                .address
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".:-_".contains(c))
        {
            return Err(SftpError::Other("Invalid SFTP server address.".to_owned()));
        }
        if bad(&t.user) {
            return Err(SftpError::Other("Invalid SFTP user.".to_owned()));
        }
        if t.pinned_host_key
            .as_deref()
            .is_some_and(|k| !valid_host_key(k))
        {
            return Err(SftpError::Other(
                "The saved SFTP host key is invalid. Reset the host key in the storage settings and validate the connection again."
                    .to_owned(),
            ));
        }
        let mut files: Vec<tempfile::TempPath> = Vec::new();
        let known_hosts = self
            .sftp_temp_file(
                "known-",
                t.pinned_host_key
                    .as_ref()
                    .map(|k| format!("{HOST_ALIAS} {k}\n"))
                    .unwrap_or_default()
                    .as_bytes(),
            )?
            .into_temp_path();
        let known_hosts_path: PathBuf = known_hosts.to_path_buf();
        files.push(known_hosts);

        let mut envs: Vec<(&str, OsString)> = Vec::new();
        let mut key_path: Option<PathBuf> = None;
        if let Some(pem) = &t.private_key {
            let mut content = pem.as_bytes().to_vec();
            if !content.ends_with(b"\n") {
                content.push(b'\n');
            }
            let key = self.sftp_temp_file("key-", &content)?.into_temp_path();
            key_path = Some(key.to_path_buf());
            files.push(key);
        } else {
            let password = t.password.as_ref().map(|p| p.as_str()).unwrap_or_default();
            let pass = self
                .sftp_temp_file("pass-", password.as_bytes())?
                .into_temp_path();
            // ssh runs this program for the password prompt and reads the password from its
            // output, so the password is never an argument or an environment variable.
            let script = format!(
                "#!/bin/sh\ncat {}\n",
                crate::shell::quote(&pass.display().to_string())
            );
            let askpass = self.sftp_temp_file("askpass-", script.as_bytes())?;
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(askpass.path(), std::fs::Permissions::from_mode(0o700))
                    .map_err(|e| {
                        SftpError::Other(format!("Failed to prepare the SFTP connection: {e}"))
                    })?;
            }
            // Closed before ssh runs it: a file that is open for writing cannot be executed.
            let askpass = askpass.into_temp_path();
            envs.push(("SSH_ASKPASS", askpass.as_os_str().to_owned()));
            envs.push(("SSH_ASKPASS_REQUIRE", "force".into()));
            // OpenSSH before 8.4 uses the askpass program only when DISPLAY is set.
            envs.push(("DISPLAY", "none:0".into()));
            files.push(pass);
            files.push(askpass);
        }

        let mut cmd = Command::new(self.ssh_binary());
        cmd.env_clear()
            .env(
                "PATH",
                std::env::var_os("PATH").unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".into()),
            )
            .env("HOME", self.ssh_home())
            .envs(envs)
            .args(ssh_args(t, key_path.as_deref(), &known_hosts_path))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| SftpError::Other(format!("Failed to start ssh: {e}")))?;
        let (Some(stdin), Some(stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(SftpError::Other(
                "Failed to start ssh: missing pipes".to_owned(),
            ));
        };
        let mut session = SftpSession {
            child,
            stdin,
            stdout,
            next_id: 0,
            timeout: t.timeout,
            presented_host_key: t.pinned_host_key.clone(),
            _files: files,
        };

        // The connection, the key exchange and the login happen before the first answer.
        let handshake = async {
            let mut init = Vec::new();
            put_u32(&mut init, 3);
            session.send(FXP_INIT, &init).await?;
            let (kind, _) = session.receive().await?;
            if kind == FXP_VERSION {
                Ok(())
            } else {
                Err(SftpError::Other(
                    "The SFTP server sent an unexpected answer.".to_owned(),
                ))
            }
        };
        let started = tokio::time::timeout(t.timeout * 2 + Duration::from_secs(5), handshake).await;
        match started {
            Ok(Ok(())) => {}
            failed => {
                let _ = session.child.start_kill();
                let _ = session.child.wait().await;
                let mut err = Vec::new();
                let _ = tokio::time::timeout(Duration::from_secs(2), stderr.read_to_end(&mut err))
                    .await;
                let text = sanitize_utf8(&err);
                return Err(match failed {
                    Err(_) if php_trim(&text).is_empty() => SftpError::Connect(format!(
                        "connect to host {} port {}: Connection timed out",
                        t.address, t.port
                    )),
                    Ok(Err(e @ SftpError::Other(_))) if php_trim(&text).is_empty() => e,
                    _ => classify_failure(&text, t),
                });
            }
        }
        // Nothing reads the error output after the login; ssh must not block on a full pipe.
        tokio::spawn(async move {
            let mut sink = [0u8; 4096];
            while matches!(stderr.read(&mut sink).await, Ok(n) if n > 0) {}
        });
        if t.pinned_host_key.is_none() {
            session.presented_host_key = std::fs::read_to_string(&known_hosts_path)
                .ok()
                .and_then(|text| presented_host_key(&text));
        }
        Ok(session)
    }
}

impl SftpSession {
    /// The host key that the server presented, as `<key type> <base64>`.
    pub fn presented_host_key(&self) -> Option<&str> {
        self.presented_host_key.as_deref()
    }

    async fn send(&mut self, kind: u8, payload: &[u8]) -> Result<(), SftpError> {
        self.stdin
            .write_all(&packet(kind, payload))
            .await
            .map_err(|e| SftpError::Other(format!("The SFTP connection closed: {e}")))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| SftpError::Other(format!("The SFTP connection closed: {e}")))
    }

    async fn receive(&mut self) -> Result<(u8, Vec<u8>), SftpError> {
        let closed =
            |e: std::io::Error| SftpError::Other(format!("The SFTP connection closed: {e}"));
        let mut head = [0u8; 5];
        self.stdout.read_exact(&mut head).await.map_err(closed)?;
        let length = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        if length == 0 || length > MAX_PACKET {
            return Err(SftpError::Other(
                "The SFTP server sent a malformed packet.".to_owned(),
            ));
        }
        let mut payload = vec![0u8; length - 1];
        self.stdout.read_exact(&mut payload).await.map_err(closed)?;
        Ok((head[4], payload))
    }

    /// One request and its answer, without the request id.
    async fn request(&mut self, kind: u8, fields: &[u8]) -> Result<(u8, Vec<u8>), SftpError> {
        self.next_id = self.next_id.wrapping_add(1);
        let id = self.next_id;
        let mut payload = Vec::with_capacity(fields.len() + 4);
        put_u32(&mut payload, id);
        payload.extend_from_slice(fields);
        let timeout = self.timeout.max(Duration::from_secs(1)) * 2;
        let exchange = async {
            self.send(kind, &payload).await?;
            self.receive().await
        };
        let (answer, body) = tokio::time::timeout(timeout, exchange)
            .await
            .map_err(|_| {
                SftpError::Other("The SFTP server did not answer in time.".to_owned())
            })??;
        let mut r = Reader { buf: &body };
        if r.u32()? != id {
            return Err(SftpError::Other(
                "The SFTP server sent an unexpected answer.".to_owned(),
            ));
        }
        Ok((answer, r.buf.to_vec()))
    }

    async fn expect_status(&mut self, kind: u8, fields: &[u8]) -> Result<(), SftpError> {
        match self.request(kind, fields).await? {
            (FXP_STATUS, body) => status(&body).map(|_| ()),
            _ => Err(SftpError::Other(
                "The SFTP server sent an unexpected answer.".to_owned(),
            )),
        }
    }

    async fn handle(&mut self, kind: u8, fields: &[u8]) -> Result<Vec<u8>, SftpError> {
        match self.request(kind, fields).await? {
            (FXP_HANDLE, body) => Ok(Reader { buf: &body }.bytes()?.to_vec()),
            (FXP_STATUS, body) => match status(&body) {
                Err(e) => Err(e),
                Ok(_) => Err(SftpError::Other(
                    "The SFTP server sent an unexpected answer.".to_owned(),
                )),
            },
            _ => Err(SftpError::Other(
                "The SFTP server sent an unexpected answer.".to_owned(),
            )),
        }
    }

    async fn close(&mut self, handle: &[u8]) {
        let mut fields = Vec::new();
        put_bytes(&mut fields, handle);
        let _ = self.expect_status(FXP_CLOSE, &fields).await;
    }

    /// The names in a directory, without `.` and `..` (`rawlist()`).
    pub async fn list(&mut self, path: &str) -> Result<Vec<String>, SftpError> {
        let mut fields = Vec::new();
        put_bytes(&mut fields, path.as_bytes());
        let handle = self.handle(FXP_OPENDIR, &fields).await?;
        let mut fields = Vec::new();
        put_bytes(&mut fields, &handle);
        let mut out = Vec::new();
        let result = loop {
            match self.request(FXP_READDIR, &fields).await {
                Ok((FXP_NAME, body)) => match names(&body) {
                    Ok(page) => out.extend(page.into_iter().filter(|n| n != "." && n != "..")),
                    Err(e) => break Err(e),
                },
                Ok((FXP_STATUS, body)) => match status(&body) {
                    Ok(_) => break Ok(()),
                    Err(e) => break Err(e),
                },
                Ok(_) => {
                    break Err(SftpError::Other(
                        "The SFTP server sent an unexpected answer.".to_owned(),
                    ));
                }
                Err(e) => break Err(e),
            }
        };
        self.close(&handle).await;
        result.map(|()| out)
    }

    /// The attributes of a path, or `None` when it does not exist (`stat()`).
    pub async fn stat(&mut self, path: &str) -> Result<Option<SftpAttrs>, SftpError> {
        let mut fields = Vec::new();
        put_bytes(&mut fields, path.as_bytes());
        match self.request(FXP_STAT, &fields).await? {
            (FXP_ATTRS, body) => Ok(Some(Reader { buf: &body }.attrs()?)),
            (FXP_STATUS, body) => match status(&body) {
                Err(e) if e.is_not_found() => Ok(None),
                Err(e) => Err(e),
                Ok(_) => Ok(None),
            },
            _ => Err(SftpError::Other(
                "The SFTP server sent an unexpected answer.".to_owned(),
            )),
        }
    }

    /// Deletes a file (`delete()`).
    pub async fn remove(&mut self, path: &str) -> Result<(), SftpError> {
        let mut fields = Vec::new();
        put_bytes(&mut fields, path.as_bytes());
        self.expect_status(FXP_REMOVE, &fields).await
    }

    /// `SftpFilesystemAdapter::deleteEmptyDirectory()`: RMDIR, which the server refuses for a
    /// directory that is not empty. False when the directory could not be deleted.
    pub async fn rmdir(&mut self, path: &str) -> Result<bool, SftpError> {
        let mut fields = Vec::new();
        put_bytes(&mut fields, path.as_bytes());
        match self.expect_status(FXP_RMDIR, &fields).await {
            Ok(()) => Ok(true),
            Err(SftpError::Status { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Opens a file for reading; read it with [`SftpSession::read`].
    pub async fn open(&mut self, path: &str) -> Result<Vec<u8>, SftpError> {
        let mut fields = Vec::new();
        put_bytes(&mut fields, path.as_bytes());
        // SSH_FXF_READ, no attributes.
        put_u32(&mut fields, 1);
        put_u32(&mut fields, 0);
        self.handle(FXP_OPEN, &fields).await
    }

    /// Up to `length` bytes at `offset`, or `None` at the end of the file.
    pub async fn read(
        &mut self,
        handle: &[u8],
        offset: u64,
        length: u32,
    ) -> Result<Option<Vec<u8>>, SftpError> {
        let mut fields = Vec::new();
        put_bytes(&mut fields, handle);
        fields.extend_from_slice(&offset.to_be_bytes());
        put_u32(&mut fields, length.min(READ_CHUNK));
        match self.request(FXP_READ, &fields).await? {
            (FXP_DATA, body) => Ok(Some(Reader { buf: &body }.bytes()?.to_vec())),
            (FXP_STATUS, body) => status(&body).map(|_| None),
            _ => Err(SftpError::Other(
                "The SFTP server sent an unexpected answer.".to_owned(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(pinned: Option<&str>, key: bool) -> SftpTarget {
        SftpTarget {
            host: "backup.example.com".into(),
            address: "203.0.113.7".into(),
            port: 2222,
            user: "backup user".into(),
            password: (!key).then(|| Zeroizing::new("secret".to_owned())),
            private_key: key
                .then(|| Zeroizing::new("-----BEGIN OPENSSH PRIVATE KEY-----".to_owned())),
            pinned_host_key: pinned.map(str::to_owned),
            timeout: Duration::from_secs(15),
        }
    }

    fn text(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_saved_key_is_the_only_key_ssh_accepts() {
        let t = target(Some("ssh-ed25519 AAAAC3Nz"), true);
        let args = text(&ssh_args(
            &t,
            Some(Path::new("/k/key-1")),
            Path::new("/k/known-1"),
        ));
        assert!(args.contains(&"StrictHostKeyChecking=yes".to_owned()));
        assert!(args.contains(&"HostKeyAlgorithms=ssh-ed25519".to_owned()));
        assert!(args.contains(&"UserKnownHostsFile=/k/known-1".to_owned()));
        assert!(args.contains(&"BatchMode=yes".to_owned()));
        assert!(args.contains(&"PasswordAuthentication=no".to_owned()));
        assert_eq!(
            args[args.len() - 8..],
            [
                "-l",
                "backup user",
                "-p",
                "2222",
                "-s",
                "--",
                "203.0.113.7",
                "sftp"
            ]
        );
        assert_eq!(args[..4], ["-F", "/dev/null", "-i", "/k/key-1"]);
    }

    #[test]
    fn without_a_saved_key_the_first_key_is_recorded() {
        let t = target(None, false);
        let args = text(&ssh_args(&t, None, Path::new("/k/known-1")));
        assert!(args.contains(&"StrictHostKeyChecking=accept-new".to_owned()));
        assert!(!args.iter().any(|a| a.starts_with("HostKeyAlgorithms=")));
        assert!(args.contains(&"PubkeyAuthentication=no".to_owned()));
        assert!(args.contains(&"BatchMode=no".to_owned()));
        assert!(!args.contains(&"-i".to_owned()));
        // The password is never an argument.
        assert!(!args.iter().any(|a| a.contains("secret")));
        assert_eq!(
            presented_host_key("coolify-backup-storage ssh-ed25519 AAAAC3NzaC1lZDI1NTE5\n"),
            Some("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5".to_owned())
        );
        assert_eq!(presented_host_key(""), None);
        assert_eq!(presented_host_key("other ssh-ed25519 AAAA\n"), None);
    }

    #[test]
    fn rsa_keys_ask_for_every_rsa_signature() {
        assert_eq!(
            host_key_algorithms("ssh-rsa AAAAB3"),
            ["rsa-sha2-512", "rsa-sha2-256", "ssh-rsa"]
        );
        assert_eq!(
            host_key_algorithms("ecdsa-sha2-nistp256 AAAAE2"),
            ["ecdsa-sha2-nistp256"]
        );
        assert!(valid_host_key("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA+/="));
        assert!(!valid_host_key("ssh-ed25519"));
        assert!(!valid_host_key("ssh-ed25519 AAAA comment"));
        assert!(!valid_host_key("ssh-ed25519 AAAA\nother ssh-rsa BBBB"));
    }

    #[test]
    fn failures_of_ssh_are_classified() {
        let pinned = target(Some("ssh-ed25519 AAAA"), false);
        let changed = classify_failure(
            "@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\nHost key verification failed.",
            &pinned,
        );
        assert_eq!(
            changed.to_string(),
            "The SSH host key of the SFTP server backup.example.com changed. If you expect this change, for example after a reinstall of the server, reset the host key in the storage settings and validate the connection again."
        );
        assert_eq!(
            classify_failure(
                "Permission denied, please try again.\nbackup@203.0.113.7: Permission denied (publickey,password).",
                &pinned
            )
            .to_string(),
            "Unable to authenticate using a password."
        );
        assert_eq!(
            classify_failure("x: Permission denied (publickey).", &target(None, true)).to_string(),
            "Unable to authenticate using a private key."
        );
        assert_eq!(
            classify_failure(
                "ssh: connect to host 203.0.113.7 port 2222: Connection refused\r\n",
                &pinned
            ),
            SftpError::Connect("connect to host 203.0.113.7 port 2222: Connection refused".into())
        );
        assert_eq!(
            classify_failure("subsystem request failed on channel 0", &pinned),
            SftpError::Other("subsystem request failed on channel 0".into())
        );
    }

    #[test]
    fn packets_have_a_length_and_a_type() {
        let mut fields = Vec::new();
        put_u32(&mut fields, 7);
        put_bytes(&mut fields, b"/a");
        assert_eq!(
            packet(FXP_STAT, &fields),
            [0, 0, 0, 11, 17, 0, 0, 0, 7, 0, 0, 0, 2, b'/', b'a']
        );
    }

    #[test]
    fn attributes_and_statuses_are_read() {
        // size + permissions of a directory.
        let mut body = Vec::new();
        put_u32(
            &mut body,
            ATTR_SIZE | ATTR_UIDGID | ATTR_PERMISSIONS | ATTR_ACMODTIME,
        );
        body.extend_from_slice(&4096u64.to_be_bytes());
        put_u32(&mut body, 1000);
        put_u32(&mut body, 1000);
        put_u32(&mut body, 0o040_755);
        put_u32(&mut body, 1);
        put_u32(&mut body, 2);
        let attrs = Reader { buf: &body }.attrs().unwrap();
        assert_eq!(attrs.size, Some(4096));
        assert!(attrs.is_dir());

        let mut file = Vec::new();
        put_u32(&mut file, ATTR_SIZE | ATTR_PERMISSIONS);
        file.extend_from_slice(&123u64.to_be_bytes());
        put_u32(&mut file, 0o100_644);
        let attrs = Reader { buf: &file }.attrs().unwrap();
        assert_eq!((attrs.size, attrs.is_dir()), (Some(123), false));

        let mut missing = Vec::new();
        put_u32(&mut missing, FX_NO_SUCH_FILE);
        put_bytes(&mut missing, b"No such file");
        put_bytes(&mut missing, b"");
        let error = status(&missing).unwrap_err();
        assert!(error.is_not_found());
        assert_eq!(error.to_string(), "No such file");
        let mut eof = Vec::new();
        put_u32(&mut eof, FX_EOF);
        assert_eq!(status(&eof), Ok(false));
        assert!(Reader { buf: &[0, 0] }.u32().is_err());
    }

    #[test]
    fn directory_pages_list_names() {
        let mut body = Vec::new();
        put_u32(&mut body, 2);
        for name in ["..", "db.dmp"] {
            put_bytes(&mut body, name.as_bytes());
            put_bytes(&mut body, b"-rw-r--r-- 1 0 0 1 Jan 1 00:00 x");
            put_u32(&mut body, 0);
        }
        assert_eq!(names(&body).unwrap(), ["..", "db.dmp"]);
    }
}
