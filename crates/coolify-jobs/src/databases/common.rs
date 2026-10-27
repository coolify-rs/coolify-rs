//! Helpers every database start action uses (upstream helpers in
//! bootstrap/helpers/docker.php and shared.php).

use chrono::{NaiveDateTime, Utc};

use crate::Jobs;
use crate::server::ServerInfo;

/// `database_configuration_dir()` with upstream's default base path.
pub(crate) const CONFIGURATION_DIR: &str = "/data/coolify/databases";

pub(crate) fn now() -> NaiveDateTime {
    Jobs::now()
}

/// `hrtime(true)` stands in as a monotonic nanosecond counter for log ordering.
pub(crate) fn hrtime() -> i64 {
    Utc::now().timestamp_nanos_opt().unwrap_or_default()
}

/// `Str::slug($title)` for the label values (ASCII transliteration is not ported:
/// non-ASCII letters are kept, lowercased).
pub fn slug(title: &str) -> String {
    let mut s = title.replace('_', "-");
    s = s.replace('@', "-at-");
    let lowered = s.to_lowercase();
    let kept: String = lowered
        .chars()
        .filter(|c| *c == '-' || c.is_alphanumeric() || c.is_whitespace())
        .collect();
    let mut out = String::new();
    let mut sep = false;
    for c in kept.chars() {
        if c == '-' || c.is_whitespace() {
            sep = true;
        } else {
            if sep && !out.is_empty() {
                out.push('-');
            }
            sep = false;
            out.push(c);
        }
    }
    out
}

/// `dockerStopCommand($timeout, $containers, $server)`.
pub(crate) fn docker_stop_command(
    timeout: u32,
    container: &str,
    docker_version: Option<&str>,
) -> String {
    let normalized = crate::checks::parse_docker_engine_version(docker_version);
    let new_flag = normalized.as_deref().is_some_and(|v| {
        let mut p = v.split('.').map(|x| x.parse::<u64>().unwrap_or(0));
        (
            p.next().unwrap_or(0),
            p.next().unwrap_or(0),
            p.next().unwrap_or(0),
        ) >= (28, 0, 0)
    });
    let flag = if new_flag {
        format!("--timeout={timeout}")
    } else {
        format!("--time={timeout}")
    };
    format!("docker stop {flag} {container}")
}

/// `serverDockerConfigPrefix`.
pub(crate) fn server_docker_config_prefix(server: &ServerInfo) -> &'static str {
    if server.is_non_root() {
        r#"DOCKER_CONTEXT=default DOCKER_CONFIG="$(find "$HOME/.docker/config.json" -maxdepth 0 -exec dirname {} \; 2>/dev/null)" "#
    } else {
        ""
    }
}

/// `escapeDollarSign`.
pub(crate) fn escape_dollar(v: &str) -> String {
    v.replace('$', "$$")
}

/// `base64_encode`.
pub(crate) fn b64(s: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
}

/// `generate_readme_file($name, now())`: `sanitize_string` keeps the plain text.
pub(crate) fn readme(name: &str, at: NaiveDateTime) -> String {
    format!(
        "Resource name: {name}\nLatest Deployment Date: {}",
        at.format("%Y-%m-%d %H:%M:%S")
    )
}
/// `generate_local_persistent_volumes` and `..._only_volume_names` of the start actions:
/// the service `volumes` entries and the top-level named volumes.
pub(crate) fn volume_entries(
    volumes: &[(String, String, Option<String>)],
) -> (
    Vec<coolify_compat::yaml::PhpValue>,
    coolify_compat::yaml::PhpValue,
) {
    use coolify_compat::yaml::PhpValue;
    let mut entries = Vec::new();
    let mut names = PhpValue::list([]);
    for (name, mount, host) in volumes {
        match host.as_deref().filter(|h| !h.is_empty()) {
            Some(h) => entries.push(PhpValue::from(format!("{h}:{mount}"))),
            None => {
                entries.push(PhpValue::from(format!("{name}:{mount}")));
                names.set(
                    name,
                    PhpValue::map([
                        ("name", PhpValue::from(name.as_str())),
                        ("external", PhpValue::Bool(false)),
                    ]),
                );
            }
        }
    }
    (entries, names)
}

/// `healthCheckConfiguration($test)` (HasDatabaseHealthCheck).
pub(crate) fn healthcheck(
    d: &coolify_db::databases::DatabaseRow,
    test: &[&str],
) -> coolify_compat::yaml::PhpValue {
    use coolify_compat::yaml::PhpValue;
    let int = |k: &str, default: i64| d.int(k).unwrap_or(default);
    PhpValue::map([
        ("test", PhpValue::strings(test.iter().copied())),
        (
            "interval",
            PhpValue::from(format!("{}s", int("health_check_interval", 15))),
        ),
        (
            "timeout",
            PhpValue::from(format!("{}s", int("health_check_timeout", 5))),
        ),
        ("retries", PhpValue::Int(int("health_check_retries", 5))),
        (
            "start_period",
            PhpValue::from(format!("{}s", int("health_check_start_period", 5))),
        ),
    ])
}

/// `defaultDatabaseLabels($database)`.
pub(crate) fn database_labels(
    d: &coolify_db::databases::DatabaseRow,
) -> coolify_compat::yaml::PhpValue {
    use serde_json::Value;
    let name = d.str("name").unwrap_or_default();
    let project = d
        .project
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let environment = d
        .environment
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    coolify_compat::yaml::PhpValue::strings([
        "coolify.managed=true".to_owned(),
        "coolify.type=database".to_owned(),
        format!("coolify.databaseUuid={}", d.uuid),
        format!("coolify.resourceName={}", slug(name)),
        format!("coolify.serviceName={}", slug(name)),
        format!("coolify.projectName={}", slug(project)),
        format!("coolify.environmentName={}", slug(environment)),
        format!("coolify.database.subType={}", d.kind.type_name()),
    ])
}

/// The resource limit keys every database service gets, after `labels` and `healthcheck`.
pub(crate) fn limits(
    d: &coolify_db::databases::DatabaseRow,
) -> Vec<(&'static str, coolify_compat::yaml::PhpValue)> {
    use coolify_compat::yaml::PhpValue;
    let s = |k: &str| d.str(k).unwrap_or_default().to_owned();
    vec![
        ("mem_limit", PhpValue::from(s("limits_memory"))),
        ("memswap_limit", PhpValue::from(s("limits_memory_swap"))),
        (
            "mem_swappiness",
            PhpValue::Int(d.int("limits_memory_swappiness").unwrap_or(60)),
        ),
        (
            "mem_reservation",
            PhpValue::from(s("limits_memory_reservation")),
        ),
        (
            "cpus",
            PhpValue::Float(s("limits_cpus").trim().parse().unwrap_or(0.0)),
        ),
        (
            "cpu_shares",
            PhpValue::Int(d.int("limits_cpu_shares").unwrap_or(1024)),
        ),
    ]
}

/// The external network block of a database compose file.
pub(crate) fn networks(network: &str) -> coolify_compat::yaml::PhpValue {
    use coolify_compat::yaml::PhpValue;
    PhpValue::map([(
        network.to_owned(),
        PhpValue::map([
            ("external", PhpValue::Bool(true)),
            ("name", PhpValue::from(network)),
            ("attachable", PhpValue::Bool(true)),
        ]),
    )])
}

/// The commands after the compose file is built: write it and the README, pull,
/// replace the old container and start (`$extra` runs after the pull, e.g. `chown`).
pub(crate) fn finish_commands(
    commands: &mut Vec<String>,
    d: &coolify_db::databases::DatabaseRow,
    server: &ServerInfo,
    compose_yaml: &str,
    now: NaiveDateTime,
    extra: &[String],
) {
    let name = d.uuid.as_str();
    let dir = format!("{CONFIGURATION_DIR}/{name}");
    commands.push(format!(
        "echo '{}' | base64 -d | tee {dir}/docker-compose.yml > /dev/null",
        b64(compose_yaml)
    ));
    commands.push(format!(
        "echo '{}' | tee {dir}/README.md > /dev/null",
        readme(d.str("name").unwrap_or_default(), now)
    ));
    commands.push(format!(
        "echo {}",
        coolify_remote::shell::quote(&format!(
            "Pulling {} image.",
            d.str("image").unwrap_or_default()
        ))
    ));
    commands.push(format!(
        "{}docker compose -f {dir}/docker-compose.yml pull",
        server_docker_config_prefix(server)
    ));
    commands.extend(extra.iter().cloned());
    commands.push(format!(
        "{} 2>/dev/null || true",
        docker_stop_command(10, name, server.docker_version())
    ));
    commands.push(format!("docker rm -f {name} 2>/dev/null || true"));
    commands.push(format!("docker compose -f {dir}/docker-compose.yml up -d"));
    commands.push("echo 'Database started.'".into());
}
