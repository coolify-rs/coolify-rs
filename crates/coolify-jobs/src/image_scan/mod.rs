//! Image vulnerability scanning (upstream v4.7.0 to v4.7.5): Syft writes the package list (SBOM)
//! of an image on the server that runs it, Grype matches it against its vulnerability database,
//! and the findings are stored per resource (`image_scans`). A map of the engine for callers is
//! in `compat/notes/image-scan.md`.
//!
//! | Module | Ports |
//! |---|---|
//! | [`error`] | `ImageScanErrorCode`, `ScanFailed`, `ScanLockBusy`, `ImageScanningRefused`, `ImageUpdateRefused` |
//! | [`image_ref`] | `ImageRef` |
//! | [`text`] | `ScannerText` |
//! | [`finding`] | `Finding`, `Package` |
//! | [`commands`] | `ScannerCommands`, the scanner image pins, `grype.tmpl`, `syft.tmpl` |
//! | [`grype`] | `GrypeReportParser`, `SyftPackageParser` |
//! | [`sbom_document`] | `SbomDocument` |
//! | [`rules`] | `SecurityRules`, `RelevanceRules`, `IgnoreMatcher` |
//! | [`policy`] | `ImageScanPolicyResolver`, `ResolvedImageScanPolicy`, `ImageScanGateMode`, `ImageScanOnError` |
//! | [`cache`] | the Laravel cache keys and the server lock of the scans |
//! | [`events`] | `ImageScanUpdated` |
//! | [`resource_scans`] | `ResourceScans`, `RefreshImageScanSummariesJob` |
//! | [`scan_image`] | `ScanImage` |
//! | [`prepare`] | `PrepareImageScannerJob`, `SetServerImageScanning` |
//! | [`scan_jobs`] | `ScanResourceImagesJob`, `ImageRescanJob` |
//! | [`host`] | `ScanHostPackages`, `ScanHostPackagesJob` |
//! | [`gate`] | `SecurityGate`, `DeploymentImageScan` |
//! | [`compose_image_variable`] | `ComposeImageVariable` |
//! | [`candidates`] | `RefreshCandidates`, `RefreshCandidatesJob`, `CandidateEvaluator`, `TagSelector`, `BaseImageCheck` |
//! | [`update`] | `UpdateResourceImage`, `DockerfileBaseImage`, `ImageStartPin` |
//! | [`notify`] | `ScanNotifications`, `ImageScanAlertCheckJob`, `ImageScanDigestJob`, `Notifications\ImageScan\*` |
//! | [`schedule`] | the scan entries of `Console\Kernel` and `ServerManagerJob` |

pub mod cache;
pub mod candidates;
pub mod commands;
pub mod compose_image_variable;
pub mod error;
pub mod events;
pub mod finding;
pub mod gate;
pub mod grype;
pub mod host;
pub mod image_ref;
pub mod notify;
pub mod policy;
pub mod prepare;
pub mod resource_scans;
pub mod rules;
pub mod sbom_document;
pub mod scan_image;
pub mod scan_jobs;
pub mod schedule;
pub mod text;
pub mod update;
pub mod views;

use std::time::Duration;

use coolify_db::image_scans::{self, ServerScanSettings};
use coolify_db::servers::ServerRow;

use crate::Jobs;
use crate::server_ops::Srv;
use commands::{InvalidArgument, ScannerCommands, ScannerHost};

/// A server as the scanner uses it: the loaded server and the scan columns of its settings.
#[derive(Debug)]
pub struct ScanServer {
    pub srv: Srv,
    /// `None` when the server has no settings row.
    pub settings: Option<ServerScanSettings>,
}

impl ScanServer {
    pub async fn load(jobs: &Jobs, server_id: i64) -> Result<Self, String> {
        let srv = Srv::load(jobs, server_id).await?;
        Self::from_srv(srv).await
    }

    pub async fn from_row(jobs: &Jobs, row: ServerRow) -> Result<Self, String> {
        Self::from_srv(Srv::from_row(jobs, row)).await
    }

    async fn from_srv(srv: Srv) -> Result<Self, String> {
        let settings = image_scans::server_scan_settings(srv.db(), srv.id())
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self { srv, settings })
    }

    pub fn id(&self) -> i64 {
        self.srv.id()
    }

    pub fn uuid(&self) -> &str {
        &self.srv.row.uuid
    }

    /// `$server->name`.
    pub fn name(&self) -> String {
        self.srv.str_column("name").unwrap_or_default().to_owned()
    }

    pub fn team_id(&self) -> i64 {
        self.srv.row.team_id
    }

    pub fn jobs(&self) -> &Jobs {
        &self.srv.jobs
    }

    pub fn db(&self) -> &coolify_db::Db {
        self.srv.db()
    }

    /// `$server->isTransferredAway()`: another instance manages the server now.
    pub fn is_transferred_away(&self) -> bool {
        self.srv.info.transferred
    }

    /// `$server->settings?->is_image_scan_enabled`.
    pub fn is_scan_enabled(&self) -> bool {
        self.settings
            .as_ref()
            .is_some_and(|s| s.is_image_scan_enabled)
    }

    /// What `ScannerCommands` reads of the server.
    pub fn host(&self) -> ScannerHost {
        ScannerHost::new(
            self.srv.info.is_non_root(),
            self.settings.as_ref(),
            self.srv.row.record.get("server_metadata"),
        )
    }

    /// `new ScannerCommands($server, $runId)`.
    pub fn commands(&self, run_id: &str) -> Result<ScannerCommands, InvalidArgument> {
        ScannerCommands::new(self.host(), run_id)
    }

    /// `instant_remote_process($commands, $server, throwError: false, timeout: $seconds)`: the
    /// trimmed output, `None` when the batch failed or printed nothing.
    pub async fn run(&self, commands: &[String], timeout_seconds: i64) -> Option<String> {
        self.srv
            .run_timeout(
                commands,
                "image-scan",
                Duration::from_secs(timeout_seconds.max(1).unsigned_abs()),
            )
            .await
    }
}

/// `Str::lower(Str::random(24))`: the ID of one scan run (container name, work directory).
pub fn new_run_id() -> String {
    coolify_domain::new_public_id()
}

/// `ScanResourceImagesJob::LOCK_RETRY_DELAYS`: the waits of a scan job after a busy server lock.
pub const LOCK_RETRY_DELAYS: [u64; 3] = [60, 120, 300];

/// `ScanResourceImagesJob::MAX_ATTEMPTS`: attempts of a scan job, for a busy lock and for an
/// image that is still being pulled.
pub const MAX_ATTEMPTS: u32 = 10;

/// `ScanResourceImagesJob::lockRetryDelay($attempt)`: the seconds to wait before attempt
/// `attempt + 1` after a busy lock: 60, 120, then 300.
pub fn lock_retry_delay(attempt: u32) -> u64 {
    let index = (attempt.max(1) as usize).min(LOCK_RETRY_DELAYS.len()) - 1;
    LOCK_RETRY_DELAYS[index]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_retry_delays() {
        assert_eq!(lock_retry_delay(0), 60);
        assert_eq!(lock_retry_delay(1), 60);
        assert_eq!(lock_retry_delay(2), 120);
        assert_eq!(lock_retry_delay(3), 300);
        assert_eq!(lock_retry_delay(9), 300);
        assert_eq!(new_run_id().len(), 24);
    }
}
