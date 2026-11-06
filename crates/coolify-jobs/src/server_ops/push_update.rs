//! `PushServerUpdateJob`: what a Sentinel push changes. The rows follow the same rules as
//! `GetContainersStatus`, so the Sentinel containers are turned into the shape of
//! `docker container inspect` and go through the same code.

use std::time::Duration;

use coolify_db::applications as apps;
use coolify_db::databases as dbs;
use coolify_db::services as svc;
use serde_json::{Value, json};

use super::{MARKS, Srv, checks, proxy};
use crate::Jobs;
use crate::applications::status::Snapshot;
use crate::notifications::{self, Notification, lookup};
use crate::scheduler::Overlap;

/// `$timeout` and `expireAfter(30)`.
const LOCK_SECONDS: u64 = 30;
/// `HighDiskUsage::RECOVERY_MARGIN`.
const RECOVERY_MARGIN: f64 = 5.0;

/// A container of the push as `docker container inspect` shows it. `None` for one Coolify does not
/// manage, or a one-off: `handle()` skips those. A database's uuid is its compose service.
pub fn inspect_shape(container: &Value) -> Option<Value> {
    let mut labels = container
        .get("labels")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if !labels.contains_key("coolify.managed") {
        return None;
    }
    let is_type = |labels: &serde_json::Map<String, Value>, key: &str| labels.contains_key(key);
    if !is_type(&labels, "coolify.applicationId")
        && !is_type(&labels, "coolify.applicationUuid")
        && !is_type(&labels, "coolify.serviceId")
        && !is_type(&labels, "coolify.serviceUuid")
        && !is_type(&labels, "coolify.databaseUuid")
        && let Some(uuid) = labels.get("com.docker.compose.service").cloned()
    {
        labels.insert("coolify.databaseUuid".into(), uuid);
    }
    let name = container
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut state = json!({
        "Status": container.get("state").and_then(Value::as_str).unwrap_or("exited"),
    });
    if let Some(health) = container.get("health_status").and_then(Value::as_str) {
        state["Health"] = json!({"Status": health});
    }
    Some(json!({
        "Name": format!("/{name}"),
        "RestartCount": container.get("restart_count").and_then(Value::as_i64).unwrap_or(0),
        "State": state,
        "Config": {"Labels": labels},
    }))
}

/// `isRunning()`.
fn running(container: &Value) -> bool {
    container.get("state").and_then(Value::as_str) == Some("running")
}

fn managed_named(containers: &[Value], name: &str) -> bool {
    containers.iter().any(|c| {
        c.get("name").and_then(Value::as_str) == Some(name)
            && c.pointer("/labels/coolify.managed").is_some()
            && running(c)
    })
}

fn named_running(containers: &[Value], name: &str) -> bool {
    containers
        .iter()
        .any(|c| c.get("name").and_then(Value::as_str) == Some(name) && running(c))
}

/// The disk part of `handle()`.
async fn check_disk_usage(jobs: &Jobs, srv: &Srv, data: &Value) {
    let Some(usage) = data
        .pointer("/filesystem_usage_root/used_percentage")
        .and_then(Value::as_f64)
    else {
        return;
    };
    let id = srv.id();
    let threshold = srv
        .setting_int("server_disk_usage_notification_threshold")
        .unwrap_or(80);
    let threshold_f = threshold as f64;
    let check_key = format!("storage-check:{id}");
    let high_key = format!("storage-high:{id}");
    if usage >= threshold_f {
        if MARKS.get(&check_key).as_deref() != Some(usage.to_string().as_str()) {
            MARKS.put(check_key, usage.to_string(), Duration::from_secs(600));
            MARKS.put(high_key, "1".into(), Duration::from_secs(u64::MAX / 4));
            // `ServerStorageCheckJob::dispatch($this->server, $filesystemUsageRoot)`.
            if let Err(e) = checks::report_storage_usage(jobs, srv, usage as i64).await {
                tracing::warn!(server_id = id, error = %e, "server storage check failed");
            }
        }
    } else {
        MARKS.forget(&check_key);
        if usage <= threshold_f - RECOVERY_MARGIN && MARKS.has(&high_key) {
            MARKS.forget(&high_key);
            notifications::release_high_disk_usage(srv.db(), id).await;
        }
    }
}

/// `updateProxyStatus()`.
async fn update_proxy_status(
    jobs: &Jobs,
    srv: &mut Srv,
    containers: &[Value],
) -> Result<(), String> {
    // `isProxyShouldRun()`
    let should_run = srv.proxy_type().is_some_and(|t| t != "NONE") && !srv.info.is_build_server();
    if !should_run || srv.is_proxy_transition_in_progress() {
        return Ok(());
    }
    if !managed_named(containers, "coolify-proxy") {
        if proxy::check_proxy(srv, false).await.unwrap_or(false)
            && proxy::start_proxy(srv, false, false, false).await.is_ok()
            && let Some((team_id, server)) = lookup::server(srv.db(), srv.id()).await
        {
            notifications::notify(
                srv.db(),
                jobs.0.encrypter.as_ref(),
                team_id,
                Notification::ContainerRestarted {
                    name: "coolify-proxy".to_owned(),
                    server,
                    url: None,
                    throttle: Some(("App\\Models\\Server".to_owned(), srv.id())),
                },
            )
            .await;
        }
    } else if srv.proxy.get("status").and_then(Value::as_str) != Some("running") {
        srv.proxy_set("status", json!("running"));
        srv.save_proxy().await?;
    }
    Ok(())
}

/// The TCP proxy part of `updateDatabaseStatus()`.
async fn update_database_proxies(jobs: &Jobs, srv: &Srv, containers: &[Value]) {
    let Ok(databases) = dbs::list_for_server(srv.db(), srv.id()).await else {
        return;
    };
    for d in databases.into_iter().filter(|d| d.kind.ported()) {
        let is_running = containers.iter().any(|c| {
            running(c)
                && c.pointer("/labels/com.docker.compose.service")
                    .and_then(Value::as_str)
                    == Some(d.uuid.as_str())
        });
        if !is_running {
            continue;
        }
        let proxy_name = format!("{}-proxy", d.uuid);
        let proxy_running = named_running(containers, &proxy_name);
        if d.bool("is_public")
            && proxy_running
            && crate::databases::proxy::claim_outdated_refresh(
                &d.uuid,
                containers
                    .iter()
                    .find(|c| {
                        c.get("name").and_then(Value::as_str) == Some(proxy_name.as_str())
                            && running(c)
                    })
                    .and_then(|c| c.get("labels")),
            )
        {
            // A proxy that an older version started with an older configuration.
            jobs.spawn_database_proxy_start(d.clone());
        } else if d.bool("is_public") && !proxy_running {
            let name = d.str("name").unwrap_or_default().to_owned();
            jobs.spawn_database_proxy_start(d.clone());
            if let Some((team_id, server)) = lookup::server(srv.db(), srv.id()).await {
                notifications::notify(
                    srv.db(),
                    jobs.0.encrypter.as_ref(),
                    team_id,
                    Notification::ContainerRestarted {
                        name: format!("TCP Proxy for {name}"),
                        server,
                        url: None,
                        throttle: Some((d.kind.morph().to_owned(), d.id)),
                    },
                )
                .await;
            }
        } else if !d.bool("is_public") && proxy_running {
            jobs.spawn_database_proxy_stop(d);
        }
    }
}

/// `PushServerUpdateJob::handle`.
pub async fn push_server_update(jobs: &Jobs, server_id: i64, data: Value) -> Result<(), String> {
    let mut srv = Srv::load(jobs, server_id).await?;
    let Some(_guard) = jobs.try_overlap(&Overlap {
        key: format!("push-server-update-{}", srv.row.uuid),
        ttl: Duration::from_secs(LOCK_SECONDS),
    }) else {
        return Ok(());
    };
    if data.is_null() {
        return Err("No data provided".into());
    }
    let containers: Vec<Value> = match data.get("containers") {
        Some(Value::Array(list)) => list.clone(),
        Some(Value::Object(map)) => map.values().cloned().collect(),
        _ => Vec::new(),
    };
    check_disk_usage(jobs, &srv, &data).await;
    // `isCompleteSnapshot()`
    let complete = data.pointer("/snapshot/complete") != Some(&Value::Bool(false));
    if containers.is_empty() && !complete {
        return Ok(());
    }
    let inspected: Vec<Value> = containers.iter().filter_map(inspect_shape).collect();
    let snapshot = Snapshot {
        complete,
        query_may_have_failed: false,
    };
    let db = srv.db().clone();
    let applications = apps::list_for_server(&db, server_id)
        .await
        .map_err(|e| e.to_string())?;
    jobs.apply_application_containers(&applications, &inspected, snapshot, false)
        .await;
    let databases = dbs::list_for_server(&db, server_id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|d| d.kind.ported())
        .collect::<Vec<_>>();
    jobs.apply_database_containers(&databases, &inspected, snapshot)
        .await;
    if !complete {
        return Ok(());
    }
    update_proxy_status(jobs, &mut srv, &containers).await?;
    update_database_proxies(jobs, &srv, &containers).await;
    let services = svc::list_for_server(&db, server_id)
        .await
        .map_err(|e| e.to_string())?;
    jobs.apply_service_containers(&services, &inspected).await;
    // `checkLogDrainContainer()`
    let on = |k: &str| srv.info.setting_bool(k) == Some(true);
    if on("is_logdrain_newrelic_enabled")
        || on("is_logdrain_axiom_enabled")
        || on("is_logdrain_custom_enabled")
    {
        // `$this->logDrainContainerLabels` of the running, managed drain.
        let drain = containers.iter().find(|c| {
            c.get("name").and_then(Value::as_str) == Some("coolify-log-drain")
                && c.pointer("/labels/coolify.managed").is_some()
                && running(c)
        });
        let outdated = || {
            let version = drain.and_then(|c| {
                c.get("labels")
                    .and_then(|l| l.get(super::log_drain::CONFIG_VERSION_LABEL))
                    .and_then(Value::as_str)
            });
            super::log_drain::claim_outdated_refresh(
                srv.str_column("uuid").unwrap_or_default(),
                version,
            )
        };
        if drain.is_none() || outdated() {
            jobs.spawn_log_drain(server_id, true);
        }
    }
    Ok(())
}

impl Jobs {
    /// `PushServerUpdateJob::dispatch($server, $data)`.
    pub fn spawn_push_server_update(&self, server_id: i64, data: Value) {
        let jobs = self.clone();
        tokio::spawn(async move {
            if let Err(e) = push_server_update(&jobs, server_id, data).await {
                tracing::warn!(server_id, error = %e, "PushServerUpdateJob failed");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applications::status::container_status;

    #[test]
    fn a_container_of_the_push_reads_like_inspect() {
        let c = json!({"name": "web", "state": "running", "health_status": "healthy", "restart_count": 3,
            "labels": {"coolify.managed": "true", "coolify.applicationId": "1"}});
        let inspected = inspect_shape(&c).unwrap();
        assert_eq!(container_status(&inspected), "running:healthy");
        assert_eq!(inspected["RestartCount"], 3);
        assert_eq!(inspected["Config"]["Labels"]["coolify.applicationId"], "1");
        let exited =
            json!({"name": "web", "state": "exited", "labels": {"coolify.managed": "true"}});
        assert_eq!(container_status(&inspect_shape(&exited).unwrap()), "exited");
        let no_health =
            json!({"name": "web", "state": "running", "labels": {"coolify.managed": "true"}});
        assert_eq!(
            container_status(&inspect_shape(&no_health).unwrap()),
            "running:unknown"
        );
        assert!(inspect_shape(&json!({"name": "x", "state": "running", "labels": {}})).is_none());
    }

    #[test]
    fn a_database_is_found_by_its_compose_service() {
        let c = json!({"name": "db", "state": "running", "labels": {"coolify.managed": "true",
            "com.docker.compose.service": "abc"}});
        assert_eq!(
            inspect_shape(&c).unwrap()["Config"]["Labels"]["coolify.databaseUuid"],
            "abc"
        );
        let app = json!({"name": "a", "state": "running", "labels": {"coolify.managed": "true",
            "coolify.applicationId": "1", "com.docker.compose.service": "abc"}});
        assert!(
            inspect_shape(&app).unwrap()["Config"]["Labels"]
                .get("coolify.databaseUuid")
                .is_none()
        );
    }
}
