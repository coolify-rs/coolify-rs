//! `CheckForUpdatesJob`, `UpdateCoolifyJob`, `UpdateCoolify` and `CoolifyUpgradeStatus`, for a
//! coolify-rs installation: the version file and `upgrade.sh` come from the addresses the installer
//! sets, never from upstream's CDN. Without them, updates are off.

use crate::git_http::SendRetry as _;
use chrono::Timelike;
use coolify_compat::cron_expression::{in_zone, time_zone};
use std::cmp::Ordering;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::Jobs;
use crate::server_ops::{Srv, helper_version, shell_arg, version_compare};

pub mod auto_update;

/// `CoolifyUpgradeStatus::FILE`.
pub const STATUS_FILE: &str = "/data/coolify/source/.upgrade-status";
/// `CoolifyUpgradeStatus::LOCK_FILE`: `upgrade.sh` holds this lock while it writes a status, so
/// an error write cannot interleave with it.
pub const STATUS_LOCK_FILE: &str = "/data/coolify/source/.upgrade-status.lock";
/// `CoolifyUpgradeStatus::STALE_AFTER_MINUTES`.
const STALE_AFTER_MINUTES: i64 = 10;
/// `CoolifyUpgradeStatus::RUNNING_LOCK_EXPIRES_AFTER_MINUTES`.
const RUNNING_LOCK_EXPIRES_AFTER_MINUTES: i64 = 15;

fn setting(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// `constants.coolify.versions_url`.
fn versions_url() -> Option<String> {
    setting("COOLIFY_RS_VERSIONS_URL")
}

/// `constants.coolify.upgrade_script_url`.
fn upgrade_script_url() -> Option<String> {
    setting("COOLIFY_RS_UPGRADE_SCRIPT_URL")
}

fn versions_path() -> PathBuf {
    PathBuf::from(setting("COOLIFY_RS_DATA_DIR").unwrap_or_else(|| "/data/coolify-rs".to_owned()))
        .join("versions.json")
}

/// `refresh_versions_file`: `None` when the file could not be fetched.
pub async fn refresh_versions() -> Option<Value> {
    let response = reqwest::Client::new()
        .get(versions_url()?)
        .timeout(Duration::from_secs(10))
        .send_retry()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let text = response.text().await.ok()?;
    let data: Value = serde_json::from_str(&text).ok()?;
    let _ = std::fs::write(versions_path(), &text);
    Some(data)
}

/// The Coolify version of a versions file: `coolify.version` (v4.6.0) or `coolify.v4.version`.
fn version_of(versions: &Value) -> Option<String> {
    versions
        .pointer("/coolify/version")
        .or_else(|| versions.pointer("/coolify/v4/version"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// The update channels, most stable first.
pub const CHANNELS: [&str; 3] = ["stable", "preview", "nightly"];

/// `update_channel`, `update_custom_tag` and `previous_update_channel` of the instance settings
/// (v4.6.0, `add_update_channel_to_instance_settings_table`). An older schema is on `stable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateChannel {
    pub channel: String,
    pub custom_tag: Option<String>,
    pub previous: Option<String>,
}

impl Default for UpdateChannel {
    fn default() -> Self {
        Self {
            channel: "stable".to_owned(),
            custom_tag: None,
            previous: None,
        }
    }
}

impl UpdateChannel {
    pub fn from_settings(row: &serde_json::Map<String, Value>) -> Self {
        let text = |k: &str| {
            row.get(k)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        Self {
            channel: text("update_channel").unwrap_or_else(|| "stable".to_owned()),
            custom_tag: text("update_custom_tag"),
            previous: text("previous_update_channel"),
        }
    }

    pub async fn load(db: &coolify_db::Db) -> Self {
        let row: Option<Value> =
            sqlx::query_scalar("SELECT to_jsonb(s) FROM instance_settings s WHERE id = 0")
                .fetch_optional(db.pool())
                .await
                .ok()
                .flatten();
        row.as_ref()
            .and_then(Value::as_object)
            .map(Self::from_settings)
            .unwrap_or_default()
    }

    pub fn is_custom(&self) -> bool {
        self.channel == "custom"
    }
}

fn rank(channel: Option<&str>) -> Option<usize> {
    channel.and_then(|c| CHANNELS.iter().position(|x| *x == c))
}

/// `InstanceSettings::switchUpdateChannel()`: the `previous_update_channel` after a switch. After a
/// switch to a more stable channel, the instance keeps the updates of the channel it left until the
/// stable release of its version is out.
pub fn previous_after_switch(
    from: &str,
    to: &str,
    previous: Option<&str>,
) -> Option<Option<String>> {
    if from == to {
        return None;
    }
    let (from_rank, to_rank) = (rank(Some(from)), rank(Some(to)));
    Some(match (from_rank, to_rank) {
        (Some(f), Some(t)) if t < f => {
            // `array_search($followed, ...) > $fromRank`: false compares as 0.
            if rank(previous).unwrap_or(0) > f {
                previous.map(str::to_owned)
            } else {
                Some(from.to_owned())
            }
        }
        _ => None,
    })
}

/// `version_compare($version, $release, '<')` as PHP orders a pre-release: below its release.
/// [`version_compare`] puts a word part at 0, so `4.7.0-beta.1` would be above `4.7.0` there.
fn before_release(version: &str, release: &str) -> bool {
    let core_end = version
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(version.len());
    let (core, suffix) = version.split_at(core_end);
    match version_compare(core.trim_end_matches('.'), release) {
        Ordering::Less => true,
        Ordering::Equal => !suffix.is_empty(),
        Ordering::Greater => false,
    }
}

/// `newest_update_channel()`: the channel of the newest Coolify version this instance takes, its
/// update channel and the more stable ones, and the channel it left below the stable release of
/// the running version. `channels` holds the versions of each channel.
pub fn newest_update_channel(channels: &Value, settings: &UpdateChannel, running: &str) -> String {
    let follows = rank(Some(&settings.channel)).unwrap_or(0);
    let until_release = settings
        .previous
        .as_deref()
        .map_or(-1, |p| rank(Some(p)).unwrap_or(0) as i64);
    let release = {
        let re = regex::Regex::new(r"^(\d+\.\d+\.\d+).*").unwrap_or_else(|_| unreachable!());
        re.replace(running, "$1").into_owned()
    };
    let version_of_channel = |c: &str| channels.get(c).and_then(version_of);
    let mut newest = "stable";
    for (i, &channel) in CHANNELS.iter().enumerate() {
        let Some(version) = version_of_channel(channel) else {
            continue;
        };
        let current = version_of_channel(newest).unwrap_or_default();
        if version_compare(&version, &current) != Ordering::Greater {
            continue;
        }
        if i <= follows || (i as i64 <= until_release && before_release(&version, &release)) {
            newest = channel;
        }
    }
    newest.to_owned()
}

/// The versions this instance follows in a versions file: the file of the newest channel when it
/// lists channels (`{"stable": {...}, "preview": {...}, "nightly": {...}}`, v4.6.0), else the
/// file itself, a stable-only file of an older release.
pub fn channel_versions(file: &Value, settings: &UpdateChannel, running: &str) -> (String, Value) {
    if file.get("stable").is_some_and(Value::is_object) {
        let channel = newest_update_channel(file, settings, running);
        let versions = file.get(&channel).cloned().unwrap_or(Value::Null);
        (channel, versions)
    } else {
        ("stable".to_owned(), file.clone())
    }
}

/// `upgrade_script_url($channel)`: a channel's `upgrade.sh` is next to the versions file.
fn upgrade_script_url_for(channel: &str) -> Option<String> {
    if channel == "stable" {
        return upgrade_script_url();
    }
    let versions = versions_url()?;
    let base = versions
        .rsplit_once('/')
        .map_or(versions.as_str(), |(b, _)| b);
    Some(format!("{base}/{channel}/upgrade.sh"))
}

/// The newest version this instance takes from a versions file.
pub fn latest_in(file: &Value, settings: &UpdateChannel, running: &str) -> Option<String> {
    version_of(&channel_versions(file, settings, running).1)
}

/// `CheckForUpdatesJob::handle`. `true` when `new_version_available` is up to date.
pub async fn check_for_updates(jobs: &Jobs, current: &str) -> bool {
    // `AutoUpdateNotifier::checkResult()`: the new version reports the result of the attempt.
    auto_update::check_result(jobs, current).await;
    let settings = UpdateChannel::load(&jobs.0.db).await;
    let Some(latest) = refresh_versions()
        .await
        .as_ref()
        .and_then(|f| latest_in(f, &settings, current))
    else {
        return false;
    };
    // A custom image is installed by hand only (v4.6.0).
    let newer = !settings.is_custom() && version_compare(&latest, current) == Ordering::Greater;
    sqlx::query(
        "UPDATE instance_settings SET new_version_available = $1, updated_at = $2 WHERE id = 0",
    )
    .bind(newer)
    .bind(Utc::now().naive_utc())
    .execute(jobs.0.db.pool())
    .await
    .is_ok()
}

/// `CoolifyUpgradeStatus::isRunning`.
pub fn is_running(content: &str, now: DateTime<Utc>) -> bool {
    let parts: Vec<&str> = content.trim().split('|').collect();
    if parts.len() < 3 || !["1", "2", "3", "4", "5"].contains(&parts[0]) {
        return false;
    }
    DateTime::parse_from_rfc3339(parts[2].trim()).is_ok_and(|at| {
        (now - at.with_timezone(&Utc)).num_minutes() <= RUNNING_LOCK_EXPIRES_AFTER_MINUTES
    })
}

/// `CoolifyUpgradeStatus::fromFile`.
pub fn status_from_file(content: &str, running: &str, target: &str, now: DateTime<Utc>) -> Value {
    let none = json!({"status": "none", "running_version": running, "target_version": target});
    let content = content.trim();
    let parts: Vec<&str> = content.split('|').collect();
    if parts.len() < 3 {
        return none;
    }
    let Ok(at) = DateTime::parse_from_rfc3339(parts[2].trim()) else {
        return none;
    };
    if (now - at.with_timezone(&Utc)).num_minutes() > STALE_AFTER_MINUTES {
        return none;
    }
    let (step, message) = (parts[0], parts[1]);
    if step == "error" {
        return json!({"status": "error", "step": 0, "message": message, "running_version": running, "target_version": target});
    }
    let step: i64 = step.parse().unwrap_or(0);
    let reached = has_reached_target_version(running, target);
    if step >= 6 && !reached {
        return json!({
            "status": "in_progress",
            "step": step,
            "message": format!("Waiting for coolify-rs {target} to come online..."),
            "running_version": running,
            "target_version": target,
        });
    }
    json!({
        "status": if step >= 6 { "complete" } else { "in_progress" },
        "step": step,
        "message": message,
        "running_version": running,
        "target_version": target,
    })
}

/// `CoolifyUpgradeStatus::hasReachedTargetVersion`: an image tag that is not a version (v4.6.0)
/// counts as reached.
pub fn has_reached_target_version(running: &str, target: &str) -> bool {
    if running.is_empty() || target.is_empty() {
        return false;
    }
    // `preg_match('/^\d+\.\d+/', $targetVersion)`.
    let is_version = {
        let mut parts = target.splitn(3, '.');
        let numeric = |p: Option<&str>| {
            p.is_some_and(|p| p.chars().next().is_some_and(|c| c.is_ascii_digit()))
        };
        let first = parts.next();
        first.is_some_and(|f| !f.is_empty() && f.chars().all(|c| c.is_ascii_digit()))
            && numeric(parts.next())
    };
    !is_version || version_compare(running, target) != Ordering::Less
}

/// `CoolifyUpgradeStatus::compareAndWriteCommand($expected, $line)`: a shell command that writes
/// `line` to the status file only if the file still contains `expected`. It holds the lock that
/// `upgrade.sh` takes for its status writes, if `flock` is available.
pub fn compare_and_write_command(expected: &str, line: &str) -> String {
    use base64::Engine as _;
    let b64 = |text: &str| base64::engine::general_purpose::STANDARD.encode(text);
    let script = concat!(
        "if command -v flock >/dev/null 2>&1; then exec 9>\"$2\"; flock -w 10 9 || exit 1; fi; ",
        "if [ \"$(cat \"$1\" 2>/dev/null)\" = \"$(printf %s \"$3\" | base64 -d)\" ]; then printf %s \"$4\" | base64 -d > \"$1\"; echo >> \"$1\"; fi"
    );
    format!(
        "sh -c {} _ {} {} {} {}",
        shell_arg(script),
        shell_arg(STATUS_FILE),
        shell_arg(STATUS_LOCK_FILE),
        shell_arg(&b64(expected.trim())),
        shell_arg(&b64(line)),
    )
}

/// The line `writeError()` writes: `error|<message without separators>|<ISO 8601 time>`.
pub fn error_status_line(message: &str, now: DateTime<Utc>) -> String {
    // `trim(preg_replace('/[|\r\n]+/', ' ', $message))`.
    let mut clean = String::with_capacity(message.len());
    let mut in_run = false;
    for c in message.chars() {
        if matches!(c, '|' | '\r' | '\n') {
            if !in_run {
                clean.push(' ');
            }
            in_run = true;
        } else {
            clean.push(c);
            in_run = false;
        }
    }
    format!(
        "error|{}|{}",
        clean.trim(),
        now.format("%Y-%m-%dT%H:%M:%S+00:00")
    )
}

/// `CoolifyUpgradeStatus::writeError($server, $message)`: writes an error that stopped an upgrade
/// before the upgrade script started, so the upgrade modal shows it. Does nothing while an
/// upgrade script runs, because the error then belongs to this attempt only. The write happens
/// only if the status file did not change after the read, so a status that an upgrade script
/// wrote in the meantime is kept.
pub async fn write_error(jobs: &Jobs, message: &str) -> Result<(), String> {
    let srv = Srv::load(jobs, 0).await?;
    let current = srv
        .run_quiet(
            &[format!("cat {STATUS_FILE} 2>/dev/null || true")],
            "upgrade-status",
        )
        .await
        .unwrap_or_default();
    let now = Utc::now();
    if is_running(&current, now) {
        return Ok(());
    }
    let command = compare_and_write_command(&current, &error_status_line(message, now));
    // `instant_remote_process(..., throwError: false)`.
    let _ = srv.run_quiet(&[command], "upgrade-status").await;
    Ok(())
}

/// `Upgrade::getUpgradeStatus`, read from the server.
pub async fn upgrade_status(jobs: &Jobs, running: &str, target: &str) -> Value {
    let Ok(srv) = Srv::load(jobs, 0).await else {
        return json!({"status": "none"});
    };
    let content = srv
        .run_quiet(
            &[format!("cat {STATUS_FILE} 2>/dev/null || echo ''")],
            "upgrade-status",
        )
        .await
        .unwrap_or_default();
    status_from_file(&content, running, target, Utc::now())
}

/// The exception of `UpdateCoolify::backupDatabase` when the backup did not succeed. `reason` is
/// the message of the backup execution.
fn backup_failed_message(reason: Option<&str>) -> String {
    let reason = reason
        .filter(|r| !r.is_empty() && *r != "0")
        .map(|r| format!(" Error: {r}"))
        .unwrap_or_default();
    format!(
        "The database backup failed, so the update was not started.{reason} You can turn off \"Backup before update\" in Settings > Backup to update without a backup."
    )
}

/// `UpdateCoolify::backupDatabase`: the instance database's backup schedule, when there is one
/// and it is enabled. `upgrade.sh` always takes its own dump as well.
async fn backup_database(jobs: &Jobs) -> Result<(), String> {
    let db = jobs.0.db.pool();
    let enabled: bool = sqlx::query_scalar(
        "SELECT COALESCE(is_backup_before_update_enabled, false) FROM instance_settings WHERE id = 0",
    )
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())?;
    // `! $backup?->enabled`: the first schedule, when it is enabled.
    let backup: Option<(i64, Option<bool>)> = sqlx::query_as(
        "SELECT id, enabled FROM scheduled_database_backups \
         WHERE database_id = 0 AND database_type = 'App\\Models\\StandalonePostgresql' ORDER BY id LIMIT 1",
    )
    .fetch_optional(db)
    .await
    .map_err(|e| e.to_string())?;
    let Some((backup, _)) = backup.filter(|(_, on)| enabled && *on == Some(true)) else {
        return Ok(());
    };
    jobs.run_database_backup(backup).await;
    let execution: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT status::text, message::text FROM scheduled_database_backup_executions \
         WHERE scheduled_database_backup_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(backup)
    .fetch_optional(db)
    .await
    .map_err(|e| e.to_string())?;
    let (status, message) = execution.unwrap_or_default();
    if status.as_deref() == Some("success") {
        Ok(())
    } else {
        Err(backup_failed_message(message.as_deref()))
    }
}

/// Why `UpdateCoolify::handle` did not start an upgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// `UpgradeAlreadyRunningException`: another upgrade script is still running. This is not a
    /// failure of the running upgrade.
    AlreadyRunning(String),
    Failed(String),
}

impl UpdateError {
    pub fn message(&self) -> &str {
        match self {
            UpdateError::AlreadyRunning(m) | UpdateError::Failed(m) => m,
        }
    }
}

impl From<String> for UpdateError {
    fn from(message: String) -> Self {
        UpdateError::Failed(message)
    }
}

/// The message of `UpgradeAlreadyRunningException`.
fn already_running_message() -> String {
    format!(
        "Another coolify-rs upgrade is already running. Wait for it to finish. If it has stopped, you can upgrade again {RUNNING_LOCK_EXPIRES_AFTER_MINUTES} minutes after its last status update."
    )
}

/// `UpdateCoolify::ensureNoUpgradeIsRunning`.
async fn ensure_no_upgrade_is_running(srv: &Srv) -> Result<(), UpdateError> {
    let status = srv
        .run_quiet(
            &[format!("cat {STATUS_FILE} 2>/dev/null || true")],
            "upgrade-status",
        )
        .await
        .unwrap_or_default();
    if is_running(&status, Utc::now()) {
        return Err(UpdateError::AlreadyRunning(already_running_message()));
    }
    Ok(())
}

/// `UpdateCoolify::handle`. `Ok(false)` when there is nothing to do.
pub async fn update_coolify(jobs: &Jobs, current: &str, manual: bool) -> Result<bool, String> {
    start_update(jobs, current, manual)
        .await
        .map_err(|e| e.message().to_owned())
}

/// `Upgrade::upgrade`: `UpdateCoolify::run(manual_update: true)`, and an error that stopped the
/// upgrade before the script started is written to the status file, so the upgrade modal of
/// every viewer shows it.
pub async fn upgrade_manually(jobs: &Jobs, current: &str) -> Result<bool, String> {
    match start_update(jobs, current, true).await {
        Ok(started) => Ok(started),
        Err(e) => {
            if let Err(status_error) = write_error(jobs, e.message()).await {
                tracing::warn!(error = %status_error, "could not write the upgrade error");
            }
            Err(e.message().to_owned())
        }
    }
}

/// `UpdateCoolifyJob::handle` after `CheckForUpdatesJob`: starts the automatic update and tells
/// the root team when it could not start.
pub async fn run_update_job(jobs: &Jobs, current: &str) {
    match start_update(jobs, current, false).await {
        Ok(true) => tracing::info!(
            "coolify-rs upgrade script started. Result: {STATUS_FILE}, log: /data/coolify/source/upgrade-*.log."
        ),
        Ok(false) => {}
        // The running upgrade keeps its saved attempt, so its result is still reported.
        Err(UpdateError::AlreadyRunning(message)) => {
            tracing::warn!("Automatic update skipped: {message}");
        }
        Err(UpdateError::Failed(message)) => {
            tracing::error!("UpdateCoolifyJob failed: {message}");
            if let Err(status_error) = write_error(jobs, &message).await {
                tracing::warn!(error = %status_error, "could not write the upgrade error");
            }
            auto_update::failed(jobs, &message).await;
        }
    }
}

/// `UpdateCoolify::handle` with the reason an upgrade did not start.
async fn start_update(jobs: &Jobs, current: &str, manual: bool) -> Result<bool, UpdateError> {
    let settings = UpdateChannel::load(&jobs.0.db).await;
    // A custom image (v4.6.0) installs only when upgraded to by hand, from ghcr.io.
    if settings.is_custom() {
        let Some(tag) = settings.custom_tag.clone().filter(|_| manual) else {
            return Ok(false);
        };
        let Some(script_url) = upgrade_script_url() else {
            return Err("This installation has no update address (COOLIFY_RS_CDN)."
                .to_owned()
                .into());
        };
        backup_database(jobs).await?;
        let srv = Srv::load(jobs, 0).await?;
        ensure_no_upgrade_is_running(&srv).await?;
        return Ok(run_upgrade(jobs, srv, &script_url, &tag, Some("ghcr.io")).await?);
    }
    let file = match refresh_versions().await {
        Some(file) => Some(file),
        None => std::fs::read_to_string(versions_path())
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok()),
    };
    let (channel, versions) = file
        .as_ref()
        .map(|f| channel_versions(f, &settings, current))
        .ok_or_else(|| "Cannot determine latest version.".to_owned())?;
    let latest =
        version_of(&versions).ok_or_else(|| "Cannot determine latest version.".to_owned())?;
    let Some(script_url) = upgrade_script_url_for(&channel) else {
        return Err("This installation has no update address (COOLIFY_RS_CDN)."
            .to_owned()
            .into());
    };
    let db = jobs.0.db.pool();
    if !manual {
        let auto: bool = sqlx::query_scalar(
            "SELECT COALESCE(is_auto_update_enabled, false) FROM instance_settings WHERE id = 0",
        )
        .fetch_one(db)
        .await
        .map_err(|e| e.to_string())?;
        if !auto || version_compare(&latest, current) != Ordering::Greater {
            return Ok(false);
        }
    }
    if version_compare(&latest, current) == Ordering::Less {
        return Err(format!(
            "Cannot downgrade from {current} to {latest}. If you need to downgrade, please do so manually via Docker commands."
        )
        .into());
    }
    backup_database(jobs).await?;
    // Check the lock before the attempt is saved, so a running upgrade keeps its own saved
    // attempt.
    let srv = Srv::load(jobs, 0).await?;
    ensure_no_upgrade_is_running(&srv).await?;
    if !manual {
        auto_update::save_attempt(jobs, &latest).await?;
    }
    let started = run_upgrade(jobs, srv, &script_url, &latest, None).await?;
    if !manual {
        auto_update::started(jobs, current, &latest);
    }
    Ok(started)
}

/// `UpdateCoolify::update($registryUrl)`: runs `upgrade.sh` for the version or image tag.
async fn run_upgrade(
    jobs: &Jobs,
    srv: Srv,
    script_url: &str,
    latest: &str,
    registry_url: Option<&str>,
) -> Result<bool, String> {
    let db = jobs.0.db.pool();
    let (helper, registry): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT helper_version, docker_registry_url FROM instance_settings WHERE id = 0",
    )
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())?;
    let helper = helper_version(helper.as_deref());
    let registry = registry_url.map(str::to_owned).unwrap_or_else(|| {
        registry
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| "docker.io".to_owned())
    });
    let commands = vec![
        format!(
            "curl -fsSL {} -o /data/coolify/source/upgrade.sh",
            shell_arg(script_url)
        ),
        format!(
            "bash /data/coolify/source/upgrade.sh {} {} {}",
            shell_arg(latest),
            shell_arg(&helper),
            shell_arg(&registry)
        ),
    ];
    // `remote_process`: the script detaches before it stops this container.
    tokio::spawn(async move {
        if let Err(e) = srv.run(&commands, "upgrade").await {
            tracing::error!(error = %e, "the upgrade script failed");
        }
    });
    sqlx::query(
        "UPDATE instance_settings SET new_version_available = false, updated_at = $1 WHERE id = 0",
    )
    .bind(Utc::now().naive_utc())
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(true)
}

type ScheduleRow = (Option<String>, Option<String>, bool, Option<String>);

/// `->cron($frequency)->timezone($instanceTimezone)`.
fn due_in_zone(frequency: &str, timezone: Option<&str>, now: DateTime<Utc>) -> bool {
    let Ok(cron) = crate::scheduler::parse_frequency(frequency) else {
        return false;
    };
    let zone = timezone.and_then(time_zone).unwrap_or(chrono_tz::UTC);
    cron.is_due(&in_zone(
        now.with_second(0)
            .unwrap_or(now)
            .with_nanosecond(0)
            .unwrap_or(now),
        zone,
    ))
}

/// The `UpdateCoolifyJob` line of `Kernel::scheduleUpdates`: runs in `coolify-rs boot` only.
/// `CheckForUpdatesJob` is in [`crate::kernel`].
pub fn spawn_schedule(jobs: &Jobs, current: String) {
    if versions_url().is_none() {
        tracing::info!("no update address: update checks are off");
        return;
    }
    let jobs = jobs.clone();
    tokio::spawn(async move {
        loop {
            let now = Utc::now();
            let wait = 60 - now.second();
            tokio::time::sleep(Duration::from_secs(u64::from(wait))).await;
            let row: Result<ScheduleRow, _> = sqlx::query_as(
                "SELECT update_check_frequency, auto_update_frequency, COALESCE(is_auto_update_enabled, false), instance_timezone \
                 FROM instance_settings WHERE id = 0",
            )
            .fetch_one(jobs.0.db.pool())
            .await;
            let Ok((_check, auto, enabled, timezone)) = row else {
                continue;
            };
            let due = |frequency: &Option<String>| {
                frequency
                    .as_deref()
                    .is_some_and(|f| due_in_zone(f, timezone.as_deref(), Utc::now()))
            };
            // `UpdateCoolifyJob::handle`.
            if enabled && due(&auto) {
                if !check_for_updates(&jobs, &current).await {
                    continue;
                }
                run_update_job(&jobs, &current).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn channel(channel: &str, previous: Option<&str>) -> UpdateChannel {
        UpdateChannel {
            channel: channel.to_owned(),
            custom_tag: None,
            previous: previous.map(str::to_owned),
        }
    }

    /// `InstanceSettings::switchUpdateChannel()` (v4.6.0).
    #[test]
    fn switching_update_channels() {
        assert_eq!(previous_after_switch("stable", "stable", None), None);
        // A more stable channel remembers the one it left.
        assert_eq!(
            previous_after_switch("nightly", "stable", None),
            Some(Some("nightly".to_owned()))
        );
        assert_eq!(
            previous_after_switch("nightly", "preview", None),
            Some(Some("nightly".to_owned()))
        );
        // ...or the less stable one it still follows.
        assert_eq!(
            previous_after_switch("preview", "stable", Some("nightly")),
            Some(Some("nightly".to_owned()))
        );
        // A less stable channel, or custom, forgets it.
        assert_eq!(
            previous_after_switch("stable", "nightly", Some("nightly")),
            Some(None)
        );
        assert_eq!(previous_after_switch("nightly", "custom", None), Some(None));
        assert_eq!(previous_after_switch("custom", "stable", None), Some(None));
    }

    /// `newest_update_channel()` (v4.6.0).
    #[test]
    fn newest_channel_of_the_versions_file() {
        let file = json!({
            "stable": {"coolify": {"version": "4.6.0"}},
            "preview": {"coolify": {"version": "4.7.0-beta.1"}},
            "nightly": {"coolify": {"version": "4.7.0-beta.2"}},
        });
        assert_eq!(
            newest_update_channel(&file, &channel("stable", None), "4.6.0"),
            "stable"
        );
        assert_eq!(
            newest_update_channel(&file, &channel("preview", None), "4.6.0"),
            "preview"
        );
        assert_eq!(
            newest_update_channel(&file, &channel("nightly", None), "4.6.0"),
            "nightly"
        );
        // Back on stable from nightly: the nightlies of a version not yet released as stable.
        assert_eq!(
            newest_update_channel(&file, &channel("stable", Some("nightly")), "4.7.0-beta.1"),
            "nightly"
        );
        assert_eq!(
            newest_update_channel(&file, &channel("stable", Some("nightly")), "4.6.0"),
            "stable"
        );
        assert!(before_release("4.7.0-beta.1", "4.7.0"));
        assert!(before_release("4.6.9", "4.7.0"));
        assert!(!before_release("4.7.0", "4.7.0"));
        assert!(!before_release("4.8.0-beta.1", "4.7.0"));
        // A file of an older release lists no channels: its version is the stable one.
        let old = json!({"coolify": {"v4": {"version": "4.5.3-rs.1"}}});
        let (name, versions) = channel_versions(&old, &channel("nightly", None), "4.5.3");
        assert_eq!(name, "stable");
        assert_eq!(version_of(&versions).as_deref(), Some("4.5.3-rs.1"));
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc.timestamp_opt(0, 0).unwrap())
    }

    /// Release builds report `COOLIFY_VERSION=4.5.3-rs.N`; the update check compares them with
    /// the feed's latest version and the upgrade status with the target version.
    #[test]
    fn release_versions_compare() {
        assert_eq!(
            version_compare("4.5.3-rs.2", "4.5.3-rs.1"),
            Ordering::Greater
        );
        assert_eq!(
            version_compare("4.5.3-rs.10", "4.5.3-rs.9"),
            Ordering::Greater
        );
        assert_eq!(
            version_compare("4.5.4-rs.1", "4.5.3-rs.9"),
            Ordering::Greater
        );
        assert_eq!(version_compare("4.5.3-rs.1", "4.5.3-rs.1"), Ordering::Equal);
        assert_eq!(version_compare("4.5.3-rs.1", "4.5.4-rs.1"), Ordering::Less);
        // Upstream's own release number never offers an rs build of it as an upgrade.
        assert_ne!(version_compare("4.5.3", "4.5.3-rs.1"), Ordering::Greater);
        let now = at("2026-11-01T10:10:00+00:00");
        let done = status_from_file(
            "6|Done|2026-11-01T10:09:00+00:00",
            "4.5.3-rs.2",
            "4.5.3-rs.2",
            now,
        );
        assert_ne!(done["status"], "in_progress");
        let waiting = status_from_file(
            "6|Done|2026-11-01T10:09:00+00:00",
            "4.5.3-rs.1",
            "4.5.3-rs.2",
            now,
        );
        assert_eq!(waiting["status"], "in_progress");
    }

    #[test]
    fn the_target_version_is_reached_when_the_running_version_is_not_older() {
        assert!(has_reached_target_version("4.7.5", "4.7.5"));
        assert!(has_reached_target_version("4.7.6", "4.7.5"));
        assert!(!has_reached_target_version("4.7.4", "4.7.5"));
        assert!(!has_reached_target_version("", "4.7.5"));
        assert!(!has_reached_target_version("4.7.5", ""));
        // An image tag that is not a version counts as reached.
        assert!(has_reached_target_version("4.7.5", "pr-1234"));
    }

    #[test]
    fn an_error_is_written_only_over_the_status_that_was_read() {
        let command =
            compare_and_write_command(" 2|Pulling images|2026-11-10T12:00:00+00:00\n", "error|x|y");
        assert_eq!(
            command,
            "sh -c 'if command -v flock >/dev/null 2>&1; then exec 9>\"$2\"; flock -w 10 9 || exit 1; fi; if [ \"$(cat \"$1\" 2>/dev/null)\" = \"$(printf %s \"$3\" | base64 -d)\" ]; then printf %s \"$4\" | base64 -d > \"$1\"; echo >> \"$1\"; fi' _ '/data/coolify/source/.upgrade-status' '/data/coolify/source/.upgrade-status.lock' 'MnxQdWxsaW5nIGltYWdlc3wyMDI2LTExLTEwVDEyOjAwOjAwKzAwOjAw' 'ZXJyb3J8eHx5'"
        );
        // No status file yet: the expected content is empty.
        assert!(compare_and_write_command("", "error|x|y").contains(" '' 'ZXJyb3J8eHx5'"));
    }

    #[test]
    fn the_error_line_has_no_separators_of_the_status_file() {
        assert_eq!(
            error_status_line(
                " The backup failed.\r\nError: a|b || c \n",
                at("2026-11-10T12:00:05Z")
            ),
            "error|The backup failed. Error: a b   c|2026-11-10T12:00:05+00:00"
        );
        // The line is a status that the upgrade modal shows.
        let status = status_from_file(
            &error_status_line("no space left", at("2026-11-10T12:00:05Z")),
            "4.7.4",
            "4.7.5",
            at("2026-11-10T12:01:00Z"),
        );
        assert_eq!(status["status"], "error");
        assert_eq!(status["message"], "no space left");
    }

    #[test]
    fn a_failed_backup_tells_the_reason_and_the_way_out() {
        assert_eq!(
            backup_failed_message(Some("pg_dump: connection refused")),
            "The database backup failed, so the update was not started. Error: pg_dump: connection refused You can turn off \"Backup before update\" in Settings > Backup to update without a backup."
        );
        assert_eq!(
            backup_failed_message(None),
            "The database backup failed, so the update was not started. You can turn off \"Backup before update\" in Settings > Backup to update without a backup."
        );
        assert_eq!(backup_failed_message(Some("")), backup_failed_message(None));
    }

    #[test]
    fn an_upgrade_that_is_already_running_is_not_a_failure() {
        let running = UpdateError::AlreadyRunning(already_running_message());
        assert!(
            running
                .message()
                .starts_with("Another coolify-rs upgrade is already running.")
        );
        assert!(running.message().contains("15 minutes"));
        assert_eq!(
            UpdateError::from("boom".to_owned()),
            UpdateError::Failed("boom".to_owned())
        );
    }

    #[test]
    fn running_lock() {
        let now = at("2026-11-01T10:10:00+00:00");
        assert!(is_running("3|Pulling|2026-11-01T10:00:00+00:00\n", now));
        assert!(!is_running("6|Done|2026-11-01T10:09:00+00:00", now));
        assert!(!is_running("error|Bad|2026-11-01T10:09:00+00:00", now));
        assert!(!is_running("3|Pulling|2026-11-01T09:00:00+00:00", now));
        assert!(!is_running("", now));
    }

    #[test]
    fn status_of_the_file() {
        let now = at("2026-11-01T10:10:00+00:00");
        let s = status_from_file("", "1.0.0", "1.0.1", now);
        assert_eq!(s["status"], "none");
        let s = status_from_file("3|Pulling|2026-11-01T10:09:00+00:00", "1.0.0", "1.0.1", now);
        assert_eq!(s["status"], "in_progress");
        assert_eq!(s["step"], 3);
        let s = status_from_file(
            "6|Upgrade complete|2026-11-01T10:09:00+00:00",
            "1.0.0",
            "1.0.1",
            now,
        );
        assert_eq!(
            s["status"], "in_progress",
            "the old version is still answering"
        );
        let s = status_from_file(
            "6|Upgrade complete|2026-11-01T10:09:00+00:00",
            "1.0.1",
            "1.0.1",
            now,
        );
        assert_eq!(s["status"], "complete");
        // A custom image tag is not a version: reached once the script is done (v4.6.0).
        let s = status_from_file(
            "6|Upgrade complete|2026-11-01T10:09:00+00:00",
            "1.0.0",
            "pr-1234",
            now,
        );
        assert_eq!(s["status"], "complete");
        let s = status_from_file(
            "error|Failed|2026-11-01T10:09:00+00:00",
            "1.0.0",
            "1.0.1",
            now,
        );
        assert_eq!(s["status"], "error");
        let s = status_from_file("3|Pulling|2026-11-01T09:00:00+00:00", "1.0.0", "1.0.1", now);
        assert_eq!(s["status"], "none", "a stale file means nothing runs");
    }
}
