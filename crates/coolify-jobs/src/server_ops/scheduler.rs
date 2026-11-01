//! `ServerManagerJob`: one run of the per-minute dispatch of the server checks, started by
//! [`crate::kernel`].

use chrono::{DateTime, Timelike, Utc};
use coolify_compat::timezones::valid_timezone;
use coolify_db::server_ops as ops;
use coolify_db::servers::{self, ServerRow};
use serde_json::Value;

use super::{MARKS, Srv, checks, patches, sentinel};
use crate::Jobs;
use crate::scheduler::{allowed, should_run_cron_now};

/// `$checkFrequency` off Coolify Cloud.
const CHECK_FREQUENCY: &str = "* * * * *";
/// `CLOUD_PROVIDER_STATUS_CHECK_CRON`.
const CLOUD_PROVIDER_STATUS_CHECK_CRON: &str = "*/5 * * * *";

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

/// PHP `crc32()`.
pub(crate) fn crc32(data: &[u8]) -> u32 {
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

/// `ServerManagerJob::sentinelVersionCheckCron`.
fn sentinel_version_check_cron(id: i64) -> String {
    format!("{} * * * *", id % 60)
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
        return f.to_string();
    }
    let slot = row.id % (20 * 60);
    format!("{} {} * * 0", slot % 60, 4 + slot / 60)
}

/// `$server->hetzner_server_id || ...` of `dispatchCloudProviderStatusChecks`.
fn has_cloud_resource(row: &ServerRow) -> bool {
    [
        "hetzner_server_id",
        "vultr_instance_id",
        "digitalocean_droplet_id",
        "hostinger_virtual_machine_id",
    ]
    .iter()
    .any(|k| {
        row.record
            .get(*k)
            .is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
    })
}

/// `ServerManagerJob::handle`, one run.
pub async fn server_manager_tick(jobs: &Jobs, at: DateTime<Utc>) -> Result<(), String> {
    let rows = servers::all(&jobs.0.db).await.map_err(|e| e.to_string())?;
    let settings = ops::instance_server_settings(&jobs.0.db)
        .await
        .map_err(|e| e.to_string())?;
    let instance_tz = settings
        .instance_timezone
        .filter(|tz| valid_timezone(tz))
        .unwrap_or_else(|| "UTC".to_string());

    // `dispatchCloudProviderStatusChecks`. The key is one for all servers, so it is read once.
    if should_run_cron_now(
        CLOUD_PROVIDER_STATUS_CHECK_CRON,
        &instance_tz,
        "server-cloud-provider-status-checks",
        at,
    ) {
        for row in rows.iter().filter(|r| allowed(&format!("server:{}", r.id))) {
            let srv = Srv::from_row(jobs, row.clone());
            let has_token = row
                .record
                .get("cloud_provider_token_id")
                .is_some_and(|v| !v.is_null());
            if has_cloud_resource(row) && has_token && !srv.info.transferred {
                jobs.spawn_cloud_provider_status_check(row.id);
            }
        }
    }

    // `dispatchConnectionChecks`: one key for all servers too.
    let connection_checks_due = should_run_cron_now(
        CHECK_FREQUENCY,
        &instance_tz,
        "server-connection-checks",
        at,
    );

    for row in rows {
        let id = row.id;
        if !allowed(&format!("server:{id}")) {
            continue;
        }
        let srv = Srv::from_row(jobs, row.clone());
        if srv.info.has_placeholder_ip() {
            continue;
        }
        let reachable = srv.info.setting_bool("is_reachable") == Some(true);
        let usable = srv.info.setting_bool("is_usable") == Some(true);
        let live = sentinel::sentinel_live(&srv);
        let enabled = sentinel::sentinel_enabled(&srv);
        // A healthy Sentinel's heartbeat proves the connection, but it does not restore the flags.
        if connection_checks_due
            && !(enabled && live && reachable && usable)
            && !skip_due_to_backoff(&row, at)
        {
            jobs.spawn_server_job("server-connection-check", id, |j, i| async move {
                checks::server_connection_check(&j, i).await
            });
        }

        // `processServerTasks`
        let timezone = row
            .settings
            .as_ref()
            .and_then(|s| s.get("server_timezone"))
            .and_then(Value::as_str)
            .map_or_else(|| instance_tz.clone(), str::to_string);
        let timezone = if valid_timezone(&timezone) {
            timezone
        } else {
            "UTC".to_string()
        };
        // `$sentinelOutOfSync`: a never-updated `sentinel_updated_at` is out of sync as well.
        let out_of_sync = !live;
        if out_of_sync
            && !srv.info.is_build_server()
            && !srv.is_swarm_worker()
            && should_run_cron_now(
                CHECK_FREQUENCY,
                &timezone,
                &format!("server-check:{id}"),
                at,
            )
            && !skip_due_to_backoff(&row, at)
        {
            jobs.spawn_server_job("server-check", id, |j, i| async move {
                checks::server_check(&j, i).await
            });
        }
        if enabled
            && !MARKS.has(&format!("sentinel:reported-version:{id}"))
            && should_run_cron_now(
                &sentinel_version_check_cron(id),
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
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn backoff_intervals() {
        assert_eq!(backoff_interval(2), 1);
        assert_eq!(backoff_interval(5), 3);
        assert_eq!(backoff_interval(11), 6);
        assert_eq!(backoff_interval(12), 12);
    }
}
