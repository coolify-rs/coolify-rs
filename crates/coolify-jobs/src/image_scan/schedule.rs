//! The image scan entries of `ServerManagerJob` (upstream's `Console\Kernel` schedules none of
//! its own): the daily rescan of each server that scans images, and the daily digest of each
//! team with such a server. `server_ops::scheduler` calls [`spawn_server_rescan`] for each server
//! of a run and [`spawn_digests`] once per run.
//!
//! The rescan of a server dispatches its host package scan itself (`ImageRescanJob`), so the
//! host scan has no schedule entry here.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};

use super::{ScanServer, scan_jobs};
use crate::Jobs;
use crate::scheduler::should_run_cron_now;

/// `ServerManagerJob::imageRescanCron($server)`: daily image rescan at a stable per-server time
/// between 02:00 and 04:59 (spec 4.5).
pub fn image_rescan_cron(server_id: i64) -> String {
    format!(
        "{} {} * * *",
        server_id.rem_euclid(60),
        2 + server_id.rem_euclid(180) / 60
    )
}

/// `ServerManagerJob::imageScanDigestCron($teamId)`: daily image vulnerability digest at a stable
/// per-team time between 06:00 and 07:59, after the rescans of 02:00 to 04:59 (spec 8).
pub fn image_scan_digest_cron(team_id: i64) -> String {
    let slot = team_id.rem_euclid(120);
    format!("{} {} * * *", slot % 60, 6 + slot / 60)
}

/// `ServerManagerJob::processServerTasks()`, the rescan part: daily image rescan, staggered per
/// server. The cron is read only for servers that can be scanned, so a server that comes back
/// catches up a missed day. `timezone` is the server's time zone as the run resolved it.
pub async fn server_rescan_tick(jobs: &Jobs, server_id: i64, timezone: &str, at: DateTime<Utc>) {
    let Ok(server) = ScanServer::load(jobs, server_id).await else {
        return;
    };
    if server.is_scan_enabled()
        && scan_jobs::can_run_on(&server)
        && should_run_cron_now(
            &image_rescan_cron(server_id),
            timezone,
            &format!("image-rescan:{server_id}"),
            at,
        )
    {
        jobs.dispatch_image_rescan(server_id);
    }
}

/// [`server_rescan_tick`] on its own task, so the per-minute run never waits for the database.
pub fn spawn_server_rescan(jobs: &Jobs, server_id: i64, timezone: &str, at: DateTime<Utc>) {
    let jobs = jobs.clone();
    let timezone = timezone.to_owned();
    tokio::spawn(async move {
        server_rescan_tick(&jobs, server_id, &timezone, at).await;
    });
}

/// `ServerManagerJob::dispatchImageScanDigests($servers)`: one image vulnerability digest per
/// team and day, for teams with a server that scans images (spec 8). Servers with a placeholder
/// IP and servers another instance manages do not count. `instance_tz` is the instance time
/// zone.
pub async fn digests_tick(jobs: &Jobs, instance_tz: &str, at: DateTime<Utc>) {
    let server_ids = match coolify_db::image_scans::scanning_server_ids(&jobs.0.db).await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::error!(error = %e, "Failed to dispatch ImageScanDigestJob");
            return;
        }
    };
    let mut teams = BTreeSet::new();
    for server_id in server_ids {
        let Ok(server) = ScanServer::load(jobs, server_id).await else {
            continue;
        };
        if !server.srv.info.has_placeholder_ip() && !server.is_transferred_away() {
            teams.insert(server.team_id());
        }
    }
    for team_id in teams {
        if should_run_cron_now(
            &image_scan_digest_cron(team_id),
            instance_tz,
            &format!("image-scan-digest:{team_id}"),
            at,
        ) {
            match coolify_db::image_scans::notifications::team_exists(&jobs.0.db, team_id).await {
                Ok(true) => jobs.dispatch_image_scan_digest(team_id),
                Ok(false) => {}
                Err(e) => {
                    tracing::error!(team_id, error = %e, "Failed to dispatch ImageScanDigestJob");
                }
            }
        }
    }
}

/// [`digests_tick`] on its own task.
pub fn spawn_digests(jobs: &Jobs, instance_tz: &str, at: DateTime<Utc>) {
    let jobs = jobs.clone();
    let instance_tz = instance_tz.to_owned();
    tokio::spawn(async move {
        digests_tick(&jobs, &instance_tz, at).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rescans_run_between_two_and_five() {
        assert_eq!(image_rescan_cron(0), "0 2 * * *");
        assert_eq!(image_rescan_cron(61), "1 3 * * *");
        assert_eq!(image_rescan_cron(179), "59 4 * * *");
        assert_eq!(image_rescan_cron(180), "0 2 * * *");
        assert_eq!(image_rescan_cron(245), "5 3 * * *");
    }

    #[test]
    fn digests_run_between_six_and_eight() {
        assert_eq!(image_scan_digest_cron(0), "0 6 * * *");
        assert_eq!(image_scan_digest_cron(59), "59 6 * * *");
        assert_eq!(image_scan_digest_cron(60), "0 7 * * *");
        assert_eq!(image_scan_digest_cron(119), "59 7 * * *");
        assert_eq!(image_scan_digest_cron(121), "1 6 * * *");
    }
}
