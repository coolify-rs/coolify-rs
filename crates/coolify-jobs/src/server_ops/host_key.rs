//! SSH host key pinning with the database (upstream v4.7.5): `SshMultiplexingHelper`
//! (`hostKeyCheckingMode`, `hostKeyOptions`, `learnHostKey`, `loadSavedHostKeySettings`),
//! `ServerSetting::forgetSshHostKey` and `Server::recordHostKeyMismatch`.
//!
//! The parts without a database (options, known_hosts files, the scan) are in
//! `coolify_remote::host_key`. coolify-rs opens no multiplexed connections, so upstream's
//! `removeMuxFile()` calls have nothing to stop here.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use coolify_db::ssh_host_keys::{self, HostKeySettings};
use coolify_remote::host_key::{
    HOST_KEY_SCAN_MAX_CONNECT_TIMEOUT_SECONDS, sha256_hex, ssh_host_key_fingerprints,
};
use coolify_remote::{HostKeyCheck, HostKeyMode, Target};
use serde_json::{Value, json};

use super::MARKS;
use crate::Jobs;
use crate::notifications::{self, Notification, lookup};
use crate::server::ServerInfo;

/// `HOST_KEY_SCAN_RETRY_SECONDS`: after a failed scan (server offline), commands and the
/// connection check do not scan again for this time.
const HOST_KEY_SCAN_RETRY_SECONDS: u64 = 600;
/// `HOST_KEY_SCAN_ENFORCE_RETRY_SECONDS`: enforce mode cannot connect without a pinned key, so it
/// scans again sooner.
const HOST_KEY_SCAN_ENFORCE_RETRY_SECONDS: u64 = 60;
/// `DEFAULT_CONNECT_TIMEOUT` of the executor (`constants.ssh.connection_timeout`).
const DEFAULT_CONNECT_TIMEOUT: i64 = 10;

/// The start of the error of `hostKeyOptions()` in enforce mode without a pinned key.
pub const UNREADABLE_HOST_KEY: &str =
    "SSH connection failed: Coolify could not read the SSH host key of the server";

/// `Cache::lock('ssh_host_key_learn_'.$server->uuid)`: one scan per server at a time.
static LEARN_LOCKS: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Mutex::default);

fn learn_lock(server_uuid: &str) -> Arc<tokio::sync::Mutex<()>> {
    LEARN_LOCKS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(server_uuid.to_owned())
        .or_default()
        .clone()
}

/// Laravel `filled()` for a string column.
fn filled(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.trim().is_empty())
}

/// `SshMultiplexingHelper::hostKeyCheckingMode($server)`.
pub fn mode(jobs: &Jobs, saved: Option<&HostKeySettings>) -> HostKeyMode {
    jobs.remote().host_key_checking().for_server(
        saved.is_some_and(|s| s.is_ssh_host_key_check_disabled),
        saved.is_some_and(|s| s.is_ssh_host_key_check_enforced),
    )
}

/// `hostKeyScanFailedCacheKey()`: the key has the mode, so a failure of a warn mode scan does not
/// stop an enforce mode scan for its longer retry time.
fn scan_failed_key(mode: HostKeyMode, server_uuid: &str) -> String {
    format!("ssh_host_key_scan_failed_{}_{server_uuid}", mode.as_str())
}

/// `hostKeyScanProcessTimeout()`.
fn scan_process_timeout(target: &Target) -> Duration {
    let connect = target
        .connection_timeout
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_CONNECT_TIMEOUT)
        .min(HOST_KEY_SCAN_MAX_CONNECT_TIMEOUT_SECONDS);
    Duration::from_secs(connect.unsigned_abs() + 5)
}

/// `loadSavedHostKeySettings()`: a job that loaded the server before an admin enforced the check,
/// trusted a new key, or reset the key must not connect with the old settings.
pub async fn saved(jobs: &Jobs, server_id: i64) -> Result<Option<HostKeySettings>, String> {
    ssh_host_keys::load(&jobs.0.db, server_id)
        .await
        .map_err(|e| e.to_string())
}

/// `SshMultiplexingHelper::hostKeyOptions()`: the host key check of the next connection.
/// `bare` is the target without host key options, for the scan.
pub async fn options(
    jobs: &Jobs,
    info: &ServerInfo,
    bare: &Target,
) -> Result<HostKeyCheck, String> {
    let mut settings = saved(jobs, info.id).await?;
    learn(jobs, info, bare, &mut settings, false).await;
    check_for(
        mode(jobs, settings.as_ref()),
        settings.as_ref().and_then(|s| s.ssh_host_key.as_deref()),
        || MARKS.get(&scan_failed_key(HostKeyMode::Enforce, &info.uuid)),
    )
}

/// The decision of `hostKeyOptions()` once the settings are loaded and the key is learned.
fn check_for(
    mode: HostKeyMode,
    pinned: Option<&str>,
    scan_error: impl FnOnce() -> Option<String>,
) -> Result<HostKeyCheck, String> {
    if mode == HostKeyMode::Off {
        return Ok(HostKeyCheck::Off);
    }
    match pinned.filter(|k| filled(Some(k))) {
        Some(keys) => Ok(HostKeyCheck::Pinned {
            keys: keys.to_owned(),
            enforce: mode == HostKeyMode::Enforce,
        }),
        None if mode == HostKeyMode::Enforce => Err(match scan_error().filter(|e| !e.is_empty()) {
            Some(error) => format!("{UNREADABLE_HOST_KEY}: {error}"),
            None => format!("{UNREADABLE_HOST_KEY}."),
        }),
        // Warn mode connects without a pinned key.
        None => Ok(HostKeyCheck::Off),
    }
}

/// `SshMultiplexingHelper::learnHostKey()`: pins the host key that the server sends on the first
/// connection (trust on first use). Nothing scans again within the retry time of a failed scan.
/// Commands also do not scan a server that is unreachable, so they do not wait for the scan
/// timeout; the connection check passes `even_if_unreachable`.
pub async fn learn(
    jobs: &Jobs,
    info: &ServerInfo,
    bare: &Target,
    settings: &mut Option<HostKeySettings>,
    even_if_unreachable: bool,
) {
    let current = mode(jobs, settings.as_ref());
    let Some(known) = settings.as_ref() else {
        return;
    };
    if current == HostKeyMode::Off || filled(known.ssh_host_key.as_deref()) {
        return;
    }
    let failed_key = scan_failed_key(current, &info.uuid);
    if MARKS.has(&failed_key) || (!even_if_unreachable && info.unreachable_count > 0) {
        return;
    }
    let lock = learn_lock(&info.uuid);
    // Enforce mode cannot connect without the key, so it waits for another task that reads it.
    // Warn mode connects without it.
    let _guard = if current == HostKeyMode::Enforce {
        match tokio::time::timeout(scan_process_timeout(bare), lock.lock()).await {
            Ok(guard) => guard,
            Err(_) => {
                reload(jobs, info.id, settings).await;
                return;
            }
        }
    } else {
        match lock.try_lock() {
            Ok(guard) => guard,
            Err(_) => return,
        }
    };
    if reload(jobs, info.id, settings).await {
        return;
    }
    let Some((settings_id, usable)) = settings.as_ref().map(|s| (s.settings_id, s.is_usable))
    else {
        return;
    };
    // coolify-rs before 4.7.5 pinned the key in a file of its own. That key stays the trusted
    // key: a scan now would trust whatever answers at the address today.
    let legacy = jobs.remote().legacy_host_keys(&info.uuid);
    let (key, source) = match legacy {
        Some(key) => (key, "upgrade"),
        None => {
            let scan = jobs.remote().scan_host_key(bare).await;
            let Some(key) = scan.key else {
                let retry = if current == HostKeyMode::Enforce {
                    HOST_KEY_SCAN_ENFORCE_RETRY_SECONDS
                } else {
                    HOST_KEY_SCAN_RETRY_SECONDS
                };
                MARKS.put(failed_key, scan.error, Duration::from_secs(retry));
                return;
            };
            // A server that was never usable is new. A usable server learns its key after the
            // upgrade.
            (key, if usable { "upgrade" } else { "tofu" })
        }
    };
    let now = Jobs::now();
    let pinned = ssh_host_keys::pin_if_unset(&jobs.0.db, settings_id, &key, source, now)
        .await
        .unwrap_or(false);
    if !pinned {
        reload(jobs, info.id, settings).await;
        return;
    }
    jobs.remote().forget_legacy_host_keys(&info.uuid);
    // `ServerSetting::updated`: the files of older keys go.
    jobs.remote()
        .delete_stale_known_hosts_files(&info.uuid, Some(&key));
    if let Some(s) = settings.as_mut() {
        s.ssh_host_key = Some(key);
        s.ssh_host_key_source = Some(source.to_owned());
        s.ssh_host_key_trusted_at = Some(now);
        s.ssh_host_key_pending = None;
    }
}

/// `loadPinnedHostKey()`: true when a key is pinned in the database.
async fn reload(jobs: &Jobs, server_id: i64, settings: &mut Option<HostKeySettings>) -> bool {
    let Ok(Some(stored)) = ssh_host_keys::load(&jobs.0.db, server_id).await else {
        return false;
    };
    if !filled(stored.ssh_host_key.as_deref()) {
        return false;
    }
    *settings = Some(stored);
    true
}

/// `learnHostKey($server, evenIfUnreachable: true)` for callers that hold only the server: the
/// connection check and the validation.
pub async fn learn_now(jobs: &Jobs, info: &ServerInfo) {
    let Ok(bare) = info.bare_target(jobs).await else {
        return;
    };
    let Ok(mut settings) = saved(jobs, info.id).await else {
        return;
    };
    learn(jobs, info, &bare, &mut settings, true).await;
}

/// `Server::validateConnection()` before it connects: enforce mode cannot connect without the
/// key, so it reads the key first. Warn mode learns it after the connection.
pub async fn before_validation(jobs: &Jobs, info: &ServerInfo) {
    if saved_mode(jobs, info.id).await == HostKeyMode::Enforce {
        learn_now(jobs, info).await;
    }
}

/// `Server::validateConnection()` after a failed connection: a refused host key is recorded, and
/// when the server still sends the trusted key the error says so.
pub async fn validation_error(jobs: &Jobs, info: &ServerInfo, error: String) -> String {
    if error == coolify_remote::HOST_KEY_CHANGED_MESSAGE && !record_mismatch(jobs, info, "").await {
        return "SSH host key verification failed, but the server still sends the trusted host key."
            .to_owned();
    }
    error
}

/// `hostKeyCheckingMode($server)` with the saved settings.
pub async fn saved_mode(jobs: &Jobs, server_id: i64) -> HostKeyMode {
    let settings = saved(jobs, server_id).await.ok().flatten();
    mode(jobs, settings.as_ref())
}

/// `auditLog()` outside a request.
async fn audit_log(jobs: &Jobs, team_id: i64, event: &str, context: Value, level: &str) {
    let actor = coolify_db::audit::Actor::default();
    if let Err(e) = coolify_db::audit::record(
        &jobs.0.db,
        Some(team_id),
        event,
        &context,
        level,
        &actor,
        None,
        Jobs::now(),
    )
    .await
    {
        tracing::warn!(event, error = %e, "audit log failed");
    }
}

/// `ServerSetting::forgetSshHostKey($reason)`: removes the pinned and the pending host key after
/// the address of the server changed, so that the next connection trusts the key of the new
/// address. Reasons: `address_changed`, `cloudflare_tunnel_changed`.
pub async fn forget(jobs: &Jobs, server_id: i64, team_id: i64, server_uuid: &str, reason: &str) {
    // The pin file of coolify-rs before 4.7.5 is for the old address too.
    jobs.remote().forget_legacy_host_keys(server_uuid);
    let Ok(Some(settings)) = saved(jobs, server_id).await else {
        return;
    };
    if !filled(settings.ssh_host_key.as_deref())
        && !filled(settings.ssh_host_key_pending.as_deref())
    {
        return;
    }
    let previous = ssh_host_key_fingerprints(settings.ssh_host_key.as_deref());
    if let Err(e) = ssh_host_keys::forget(&jobs.0.db, settings.settings_id, Jobs::now()).await {
        tracing::warn!(server_uuid, error = %e, "could not forget the SSH host key");
        return;
    }
    jobs.remote().forget_host_key(server_uuid);
    audit_log(
        jobs,
        team_id,
        "server.ssh_host_key.forgotten",
        json!({
            "team_id": team_id,
            "server_uuid": server_uuid,
            "reason": reason,
            "previous_fingerprints": previous,
        }),
        "info",
    )
    .await;
}

/// The `SHA256:...` fingerprints in the stderr of ssh (`/SHA256:[A-Za-z0-9+\/]+/`).
fn fingerprints_in(output: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = output;
    while let Some(start) = rest.find("SHA256:") {
        let tail = &rest[start + "SHA256:".len()..];
        let len = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '+' || c == '/'))
            .unwrap_or(tail.len());
        if len > 0 {
            found.push(&rest[start..start + "SHA256:".len() + len]);
        }
        rest = &tail[len..];
    }
    found
}

/// The first part of `recordHostKeyMismatch()`: the stderr of ssh shows only fingerprints of the
/// pending key, so no scan is needed.
fn shows_pending_key(pending: Option<&str>, ssh_error_output: &str) -> bool {
    if !filled(pending) {
        return false;
    }
    let found = fingerprints_in(ssh_error_output);
    if found.is_empty() {
        return false;
    }
    let pending_fingerprints = ssh_host_key_fingerprints(pending);
    let pending_fingerprints: Vec<&str> = pending_fingerprints
        .iter()
        .map(|f| f.split(' ').next().unwrap_or_default())
        .collect();
    found.iter().all(|f| pending_fingerprints.contains(f))
}

/// `array_diff(explode("\n", $newKey), explode("\n", $pinned)) === []`: the server still sends
/// only pinned keys.
fn only_pinned_keys(new_key: &str, pinned: Option<&str>) -> bool {
    let pinned: Vec<&str> = pinned.unwrap_or_default().split('\n').collect();
    new_key.split('\n').all(|line| pinned.contains(&line))
}

/// `Server::recordHostKeyMismatch()`: ssh reported a host key problem. Returns true when the
/// server sends a host key that is not pinned, and saves that key as pending, so that an admin
/// can compare and trust it. Returns false when the server still sends the pinned key or the key
/// cannot be read: then it is a normal connection error. A key is reported once a day, and the
/// team gets at most one notification an hour. While a key is pending, the server is scanned at
/// most every ten minutes.
pub async fn record_mismatch(jobs: &Jobs, info: &ServerInfo, ssh_error_output: &str) -> bool {
    let Ok(Some(settings)) = saved(jobs, info.id).await else {
        return false;
    };
    let pending = settings.ssh_host_key_pending.as_deref();
    if shows_pending_key(pending, ssh_error_output) {
        return true;
    }
    // Each scan is a failed login on the server (fail2ban), so a pending key is scanned again at
    // most every ten minutes.
    let scanned_key = format!(
        "ssh_host_key_mismatch_scanned_{}_{}",
        info.uuid,
        sha256_hex(settings.ssh_host_key.as_deref().unwrap_or_default())
    );
    if filled(pending) && MARKS.has(&scanned_key) {
        return true;
    }
    let Ok(bare) = info.bare_target(jobs).await else {
        return false;
    };
    let new_key = jobs.remote().scan_host_key(&bare).await.key;
    MARKS.put(scanned_key, String::new(), Duration::from_secs(600));
    let Some(new_key) = new_key else {
        return false;
    };
    if only_pinned_keys(&new_key, settings.ssh_host_key.as_deref()) {
        return false;
    }
    if Some(new_key.as_str()) == pending {
        return true;
    }
    if let Err(e) =
        ssh_host_keys::set_pending(&jobs.0.db, settings.settings_id, &new_key, Jobs::now()).await
    {
        tracing::warn!(server_uuid = %info.uuid, error = %e, "could not save the new SSH host key");
    }
    if !MARKS.add(
        format!(
            "ssh_host_key_reported_{}_{}",
            info.uuid,
            sha256_hex(&new_key)
        ),
        Duration::from_secs(86400),
    ) {
        return true;
    }
    tracing::warn!(
        server_uuid = %info.uuid,
        mode = mode(jobs, Some(&settings)).as_str(),
        "SSH host key of server changed"
    );
    let server = lookup::server(&jobs.0.db, info.id).await;
    audit_log(
        jobs,
        info.team_id,
        "server.ssh_host_key_changed",
        json!({
            "team_id": info.team_id,
            "server_uuid": info.uuid,
            "server_name": server.as_ref().map(|(_, s)| s.name.clone()),
            "fingerprints": ssh_host_key_fingerprints(Some(&new_key)),
        }),
        "warning",
    )
    .await;
    if MARKS.add(
        format!("ssh_host_key_changed_notified_{}", info.uuid),
        Duration::from_secs(3600),
    ) && let Some((team_id, server)) = server
    {
        notifications::notify(
            &jobs.0.db,
            jobs.0.encrypter.as_ref(),
            team_id,
            Notification::HostKeyChanged { server },
        )
        .await;
    }
    true
}

/// `SshMultiplexingHelper::noticeHostKeyMismatch()`: when an SSH process reports a changed host
/// key, check the connection once in ten minutes. The check saves the new key as pending and
/// notifies the team, also for a server that the scheduled connection check skips (Sentinel).
pub async fn noticed(jobs: &Jobs, server_uuid: &str) {
    if !MARKS.add(
        format!("ssh_host_key_mismatch_noticed_{server_uuid}"),
        Duration::from_secs(600),
    ) {
        return;
    }
    let Ok(Some(row)) = coolify_db::servers::find_any_team(&jobs.0.db, server_uuid).await else {
        return;
    };
    if let Err(e) = super::checks::server_connection_check(jobs, row.id).await {
        tracing::warn!(server_uuid, error = %e, "connection check after a changed SSH host key failed");
    }
}

impl Jobs {
    /// `ServerConnectionCheckJob::dispatch($server)`.
    pub fn spawn_connection_check(&self, server_id: i64) {
        self.spawn_server_job("connection-check", server_id, |jobs, id| async move {
            super::checks::server_connection_check(&jobs, id).await
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_follow_the_mode_and_the_pinned_key() {
        let none = || None;
        assert_eq!(
            check_for(HostKeyMode::Off, Some("ssh-ed25519 AAAA"), none),
            Ok(HostKeyCheck::Off)
        );
        // Warn mode connects before the first key is pinned.
        assert_eq!(
            check_for(HostKeyMode::Warn, None, none),
            Ok(HostKeyCheck::Off)
        );
        assert_eq!(
            check_for(HostKeyMode::Warn, Some("  "), none),
            Ok(HostKeyCheck::Off)
        );
        assert_eq!(
            check_for(HostKeyMode::Warn, Some("ssh-ed25519 AAAA"), none),
            Ok(HostKeyCheck::Pinned {
                keys: "ssh-ed25519 AAAA".into(),
                enforce: false
            })
        );
        assert_eq!(
            check_for(HostKeyMode::Enforce, Some("ssh-ed25519 AAAA"), none),
            Ok(HostKeyCheck::Pinned {
                keys: "ssh-ed25519 AAAA".into(),
                enforce: true
            })
        );
        // Enforce mode cannot connect without the key.
        assert_eq!(
            check_for(HostKeyMode::Enforce, None, none),
            Err(
                "SSH connection failed: Coolify could not read the SSH host key of the server."
                    .into()
            )
        );
        assert_eq!(
            check_for(HostKeyMode::Enforce, None, || Some("The connection timed out.".into())),
            Err("SSH connection failed: Coolify could not read the SSH host key of the server: The connection timed out.".into())
        );
    }

    #[test]
    fn finds_fingerprints_in_the_output_of_ssh() {
        let out = "The fingerprint for the ED25519 key sent by the remote host is\nSHA256:P/QnH16tWXGbHRqp4+IUWllIA/aw5lP/GZvdfsdVilk.\nSHA256: SHA256:abc";
        assert_eq!(
            fingerprints_in(out),
            [
                "SHA256:P/QnH16tWXGbHRqp4+IUWllIA/aw5lP/GZvdfsdVilk",
                "SHA256:abc"
            ]
        );
        assert!(fingerprints_in("Host key verification failed.").is_empty());
    }

    #[test]
    fn a_pending_key_in_the_output_needs_no_scan() {
        let pending =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFuGmoeGq/pojrsyP1pszcNVuZx9iFkCELtxrh31QJ68";
        let print = ssh_host_key_fingerprints(Some(pending));
        let print = print[0].split(' ').next().unwrap_or_default().to_owned();
        assert!(shows_pending_key(
            Some(pending),
            &format!("The fingerprint for the ED25519 key sent by the remote host is\n{print}.\n")
        ));
        // Another fingerprint: the server sends a third key.
        assert!(!shows_pending_key(
            Some(pending),
            &format!("{print}\nSHA256:other")
        ));
        assert!(!shows_pending_key(
            Some(pending),
            "Host key verification failed."
        ));
        assert!(!shows_pending_key(None, &print));
    }

    #[test]
    fn a_server_that_sends_pinned_keys_has_no_mismatch() {
        let pinned = Some("ssh-ed25519 AAAA\nssh-rsa BBBB");
        assert!(only_pinned_keys("ssh-ed25519 AAAA", pinned));
        assert!(only_pinned_keys("ssh-rsa BBBB\nssh-ed25519 AAAA", pinned));
        assert!(!only_pinned_keys("ssh-ed25519 CCCC", pinned));
        assert!(!only_pinned_keys(
            "ssh-ed25519 AAAA\necdsa-sha2-nistp256 DDDD",
            pinned
        ));
        assert!(!only_pinned_keys("ssh-ed25519 AAAA", None));
    }

    #[test]
    fn scan_failures_are_remembered_per_mode() {
        assert_eq!(
            scan_failed_key(HostKeyMode::Warn, "abc"),
            "ssh_host_key_scan_failed_warn_abc"
        );
        assert_eq!(
            scan_failed_key(HostKeyMode::Enforce, "abc"),
            "ssh_host_key_scan_failed_enforce_abc"
        );
    }
}
