//! `ValidateAndInstallServerJob`, `InstallPrerequisites`, `InstallDocker` and
//! `Server::gatherServerMetadata`.

use std::time::Duration;

use base64::Engine as _;
use coolify_compat::php_json;
use coolify_db::server_ops as ops;
use coolify_db::servers::ColumnValue;
use coolify_remote::RunOptions;
use serde_json::{Map, Value, json};

use super::{Srv, proxy, sentinel};
use crate::Jobs;
use crate::checks;

const MAX_TRIES: u32 = 3;
const TRANSFERRED: &str =
    "This server was transferred to another Coolify instance and cannot be revalidated here.";
/// `InstallDocker`'s daemon configuration, with its original layout.
const DAEMON_CONFIG: &str = "{\n            \"log-driver\": \"json-file\",\n            \"log-opts\": {\n              \"max-size\": \"10m\",\n              \"max-file\": \"3\"\n            }\n          }";

fn stored_logs(message: &str) -> String {
    coolify_compat::purify::validation_logs(&coolify_compat::redact::redact_text(message, &[], &[]))
}

async fn set_logs(srv: &Srv, message: &str) -> Result<(), String> {
    srv.server_update(&[
        (
            "validation_logs",
            ColumnValue::Text(Some(stored_logs(message))),
        ),
        ("is_validating", ColumnValue::Bool(false)),
    ])
    .await
    .map(|_| ())
}

/// `validateConnection()`: one attempt.
pub async fn validate_connection(srv: &Srv) -> Result<(bool, String), String> {
    validate_connection_with(srv, false).await
}

/// `validateConnection(justCheckingNewKey: true)`: a key that cannot log in is not a server that
/// went offline, so a failure does not mark the server unreachable. The error is the SSH error.
pub async fn validate_new_key(srv: &Srv) -> Result<(bool, String), String> {
    validate_connection_with(srv, true).await
}

async fn validate_connection_with(
    srv: &Srv,
    just_checking_new_key: bool,
) -> Result<(bool, String), String> {
    // v4.7.5: a validation enables a disabled self-hosted server again, but only when the
    // connection works.
    let re_enabling = srv.info.can_be_re_enabled() && !srv.info.has_placeholder_ip();
    let enabled;
    let srv = if re_enabling {
        // `forceEnableServer()`.
        srv.settings_update(&[("force_disabled", ColumnValue::Bool(false))])
            .await?;
        enabled = Srv::load(&srv.jobs, srv.id()).await?;
        &enabled
    } else {
        srv
    };
    if srv.info.skipped() {
        return Ok((false, "Server skipped.".to_string()));
    }
    super::host_key::before_validation(&srv.jobs, &srv.info).await;
    let (uptime, error) = match srv.target().await {
        Err(e) => (false, e),
        Ok(t) => match srv
            .jobs
            .remote()
            .run(
                &t,
                &["ls /"],
                RunOptions::op("validate.connection").attempts(1),
            )
            .await
        {
            Ok(_) => {
                super::host_key::learn_now(&srv.jobs, &srv.info).await;
                (true, String::new())
            }
            Err(e) => (
                false,
                super::host_key::validation_error(&srv.jobs, &srv.info, e.message).await,
            ),
        },
    };
    if re_enabling && !uptime {
        // `forceDisableServer()`.
        srv.settings_update(&[("force_disabled", ColumnValue::Bool(true))])
            .await?;
    }
    if srv.info.setting_bool("is_reachable") != Some(uptime) && (uptime || !just_checking_new_key) {
        srv.settings_update(&[("is_reachable", ColumnValue::Bool(uptime))])
            .await?;
    }
    Ok((uptime, error))
}

/// `validateOS()`.
async fn validate_os(srv: &Srv) -> Result<Option<String>, String> {
    let release = srv
        .run(&["cat /etc/os-release".to_string()], "validate.os")
        .await?;
    Ok(checks::supported_os_from_os_release(release.as_deref()).map(str::to_string))
}

pub async fn missing_prerequisites(srv: &Srv) -> Vec<&'static str> {
    let mut missing = Vec::new();
    for cmd in ["git", "curl", "jq"] {
        let found = srv
            .run_quiet(&[format!("command -v {cmd}")], "validate.prerequisites")
            .await;
        if !checks::php_truthy(found.as_deref()) {
            missing.push(cmd);
        }
    }
    missing
}

/// The command list of `InstallPrerequisites::handle`.
pub fn prerequisite_commands(os: &str) -> Result<Vec<String>, String> {
    let mut c: Vec<&str> = if os.contains("debian") {
        vec![
            "echo 'Installing Prerequisites...'",
            "apt-get update -y",
            "command -v curl >/dev/null || apt install -y curl",
            "command -v wget >/dev/null || apt install -y wget",
            "command -v git >/dev/null || apt install -y git",
            "command -v jq >/dev/null || apt install -y jq",
        ]
    } else if os.contains("rhel") {
        vec![
            "echo 'Installing Prerequisites...'",
            "command -v curl >/dev/null || dnf install -y curl",
            "command -v wget >/dev/null || dnf install -y wget",
            "command -v git >/dev/null || dnf install -y git",
            "command -v jq >/dev/null || dnf install -y jq",
        ]
    } else if os.contains("sles") {
        vec![
            "echo 'Installing Prerequisites...'",
            "zypper update -y",
            "command -v curl >/dev/null || zypper install -y curl",
            "command -v wget >/dev/null || zypper install -y wget",
            "command -v git >/dev/null || zypper install -y git",
            "command -v jq >/dev/null || zypper install -y jq",
        ]
    } else if os.contains("arch") {
        vec![
            "echo 'Installing Prerequisites for Arch Linux...'",
            "pacman -Syu --noconfirm --needed curl wget git jq",
        ]
    } else if os.contains("alpine") {
        vec![
            "echo 'Installing Prerequisites for Alpine Linux...'",
            "sed -i '/^#.*\\/community/s/^#//' /etc/apk/repositories 2>/dev/null || true",
            "apk update",
            "command -v bash >/dev/null || apk add bash",
            "command -v curl >/dev/null || apk add curl",
            "command -v wget >/dev/null || apk add wget",
            "command -v git >/dev/null || apk add git",
            "command -v jq >/dev/null || apk add jq",
        ]
    } else {
        return Err("Unsupported OS type for prerequisites installation".to_string());
    };
    c.push("echo 'Prerequisites installed successfully.'");
    Ok(c.iter().map(|s| (*s).to_string()).collect())
}

/// The command list of `InstallDocker::handle`.
pub fn docker_install_commands(os: &str, swarm: bool) -> Vec<String> {
    let config = base64::engine::general_purpose::STANDARD.encode(DAEMON_CONFIG);
    let install = if os.contains("debian") {
        // v4.7.5: Docker has no repository for Ubuntu derivatives (Pop!_OS, Linux Mint) and none
        // for Raspbian Trixie.
        "curl -fsSL https://get.docker.com | sh || (. /etc/os-release && DOCKER_CODENAME=\"${UBUNTU_CODENAME:-$VERSION_CODENAME}\" && case \"$ID:$DOCKER_CODENAME: $ID_LIKE \" in raspbian:trixie:*) DOCKER_REPO=debian ;; *\" ubuntu \"*) DOCKER_REPO=ubuntu ;; *) DOCKER_REPO=$ID ;; esac && install -m 0755 -d /etc/apt/keyrings && curl -fsSL https://download.docker.com/linux/${DOCKER_REPO}/gpg -o /etc/apt/keyrings/docker.asc && chmod a+r /etc/apt/keyrings/docker.asc && echo \"deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/${DOCKER_REPO} ${DOCKER_CODENAME} stable\" > /etc/apt/sources.list.d/docker.list && apt-get update && apt-get install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin)"
    } else if os.contains("rhel") {
        "curl -fsSL https://get.docker.com | sh || (dnf config-manager --add-repo https://download.docker.com/linux/centos/docker-ce.repo && dnf install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin && systemctl start docker && systemctl enable docker)"
    } else if os.contains("sles") {
        "curl -fsSL https://get.docker.com | sh || (zypper addrepo https://download.docker.com/linux/sles/docker-ce.repo && zypper refresh && zypper install -y --no-confirm docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin && systemctl start docker && systemctl enable docker)"
    } else if os.contains("arch") {
        "pacman -Syu --noconfirm --needed docker docker-compose && systemctl enable docker.service && systemctl start docker.service"
    } else if os.contains("alpine") {
        "apk update && apk add docker docker-cli-buildx docker-cli-compose && mkdir -p /etc/docker"
    } else {
        "curl -fsSL https://get.docker.com | sh"
    };
    let mut c = vec![
        "echo 'Installing Docker Engine...'".to_string(),
        install.to_string(),
        "echo 'Configuring Docker Engine (merging existing configuration with the required)...'"
            .to_string(),
        "test -s /etc/docker/daemon.json && cp /etc/docker/daemon.json \"/etc/docker/daemon.json.original-$(date +\"%Y%m%d-%H%M%S\")\"".to_string(),
        format!("test ! -s /etc/docker/daemon.json && echo '{config}' | base64 -d | tee /etc/docker/daemon.json > /dev/null"),
        format!("echo '{config}' | base64 -d | tee /etc/docker/daemon.json.coolify > /dev/null"),
        "jq . /etc/docker/daemon.json.coolify | tee /etc/docker/daemon.json.coolify.pretty > /dev/null".to_string(),
        "mv /etc/docker/daemon.json.coolify.pretty /etc/docker/daemon.json.coolify".to_string(),
        "jq -s '.[0] * .[1]' /etc/docker/daemon.json.coolify /etc/docker/daemon.json | tee /etc/docker/daemon.json.appended > /dev/null".to_string(),
        "mv /etc/docker/daemon.json.appended /etc/docker/daemon.json".to_string(),
        "echo 'Restarting Docker Engine...'".to_string(),
    ];
    if os.contains("alpine") {
        c.push("rc-update add docker default".to_string());
        c.push("rc-service docker restart".to_string());
    } else {
        c.push("systemctl enable docker >/dev/null 2>&1 || true".to_string());
        c.push("systemctl restart docker".to_string());
    }
    if swarm {
        c.push(
            "docker network create --attachable --driver overlay coolify-overlay >/dev/null 2>&1 || true"
                .to_string(),
        );
    } else {
        c.push("docker network create --attachable coolify >/dev/null 2>&1 || true".to_string());
        c.push("echo 'Done!'".to_string());
    }
    c
}

/// `InstallDocker::handle`.
pub async fn install_docker(srv: &Srv, os: &str) -> Result<(), String> {
    if ops::ca_certificate(srv.db(), srv.id())
        .await
        .map_err(|e| e.to_string())?
        .is_none()
    {
        let cert = super::certificate::generate_ca_certificate(&srv.jobs, srv.id()).await?;
        srv.run(
            &super::certificate::ca_certificate_file_commands(&cert),
            "server.ca_certificate",
        )
        .await?;
    }
    let networks = coolify_db::destinations::networks(
        srv.db(),
        coolify_db::destinations::Kind::Standalone,
        srv.id(),
    )
    .await
    .map_err(|e| e.to_string())?;
    if networks.is_empty() && srv.id() != 0 {
        let now = Jobs::now();
        coolify_db::destinations::create(
            srv.db(),
            coolify_db::destinations::Kind::Standalone,
            &coolify_domain::new_public_id(),
            "coolify",
            "coolify",
            srv.id(),
            now,
        )
        .await
        .map_err(|e| e.to_string())?;
        let _ = srv
            .run_quiet(
                &proxy::destination_created_commands("coolify"),
                "destination.created",
            )
            .await;
    }
    srv.run(
        &docker_install_commands(os, srv.info.is_swarm()),
        "server.install_docker",
    )
    .await
    .map(|_| ())
}

/// `validateDockerEngine()`, `validateDockerCompose()`.
async fn docker_ready(srv: &Srv) -> Result<(bool, bool), String> {
    let t = srv.target().await?;
    let remote = srv.jobs.remote();
    let mut docker = false;
    let binary = remote
        .run_quiet(
            &t,
            &["command -v docker"],
            RunOptions::op("validate.docker").no_sudo(),
        )
        .await;
    if binary.is_some()
        && remote
            .run(&t, &["docker version"], RunOptions::op("validate.docker"))
            .await
            .is_ok()
    {
        docker = true;
        srv.settings_update(&[("is_usable", ColumnValue::Bool(true))])
            .await?;
        if !srv.info.is_build_server() {
            remote
                .run_quiet(
                    &t,
                    &["docker network create coolify --attachable >/dev/null 2>&1 || true"],
                    RunOptions::op("validate.network"),
                )
                .await;
        }
    } else {
        srv.settings_update(&[("is_usable", ColumnValue::Bool(false))])
            .await?;
    }
    let compose = remote
        .run_quiet(
            &t,
            &["docker compose version"],
            RunOptions::op("validate.compose"),
        )
        .await
        .is_some();
    srv.settings_update(&[("is_usable", ColumnValue::Bool(compose))])
        .await?;
    Ok((docker, compose))
}

/// `validateDockerEngineVersion()`.
async fn docker_version(srv: &Srv) -> Result<(bool, Option<String>), String> {
    let t = srv.target().await?;
    let remote = srv.jobs.remote();
    let raw = remote
        .run_quiet(
            &t,
            &["docker version --format json"],
            RunOptions::op("validate.docker_version"),
        )
        .await;
    let version = checks::docker_engine_version_from_json(raw.as_deref());
    let stored = checks::parse_docker_engine_version(version.as_deref());
    srv.settings_update(&[
        ("docker_version", ColumnValue::Text(stored.clone())),
        ("docker_version_checked_at", ColumnValue::Time(Jobs::now())),
    ])
    .await?;
    let compose_raw = remote
        .run_quiet(
            &t,
            &["docker compose version --short"],
            RunOptions::op("validate.compose_version"),
        )
        .await;
    srv.settings_update(&[
        (
            "compose_version",
            ColumnValue::Text(checks::parse_docker_engine_version(compose_raw.as_deref())),
        ),
        ("compose_version_checked_at", ColumnValue::Time(Jobs::now())),
    ])
    .await?;
    let ok = checks::check_minimum_docker_engine_version(version.as_deref()).is_some();
    if ok {
        srv.settings_update(&[
            ("is_reachable", ColumnValue::Bool(true)),
            ("is_usable", ColumnValue::Bool(true)),
        ])
        .await?;
    } else {
        srv.settings_update(&[("is_usable", ColumnValue::Bool(false))])
            .await?;
    }
    Ok((ok, stored))
}

/// `Server::gatherServerMetadata`.
pub async fn gather_server_metadata(jobs: &Jobs, server_id: i64) -> Result<bool, String> {
    let srv = Srv::load(jobs, server_id).await?;
    if !srv.info.is_functional() {
        return Ok(false);
    }
    let command = r#"echo "---PRETTY_NAME---" && grep PRETTY_NAME /etc/os-release | cut -d= -f2 | tr -d '"' && echo "---ARCH---" && uname -m && echo "---KERNEL---" && uname -r && echo "---CPUS---" && nproc && echo "---MEMORY---" && free -b | awk '/Mem:/{print $2}' && echo "---UPTIME_SINCE---" && uptime -s && echo "---DOCKER---" && (docker version --format '{{.Server.Version}}' 2>/dev/null || true) && echo "---COMPOSE---" && (docker compose version --short 2>/dev/null || true) && echo "---GPUS---" && (nvidia-smi --query-gpu=index,uuid,memory.total,name --format=csv,noheader,nounits 2>/dev/null || true) && echo "---NVIDIA_TOOLKIT---" && (command -v nvidia-container-runtime-hook nvidia-ctk 2>/dev/null || true)"#;
    let Some(output) = srv
        .run_quiet(&[command.to_string()], "server.metadata")
        .await
    else {
        return Ok(false);
    };
    let mut sections: Map<String, Value> = Map::new();
    let mut lines: std::collections::HashMap<String, Vec<String>> = Default::default();
    let mut current: Option<String> = None;
    for line in coolify_remote::text::php_trim(&output).split('\n') {
        let line = coolify_remote::text::php_trim(line);
        if line.starts_with("---") && line.ends_with("---") && line.len() > 6 {
            current = Some(line.trim_matches('-').to_string());
        } else if let Some(k) = &current {
            sections.insert(k.clone(), json!(line));
            if !line.is_empty() {
                lines.entry(k.clone()).or_default().push(line.to_string());
            }
        }
    }
    let text = |k: &str, d: &str| {
        sections
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or(d)
            .to_string()
    };
    let gpus: Vec<Value> = lines
        .get("GPUS")
        .map(|gs| {
            gs.iter()
                .filter_map(|l| {
                    let p: Vec<&str> = l.splitn(4, ',').map(str::trim).collect();
                    (p.len() == 4).then(|| {
                        json!({
                            "index": p[0].parse::<i64>().unwrap_or(0),
                            "uuid": p[1],
                            "memory_mb": p[2].parse::<i64>().unwrap_or(0),
                            "name": p[3],
                        })
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let mut current_meta = match srv.row.record.get("server_metadata") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    current_meta.insert("os".into(), json!(text("PRETTY_NAME", "Unknown")));
    current_meta.insert("arch".into(), json!(text("ARCH", "Unknown")));
    current_meta.insert("kernel".into(), json!(text("KERNEL", "Unknown")));
    current_meta.insert(
        "cpus".into(),
        json!(text("CPUS", "0").parse::<i64>().unwrap_or(0)),
    );
    current_meta.insert(
        "memory_bytes".into(),
        json!(text("MEMORY", "0").parse::<i64>().unwrap_or(0)),
    );
    current_meta.insert(
        "uptime_since".into(),
        sections.get("UPTIME_SINCE").cloned().unwrap_or(Value::Null),
    );
    current_meta.insert("gpus".into(), Value::Array(gpus));
    current_meta.insert(
        "nvidia_container_toolkit".into(),
        json!(lines.contains_key("NVIDIA_TOOLKIT")),
    );
    current_meta.insert(
        "collected_at".into(),
        json!(
            chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%S+00:00")
                .to_string()
        ),
    );
    let encoded = php_json::encode(&Value::Object(current_meta));
    ops::save_json_column(
        srv.db(),
        srv.id(),
        "server_metadata",
        Some(&encoded),
        Jobs::now(),
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Some(v) =
        checks::parse_docker_engine_version(sections.get("DOCKER").and_then(Value::as_str))
    {
        srv.settings_update(&[
            ("docker_version", ColumnValue::Text(Some(v))),
            ("docker_version_checked_at", ColumnValue::Time(Jobs::now())),
        ])
        .await?;
    }
    if let Some(v) =
        checks::parse_docker_engine_version(sections.get("COMPOSE").and_then(Value::as_str))
    {
        srv.settings_update(&[
            ("compose_version", ColumnValue::Text(Some(v))),
            ("compose_version_checked_at", ColumnValue::Time(Jobs::now())),
        ])
        .await?;
    }
    Ok(true)
}

/// `ValidateAndInstallServerJob::handle`; retries follow after 30 seconds, as the queued job.
pub async fn validate_and_install(jobs: &Jobs, server_id: i64, tries: u32) -> Result<(), String> {
    let result = steps(jobs, server_id, tries).await;
    if let Err(e) = &result {
        let srv = Srv::load(jobs, server_id).await?;
        let message = format!(
            "An error occurred during validation: {}",
            checks::html_escape(e)
        );
        set_logs(&srv, &message).await?;
    }
    Ok(())
}

fn retry(jobs: &Jobs, server_id: i64, tries: u32) {
    let j = jobs.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        if let Err(e) = validate_and_install(&j, server_id, tries + 1).await {
            tracing::warn!(server_id, error = %e, "server install failed");
        }
    });
}

async fn steps(jobs: &Jobs, server_id: i64, tries: u32) -> Result<(), String> {
    let mut srv = Srv::load(jobs, server_id).await?;
    if srv.info.transferred {
        return set_logs(&srv, TRANSFERRED).await;
    }
    srv.server_update(&[("is_validating", ColumnValue::Bool(true))])
        .await?;
    let (uptime, error) = validate_connection(&srv).await?;
    if !uptime {
        let message = format!(
            "Server is not reachable. Please validate your configuration and connection.<br>Check this <a target=\"_blank\" class=\"underline\" href=\"https://coolify.io/docs/knowledge-base/server/openssh\">documentation</a> for further help. <br><br>Error: {}",
            checks::html_escape(&error)
        );
        return set_logs(&srv, &message).await;
    }
    let Some(os) = validate_os(&srv).await? else {
        return set_logs(
            &srv,
            "Server OS type is not supported. Please install Docker manually before continuing: <a target=\"_blank\" class=\"underline\" href=\"https://docs.docker.com/engine/install/#server\">documentation</a>.",
        )
        .await;
    };
    let missing = missing_prerequisites(&srv).await;
    if !missing.is_empty() {
        if tries >= MAX_TRIES {
            return set_logs(
                &srv,
                &format!(
                    "Prerequisites ({}) could not be installed after {MAX_TRIES} attempts. Please install them manually before continuing.",
                    missing.join(", ")
                ),
            )
            .await;
        }
        // `installPrerequisites()` runs as a queued remote process; its failure does not stop the retry.
        if let Ok(cmds) = prerequisite_commands(&os)
            && let Err(e) = srv.run(&cmds, "server.install_prerequisites").await
        {
            tracing::warn!(server_id, error = %e, "installing prerequisites failed");
        }
        retry(jobs, server_id, tries);
        return Ok(());
    }
    let (docker, compose) = docker_ready(&srv).await?;
    if !docker || !compose {
        if tries >= MAX_TRIES {
            return set_logs(
                &srv,
                &format!(
                    "Docker Engine could not be installed after {MAX_TRIES} attempts. Please install Docker manually before continuing: <a target=\"_blank\" class=\"underline\" href=\"https://docs.docker.com/engine/install/#server\">documentation</a>."
                ),
            )
            .await;
        }
        if let Err(e) = install_docker(&srv, &os).await {
            tracing::warn!(server_id, error = %e, "installing Docker failed");
        }
        retry(jobs, server_id, tries);
        return Ok(());
    }
    srv.refresh().await?;
    let (version_ok, stored) = docker_version(&srv).await?;
    if !version_ok {
        return set_logs(
            &srv,
            &checks::docker_version_message(stored.as_deref(), &srv.info.user),
        )
        .await;
    }
    srv.refresh().await?;
    if srv.is_proxy_should_run() {
        match proxy::check_proxy(&mut srv, true).await {
            Ok(true) => {
                let networks = proxy::ensure_proxy_networks_exist_public(&srv).await?;
                let _ = srv.run_quiet(&networks, "proxy.networks").await;
                let j = jobs.clone();
                tokio::spawn(async move {
                    let Ok(mut s) = Srv::load(&j, server_id).await else {
                        return;
                    };
                    if let Err(e) = proxy::start_proxy(&mut s, true, false, false).await {
                        tracing::warn!(server_id, error = %e, "proxy start failed");
                    }
                });
            }
            Ok(false) => {}
            Err(e) => return Err(e),
        }
    }
    srv.server_update(&[("is_validating", ColumnValue::Bool(false))])
        .await?;
    gather_server_metadata(jobs, server_id).await?;
    let fresh = Srv::load(jobs, server_id).await?;
    if sentinel::sentinel_enabled(&fresh) {
        jobs.spawn_check_sentinel(server_id);
    }
    Ok(())
}

impl Jobs {
    /// `ValidateAndInstallServerJob::dispatch($server)`.
    pub fn spawn_validate_and_install(&self, server_id: i64) {
        self.spawn_server_job("validate-and-install", server_id, |jobs, id| async move {
            validate_and_install(&jobs, id, 0).await
        });
    }
}
