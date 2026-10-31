//! `ServerConnectionCheckJob`, `ServerStorageCheckJob`, `ServerCheckJob` (the
//! parts for the proxy, Sentinel and log drain), `CheckTraefikVersionForServerJob`
//! and the status checks `Server` exposes (`hasCurrentTraefikOutdatedInfo`,
//! `sentinelStatus`).

use coolify_db::server_ops as ops;
use coolify_db::servers::{self, ColumnValue, ServerRow};
use coolify_remote::RunOptions;
use serde_json::{Value, json};

use super::{Srv, proxy, sentinel, version_compare, versions_data};
use crate::Jobs;
use crate::checks as vchecks;
use crate::notifications::{self, Notification, TraefikServer, lookup};

/// `ServerConnectionCheckJob::UNREACHABLE_THRESHOLD`.
const UNREACHABLE_THRESHOLD: i64 = 2;
const SSH_ATTEMPTS: u32 = 2;
const SSH_TIMEOUT_SECONDS: u64 = 10;
const DOCKER_TIMEOUT_SECONDS: u64 = 15;
/// `ServerTransferClaimer::OWNER_MARKER_PATH`.
use crate::server_transfer::OWNER_MARKER_PATH;

/// `ServerConnectionCheckJob::handle`.
pub async fn server_connection_check(jobs: &Jobs, server_id: i64) -> Result<(), String> {
    let srv = Srv::load(jobs, server_id).await?;
    if srv.info.has_placeholder_ip() {
        return Ok(());
    }
    let lock = format!("server-connection-check-{}", srv.row.uuid);
    if !super::MARKS.add(lock.clone(), std::time::Duration::from_secs(90)) {
        return Ok(());
    }
    let r = connection_check(&srv).await;
    super::MARKS.forget(&lock);
    r
}

async fn connection_check(srv: &Srv) -> Result<(), String> {
    if srv.info.setting_bool("force_disabled") == Some(true) {
        servers::update_if_changed(
            srv.db(),
            servers::Row::Settings(srv.id()),
            &[
                ("is_reachable", ColumnValue::Bool(false)),
                ("is_usable", ColumnValue::Bool(false)),
            ],
            Jobs::now(),
        )
        .await
        .map_err(|e| e.to_string())?;
        return Ok(());
    }
    let target = srv.target().await?;
    let probe = format!("cat {OWNER_MARKER_PATH} 2>/dev/null || true; ls -la / > /dev/null");
    let mut reachable = false;
    let mut owner_marker = None;
    for attempt in 0..SSH_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        if let Ok(output) = srv
            .jobs
            .remote()
            .run(
                &target,
                &[probe.as_str()],
                RunOptions::op("server.connection_check")
                    .attempts(1)
                    .timeout(std::time::Duration::from_secs(SSH_TIMEOUT_SECONDS)),
            )
            .await
        {
            owner_marker = output;
            reachable = true;
            break;
        }
    }
    // `releasedToAnotherInstance`.
    if reachable
        && crate::server_transfer::release_if_claimed_elsewhere(
            &srv.jobs,
            srv,
            owner_marker.as_deref(),
        )
        .await?
    {
        return Ok(());
    }
    let unreachable_count = srv
        .row
        .record
        .get("unreachable_count")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let was_reachable = srv.info.setting_bool("is_reachable") == Some(true);
    let was_notified = lookup::unreachable_notification_sent(srv.db(), srv.id()).await;
    if !reachable {
        let count = unreachable_count + 1;
        srv.server_update(&[("unreachable_count", ColumnValue::Int(count))])
            .await?;
        if count < UNREACHABLE_THRESHOLD {
            return Ok(());
        }
        srv.settings_update(&[
            ("is_reachable", ColumnValue::Bool(false)),
            ("is_usable", ColumnValue::Bool(false)),
        ])
        .await?;
        // `dispatchReachabilityChangedIfNeeded(..., false)`
        if !was_notified {
            lookup::server_reachability_changed(
                srv.db(),
                srv.jobs.0.encrypter.as_ref(),
                srv.id(),
                UNREACHABLE_THRESHOLD,
            )
            .await;
        }
        return Ok(());
    }
    let target = &target;
    let docker = |command: &'static str| async move {
        srv.jobs
            .remote()
            .run(
                target,
                &[command],
                RunOptions::op("server.connection_check")
                    .attempts(1)
                    .timeout(std::time::Duration::from_secs(DOCKER_TIMEOUT_SECONDS)),
            )
            .await
            .ok()
            .flatten()
    };
    let mut usable = false;
    if let Some(output) = docker("docker version --format json").await {
        let output = output.trim().to_string();
        if !output.is_empty() {
            let version = vchecks::docker_engine_version_from_json(Some(&output));
            if let Some(v) = vchecks::parse_docker_engine_version(version.as_deref()) {
                srv.settings_update(&[
                    ("docker_version", ColumnValue::Text(Some(v))),
                    ("docker_version_checked_at", ColumnValue::Time(Jobs::now())),
                ])
                .await?;
            }
            let compose = docker("docker compose version --short").await;
            if let Some(v) = vchecks::parse_docker_engine_version(compose.as_deref()) {
                srv.settings_update(&[
                    ("compose_version", ColumnValue::Text(Some(v))),
                    ("compose_version_checked_at", ColumnValue::Time(Jobs::now())),
                ])
                .await?;
            }
            usable = serde_json::from_str::<Value>(&output)
                .ok()
                .is_some_and(|d| d.pointer("/Server/Version").is_some_and(|v| !v.is_null()));
        }
    }
    srv.settings_update(&[
        ("is_reachable", ColumnValue::Bool(true)),
        ("is_usable", ColumnValue::Bool(usable)),
    ])
    .await?;
    if unreachable_count > 0 {
        srv.server_update(&[("unreachable_count", ColumnValue::Int(0))])
            .await?;
    }
    // `dispatchReachabilityChangedIfNeeded(..., true)`
    if !was_reachable || was_notified {
        lookup::server_reachability_changed(
            srv.db(),
            srv.jobs.0.encrypter.as_ref(),
            srv.id(),
            UNREACHABLE_THRESHOLD,
        )
        .await;
    }
    Ok(())
}

/// `ServerStorageCheckJob::handle`.
pub async fn server_storage_check(jobs: &Jobs, server_id: i64) -> Result<Option<String>, String> {
    let srv = Srv::load(jobs, server_id).await?;
    if !srv.info.is_functional() {
        return Ok(None);
    }
    let usage = srv
        .run_quiet(
            &["df / --output=pcent | tr -cd 0-9".to_string()],
            "server.storage_check",
        )
        .await;
    let percentage = usage
        .as_deref()
        .and_then(|u| u.trim().parse::<i64>().ok())
        .unwrap_or(0);
    if percentage == 0 {
        return Ok(usage);
    }
    let threshold = srv
        .setting_int("server_disk_usage_notification_threshold")
        .unwrap_or(80);
    if percentage > threshold {
        if let Some((team_id, server)) = lookup::server(srv.db(), srv.id()).await {
            let interval_hours = srv
                .setting_int("server_disk_usage_notification_interval_hours")
                .unwrap_or(24);
            notifications::notify(
                srv.db(),
                jobs.0.encrypter.as_ref(),
                team_id,
                Notification::HighDiskUsage {
                    server,
                    disk_usage: percentage,
                    threshold,
                    interval_hours,
                },
            )
            .await;
        }
    } else if percentage <= threshold - 5 {
        // `HighDiskUsage::hasRecovered`: the next spike should alert again.
        notifications::release_high_disk_usage(srv.db(), srv.id()).await;
    }
    Ok(usage)
}

/// `ServerCheckJob::handle` for the proxy, Sentinel and log drain (container status
/// sync is `GetContainersStatus`, which lives with the resources).
pub async fn server_check(jobs: &Jobs, server_id: i64) -> Result<(), String> {
    let mut srv = Srv::load(jobs, server_id).await?;
    if !srv.info.is_functional() {
        return Ok(());
    }
    if srv.is_swarm_worker() || srv.info.is_build_server() {
        return Ok(());
    }
    let containers = srv
        .run(
            &["docker ps -a --no-trunc --format '{{json .}}'".to_string()],
            "server.check",
        )
        .await?
        .unwrap_or_default();
    let listed: Vec<Value> = containers
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let find = |name: &str| {
        listed
            .iter()
            .find(|c| c.get("Names").and_then(Value::as_str) == Some(name))
    };
    if sentinel::sentinel_enabled(&srv) {
        jobs.spawn_check_sentinel(server_id);
    }
    let on = |k: &str| srv.info.setting_bool(k) == Some(true);
    if on("is_logdrain_newrelic_enabled")
        || on("is_logdrain_highlight_enabled")
        || on("is_logdrain_axiom_enabled")
        || on("is_logdrain_custom_enabled")
    {
        let running = find("coolify-log-drain")
            .and_then(|c| c.get("State").and_then(Value::as_str))
            == Some("running");
        if !running {
            jobs.spawn_log_drain(server_id, true);
        }
    }
    if srv.proxy_is_set() && !srv.force_stop() && !srv.is_proxy_transition_in_progress() {
        let found = find("coolify-proxy").cloned();
        match found {
            None => {
                if proxy::check_proxy(&mut srv, false).await.unwrap_or(false)
                    && proxy::start_proxy(&mut srv, false, false, false)
                        .await
                        .is_ok()
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
                            throttle: Some(("App\\Models\\Server".to_owned(), server_id)),
                        },
                    )
                    .await;
                }
            }
            Some(c) => {
                let state = c.get("State").and_then(Value::as_str).unwrap_or_default();
                srv.proxy_set("status", json!(state));
                srv.save_proxy().await?;
            }
        }
    }
    Ok(())
}

/// `get_traefik_versions()`.
fn traefik_versions() -> Vec<(String, String)> {
    versions_data()
        .get("traefik")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// `getExactTraefikVersionFromContainer` then `getTraefikVersionFromDockerCompose`.
async fn detect_traefik_version(srv: &Srv) -> Option<String> {
    let exact = srv
        .run_quiet(
            &[r"docker exec coolify-proxy traefik version 2>/dev/null | grep -oP 'Version:\s+\K\d+\.\d+\.\d+'".to_string()],
            "proxy.traefik_version",
        )
        .await;
    if let Some(v) = exact.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        return Some(v.to_string());
    }
    let label = srv
        .run_quiet(
            &["docker inspect coolify-proxy --format '{{index .Config.Labels \"org.opencontainers.image.version\"}}' 2>/dev/null".to_string()],
            "proxy.traefik_version",
        )
        .await;
    if let Some(l) = label.as_deref().map(str::trim).filter(|l| !l.is_empty())
        && let Some(c) = regex::Regex::new(r"(\d+\.\d+\.\d+)").ok()?.captures(l)
    {
        return Some(c[1].to_string());
    }
    let image = srv
        .run_quiet(
            &["docker inspect coolify-proxy --format '{{.Config.Image}}' 2>/dev/null".to_string()],
            "proxy.traefik_version",
        )
        .await?;
    let image = image.trim();
    let c = regex::Regex::new(r"(?i)traefik:(v?\d+\.\d+(?:\.\d+)?|latest)")
        .ok()?
        .captures(image)?;
    Some(c[1].to_string())
}

/// `CheckTraefikVersionForServerJob::getNewerBranchInfo`.
fn newer_branch(versions: &[(String, String)], current: &str) -> Option<(String, String)> {
    let mut best: Option<(String, String)> = None;
    for (branch, version) in versions {
        let num = branch.trim_start_matches('v');
        if version_compare(num, current) == std::cmp::Ordering::Greater
            && best
                .as_ref()
                .is_none_or(|(_, v)| version_compare(version, v) == std::cmp::Ordering::Greater)
        {
            best = Some((num.to_string(), version.clone()));
        }
    }
    best.map(|(b, v)| (format!("v{b}"), v.trim_start_matches('v').to_string()))
}

/// `CheckTraefikVersionForServerJob::handle`.
pub async fn check_traefik_version(jobs: &Jobs, server_id: i64) -> Result<(), String> {
    let srv = Srv::load(jobs, server_id).await?;
    let db = srv.db();
    // `$this->previousOutdatedInfo`
    let previous = srv
        .row
        .record
        .get("traefik_outdated_info")
        .and_then(|v| match v {
            Value::String(s) => serde_json::from_str::<Value>(s).ok(),
            other => Some(other.clone()),
        })
        .unwrap_or(Value::Null);
    let clear_info = |value: Option<String>| async move {
        ops::save_json_column(
            db,
            server_id,
            "traefik_outdated_info",
            value.as_deref(),
            Jobs::now(),
        )
        .await
        .map_err(|e| e.to_string())
    };
    srv.server_update(&[("detected_traefik_version", ColumnValue::Text(None))])
        .await?;
    clear_info(None).await?;
    if srv.proxy_type() != Some("TRAEFIK")
        || srv.proxy.get("status").and_then(Value::as_str) != Some("running")
    {
        return Ok(());
    }
    let current_version = detect_traefik_version(&srv).await;
    srv.server_update(&[(
        "detected_traefik_version",
        ColumnValue::Text(current_version.clone()),
    )])
    .await?;
    let Some(current_version) = current_version else {
        return Ok(());
    };
    let image = srv
        .run_quiet(
            &["docker inspect coolify-proxy --format '{{.Config.Image}}' 2>/dev/null".to_string()],
            "proxy.traefik_version",
        )
        .await
        .unwrap_or_default();
    if image.trim().is_empty() || image.trim().to_lowercase().contains(":latest") {
        return Ok(());
    }
    let current = current_version.trim_start_matches('v').to_string();
    let Some(c) = regex::Regex::new(r"^(\d+\.\d+)\.(\d+)$")
        .map_err(|e| e.to_string())?
        .captures(&current)
    else {
        return Ok(());
    };
    let branch = c[1].to_string();
    let versions = traefik_versions();
    let latest_for_branch = versions
        .iter()
        .find(|(k, _)| *k == format!("v{branch}"))
        .map(|(_, v)| v.clone());
    let store = |latest: String, kind: &str, target: Option<String>| {
        let mut m = serde_json::Map::new();
        m.insert("current".into(), json!(current));
        m.insert("latest".into(), json!(latest));
        m.insert("type".into(), json!(kind));
        m.insert(
            "checked_at".into(),
            json!(
                chrono::Utc::now()
                    .format("%Y-%m-%dT%H:%M:%S+00:00")
                    .to_string()
            ),
        );
        if let Some(t) = target {
            m.insert("upgrade_target".into(), json!(t));
        }
        coolify_compat::php_json::encode(&Value::Object(m))
    };
    let newer = newer_branch(&versions, &branch);
    if let Some((target, latest)) = newer {
        clear_info(Some(store(
            latest.clone(),
            "minor_upgrade",
            Some(target.clone()),
        )))
        .await?;
        // `storeOutdatedInfo`: a repeated upgrade does not notify again, patch updates never do.
        let repeated = previous.get("type").and_then(Value::as_str) == Some("minor_upgrade")
            && previous.get("upgrade_target").and_then(Value::as_str) == Some(target.as_str());
        if !repeated && let Some((team_id, server)) = lookup::server(db, server_id).await {
            notifications::notify(
                db,
                jobs.0.encrypter.as_ref(),
                team_id,
                Notification::TraefikVersionOutdated {
                    servers: vec![TraefikServer {
                        name: server.name,
                        uuid: server.uuid,
                        current: Some(current.clone()),
                        latest: Some(latest),
                        update_type: Some("minor_upgrade".to_owned()),
                        upgrade_target: Some(target),
                        newer_branch_target: None,
                        newer_branch_latest: None,
                    }],
                },
            )
            .await;
        }
    } else if let Some(latest) = latest_for_branch {
        let latest = latest.trim_start_matches('v').to_string();
        if version_compare(&current, &latest) == std::cmp::Ordering::Less {
            clear_info(Some(store(latest, "patch_update", None))).await?;
        }
    }
    Ok(())
}

/// `Server::hasCurrentTraefikOutdatedInfo` on a loaded row.
pub fn has_current_traefik_outdated_info(row: &ServerRow) -> bool {
    let r = &row.record;
    if r.get("proxy")
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        != Some("TRAEFIK")
    {
        return false;
    }
    let detected = r
        .get("detected_traefik_version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_start_matches('v')
        .to_string();
    let info = r.get("traefik_outdated_info");
    let stored = info
        .and_then(|i| i.get("current"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_start_matches('v');
    let kind = info.and_then(|i| i.get("type")).and_then(Value::as_str);
    !detected.trim().is_empty()
        && stored == detected
        && matches!(kind, Some("patch_update" | "minor_upgrade"))
}

/// `Server::sentinelStatus` on a loaded row.
pub fn sentinel_status(row: &ServerRow) -> &'static str {
    let settings = row.settings.clone().unwrap_or_default();
    let interval = settings
        .get("sentinel_push_interval_seconds")
        .and_then(Value::as_i64)
        .unwrap_or(60);
    let parse = |v: Option<&Value>| {
        v.and_then(Value::as_str).and_then(|s| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
                .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S"))
                .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.fZ"))
                .ok()
        })
    };
    let now = Jobs::now();
    if let Some(w) = parse(row.record.get("sentinel_waiting_since")) {
        return if w > now - chrono::Duration::seconds((interval + 30).max(30)) {
            "waiting"
        } else {
            "out_of_sync"
        };
    }
    let wait = (interval * 3).max(120);
    if parse(row.record.get("sentinel_updated_at"))
        .is_some_and(|t| t > now - chrono::Duration::seconds(wait))
    {
        "in_sync"
    } else {
        "out_of_sync"
    }
}

impl Jobs {
    /// `RestartProxyJob::dispatch($server)`.
    pub fn spawn_restart_proxy(&self, server_id: i64) {
        self.spawn_server_job("restart-proxy", server_id, |jobs, id| async move {
            proxy::restart_proxy(&jobs, id).await
        });
    }
}
