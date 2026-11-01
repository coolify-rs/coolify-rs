//! Background work on managed servers.
//!
//! Upstream runs these as queued jobs (Redis/Horizon). coolify-rs runs them on
//! tokio tasks in the API process (ADR 0009): a job is lost if the process
//! restarts, and one validation per server runs at a time.

pub mod applications;
pub mod checks;
pub mod cloud;
pub mod compose_parser;
pub mod databases;
pub mod file_browser;
pub mod git_http;
pub mod gitea;
pub mod github;
pub mod gitlab;
pub mod kernel;
pub mod kernel_resources;
pub mod notifications;
pub mod schedule_sources;
pub mod scheduled_task;
pub mod scheduler;
pub mod server;
pub mod server_ops;
pub mod server_transfer;
pub mod services;
pub mod storages;
mod validate;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use chrono::{NaiveDateTime, SubsecRound, Utc};
use coolify_compat::Encrypter;
use coolify_db::Db;
use coolify_remote::Remote;
use serde::Serialize;

pub use checks::Wording;

/// Checkpoints of a running or finished validation, as the Livewire
/// `ValidateAndInstall` component exposes them. `None` means not reached.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Progress {
    pub running: bool,
    pub uptime: Option<bool>,
    pub supported_os_type: Option<bool>,
    pub prerequisites_installed: Option<bool>,
    pub docker_installed: Option<bool>,
    pub docker_compose_installed: Option<bool>,
    pub docker_version: Option<bool>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Jobs(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    db: Db,
    remote: Remote,
    encrypter: Option<Encrypter>,
    running: Mutex<HashSet<i64>>,
    progress: Mutex<HashMap<i64, Progress>>,
    /// Databases with a start or restart in progress (`DatabaseOperationReservation`).
    db_ops: Mutex<HashSet<String>>,
    /// `config('constants.coolify.version')`, for the `coolify.version` label.
    version: std::sync::OnceLock<String>,
    /// `WithoutOverlapping` locks of scheduled jobs: expiry and owner by key.
    overlap_locks: Mutex<HashMap<String, (std::time::Instant, String)>>,
}

impl Jobs {
    pub fn new(db: Db, remote: Remote, encrypter: Option<Encrypter>) -> Self {
        Self(Arc::new(Inner {
            db,
            remote,
            encrypter,
            running: Mutex::new(HashSet::new()),
            progress: Mutex::new(HashMap::new()),
            db_ops: Mutex::new(HashSet::new()),
            version: std::sync::OnceLock::new(),
            overlap_locks: Mutex::new(HashMap::new()),
        }))
    }

    /// Sets the instance version (`COOLIFY_VERSION`) once at startup.
    pub fn set_version(&self, version: String) {
        notifications::set_version(&version);
        let _ = self.0.version.set(version);
    }

    /// `config('constants.coolify.version')`.
    pub fn version(&self) -> String {
        self.0
            .version
            .get()
            .cloned()
            .unwrap_or_else(|| "4.5.1".into())
    }

    pub fn encrypter(&self) -> Option<&Encrypter> {
        self.0.encrypter.as_ref()
    }

    pub fn remote(&self) -> &Remote {
        &self.0.remote
    }

    pub(crate) fn now() -> NaiveDateTime {
        Utc::now().naive_utc().trunc_subsecs(0)
    }

    /// Starts `ValidateServer` for a server unless one is already running.
    /// Returns false when a validation was already in progress.
    pub fn spawn_validation(
        &self,
        server_id: i64,
        team_id: i64,
        uuid: String,
        wording: Wording,
    ) -> bool {
        {
            let mut running = self.0.running.lock().unwrap_or_else(|p| p.into_inner());
            if !running.insert(server_id) {
                return false;
            }
        }
        self.set_progress(server_id, |p| {
            *p = Progress {
                running: true,
                ..Progress::default()
            }
        });
        let jobs = self.clone();
        tokio::spawn(async move {
            let result = validate::run(&jobs, server_id, team_id, &uuid, wording).await;
            if let Err(e) = &result {
                // The job failed: the message is in validation_logs, or for a
                // command that threw (as upstream's failed job) only here.
                tracing::info!(server_uuid = %uuid, error = %e, "server validation failed");
                let shown = checks::html_escape(e);
                jobs.set_progress(server_id, |p| {
                    p.error.get_or_insert(shown);
                });
            }
            jobs.set_progress(server_id, |p| p.running = false);
            jobs.0
                .running
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&server_id);
        });
        true
    }

    /// Latest checkpoints of a server's validation since this process started.
    pub fn progress(&self, server_id: i64) -> Option<Progress> {
        self.0
            .progress
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&server_id)
            .cloned()
    }

    fn set_progress(&self, server_id: i64, f: impl FnOnce(&mut Progress)) {
        let mut map = self.0.progress.lock().unwrap_or_else(|p| p.into_inner());
        f(map.entry(server_id).or_default());
    }

    /// Runs a validation to completion on the current task (tests).
    pub async fn validate_now(
        &self,
        server_id: i64,
        team_id: i64,
        uuid: &str,
        wording: Wording,
    ) -> Result<(), String> {
        self.set_progress(server_id, |p| {
            *p = Progress {
                running: true,
                ..Progress::default()
            }
        });
        let r = validate::run(self, server_id, team_id, uuid, wording).await;
        self.set_progress(server_id, |p| p.running = false);
        r
    }
}
