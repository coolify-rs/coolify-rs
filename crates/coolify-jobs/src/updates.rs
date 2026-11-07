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

/// `CoolifyUpgradeStatus::FILE`.
pub const STATUS_FILE: &str = "/data/coolify/source/.upgrade-status";
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

fn latest_in(versions: &Value) -> Option<String> {
    versions
        .pointer("/coolify/v4/version")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// `CheckForUpdatesJob::handle`. `true` when `new_version_available` is up to date.
pub async fn check_for_updates(jobs: &Jobs, current: &str) -> bool {
    let Some(latest) = refresh_versions().await.as_ref().and_then(latest_in) else {
        return false;
    };
    let newer = version_compare(&latest, current) == Ordering::Greater;
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
    let reached = !running.is_empty()
        && !target.is_empty()
        && version_compare(running, target) != Ordering::Less;
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

/// `UpdateCoolify::backupDatabase`: the instance database's backup schedule, when there is one.
/// `upgrade.sh` always takes its own dump as well.
async fn backup_database(jobs: &Jobs) -> Result<(), String> {
    let db = jobs.0.db.pool();
    let enabled: bool = sqlx::query_scalar(
        "SELECT COALESCE(is_backup_before_update_enabled, false) FROM instance_settings WHERE id = 0",
    )
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())?;
    let backup: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM scheduled_database_backups \
         WHERE database_id = 0 AND database_type = 'App\\Models\\StandalonePostgresql' ORDER BY id LIMIT 1",
    )
    .fetch_optional(db)
    .await
    .map_err(|e| e.to_string())?;
    let Some(backup) = backup.filter(|_| enabled) else {
        return Ok(());
    };
    jobs.run_database_backup(backup).await;
    let status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM scheduled_database_backup_executions \
         WHERE scheduled_database_backup_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(backup)
    .fetch_optional(db)
    .await
    .map_err(|e| e.to_string())?;
    if status.as_deref() == Some("success") {
        Ok(())
    } else {
        Err("The database backup failed, so the update was not started.".to_owned())
    }
}

/// `UpdateCoolify::handle`. `Ok(false)` when there is nothing to do.
pub async fn update_coolify(jobs: &Jobs, current: &str, manual: bool) -> Result<bool, String> {
    let Some(script_url) = upgrade_script_url() else {
        return Err("This installation has no update address (COOLIFY_RS_CDN).".to_owned());
    };
    let latest = refresh_versions()
        .await
        .as_ref()
        .and_then(latest_in)
        .or_else(|| {
            std::fs::read_to_string(versions_path())
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .as_ref()
                .and_then(latest_in)
        })
        .ok_or_else(|| "Cannot determine latest version.".to_owned())?;
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
        ));
    }
    backup_database(jobs).await?;

    let srv = Srv::load(jobs, 0).await?;
    let status = srv
        .run_quiet(
            &[format!("cat {STATUS_FILE} 2>/dev/null || true")],
            "upgrade-status",
        )
        .await
        .unwrap_or_default();
    if is_running(&status, Utc::now()) {
        return Err(format!(
            "Another coolify-rs upgrade is already running. Wait for it to finish. If it has stopped, you can upgrade again {RUNNING_LOCK_EXPIRES_AFTER_MINUTES} minutes after its last status update."
        ));
    }
    let (helper, registry): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT helper_version, docker_registry_url FROM instance_settings WHERE id = 0",
    )
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())?;
    let helper = helper_version(helper.as_deref());
    let registry = registry
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "docker.io".to_owned());
    let commands = vec![
        format!(
            "curl -fsSL {} -o /data/coolify/source/upgrade.sh",
            shell_arg(&script_url)
        ),
        format!(
            "bash /data/coolify/source/upgrade.sh {} {} {}",
            shell_arg(&latest),
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
                if let Err(e) = update_coolify(&jobs, &current, false).await {
                    tracing::error!(error = %e, "UpdateCoolifyJob failed");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

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
