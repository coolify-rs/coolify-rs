//! The application part of `GetContainersStatus` and `ContainerStatusAggregator`.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::Utc;
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::servers::{self, ColumnValue};
use coolify_remote::RunOptions;
use serde_json::Value;

use crate::Jobs;
use crate::server::ServerInfo;

/// `ContainerStatusAggregator::containerStatus()`.
pub fn container_status(c: &Value) -> String {
    let state = c
        .pointer("/State/Status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if state == "exited" {
        return "exited".into();
    }
    let health = c
        .pointer("/State/Health/Status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    format!("{state}:{health}")
}

/// `aggregateFromStrings($statuses, preserveRestarting: true)` with `resolveStatus`.
pub fn aggregate(statuses: &[String]) -> String {
    if statuses.is_empty() {
        return "exited".into();
    }
    let (mut degraded, mut restarting, mut running, mut unhealthy, mut unknown) =
        (false, false, false, false, false);
    let (mut exited, mut starting, mut paused, mut dead) = (false, false, false, false);
    for s in statuses {
        if s.contains("degraded") {
            degraded = true;
            if s.contains("unhealthy") {
                unhealthy = true;
            }
        } else if s.contains("restarting") {
            restarting = true;
        } else if s.contains("running") {
            running = true;
            if s.contains("unhealthy") {
                unhealthy = true;
            }
            if s.contains("unknown") {
                unknown = true;
            }
        } else if s.contains("exited") {
            exited = true;
        } else if s.contains("created") || s.contains("starting") {
            starting = true;
        } else if s.contains("paused") {
            paused = true;
        } else if s.contains("dead") || s.contains("removing") {
            dead = true;
        }
    }
    if degraded {
        "degraded:unhealthy"
    } else if restarting {
        "restarting:unknown"
    } else if running && exited {
        "degraded:unhealthy"
    } else if running && starting {
        "starting:unknown"
    } else if running {
        if unhealthy {
            "running:unhealthy"
        } else if unknown {
            "running:unknown"
        } else {
            "running:healthy"
        }
    } else if dead {
        "degraded:unhealthy"
    } else if paused {
        "paused:unknown"
    } else if starting {
        "starting:unknown"
    } else {
        "exited"
    }
    .into()
}

/// The `status` mutator: `x(y)` and `x:y` stay, a bare value gets `:unhealthy`.
pub fn normalize_status(v: &str) -> String {
    if let Some(i) = v.find('(') {
        let rest = &v[i + 1..];
        return format!(
            "{}:{}",
            v[..i].trim(),
            rest.split(')').next().unwrap_or_default().trim()
        );
    }
    if let Some((s, h)) = v.split_once(':') {
        return format!("{s}:{h}");
    }
    format!("{v}:unhealthy")
}

/// `resolveContainerOwner($applications, $labels, 'application')`.
fn owner<'a>(
    applications: &'a [ApplicationRow],
    labels: &serde_json::Map<String, Value>,
) -> Option<&'a ApplicationRow> {
    let label = |k: &str| {
        labels
            .get(k)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
    };
    if let Some(uuid) = label("coolify.applicationUuid") {
        return applications.iter().find(|a| a.uuid == uuid);
    }
    if label("coolify.applicationId").is_some()
        && let Some(project) =
            label("com.docker.compose.project").or_else(|| label("com.docker.stack.namespace"))
        && let Some(a) = applications.iter().find(|a| a.uuid == project)
    {
        return Some(a);
    }
    // Legacy containers without the uuid label: the uuid as a dash-delimited part of the name.
    let names = [label("com.docker.compose.service"), label("coolify.name")];
    applications.iter().find(|a| {
        let u = a.uuid.as_str();
        names.iter().flatten().any(|n| {
            *n == u
                || n.starts_with(&format!("{u}-"))
                || n.ends_with(&format!("-{u}"))
                || n.contains(&format!("-{u}-"))
        })
    })
}

impl Jobs {
    /// `GetContainersStatus::handle($server)` for the server's applications.
    pub async fn check_server_applications(&self, server_id: i64) {
        let db = &self.0.db;
        let Ok(Some(row)) = servers::find_by_id(db, server_id).await else {
            return;
        };
        let server = ServerInfo::from_row(&row);
        if !server.is_functional() || server.is_swarm() {
            return;
        }
        let Ok(applications) = apps::list_for_server(db, server_id).await else {
            return;
        };
        if applications.is_empty() {
            return;
        }
        let Ok(t) = server.target(self).await else {
            return;
        };
        let script = "docker container inspect $(docker container ls -aq) --format '{{json .}}'";
        let opts = RunOptions::op("server.containers").timeout(Duration::from_secs(30));
        let out = self
            .remote()
            .run_quiet(&t, &[script], opts)
            .await
            .unwrap_or_default();
        let containers: Vec<Value> = out
            .lines()
            .filter_map(|l| serde_json::from_str(l.trim()).ok())
            .collect();
        let now = Utc::now().naive_utc();
        let mut statuses: BTreeMap<i64, Vec<(String, i64)>> = BTreeMap::new();
        let mut found: Vec<i64> = Vec::new();
        for c in &containers {
            let Some(labels) = c.pointer("/Config/Labels").and_then(Value::as_object) else {
                continue;
            };
            let truthy = |k: &str| {
                labels
                    .get(k)
                    .and_then(Value::as_str)
                    .is_some_and(|v| !v.is_empty() && v != "0")
            };
            // `filter_var(..., FILTER_VALIDATE_BOOLEAN)`: compose writes `False` for regular containers.
            let oneoff = labels
                .get("com.docker.compose.oneoff")
                .and_then(Value::as_str)
                .is_some_and(|v| {
                    matches!(
                        v.trim().to_ascii_lowercase().as_str(),
                        "1" | "true" | "on" | "yes"
                    )
                });
            if oneoff {
                continue;
            }
            let is_app = labels
                .get("coolify.applicationUuid")
                .or_else(|| labels.get("coolify.applicationId"))
                .and_then(Value::as_str)
                .is_some_and(|v| !v.is_empty());
            if !is_app || truthy("coolify.pullRequestId") {
                continue;
            }
            let Some(a) = owner(&applications, labels) else {
                continue;
            };
            found.push(a.id);
            if a.raw.get("container_present").and_then(Value::as_bool) != Some(true) {
                let _ = apps::update_if_changed(
                    db,
                    a.id,
                    &[("container_present", ColumnValue::Bool(true))],
                    now,
                )
                .await;
            }
            if labels
                .get("com.docker.compose.service")
                .and_then(Value::as_str)
                .is_some()
            {
                let restarts = c.get("RestartCount").and_then(Value::as_i64).unwrap_or(0);
                statuses
                    .entry(a.id)
                    .or_default()
                    .push((container_status(c), restarts));
            }
        }
        for a in applications.iter().filter(|a| !found.contains(&a.id)) {
            if containers.is_empty() {
                break;
            }
            let status = a.str("status").unwrap_or_default();
            if status.starts_with("exited") {
                let _ = apps::update_if_changed(
                    db,
                    a.id,
                    &[
                        ("container_present", ColumnValue::Bool(false)),
                        ("restart_limit_reached", ColumnValue::Bool(false)),
                    ],
                    now,
                )
                .await;
                continue;
            }
            let recently_restarted = a.int("restart_count").unwrap_or(0) > 0
                && a.str("last_restart_at")
                    .and_then(|t| {
                        chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S%.f").ok()
                    })
                    .is_some_and(|t| t > now - chrono::Duration::seconds(30));
            let values: Vec<(&'static str, ColumnValue)> = if recently_restarted {
                vec![(
                    "status",
                    ColumnValue::Text(Some("degraded:unhealthy".into())),
                )]
            } else {
                vec![
                    ("status", ColumnValue::Text(Some("exited:unhealthy".into()))),
                    ("container_present", ColumnValue::Bool(false)),
                    ("restart_count", ColumnValue::Int(0)),
                    ("last_restart_at", ColumnValue::Text(None)),
                    ("last_restart_type", ColumnValue::Text(None)),
                    ("restart_limit_reached", ColumnValue::Bool(false)),
                ]
            };
            let _ = apps::update_if_changed(db, a.id, &values, now).await;
        }
        for (id, list) in statuses {
            let Some(a) = applications.iter().find(|a| a.id == id) else {
                continue;
            };
            let observed = list.iter().map(|(_, r)| *r).max().unwrap_or(0);
            if observed > a.int("restart_count").unwrap_or(0) {
                let _ = apps::update_if_changed(
                    db,
                    id,
                    &[
                        ("restart_count", ColumnValue::Int(observed)),
                        ("last_restart_at", ColumnValue::Time(now)),
                        ("last_restart_type", ColumnValue::Text(Some("crash".into()))),
                    ],
                    now,
                )
                .await;
            }
            let aggregated = normalize_status(&aggregate(
                &list.into_iter().map(|(s, _)| s).collect::<Vec<_>>(),
            ));
            let current = normalize_status(a.str("status").unwrap_or_default());
            if current != aggregated {
                let _ = apps::update_if_changed(
                    db,
                    id,
                    &[("status", ColumnValue::Text(Some(aggregated)))],
                    now,
                )
                .await;
            } else {
                let _ = apps::update_if_changed(
                    db,
                    id,
                    &[("last_online_at", ColumnValue::Time(now))],
                    now,
                )
                .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregation() {
        assert_eq!(aggregate(&["running:healthy".into()]), "running:healthy");
        assert_eq!(aggregate(&["running:starting".into()]), "running:healthy");
        assert_eq!(
            aggregate(&["running:unknown".into(), "exited".into()]),
            "degraded:unhealthy"
        );
        assert_eq!(aggregate(&[]), "exited");
        assert_eq!(normalize_status("exited"), "exited:unhealthy");
        assert_eq!(normalize_status("running (healthy)"), "running:healthy");
    }
}
