//! `ServerManagerJob`: the per-minute dispatch of the server checks.
//!
//! Off by default: `COOLIFY_RS_SERVER_SCHEDULER=1` starts it. An instance that shares
//! its database with upstream Coolify must not run it, because upstream's scheduler
//! already checks every server.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use chrono::{DateTime, Datelike, Timelike, Utc};
use coolify_db::server_ops as ops;
use coolify_db::servers::{self, ServerRow};
use serde_json::Value;

use super::{MARKS, Srv, checks, patches, sentinel};
use crate::Jobs;

/// `VALID_CRON_STRINGS`.
fn expand(frequency: &str) -> &str {
    match frequency {
        "every_minute" => "* * * * *",
        "hourly" => "0 * * * *",
        "daily" => "0 0 * * *",
        "weekly" => "0 0 * * 0",
        "monthly" => "0 0 1 * *",
        "yearly" => "0 0 1 1 *",
        "@hourly" => "0 * * * *",
        "@daily" => "0 0 * * *",
        "@weekly" => "0 0 * * 0",
        "@monthly" => "0 0 1 * *",
        "@yearly" | "@annually" => "0 0 1 1 *",
        other => other,
    }
}

fn field_matches(field: &str, value: u32, min: u32) -> bool {
    field.split(',').any(|part| {
        let (range, step) = part.split_once('/').map_or((part, 1), |(r, s)| {
            (r, s.parse::<u32>().unwrap_or(1).max(1))
        });
        let (lo, hi) = if range == "*" {
            (min, u32::MAX)
        } else if let Some((a, b)) = range.split_once('-') {
            (a.parse().unwrap_or(u32::MAX), b.parse().unwrap_or(0))
        } else {
            let v = range.parse().unwrap_or(u32::MAX);
            if step > 1 { (v, u32::MAX) } else { (v, v) }
        };
        value >= lo && value <= hi && (value - lo).is_multiple_of(step)
    })
}

/// `CronExpression::isDue` for standard five-field expressions.
pub fn cron_is_due(frequency: &str, at: DateTime<Utc>) -> bool {
    let expr = expand(frequency);
    let f: Vec<&str> = expr.split_whitespace().collect();
    if f.len() != 5 {
        return false;
    }
    let dow = at.weekday().num_days_from_sunday();
    let dow_ok = field_matches(f[4], dow, 0) || (dow == 0 && field_matches(f[4], 7, 0));
    let dom_ok = field_matches(f[2], at.day(), 1);
    // Standard cron: day of month and day of week are ORed when both are restricted.
    let day_ok = if f[2] != "*" && f[4] != "*" {
        dom_ok || dow_ok
    } else {
        dom_ok && dow_ok
    };
    field_matches(f[0], at.minute(), 0)
        && field_matches(f[1], at.hour(), 0)
        && field_matches(f[3], at.month(), 1)
        && day_ok
}

static LAST_DUE: LazyLock<Mutex<HashMap<String, DateTime<Utc>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `shouldRunCronNow($frequency, $timezone, $dedupKey, $executionTime)`.
pub fn should_run_cron_now(frequency: &str, timezone: &str, key: &str, at: DateTime<Utc>) -> bool {
    // Offsets of `server_timezone` are not applied: schedules run in UTC (docs/KNOWN_LIMITATIONS.md).
    let _ = timezone;
    if cron_is_due(frequency, at) {
        let mut seen = LAST_DUE.lock().unwrap_or_else(|p| p.into_inner());
        let minute = at
            .with_second(0)
            .and_then(|t| t.with_nanosecond(0))
            .unwrap_or(at);
        if seen.get(key) == Some(&minute) {
            return false;
        }
        seen.insert(key.to_string(), minute);
        return true;
    }
    false
}

/// `getBackoffCycleInterval`.
fn backoff_interval(count: i64) -> u32 {
    match count {
        i64::MIN..=2 => 1,
        3..=5 => 3,
        6..=11 => 6,
        _ => 12,
    }
}

/// `shouldSkipDueToBackoff`.
fn skip_due_to_backoff(row: &ServerRow, at: DateTime<Utc>) -> bool {
    let count = row
        .record
        .get("unreachable_count")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let interval = backoff_interval(count);
    if interval <= 1 {
        return false;
    }
    let hash = crc32(row.id.to_string().as_bytes());
    !at.minute()
        .wrapping_add(hash % interval)
        .is_multiple_of(interval)
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for b in data {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// `ServerManagerJob::patchCheckCron`.
pub fn patch_check_cron(row: &ServerRow) -> String {
    if let Some(f) = row
        .settings
        .as_ref()
        .and_then(|s| s.get("patch_check_frequency"))
        .and_then(Value::as_str)
        .filter(|f| !f.trim().is_empty())
    {
        return expand(f).to_string();
    }
    let slot = row.id % (20 * 60);
    format!("{} {} * * 0", slot % 60, 4 + slot / 60)
}

/// `ServerManagerJob::handle`, one tick.
pub async fn server_manager_tick(jobs: &Jobs) -> Result<(), String> {
    let at = Utc::now();
    let rows = servers::all(&jobs.0.db).await.map_err(|e| e.to_string())?;
    let settings = ops::instance_server_settings(&jobs.0.db)
        .await
        .map_err(|e| e.to_string())?;
    let instance_tz = settings
        .instance_timezone
        .unwrap_or_else(|| "UTC".to_string());
    for row in rows {
        let srv = Srv::from_row(jobs, row.clone());
        if srv.info.has_placeholder_ip() {
            continue;
        }
        let id = row.id;
        let timezone = row
            .settings
            .as_ref()
            .and_then(|s| s.get("server_timezone"))
            .and_then(Value::as_str)
            .unwrap_or(&instance_tz)
            .to_string();
        let reachable = srv.info.setting_bool("is_reachable") == Some(true);
        let usable = srv.info.setting_bool("is_usable") == Some(true);
        let live = sentinel::sentinel_live(&srv);
        let skip_conn = sentinel::sentinel_enabled(&srv) && live && reachable && usable;
        if !skip_conn
            && !skip_due_to_backoff(&row, at)
            && should_run_cron_now(
                "* * * * *",
                "UTC",
                &format!("server-connection-check:{id}"),
                at,
            )
        {
            jobs.spawn_server_job("server-connection-check", id, |j, i| async move {
                checks::server_connection_check(&j, i).await
            });
        }
        let out_of_sync = !live;
        if out_of_sync
            && !srv.info.is_build_server()
            && !srv.is_swarm_worker()
            && should_run_cron_now("* * * * *", &timezone, &format!("server-check:{id}"), at)
            && !skip_due_to_backoff(&row, at)
        {
            jobs.spawn_server_job("server-check", id, |j, i| async move {
                checks::server_check(&j, i).await
            });
        }
        if sentinel::sentinel_enabled(&srv)
            && !MARKS.has(&format!("sentinel:reported-version:{id}"))
            && should_run_cron_now(
                &format!("{} * * * *", id % 60),
                &timezone,
                &format!("sentinel-version-check-v2:{id}"),
                at,
            )
            && srv.info.is_functional()
        {
            jobs.spawn_check_sentinel(id);
        }
        if out_of_sync {
            let frequency = row
                .settings
                .as_ref()
                .and_then(|s| s.get("server_disk_usage_check_frequency"))
                .and_then(Value::as_str)
                .unwrap_or("0 23 * * *")
                .to_string();
            if should_run_cron_now(
                &frequency,
                &timezone,
                &format!("server-storage-check:{id}"),
                at,
            ) {
                jobs.spawn_server_job("server-storage-check", id, |j, i| async move {
                    checks::server_storage_check(&j, i).await.map(|_| ())
                });
            }
        }
        if should_run_cron_now(
            &patch_check_cron(&row),
            &timezone,
            &format!("server-patch-check-v2:{id}"),
            at,
        ) {
            jobs.spawn_server_job("server-patch-check", id, |j, i| async move {
                patches::server_patch_check(&j, i, false).await
            });
        }
        // Docker cleanup follows `docker_cleanup_frequency` (ScheduledJobManager upstream).
        if let Some(frequency) = row
            .settings
            .as_ref()
            .and_then(|s| s.get("docker_cleanup_frequency"))
            .and_then(Value::as_str)
            && should_run_cron_now(frequency, &timezone, &format!("docker-cleanup:{id}"), at)
        {
            let volumes = srv.info.setting_bool("delete_unused_volumes") == Some(true);
            let networks = srv.info.setting_bool("delete_unused_networks") == Some(true);
            jobs.spawn_docker_cleanup(id, false, volumes, networks);
        }
    }
    Ok(())
}

impl Jobs {
    /// Starts the per-minute `ServerManagerJob` loop.
    pub fn start_server_scheduler(&self) {
        let jobs = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                tick.tick().await;
                if let Err(e) = server_manager_tick(&jobs).await {
                    tracing::warn!(error = %e, "server manager tick failed");
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn cron() {
        let t = Utc.with_ymd_and_hms(2026, 10, 25, 4, 5, 0).unwrap(); // a Sunday
        assert!(cron_is_due("5 4 * * 0", t));
        assert!(cron_is_due("*/5 * * * *", t));
        assert!(!cron_is_due("6 4 * * 0", t));
        assert!(cron_is_due(
            "daily",
            Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap()
        ));
    }

    #[test]
    fn crc() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
