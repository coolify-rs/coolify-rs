//! `StartLogDrain` and `StopLogDrain`.

use base64::Engine as _;
use coolify_db::server_ops as ops;
use serde_json::Value;

use super::{BASE_CONFIG_DIR, Srv, shell_arg};
use crate::Jobs;

fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s)
}

const COMPOSE: &str = "\nservices:\n  coolify-log-drain:\n    image: cr.fluentbit.io/fluent/fluent-bit:2.0\n    container_name: coolify-log-drain\n    command: -c /fluent-bit.conf\n    env_file:\n      - .env\n    volumes:\n      - ./fluent-bit.conf:/fluent-bit.conf:ro\n      - ./parsers.conf:/parsers.conf:ro\n    ports:\n      - 127.0.0.1:24224:24224\n    labels:\n      - coolify.managed=true\n    restart: unless-stopped\n";
const README: &str = "# New Relic Log Drain\nThis log drain is based on [Fluent Bit](https://fluentbit.io/) and New Relic Log Forwarder.\n\nFiles:\n- `fluent-bit.conf` - configuration file for Fluent Bit\n- `docker-compose.yml` - docker-compose file to run Fluent Bit\n- `.env` - environment variables for Fluent Bit\n";
const PARSERS: &str = "\n[PARSER]\n    Name        empty_line_skipper\n    Format      regex\n    Regex       /^(?!\\s*$).+/\n";

/// `StopLogDrain::handle`.
pub async fn stop_log_drain(srv: &Srv) {
    let _ = srv
        .run_quiet(
            &["docker rm -f coolify-log-drain".to_string()],
            "log_drain.stop",
        )
        .await;
}

/// `fluentBitValue`.
fn fluent_bit_value(v: &str) -> String {
    let t = coolify_remote::text::php_trim(v);
    let mut out = String::new();
    let mut in_run = false;
    for c in t.chars() {
        if c.is_whitespace() || (c as u32) < 0x20 || c as u32 == 0x7f {
            if !in_run {
                out.push('-');
            }
            in_run = true;
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}

fn flag(srv: &Srv, k: &str) -> bool {
    srv.info.setting_bool(k) == Some(true)
}

fn text(srv: &Srv, k: &str) -> String {
    srv.info
        .settings
        .get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `StartLogDrain::handle`. A failure is only reported to the caller (`handleError`).
pub async fn start_log_drain(srv: &Srv) -> Result<Option<String>, String> {
    let kind = if flag(srv, "is_logdrain_newrelic_enabled") {
        "newrelic"
    } else if flag(srv, "is_logdrain_highlight_enabled") {
        "highlight"
    } else if flag(srv, "is_logdrain_axiom_enabled") {
        "axiom"
    } else if flag(srv, "is_logdrain_custom_enabled") {
        "custom"
    } else if flag(srv, "is_logdrain_cloudwatch_enabled") {
        "cloudwatch"
    } else {
        "none"
    };
    if kind == "cloudwatch" {
        stop_log_drain(srv).await;
        return Ok(None);
    }
    if kind == "none" {
        return Ok(None);
    }
    stop_log_drain(srv).await;
    let server_name = fluent_bit_value(srv.str_column("name").unwrap_or_default());
    let filter = format!(
        "[FILTER]\n    Name grep\n    Match *\n    Exclude log 127.0.0.1\n[FILTER]\n    Name                modify\n    Match               *\n    Set                 coolify.server_name {server_name}\n    Rename              COOLIFY_APP_NAME coolify.app_name\n    Rename              COOLIFY_PROJECT_NAME coolify.project_name\n    Rename              COOLIFY_SERVER_IP coolify.server_ip\n    Rename              COOLIFY_ENVIRONMENT_NAME coolify.environment_name\n"
    );
    let (config, parsers, env) = match kind {
        "newrelic" => (
            b64(&format!(
                "\n[SERVICE]\n    Flush     5\n    Daemon    off\n    Tag container_logs\n    Log_Level debug\n    Parsers_File  parsers.conf\n[INPUT]\n    Name              forward\n    Buffer_Chunk_Size 1M\n    Buffer_Max_Size   6M\n{filter}[OUTPUT]\n    Name nrlogs\n    Match *\n    license_key ${{LICENSE_KEY}}\n    # https://log-api.eu.newrelic.com/log/v1 - EU\n    # https://log-api.newrelic.com/log/v1 - US\n    base_uri ${{BASE_URI}}\n"
            )),
            b64(PARSERS),
            format!(
                "LICENSE_KEY={}\nBASE_URI={}\n",
                text(srv, "logdrain_newrelic_license_key"),
                text(srv, "logdrain_newrelic_base_uri")
            ),
        ),
        "highlight" => (
            b64(
                "\n[SERVICE]\n    Flush     5\n    Daemon    off\n    Log_Level debug\n    Parsers_File  parsers.conf\n[INPUT]\n    Name              forward\n    tag               ${HIGHLIGHT_PROJECT_ID}\n    Buffer_Chunk_Size 1M\n    Buffer_Max_Size   6M\n[OUTPUT]\n    Name                forward\n    Match               *\n    Host                otel.highlight.io\n    Port                24224\n",
            ),
            b64(PARSERS),
            format!(
                "HIGHLIGHT_PROJECT_ID={}\n",
                text(srv, "logdrain_highlight_project_id")
            ),
        ),
        "axiom" => (
            b64(&format!(
                "\n[SERVICE]\n    Flush     5\n    Daemon    off\n    Log_Level debug\n    Parsers_File  parsers.conf\n[INPUT]\n    Name              forward\n    Buffer_Chunk_Size 1M\n    Buffer_Max_Size   6M\n{filter}[OUTPUT]\n    Name            http\n    Match           *\n    Host            api.axiom.co\n    Port            443\n    URI             /v1/datasets/${{AXIOM_DATASET_NAME}}/ingest\n    # Authorization Bearer should be an API token\n    Header Authorization Bearer ${{AXIOM_API_KEY}}\n    compress gzip\n    format json\n    json_date_key _time\n    json_date_format iso8601\n    tls On\n"
            )),
            b64(PARSERS),
            format!(
                "AXIOM_DATASET_NAME={}\nAXIOM_API_KEY={}\n",
                text(srv, "logdrain_axiom_dataset_name"),
                text(srv, "logdrain_axiom_api_key")
            ),
        ),
        _ => {
            let enc = srv
                .jobs
                .0
                .encrypter
                .as_ref()
                .ok_or("No application encryption key has been specified.")?;
            let custom = srv
                .info
                .settings
                .get("logdrain_custom_config")
                .and_then(Value::as_str)
                .and_then(|c| enc.decrypt_string(c).ok())
                .unwrap_or_default();
            (
                b64(&custom),
                b64(&text(srv, "logdrain_custom_config_parser")),
                String::new(),
            )
        }
    };
    let dir = format!("{BASE_CONFIG_DIR}/log-drains");
    let compose_path = format!("{dir}/docker-compose.yml");
    let mut commands = vec![
        "echo 'Saving configuration'".to_string(),
        format!("mkdir -p {dir}"),
        format!("echo '{parsers}' | base64 -d | tee {dir}/parsers.conf > /dev/null"),
        format!("echo '{config}' | base64 -d | tee {dir}/fluent-bit.conf > /dev/null"),
        format!(
            "echo '{}' | base64 -d | tee {compose_path} > /dev/null",
            b64(COMPOSE)
        ),
        format!(
            "echo '{}' | base64 -d | tee {dir}/README.md > /dev/null",
            b64(README)
        ),
        format!(
            "echo '{}' | base64 -d | tee {dir}/.env > /dev/null",
            b64(&env)
        ),
        "echo 'Starting Fluent Bit'".to_string(),
        format!("docker compose -f {compose_path} up -d"),
    ];
    // `logDrainNetworkConnectCommands`.
    let services = ops::services_on_server(srv.db(), srv.id())
        .await
        .map_err(|e| e.to_string())?;
    let mut networks: Vec<String> = Vec::new();
    for s in services {
        if s.get("connect_to_docker_network").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let dest = s.get("destination_id").and_then(Value::as_i64);
        let kind = s.get("destination_type").and_then(Value::as_str);
        let table = match kind {
            Some("App\\Models\\SwarmDocker") => coolify_db::destinations::Kind::Swarm,
            _ => coolify_db::destinations::Kind::Standalone,
        };
        if let Some(id) = dest {
            let nets = coolify_db::destinations::networks(srv.db(), table, srv.id())
                .await
                .map_err(|e| e.to_string())?;
            let _ = id;
            for n in nets {
                if !networks.contains(&n) {
                    networks.push(n);
                }
            }
        }
    }
    for n in networks {
        commands.push(format!(
            "docker network connect {} coolify-log-drain >/dev/null 2>&1 || true",
            shell_arg(&n)
        ));
    }
    srv.run(&commands, "log_drain.start").await
}

impl Jobs {
    /// `StartLogDrain::dispatch($server)` or `StopLogDrain::dispatch($server)`.
    pub fn spawn_log_drain(&self, server_id: i64, enabled: bool) {
        self.spawn_server_job("log-drain", server_id, move |jobs, id| async move {
            let srv = super::Srv::load(&jobs, id).await?;
            if enabled {
                start_log_drain(&srv).await?;
            } else {
                stop_log_drain(&srv).await;
            }
            Ok(())
        });
    }
}
