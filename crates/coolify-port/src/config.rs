//! `port.toml`: migration-engine configuration (checked into the repo).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamConfig {
    /// Canonical upstream URL (informational; the mirror's remote is authoritative).
    pub url: String,
    pub branch: String,
    /// Bare mirror used for all analysis, relative to the repo root.
    pub mirror: PathBuf,
    /// Remote name inside the mirror.
    #[serde(default = "default_remote")]
    pub remote: String,
}

fn default_remote() -> String {
    "origin".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    pub runs_dir: PathBuf,
    pub ledger_dir: PathBuf,
    pub mapping: PathBuf,
    pub state_file: PathBuf,
    /// Upper bound on the size of the diff embedded in prompt packs.
    #[serde(default = "default_prompt_bytes")]
    pub prompt_pack_max_bytes: usize,
}

fn default_prompt_bytes() -> usize {
    256 * 1024
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationConfig {
    pub timeout_secs: u64,
    /// Shared target dir so worktree validations reuse build artifacts.
    pub cargo_target_dir: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepairConfig {
    pub max_attempts: u32,
    /// External agent command (argv). Empty = disabled. The agent runs in the
    /// run's worktree with `PORT_RUN_DIR` and `PORT_PROMPT_PACK` set and is
    /// never given credentials by the engine.
    #[serde(default)]
    pub agent_command: Vec<String>,
    #[serde(default = "default_agent_timeout")]
    pub agent_timeout_secs: u64,
}

fn default_agent_timeout() -> u64 {
    1800
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortConfig {
    pub upstream: UpstreamConfig,
    pub engine: EngineConfig,
    pub validation: ValidationConfig,
    #[serde(default)]
    pub repair: RepairConfig,
    /// Repo root (directory containing port.toml); not serialized.
    #[serde(skip)]
    pub root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading {0}: {1}")]
    Io(String, std::io::Error),
    #[error("parsing port.toml: {0}")]
    Parse(#[from] toml::de::Error),
}

impl PortConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let src = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Io(path.display().to_string(), e))?;
        let mut cfg: PortConfig = toml::from_str(&src)?;
        cfg.root = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Ok(cfg)
    }

    pub fn path(&self, p: &Path) -> PathBuf {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        }
    }
}
