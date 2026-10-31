//! `StartSentinel`, `StopSentinel`, `CheckAndStartSentinelJob`,
//! `ServerSetting::ensureValidSentinelToken` and `ensureSentinelUrl`.

use std::time::Duration;

use chrono::NaiveDateTime;
use coolify_db::server_ops as ops;
use coolify_db::servers::ColumnValue;
use serde_json::{Value, json};

use super::{BASE_CONFIG_DIR, MARKS, Srv, latest_sentinel_version, shell_arg, version_compare};
use crate::Jobs;

/// `CheckAndStartSentinelJob::STALL_RESTART_AFTER_SECONDS`.
const STALL_RESTART_AFTER_SECONDS: i64 = 600;
const STALL_RESTART_INTERVAL_SECONDS: u64 = 3600;

/// `StartSentinel::trafficLogDirectory`.
pub fn traffic_log_directory(srv: &Srv) -> String {
    srv.proxy_path().trim_end_matches('/').to_string()
}

fn traffic_analytics_enabled(srv: &Srv) -> bool {
    srv.info.setting_bool("is_traffic_analytics_enabled") == Some(true)
}

fn has_traffic_proxy(srv: &Srv) -> bool {
    matches!(srv.proxy_type(), Some("TRAEFIK" | "CADDY"))
}

/// `ServerSetting::isValidSentinelToken`.
pub fn is_valid_sentinel_token(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '=' | '/'))
}

/// `ensureValidSentinelToken`.
async fn ensure_valid_token(srv: &Srv) -> Result<String, String> {
    let enc = srv
        .jobs
        .0
        .encrypter
        .as_ref()
        .ok_or("No application encryption key has been specified.")?;
    let current = srv
        .info
        .settings
        .get("sentinel_token")
        .and_then(Value::as_str)
        .and_then(|c| enc.decrypt_string(c).ok());
    if let Some(t) = current.filter(|t| is_valid_sentinel_token(t)) {
        return Ok(t);
    }
    let payload = json!({"server_uuid": srv.row.uuid}).to_string();
    let token = enc.encrypt_serialized(&payload);
    srv.settings_update(&[(
        "sentinel_token",
        ColumnValue::Text(Some(enc.encrypt_string(&token))),
    )])
    .await?;
    Ok(token)
}

/// `ensureSentinelUrl`.
async fn ensure_url(srv: &Srv) -> Result<String, String> {
    let mut url = srv
        .info
        .settings
        .get("sentinel_custom_url")
        .and_then(Value::as_str)
        .filter(|u| !u.trim().is_empty())
        .map(str::to_string);
    if srv.is_localhost()
        && url.as_deref().is_some_and(|u| {
            u.to_lowercase()
                .split("://")
                .nth(1)
                .and_then(|r| r.split([':', '/']).next())
                == Some("host.docker.internal")
        })
    {
        url = None;
    }
    if let Some(u) = url {
        return Ok(u);
    }
    let generated = if srv.is_localhost() {
        Some("http://coolify:8080".to_string())
    } else {
        let s = ops::instance_server_settings(srv.db())
            .await
            .map_err(|e| e.to_string())?;
        let filled = |v: &Option<String>| v.clone().filter(|x| !x.trim().is_empty());
        filled(&s.fqdn)
            .or_else(|| filled(&s.public_ipv4).map(|ip| format!("http://{ip}:8000")))
            .or_else(|| filled(&s.public_ipv6).map(|ip| format!("http://[{ip}]:8000")))
    };
    let Some(url) = generated else {
        return Err(
            "Set an instance FQDN, public IP, or reachable Coolify URL before enabling Sentinel."
                .to_string(),
        );
    };
    srv.settings_update(&[("sentinel_custom_url", ColumnValue::Text(Some(url.clone())))])
        .await?;
    Ok(url)
}

fn setting_text(srv: &Srv, key: &str) -> String {
    match srv.info.settings.get(key) {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(true)) => "1".to_string(),
        _ => String::new(),
    }
}

/// `StartSentinel::sentinelTrafficEnvironment`.
fn traffic_environment(srv: &Srv) -> Vec<(String, String)> {
    if !(traffic_analytics_enabled(srv) && has_traffic_proxy(srv)) {
        return Vec::new();
    }
    let int_or = |k: &str, default: i64| {
        srv.setting_int(k)
            .filter(|v| *v != 0)
            .unwrap_or(default)
            .to_string()
    };
    let geoip = srv.info.setting_bool("is_geoip_enabled") == Some(true);
    let mut env = vec![
        ("TRAFFIC_ENABLED".to_string(), "true".to_string()),
        ("TRAFFIC_PROXY_TYPE".to_string(), "auto".to_string()),
        (
            "TRAFFIC_ACCESS_LOG_PATH".to_string(),
            format!("{}/access.log", traffic_log_directory(srv)),
        ),
        ("TRAFFIC_TOPN".to_string(), int_or("traffic_topn", 50)),
        (
            "TRAFFIC_SAMPLE_THRESHOLD".to_string(),
            srv.setting_int("traffic_sample_threshold")
                .unwrap_or(0)
                .to_string(),
        ),
        (
            "TRAFFIC_RETENTION_1H_DAYS".to_string(),
            int_or("traffic_retention_1h_days", 30),
        ),
        (
            "TRAFFIC_RETENTION_1D_DAYS".to_string(),
            int_or("traffic_retention_1d_days", 395),
        ),
        (
            "GEOIP_ENABLED".to_string(),
            if geoip { "true" } else { "false" }.to_string(),
        ),
        (
            "GEOIP_REFRESH_DAYS".to_string(),
            int_or("geoip_refresh_days", 30),
        ),
        (
            "TRAFFIC_IP_MODE".to_string(),
            srv.info
                .settings
                .get("traffic_ip_mode")
                .and_then(Value::as_str)
                .unwrap_or("full")
                .to_string(),
        ),
    ];
    if geoip
        && let Some(license) = srv
            .info
            .settings
            .get("geoip_maxmind_license_key")
            .and_then(Value::as_str)
            .and_then(|c| srv.jobs.0.encrypter.as_ref()?.decrypt_string(c).ok())
            .filter(|l| !l.trim().is_empty())
    {
        env.push(("GEOIP_MAXMIND_LICENSE_KEY".to_string(), license));
    }
    env
}

/// `StopSentinel::handle`.
pub async fn stop_sentinel(srv: &Srv) -> Result<(), String> {
    let _ = srv
        .run_quiet(
            &["docker rm -f coolify-sentinel".to_string()],
            "sentinel.stop",
        )
        .await;
    srv.sentinel_heartbeat(true).await
}

/// `StartSentinel::handle`.
pub async fn start_sentinel(
    jobs: &Jobs,
    server_id: i64,
    restart: bool,
    latest_version: Option<String>,
) -> Result<(), String> {
    let lock = format!("sentinel-start:{server_id}");
    // Two starts at the same time both remove the container; the second `docker run` would fail.
    let mut waited = 0;
    while !MARKS.add(lock.clone(), Duration::from_secs(300)) {
        if waited >= 90 {
            return Err("Could not acquire the Sentinel start lock.".to_string());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        waited += 1;
    }
    let result = start_inner(jobs, server_id, restart, latest_version).await;
    MARKS.forget(&lock);
    result
}

async fn start_inner(
    jobs: &Jobs,
    server_id: i64,
    restart: bool,
    latest_version: Option<String>,
) -> Result<(), String> {
    let srv = Srv::load(jobs, server_id).await?;
    if srv.info.is_swarm() || srv.info.is_build_server() {
        return Ok(());
    }
    if restart {
        stop_sentinel(&srv).await?;
    }
    let version = latest_version.unwrap_or_else(latest_sentinel_version);
    let token = ensure_valid_token(&srv).await?;
    let endpoint = ensure_url(&srv).await?;
    let debug = srv.info.setting_bool("is_sentinel_debug_enabled") == Some(true);
    let metrics = srv.info.setting_bool("is_metrics_enabled") == Some(true);
    let mount_dir = format!("{BASE_CONFIG_DIR}/sentinel");
    let settings = ops::instance_server_settings(srv.db())
        .await
        .map_err(|e| e.to_string())?;
    let registry = settings
        .docker_registry_url
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "docker.io".to_string());
    let image = format!("{registry}/coollabsio/sentinel:{version}");
    let mut environments: Vec<(String, String)> = vec![
        ("TOKEN".to_string(), token),
        (
            "DEBUG".to_string(),
            if debug { "true" } else { "false" }.to_string(),
        ),
        ("PUSH_ENDPOINT".to_string(), endpoint),
        (
            "PUSH_INTERVAL_SECONDS".to_string(),
            setting_text(&srv, "sentinel_push_interval_seconds"),
        ),
        (
            "COLLECTOR_ENABLED".to_string(),
            if metrics { "true" } else { "false" }.to_string(),
        ),
        (
            "COLLECTOR_REFRESH_RATE_SECONDS".to_string(),
            setting_text(&srv, "sentinel_metrics_refresh_rate_seconds"),
        ),
        (
            "COLLECTOR_RETENTION_PERIOD_DAYS".to_string(),
            setting_text(&srv, "sentinel_metrics_history_days"),
        ),
    ];
    environments.extend(traffic_environment(&srv));
    let docker_env = environments
        .iter()
        .map(|(k, v)| format!("-e {}", shell_arg(&format!("{k}={v}"))))
        .collect::<Vec<_>>()
        .join(" ");
    let traffic_dir = traffic_log_directory(&srv);
    let collects = traffic_analytics_enabled(&srv) && has_traffic_proxy(&srv);
    let traffic_mount = if collects {
        format!(
            "-v {} ",
            shell_arg(&format!("{traffic_dir}:{traffic_dir}:ro"))
        )
    } else {
        String::new()
    };
    let network = if srv.is_localhost() {
        " --network coolify"
    } else {
        ""
    };
    let docker_command = format!(
        "docker run -d{network} {docker_env} --name coolify-sentinel --restart unless-stopped -v /var/run/docker.sock:/var/run/docker.sock -v {mount_dir}:/app/db {traffic_mount}--userns host --pid host --health-cmd \"curl --fail http://127.0.0.1:8888/api/health || exit 1\" --health-start-period 120s --health-interval 10s --health-retries 3 --add-host=host.docker.internal:host-gateway --label coolify.managed=true {image}"
    );
    srv.sentinel_heartbeat(true).await?;
    ops::save_sentinel_waiting(srv.db(), server_id, Some(Jobs::now()), Jobs::now())
        .await
        .map_err(|e| e.to_string())?;
    let mut commands = vec![
        "docker rm -f coolify-sentinel || true".to_string(),
        format!("mkdir -p {mount_dir}"),
    ];
    // `trafficLogPreparationCommands`.
    if traffic_analytics_enabled(&srv)
        && !srv.info.is_swarm()
        && !srv.info.is_build_server()
        && has_traffic_proxy(&srv)
    {
        commands.push(format!("mkdir -p {}", shell_arg(&traffic_dir)));
        commands.push(format!(
            "touch {}",
            shell_arg(&format!("{traffic_dir}/access.log"))
        ));
    }
    commands.push(docker_command);
    commands.push(format!("chown -R 9999:root {mount_dir}"));
    commands.push(format!("chmod -R 700 {mount_dir}"));
    srv.run(&commands, "sentinel.start").await?;
    srv.settings_update(&[("is_sentinel_enabled", ColumnValue::Bool(true))])
        .await?;
    let fresh = Srv::load(jobs, server_id).await?;
    if fresh
        .row
        .record
        .get("sentinel_waiting_since")
        .is_some_and(|v| !v.is_null())
    {
        ops::save_sentinel_waiting(fresh.db(), server_id, Some(Jobs::now()), Jobs::now())
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// `Server::isSentinelEnabled()`.
pub fn sentinel_enabled(srv: &Srv) -> bool {
    !srv.info.is_build_server()
        && !srv.info.is_swarm()
        && srv.info.setting_bool("force_disabled") != Some(true)
        && !srv.info.transferred
}

fn parse_time(v: Option<&Value>) -> Option<NaiveDateTime> {
    let s = v?.as_str()?;
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S"))
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.fZ"))
        .ok()
}

/// `Server::waitBeforeDoingSshCheck`.
pub fn wait_before_ssh_check(srv: &Srv) -> i64 {
    let wait = srv
        .setting_int("sentinel_push_interval_seconds")
        .unwrap_or(60)
        * 3;
    wait.max(120)
}

/// `Server::isSentinelLive`.
pub fn sentinel_live(srv: &Srv) -> bool {
    parse_time(srv.row.record.get("sentinel_updated_at"))
        .is_some_and(|t| t > Jobs::now() - chrono::Duration::seconds(wait_before_ssh_check(srv)))
}

/// `rememberSentinelPushProblem`.
/// `$server->sentinelPushProblem()`.
pub fn push_problem(server_id: i64) -> Option<String> {
    MARKS.get(&format!("sentinel:push-problem:{server_id}"))
}

pub fn remember_push_problem(server_id: i64, problem: Option<&str>) {
    let key = format!("sentinel:push-problem:{server_id}");
    match problem.filter(|p| !p.trim().is_empty()) {
        None => MARKS.forget(&key),
        Some(p) => {
            let limited: String = if p.chars().count() > 500 {
                format!("{}...", p.chars().take(500).collect::<String>())
            } else {
                p.to_string()
            };
            MARKS.put(key, limited, Duration::from_secs(86_400));
        }
    }
}

/// `CheckAndStartSentinelJob::handle`.
pub async fn check_and_start_sentinel(jobs: &Jobs, server_id: i64) -> Result<(), String> {
    let srv = Srv::load(jobs, server_id).await?;
    if !sentinel_enabled(&srv) || !srv.info.is_functional() {
        return Ok(());
    }
    let latest = latest_sentinel_version();
    let status = srv
        .run(
            &[
                "docker ps -a --filter 'name=^coolify-sentinel$' --format '{{.State}} {{.Status}}'"
                    .to_string(),
            ],
            "sentinel.status",
        )
        .await?
        .unwrap_or_default();
    if !status.starts_with("running") {
        return start_sentinel(jobs, server_id, true, Some(latest)).await;
    }
    if status.contains("(unhealthy)") {
        remember_push_problem(
            server_id,
            Some("The Sentinel container was unhealthy. Coolify restarted it."),
        );
        return start_sentinel(jobs, server_id, true, Some(latest)).await;
    }
    let output = srv
        .run_quiet(
            &["docker exec coolify-sentinel sh -c 'curl -s http://127.0.0.1:8888/api/version; echo; curl -sf -H \"Authorization: Bearer $TOKEN\" http://127.0.0.1:8888/api/push-status'".to_string()],
            "sentinel.version",
        )
        .await
        .unwrap_or_default();
    let trimmed = coolify_remote::text::php_trim(&output);
    let mut lines = trimmed.splitn(2, '\n');
    let running = lines.next().map(str::trim).unwrap_or_default().to_string();
    if let Some(status) = lines
        .next()
        .and_then(|p| serde_json::from_str::<Value>(p.trim()).ok())
        && status
            .get("consecutive_failures")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            >= 1
        && let Some(err) = status
            .get("last_error")
            .and_then(Value::as_str)
            .filter(|e| !e.trim().is_empty())
    {
        remember_push_problem(
            server_id,
            Some(&format!("Sentinel cannot push to Coolify: {err}")),
        );
    }
    if !running.is_empty() && version_compare(&running, &latest) == std::cmp::Ordering::Less {
        return start_sentinel(jobs, server_id, true, Some(latest)).await;
    }
    let last = parse_time(srv.row.record.get("sentinel_waiting_since"))
        .or_else(|| parse_time(srv.row.record.get("sentinel_updated_at")));
    let stall_after = STALL_RESTART_AFTER_SECONDS.max(wait_before_ssh_check(&srv));
    let stalled = last.is_none_or(|t| t < Jobs::now() - chrono::Duration::seconds(stall_after));
    if stalled
        && MARKS.add(
            format!("sentinel:stall-restart:{server_id}"),
            Duration::from_secs(STALL_RESTART_INTERVAL_SECONDS),
        )
    {
        return start_sentinel(jobs, server_id, true, Some(latest)).await;
    }
    Ok(())
}

impl Jobs {
    /// `$server->restartSentinel()`: `StartSentinel::dispatch($server, true)`.
    pub fn spawn_restart_sentinel(&self, server_id: i64) {
        self.spawn_server_job("start-sentinel", server_id, |jobs, id| async move {
            start_sentinel(&jobs, id, true, None).await
        });
    }

    /// `CheckAndStartSentinelJob::dispatch($server)`.
    pub fn spawn_check_sentinel(&self, server_id: i64) {
        self.spawn_server_job("check-sentinel", server_id, |jobs, id| async move {
            check_and_start_sentinel(&jobs, id).await
        });
    }
}
