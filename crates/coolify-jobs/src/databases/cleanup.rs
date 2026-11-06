//! `CleanupDocker::dispatchAfterStop` (the run after a resource stop) and
//! `generate_log_drain_configuration`.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use coolify_compat::yaml::PhpValue;
use coolify_db::databases::DatabaseRow;
use serde_json::Value;

use crate::Jobs;
use crate::server::ServerInfo;

/// `CleanupDocker::QUEUED_DELAY`.
const QUEUED_DELAY: Duration = Duration::from_secs(60);
/// The queued jobs per server (`ShouldBeUnique` on the server uuid).
static QUEUED: LazyLock<Mutex<HashMap<String, ()>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

impl Jobs {
    /// `CleanupDocker::dispatchAfterStop`: one queued run per server, skipped after a recent run.
    pub fn spawn_cleanup_after_stop(&self, server_id: i64, server_uuid: String) {
        use crate::server_ops::{MARKS, cleanup};
        if MARKS.has(&cleanup::last_run_key(&server_uuid)) {
            return;
        }
        {
            let mut queued = QUEUED.lock().unwrap_or_else(|p| p.into_inner());
            if queued.insert(server_uuid.clone(), ()).is_some() {
                return;
            }
        }
        let jobs = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(QUEUED_DELAY).await;
            QUEUED
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&server_uuid);
            if let Err(e) = cleanup::cleanup_docker_above_threshold(&jobs, server_id).await {
                tracing::warn!(server = %server_uuid, error = %e, "Docker cleanup failed");
            }
        });
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
