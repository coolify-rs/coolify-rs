//! `GetProxyConfiguration`, `SaveProxyConfiguration`, `ProxyPortParser` and
//! `generateDefaultProxyConfiguration()` (`bootstrap/helpers/proxy.php`).

use std::collections::BTreeSet;
use std::sync::LazyLock;

use base64::Engine as _;
use coolify_compat::yaml::{self, Key, PhpValue};
use coolify_compat::yaml_parse;
use md5::{Digest, Md5};
use regex::Regex;
use serde_json::{Value, json};

use super::{Srv, sentinel};

/// `GetProxyConfiguration::MAX_CONFIGURATION_SIZE_BYTES`.
const MAX_CONFIGURATION_SIZE_BYTES: usize = 5 * 1024 * 1024;

/// `Server::RECOMMENDED_CADDY_PROXY_IMAGE`.
const RECOMMENDED_CADDY_PROXY_IMAGE: &str = "lucaslorentz/caddy-docker-proxy:2.13-alpine";
pub const TRAEFIK_LOGROTATE_CONTAINER: &str = "coolify-proxy-logrotate";

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap_or_else(|_| unreachable!())
}

/// A configuration problem shown to users as `ValidationException::withMessages`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidConfiguration(pub String);

/// Compose merge tags (`!reset`, `!override`) at the start of a value. Reading a
/// configuration ignores them (`Yaml::PARSE_CUSTOM_TAGS` + `withoutTags`), and the
/// ported parser has no custom tag support, so they are removed from the text first.
static MERGE_TAG: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?m)(^[ \t]*(?:-[ \t]+)?(?:[^\s#'\x22:]+:[ \t]+)?)!(?:reset|override)\b[ \t]*")
});

fn parse_without_tags(configuration: &str) -> Result<PhpValue, yaml_parse::ParseError> {
    let text = MERGE_TAG.replace_all(configuration, "$1");
    yaml_parse::parse(&text, 128)
}

fn get<'a>(v: &'a PhpValue, path: &[&str]) -> Option<&'a PhpValue> {
    let mut cur = v;
    for p in path {
        let PhpValue::Array(items) = cur else {
            return None;
        };
        cur = items
            .iter()
            .find(|(k, _)| k.as_string() == *p)
            .map(|(_, v)| v)?;
    }
    Some(cur)
}

fn is_list(items: &[(Key, PhpValue)]) -> bool {
    items
        .iter()
        .enumerate()
        .all(|(i, (k, _))| matches!(k, Key::Int(n) if *n == i as i64))
}

const INVALID_PORTS: &str =
    "Proxy ports must use Docker Compose port syntax with ports from 1 through 65535.";

fn invalid<T>() -> Result<T, InvalidConfiguration> {
    Err(InvalidConfiguration(INVALID_PORTS.to_string()))
}

static VARIABLE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\$(?:\{[A-Za-z_][A-Za-z0-9_]*(?::?[-?+][^${}]*)?\}|[A-Za-z_][A-Za-z0-9_]*)")
});
static DEFAULT: LazyLock<Regex> = LazyLock::new(|| re(r"\$\{[A-Za-z_][A-Za-z0-9_]*:?-([^${}]*)\}"));
static SAFE_REMAINDER: LazyLock<Regex> = LazyLock::new(|| re(r"^[0-9A-Za-z.:\[\]/-]*\z"));
static RANGE: LazyLock<Regex> = LazyLock::new(|| re(r"^(\d+)-(\d+)\z"));
static DIGITS: LazyLock<Regex> = LazyLock::new(|| re(r"^\d+\z"));
static BRACKET: LazyLock<Regex> = LazyLock::new(|| re(r"^\[([^]]+)]:([^:]*):([^:]+)\z"));

fn port_number_int(p: i64) -> Result<i64, InvalidConfiguration> {
    if (1..=65535).contains(&p) {
        Ok(p)
    } else {
        invalid()
    }
}

fn port_number_str(p: &str) -> Result<i64, InvalidConfiguration> {
    if !DIGITS.is_match(p) {
        return invalid();
    }
    port_number_int(p.parse::<i64>().unwrap_or(i64::MAX))
}

/// `portOrRange()`.
fn port_or_range_str(v: &str) -> Result<Option<i64>, InvalidConfiguration> {
    if let Some(c) = RANGE.captures(v) {
        if port_number_str(&c[1])? > port_number_str(&c[2])? {
            return invalid();
        }
        return Ok(None);
    }
    port_number_str(v).map(Some)
}

fn port_or_range(v: &PhpValue) -> Result<Option<i64>, InvalidConfiguration> {
    match v {
        PhpValue::Int(i) => port_number_int(*i).map(Some),
        PhpValue::Str(s) => port_or_range_str(s),
        _ => invalid(),
    }
}

fn validate_host_ip(ip: &str) -> Result<(), InvalidConfiguration> {
    if ip.parse::<std::net::IpAddr>().is_ok() {
        Ok(())
    } else {
        invalid()
    }
}

fn validate_protocol(p: &str) -> Result<(), InvalidConfiguration> {
    if ["tcp", "udp", "sctp"].contains(&p.to_lowercase().as_str()) {
        Ok(())
    } else {
        invalid()
    }
}

fn host_port(host: &str, container: &str) -> Result<Option<i64>, InvalidConfiguration> {
    port_or_range_str(container)?;
    if host.is_empty() {
        Ok(None)
    } else {
        port_or_range_str(host)
    }
}

fn resolve_defaults(value: &str) -> Result<Option<String>, InvalidConfiguration> {
    let remainder = VARIABLE.replace_all(value, "");
    if !SAFE_REMAINDER.is_match(&remainder) {
        return invalid();
    }
    let resolved = DEFAULT.replace_all(value, "$1").to_string();
    Ok(if VARIABLE.is_match(&resolved) {
        None
    } else {
        Some(resolved)
    })
}

fn published_short(definition: &str) -> Result<Option<i64>, InvalidConfiguration> {
    let mut port = definition;
    if let Some(i) = port.rfind('/') {
        validate_protocol(&port[i + 1..])?;
        port = &port[..i];
    }
    if port.starts_with('[') {
        let Some(c) = BRACKET.captures(port) else {
            return invalid();
        };
        validate_host_ip(&c[1])?;
        return host_port(&c[2], &c[3]);
    }
    let mut parts: Vec<&str> = port.split(':').collect();
    if parts.len() > 3 || (parts.len() > 1 && parts[0].is_empty()) {
        return invalid();
    }
    if parts.len() == 3 {
        validate_host_ip(parts.remove(0))?;
    }
    if parts.len() == 1 {
        port_or_range_str(parts[0])
    } else {
        host_port(parts[0], parts[1])
    }
}

fn check_port(v: &str) -> Result<(), InvalidConfiguration> {
    port_or_range_str(v).map(|_| ())
}

/// `publishedPort()`.
fn published_port(v: &PhpValue) -> Result<Option<i64>, InvalidConfiguration> {
    match v {
        PhpValue::Str(s) if VARIABLE.is_match(s) => {
            let Some(resolved) = resolve_defaults(s)? else {
                return Ok(None);
            };
            // The variable may be set to a valid value when Docker Compose starts the proxy.
            Ok(published_short(&resolved).unwrap_or(None))
        }
        PhpValue::Array(items) => {
            if is_list(items) || !items.iter().any(|(k, _)| k.as_string() == "target") {
                return invalid();
            }
            let field = |name: &str| {
                items
                    .iter()
                    .find(|(k, _)| k.as_string() == name)
                    .map(|(_, v)| v.clone())
            };
            let mut unresolved = false;
            let mut resolved_field =
                |name: &str,
                 check: fn(&str) -> Result<(), InvalidConfiguration>,
                 placeholder: Option<PhpValue>|
                 -> Result<Option<PhpValue>, InvalidConfiguration> {
                    let value = field(name);
                    let Some(PhpValue::Str(s)) = &value else {
                        return Ok(value);
                    };
                    if !VARIABLE.is_match(s) {
                        return Ok(value);
                    }
                    if let Some(r) = resolve_defaults(s)?
                        && check(&r).is_ok()
                    {
                        return Ok(Some(PhpValue::Str(r)));
                    }
                    unresolved = true;
                    Ok(placeholder)
                };
            let target = resolved_field("target", check_port, Some(PhpValue::Int(1)))?;
            let published = resolved_field("published", check_port, Some(PhpValue::Int(1)))?;
            let host_ip = resolved_field("host_ip", validate_host_ip, None)?;
            let protocol = resolved_field("protocol", validate_protocol, None)?;
            match protocol {
                None | Some(PhpValue::Null) => {}
                Some(PhpValue::Str(p)) => validate_protocol(&p)?,
                Some(_) => return invalid(),
            }
            match host_ip {
                None => {}
                Some(PhpValue::Str(ip)) => validate_host_ip(&ip)?,
                Some(_) => return invalid(),
            }
            let target_port = match target {
                Some(PhpValue::Int(i)) => port_number_int(i)?,
                Some(PhpValue::Str(s)) => port_number_str(&s)?,
                _ => return invalid(),
            };
            let published_port = match published {
                Some(v) => port_or_range(&v)?,
                None => Some(target_port),
            };
            Ok(if unresolved { None } else { published_port })
        }
        PhpValue::Int(i) => port_number_int(*i).map(Some),
        PhpValue::Str(s) => published_short(s),
        _ => invalid(),
    }
}

fn configured_ports(configuration: &str) -> Result<Vec<PhpValue>, InvalidConfiguration> {
    let parsed = parse_without_tags(configuration).map_err(|_| {
        InvalidConfiguration("The proxy configuration must contain valid YAML.".to_string())
    })?;
    if !matches!(parsed, PhpValue::Array(_)) {
        return Ok(Vec::new());
    }
    let mut ports = Vec::new();
    for service in ["traefik", "caddy"] {
        match get(&parsed, &["services", service, "ports"]) {
            None | Some(PhpValue::Null) => {}
            Some(PhpValue::Array(items)) if is_list(items) => {
                ports.extend(items.iter().map(|(_, v)| v.clone()));
            }
            _ => return invalid(),
        }
    }
    Ok(ports)
}

/// `ProxyPortParser::fromConfiguration()`: unique fixed host ports.
pub fn ports_from_configuration(configuration: &str) -> Result<Vec<i64>, InvalidConfiguration> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for p in configured_ports(configuration)? {
        if let Some(port) = published_port(&p)?
            && seen.insert(port)
        {
            out.push(port);
        }
    }
    Ok(out)
}

/// `ProxyPortParser::publishesOnlyUncheckedPorts()`.
pub fn publishes_only_unchecked_ports(configuration: &str) -> Result<bool, InvalidConfiguration> {
    Ok(!configured_ports(configuration)?.is_empty()
        && ports_from_configuration(configuration)?.is_empty())
}

/// `base64_encode` + `md5`, the hashes stored on the proxy attribute.
pub fn config_hash(configuration: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(configuration);
    let digest = Md5::digest(b64.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s)
}

/// `SaveProxyConfiguration::ensureDefaultContainerName`.
fn ensure_default_container_name(configuration: &str) -> Result<(), InvalidConfiguration> {
    let parsed =
        parse_without_tags(configuration).map_err(|e| InvalidConfiguration(e.message()))?;
    let Some(services) = get(&parsed, &["services"]) else {
        return Ok(());
    };
    for service in ["traefik", "caddy"] {
        let Some(s) = get(services, &[service]) else {
            continue;
        };
        let name = get(s, &["container_name"]);
        if !matches!(name, Some(PhpValue::Str(n)) if n == "coolify-proxy") {
            return Err(InvalidConfiguration(format!(
                "The {service} container_name must be coolify-proxy. Coolify uses this name to manage the proxy."
            )));
        }
    }
    Ok(())
}

/// What `SaveProxyConfiguration::run` can fail with.
#[derive(Debug)]
pub enum SaveError {
    Invalid(InvalidConfiguration),
    Other(String),
}

impl From<String> for SaveError {
    fn from(e: String) -> Self {
        Self::Other(e)
    }
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(e) => f.write_str(&e.0),
            Self::Other(e) => f.write_str(e),
        }
    }
}

/// `SaveProxyConfiguration::handle`.
pub async fn save_proxy_configuration(srv: &mut Srv, configuration: &str) -> Result<(), SaveError> {
    ports_from_configuration(configuration).map_err(SaveError::Invalid)?;
    if !srv.info.is_swarm() {
        ensure_default_container_name(configuration).map_err(SaveError::Invalid)?;
    }
    let proxy_path = srv.proxy_path();
    let encoded = b64(configuration);
    let new_hash = config_hash(configuration);
    let old_hash = srv
        .proxy
        .get("last_saved_settings")
        .and_then(Value::as_str)
        .filter(|h| !h.is_empty())
        .map(str::to_string);
    let changed = old_hash.as_deref().is_some_and(|h| h != new_hash);
    srv.proxy_set("last_saved_settings", json!(new_hash));
    srv.proxy_set("last_saved_proxy_configuration", json!(configuration));
    srv.save_proxy().await?;
    let backup_path = format!("{proxy_path}/backups");
    let mut commands = vec![format!("mkdir -p {proxy_path}")];
    if let (true, Some(old)) = (changed, &old_hash) {
        let short: String = old.chars().take(8).collect();
        let timestamp = crate::Jobs::now().format("%Y-%m-%d_%H-%M-%S");
        let backup_file = format!("docker-compose.{timestamp}.{short}.yml");
        commands.push(format!("mkdir -p {backup_path}"));
        commands.push(format!(
            "if [ -z \"$(find {backup_path} -maxdepth 1 -name 'docker-compose.*.{short}.yml')\" ]; then"
        ));
        commands.push(format!(
            "    cp -f {proxy_path}/docker-compose.yml {backup_path}/{backup_file} 2>/dev/null || true"
        ));
        commands.push("fi".into());
        commands.push(format!(
            "find {backup_path} -maxdepth 1 -name 'docker-compose.*.yml' | sort -r | tail -n +11 | xargs -r rm -f"
        ));
    }
    commands.push(format!(
        "echo '{encoded}' | base64 -d | tee {proxy_path}/docker-compose.yml > /dev/null"
    ));
    srv.run(&commands, "proxy.save_configuration").await?;
    Ok(())
}

fn config_matches_proxy_type(proxy_type: Option<&str>, configuration: &str) -> bool {
    let Ok(parsed) = parse_without_tags(configuration) else {
        return true;
    };
    let services = get(&parsed, &["services"]);
    let has = |name: &str| {
        services.is_some_and(|s| !matches!(get(s, &[name]), None | Some(PhpValue::Null)))
    };
    match proxy_type {
        Some("TRAEFIK") => has("traefik"),
        Some("CADDY") => has("caddy"),
        Some("NGINX") => has("nginx"),
        _ => true,
    }
}

fn blank_trim(s: Option<&str>) -> bool {
    s.is_none_or(|v| coolify_remote::text::php_trim(v).is_empty() || v == "0")
}

/// `GetProxyConfiguration::backfillFromDisk`.
async fn backfill_from_disk(srv: &mut Srv) -> Result<Option<String>, String> {
    let proxy_path = srv.proxy_path();
    let path = super::shell_arg(&format!("{proxy_path}/docker-compose.yml"));
    let result = srv
        .run_quiet(
            &[
                format!("mkdir -p {proxy_path}"),
                format!("if [ -f {path} ]; then"),
                format!(
                    "    if [ \"$(stat -c %s {path})\" -gt {MAX_CONFIGURATION_SIZE_BYTES} ]; then"
                ),
                "        echo '__COOLIFY_PROXY_CONFIG_TOO_LARGE__'".to_string(),
                "    else".to_string(),
                format!(
                    "        head -c {} {path}",
                    MAX_CONFIGURATION_SIZE_BYTES + 1
                ),
                "    fi".to_string(),
                "fi".to_string(),
            ],
            "proxy.backfill_configuration",
        )
        .await;
    if result.as_deref() == Some("__COOLIFY_PROXY_CONFIG_TOO_LARGE__")
        || result
            .as_deref()
            .is_some_and(|r| r.len() > MAX_CONFIGURATION_SIZE_BYTES)
    {
        return Err("Proxy configuration exceeds the 5 MiB size limit.".to_string());
    }
    if !blank_trim(result.as_deref()) {
        let text = result.unwrap_or_default();
        ports_from_configuration(&text).map_err(|e| e.0)?;
        srv.proxy_set("last_saved_proxy_configuration", json!(text));
        srv.save_proxy().await?;
        return Ok(Some(text));
    }
    Ok(None)
}

/// `GetProxyConfiguration::handle`.
pub async fn get_proxy_configuration(
    srv: &mut Srv,
    force_regenerate: bool,
) -> Result<String, String> {
    if srv.proxy_type() == Some("NONE") {
        return Ok("OK".to_string());
    }
    let mut configuration: Option<String> = None;
    if !force_regenerate {
        configuration = srv
            .proxy
            .get("last_saved_proxy_configuration")
            .and_then(Value::as_str)
            .map(str::to_string);
        if !blank_trim(configuration.as_deref())
            && !config_matches_proxy_type(srv.proxy_type(), configuration.as_deref().unwrap_or(""))
        {
            configuration = None;
        }
        if blank_trim(configuration.as_deref()) {
            configuration = backfill_from_disk(srv).await?;
        }
        if !blank_trim(configuration.as_deref())
            && remove_legacy_traefik_dashboard_exposure(srv).await?
        {
            configuration = srv
                .proxy
                .get("last_saved_proxy_configuration")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
    }
    if force_regenerate || blank_trim(configuration.as_deref()) {
        let custom = configuration
            .as_deref()
            .filter(|c| !blank_trim(Some(c)))
            .map(|c| extract_custom_proxy_commands(srv.proxy_type(), c))
            .unwrap_or_default();
        let generated = generate_default_proxy_configuration(srv, &custom)
            .await
            .map_err(|e| e.to_string())?;
        configuration = generated.map(|g| coolify_remote::text::php_trim(&g).to_string());
    }
    match configuration {
        Some(c) if !c.is_empty() && c != "0" => Ok(c),
        _ => Err("Could not get or generate proxy configuration".to_string()),
    }
}

const LEGACY_LABELS: [&str; 4] = [
    "traefik.enable=true",
    "traefik.http.routers.traefik.entrypoints=http",
    "traefik.http.routers.traefik.service=api@internal",
    "traefik.http.services.traefik.loadbalancer.server.port=8080",
];

/// `removeLegacyTraefikDashboardLabels`: a line edit inside the traefik service block,
/// kept only when the file parses to the expected labels.
pub fn remove_legacy_traefik_dashboard_labels(configuration: &str) -> String {
    let Ok(parsed) = parse_without_tags(configuration) else {
        return configuration.to_string();
    };
    let mut expected = parsed.clone();
    let mut changed = false;
    for path in [
        &["services", "traefik", "labels"][..],
        &["services", "traefik", "deploy", "labels"][..],
    ] {
        let Some(PhpValue::Array(items)) = get(&parsed, path) else {
            continue;
        };
        if !is_list(items) || !items.iter().all(|(_, v)| matches!(v, PhpValue::Str(_))) {
            continue;
        }
        let labels: Vec<String> = items
            .iter()
            .filter_map(|(_, v)| match v {
                PhpValue::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        let traefik: Vec<&String> = labels
            .iter()
            .filter(|l| l.starts_with("traefik."))
            .collect();
        if !traefik
            .iter()
            .any(|l| *l == "traefik.http.routers.traefik.service=api@internal")
            || traefik.iter().any(|l| !LEGACY_LABELS.contains(&l.as_str()))
        {
            continue;
        }
        let new: Vec<String> = labels
            .iter()
            .filter_map(|l| {
                if l == "traefik.enable=true" {
                    Some("traefik.enable=false".to_string())
                } else if LEGACY_LABELS.contains(&l.as_str()) {
                    None
                } else {
                    Some(l.clone())
                }
            })
            .collect();
        set_path(&mut expected, path, PhpValue::strings(new));
        changed = true;
    }
    if !changed {
        return configuration.to_string();
    }
    let Some((start, length)) = traefik_service_block_range(configuration) else {
        return configuration.to_string();
    };
    let block = &configuration[start..start + length];
    let enable = re(r#"(?m)^([ \t]*-[ \t]*)(['"]?)traefik\.enable=true(['"]?[ \t]*)$"#);
    let mut edited = enable
        .replace_all(block, "${1}${2}traefik.enable=false$3")
        .to_string();
    for label in &LEGACY_LABELS[1..] {
        let p = format!(
            r#"(?m)^[ \t]*-[ \t]*['"]?{}['"]?[ \t]*(?:\r?\n|\z)"#,
            regex::escape(label)
        );
        edited = re(&p).replace_all(&edited, "").to_string();
    }
    let fixed = format!(
        "{}{}{}",
        &configuration[..start],
        edited,
        &configuration[start + length..]
    );
    match parse_without_tags(&fixed) {
        Ok(v) if v == expected => fixed,
        _ => configuration.to_string(),
    }
}

fn set_path(v: &mut PhpValue, path: &[&str], value: PhpValue) {
    let mut cur = v;
    for p in &path[..path.len() - 1] {
        let Some(next) = cur.get_mut(p) else {
            return;
        };
        cur = next;
    }
    if let Some(last) = path.last() {
        cur.set(last, value);
    }
}

/// `traefikServiceBlockRange`.
fn traefik_service_block_range(c: &str) -> Option<(usize, usize)> {
    let services = re(r"(?m)^services:[ \t]*(?:#.*)?$").find(c)?;
    let offset = services.end();
    let rest = &c[offset..];
    let first = re(r"(?m)^([ \t]+)[^\s#]").captures(rest)?;
    let indent = regex::escape(first.get(1)?.as_str());
    let traefik_re = re(&format!(
        r#"(?m)^{indent}['"]?traefik['"]?:[ \t]*(?:#.*)?(?:\r?\n|\z)"#
    ));
    let t = traefik_re.find(rest)?;
    let start = offset + t.end();
    // The block ends at the next content line that is not indented deeper than the service key.
    let body = &c[start..];
    let mut pos = 0;
    let deeper = re(&format!(r"^{indent}[ \t]"));
    let mut end = c.len();
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_start_matches([' ', '\t']);
        let content = !trimmed.trim().is_empty() && !trimmed.starts_with('#');
        if content && !deeper.is_match(line) {
            end = start + pos;
            break;
        }
        pos += line.len();
    }
    Some((start, end - start))
}

/// `removeLegacyTraefikDashboardExposure`.
async fn remove_legacy_traefik_dashboard_exposure(srv: &mut Srv) -> Result<bool, String> {
    if srv.proxy_type() != Some("TRAEFIK") {
        return Ok(false);
    }
    let Some(configuration) = srv
        .proxy
        .get("last_saved_proxy_configuration")
        .and_then(Value::as_str)
        .filter(|c| !c.trim().is_empty())
        .map(str::to_string)
    else {
        return Ok(false);
    };
    let fixed = remove_legacy_traefik_dashboard_labels(&configuration);
    if fixed == configuration {
        return Ok(false);
    }
    let applied_blank = srv
        .proxy
        .get("last_applied_settings")
        .and_then(Value::as_str)
        .is_none_or(|a| a.trim().is_empty());
    if applied_blank {
        srv.proxy_set("last_applied_settings", json!(config_hash(&configuration)));
    }
    srv.proxy_set("last_saved_settings", json!(config_hash(&fixed)));
    srv.proxy_set("last_saved_proxy_configuration", json!(fixed));
    srv.save_proxy().await?;
    Ok(true)
}

/// `extractCustomProxyCommands`.
pub fn extract_custom_proxy_commands(proxy_type: Option<&str>, existing: &str) -> Vec<String> {
    if proxy_type != Some("TRAEFIK") || existing.is_empty() {
        return Vec::new();
    }
    const DEFAULT_PREFIXES: [&str; 15] = [
        "--ping=",
        "--api.",
        "--entrypoints.http.address=",
        "--entrypoints.https.address=",
        "--entrypoints.http.http.encodequerysemicolons=",
        "--entryPoints.http.http2.maxConcurrentStreams=",
        "--entrypoints.https.http.encodequerysemicolons=",
        "--entryPoints.https.http2.maxConcurrentStreams=",
        "--entrypoints.https.http3",
        "--providers.file.",
        "--certificatesresolvers.",
        "--providers.docker",
        "--providers.swarm",
        "--log.level=",
        "--accesslog.",
    ];
    let Ok(parsed) = parse_without_tags(existing) else {
        return Vec::new();
    };
    let Some(PhpValue::Array(items)) = get(&parsed, &["services", "traefik", "command"]) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|(_, v)| match v {
            PhpValue::Str(s) if !DEFAULT_PREFIXES.iter().any(|p| s.starts_with(p)) => {
                Some(s.clone())
            }
            _ => None,
        })
        .collect()
}

/// `traefikAccessLogCommands(true)`.
/// The `traefik-logrotate` service of `applyTrafficAnalyticsToProxyConfigArray`.
pub(crate) fn traefik_logrotate_service(proxy_path: &str) -> PhpValue {
    PhpValue::map([
        (
            "container_name",
            PhpValue::Str(TRAEFIK_LOGROTATE_CONTAINER.into()),
        ),
        ("image", PhpValue::Str("alpine:3.24".into())),
        ("restart", PhpValue::Str("unless-stopped".into())),
        ("network_mode", PhpValue::Str("none".into())),
        (
            "volumes",
            PhpValue::strings([format!("{proxy_path}:/traefik")]),
        ),
        ("labels", PhpValue::strings(["coolify.managed=true"])),
        (
            "entrypoint",
            PhpValue::strings([
                "/bin/sh".to_string(),
                "-c".to_string(),
                ROTATION_SCRIPT.replace('$', "$$"),
            ]),
        ),
    ])
}

pub(crate) fn traefik_access_log_commands() -> Vec<String> {
    [
        "--accesslog=true",
        "--accesslog.filepath=/traefik/access.log",
        "--accesslog.format=json",
        "--accesslog.fields.headers.names.Cf-Connecting-Ip=keep",
        "--accesslog.fields.headers.names.Cf-Ipcountry=keep",
        "--accesslog.fields.headers.names.Cf-Cache-Status=keep",
        "--accesslog.fields.headers.names.Cf-Verified-Bot=keep",
        "--accesslog.fields.headers.names.Cf-Ray=keep",
        "--accesslog.fields.headers.names.X-Forwarded-For=keep",
        "--accesslog.fields.headers.names.User-Agent=keep",
        "--accesslog.fields.headers.names.Referer=keep",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

/// `traefikAccessLogRotationScript()`.
const ROTATION_SCRIPT: &str = r#"log="${TRAEFIK_ACCESS_LOG:-/traefik/access.log}"
max_bytes="${TRAEFIK_ACCESS_LOG_MAX_BYTES:-20971520}"
interval="${TRAEFIK_ACCESS_LOG_ROTATE_INTERVAL:-60}"
keep=5
while true; do
  size=$(stat -c %s "$log" 2>/dev/null || echo 0)
  if [ "$size" -gt "$max_bytes" ]; then
    rm -f "$log.$keep.gz"
    i=$((keep - 1))
    while [ "$i" -ge 1 ]; do
      if [ -f "$log.$i.gz" ]; then mv -f "$log.$i.gz" "$log.$((i + 1)).gz"; fi
      i=$((i - 1))
    done
    if cp "$log" "$log.1"; then
      : > "$log"
      gzip -f "$log.1"
    fi
  fi
  sleep "$interval"
done"#;

/// `generateDefaultProxyConfiguration`.
pub async fn generate_default_proxy_configuration(
    srv: &mut Srv,
    custom_commands: &[String],
) -> Result<Option<String>, SaveError> {
    let proxy_path = srv.proxy_path();
    let swarm = srv.info.is_swarm();
    let mut networks: Vec<String> = Vec::new();
    for n in srv.destination_networks().await? {
        if !networks.contains(&n) {
            networks.push(n);
        }
    }
    if networks.is_empty() {
        networks.push(if swarm { "coolify-overlay" } else { "coolify" }.to_string());
    }
    let usable: Vec<String> = networks
        .into_iter()
        .filter(|n| n != "default" && n != "host")
        .collect();
    let network_map = PhpValue::map(usable.iter().map(|n| {
        (
            n.clone(),
            PhpValue::map([("external", PhpValue::Bool(true))]),
        )
    }));
    let enabled = srv.info.setting_bool("is_traffic_analytics_enabled") == Some(true);
    let proxy_type = srv.proxy_type().map(str::to_string);
    let config = match proxy_type.as_deref() {
        Some("TRAEFIK") => {
            let labels = PhpValue::strings([
                "traefik.enable=false",
                "coolify.managed=true",
                "coolify.proxy=true",
            ]);
            let mut command: Vec<String> = [
                "--ping=true",
                "--ping.entrypoint=http",
                "--api.dashboard=true",
                "--entrypoints.http.address=:80",
                "--entrypoints.https.address=:443",
                "--entrypoints.http.http.encodequerysemicolons=true",
                "--entryPoints.http.http2.maxConcurrentStreams=250",
                "--entrypoints.https.http.encodequerysemicolons=true",
                "--entryPoints.https.http2.maxConcurrentStreams=250",
                "--entrypoints.https.http3",
                "--providers.file.directory=/traefik/dynamic/",
                "--providers.file.watch=true",
                "--certificatesresolvers.letsencrypt.acme.httpchallenge=true",
                "--certificatesresolvers.letsencrypt.acme.httpchallenge.entrypoint=http",
                "--certificatesresolvers.letsencrypt.acme.storage=/traefik/acme.json",
                "--api.insecure=false",
            ]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
            if swarm {
                command.push("--providers.swarm.endpoint=unix:///var/run/docker.sock".into());
                command.push("--providers.swarm.exposedbydefault=false".into());
            } else {
                command.push("--providers.docker=true".into());
                command.push("--providers.docker.exposedbydefault=false".into());
            }
            command.extend(custom_commands.iter().cloned());
            // applyTraefikAccessLogCommands on a generated list.
            let stored = srv
                .proxy
                .get("traffic_analytics_user_accesslog_commands")
                .and_then(Value::as_array)
                .cloned();
            let managed = traefik_access_log_commands();
            if stored.is_some() || managed.iter().all(|m| command.contains(m)) {
                command.retain(|c| !managed.contains(c));
            }
            let is_log_flag = |c: &str| {
                let flag = c.split('=').next().unwrap_or_default().to_lowercase();
                flag == "--accesslog" || flag.starts_with("--accesslog.")
            };
            if enabled {
                let mut user: Vec<Value> = stored.clone().unwrap_or_default();
                for c in command.iter().filter(|c| is_log_flag(c)) {
                    if !user.iter().any(|u| u.as_str() == Some(c)) {
                        user.push(json!(c));
                    }
                }
                command.retain(|c| !is_log_flag(c));
                command.extend(managed);
                if stored.as_ref() != Some(&user) {
                    srv.proxy_set(
                        "traffic_analytics_user_accesslog_commands",
                        Value::Array(user),
                    );
                    srv.save_proxy().await?;
                }
            } else if let Some(user) = stored {
                for c in user.iter().filter_map(Value::as_str) {
                    if !command.iter().any(|x| x == c) {
                        command.push(c.to_string());
                    }
                }
                srv.proxy
                    .shift_remove("traffic_analytics_user_accesslog_commands");
                srv.save_proxy().await?;
            }
            let mut service: Vec<(&str, PhpValue)> = Vec::new();
            if !swarm {
                service.push(("container_name", PhpValue::Str("coolify-proxy".into())));
            }
            service.push(("image", PhpValue::Str("traefik:v3.7".into())));
            if !swarm {
                service.push(("restart", PhpValue::Str("unless-stopped".into())));
            }
            service.push((
                "extra_hosts",
                PhpValue::strings(["host.docker.internal:host-gateway"]),
            ));
            service.push(("networks", PhpValue::strings(usable.clone())));
            service.push((
                "ports",
                PhpValue::strings(["80:80", "443:443", "443:443/udp", "8080:8080"]),
            ));
            service.push((
                "healthcheck",
                PhpValue::map([
                    (
                        "test",
                        PhpValue::Str("wget -qO- http://localhost:80/ping || exit 1".into()),
                    ),
                    ("interval", PhpValue::Str("4s".into())),
                    ("timeout", PhpValue::Str("2s".into())),
                    ("retries", PhpValue::Int(5)),
                ]),
            ));
            service.push((
                "volumes",
                PhpValue::strings([
                    "/var/run/docker.sock:/var/run/docker.sock:ro".to_string(),
                    format!("{proxy_path}:/traefik"),
                ]),
            ));
            service.push(("command", PhpValue::strings(command)));
            if swarm {
                service.push((
                    "deploy",
                    PhpValue::map([
                        ("labels", labels.clone()),
                        (
                            "placement",
                            PhpValue::map([(
                                "constraints",
                                PhpValue::strings(["node.role==manager"]),
                            )]),
                        ),
                    ]),
                ));
            } else {
                service.push(("labels", labels));
            }
            let mut services = vec![("traefik".to_string(), PhpValue::map(service))];
            if enabled && !swarm {
                services.push((
                    "traefik-logrotate".to_string(),
                    traefik_logrotate_service(&proxy_path),
                ));
            }
            PhpValue::map([
                ("name", PhpValue::Str("coolify-proxy".into())),
                ("networks", network_map),
                ("services", PhpValue::map(services)),
            ])
        }
        Some("CADDY") => {
            let mut volumes = vec![
                "/var/run/docker.sock:/var/run/docker.sock:ro".to_string(),
                format!("{proxy_path}/dynamic:/dynamic"),
                format!("{proxy_path}/config:/config"),
                format!("{proxy_path}/data:/data"),
            ];
            if enabled {
                volumes.push(format!("{}:/traffic", sentinel::traffic_log_directory(srv)));
            }
            PhpValue::map([
                ("networks", network_map),
                (
                    "services",
                    PhpValue::map([(
                        "caddy",
                        PhpValue::map([
                            ("container_name", PhpValue::Str("coolify-proxy".into())),
                            ("image", PhpValue::Str(RECOMMENDED_CADDY_PROXY_IMAGE.into())),
                            ("restart", PhpValue::Str("unless-stopped".into())),
                            (
                                "extra_hosts",
                                PhpValue::strings(["host.docker.internal:host-gateway"]),
                            ),
                            (
                                "environment",
                                PhpValue::strings([
                                    "CADDY_DOCKER_POLLING_INTERVAL=5s",
                                    "CADDY_DOCKER_CADDYFILE_PATH=/dynamic/Caddyfile",
                                ]),
                            ),
                            ("networks", PhpValue::strings(usable.clone())),
                            (
                                "ports",
                                PhpValue::strings(["80:80", "443:443", "443:443/udp"]),
                            ),
                            (
                                "labels",
                                PhpValue::strings(["coolify.managed=true", "coolify.proxy=true"]),
                            ),
                            ("volumes", PhpValue::strings(volumes)),
                        ]),
                    )]),
                ),
            ])
        }
        _ => return Ok(None),
    };
    let text = yaml::dump_with_indent(&config, 12, 2);
    save_proxy_configuration(srv, &text).await?;
    Ok(Some(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports() {
        let c = "services:\n  traefik:\n    ports:\n      - '80:80'\n      - '443:443/udp'\n      - '${X:-8080}:8080'\n      - '${Y}:90'\n      - 10000-10100:1000\n";
        assert_eq!(ports_from_configuration(c).unwrap(), vec![80, 443, 8080]);
        assert!(ports_from_configuration("services:\n  traefik:\n    ports: 80\n").is_err());
        assert!(ports_from_configuration("a: [").is_err());
        assert!(
            publishes_only_unchecked_ports("services:\n  caddy:\n    ports:\n      - '${Y}:90'\n")
                .unwrap()
        );
    }

    #[test]
    fn merge_tags_are_ignored_for_reading() {
        let c = "services:\n  traefik:\n    ports: !override\n      - '80:80'\n";
        assert_eq!(ports_from_configuration(c).unwrap(), vec![80]);
    }

    #[test]
    fn legacy_labels() {
        let c = "services:\n  traefik:\n    labels:\n      - traefik.enable=true\n      - traefik.http.routers.traefik.entrypoints=http\n      - traefik.http.routers.traefik.service=api@internal\n      - coolify.managed=true\n";
        assert_eq!(
            remove_legacy_traefik_dashboard_labels(c),
            "services:\n  traefik:\n    labels:\n      - traefik.enable=false\n      - coolify.managed=true\n"
        );
    }
}
