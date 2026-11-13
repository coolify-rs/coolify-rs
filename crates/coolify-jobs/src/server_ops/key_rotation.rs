//! Private key rotation (upstream v4.7.5): `RotatePrivateKeyJob`,
//! `RemoveRotatedAuthorizedKeyJob` and the state `Livewire\Security\PrivateKey\Rotate` reads.
//!
//! Upstream keeps the result in the cache and the jobs in the queue. Here the result is in the
//! process (`MARKS`), and the removal of the old key waits in a task: a restart of coolify-rs
//! within the wait leaves the old key in `authorized_keys` (the page then keeps showing
//! "moved_old_pending" until the result expires).

use std::time::Duration;

use coolify_compat::sshkey;
use coolify_db::private_keys::{self, KeyRow, NewKey};
use coolify_db::servers::{self, ColumnValue};
use regex::Regex;
use serde_json::{Map, Value, json};

use super::authorized_keys::{self, KeyEditError, OLD_KEY_NOT_FOUND};
use super::{MARKS, Srv, install};
use crate::Jobs;

/// `RotatePrivateKeyJob::RESULT_TTL_SECONDS`.
pub const RESULT_TTL_SECONDS: u64 = 86400;
/// `RotatePrivateKeyJob::OLD_KEY_PENDING`: the server moved; the old key is removed later.
pub const OLD_KEY_PENDING: &str = "moved_old_pending";
/// `RemoveRotatedAuthorizedKeyJob::GRACE_SECONDS`: the shortest wait before the old key is
/// removed. A server waits at least its deployment timeout.
const GRACE_SECONDS: i64 = 3600;
/// `RotatePrivateKeyJob::$timeout`.
const ROTATION_TIMEOUT_SECONDS: u64 = 1800;
/// `RemoveRotatedAuthorizedKeyJob::$backoff`.
const REMOVE_BACKOFF_SECONDS: [u64; 2] = [300, 1800];

/// `RotatePrivateKeyJob::cacheKey()`.
pub fn cache_key(private_key_uuid: &str) -> String {
    format!("private-key-rotation:{private_key_uuid}")
}

/// `Cache::get(RotatePrivateKeyJob::cacheKey($key))`.
pub fn state(private_key_uuid: &str) -> Option<Map<String, Value>> {
    let stored = MARKS.get(&cache_key(private_key_uuid))?;
    match serde_json::from_str(&stored).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

fn store_state(private_key_uuid: &str, state: &Map<String, Value>) {
    MARKS.put(
        cache_key(private_key_uuid),
        Value::Object(state.clone()).to_string(),
        Duration::from_secs(RESULT_TTL_SECONDS),
    );
}

/// `config('app.id') ?: config('app.url')`: what the comment of an added key names.
fn instance() -> String {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    var("APP_ID")
        .or_else(|| var("APP_URL"))
        .unwrap_or_else(|| "http://localhost".to_owned())
}

/// `RemoveRotatedAuthorizedKeyJob::delaySeconds()`.
pub fn delay_seconds(dynamic_timeout: Option<i64>) -> i64 {
    GRACE_SECONDS.max(dynamic_timeout.unwrap_or(0) + 300)
}

/// `CarbonInterval::seconds($delay)->cascade()->forHumans(['parts' => 2])`.
pub fn interval_for_humans(seconds: i64) -> String {
    let units = [
        ("week", 604_800),
        ("day", 86_400),
        ("hour", 3_600),
        ("minute", 60),
        ("second", 1),
    ];
    let mut rest = seconds.max(0);
    let mut parts = Vec::new();
    for (name, size) in units {
        let count = rest / size;
        rest %= size;
        if count > 0 && parts.len() < 2 {
            parts.push(format!(
                "{count} {name}{}",
                if count == 1 { "" } else { "s" }
            ));
        }
    }
    if parts.is_empty() {
        return "1 second".to_owned();
    }
    parts.join(" ")
}

/// `RotatePrivateKeyJob::errorWithoutHost()`: the UI shows server names only, so the host
/// address is removed from SSH errors.
pub fn error_without_host(ip: &str, message: &str) -> String {
    let redacted = coolify_compat::redact::redact_text(message, &[], &[]);
    let message = if ip.is_empty() {
        redacted
    } else {
        redacted.replace(ip, "the server")
    };
    let prefix = Regex::new(r"(?i)^ssh: connect to host the server port \d+:\s*")
        .unwrap_or_else(|e| unreachable!("{e}"));
    prefix.replace(&message, "").trim().to_owned()
}

/// `createNewKey()` of the Rotate component: the name of the new key. A second rotation does
/// not stack the suffix.
pub fn rotated_key_name(old_name: &str, today: chrono::NaiveDate) -> String {
    let suffix =
        Regex::new(r"( \(rotated \d{4}-\d{2}-\d{2}\))+$").unwrap_or_else(|e| unreachable!("{e}"));
    let base = suffix.replace(old_name, "");
    // `Str::limit($baseName, 200, '')`.
    let base: String = base.chars().take(200).collect();
    format!("{} (rotated {})", base.trim_end(), today.format("%Y-%m-%d"))
}

fn result(status: &str, message: impl Into<String>) -> (String, String) {
    (status.to_owned(), message.into())
}

/// The result of `add()` failing, by exit code (`rotateServer()`).
fn add_failure(ip: &str, e: &KeyEditError) -> (String, String) {
    match e.code {
        Some(OLD_KEY_NOT_FOUND) => result(
            "old_key_not_found",
            "The old key is not in ~/.ssh/authorized_keys of the SSH user, so Coolify cannot replace it there. Add the new key on the server yourself.",
        ),
        // ssh exits with 255 when it cannot connect or log in.
        Some(255) => result(
            "unreachable",
            format!("Could not connect: {}", error_without_host(ip, &e.message)),
        ),
        _ => result(
            "failed",
            format!(
                "Could not add the new key: {}",
                error_without_host(ip, &e.message)
            ),
        ),
    }
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

fn decrypt(jobs: &Jobs, key: &KeyRow) -> Result<String, String> {
    jobs.0
        .encrypter
        .as_ref()
        .ok_or("No application encryption key has been specified.")?
        .decrypt_string(&key.private_key_encrypted)
        .map_err(|_| "The private key cannot be read.".to_owned())
}

/// Why `Rotate::rotate()` did not start a rotation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// `$this->dispatch('error', ...)`.
    Message(&'static str),
    Other(String),
}

/// `Rotate::rotate()` after validation and authorization: creates the new key (or reuses the
/// new key of the previous run, so servers that already moved stay on it) and starts the job.
/// Returns the uuid of the new key.
pub async fn start(
    jobs: &Jobs,
    old_key: &KeyRow,
    rsa: bool,
    started_by_user_id: Option<i64>,
) -> Result<String, StartError> {
    let db = &jobs.0.db;
    let other = |e: coolify_db::DbError| StartError::Other(e.to_string());
    let used: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM servers WHERE team_id = $1 AND private_key_id = $2 AND deleted_at IS NULL)",
    )
    .bind(old_key.team_id)
    .bind(old_key.id)
    .fetch_one(db.pool())
    .await
    .map_err(|e| StartError::Other(e.to_string()))?;
    if !used {
        return Err(StartError::Message("No server uses this key."));
    }
    let previous = state(&old_key.uuid).unwrap_or_default();
    let started_recently = previous
        .get("started_at")
        .and_then(Value::as_str)
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .is_some_and(|t| chrono::Utc::now() - chrono::Duration::hours(1) < t);
    if previous.get("status").and_then(Value::as_str) == Some("running") && started_recently {
        return Err(StartError::Message(
            "A rotation of this key is already running.",
        ));
    }
    let reused = match previous.get("new_private_key_id").and_then(Value::as_i64) {
        Some(id) => private_keys::find_by_id(db, old_key.team_id, id)
            .await
            .map_err(other)?,
        None => None,
    };
    let new_key = match reused {
        Some(key) => key,
        None => {
            // `PrivateKey::generateNewKeyPair('rsa', 4096)` or `('ed25519')`.
            let kind = if rsa {
                sshkey::KeyKind::Rsa4096
            } else {
                sshkey::KeyKind::Ed25519
            };
            let pem = sshkey::generate(kind)
                .ok_or_else(|| StartError::Other("Failed to generate new SSH key pair.".into()))?;
            let pem = sshkey::format_private_key(&pem);
            let info = sshkey::inspect(&pem)
                .ok_or_else(|| StartError::Other("The private key is invalid.".into()))?;
            let encrypter = jobs.0.encrypter.as_ref().ok_or_else(|| {
                StartError::Other("No application encryption key has been specified.".into())
            })?;
            let name = rotated_key_name(&old_key.name, chrono::Utc::now().date_naive());
            let uuid = private_keys::create(
                db,
                NewKey {
                    team_id: old_key.team_id,
                    name: &name,
                    description: old_key.description.as_deref().unwrap_or_default(),
                    private_key_encrypted: &encrypter.encrypt_string(&pem),
                    fingerprint: &info.fingerprint,
                },
                Jobs::now(),
            )
            .await
            .map_err(other)?;
            private_keys::find(db, old_key.team_id, &uuid)
                .await
                .map_err(other)?
                .ok_or_else(|| StartError::Other("The new key does not exist anymore.".into()))?
        }
    };
    let mut fresh = Map::new();
    fresh.insert("status".into(), json!("running"));
    fresh.insert(
        "started_at".into(),
        json!(
            chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%S+00:00")
                .to_string()
        ),
    );
    fresh.insert("new_private_key_id".into(), json!(new_key.id));
    fresh.insert("servers".into(), json!({}));
    store_state(&old_key.uuid, &fresh);
    let (jobs_for_task, old_id, new_id) = (jobs.clone(), old_key.id, new_key.id);
    tokio::spawn(async move {
        rotate(&jobs_for_task, old_id, new_id, started_by_user_id).await;
    });
    Ok(new_key.uuid)
}

/// `RotatePrivateKeyJob::handle()`: moves every server that uses the old key to the new key. A
/// server moves only after the new key logs in. Servers that fail stay on the old key, so the
/// rotation can run again.
pub async fn rotate(
    jobs: &Jobs,
    old_key_id: i64,
    new_key_id: i64,
    started_by_user_id: Option<i64>,
) {
    let db = &jobs.0.db;
    let Ok(Some(old_uuid)) = private_keys::uuid_by_id(db, old_key_id).await else {
        return;
    };
    // `WithoutOverlapping("rotate-private-key-{id}")->expireAfter($timeout + 60)->dontRelease()`.
    let lock = format!("rotate-private-key-{old_key_id}");
    if !MARKS.add(
        lock.clone(),
        Duration::from_secs(ROTATION_TIMEOUT_SECONDS + 60),
    ) {
        return;
    }
    let run = tokio::time::timeout(
        Duration::from_secs(ROTATION_TIMEOUT_SECONDS),
        rotate_all(jobs, &old_uuid, old_key_id, new_key_id, started_by_user_id),
    )
    .await;
    if run.is_err() {
        // `failed()`.
        let mut failed = state(&old_uuid).unwrap_or_default();
        failed.insert("status".into(), json!("failed"));
        failed.insert(
            "error".into(),
            json!("The rotation stopped. Start it again."),
        );
        store_state(&old_uuid, &failed);
    }
    MARKS.forget(&lock);
}

async fn rotate_all(
    jobs: &Jobs,
    old_uuid: &str,
    old_key_id: i64,
    new_key_id: i64,
    started_by_user_id: Option<i64>,
) {
    let db = &jobs.0.db;
    let team_id: Option<i64> =
        sqlx::query_scalar("SELECT team_id::bigint FROM private_keys WHERE id = $1")
            .bind(old_key_id)
            .fetch_optional(db.pool())
            .await
            .ok()
            .flatten();
    let Some(team_id) = team_id else {
        return;
    };
    let Ok(Some(old_key)) = private_keys::find_by_id(db, team_id, old_key_id).await else {
        return;
    };
    let mut state_now = state(old_uuid).unwrap_or_default();
    let Ok(Some(new_key)) = private_keys::find_by_id(db, team_id, new_key_id).await else {
        state_now.insert("status".into(), json!("failed"));
        state_now.insert("error".into(), json!("The new key does not exist anymore."));
        store_state(old_uuid, &state_now);
        return;
    };
    state_now.insert("status".into(), json!("running"));
    state_now.insert("new_private_key_id".into(), json!(new_key.id));
    state_now.insert("servers".into(), json!({}));
    let server_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id::bigint FROM servers WHERE team_id = $1 AND private_key_id = $2 AND deleted_at IS NULL ORDER BY id",
    )
    .bind(team_id)
    .bind(old_key_id)
    .fetch_all(db.pool())
    .await
    .unwrap_or_default();
    let mut counts: Map<String, Value> = Map::new();
    for server_id in server_ids {
        let Ok(srv) = Srv::load(jobs, server_id).await else {
            continue;
        };
        let name = srv.str_column("name").unwrap_or_default().to_owned();
        let (status, message) = rotate_server(&srv, &old_key, &new_key).await;
        if let Some(Value::Object(servers)) = state_now.get_mut("servers") {
            servers.insert(
                srv.row.uuid.clone(),
                json!({"name": name, "status": status, "message": message}),
            );
        }
        store_state(old_uuid, &state_now);
        let moved = status == "moved" || status == OLD_KEY_PENDING;
        audit_log(
            jobs,
            team_id,
            "system.server.ssh_key_rotated",
            json!({
                "team_id": team_id,
                "server_uuid": srv.row.uuid,
                "server_name": name,
                "old_private_key_uuid": old_key.uuid,
                "new_private_key_uuid": new_key.uuid,
                "result": status,
            }),
            if moved { "info" } else { "warning" },
        )
        .await;
        let count = counts.get(&status).and_then(Value::as_i64).unwrap_or(0);
        counts.insert(status, json!(count + 1));
    }
    state_now.insert("status".into(), json!("finished"));
    store_state(old_uuid, &state_now);
    let all_moved = counts
        .keys()
        .all(|status| status == "moved" || status == OLD_KEY_PENDING);
    audit_log(
        jobs,
        team_id,
        "system.private_key.rotated",
        json!({
            "team_id": team_id,
            "private_key_uuid": old_key.uuid,
            "private_key_name": old_key.name,
            "new_private_key_uuid": new_key.uuid,
            "started_by_user_id": started_by_user_id,
            "servers": counts,
        }),
        if all_moved { "info" } else { "warning" },
    )
    .await;
}

/// `RotatePrivateKeyJob::rotateServer()`.
async fn rotate_server(srv: &Srv, old_key: &KeyRow, new_key: &KeyRow) -> (String, String) {
    if srv.info.transferred {
        return result(
            "skipped",
            "Another Coolify instance manages this server now.",
        );
    }
    if !authorized_keys::can_run_on(srv) {
        return result(
            "unreachable",
            "The server is disabled or has no IP address.",
        );
    }
    let jobs = &srv.jobs;
    let keys = (|| {
        let new_plain = decrypt(jobs, new_key)?;
        let old_plain = decrypt(jobs, old_key)?;
        let new_body = sshkey::public_key_body(&new_plain).ok_or("The private key is invalid.")?;
        let old_body = sshkey::public_key_body(&old_plain).ok_or("The private key is invalid.")?;
        let new_line = sshkey::authorized_keys_line(&new_plain, &new_key.uuid, &instance())
            .ok_or("The private key is invalid.")?;
        Ok::<_, String>((new_line, new_body, old_body))
    })();
    let (new_line, new_body, old_body) = match keys {
        Ok(keys) => keys,
        Err(e) => return result("failed", format!("Could not add the new key: {e}")),
    };
    if let Err(e) = authorized_keys::add(srv, &new_line, &old_body).await {
        return add_failure(&srv.info.ip, &e);
    }
    // `$probe = clone $server` with the new key: `validateConnection(justCheckingNewKey: true)`.
    let mut probe = Srv::from_row(jobs, srv.row.clone());
    probe.info.private_key_id = Some(new_key.id);
    let new_key_works = matches!(install::validate_new_key(&probe).await, Ok((true, _)));
    if !new_key_works {
        // The server stays on the old key. The next rotation adds the same new key again.
        let _ = authorized_keys::undo_add(srv, &new_line, &new_body, new_key.id, &old_body).await;
        return result(
            "failed",
            "The new key could not log in. Check the AuthorizedKeysFile setting of sshd.",
        );
    }
    if let Err(e) = servers::update_columns(
        srv.db(),
        srv.id(),
        &[("private_key_id", ColumnValue::Int(new_key.id))],
        Jobs::now(),
    )
    .await
    {
        return result("failed", format!("Could not add the new key: {e}"));
    }
    let delay = delay_seconds(srv.setting_int("dynamic_timeout"));
    spawn_remove_rotated_key(
        jobs,
        srv.id(),
        old_key.id,
        old_key.uuid.clone(),
        old_body,
        Duration::from_secs(delay.unsigned_abs()),
    );
    result(
        OLD_KEY_PENDING,
        format!(
            "Moved to the new key. The old key is removed from the server in {}, when jobs that started before the rotation have finished.",
            interval_for_humans(delay)
        ),
    )
}

/// `RemoveRotatedAuthorizedKeyJob::dispatch(...)->delay($delay)`: three tries with upstream's
/// backoff.
fn spawn_remove_rotated_key(
    jobs: &Jobs,
    server_id: i64,
    old_key_id: i64,
    old_key_uuid: String,
    old_public_key_body: String,
    delay: Duration,
) {
    let jobs = jobs.clone();
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let mut last_error = None;
        for attempt in 0..=REMOVE_BACKOFF_SECONDS.len() {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(REMOVE_BACKOFF_SECONDS[attempt - 1])).await;
            }
            match remove_rotated_key(
                &jobs,
                server_id,
                old_key_id,
                &old_key_uuid,
                &old_public_key_body,
            )
            .await
            {
                Ok(()) => return,
                Err(e) => last_error = Some(e),
            }
        }
        // `failed()`.
        if let (Some(e), Ok(srv)) = (last_error, Srv::load(&jobs, server_id).await) {
            report(
                &srv,
                &old_key_uuid,
                "moved_old_not_removed",
                &format!(
                    "The old key was not removed: {}",
                    error_without_host(&srv.info.ip, &e.message)
                ),
            )
            .await;
        }
    });
}

/// `RemoveRotatedAuthorizedKeyJob::handle()`: removes the old key from a server some time after
/// a rotation moved the server to the new key. Safe to run again.
pub async fn remove_rotated_key(
    jobs: &Jobs,
    server_id: i64,
    old_key_id: i64,
    old_key_uuid: &str,
    old_public_key_body: &str,
) -> Result<(), KeyEditError> {
    let Ok(srv) = Srv::load(jobs, server_id).await else {
        return Ok(());
    };
    if server_uses_old_key(&srv, old_key_id, old_public_key_body).await {
        report(
            &srv,
            old_key_uuid,
            "moved_old_not_removed",
            "The old key stays on the server, because the server uses it again.",
        )
        .await;
        return Ok(());
    }
    if !authorized_keys::can_run_on(&srv) {
        report(
            &srv,
            old_key_uuid,
            "moved_old_not_removed",
            "The old key stays on the server, because Coolify does not run commands on it.",
        )
        .await;
        return Ok(());
    }
    if !authorized_keys::remove_public_key(&srv, old_public_key_body, Some(old_key_id)).await? {
        report(
            &srv,
            old_key_uuid,
            "moved_old_not_removed",
            "The old key stays on the server, because another server for this host uses it.",
        )
        .await;
        return Ok(());
    }
    report(
        &srv,
        old_key_uuid,
        "moved",
        "Moved to the new key. The old key was removed.",
    )
    .await;
    Ok(())
}

/// `serverUsesOldKey()`.
async fn server_uses_old_key(srv: &Srv, old_key_id: i64, old_public_key_body: &str) -> bool {
    if srv.info.private_key_id == Some(old_key_id) {
        return true;
    }
    let Some(old_fingerprint) = sshkey::public_key_fingerprint(old_public_key_body) else {
        return false;
    };
    let Some(current_id) = srv.info.private_key_id else {
        return false;
    };
    let Ok(Some(current)) = private_keys::find_by_id(srv.db(), srv.info.team_id, current_id).await
    else {
        return false;
    };
    let stored = current
        .record
        .get("fingerprint")
        .and_then(Value::as_str)
        .filter(|f| !f.is_empty())
        .map(str::to_owned);
    let fingerprint = stored.or_else(|| {
        decrypt(&srv.jobs, &current)
            .ok()
            .and_then(|plain| sshkey::inspect(&plain))
            .map(|i| i.fingerprint)
    });
    fingerprint.as_deref() == Some(old_fingerprint.as_str())
}

/// `report()`: updates the row of the server in the result of the rotation, while the rotation
/// still waits for this job.
async fn report(srv: &Srv, old_key_uuid: &str, status: &str, message: &str) {
    audit_log(
        &srv.jobs,
        srv.info.team_id,
        "system.server.ssh_key_old_removed",
        json!({
            "team_id": srv.info.team_id,
            "server_uuid": srv.row.uuid,
            "server_name": srv.str_column("name"),
            "old_private_key_uuid": old_key_uuid,
            "result": status,
        }),
        if status == "moved" { "info" } else { "warning" },
    )
    .await;
    let Some(mut stored) = state(old_key_uuid) else {
        return;
    };
    let pending = stored
        .get("servers")
        .and_then(|s| s.get(&srv.row.uuid))
        .and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        == Some(OLD_KEY_PENDING);
    if !pending {
        return;
    }
    if let Some(Value::Object(row)) = stored
        .get_mut("servers")
        .and_then(|s| s.get_mut(&srv.row.uuid))
    {
        row.insert("status".into(), json!(status));
        row.insert("message".into(), json!(message));
    }
    store_state(old_key_uuid, &stored);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_old_key_waits_for_running_jobs() {
        assert_eq!(delay_seconds(None), 3600);
        assert_eq!(delay_seconds(Some(3600)), 3900);
        assert_eq!(delay_seconds(Some(60)), 3600);
        assert_eq!(interval_for_humans(3600), "1 hour");
        assert_eq!(interval_for_humans(3900), "1 hour 5 minutes");
        assert_eq!(interval_for_humans(7200), "2 hours");
        // Two parts at most.
        assert_eq!(interval_for_humans(3661), "1 hour 1 minute");
        assert_eq!(interval_for_humans(90_061), "1 day 1 hour");
    }

    #[test]
    fn ssh_errors_do_not_show_the_address() {
        assert_eq!(
            error_without_host(
                "203.0.113.7",
                "ssh: connect to host 203.0.113.7 port 22: Connection refused"
            ),
            "Connection refused"
        );
        assert_eq!(
            error_without_host(
                "203.0.113.7",
                "root@203.0.113.7: Permission denied (publickey)."
            ),
            "root@the server: Permission denied (publickey)."
        );
    }

    #[test]
    fn add_failures_map_to_the_result_of_the_server() {
        let e = |code| KeyEditError {
            message: "ssh: connect to host 203.0.113.7 port 22: Connection timed out".into(),
            code,
        };
        assert_eq!(
            add_failure("203.0.113.7", &e(Some(3))).0,
            "old_key_not_found"
        );
        assert_eq!(
            add_failure("203.0.113.7", &e(Some(255))),
            (
                "unreachable".to_owned(),
                "Could not connect: Connection timed out".to_owned()
            )
        );
        assert_eq!(
            add_failure("203.0.113.7", &e(Some(1))),
            (
                "failed".to_owned(),
                "Could not add the new key: Connection timed out".to_owned()
            )
        );
    }

    #[test]
    fn the_new_key_is_named_after_the_old_key() {
        let day = chrono::NaiveDate::from_ymd_opt(2026, 11, 12).unwrap_or_default();
        assert_eq!(rotated_key_name("prod", day), "prod (rotated 2026-11-12)");
        // A second rotation does not stack the suffix.
        assert_eq!(
            rotated_key_name("prod (rotated 2026-10-01) (rotated 2026-10-02)", day),
            "prod (rotated 2026-11-12)"
        );
        let long = "x".repeat(300);
        assert_eq!(
            rotated_key_name(&long, day),
            format!("{} (rotated 2026-11-12)", "x".repeat(200))
        );
    }

    #[test]
    fn the_result_is_kept_per_key() {
        assert_eq!(cache_key("abc"), "private-key-rotation:abc");
        let mut s = Map::new();
        s.insert("status".into(), json!("running"));
        store_state("test-rotation-state", &s);
        assert_eq!(
            state("test-rotation-state").and_then(|s| s.get("status").cloned()),
            Some(json!("running"))
        );
        assert_eq!(state("no-such-key"), None);
    }
}
