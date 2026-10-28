//! The Docker Compose parsers of upstream (bootstrap/helpers/parsers.php) and the application
//! methods around them (`Application::loadComposeFile()`, `parse()`, `oldRawParser()`).
//!
//! The helpers are split by concern so that a later `serviceParser()` port can reuse them:
//! [`validate`] (shell-safe paths, `validateDockerComposeForInjection()`, volume strings, network
//! names), [`volumes`], [`env`] (environment and magic `SERVICE_*` variables), [`domains`],
//! [`labels`] (default and proxy labels), [`service`] (per-service rules) and [`php`] (the PHP
//! and Laravel semantics everything relies on). [`application`] is `applicationParser()` itself.

pub mod application;
pub mod domains;
pub mod env;
pub mod labels;
pub mod load;
pub mod php;
pub mod raw;
pub mod service;
pub mod validate;
pub mod volumes;

use chrono::NaiveDateTime;
use coolify_compat::yaml::{Key, PhpValue};

use crate::Jobs;

pub use load::LoadError;

/// The result of `applicationParser()`: the compose structure upstream returns (top-level keys
/// `services`, `volumes`, `networks`, `configs`, `secrets`, empty sections dropped) and the volume
/// warnings the deployment logs as `Warning: {w}`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCompose {
    pub top_level: PhpValue,
    pub warnings: Vec<String>,
}

impl ParsedCompose {
    /// `collect([])`.
    pub fn empty() -> Self {
        Self {
            top_level: PhpValue::Array(Vec::new()),
            warnings: Vec::new(),
        }
    }

    /// `$compose->isEmpty()`.
    pub fn is_empty(&self) -> bool {
        matches!(&self.top_level, PhpValue::Array(items) if items.is_empty())
    }

    /// The `services` map (name → service).
    pub fn services(&self) -> &[(Key, PhpValue)] {
        match php::get(&self.top_level, "services") {
            Some(PhpValue::Array(items)) => items,
            _ => &[],
        }
    }

    /// The `services` map, to change services before dumping again (for example `env_file`).
    pub fn services_mut(&mut self) -> Option<&mut Vec<(Key, PhpValue)>> {
        match self.top_level.get_mut("services") {
            Some(PhpValue::Array(items)) => Some(items),
            _ => None,
        }
    }

    /// The services that have a `build` key.
    pub fn build_services(&self) -> Vec<String> {
        self.services()
            .iter()
            .filter(|(_, s)| php::get(s, "build").is_some_and(|b| *b != PhpValue::Null))
            .map(|(k, _)| k.as_string())
            .collect()
    }

    /// `Yaml::dump($compose, 10, $indent)` (`docker_compose` uses 2, the deployment 4).
    pub fn to_yaml(&self, indent: usize) -> String {
        coolify_compat::yaml::dump_with_indent(&self.top_level, 10, indent)
    }
}

/// `Application::parse($pull_request_id, null, $commit)` → `applicationParser()` for an
/// application with `compose_parsing_version >= 3`, with all its database writes. Only
/// `pull_request_id = 0` is ported.
pub async fn application_parser(
    jobs: &Jobs,
    app_id: i64,
    pull_request_id: i64,
    commit: Option<&str>,
) -> Result<ParsedCompose, String> {
    application_parser_at(jobs, app_id, pull_request_id, commit, Jobs::now()).await
}

/// [`application_parser`] at a given time (`now()`: container names end with it).
pub async fn application_parser_at(
    jobs: &Jobs,
    app_id: i64,
    pull_request_id: i64,
    commit: Option<&str>,
    now: NaiveDateTime,
) -> Result<ParsedCompose, String> {
    let mut run = application::Run::load(jobs, app_id, now).await?;
    if run.parsing_version() < 3 {
        return Err(
            "Compose parsing versions before 3 (parseDockerComposeFile) are not ported.".into(),
        );
    }
    application::application_parser(&mut run, pull_request_id, commit).await
}

/// `Application::loadComposeFile($isInit)`: reads the compose file from the branch tip on the
/// destination server, validates it, saves `docker_compose_raw`, parses it and reconciles
/// `docker_compose_domains`.
pub async fn load_compose_file(jobs: &Jobs, app_id: i64, is_init: bool) -> Result<(), LoadError> {
    load::load_compose_file(jobs, app_id, is_init, Jobs::now()).await
}

/// `Application::oldRawParser()` for raw compose deployments: adds the Coolify labels, creates
/// the bind source directories on the server and returns the new `docker_compose_raw` (upstream
/// sets the attribute without saving it).
pub async fn old_raw_parser(jobs: &Jobs, app_id: i64) -> Result<String, String> {
    raw::old_raw_parser(jobs, app_id, Jobs::now()).await
}
