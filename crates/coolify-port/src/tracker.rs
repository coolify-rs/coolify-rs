//! Upstream tracking state (`migration/state.json`).
//!
//! Two markers are kept separately on purpose:
//! * `last_analyzed`: the newest upstream commit a run has analysed;
//! * `last_integrated`: the newest upstream commit whose port a human merged.
//!
//! Only `mark-integrated` (a human action) moves the second marker.

use serde::{Deserialize, Serialize};

use crate::config::PortConfig;
use crate::git::{Git, GitError};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrackerState {
    pub upstream_branch: String,
    pub baseline: String,
    pub last_analyzed: Option<String>,
    pub last_integrated: Option<String>,
    #[serde(default)]
    pub history: Vec<TrackerEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackerEvent {
    pub at: String,
    pub kind: String,
    pub commit: String,
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub upstream_head: String,
    pub baseline: String,
    pub last_analyzed: Option<String>,
    pub last_integrated: Option<String>,
    pub pending_since_analyzed: usize,
    pub pending_since_integrated: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum TrackerError {
    #[error(transparent)]
    Git(#[from] GitError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

impl TrackerState {
    pub fn load(cfg: &PortConfig) -> Result<Self, TrackerError> {
        let path = cfg.path(&cfg.engine.state_file);
        match std::fs::read(&path) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, cfg: &PortConfig) -> Result<(), TrackerError> {
        let path = cfg.path(&cfg.engine.state_file);
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        // Write-then-rename so an interrupted save never corrupts state.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn record(&mut self, kind: &str, commit: &str, run_id: Option<&str>) {
        self.history.push(TrackerEvent {
            at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            kind: kind.into(),
            commit: commit.into(),
            run_id: run_id.map(str::to_string),
        });
        match kind {
            "analyzed" => self.last_analyzed = Some(commit.into()),
            "integrated" => self.last_integrated = Some(commit.into()),
            _ => {}
        }
    }

    pub fn status(&self, git: &Git, branch: &str) -> Result<Status, TrackerError> {
        let head = git.rev_parse(branch)?;
        let count = |from: &Option<String>| -> Result<usize, TrackerError> {
            let from = from.clone().unwrap_or_else(|| self.baseline.clone());
            if from.is_empty() {
                return Ok(0);
            }
            Ok(git.commits(&from, &head)?.len())
        };
        Ok(Status {
            upstream_head: head.clone(),
            baseline: self.baseline.clone(),
            last_analyzed: self.last_analyzed.clone(),
            last_integrated: self.last_integrated.clone(),
            pending_since_analyzed: count(&self.last_analyzed)?,
            pending_since_integrated: count(&self.last_integrated)?,
        })
    }
}
