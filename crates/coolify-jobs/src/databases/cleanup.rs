//! `CleanupDocker::dispatchAfterStop` (the run after a resource stop) and
//! `generate_log_drain_configuration`.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use coolify_compat::yaml::PhpValue;
use coolify_db::databases::DatabaseRow;
use coolify_remote::{RunOptions, Target, shell::quote};
use serde_json::Value;

use super::common::HELPER_VERSION;
use crate::Jobs;
use crate::server::ServerInfo;

/// `CleanupDocker::QUEUED_DELAY`.
const QUEUED_DELAY: Duration = Duration::from_secs(60);
/// `CleanupDocker::STOP_CLEANUP_COOLDOWN`.
const COOLDOWN: Duration = Duration::from_secs(3600);
/// `CleanupDocker::REMOTE_COMMANDS_DEADLINE`.
const DEADLINE: Duration = Duration::from_secs(540);

/// `lastRunCacheKey` and the queued jobs per server.
static LAST_RUN: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static QUEUED: LazyLock<Mutex<HashMap<String, ()>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn escape_ere(v: &str) -> String {
    let mut out = String::new();
    for c in v.chars() {
        if ".\\+*?[]^$(){}|".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `buildContainerPruneCommand`.
pub fn container_prune_command() -> String {
    "docker ps -a --filter status=created --filter status=exited --filter status=dead --filter label=coolify.managed=true --format '{{.ID}} coolify.proxy={{.Label \"coolify.proxy\"}} coolify.type={{.Label \"coolify.type\"}}' | grep -v -E 'coolify\\.proxy=true |coolify\\.type=(database|application|service)$' | awk '{print $1}' | xargs -r docker rm".to_owned()
}

/// `buildCoolifyImageCleanupCommand`.
pub fn coolify_image_cleanup_command() -> String {
    let version = escape_ere(HELPER_VERSION);
    format!(
        "docker images --format '{{{{.Repository}}}}:{{{{.Tag}}}}' | grep -E '(^|/)coollabsio/(coolify-helper|coolify-realtime|sentinel):' | grep -v -E ':<none>$|(^|/)coollabsio/coolify-helper:{version}$' | xargs -r docker rmi 2>/dev/null || true"
    )
}

/// `buildImagePruneCommand`.
pub fn image_prune_command(application_repos: &[String]) -> String {
    let version = escape_ere(HELPER_VERSION);
    let infra = format!("(^|/)coollabsio/coolify-helper:{version}$");
    let mut grep = "grep -v '<none>'".to_owned();
    if !application_repos.is_empty() {
        let patterns: Vec<String> = application_repos.iter().map(|r| escape_ere(r)).collect();
        grep.push_str(&format!(
            " | grep -v -E {}",
            quote(&format!("^({})[_:@].+", patterns.join("|")))
        ));
    }
    grep.push_str(&format!(" | grep -v -E {}", quote(&infra)));
    format!(
        "docker image prune -f && docker images --digests --format '{{{{if eq .Tag \"<none>\"}}}}{{{{.Repository}}}}@{{{{.Digest}}}}{{{{else}}}}{{{{.Repository}}}}:{{{{.Tag}}}}{{{{end}}}}' | {grep} | xargs -r -I {{}} sh -c 'docker inspect --format \"{{{{index .Config.Labels \\\"coolify.managed\\\"}}}}\" \"{{}}\" 2>/dev/null | grep -q true || docker rmi \"{{}}\" 2>/dev/null' || true"
    )
}

impl Jobs {
    /// `CleanupDocker::dispatchAfterStop`: one queued run per server, skipped after a recent run.
    pub fn spawn_cleanup_after_stop(&self, server: ServerInfo) {
        {
            let last = LAST_RUN.lock().unwrap_or_else(|p| p.into_inner());
            if last
                .get(&server.uuid)
                .is_some_and(|t| t.elapsed() < COOLDOWN)
            {
                return;
            }
        }
        {
            let mut queued = QUEUED.lock().unwrap_or_else(|p| p.into_inner());
            if queued.insert(server.uuid.clone(), ()).is_some() {
                return;
            }
        }
        let jobs = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(QUEUED_DELAY).await;
            QUEUED
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&server.uuid);
            if let Err(e) = jobs.cleanup_docker_above_threshold(&server).await {
                tracing::warn!(server = %server.uuid, error = %e, "Docker cleanup failed");
            }
        });
    }

    /// `CleanupDocker::handle($server, false, false, onlyAboveThreshold: true)`.
    /// Application images keep their rollback copies: `cleanupApplicationImages` is not run here,
    /// and the prune excludes application repositories as upstream does.
    async fn cleanup_docker_above_threshold(&self, server: &ServerInfo) -> Result<(), String> {
        let db = &self.0.db;
        if coolify_db::applications::server_has_active_deployment(db, server.id)
            .await
            .map_err(|e| e.to_string())?
        {
            return Ok(());
        }
        let t: Target = server.target(self).await?;
        let usage = self
            .remote()
            .run_quiet(
                &t,
                &["df / --output=pcent | tr -cd 0-9"],
                RunOptions::op("docker.cleanup"),
            )
            .await
            .unwrap_or_default();
        let threshold = server
            .settings
            .get("docker_cleanup_threshold")
            .and_then(Value::as_i64)
            .unwrap_or(80);
        if usage.trim().parse::<i64>().is_ok_and(|u| u < threshold) {
            return Ok(());
        }
        let repos = coolify_db::applications::server_image_repositories(db, server.id)
            .await
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + DEADLINE;
        for command in [
            container_prune_command(),
            image_prune_command(&repos),
            "docker builder prune -af".to_owned(),
            coolify_image_cleanup_command(),
        ] {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("Docker cleanup did not finish within 540 seconds. The remaining cleanup commands were not run.".to_owned());
            }
            self.remote()
                .run_quiet(
                    &t,
                    &[command.as_str()],
                    RunOptions::op("docker.cleanup").timeout(left),
                )
                .await;
        }
        LAST_RUN
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(server.uuid.clone(), Instant::now());
        Ok(())
    }
}

fn fluent_bit_enabled(server: &ServerInfo) -> bool {
    [
        "is_logdrain_newrelic_enabled",
        "is_logdrain_axiom_enabled",
        "is_logdrain_custom_enabled",
    ]
    .iter()
    .any(|k| server.setting_bool(k) == Some(true))
}

/// `Server::isLogDrainEnabled`.
pub fn server_log_drain_enabled(server: &ServerInfo) -> bool {
    fluent_bit_enabled(server)
        || server.setting_bool("is_logdrain_cloudwatch_enabled") == Some(true)
}

/// `generate_log_drain_configuration`, when the server and the database use a log drain.
pub fn log_drain_configuration(server: &ServerInfo, d: &DatabaseRow) -> Option<PhpValue> {
    if !server_log_drain_enabled(server) || !d.bool("is_log_drain_enabled") {
        return None;
    }
    if fluent_bit_enabled(server) {
        return Some(PhpValue::map([
            ("driver", PhpValue::from("fluentd")),
            (
                "options",
                PhpValue::map([
                    ("fluentd-address", PhpValue::from("tcp://127.0.0.1:24224")),
                    ("fluentd-async", PhpValue::from("true")),
                    ("fluentd-sub-second-precision", PhpValue::from("true")),
                    (
                        "env",
                        PhpValue::from(
                            "COOLIFY_APP_NAME,COOLIFY_PROJECT_NAME,COOLIFY_SERVER_IP,COOLIFY_ENVIRONMENT_NAME",
                        ),
                    ),
                ]),
            ),
        ]));
    }
    let setting = |k: &str| {
        server
            .settings
            .get(k)
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
            .map(str::to_owned)
    };
    let mut options = PhpValue::Array(Vec::new());
    if let Some(v) = setting("logdrain_cloudwatch_region") {
        options.set("awslogs-region", PhpValue::from(v));
    }
    if let Some(v) = setting("logdrain_cloudwatch_group") {
        options.set("awslogs-group", PhpValue::from(v));
    }
    options.set("awslogs-create-group", PhpValue::from("true"));
    options.set("mode", PhpValue::from("non-blocking"));
    let prefix = setting("logdrain_cloudwatch_stream_prefix").unwrap_or_default();
    options.set("tag", PhpValue::from(format!("{prefix}{{{{.Name}}}}")));
    Some(PhpValue::map([
        ("driver", PhpValue::from("awslogs")),
        ("options", options),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_commands() {
        assert!(coolify_image_cleanup_command().contains("coolify-helper:1\\.0\\.19$"));
        assert!(image_prune_command(&["abc".to_owned()]).contains("'^(abc)[_:@].+'"));
    }
}
