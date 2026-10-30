//! The services part of `GetContainersStatus`, `ResourceStatusRefresher::refreshService()` and the
//! status accessors of `Service` (`getStatusAttribute()`, `aggregateResourceStatuses()`).

use std::time::Duration;

use coolify_db::servers::{self, ColumnValue};
use coolify_db::services::{self as svc, ServiceRow, SubKind, SubRow};
use coolify_remote::{RunOptions, shell::quote};
use serde_json::{Map, Value};

use crate::Jobs;
use crate::applications::status::{aggregate, container_status};
use crate::compose_parser::{php, validate};
use crate::server::ServerInfo;
use coolify_compat::yaml::PhpValue;

/// `HEALTH_WAIT_ATTEMPTS` of `ResourceStatusRefresher`.
const HEALTH_WAIT_ATTEMPTS: u32 = 12;

/// `aggregateFromStrings($statuses)`: a restarting container makes the stack degraded.
pub fn aggregate_for_stack(statuses: &[String]) -> String {
    if statuses
        .iter()
        .any(|s| !s.contains("degraded") && s.contains("restarting"))
        && !statuses.iter().any(|s| s.contains("degraded"))
    {
        return "degraded:unhealthy".into();
    }
    aggregate(statuses)
}

/// `appendExcludedSuffix($status)`.
fn append_excluded_suffix(status: &str) -> String {
    if status.starts_with("degraded") {
        return "degraded:excluded".into();
    }
    if status.starts_with("paused") {
        return "paused:excluded".into();
    }
    if status.starts_with("starting") {
        return "starting:excluded".into();
    }
    if status.starts_with("exited") {
        return "exited".into();
    }
    format!("{status}:excluded")
}

/// `getExcludedContainersFromDockerCompose($raw)`.
fn excluded_containers(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return Vec::new();
    };
    let Ok(yaml) = validate::parse_docker_compose_yaml(raw) else {
        return Vec::new();
    };
    let Some(PhpValue::Array(services)) = php::get(&yaml, "services") else {
        return Vec::new();
    };
    services
        .iter()
        .filter(|(_, c)| {
            php::get(c, "exclude_from_hc").is_some_and(php::truthy)
                || php::get(c, "restart") == Some(&PhpValue::Str("no".into()))
        })
        .map(|(k, _)| k.as_string())
        .collect()
}

/// `aggregateForCompose($containerStatuses, $dockerComposeRaw)`.
pub fn aggregate_for_compose(statuses: &[(String, String)], raw: Option<&str>) -> String {
    let excluded = excluded_containers(raw);
    let relevant: Vec<String> = statuses
        .iter()
        .filter(|(name, _)| !excluded.contains(name))
        .map(|(_, s)| s.clone())
        .collect();
    if relevant.is_empty() {
        let all: Vec<String> = statuses.iter().map(|(_, s)| s.clone()).collect();
        return append_excluded_suffix(&aggregate_for_stack(&all));
    }
    aggregate(&relevant)
}

/// `Service::containerStatus()` from the stored statuses of its applications and databases.
pub fn container_status_of(subs: &[SubRow]) -> Option<String> {
    let status_of = |s: &SubRow| s.str("status").unwrap_or_default().to_owned();
    let is_excluded =
        |s: &SubRow| s.bool("exclude_from_status") || status_of(s).contains(":excluded");
    let strip = |s: &SubRow| php::before(&status_of(s), ":excluded");
    let split = |aggregated: String| {
        let mut parts = aggregated.split(':');
        (
            parts.next().map(str::to_owned),
            parts.next().map(str::to_owned),
        )
    };
    let normal: Vec<String> = subs.iter().filter(|s| !is_excluded(s)).map(strip).collect();
    if normal.is_empty() {
        let excluded: Vec<String> = subs.iter().filter(|s| is_excluded(s)).map(strip).collect();
        if excluded.is_empty() {
            return Some("unknown:unknown:excluded".into());
        }
        let (status, health) = split(aggregate_for_stack(&excluded));
        return match (status, health) {
            (Some(s), Some(h)) if !s.is_empty() && !h.is_empty() => {
                Some(format!("{s}:{h}:excluded"))
            }
            _ => Some("exited".into()),
        };
    }
    let (status, health) = split(aggregate_for_stack(&normal));
    match health.filter(|h| !h.is_empty()) {
        Some(h) => Some(format!("{}:{h}", status.unwrap_or_default())),
        None => status,
    }
}

fn labels_of(container: &Value) -> Map<String, Value> {
    container
        .pointer("/Config/Labels")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn label<'a>(labels: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    labels
        .get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
}

fn oneoff(labels: &Map<String, Value>) -> bool {
    matches!(
        label(labels, "com.docker.compose.oneoff")
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("true" | "1" | "on" | "yes")
    )
}

/// `resolveServiceContainerOwner($services, $labels)`'s part lookup within one service.
fn part_of<'a>(
    apps: &'a [SubRow],
    databases: &'a [SubRow],
    labels: &Map<String, Value>,
) -> Option<&'a SubRow> {
    let parts = if label(labels, "coolify.service.subType") == Some("database") {
        databases
    } else {
        apps
    };
    if let Some(uuid) = label(labels, "coolify.service.subUuid") {
        return parts.iter().find(|p| p.uuid == uuid);
    }
    let by_name = label(labels, "com.docker.compose.service")
        .and_then(|n| parts.iter().find(|p| p.name == n));
    by_name.or_else(|| {
        label(labels, "coolify.service.subId")
            .and_then(|i| i.parse::<i64>().ok())
            .and_then(|i| parts.iter().find(|p| p.id == i))
    })
}

/// A part, the statuses of its containers by compose name, and its highest restart count.
type Group = (SubRow, Vec<(String, String)>, i64);

impl Jobs {
    /// `ResourceStatusRefresher::store()`.
    async fn store_sub_status(&self, sub: &SubRow, status: &str) {
        let now = Jobs::now();
        if sub.str("status") != Some(status) {
            let _ = svc::sub_set_status(&self.0.db, sub.kind, sub.id, status, now).await;
        } else {
            let _ = svc::sub_touch_online(&self.0.db, sub.kind, sub.id, now).await;
        }
    }

    /// `ResourceStatusRefresher::refreshService($service)`.
    pub async fn refresh_service_status(&self, s: &ServiceRow, server: &ServerInfo) {
        if !server.is_functional() || server.is_swarm() {
            return;
        }
        let Ok(target) = server.target(self).await else {
            return;
        };
        let script = format!(
            "{}; [ -z \"$container_ids\" ] || docker container inspect $container_ids --format '{{{{json .}}}}'",
            super::container_ids_script(&s.uuid)
        );
        let command = format!("sh -c {}", quote(&script));
        for attempt in 1..=HEALTH_WAIT_ATTEMPTS {
            let Some(out) = self
                .remote()
                .run_quiet(
                    &target,
                    &[command.as_str()],
                    RunOptions::op("service.status"),
                )
                .await
            else {
                return;
            };
            let containers: Vec<Value> = out
                .lines()
                .filter_map(|l| serde_json::from_str(l.trim()).ok())
                .collect();
            if containers.is_empty() {
                return;
            }
            let db = &self.0.db;
            let apps = svc::subs(db, SubKind::Application, s.id)
                .await
                .unwrap_or_default();
            let databases = svc::subs(db, SubKind::Database, s.id)
                .await
                .unwrap_or_default();
            let mut groups: Vec<(SubRow, Vec<(String, String)>)> = Vec::new();
            for c in &containers {
                let labels = labels_of(c);
                let Some(name) = label(&labels, "com.docker.compose.service") else {
                    continue;
                };
                if oneoff(&labels) {
                    continue;
                }
                let Some(part) = part_of(&apps, &databases, &labels) else {
                    continue;
                };
                let entry = (name.to_owned(), container_status(c));
                match groups
                    .iter_mut()
                    .find(|(p, _)| p.id == part.id && p.kind == part.kind)
                {
                    Some((_, list)) => list.push(entry),
                    None => groups.push((part.clone(), vec![entry])),
                }
            }
            for (part, statuses) in &groups {
                let status = aggregate_for_compose(statuses, s.str("docker_compose_raw"));
                self.store_sub_status(part, &status).await;
            }
            let starting = containers.iter().any(|c| {
                c.pointer("/State/Health/Status").and_then(Value::as_str) == Some("starting")
            });
            if !starting || attempt >= HEALTH_WAIT_ATTEMPTS {
                return;
            }
            tokio::time::sleep(Self::health_wait()).await;
        }
    }

    /// The services part of `GetContainersStatus` for one server.
    #[allow(clippy::too_many_lines)]
    pub async fn check_server_services(&self, server_id: i64) {
        let db = &self.0.db;
        let Ok(Some(row)) = servers::find_by_id(db, server_id).await else {
            return;
        };
        let server = ServerInfo::from_row(&row);
        if !server.is_functional() || server.is_swarm() {
            return;
        }
        let Ok(services) = svc::list_for_server(db, server_id).await else {
            return;
        };
        if services.is_empty() {
            return;
        }
        let Ok(t) = server.target(self).await else {
            return;
        };
        let script = "docker container inspect $(docker container ls -aq) --format '{{json .}}'";
        let opts = RunOptions::op("server.containers").timeout(Duration::from_secs(30));
        let Some(out) = self.remote().run_quiet(&t, &[script], opts).await else {
            return;
        };
        let containers: Vec<Value> = out
            .lines()
            .filter_map(|l| serde_json::from_str(l.trim()).ok())
            .collect();
        // Only protection: if no containers at all, the Docker query might have failed.
        if containers.is_empty() {
            return;
        }
        let now = Jobs::now();
        for s in &services {
            let apps = svc::subs(db, SubKind::Application, s.id)
                .await
                .unwrap_or_default();
            let databases = svc::subs(db, SubKind::Database, s.id)
                .await
                .unwrap_or_default();
            let mut groups: Vec<Group> = Vec::new();
            let mut found: Vec<(SubKind, i64)> = Vec::new();
            for c in &containers {
                let labels = labels_of(c);
                if oneoff(&labels) {
                    continue;
                }
                let owned = label(&labels, "coolify.serviceUuid") == Some(s.uuid.as_str())
                    || (label(&labels, "coolify.serviceId").is_some()
                        && label(&labels, "com.docker.compose.project") == Some(s.uuid.as_str()));
                if !owned {
                    continue;
                }
                let Some(part) = part_of(&apps, &databases, &labels) else {
                    continue;
                };
                found.push((part.kind, part.id));
                let Some(name) = label(&labels, "com.docker.compose.service") else {
                    continue;
                };
                let restarts = c.get("RestartCount").and_then(Value::as_i64).unwrap_or(0);
                let entry = (name.to_owned(), container_status(c));
                match groups
                    .iter_mut()
                    .find(|(p, ..)| p.id == part.id && p.kind == part.kind)
                {
                    Some((_, list, max)) => {
                        list.push(entry);
                        *max = (*max).max(restarts);
                    }
                    None => groups.push((part.clone(), vec![entry], restarts)),
                }
            }
            for part in apps.iter().chain(databases.iter()) {
                if found.contains(&(part.kind, part.id)) {
                    continue;
                }
                if part.str("status").unwrap_or_default().starts_with("exited") {
                    continue;
                }
                let _ = svc::sub_set_status(db, part.kind, part.id, "exited", now).await;
                if part.is_application() {
                    let _ = svc::reset_restart_limit(db, part.id, now).await;
                }
            }
            for (part, statuses, restarts) in &groups {
                if part.is_application() && self.track_restart_count(s, part, *restarts).await {
                    continue;
                }
                let status = aggregate_for_compose(statuses, s.str("docker_compose_raw"));
                self.store_sub_status(part, &status).await;
            }
        }
    }

    /// `HasRestartLimit::trackRestartCount()`: true when this call claimed the restart limit, which
    /// stops and removes the container (the notification is not ported).
    async fn track_restart_count(&self, s: &ServiceRow, part: &SubRow, observed: i64) -> bool {
        let db = &self.0.db;
        let now = Jobs::now();
        let previous = part.int("restart_count").unwrap_or(0);
        let max = part.int("max_restart_count").unwrap_or(0);
        if observed > previous {
            let values = [
                ("restart_count", ColumnValue::Int(observed)),
                ("last_restart_at", ColumnValue::Time(now)),
                ("last_restart_type", ColumnValue::Text(Some("crash".into()))),
            ];
            let _ = svc::sub_update(db, part.kind, part.id, &values, now).await;
        }
        if !(max > 0 && observed >= max) || part.bool("restart_limit_reached") {
            return false;
        }
        let values = [("restart_limit_reached", ColumnValue::Bool(true))];
        let _ = svc::sub_update(db, part.kind, part.id, &values, now).await;
        let jobs = self.clone();
        let (s, part) = (s.clone(), part.clone());
        tokio::spawn(async move {
            jobs.stop_sub(&s, &part, false, false).await;
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_status_treats_restarting_as_degraded() {
        assert_eq!(
            aggregate_for_stack(&["restarting:unknown".to_owned()]),
            "degraded:unhealthy"
        );
        assert_eq!(
            aggregate_for_stack(&["running:healthy".to_owned()]),
            "running:healthy"
        );
    }

    #[test]
    fn excluded_suffix() {
        assert_eq!(
            append_excluded_suffix("running:healthy"),
            "running:healthy:excluded"
        );
        assert_eq!(append_excluded_suffix("exited"), "exited");
    }
}
