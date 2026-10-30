//! `StartProxy`, `StopProxy`, `RestartProxyJob`, `CheckProxy`,
//! `ProxyStatusChangedNotification` and the proxy methods of `Server`
//! (`setupDefaultRedirect`, `setupDynamicProxyConfiguration`, `changeProxy`, ...).

use base64::Engine as _;
use coolify_compat::yaml::{self, PhpValue};
use coolify_db::destinations;
use coolify_db::server_ops as ops;
use coolify_db::servers::ColumnValue;
use coolify_remote::{RunOptions, Stream};
use serde_json::{Value, json};

use super::proxy_config::{
    self as config, SaveError, TRAEFIK_LOGROTATE_CONTAINER, get_proxy_configuration,
    save_proxy_configuration,
};
use super::{BASE_CONFIG_DIR, Srv, shell_arg};
use crate::Jobs;
use crate::databases::common::{docker_stop_command, server_docker_config_prefix};

/// `Server::PROXY_ERROR_PAGE_CONTAINER`.
pub const PROXY_ERROR_PAGE_CONTAINER: &str = "coolify-proxy-error-pages";
/// `Server::PROXY_ERROR_PAGE_IMAGE`.
const PROXY_ERROR_PAGE_IMAGE: &str = "busybox:musl";
/// `Server::PROXY_ERROR_PAGE_MAX_BYTES`.
pub const PROXY_ERROR_PAGE_MAX_BYTES: usize = 1_048_576;
/// `view('proxy.error-page')->render()`.
const DEFAULT_ERROR_PAGE: &str = include_str!("error-page.html");

fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s)
}

/// `dockerNetworkEnsureCommand`.
pub fn docker_network_ensure_command(network: &str, overlay: bool, quiet_create: bool) -> String {
    let safe = shell_arg(network);
    let flags = if overlay {
        "--driver overlay --attachable"
    } else {
        "--attachable"
    };
    let quiet = if quiet_create { " >/dev/null" } else { "" };
    format!(
        "docker network inspect {safe} >/dev/null 2>&1 || docker network create {flags} {safe}{quiet}"
    )
}

/// `dockerCommandIgnoringError`.
fn docker_command_ignoring_error(command: &str, ignored: &str) -> String {
    let script = format!(
        "output=$({command} 2>&1); exit_code=$?; if [ \"$exit_code\" -ne 0 ] && printf '%s' \"$output\" | grep -Eq {}; then exit 0; fi; if [ \"$exit_code\" -ne 0 ]; then printf '%s\\n' \"$output\" >&2; else printf '%s\\n' \"$output\"; fi; exit $exit_code",
        shell_arg(ignored)
    );
    format!("bash -c {}", shell_arg(&script))
}

/// `dockerNetworkRemoveCommand`.
pub fn docker_network_remove_command(network: &str) -> String {
    docker_command_ignoring_error(
        &format!("docker network rm {}", shell_arg(network)),
        "network .* not found",
    )
}

/// `dockerRemoveCommand`.
pub fn docker_remove_command(container: &str) -> String {
    docker_command_ignoring_error(
        &format!("docker rm -f {}", shell_arg(container)),
        "No such container:",
    )
}

/// `getContainerStatus($server, $container)`.
pub async fn container_status(srv: &Srv, container: &str) -> String {
    let swarm = srv.info.is_swarm();
    let command = if swarm {
        format!(
            "docker service ls --filter {} --format '{{{{json .}}}}' ",
            shell_arg(&format!("name={container}"))
        )
    } else {
        format!(
            "docker inspect --format '{{{{json .}}}}' {}",
            shell_arg(container)
        )
    };
    let Some(output) = srv.run_quiet(&[command], "proxy.container_status").await else {
        return "exited".into();
    };
    let first: Option<Value> = output
        .lines()
        .find(|l| !l.is_empty())
        .and_then(|l| serde_json::from_str(l).ok());
    let Some(first) = first else {
        return "exited".into();
    };
    if swarm {
        let replicas = first.get("Replicas").and_then(Value::as_str).unwrap_or("");
        let mut parts = replicas.split('/');
        let active: i64 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        let total: i64 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        if active == total {
            "running".into()
        } else {
            "starting".into()
        }
    } else {
        first
            .pointer("/State/Status")
            .and_then(Value::as_str)
            .unwrap_or("exited")
            .to_string()
    }
}

/// `collectDockerNetworksByServer()['allNetworks']`.
async fn all_networks(srv: &Srv) -> Result<Vec<String>, String> {
    let mut all = srv.destination_networks().await?;
    // Service and Compose application networks.
    let services = ops::services_on_server(srv.db(), srv.id())
        .await
        .map_err(|e| e.to_string())?;
    for s in &services {
        if let Some(uuid) = s.get("uuid").and_then(Value::as_str) {
            all.push(uuid.to_string());
        }
    }
    let apps = ops::applications_for_cleanup(srv.db(), srv.id())
        .await
        .map_err(|e| e.to_string())?;
    for (_, uuid, _, _, build_pack) in &apps {
        if build_pack.as_deref() == Some("dockercompose") {
            all.push(uuid.clone());
        }
    }
    let valid = |n: &String| {
        !n.is_empty()
            && n != "default"
            && n != "host"
            && n.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
            && n.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    let mut out: Vec<String> = Vec::new();
    for n in all.into_iter().filter(valid) {
        if !out.contains(&n) {
            out.push(n);
        }
    }
    if out.is_empty() {
        out.push(
            if srv.info.is_swarm() {
                "coolify-overlay"
            } else {
                "coolify"
            }
            .to_string(),
        );
    }
    Ok(out)
}

/// `ensureProxyNetworksExist`.
async fn ensure_proxy_networks_exist(srv: &Srv) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for n in all_networks(srv).await? {
        let safe = shell_arg(&n);
        out.push(format!("echo 'Ensuring network {safe} exists...'"));
        out.push(docker_network_ensure_command(
            &n,
            srv.info.is_swarm(),
            false,
        ));
    }
    Ok(out)
}

/// `ensureProxyNetworksExist` for callers outside this module.
pub async fn ensure_proxy_networks_exist_public(srv: &Srv) -> Result<Vec<String>, String> {
    ensure_proxy_networks_exist(srv).await
}

/// `connectProxyToNetworks`.
async fn connect_proxy_to_networks(srv: &Srv) -> Result<Vec<String>, String> {
    if srv.info.is_swarm() {
        let mut out = Vec::new();
        for n in all_networks(srv).await? {
            let safe = shell_arg(&n);
            out.push(docker_network_ensure_command(&n, true, true));
            out.push(format!(
                "docker network connect {safe} coolify-proxy >/dev/null 2>&1 || true"
            ));
            out.push(format!(
                "echo 'Successfully connected coolify-proxy to {safe} network.'"
            ));
        }
        return Ok(out);
    }
    Ok([
        r#"for network in $(docker inspect $(docker ps -a --filter label=coolify.managed=true --format "{{.ID}}") --format='{{range $network, $_ := .NetworkSettings.Networks}}{{println $network}}{{end}}' 2>/dev/null | sort -u); do"#,
        r#"    if [ -z "$network" ] || [ "$network" = "bridge" ] || [ "$network" = "host" ] || [ "$network" = "none" ] || [ "$network" = "default" ]; then"#,
        "        continue",
        "    fi",
        r#"    if docker network inspect "$network" >/dev/null 2>&1; then"#,
        r#"        docker network connect "$network" coolify-proxy >/dev/null 2>&1 || true"#,
        "    fi",
        "done",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect())
}

/// `Server::caddyBaseCaddyfile()` without maintenance resources (maintenance mode is not ported).
fn caddy_base_caddyfile() -> String {
    "import /dynamic/*.caddy\n".to_string()
}

/// `StartProxy::startCommands`.
pub async fn start_commands(srv: &Srv) -> Result<Vec<String>, String> {
    let proxy_path = srv.proxy_path();
    let compose_file = format!("{}/docker-compose.yml", proxy_path.trim_end_matches('/'));
    let mut c: Vec<String> = Vec::new();
    if srv.is_swarm_manager() {
        c.extend([
            format!("mkdir -p {proxy_path}/dynamic"),
            "echo 'Creating required Docker Compose file.'".to_string(),
            "echo 'Starting coolify-proxy.'".to_string(),
            format!("docker stack deploy --detach=true -c {compose_file} coolify-proxy"),
            "echo 'Successfully started coolify-proxy.'".to_string(),
        ]);
        return Ok(c);
    }
    let caddyfile = b64(&caddy_base_caddyfile());
    c.extend([
        format!("mkdir -p {proxy_path}/dynamic"),
        format!("echo '{caddyfile}' | base64 -d | tee {proxy_path}/dynamic/Caddyfile > /dev/null"),
        "echo 'Creating required Docker Compose file.'".to_string(),
        "echo 'Pulling docker image.'".to_string(),
        format!(
            "{}docker compose -f {compose_file} pull",
            server_docker_config_prefix(&srv.info)
        ),
        r#"if docker ps -a --format "{{.Names}}" | grep -q "^coolify-proxy$"; then"#.to_string(),
        "    echo 'Stopping and removing existing coolify-proxy.'".to_string(),
        "    docker stop coolify-proxy 2>/dev/null || true".to_string(),
        "    docker rm -f coolify-proxy 2>/dev/null || true".to_string(),
        "    # Wait for container to be fully removed".to_string(),
        "    for i in {1..10}; do".to_string(),
        r#"        if ! docker ps -a --format "{{.Names}}" | grep -q "^coolify-proxy$"; then"#
            .to_string(),
        "            break".to_string(),
        "        fi".to_string(),
        r#"        echo "Waiting for coolify-proxy to be removed... ($i/10)""#.to_string(),
        "        sleep 1".to_string(),
        "    done".to_string(),
        "    echo 'Successfully stopped and removed existing coolify-proxy.'".to_string(),
        "fi".to_string(),
    ]);
    if srv.proxy_type() != Some("TRAEFIK") {
        c.push(format!(
            "docker rm -f {TRAEFIK_LOGROTATE_CONTAINER} 2>/dev/null || true"
        ));
    }
    c.extend(ensure_proxy_networks_exist(srv).await?);
    c.extend([
        "echo 'Starting coolify-proxy.'".to_string(),
        format!("docker compose -f {compose_file} up -d --wait --remove-orphans"),
        "echo 'Successfully started coolify-proxy.'".to_string(),
    ]);
    c.extend(connect_proxy_to_networks(srv).await?);
    Ok(c)
}

/// `markProxyConfigurationApplied`.
async fn mark_configuration_applied(srv: &mut Srv, configuration: &str) -> Result<(), String> {
    srv.proxy_set(
        "last_applied_settings",
        json!(config::config_hash(configuration)),
    );
    srv.proxy_set("certificates_restart_required", json!(false));
    srv.save_proxy().await
}

/// `remote_process(...)`: runs the commands as an `activity_log` entry and streams the output.
async fn remote_process(srv: &Srv, commands: &[String], op: &'static str) -> Result<(), String> {
    let db = srv.db().clone();
    let props = json!({
        "server_uuid": srv.row.uuid,
        "type": "inline",
        "status": "queued",
        "team_id": srv.info.team_id,
        "operation": op,
    });
    let activity: i64 =
        coolify_db::server_ops::insert_server_activity(&db, srv.id(), &props, Jobs::now())
            .await
            .map_err(|e| e.to_string())?;
    let target = srv.target().await?;
    let _ = coolify_db::databases::update_activity(
        &db,
        activity,
        &json!({"status": "in_progress"}),
        None,
        Jobs::now(),
    )
    .await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Stream, String)>();
    let writer_db = db.clone();
    let writer = tokio::spawn(async move {
        let mut entries: Vec<Value> = Vec::new();
        while let Some((stream, chunk)) = rx.recv().await {
            let output = coolify_compat::redact::redact_text(&chunk, &[], &[]);
            if output.is_empty() {
                continue;
            }
            entries.push(json!({"type": stream.as_str(), "output": output, "batch": 1, "order": entries.len() + 1}));
            let _ = coolify_db::databases::update_activity(
                &writer_db,
                activity,
                &json!({}),
                Some(&serde_json::to_string(&entries).unwrap_or_default()),
                Jobs::now(),
            )
            .await;
        }
    });
    let result = srv
        .jobs
        .remote()
        .run_streaming(&target, commands, RunOptions::op(op), tx)
        .await;
    let _ = writer.await;
    let merge = match &result {
        Ok(_) => json!({"status": "finished", "exitCode": 0}),
        Err(e) => json!({"status": "error", "exitCode": e.code.unwrap_or(1), "error": e.message}),
    };
    let _ = coolify_db::databases::update_activity(&db, activity, &merge, None, Jobs::now()).await;
    result.map(|_| ()).map_err(|e| e.message)
}

/// `StartProxy::handle`. With `run_async` the commands run after this returns,
/// followed by the `ProxyStatusChanged` handling.
pub async fn start_proxy(
    srv: &mut Srv,
    run_async: bool,
    force: bool,
    restarting: bool,
) -> Result<(), String> {
    let _ = restarting;
    let proxy_type = srv.proxy_type().map(str::to_string);
    if (proxy_type.is_none()
        || proxy_type.as_deref() == Some("NONE")
        || srv.force_stop()
        || srv.info.is_build_server())
        && !force
    {
        return Ok(());
    }
    let configuration = get_proxy_configuration(srv, false).await?;
    if configuration.is_empty() {
        return Err("Configuration is not synced".to_string());
    }
    config::ports_from_configuration(&configuration).map_err(|e| e.0)?;
    srv.proxy_set("status", json!("starting"));
    srv.save_proxy().await?;
    srv.refresh().await?;
    save_proxy_configuration(srv, &configuration)
        .await
        .map_err(|e| e.to_string())?;
    mark_configuration_applied(srv, &configuration).await?;
    let commands = start_commands(srv).await?;
    if run_async {
        srv.mark_proxy_transition_started();
        let id = srv.id();
        let jobs = srv.jobs.clone();
        tokio::spawn(async move {
            let Ok(s) = Srv::load(&jobs, id).await else {
                return;
            };
            if let Err(e) = remote_process(&s, &commands, "proxy.start").await {
                tracing::warn!(server_id = id, error = %e, "proxy start failed");
            }
            let _ = proxy_status_changed(&jobs, id).await;
        });
    } else {
        srv.run(&commands, "proxy.start").await?;
        if let Some(t) = &proxy_type {
            srv.proxy_set("type", json!(t));
            srv.save_proxy().await?;
        }
        proxy_status_changed(&srv.jobs.clone(), srv.id()).await?;
        srv.refresh().await?;
    }
    Ok(())
}

/// `StopProxy::handle`.
pub async fn stop_proxy(
    srv: &mut Srv,
    force_stop: bool,
    timeout: u32,
    restarting: bool,
) -> Result<(), String> {
    let container = if srv.info.is_swarm() {
        "coolify-proxy_traefik"
    } else {
        "coolify-proxy"
    };
    srv.proxy_set("status", json!("stopping"));
    srv.save_proxy().await?;
    let mut commands = vec![
        format!(
            "{} 2>/dev/null || true",
            docker_stop_command(timeout, container, srv.info.docker_version())
        ),
        format!("docker rm -f {container} 2>/dev/null || true"),
        format!("docker rm -f {TRAEFIK_LOGROTATE_CONTAINER} 2>/dev/null || true"),
    ];
    if !srv.info.is_swarm() {
        commands.push(format!(
            "docker rm -f {PROXY_ERROR_PAGE_CONTAINER} 2>/dev/null || true"
        ));
    }
    commands.extend([
        "# Wait for container to be fully removed".to_string(),
        "for i in {1..10}; do".to_string(),
        format!(
            "    if ! docker ps -a --format \"{{{{.Names}}}}\" | grep -q \"^{container}$\"; then"
        ),
        "        break".to_string(),
        "    fi".to_string(),
        "    sleep 1".to_string(),
        "done".to_string(),
    ]);
    let _ = srv.run_quiet(&commands, "proxy.stop").await;
    srv.proxy_set("force_stop", json!(force_stop));
    srv.proxy_set("status", json!("exited"));
    srv.save_proxy().await?;
    if !restarting {
        proxy_status_changed(&srv.jobs.clone(), srv.id()).await?;
    }
    Ok(())
}

/// `RestartProxyJob::handle` and `buildRestartCommands`.
pub async fn restart_proxy(jobs: &Jobs, server_id: i64) -> Result<(), String> {
    let mut srv = Srv::load(jobs, server_id).await?;
    let lock = format!("restart-proxy-{}", srv.row.uuid);
    if !super::MARKS.add(lock.clone(), std::time::Duration::from_secs(120)) {
        return Ok(());
    }
    let result = restart_inner(&mut srv).await;
    if let Err(e) = &result {
        srv.mark_proxy_transition_finished();
        // An invalid configuration leaves the status as it is (`InvalidArgumentException`).
        if !matches!(e, RestartError::InvalidPorts(_))
            && let Ok(mut s) = Srv::load(jobs, server_id).await
        {
            s.proxy_set("status", json!("error"));
            let _ = s.save_proxy().await;
        }
    }
    super::MARKS.forget(&lock);
    result.map_err(|e| match e {
        RestartError::InvalidPorts(m) | RestartError::Other(m) => m,
    })
}

#[derive(Debug)]
enum RestartError {
    InvalidPorts(String),
    Other(String),
}

impl From<String> for RestartError {
    fn from(e: String) -> Self {
        Self::Other(e)
    }
}

async fn restart_inner(srv: &mut Srv) -> Result<(), RestartError> {
    let configuration = get_proxy_configuration(srv, false).await?;
    if configuration.is_empty() {
        return Err(RestartError::Other(
            "Configuration is not synced".to_string(),
        ));
    }
    config::ports_from_configuration(&configuration)
        .map_err(|e| RestartError::InvalidPorts(e.0))?;
    srv.mark_proxy_transition_started();
    srv.proxy_set("status", json!("restarting"));
    srv.proxy_set("force_stop", json!(false));
    srv.save_proxy().await?;
    let container = if srv.info.is_swarm() {
        "coolify-proxy_traefik"
    } else {
        "coolify-proxy"
    };
    save_proxy_configuration(srv, &configuration)
        .await
        .map_err(|e: SaveError| RestartError::Other(e.to_string()))?;
    mark_configuration_applied(srv, &configuration).await?;
    let mut commands = vec![
        "echo 'Stopping proxy...'".to_string(),
        format!(
            "{} 2>/dev/null || true",
            docker_stop_command(30, container, srv.info.docker_version())
        ),
        format!("docker rm -f {container} 2>/dev/null || true"),
        "# Wait for container to be fully removed".to_string(),
        "for i in {1..15}; do".to_string(),
        format!(
            "    if ! docker ps -a --format \"{{{{.Names}}}}\" | grep -q \"^{container}$\"; then"
        ),
        "        echo 'Container removed successfully.'".to_string(),
        "        break".to_string(),
        "    fi".to_string(),
        "    echo \"Waiting for container to be removed... ($i/15)\"".to_string(),
        "    sleep 1".to_string(),
        "    # Force remove on each iteration in case it got stuck".to_string(),
        format!("    docker rm -f {container} 2>/dev/null || true"),
        "done".to_string(),
        "# Final verification and force cleanup".to_string(),
        format!("if docker ps -a --format \"{{{{.Names}}}}\" | grep -q \"^{container}$\"; then"),
        "    echo 'Container still exists after wait, forcing removal...'".to_string(),
        format!("    docker rm -f {container} 2>/dev/null || true"),
        "    sleep 2".to_string(),
        "fi".to_string(),
        "echo 'Proxy stopped successfully.'".to_string(),
    ];
    commands.extend(start_commands(srv).await?);
    let id = srv.id();
    let jobs = srv.jobs.clone();
    tokio::spawn(async move {
        let Ok(s) = Srv::load(&jobs, id).await else {
            return;
        };
        if let Err(e) = remote_process(&s, &commands, "proxy.restart").await {
            tracing::warn!(server_id = id, error = %e, "proxy restart failed");
        }
        let _ = proxy_status_changed(&jobs, id).await;
    });
    Ok(())
}

/// `ProxyStatusChangedNotification::handle`.
pub async fn proxy_status_changed(jobs: &Jobs, server_id: i64) -> Result<(), String> {
    let mut srv = Srv::load(jobs, server_id).await?;
    srv.mark_proxy_transition_finished();
    let status = container_status(&srv, "coolify-proxy").await;
    srv.proxy_set("status", json!(status));
    srv.save_proxy().await?;
    if status == "running" {
        setup_default_redirect(&srv).await?;
        setup_maintenance_pages(&srv).await?;
        setup_dynamic_proxy_configuration(&srv).await?;
        srv.proxy_set("force_stop", json!(false));
        srv.save_proxy().await?;
        if srv.proxy_type() == Some("TRAEFIK") {
            let j = jobs.clone();
            tokio::spawn(async move {
                if let Err(e) = super::checks::check_traefik_version(&j, server_id).await {
                    tracing::warn!(server_id, error = %e, "traefik version check failed");
                }
            });
        }
    }
    if status == "created" {
        srv.run(
            &[docker_remove_command("coolify-proxy")],
            "proxy.remove_created",
        )
        .await?;
    }
    Ok(())
}

/// `Server::usesProxyErrorPage`.
fn uses_proxy_error_page(srv: &Srv) -> bool {
    !srv.info.is_swarm()
        && matches!(srv.proxy_type(), Some("TRAEFIK" | "CADDY"))
        && srv.proxy.get("redirect_enabled") != Some(&Value::Bool(false))
        && srv
            .proxy
            .get("redirect_url")
            .and_then(Value::as_str)
            .is_none_or(|u| u.trim().is_empty())
}

/// `Server::uploadProxyFile`. Upstream copies with scp; here the content goes
/// through the script in pieces, with the same result on the server.
async fn upload_proxy_file(srv: &Srv, path: &str, contents: &str) -> Result<(), String> {
    let mut upload = String::from("/tmp/coolify-upload-");
    for _ in 0..16 {
        upload.push(char::from_digit(rand::random::<u32>() % 16, 16).unwrap_or('0'));
    }
    let encoded = b64(contents);
    let mut commands = vec![format!(": > {upload}")];
    for piece in encoded.as_bytes().chunks(65_536) {
        commands.push(format!(
            "echo '{}' | base64 -d >> {upload}",
            String::from_utf8_lossy(piece)
        ));
    }
    commands.push(format!("mv -f {upload} {path}"));
    commands.push(format!("chmod 644 {path}"));
    srv.run(&commands, "proxy.upload_file").await.map(|_| ())
}

/// `Server::setupProxyErrorPageContainer`.
async fn setup_proxy_error_page_container(srv: &Srv) -> Result<(), String> {
    if srv.info.is_swarm() {
        return Ok(());
    }
    let container = PROXY_ERROR_PAGE_CONTAINER;
    if !uses_proxy_error_page(srv) {
        srv.run(
            &[format!("docker rm -f {container} >/dev/null 2>&1 || true")],
            "proxy.error_page",
        )
        .await?;
        return Ok(());
    }
    let path = format!("{BASE_CONFIG_DIR}/proxy/error-pages");
    srv.run(&[format!("mkdir -p {path}")], "proxy.error_page")
        .await?;
    let html = srv
        .str_column("proxy_error_page")
        .filter(|p| !p.trim().is_empty())
        .unwrap_or(DEFAULT_ERROR_PAGE);
    upload_proxy_file(srv, &format!("{path}/503.html"), html).await?;
    srv.run(
        &[
            docker_network_ensure_command("coolify", false, false),
            format!(
                "docker start {container} >/dev/null 2>&1 || docker run -d --name {container} --network coolify --restart unless-stopped --label coolify.managed=true -v {path}:/www:ro {PROXY_ERROR_PAGE_IMAGE} httpd -f -p 80 -h /www"
            ),
            "docker network connect coolify coolify-proxy >/dev/null 2>&1 || true".to_string(),
        ],
        "proxy.error_page",
    )
    .await?;
    Ok(())
}

/// `Server::setupMaintenancePages` for a server without resources in maintenance mode.
pub async fn setup_maintenance_pages(srv: &Srv) -> Result<(), String> {
    let proxy_type = srv.proxy_type();
    if srv.info.is_swarm() || !matches!(proxy_type, Some("TRAEFIK" | "CADDY")) {
        return Ok(());
    }
    let path = format!("{BASE_CONFIG_DIR}/proxy/error-pages");
    srv.run(
        &[
            format!("mkdir -p {path}"),
            format!("find {path} -maxdepth 1 -name 'maintenance-*.html' -delete"),
        ],
        "proxy.maintenance",
    )
    .await?;
    setup_proxy_error_page_container(srv).await?;
    let dynamic = format!("{}/dynamic", srv.proxy_path());
    srv.run(&[format!("mkdir -p {dynamic}")], "proxy.maintenance")
        .await?;
    if proxy_type == Some("TRAEFIK") {
        srv.run(
            &[format!("rm -f {dynamic}/coolify-maintenance.yaml")],
            "proxy.maintenance",
        )
        .await?;
    } else {
        upload_proxy_file(
            srv,
            &format!("{dynamic}/Caddyfile"),
            &caddy_base_caddyfile(),
        )
        .await?;
    }
    Ok(())
}

/// `Server::defaultRedirectConfiguration`.
fn default_redirect_configuration(srv: &Srv) -> String {
    let redirect_url = srv
        .proxy
        .get("redirect_url")
        .and_then(Value::as_str)
        .filter(|u| !u.trim().is_empty())
        .map(str::to_string);
    let uses_error_page = uses_proxy_error_page(srv);
    if srv.proxy_type() == Some("CADDY") {
        if let Some(url) = &redirect_url {
            return format!(":80, :443 {{\n   redir {url}\n}}");
        }
        if uses_error_page {
            return format!(
                ":80, :443 {{\n    reverse_proxy {PROXY_ERROR_PAGE_CONTAINER}:80 {{\n        rewrite /503.html\n        handle_response {{\n            copy_response 503\n        }}\n    }}\n}}"
            );
        }
        return ":80, :443 {\n    respond 503\n}".to_string();
    }
    let tls = match srv.traefik_certificate_resolver() {
        Some(r) => PhpValue::map([("certResolver", PhpValue::Str(r))]),
        None => PhpValue::Array(Vec::new()),
    };
    let mut catchall: Vec<(String, PhpValue)> = vec![
        ("entryPoints".into(), PhpValue::strings(["http", "https"])),
        ("service".into(), PhpValue::Str("noop".into())),
        ("rule".into(), PhpValue::Str("PathPrefix(`/`)".into())),
        ("tls".into(), tls),
        ("priority".into(), PhpValue::Int(-1000)),
    ];
    let mut noop_servers = PhpValue::Array(Vec::new());
    let mut middlewares: Option<PhpValue> = None;
    let mut error_service: Option<PhpValue> = None;
    if let Some(url) = &redirect_url {
        catchall.push(("middlewares".into(), PhpValue::strings(["redirect-regexp"])));
        noop_servers = PhpValue::list([PhpValue::map([("url", PhpValue::Str(String::new()))])]);
        middlewares = Some(PhpValue::map([(
            "redirect-regexp",
            PhpValue::map([(
                "redirectRegex",
                PhpValue::map([
                    ("regex", PhpValue::Str("(.*)".into())),
                    ("replacement", PhpValue::Str(url.clone())),
                    ("permanent", PhpValue::Bool(false)),
                ]),
            )]),
        )]));
    } else if uses_error_page {
        catchall.push(("middlewares".into(), PhpValue::strings(["error-pages"])));
        middlewares = Some(PhpValue::map([(
            "error-pages",
            PhpValue::map([(
                "errors",
                PhpValue::map([
                    ("status", PhpValue::strings(["503"])),
                    ("service", PhpValue::Str("error-pages".into())),
                    ("query", PhpValue::Str("/503.html".into())),
                ]),
            )]),
        )]));
        error_service = Some(PhpValue::map([(
            "loadBalancer",
            PhpValue::map([(
                "servers",
                PhpValue::list([PhpValue::map([(
                    "url",
                    PhpValue::Str(format!("http://{PROXY_ERROR_PAGE_CONTAINER}:80")),
                )])]),
            )]),
        )]));
    }
    let mut routers: Vec<(String, PhpValue)> = Vec::new();
    if srv.info.is_swarm() {
        routers.push(("catchall".into(), PhpValue::map(catchall)));
    } else {
        // A router with `tls` matches only HTTPS, so plain HTTP needs its own catch-all router.
        let mut https = catchall.clone();
        if let Some(e) = https.iter_mut().find(|(k, _)| k == "entryPoints") {
            e.1 = PhpValue::strings(["https"]);
        }
        routers.push(("catchall".into(), PhpValue::map(https)));
        let mut http = catchall;
        http.retain(|(k, _)| k != "tls");
        if let Some(e) = http.iter_mut().find(|(k, _)| k == "entryPoints") {
            e.1 = PhpValue::strings(["http"]);
        }
        routers.push(("catchall-http".into(), PhpValue::map(http)));
    }
    let mut services: Vec<(String, PhpValue)> = vec![(
        "noop".into(),
        PhpValue::map([("loadBalancer", PhpValue::map([("servers", noop_servers)]))]),
    )];
    if let Some(s) = error_service {
        services.push(("error-pages".into(), s));
    }
    let mut http: Vec<(String, PhpValue)> = vec![
        ("routers".into(), PhpValue::map(routers)),
        ("services".into(), PhpValue::map(services)),
    ];
    if let Some(m) = middlewares {
        http.push(("middlewares".into(), m));
    }
    yaml::dump_with_indent(&PhpValue::map([("http", PhpValue::map(http))]), 12, 2)
}

/// `Server::setupDefaultRedirect`.
pub async fn setup_default_redirect(srv: &Srv) -> Result<(), String> {
    let banner = "# This file is generated by Coolify, do not edit it manually.\n# Disable the default redirect to customize (only if you know what are you doing).\n\n";
    let dynamic = format!("{}/dynamic", srv.proxy_path());
    let proxy_type = srv.proxy_type();
    let file = match proxy_type {
        Some("TRAEFIK") => Some(format!("{dynamic}/default_redirect_503.yaml")),
        Some("CADDY") => Some(format!("{dynamic}/default_redirect_503.caddy")),
        _ => None,
    };
    srv.run(
        &[
            format!("mkdir -p {dynamic}"),
            format!("rm -f {dynamic}/default_redirect_404.yaml"),
            format!("rm -f {dynamic}/default_redirect_404.caddy"),
        ],
        "proxy.redirect",
    )
    .await?;
    setup_proxy_error_page_container(srv).await?;
    let file = file.unwrap_or_default();
    if srv.proxy.get("redirect_enabled") == Some(&Value::Bool(false)) {
        srv.run(&[format!("rm -f {file}")], "proxy.redirect")
            .await?;
    } else {
        let conf = format!("{banner}{}", default_redirect_configuration(srv));
        srv.run(
            &[format!(
                "echo '{}' | base64 -d | tee {file} > /dev/null",
                b64(&conf)
            )],
            "proxy.redirect",
        )
        .await?;
    }
    if proxy_type == Some("CADDY") {
        reload_caddy(srv).await?;
    }
    Ok(())
}

/// `Server::reloadCaddy`.
async fn reload_caddy(srv: &Srv) -> Result<(), String> {
    srv.run(
        &[
            "docker exec coolify-proxy caddy reload --config /config/caddy/Caddyfile.autosave"
                .to_string(),
        ],
        "proxy.reload_caddy",
    )
    .await
    .map(|_| ())
}

/// The scheme and host of `Url::fromString($settings->fqdn)`.
fn url_parts(fqdn: &str) -> (String, String) {
    let (scheme, rest) = fqdn.split_once("://").unwrap_or(("", fqdn));
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = if authority.starts_with('[') {
        authority
            .split_once(']')
            .map(|(h, _)| format!("{h}]"))
            .unwrap_or_else(|| authority.to_string())
    } else {
        authority.split(':').next().unwrap_or_default().to_string()
    };
    (scheme.to_string(), host)
}

/// `Server::setupDynamicProxyConfiguration`.
pub async fn setup_dynamic_proxy_configuration(srv: &Srv) -> Result<(), String> {
    let settings = ops::instance_server_settings(srv.db())
        .await
        .map_err(|e| e.to_string())?;
    let dynamic = format!("{}/dynamic", srv.proxy_path());
    let fqdn = settings
        .fqdn
        .as_deref()
        .filter(|f| !f.is_empty() && *f != "0");
    let unused = fqdn.is_none() || !srv.is_localhost();
    match srv.proxy_type() {
        Some("TRAEFIK") => {
            let file = format!("{dynamic}/coolify.yaml");
            if unused {
                srv.run(&[format!("rm -f {file}")], "proxy.dynamic").await?;
                return Ok(());
            }
            let (schema, host_raw) = url_parts(fqdn.unwrap_or_default());
            let host = host_raw.trim_matches(['[', ']']).to_string();
            let force_https = settings.is_dashboard_force_https_enabled == Some(true);
            let router = |entry: &str, service: &str, rule: String| {
                PhpValue::map([
                    ("entryPoints", PhpValue::strings([entry])),
                    ("service", PhpValue::Str(service.to_string())),
                    ("rule", PhpValue::Str(rule)),
                ])
            };
            let http_middlewares = if schema == "https" && force_https {
                "redirect-to-https"
            } else {
                "gzip"
            };
            let host_rule = format!("Host(`{host}`)");
            let mut routers: Vec<(String, PhpValue)> = vec![
                (
                    "coolify-http".into(),
                    PhpValue::map([
                        ("middlewares", PhpValue::strings([http_middlewares])),
                        ("entryPoints", PhpValue::strings(["http"])),
                        ("service", PhpValue::Str("coolify".into())),
                        ("rule", PhpValue::Str(host_rule.clone())),
                    ]),
                ),
                (
                    "coolify-reverb-ws".into(),
                    router(
                        "http",
                        "coolify-reverb",
                        format!("{host_rule} && PathPrefix(`/app`)"),
                    ),
                ),
                (
                    "coolify-reverb-api".into(),
                    router(
                        "http",
                        "coolify-reverb",
                        format!("{host_rule} && PathPrefix(`/apps`)"),
                    ),
                ),
                (
                    "coolify-terminal-ws".into(),
                    router(
                        "http",
                        "coolify-terminal",
                        format!("{host_rule} && PathPrefix(`/terminal/ws`)"),
                    ),
                ),
            ];
            if schema == "https" {
                let tls = match srv.traefik_certificate_resolver() {
                    Some(r) => PhpValue::map([("certresolver", PhpValue::Str(r))]),
                    None => PhpValue::Array(Vec::new()),
                };
                routers.push((
                    "coolify-https".into(),
                    PhpValue::map([
                        ("middlewares", PhpValue::strings(["gzip"])),
                        ("entryPoints", PhpValue::strings(["https"])),
                        ("service", PhpValue::Str("coolify".into())),
                        ("rule", PhpValue::Str(host_rule.clone())),
                        ("tls", tls.clone()),
                    ]),
                ));
                for (name, service, rule) in [
                    (
                        "coolify-reverb-wss",
                        "coolify-reverb",
                        format!("{host_rule} && PathPrefix(`/app`)"),
                    ),
                    (
                        "coolify-reverb-api-https",
                        "coolify-reverb",
                        format!("{host_rule} && PathPrefix(`/apps`)"),
                    ),
                    (
                        "coolify-terminal-wss",
                        "coolify-terminal",
                        format!("{host_rule} && PathPrefix(`/terminal/ws`)"),
                    ),
                ] {
                    routers.push((
                        name.into(),
                        PhpValue::map([
                            ("entryPoints", PhpValue::strings(["https"])),
                            ("service", PhpValue::Str(service.into())),
                            ("rule", PhpValue::Str(rule)),
                            ("tls", tls.clone()),
                        ]),
                    ));
                }
            }
            let lb = |url: &str| {
                PhpValue::map([(
                    "loadBalancer",
                    PhpValue::map([(
                        "servers",
                        PhpValue::list([PhpValue::map([("url", PhpValue::Str(url.into()))])]),
                    )]),
                )])
            };
            let conf = PhpValue::map([(
                "http",
                PhpValue::map([
                    (
                        "middlewares",
                        PhpValue::map([
                            (
                                "redirect-to-https",
                                PhpValue::map([(
                                    "redirectscheme",
                                    PhpValue::map([("scheme", PhpValue::Str("https".into()))]),
                                )]),
                            ),
                            ("gzip", PhpValue::map([("compress", PhpValue::Bool(true))])),
                        ]),
                    ),
                    ("routers", PhpValue::map(routers)),
                    (
                        "services",
                        PhpValue::map([
                            ("coolify", lb("http://coolify:8080")),
                            ("coolify-reverb", lb("http://coolify:6001")),
                            ("coolify-terminal", lb("http://coolify:6002")),
                        ]),
                    ),
                ]),
            )]);
            let text = format!(
                "# This file is automatically generated by Coolify.\n# Do not edit it manually (only if you know what are you doing).\n\n{}",
                yaml::dump_with_indent(&conf, 12, 2)
            );
            srv.run(
                &[
                    format!("mkdir -p {dynamic}"),
                    format!("echo '{}' | base64 -d | tee {file} > /dev/null", b64(&text)),
                ],
                "proxy.dynamic",
            )
            .await?;
        }
        Some("CADDY") => {
            let file = format!("{dynamic}/coolify.caddy");
            if unused {
                srv.run(&[format!("rm -f {file}")], "proxy.dynamic").await?;
                reload_caddy(srv).await?;
                return Ok(());
            }
            let (schema, host) = url_parts(fqdn.unwrap_or_default());
            let force_https = settings.is_dashboard_force_https_enabled == Some(true);
            let site = if schema == "https" && !force_https {
                format!("http://{host}, https://{host}")
            } else {
                format!("{schema}://{host}")
            };
            let text = format!(
                "\n{site} {{\n    encode zstd gzip\n    handle /app/* {{\n        reverse_proxy coolify:6001\n    }}\n    handle /apps* {{\n        reverse_proxy coolify:6001\n    }}\n    handle /terminal/ws {{\n        reverse_proxy coolify:6002\n    }}\n    reverse_proxy coolify:8080\n}}"
            );
            srv.run(
                &[format!(
                    "echo '{}' | base64 -d | tee {file} > /dev/null",
                    b64(&text)
                )],
                "proxy.dynamic",
            )
            .await?;
            reload_caddy(srv).await?;
        }
        _ => {}
    }
    Ok(())
}

/// `Server::changeProxy`.
pub async fn change_proxy(jobs: &Jobs, server_id: i64, proxy_type: &str) -> Result<(), String> {
    let mut srv = Srv::load(jobs, server_id).await?;
    let previous = srv.proxy_type().map(str::to_string);
    srv.proxy_set("type", json!(proxy_type.to_uppercase()));
    srv.proxy_set("status", json!("exited"));
    srv.proxy_set("last_saved_proxy_configuration", Value::Null);
    srv.proxy_set("last_saved_settings", Value::Null);
    srv.proxy_set("last_applied_settings", Value::Null);
    srv.proxy_set("certificates_restart_required", json!(false));
    srv.save_proxy().await?;
    srv.server_update(&[("detected_traefik_version", ColumnValue::Text(None))])
        .await?;
    ops::save_json_column(
        srv.db(),
        srv.id(),
        "traefik_outdated_info",
        None,
        Jobs::now(),
    )
    .await
    .map_err(|e| e.to_string())?;
    if srv.proxy_is_set() {
        let j = jobs.clone();
        tokio::spawn(async move {
            let Ok(mut s) = Srv::load(&j, server_id).await else {
                return;
            };
            if let Err(e) = start_proxy(&mut s, true, false, false).await {
                tracing::warn!(server_id, error = %e, "proxy start failed");
            }
        });
    }
    if previous.as_deref() != srv.proxy_type()
        && srv.info.setting_bool("is_sentinel_enabled") == Some(true)
        && !srv.info.is_build_server()
        && !srv.info.is_swarm()
        && srv.info.setting_bool("force_disabled") != Some(true)
        && !srv.info.transferred
        && srv.info.setting_bool("is_traffic_analytics_enabled") == Some(true)
    {
        jobs.spawn_restart_sentinel(server_id);
    }
    Ok(())
}

/// `CheckProxy::buildPortCheckScript`.
fn port_check_script(port: i64, container: &str) -> String {
    let docker_pattern = shell_arg(&format!("\"{port}/tcp\""));
    let socket_port = shell_arg(&format!(":{port}"));
    let suffix = shell_arg(&format!(":{port} "));
    let port_arg = shell_arg(&port.to_string());
    let get_id = format!("docker ps -a --filter name={container} --format '{{{{.ID}}}}'");
    format!(
        r#"

            CONTAINER_ID=$({get_id});
            if [ ! -z "$CONTAINER_ID" ]; then
                if docker inspect $CONTAINER_ID --format '{{{{json .NetworkSettings.Ports}}}}' | grep -q {docker_pattern}; then
                    echo 'proxy_using_port';
                    exit 0;
                fi;
            fi;


            # Try ss command first
            if command -v ss >/dev/null 2>&1; then
                ss_output=$(ss -Htuln state listening sport = {socket_port} 2>/dev/null);
                if [ -z "$ss_output" ]; then
                    echo 'port_free';
                    exit 0;
                fi;
                count=$(echo "$ss_output" | grep -c {suffix});
                if [ $count -eq 0 ]; then
                    echo 'port_free';
                    exit 0;
                fi;
                # Check for dual-stack or docker processes
                if [ $count -le 2 ] && (echo "$ss_output" | grep -q 'docker\|coolify'); then
                    echo 'port_free';
                    exit 0;
                fi;
                echo "port_conflict|$ss_output";
                exit 0;
            fi;

            # Try netstat as fallback
            if command -v netstat >/dev/null 2>&1; then
                netstat_output=$(netstat -tuln 2>/dev/null | grep {suffix});
                if [ -z "$netstat_output" ]; then
                    echo 'port_free';
                    exit 0;
                fi;
                count=$(echo "$netstat_output" | grep -c 'LISTEN');
                if [ $count -eq 0 ]; then
                    echo 'port_free';
                    exit 0;
                fi;
                if [ $count -le 2 ] && (echo "$netstat_output" | grep -q 'docker\|coolify'); then
                    echo 'port_free';
                    exit 0;
                fi;
                echo "port_conflict|$netstat_output";
                exit 0;
            fi;

            # Final fallback using nc
            if nc -z -w1 127.0.0.1 {port_arg} >/dev/null 2>&1; then
                echo 'port_conflict|nc_detected';
            else
                echo 'port_free';
            fi;
        "#
    )
}

/// `CheckProxy::parsePortCheckResult`.
fn port_conflict(output: Option<&str>, port: i64) -> bool {
    let output = output.map(str::trim).unwrap_or_default();
    let Some(details) = output.strip_prefix("port_conflict|") else {
        return false;
    };
    if details != "nc_detected" {
        let lines = details.split('\n').count();
        if lines <= 2 {
            let any4 = details.contains(&format!("0.0.0.0:{port}"));
            let any6 = details.contains(&format!(":::{port}"));
            let star = details.contains(&format!("*:{port}"));
            let starred = regex::Regex::new(&format!(r"\*:{port}.*IPv[46]"))
                .is_ok_and(|r| r.is_match(details));
            if (any4 && any6) || (star && starred) {
                return false;
            }
        }
    }
    true
}

/// `CheckProxy::handle`: whether the proxy should be started.
pub async fn check_proxy(srv: &mut Srv, from_ui: bool) -> Result<bool, String> {
    if !srv.info.is_functional() {
        return Ok(false);
    }
    if srv.info.is_build_server() {
        let raw = srv.row.record.get("proxy");
        if raw.is_some_and(|p| !p.is_null()) {
            ops::save_proxy(srv.db(), srv.id(), None, Jobs::now())
                .await
                .map_err(|e| e.to_string())?;
        }
        return Ok(false);
    }
    let proxy_type = srv.proxy_type().map(str::to_string);
    if (proxy_type.is_none() || proxy_type.as_deref() == Some("NONE") || srv.force_stop())
        && !from_ui
    {
        return Ok(false);
    }
    if !srv.is_proxy_should_run() {
        if from_ui {
            return Err("Proxy should not run. You selected the Custom Proxy.".to_string());
        }
        return Ok(false);
    }
    let container = if srv.info.is_swarm() {
        "coolify-proxy_traefik"
    } else {
        "coolify-proxy"
    };
    let status = container_status(srv, container).await;
    if srv.info.is_swarm() {
        srv.proxy_set("status", json!(status));
        srv.save_proxy().await?;
        return Ok(status != "running");
    }
    if status == "running" {
        srv.proxy_set("status", json!("running"));
        srv.save_proxy().await?;
        return Ok(false);
    }
    if srv.info.setting_bool("is_cloudflare_tunnel") == Some(true) {
        return Ok(false);
    }
    let mut unchecked_only = false;
    let ports = match get_proxy_configuration(srv, false).await {
        Ok(compose) => match config::ports_from_configuration(&compose) {
            Ok(p) => {
                if p.is_empty() {
                    unchecked_only =
                        config::publishes_only_unchecked_ports(&compose).unwrap_or(false);
                }
                p
            }
            Err(e) => {
                tracing::error!(error = %e.0, "Error checking proxy");
                return Ok(false);
            }
        },
        Err(e) => {
            tracing::error!(error = %e, "Error checking proxy");
            return Ok(false);
        }
    };
    if ports.is_empty() {
        return Ok(unchecked_only);
    }
    for port in ports {
        let script = port_check_script(port, container);
        let output = srv
            .run_timeout(
                &[script],
                "proxy.port_check",
                std::time::Duration::from_secs(10),
            )
            .await;
        if port_conflict(output.as_deref(), port) {
            if from_ui {
                return Err(format!(
                    "Port {port} is in use.<br>You must stop the process using this port.<br><br>Docs: <a target='_blank' class='dark:text-white hover:underline' href='https://coolify.io/docs'>https://coolify.io/docs</a><br>Discord: <a target='_blank' class='dark:text-white hover:underline' href='https://coolify.io/discord'>https://coolify.io/discord</a>"
                ));
            }
            return Ok(false);
        }
    }
    Ok(true)
}

/// `StandaloneDocker::created` hook commands.
pub fn destination_created_commands(network: &str) -> Vec<String> {
    let safe = shell_arg(network);
    vec![
        format!(
            "docker network inspect {safe} >/dev/null 2>&1 || docker network create --attachable {safe} >/dev/null"
        ),
        format!("docker network connect {safe} coolify-proxy >/dev/null 2>&1 || true"),
    ]
}

/// `RemoveStandaloneDockerNetwork::handle`.
pub async fn remove_standalone_network(
    srv: &Srv,
    d: &destinations::Destination,
) -> Result<(), String> {
    let safe = shell_arg(&d.network);
    let _ = srv
        .run_quiet(
            &[format!("docker network disconnect {safe} coolify-proxy")],
            "destination.remove",
        )
        .await;
    srv.run(
        &[docker_network_remove_command(&d.network)],
        "destination.remove",
    )
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_commands() {
        assert_eq!(
            docker_network_ensure_command("coolify", false, false),
            "docker network inspect 'coolify' >/dev/null 2>&1 || docker network create --attachable 'coolify'"
        );
        assert!(
            docker_network_remove_command("n")
                .starts_with("bash -c 'output=$(docker network rm '\\''n'\\'' 2>&1)")
        );
    }

    #[test]
    fn dual_stack_is_not_a_conflict() {
        assert!(!port_conflict(Some("port_free"), 80));
        assert!(port_conflict(Some("port_conflict|nc_detected"), 80));
        assert!(!port_conflict(
            Some("port_conflict|tcp 0.0.0.0:80\ntcp :::80"),
            80
        ));
    }
}
