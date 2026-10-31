//! What a Docker image deployment writes: the container name, the Compose file
//! (`generate_compose_file`) and the runtime env file (`generate_runtime_environment_variables`).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use coolify_compat::yaml::{self, PhpValue};
use coolify_db::applications::ApplicationRow;
use serde_json::Value;

use super::domains::{UrlParts, is_valid_domain_url};
use crate::databases::env::Var;

/// The env file name in the working directory and the configuration directory.
pub const ENV_FILE: &str = concat!(".", "env");

/// `generateApplicationContainerName($application, $pull_request_id)` for a preview.
pub fn preview_container_name(a: &ApplicationRow, pull_request_id: i64) -> String {
    let name = if a.setting_bool("is_consistent_container_name_enabled") {
        a.setting_str("custom_internal_name")
            .filter(|n| !n.is_empty())
            .unwrap_or(&a.uuid)
    } else {
        &a.uuid
    };
    format!("{name}-pr-{pull_request_id}")
}

/// `generateApplicationContainerName($application, 0)`.
pub fn container_name(a: &ApplicationRow, now: chrono::NaiveDateTime) -> String {
    if a.setting_bool("is_consistent_container_name_enabled") {
        return a
            .setting_str("custom_internal_name")
            .filter(|n| !n.is_empty())
            .unwrap_or(&a.uuid)
            .to_owned();
    }
    let prefix = a
        .setting_str("custom_container_name_prefix")
        .filter(|n| !n.is_empty())
        .unwrap_or(&a.uuid);
    format!("{prefix}-{}", now.format("%Y%m%dT%H%M%S"))
}

/// `main_port()`: `[80]` for static sites, else `explode(',', ports_exposes)` (`['']` when unset).
pub fn main_ports(a: &ApplicationRow) -> Vec<String> {
    if a.setting_bool("is_static") {
        return vec!["80".into()];
    }
    a.str("ports_exposes")
        .unwrap_or_default()
        .split(',')
        .map(str::to_owned)
        .collect()
}

/// `isBase64Encoded`.
fn is_base64(v: &str) -> bool {
    B64.decode(v).is_ok_and(|b| B64.encode(b) == v)
}

/// `decodeBase64EncodedLabels`: peels nested base64 layers that decode to label lines.
fn decode_base64_labels(v: &str) -> Option<String> {
    if !is_base64(v) {
        return None;
    }
    let mut decoded = B64.decode(v).ok()?;
    let mut labels = decoded.clone();
    while !decoded.is_empty() {
        let Ok(text) = std::str::from_utf8(&decoded) else {
            break;
        };
        if !is_base64(text) {
            break;
        }
        let Ok(next) = B64.decode(text) else { break };
        decoded = next;
        if let Ok(t) = std::str::from_utf8(&decoded) {
            let only_labels = t
                .split(['\n', '\r'])
                .filter(|l| !l.is_empty())
                .all(|l| l.contains('=') && !l.starts_with('='));
            if only_labels {
                labels = decoded.clone();
            }
        }
    }
    String::from_utf8(labels).ok()
}

/// `parseContainerLabels()`: the label text of the stored `custom_labels` (`None` when unset),
/// or `Err` when the stored base64 is unreadable and the labels have to be regenerated.
pub fn stored_labels(stored: Option<&str>) -> Option<Result<String, ()>> {
    let s = stored.filter(|s| !s.is_empty())?;
    Some(match decode_base64_labels(s) {
        Some(text) => Ok(text),
        None if !is_base64(s) => Ok(s.to_owned()),
        None => Err(()),
    })
}

/// `escapeComposeEnvFileValue`.
pub fn escape_env_file_value(value: Option<&str>, allow_interpolation: bool) -> String {
    let mut v = value
        .unwrap_or_default()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    if !allow_interpolation {
        v = v.replace('$', "\\$");
    }
    format!("\"{v}\"")
}

/// Inputs of the env file besides the application.
#[derive(Debug)]
pub struct EnvContext<'a> {
    pub commit: &'a str,
    /// `$this->branch`: only set by a git import.
    pub branch: Option<&'a str>,
    /// Docker Compose build pack: the `SERVICE_URL_*` / `SERVICE_FQDN_*` / `SERVICE_NAME_*` lines,
    /// and the variables left out of the file (generated `SERVICE_*` keys, unset blanks).
    pub compose: Option<ComposeEnv>,
    pub container_name: &'a str,
    pub project_name: Option<&'a str>,
    pub environment_name: Option<&'a str>,
    pub server_ip: Option<&'a str>,
    /// A preview deployment: `$this->preview->fqdn`, and `variables` holds the deployed preview
    /// variables.
    pub preview: Option<Option<&'a str>>,
}

/// What a Docker Compose deployment adds to the runtime env file.
#[derive(Debug, Default)]
pub struct ComposeEnv {
    pub service_lines: Vec<String>,
    pub excluded_ids: Vec<i64>,
}

/// `hostsFromFqdn`.
fn hosts_from_fqdn(fqdn: Option<&str>) -> String {
    fqdn.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|d| !d.is_empty() && is_valid_domain_url(d))
        .filter_map(|d| UrlParts::parse(d).map(|p| p.host))
        .collect::<Vec<_>>()
        .join(",")
}

/// The NIXPACKS_ / RAILPACK_ variables `runtime_environment_variables` leaves out.
fn excluded_runtime_key(key: &str) -> bool {
    const KEEP: [&str; 4] = [
        "NIXPACKS_PHP_ROOT_DIR",
        "NIXPACKS_PHP_FALLBACK_PATH",
        "NIXPACKS_SPA_OUTPUT_DIR",
        "RAILPACK_SKIP_MIGRATIONS",
    ];
    (key.starts_with("NIXPACKS_") || key.starts_with("RAILPACK_")) && !KEEP.contains(&key)
}

/// `generate_runtime_environment_variables()` for a production Docker image deployment, and
/// the warning upstream logs when the user's `PORT` does not match the exposed ports.
pub fn runtime_env(
    a: &ApplicationRow,
    variables: &[Var],
    ctx: &EnvContext<'_>,
) -> (Vec<String>, Option<String>) {
    let production: Vec<&Var> = variables
        .iter()
        .filter(|v| ctx.preview.is_some() || !v.is_preview)
        .collect();
    let defined = |key: &str| production.iter().any(|v| v.key == key);
    let mut coolify: Vec<(&str, Option<String>)> = Vec::new();
    if !defined("SOURCE_COMMIT") {
        coolify.push(("SOURCE_COMMIT", Some(ctx.commit.to_owned())));
    }
    let parsing = a
        .str("compose_parsing_version")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(1);
    let fqdn = ctx.preview.unwrap_or(a.str("fqdn"));
    if !defined("COOLIFY_FQDN") {
        let key = if parsing >= 3 {
            "COOLIFY_URL"
        } else {
            "COOLIFY_FQDN"
        };
        coolify.push((key, fqdn.map(str::to_owned)));
    }
    if !defined("COOLIFY_URL") {
        let key = if parsing >= 3 {
            "COOLIFY_FQDN"
        } else {
            "COOLIFY_URL"
        };
        coolify.push((key, Some(hosts_from_fqdn(fqdn))));
    }
    let compose_v3 =
        ctx.compose.is_some() && !matches!(a.str("compose_parsing_version"), Some("1" | "2"));
    if !compose_v3 {
        // `$this->branch` is only set by a git import.
        if !defined("COOLIFY_BRANCH") {
            coolify.push(("COOLIFY_BRANCH", ctx.branch.map(str::to_owned)));
        }
        if !defined("COOLIFY_RESOURCE_UUID") {
            coolify.push(("COOLIFY_RESOURCE_UUID", Some(a.uuid.clone())));
        }
        if !defined("COOLIFY_CONTAINER_NAME") {
            coolify.push((
                "COOLIFY_CONTAINER_NAME",
                Some(ctx.container_name.to_owned()),
            ));
        }
    }
    // coolifyDefaultEnvironmentVariables
    if a.settings
        .get("include_coolify_default_variables")
        .and_then(Value::as_bool)
        != Some(false)
    {
        for (k, v) in [
            ("COOLIFY_APP_NAME", a.str("name")),
            ("COOLIFY_PROJECT_NAME", ctx.project_name),
            ("COOLIFY_ENVIRONMENT_NAME", ctx.environment_name),
            ("COOLIFY_SERVER_IP", ctx.server_ip),
        ] {
            if let Some(v) = v
                && !defined(k)
            {
                // `merge` replaces an earlier entry of the same key in place.
                match coolify.iter_mut().find(|(key, _)| *key == k) {
                    Some(slot) => slot.1 = Some(v.to_owned()),
                    None => coolify.push((k, Some(v.to_owned()))),
                }
            }
        }
    }
    let mut envs: Vec<String> = coolify
        .iter()
        .map(|(k, v)| format!("{k}={}", escape_env_file_value(v.as_deref(), false)))
        .collect();
    if let Some(c) = &ctx.compose {
        envs.extend(c.service_lines.iter().cloned());
    }

    let excluded: &[i64] = ctx
        .compose
        .as_ref()
        .map_or(&[], |c| c.excluded_ids.as_slice());
    let mut runtime: Vec<&Var> = production
        .iter()
        .copied()
        .filter(|v| !excluded_runtime_key(&v.key) && !excluded.contains(&v.id))
        .collect();
    if a.setting_bool("is_env_sorting_enabled") {
        runtime.sort_by(|x, y| x.key.cmp(&y.key));
    } else {
        runtime.sort_by_key(|v| v.id);
    }
    let runtime: Vec<&Var> = runtime.into_iter().filter(|v| v.is_runtime).collect();
    let service_ref = |v: &&Var| {
        let value = v.value.as_deref().unwrap_or_default();
        value.starts_with("$SERVICE_") || value.contains("${SERVICE_")
    };
    let (late, early): (Vec<&Var>, Vec<&Var>) = runtime.into_iter().partition(service_ref);
    for v in early.into_iter().chain(late) {
        let line = if v.uses_legacy_escaping {
            v.container_value().unwrap_or_default()
        } else {
            let resolved = v.resolved.clone().unwrap_or_default();
            escape_env_file_value(Some(&resolved), v.allows_interpolation(&resolved))
        };
        envs.push(format!("{}={line}", v.key));
    }

    let ports = main_ports(a);
    let mut warning = None;
    if ctx.compose.is_none()
        && ctx.preview.is_none()
        && a.setting_bool("is_container_label_readonly_enabled")
        && let Some(port) = production
            .iter()
            .find(|v| v.key == "PORT")
            .and_then(|v| v.value.clone())
        && port.trim().parse::<i64>().is_ok()
        && !ports.contains(&port)
    {
        warning = Some(format!(
            "Warning: PORT environment variable ({port}) does not match configured ports_exposes: {}. It could case \"bad gateway\" or \"no server\" errors. Check the \"General\" page to fix it.",
            ports.join(",")
        ));
    }
    if ctx.compose.is_none() && !defined("PORT") && !ports.is_empty() {
        envs.push(format!("PORT={}", ports[0]));
    }
    if !defined("HOST") {
        envs.push("HOST=0.0.0.0".into());
    }
    (envs, warning)
}

/// `generate_healthcheck_commands()`: `(command, full_healthcheck_url)`.
pub fn healthcheck_command(a: &ApplicationRow) -> Option<(String, String)> {
    if a.str("health_check_type") == Some("cmd")
        && let Some(cmd) = a
            .str("health_check_command")
            .filter(|c| !c.trim().is_empty())
    {
        let cmd = cmd.replace("\r\n", " ").replace(['\r', '\n'], " ");
        let ok = cmd.len() <= 1000
            && cmd
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || " -_./:=@,+".contains(c));
        if ok {
            return Some((cmd.clone(), cmd));
        }
    }
    let port = match a.str("health_check_port").filter(|p| !p.is_empty()) {
        Some(p) => p.to_owned(),
        None if a.setting_bool("is_static") => "80".into(),
        None => {
            let first = a
                .str("ports_exposes")
                .unwrap_or_default()
                .split(',')
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned();
            if first.is_empty() {
                return None;
            }
            // `(int) $ports_exposes_array[0]`
            let n: i64 = first
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .unwrap_or(0);
            n.to_string()
        }
    };
    let method = a
        .str("health_check_method")
        .filter(|m| !m.is_empty() && m.chars().all(|c| c.is_ascii_uppercase()))
        .unwrap_or("GET");
    let scheme = a
        .str("health_check_scheme")
        .filter(|s| *s == "http" || *s == "https")
        .unwrap_or("http");
    let host = a
        .str("health_check_host")
        .filter(|h| {
            !h.is_empty()
                && (h
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c))
                    || h.parse::<std::net::IpAddr>().is_ok())
        })
        .map_or_else(
            || "localhost".to_owned(),
            |h| {
                if h.parse::<std::net::Ipv6Addr>().is_ok() {
                    format!("[{h}]")
                } else {
                    h.to_owned()
                }
            },
        );
    let path = a
        .str("health_check_path")
        .filter(|p| !p.is_empty())
        .map(|p| {
            if p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "/-_.~%,;".contains(c))
            {
                p.to_owned()
            } else {
                "/".into()
            }
        })
        .unwrap_or_default();
    let url = format!("{scheme}://{host}:{port}{path}");
    let shown_path = if path.is_empty() { "/" } else { path.as_str() };
    let full = format!("{method}: {scheme}://{host}:{port}{shown_path}");
    Some((
        format!(
            "curl -s -X '{method}' -f '{url}' > /dev/null || wget -q -O- '{url}' > /dev/null || exit 1"
        ),
        full,
    ))
}

/// The inputs of the Compose file of a Docker image deployment.
#[derive(Debug)]
pub struct ComposeInput<'a> {
    pub a: &'a ApplicationRow,
    pub container_name: &'a str,
    pub image: &'a str,
    pub network: &'a str,
    pub labels: Vec<String>,
    /// `(name, mount_path, host_path)` of the persistent volumes.
    pub volumes: &'a [(String, String, Option<String>)],
    /// `"$fs_path:$mount_path"` of the file storages.
    pub file_volumes: &'a [String],
    /// `convertDockerRunToCompose($application->custom_docker_run_options)`, without the `--gpus`
    /// devices when the GPU settings apply.
    pub custom: PhpValue,
}

/// `generate_compose_file()` → `Yaml::dump($docker_compose, 10)`.
pub fn compose(c: &ComposeInput<'_>) -> String {
    let a = c.a;
    let s = |k: &str| a.str(k).unwrap_or_default().to_owned();
    let i = |k: &str, d: i64| a.int(k).unwrap_or(d);
    let ports = main_ports(a);
    let aliases: Vec<String> = std::iter::once(c.container_name.to_owned())
        .chain(
            a.str("custom_network_aliases")
                .and_then(|t| serde_json::from_str::<Vec<String>>(t).ok())
                .unwrap_or_default(),
        )
        .collect();
    let mut service = PhpValue::map([
        ("image", PhpValue::from(c.image)),
        ("container_name", PhpValue::from(c.container_name)),
        ("restart", PhpValue::from("unless-stopped")),
    ]);
    if !ports.is_empty() {
        service.set("expose", PhpValue::strings(ports.clone()));
    }
    service.set(
        "networks",
        PhpValue::map([(
            c.network.to_owned(),
            PhpValue::map([("aliases", PhpValue::strings(aliases))]),
        )]),
    );
    service.set("mem_limit", PhpValue::from(s("limits_memory")));
    service.set("memswap_limit", PhpValue::from(s("limits_memory_swap")));
    service.set(
        "mem_swappiness",
        PhpValue::Int(i("limits_memory_swappiness", 60)),
    );
    service.set(
        "mem_reservation",
        PhpValue::from(s("limits_memory_reservation")),
    );
    service.set(
        "cpus",
        PhpValue::Float(s("limits_cpus").trim().parse().unwrap_or(0.0)),
    );
    service.set("cpu_shares", PhpValue::Int(i("limits_cpu_shares", 1024)));
    service.set("env_file", PhpValue::strings([ENV_FILE]));
    if let Some(grace) = a.setting_int("stop_grace_period") {
        let g = if (1..=3600).contains(&grace) {
            grace
        } else {
            30
        };
        service.set("stop_grace_period", PhpValue::from(format!("{g}s")));
    }
    if a.raw.get("health_check_enabled").and_then(Value::as_bool) != Some(false)
        && let Some((cmd, _)) = healthcheck_command(a)
    {
        service.set(
            "healthcheck",
            PhpValue::map([
                ("test", PhpValue::strings(["CMD-SHELL".to_owned(), cmd])),
                (
                    "interval",
                    PhpValue::from(format!("{}s", i("health_check_interval", 5))),
                ),
                (
                    "timeout",
                    PhpValue::from(format!("{}s", i("health_check_timeout", 5))),
                ),
                ("retries", PhpValue::Int(i("health_check_retries", 10))),
                (
                    "start_period",
                    PhpValue::from(format!("{}s", i("health_check_start_period", 5))),
                ),
            ]),
        );
    }
    if let Some(Value::String(cpuset)) = a.raw.get("limits_cpuset") {
        service.set("cpuset", PhpValue::from(cpuset.as_str()));
    }
    service.set("labels", PhpValue::strings(c.labels.clone()));
    if a.setting_bool("is_gpu_enabled") {
        service.set(
            "deploy",
            PhpValue::map([(
                "resources",
                PhpValue::map([(
                    "reservations",
                    PhpValue::map([(
                        "devices",
                        PhpValue::list([super::docker_run::gpu_device_reservation(&a.settings)]),
                    )]),
                )]),
            )]),
        );
    }
    if let Some(p) = a.str("ports_mappings").filter(|p| !p.is_empty()) {
        service.set("ports", PhpValue::strings(p.split(',').map(str::to_owned)));
    }
    let mut volume_entries: Vec<PhpValue> = Vec::new();
    let mut names = PhpValue::list([]);
    for (name, mount, host) in c.volumes {
        let source = host.as_deref().filter(|h| !h.is_empty()).unwrap_or(name);
        volume_entries.push(PhpValue::from(format!("{source}:{mount}")));
        if host.as_deref().is_none_or(str::is_empty) {
            names.set(
                name,
                PhpValue::map([
                    ("name", PhpValue::from(name.as_str())),
                    ("external", PhpValue::Bool(false)),
                ]),
            );
        }
    }
    volume_entries.extend(c.file_volumes.iter().map(|v| PhpValue::from(v.as_str())));
    if !volume_entries.is_empty() {
        service.set("volumes", PhpValue::list(volume_entries));
    }
    let consistent = a.setting_bool("is_consistent_container_name_enabled");
    let mut custom = c.custom.clone();
    if !matches!(&custom, PhpValue::Array(items) if items.is_empty()) {
        let mut first = |key: &str| {
            let value = custom.get_mut(key).and_then(|v| match v {
                PhpValue::Array(items) => items.first().map(|(_, v)| v.clone()),
                _ => None,
            });
            match value {
                Some(PhpValue::Str(s)) if !s.is_empty() && s != "0" => Some(s),
                _ => None,
            }
        };
        let (ipv4, ipv6) = (first("ip"), first("ip6"));
        custom.remove("ip");
        custom.remove("ip6");
        if !consistent && (ipv4.is_some() || ipv6.is_some()) {
            service.remove("networks");
        }
        for (key, ip) in [("ipv4_address", ipv4), ("ipv6_address", ipv6)] {
            let Some(ip) = ip else { continue };
            if service.get_mut("networks").is_none() {
                service.set("networks", PhpValue::list([]));
            }
            if let Some(networks) = service.get_mut("networks") {
                if networks.get_mut(c.network).is_none() {
                    networks.set(c.network, PhpValue::list([]));
                }
                if let Some(network) = networks.get_mut(c.network) {
                    network.set(key, PhpValue::Str(ip));
                }
            }
        }
        super::docker_run::array_merge_recursive(&mut service, custom);
    }
    // `$docker_compose['services'][$this->application->uuid]` with a consistent container name.
    let service_key = if consistent {
        a.uuid.clone()
    } else {
        c.container_name.to_owned()
    };
    let mut compose = PhpValue::map([
        ("services", PhpValue::map([(service_key, service)])),
        (
            "networks",
            PhpValue::map([(
                c.network.to_owned(),
                PhpValue::map([
                    ("external", PhpValue::Bool(true)),
                    ("name", PhpValue::from(c.network)),
                    ("attachable", PhpValue::Bool(true)),
                ]),
            )]),
        ),
    ]);
    if !matches!(&names, PhpValue::Array(v) if v.is_empty()) {
        compose.set("volumes", names);
    }
    yaml::dump(&compose, 10)
}
