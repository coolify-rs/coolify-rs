//! The application part of `GetContainersStatus` and `ContainerStatusAggregator`.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::Utc;
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::instance_jobs as queries;
use coolify_db::records::Record;
use coolify_db::servers::{self, ColumnValue};
use coolify_remote::RunOptions;
use coolify_remote::shell::quote;
use serde_json::Value;

use crate::Jobs;
use crate::notifications::{RestartKind, lookup, notify};
use crate::server::ServerInfo;

/// What a list of containers can say about the resources missing from it.
#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    /// `isCompleteSnapshot()`.
    pub complete: bool,
    /// `GetContainersStatus`: no containers at all may be a failed Docker query.
    pub query_may_have_failed: bool,
}

impl Snapshot {
    pub const POLLED: Self = Self {
        complete: true,
        query_may_have_failed: true,
    };

    pub fn not_found_counts(self, containers: &[Value]) -> bool {
        self.complete && !(self.query_may_have_failed && containers.is_empty())
    }
}

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
    /// `Server::getContainers()`: the replica counts become a status on a Swarm service.
    async fn server_containers(&self, t: &coolify_remote::Target, swarm: bool) -> Vec<Value> {
        let opts = || RunOptions::op("server.containers").timeout(Duration::from_secs(30));
        let script = if swarm {
            "docker service inspect $(docker service ls -q) --format '{{json .}}'"
        } else {
            "docker container inspect $(docker container ls -aq) --format '{{json .}}'"
        };
        let out = self
            .remote()
            .run_quiet(t, &[script], opts())
            .await
            .unwrap_or_default();
        let mut containers: Vec<Value> = out
            .lines()
            .filter_map(|l| serde_json::from_str(l.trim()).ok())
            .collect();
        if swarm {
            let replicas = self
                .remote()
                .run_quiet(t, &["docker service ls --format '{{json .}}'"], opts())
                .await
                .unwrap_or_default();
            for line in replicas.lines() {
                let Ok(replica) = serde_json::from_str::<Value>(line.trim()) else {
                    continue;
                };
                let name = replica.get("Name").and_then(Value::as_str);
                let counts = replica
                    .get("Replicas")
                    .and_then(Value::as_str)
                    .unwrap_or("0/0");
                let mut parts = counts.split('/');
                let running = parts.next() == parts.next();
                let (state, health) = if running {
                    ("running", "healthy")
                } else {
                    ("starting", "unhealthy")
                };
                for c in &mut containers {
                    if c.pointer("/Spec/Name").and_then(Value::as_str) == name
                        && let Some(map) = c.as_object_mut()
                    {
                        map.insert(
                            "State".into(),
                            serde_json::json!({"Status": state, "Health": {"Status": health}}),
                        );
                    }
                }
            }
        }
        containers
    }

    /// `ComplexStatusCheck::handle($application)`.
    async fn complex_status_check(
        &self,
        a: &ApplicationRow,
        additional: &[coolify_db::application_ops::DestinationRow],
    ) {
        let db = &self.0.db;
        let main = a.server_id();
        let mut ids: Vec<i64> = additional.iter().map(|d| d.server_id).collect();
        ids.extend(main);
        for id in ids {
            let is_main = main == Some(id);
            let status = async {
                let row = servers::find_by_id(db, id).await.ok().flatten()?;
                let server = ServerInfo::from_row(&row);
                if !server.is_functional() {
                    return None;
                }
                let t = server.target(self).await.ok()?;
                let containers = self
                    .application_containers(&t, &a.uuid, false)
                    .await
                    .ok()?
                    .into_iter()
                    .filter(|c| {
                        crate::applications::ops::matches_pull_request(
                            c.get("Labels").and_then(Value::as_str).unwrap_or_default(),
                            Some(0),
                        )
                    })
                    .collect::<Vec<_>>();
                if containers.is_empty() {
                    return None;
                }
                let names: Vec<String> = containers
                    .iter()
                    .filter_map(|c| c.get("Names").and_then(Value::as_str).map(quote))
                    .collect();
                let inspect = format!(
                    "docker container inspect {} --format '{{{{json .}}}}'",
                    names.join(" ")
                );
                let out = self
                    .remote()
                    .run_quiet(&t, &[&inspect], RunOptions::op("server.containers"))
                    .await?;
                let statuses: Vec<String> = out
                    .lines()
                    .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
                    .map(|c| container_status(&c))
                    .collect();
                Some(aggregate(&statuses))
            }
            .await
            .unwrap_or_else(|| "exited".to_owned());
            let now = Utc::now().naive_utc();
            if is_main {
                if a.str("status") != Some(status.as_str()) {
                    let _ = apps::update_if_changed(
                        db,
                        a.id,
                        &[("status", ColumnValue::Text(Some(status)))],
                        now,
                    )
                    .await;
                }
            } else {
                let _ = apps::set_additional_status(db, a.id, id, &status, now).await;
            }
        }
    }

    /// `GetContainersStatus::handle($server)` for the server's applications.
    pub async fn check_server_applications(&self, server_id: i64) {
        let db = &self.0.db;
        let Ok(Some(row)) = servers::find_by_id(db, server_id).await else {
            return;
        };
        let server = ServerInfo::from_row(&row);
        if !server.is_functional() {
            return;
        }
        let Ok(mut applications) = apps::list_for_server(db, server_id).await else {
            return;
        };
        // An application with additional servers goes through `ComplexStatusCheck`.
        let mut simple = Vec::with_capacity(applications.len());
        for a in applications.drain(..) {
            let additional = coolify_db::application_ops::additional_networks(db, a.id)
                .await
                .unwrap_or_default();
            if additional.is_empty() {
                simple.push(a);
            } else {
                self.complex_status_check(&a, &additional).await;
            }
        }
        let applications = simple;
        if applications.is_empty() {
            return;
        }
        let Ok(t) = server.target(self).await else {
            return;
        };
        let swarm = server.is_swarm();
        let containers = self.server_containers(&t, swarm).await;
        self.apply_application_containers(&applications, &containers, Snapshot::POLLED, swarm)
            .await;
    }

    /// The application part of `GetContainersStatus` and `PushServerUpdateJob`.
    pub async fn apply_application_containers(
        &self,
        applications: &[ApplicationRow],
        containers: &[Value],
        snapshot: Snapshot,
        swarm: bool,
    ) {
        let db = &self.0.db;
        let not_found_counts = snapshot.not_found_counts(containers);
        let now = Utc::now().naive_utc();
        let mut statuses: BTreeMap<i64, Vec<(String, i64)>> = BTreeMap::new();
        let mut found: Vec<i64> = Vec::new();
        let mut found_previews: Vec<i64> = Vec::new();
        // $previewContainerRestartCounts: the preview, its application and the highest count of its
        // containers.
        let mut preview_restarts: BTreeMap<i64, (Record, i64, i64)> = BTreeMap::new();
        for c in containers {
            let labels_at = if swarm {
                "/Spec/Labels"
            } else {
                "/Config/Labels"
            };
            let Some(labels) = c.pointer(labels_at).and_then(Value::as_object) else {
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
            if !is_app {
                continue;
            }
            if truthy("coolify.pullRequestId") {
                // The preview that owns the container.
                let pull_request_id = labels
                    .get("coolify.pullRequestId")
                    .and_then(Value::as_str)
                    .and_then(|v| v.parse::<i64>().ok());
                if let (Some(a), Some(pr)) = (owner(applications, labels), pull_request_id)
                    && let Ok(Some(preview)) =
                        coolify_db::application_ops::preview(db, a.id, pr).await
                {
                    let id = preview
                        .get("id")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    found_previews.push(id);
                    let observed = c.get("RestartCount").and_then(Value::as_i64).unwrap_or(0);
                    let entry = preview_restarts
                        .entry(id)
                        .or_insert_with(|| (preview.clone(), a.id, 0));
                    entry.2 = entry.2.max(observed);
                    let status = container_status(c);
                    let values =
                        if preview.get("status").and_then(Value::as_str) == Some(status.as_str()) {
                            ("last_online_at", ColumnValue::Time(now))
                        } else {
                            ("status", ColumnValue::Text(Some(status)))
                        };
                    let _ =
                        coolify_db::application_ops::update_preview(db, id, &[values], now).await;
                }
                continue;
            }
            let Some(a) = owner(applications, labels) else {
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
            // Swarm services have other labels.
            let service_label = labels.get("com.docker.compose.service").or_else(|| {
                if swarm {
                    labels
                        .get("coolify.serviceName")
                        .or_else(|| labels.get("coolify.name"))
                        .or_else(|| labels.get("com.docker.stack.namespace"))
                } else {
                    None
                }
            });
            if service_label.and_then(Value::as_str).is_some() {
                let restarts = c.get("RestartCount").and_then(Value::as_i64).unwrap_or(0);
                statuses
                    .entry(a.id)
                    .or_default()
                    .push((container_status(c), restarts));
            }
        }
        // `! isCompleteSnapshot()`: only what the containers themselves say.
        if !snapshot.complete {
            return;
        }
        // trackPreviewRestartCounts()
        for (id, (preview, application_id, observed)) in preview_restarts {
            let stored = preview
                .get("restart_count")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            if observed > stored {
                let _ = coolify_db::application_ops::update_preview(
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
            let Some(a) = applications.iter().find(|a| a.id == application_id) else {
                continue;
            };
            // `restartLimitMaximum()`: the application's maximum, then the preview's own.
            let max = a
                .int("max_restart_count")
                .or_else(|| preview.get("max_restart_count").and_then(Value::as_i64))
                .unwrap_or(0);
            if max > 0
                && observed >= max
                && queries::claim_preview_restart_limit(db, id, now)
                    .await
                    .unwrap_or(false)
            {
                let pull_request_id = preview
                    .get("pull_request_id")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let jobs = self.clone();
                let stopped = (*a).clone();
                tokio::spawn(async move {
                    jobs.stop_application_preview(&stopped, id, pull_request_id, false, false)
                        .await;
                });
                if let Some(environment_id) = a.int("environment_id")
                    && let Some((team_id, notification)) = lookup::application_restart_limit(
                        db,
                        environment_id,
                        RestartKind::Preview,
                        format!(
                            "{} PR #{pull_request_id}",
                            a.str("name").unwrap_or_default()
                        ),
                        preview
                            .get("uuid")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                        &a.uuid,
                        preview.get("fqdn").and_then(Value::as_str),
                        observed.max(stored),
                        max,
                    )
                    .await
                {
                    notify(db, self.0.encrypter.as_ref(), team_id, notification).await;
                }
            }
        }
        // $notRunningApplicationPreviews
        if not_found_counts {
            for a in applications {
                for preview in coolify_db::application_ops::previews(db, a.id)
                    .await
                    .unwrap_or_default()
                {
                    let id = preview
                        .get("id")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    let status = preview
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !found_previews.contains(&id) && !status.starts_with("exited") {
                        let _ = coolify_db::application_ops::update_preview(
                            db,
                            id,
                            &[("status", ColumnValue::Text(Some("exited".into())))],
                            now,
                        )
                        .await;
                    }
                }
            }
        }
        for a in applications.iter().filter(|a| !found.contains(&a.id)) {
            if !not_found_counts {
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
            let previous = a.int("restart_count").unwrap_or(0);
            let max = a.int("max_restart_count").unwrap_or(0);
            if observed > previous {
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
            // `$restartLimitReached`: the first check that sees it stops the application and tells
            // the team; the application stays stopped until a deployment resets the limit.
            if max > 0
                && observed >= max
                && queries::claim_application_restart_limit(db, id, now)
                    .await
                    .unwrap_or(false)
            {
                let jobs = self.clone();
                let stopped = a.clone();
                tokio::spawn(async move {
                    jobs.stop_application(&stopped, false, false, false).await;
                });
                if let Some(environment_id) = a.int("environment_id")
                    && let Some((team_id, notification)) = lookup::application_restart_limit(
                        db,
                        environment_id,
                        RestartKind::Application,
                        a.str("name").unwrap_or_default().to_owned(),
                        &a.uuid,
                        &a.uuid,
                        a.str("fqdn"),
                        observed.max(previous),
                        max,
                    )
                    .await
                {
                    notify(db, self.0.encrypter.as_ref(), team_id, notification).await;
                }
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
