//! `serviceParser(Service $resource)` (bootstrap/helpers/parsers.php): parses `docker_compose_raw`,
//! creates the service applications and databases, the environment variables and the storages,
//! saves `docker_compose` and returns the compose structure. Also `updateCompose()`
//! (bootstrap/helpers/services.php) and the model methods the parser calls.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use chrono::NaiveDateTime;
use coolify_compat::Encrypter;
use coolify_compat::yaml::{Key, PhpValue};
use coolify_db::Db;
use coolify_db::compose::{self as store, EnvRow, EnvUpdate, NewEnv};
use coolify_db::servers::ColumnValue;
use coolify_db::services::{self as svc, MORPH, ServiceRow, SubKind, SubRow};
use serde_json::Value;

use super::ParsedCompose;
use super::domains::{self, UrlBase};
use super::env;
use super::labels::{self, ProxyLabelArgs};
use super::php::{self, Arr, SpatieUrl};
use super::service as service_helpers;
use super::validate;
use super::volumes;
use crate::Jobs;
use crate::applications::domains as app_domains;
use crate::services::templates;

const RESTART_MODE: &str = "unless-stopped";
const PHP_TRIM: [char; 6] = [' ', '\t', '\n', '\r', '\0', '\x0B'];

/// `SPECIFIC_SERVICES`.
const SPECIFIC_SERVICES: [&str; 8] = [
    "quay.io/minio/minio",
    "minio/minio",
    "ghcr.io/coollabsio/minio",
    "coollabsio/minio",
    "pgsty/silo",
    "docker.io/pgsty/silo",
    "svhd/logto",
    "dxflrs/garage",
];

static ENV_KEY: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\A[A-Za-z_][A-Za-z0-9_.]*\z").unwrap_or_else(|_| unreachable!())
});
static SHARED_REFERENCE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?s)^\{\{\s*(?:team|project|environment|server)\..*\}\}$")
        .unwrap_or_else(|_| unreachable!())
});
static MAGIC_REFERENCE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\$(\{?([a-zA-Z_\x{80}-\x{10FFFF}][a-zA-Z0-9_\x{80}-\x{10FFFF}]*)\}?)")
        .unwrap_or_else(|_| unreachable!())
});
static SHARED_PLACEHOLDER: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\{\{(.*?)\}\}").unwrap_or_else(|_| unreachable!()));
static LONG_BIND_DEFAULT: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^\$\{([A-Za-z_][A-Za-z0-9_]*):-[^}]*\}\n?\z")
        .unwrap_or_else(|_| unreachable!())
});
static YAML_ENV_BLOCK: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^(\s*)environment\s*:\s*$").unwrap_or_else(|_| unreachable!())
});
static YAML_ENV_INLINE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^(\s*)environment\s*:\s*\{").unwrap_or_else(|_| unreachable!())
});
static YAML_KEY: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[a-zA-Z_][a-zA-Z0-9_]*\s*:").unwrap_or_else(|_| unreachable!())
});
static YAML_NAME: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^([A-Za-z_][A-Za-z0-9_]*)").unwrap_or_else(|_| unreachable!())
});
static YAML_MAP_KEY: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^([A-Za-z_][A-Za-z0-9_]*)\s*:").unwrap_or_else(|_| unreachable!())
});
static YAML_UNQUOTED_COMMENT: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\s+#\s*(.*)$").unwrap_or_else(|_| unreachable!()));

/// `extractYamlEnvironmentComments($rawYaml)`.
pub fn extract_yaml_environment_comments(raw: &str) -> BTreeMap<String, String> {
    let mut comments = BTreeMap::new();
    let mut in_block = false;
    let mut env_indent = 0usize;
    for line in raw.split('\n') {
        if line.trim_matches(PHP_TRIM).is_empty() {
            continue;
        }
        let current = line.len() - line.trim_start_matches(PHP_TRIM).len();
        if let Some(c) = YAML_ENV_BLOCK.captures(line) {
            in_block = true;
            env_indent = c[1].len();
            continue;
        }
        if YAML_ENV_INLINE.is_match(line) {
            continue;
        }
        if in_block {
            let trimmed = line.trim_start_matches(PHP_TRIM);
            if current <= env_indent
                && !trimmed.starts_with('-')
                && !trimmed.starts_with('#')
                && YAML_KEY.is_match(trimmed)
            {
                in_block = false;
                continue;
            }
            if trimmed.starts_with('#') {
                continue;
            }
            if let Some((key, Some(comment))) = extract_env_var_comment(trimmed) {
                comments.insert(key, comment);
            }
        }
    }
    comments
}

/// `extractEnvVarCommentFromYamlLine($line)`.
fn extract_env_var_comment(line: &str) -> Option<(String, Option<String>)> {
    if let Some(rest) = line.strip_prefix('-') {
        let content = rest.trim_start_matches(PHP_TRIM);
        let m = YAML_NAME.captures(content)?;
        return Some((m[1].to_owned(), extract_comment_after_value(content)));
    }
    let m = YAML_MAP_KEY.captures(line)?;
    let after = &line[m[0].len()..];
    Some((m[1].to_owned(), extract_comment_after_value(after)))
}

/// `extractCommentAfterValue($valueAndComment)`.
fn extract_comment_after_value(v: &str) -> Option<String> {
    let v = v.trim_start_matches(PHP_TRIM);
    let bytes = v.as_bytes();
    let first = *bytes.first()?;
    if first == b'#' {
        let c = v[1..].trim_matches(PHP_TRIM);
        return (!c.is_empty()).then(|| c.to_owned());
    }
    if first == b'"' {
        let mut pos = 1;
        while pos < bytes.len() {
            if bytes[pos] == b'\\' && pos + 1 < bytes.len() {
                pos += 2;
                continue;
            }
            if bytes[pos] == b'"' {
                return comment_from_remainder(v.get(pos + 1..).unwrap_or_default());
            }
            pos += 1;
        }
        return None;
    }
    if first == b'\'' {
        let close = v[1..].find('\'')? + 1;
        return comment_from_remainder(&v[close + 1..]);
    }
    let c = YAML_UNQUOTED_COMMENT.captures(v)?;
    let c = c[1].trim_matches(PHP_TRIM);
    (!c.is_empty()).then(|| c.to_owned())
}

/// `extractCommentFromRemainder($remainder)`.
fn comment_from_remainder(remainder: &str) -> Option<String> {
    let pos = remainder.find('#')?;
    let c = remainder[pos + 1..].trim_matches(PHP_TRIM);
    (!c.is_empty()).then(|| c.to_owned())
}

/// The server the service runs on, as the parser reads it.
pub(crate) struct ServerInfo {
    pub ip: String,
    pub id: i64,
    pub settings: Value,
    pub proxy: Value,
    pub url_base: UrlBase,
}

impl ServerInfo {
    fn proxy_type(&self) -> Option<&str> {
        self.proxy.get("type").and_then(Value::as_str)
    }
    fn setting_bool(&self, key: &str) -> bool {
        self.settings
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
    /// `Server::traefikCertificateResolver($resourceResolver)`.
    fn certificate_resolver(&self, resource: Option<&str>) -> Option<String> {
        let resolver = match resource.filter(|r| !r.trim().is_empty()) {
            Some(r) => Some(r.to_owned()),
            None => self
                .settings
                .get("traefik_certificate_resolver")
                .and_then(Value::as_str)
                .map(str::to_owned),
        };
        resolver.filter(|r| !r.trim().is_empty() && r != "none")
    }
}

/// What a parse leaves to do on the server: `ServerFilesFromServerJob` for each sub-resource with a
/// volume, in dispatch order (`dispatch(new ServerFilesFromServerJob($originalResource))`), and
/// `ServerStorageSaveJob` for the file volumes it created.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParseEffects {
    pub files_from_server: Vec<(SubKind, i64)>,
    pub new_file_volumes: Vec<i64>,
}

/// One run of the parser.
pub struct ServiceRun<'a> {
    db: &'a Db,
    enc: &'a Encrypter,
    now: NaiveDateTime,
    version: String,
    pub(crate) service: ServiceRow,
    docker_compose: Option<String>,
    docker_compose_raw: String,
    original: (Option<String>, String),
    pub(crate) server: ServerInfo,
    env_cache: Option<Vec<EnvRow>>,
    pub warnings: Vec<String>,
    pub effects: ParseEffects,
    /// `is_container_label_escape_enabled` of a model created in this request.
    pub label_escape_override: Option<bool>,
    comments: BTreeMap<String, String>,
}

impl std::fmt::Debug for ServiceRun<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceRun")
            .field("service", &self.service.uuid)
            .finish_non_exhaustive()
    }
}

fn db_err(e: coolify_db::DbError) -> String {
    e.to_string()
}

fn json_to_php(v: Option<&Value>) -> Option<PhpValue> {
    match v {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => php::json_decode(s),
        Some(other) => php::json_decode(&other.to_string()),
    }
}

fn overrides_map(v: Option<&PhpValue>) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::new();
    if let Some(PhpValue::Array(items)) = v {
        for (k, val) in items {
            let port = match val {
                PhpValue::Int(i) => Some(*i),
                PhpValue::Str(s) => s.parse().ok(),
                _ => None,
            };
            if let Some(p) = port {
                out.insert(k.as_string(), p);
            }
        }
    }
    out
}

/// The loose `==` of `$value == $networkName || $key == $networkName`.
fn network_matches(key: &Key, value: &PhpValue, name: &Key) -> bool {
    let to_value = |k: &Key| match k {
        Key::Int(i) => PhpValue::Int(*i),
        Key::Str(s) => PhpValue::Str(s.clone()),
    };
    let name_v = to_value(name);
    php::loose_eq(value, &name_v) || php::loose_eq(&to_value(key), &name_v)
}

/// `str($fqdn)->contains(':') && str($fqdn)->afterLast(':')->isMatch('/^\d+$/')`.
fn fqdn_has_port(fqdn: Option<&str>) -> bool {
    let f = fqdn.unwrap_or_default();
    if !f.contains(':') {
        return false;
    }
    let after = php::after_last(f, ":");
    let after = after.strip_suffix('\n').unwrap_or(&after);
    !after.is_empty() && after.bytes().all(|b| b.is_ascii_digit())
}

/// `firstDomainFromList($fqdns)`.
fn first_domain_from_list(fqdns: Option<&str>) -> String {
    fqdns
        .unwrap_or_default()
        .split(',')
        .next()
        .unwrap_or_default()
        .trim_matches(PHP_TRIM)
        .to_owned()
}

/// `ServiceApplication::serviceType()`.
fn service_type(image: Option<&str>) -> Option<&'static str> {
    let image = php::before(image.unwrap_or_default(), ":");
    SPECIFIC_SERVICES.iter().find(|s| **s == image).copied()
}

/// The declarations of a compose service: environment plus build args.
fn declarations(service: &PhpValue) -> Arr {
    env::convert_to_key_value_collection(&php::merge(
        &php::collect(php::get(service, "environment")),
        &php::collect(php::data_get(service, "build.args")),
    ))
}

/// Which SERVICE_URL/FQDN keys a service declares directly (`getRequiredPort()`, `updateCompose()`).
fn declared_url_names(service: &PhpValue) -> Vec<String> {
    let mut out = Vec::new();
    let Some(PhpValue::Array(items)) = php::get(service, "environment") else {
        return out;
    };
    for (k, v) in items {
        let name = match (k, v) {
            (Key::Int(_), PhpValue::Str(s)) => {
                php::before(s, "=").trim_matches(PHP_TRIM).to_owned()
            }
            (Key::Str(s), _) => s.clone(),
            _ => continue,
        };
        if name.starts_with("SERVICE_FQDN_") || name.starts_with("SERVICE_URL_") {
            out.push(name);
        }
    }
    out
}

/// `getSavedLegacyRoutingPort($serviceConfig)`.
fn saved_legacy_routing_port(
    saved_compose: Option<&str>,
    name: &str,
    config: &PhpValue,
) -> Option<i64> {
    static TRAEFIK_PORT: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^traefik\.http\.services\.[^.]+\.loadbalancer\.server\.port$")
            .unwrap_or_else(|_| unreachable!())
    });
    static CADDY_PROXY: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^caddy(?:_\d+)?\..*reverse_proxy$").unwrap_or_else(|_| unreachable!())
    });
    static UPSTREAMS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^\{\{upstreams ([0-9]+)\}\}$").unwrap_or_else(|_| unreachable!())
    });
    let saved = validate::parse_docker_compose_yaml(saved_compose.unwrap_or_default()).ok()?;
    let saved_service = php::get(php::get(&saved, "services")?, name)?;
    let image = match php::get(config, "image") {
        Some(PhpValue::Str(s)) if !s.is_empty() => s.clone(),
        _ => return None,
    };
    if php::get(saved_service, "image") != Some(&PhpValue::Str(image)) {
        return None;
    }
    let labels = match php::get(saved_service, "labels") {
        Some(PhpValue::Array(l)) => l.clone(),
        None => Vec::new(),
        _ => return None,
    };
    let mut ports: Vec<i64> = Vec::new();
    for (k, v) in labels {
        let (key, value) = match k {
            Key::Int(_) => {
                let PhpValue::Str(s) = v else { return None };
                match s.split_once('=') {
                    Some((a, b)) => (a.to_owned(), Some(PhpValue::Str(b.to_owned()))),
                    None => (s, None),
                }
            }
            Key::Str(s) => (s, Some(v)),
        };
        let port = if TRAEFIK_PORT.is_match(&key) {
            value
        } else if CADDY_PROXY.is_match(&key) {
            let Some(PhpValue::Str(text)) = value else {
                return None;
            };
            let c = UPSTREAMS.captures(&text)?;
            Some(PhpValue::Str(c[1].to_owned()))
        } else {
            continue;
        };
        let text = match port {
            Some(PhpValue::Str(s)) => s,
            Some(PhpValue::Int(i)) => i.to_string(),
            _ => return None,
        };
        if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let n: i64 = text.parse().ok()?;
        if !(1..=65535).contains(&n) {
            return None;
        }
        ports.push(n);
    }
    ports.dedup();
    let mut unique: Vec<i64> = Vec::new();
    for p in ports {
        if !unique.contains(&p) {
            unique.push(p);
        }
    }
    (unique.len() == 1).then(|| unique[0])
}

/// `ServiceApplication::getRequiredPort()`.
pub fn application_required_port(
    service_type: Option<&str>,
    compose_raw: Option<&str>,
    saved_compose: Option<&str>,
    name: &str,
) -> Option<i64> {
    let service_port = || templates::required_port(service_type);
    let raw = match compose_raw {
        Some(r) if php::truthy(&PhpValue::Str(r.to_owned())) => r,
        _ => return service_port(),
    };
    let Ok(compose) = validate::parse_docker_compose_yaml(raw) else {
        return None;
    };
    let Some(config) = php::get(&compose, "services").and_then(|s| php::get(s, name)) else {
        return service_port();
    };
    let mut declares = false;
    for declared in declared_url_names(config) {
        declares = true;
        let parsed = env::parse_service_environment_variable(&declared);
        if parsed.has_port
            && let Some(p) = parsed
                .port
                .as_deref()
                .filter(|p| php::truthy(&PhpValue::Str((*p).to_owned())))
        {
            return p.parse().ok();
        }
    }
    if let Some(p) = service_helpers::first_docker_compose_service_port(config) {
        return Some(p);
    }
    if declares {
        if service_type.is_none_or(|t| t.trim().is_empty())
            && let Some(p) = saved_legacy_routing_port(saved_compose, name, config)
        {
            return Some(p);
        }
        return service_port();
    }
    None
}

impl<'a> ServiceRun<'a> {
    /// Loads the service and its server.
    pub async fn load(
        jobs: &'a Jobs,
        service_id: i64,
        now: NaiveDateTime,
    ) -> Result<ServiceRun<'a>, String> {
        let db = &jobs.0.db;
        let enc = jobs
            .0
            .encrypter
            .as_ref()
            .ok_or("APP_KEY is not configured.")?;
        let service = svc::find_by_id(db, service_id)
            .await
            .map_err(db_err)?
            .ok_or_else(|| format!("Service {service_id} not found."))?;
        let server = load_server(db, &service).await?;
        let raw = service
            .str("docker_compose_raw")
            .unwrap_or_default()
            .to_owned();
        let compose = service.str("docker_compose").map(str::to_owned);
        Ok(ServiceRun {
            db,
            enc,
            now,
            version: jobs.version(),
            original: (compose.clone(), raw.clone()),
            docker_compose: compose,
            docker_compose_raw: raw,
            service,
            server,
            env_cache: None,
            warnings: Vec::new(),
            effects: ParseEffects::default(),
            label_escape_override: None,
            comments: BTreeMap::new(),
        })
    }

    fn uuid(&self) -> String {
        self.service.uuid.clone()
    }

    fn parsing_version(&self) -> i64 {
        self.service.parsing_version()
    }

    fn decrypt(&self, raw: Option<&str>) -> Option<String> {
        let raw = raw.filter(|r| !r.is_empty() && *r != "0")?;
        self.enc
            .decrypt_maybe_serialized(raw)
            .ok()
            .map(|v| v.trim_matches(PHP_TRIM).to_owned())
    }

    fn is_shared_reference(value: Option<&str>) -> bool {
        value.is_some_and(|v| {
            !v.trim().is_empty() && SHARED_REFERENCE.is_match(v.trim_matches(PHP_TRIM))
        })
    }

    fn comment(&self, key: &str) -> Option<String> {
        self.comments.get(key).cloned()
    }

    async fn env_first(&self, key: &str) -> Result<Option<EnvRow>, String> {
        store::env_first(self.db, MORPH, self.service.id, key, false)
            .await
            .map_err(db_err)
    }

    /// `firstOrCreate(['key' => ...], $values)`. `value`: `None` when not given or null.
    async fn env_first_or_create(
        &mut self,
        key: &str,
        value: Option<&str>,
        is_required: Option<bool>,
        comment: Option<String>,
    ) -> Result<EnvRow, String> {
        if let Some(row) = self.env_first(key).await? {
            return Ok(row);
        }
        self.env_create(key, value, is_required, comment).await
    }

    async fn env_create(
        &mut self,
        key: &str,
        value: Option<&str>,
        is_required: Option<bool>,
        comment: Option<String>,
    ) -> Result<EnvRow, String> {
        let key = key.trim_matches(PHP_TRIM);
        if !ENV_KEY.is_match(key) {
            return Err("The key must start with a letter or underscore and may only contain letters, numbers, underscores, and dots.".into());
        }
        let trimmed = value.map(|v| v.trim_matches(PHP_TRIM).to_owned());
        let new = NewEnv {
            key: key.to_owned(),
            value_encrypted: trimmed.as_deref().map(|v| self.enc.encrypt_serialized(v)),
            is_preview: false,
            is_required: is_required.unwrap_or(false),
            is_multiline: false,
            is_literal: false,
            is_shown_once: false,
            is_shared: Self::is_shared_reference(trimmed.as_deref()),
            comment,
        };
        let id = store::env_insert(
            self.db,
            MORPH,
            self.service.id,
            &new,
            &self.version,
            self.now,
        )
        .await
        .map_err(db_err)?;
        Ok(EnvRow {
            id,
            key: key.to_owned(),
            value_encrypted: new.value_encrypted,
            is_required: new.is_required,
            preserve_compose_null: false,
            is_preview: false,
            is_shared: new.is_shared,
        })
    }

    /// `updateOrCreate(['key' => ...], ['value' => ..., 'is_preview' => false, 'comment' => ...])`.
    async fn env_update_or_create(
        &mut self,
        key: &str,
        value: Option<&str>,
        comment: Option<String>,
    ) -> Result<(), String> {
        let Some(row) = self.env_first(key).await? else {
            self.env_create(key, value, None, comment).await?;
            return Ok(());
        };
        let new = value.map(|v| v.trim_matches(PHP_TRIM).to_owned());
        let old = row
            .value_encrypted
            .as_deref()
            .and_then(|c| self.enc.decrypt_maybe_serialized(c).ok());
        let mut u = EnvUpdate::default();
        if old != new {
            u.value_encrypted = Some(new.as_deref().map(|v| self.enc.encrypt_serialized(v)));
            if row.preserve_compose_null && new.is_some() {
                u.preserve_compose_null = Some(false);
            }
        }
        let shared = Self::is_shared_reference(new.as_deref());
        if shared != row.is_shared {
            u.is_shared = Some(shared);
        }
        store::env_update(self.db, row.id, &u, self.now)
            .await
            .map_err(db_err)?;
        svc::env_set_comment(self.db, row.id, comment.as_deref(), self.now)
            .await
            .map_err(db_err)
    }

    /// `$variable->update(['is_required' => $required])`.
    async fn env_set_required(&self, row: &EnvRow, required: bool) -> Result<(), String> {
        let value = self.decrypt(row.value_encrypted.as_deref());
        let shared = Self::is_shared_reference(value.as_deref());
        let u = EnvUpdate {
            is_required: (row.is_required != required).then_some(required),
            is_shared: (shared != row.is_shared).then_some(shared),
            ..EnvUpdate::default()
        };
        store::env_update(self.db, row.id, &u, self.now)
            .await
            .map_err(db_err)
    }

    async fn env_relation(&mut self) -> Result<Vec<EnvRow>, String> {
        if self.env_cache.is_none() {
            self.env_cache = Some(
                store::env_list(self.db, MORPH, self.service.id, false)
                    .await
                    .map_err(db_err)?,
            );
        }
        Ok(self.env_cache.clone().unwrap_or_default())
    }

    /// `resolveSharedEnvironmentVariables($value, $resource)` (`server` too for `real_value`).
    async fn resolve_shared(&self, value: &str, with_server: bool) -> Result<String, String> {
        let mut value = value.trim_matches(PHP_TRIM).to_owned();
        let found: Vec<String> = SHARED_PLACEHOLDER
            .captures_iter(&value)
            .map(|c| c[1].to_owned())
            .collect();
        for shared in found {
            let t = shared.trim();
            let Some(p) = t.find('.') else { continue };
            let kind = t[..p].to_owned();
            let variable = t[p + 1..].to_owned();
            let id = match kind.as_str() {
                "environment" => self.service.environment.get("id").and_then(Value::as_i64),
                "project" => self.service.project.get("id").and_then(Value::as_i64),
                "team" => Some(self.service.team_id),
                "server" if with_server => Some(self.server.id),
                _ => None,
            };
            let Some(id) = id else { continue };
            let row = if kind == "server" {
                coolify_db::environment_variables::shared(
                    self.db,
                    self.service.team_id,
                    &kind,
                    &variable,
                    id,
                )
                .await
                .map_err(db_err)?
                .map(|(v, _)| v)
            } else {
                store::shared_variable_value(self.db, self.service.team_id, &kind, &variable, id)
                    .await
                    .map_err(db_err)?
            };
            if let Some(raw) = row {
                let plain = raw
                    .as_deref()
                    .and_then(|r| self.enc.decrypt_string(r).ok())
                    .unwrap_or_default();
                value = value.replace(&format!("{{{{{shared}}}}}"), &plain);
            }
        }
        Ok(value)
    }

    /// `resolveComposeLongBindSource($source, $resource)`.
    async fn resolve_compose_long_bind_source(&self, source: &str) -> Result<String, String> {
        let Some(c) = LONG_BIND_DEFAULT.captures(source) else {
            return Ok(validate::resolve_compose_volume_source_default(source));
        };
        let key = c[1].to_owned();
        let value = match self.env_first(&key).await? {
            Some(r) => match self.decrypt(r.value_encrypted.as_deref()) {
                Some(v) => Some(self.resolve_shared(&v, true).await?),
                None => None,
            },
            None => None,
        };
        let Some(value) = value.filter(|v| !v.trim().is_empty()) else {
            return Ok(validate::resolve_compose_volume_source_default(source));
        };
        if value.contains('$') || value.contains("{{secret:") {
            return Ok(source.to_owned());
        }
        validate::validate_shell_safe_path(&value, "volume source").map_err(|e| {
            format!("Invalid Docker volume definition (array syntax): the value of {key} is not a safe path. {e}")
        })?;
        Ok(value)
    }

    // --- children -------------------------------------------------------------------------

    async fn migrated(&self, name: &str) -> Result<Option<SubRow>, String> {
        for kind in [SubKind::Application, SubKind::Database] {
            if let Some(row) = svc::sub_by_name(self.db, kind, self.service.id, name)
                .await
                .map_err(db_err)?
                .filter(|r| r.bool("is_migrated"))
            {
                return Ok(Some(row));
            }
        }
        Ok(None)
    }

    async fn first_or_create(&self, kind: SubKind, name: &str) -> Result<SubRow, String> {
        if let Some(row) = svc::sub_by_name(self.db, kind, self.service.id, name)
            .await
            .map_err(db_err)?
        {
            return Ok(row);
        }
        svc::sub_create(self.db, kind, self.service.id, name, self.now)
            .await
            .map_err(db_err)
    }

    /// The model the second and third loops work with.
    async fn resolve_sub(&self, name: &str, is_database: bool) -> Result<SubRow, String> {
        if is_database {
            if let Some(app) =
                svc::sub_by_name(self.db, SubKind::Application, self.service.id, name)
                    .await
                    .map_err(db_err)?
            {
                return Ok(app);
            }
            self.first_or_create(SubKind::Database, name).await
        } else {
            self.first_or_create(SubKind::Application, name).await
        }
    }

    /// `$savedService->image = $image; $savedService->save();`: a `Stringable` is always dirty.
    async fn set_image(&self, sub: &mut SubRow, image: &str) -> Result<(), String> {
        let new = Value::String(image.to_owned());
        svc::sub_write_image(self.db, sub.kind, sub.id, image, self.now)
            .await
            .map_err(db_err)?;
        sub.raw.insert("image".into(), new);
        Ok(())
    }

    /// `$app->fqdn = $url; $app->save()`: the `saving` hook normalizes the ports and syncs noindex.
    async fn save_fqdn(&self, sub: &mut SubRow, fqdn: Option<String>) -> Result<(), String> {
        let existing = overrides_map(json_to_php(sub.raw.get("domain_port_overrides")).as_ref());
        let (new_fqdn, overrides) =
            app_domains::normalize_port_overrides(fqdn.as_deref(), &existing);
        let old_fqdn = sub.str("fqdn").map(str::to_owned);
        if new_fqdn != old_fqdn {
            svc::sub_update(
                self.db,
                sub.kind,
                sub.id,
                &[("fqdn", ColumnValue::Text(new_fqdn.clone()))],
                self.now,
            )
            .await
            .map_err(db_err)?;
        }
        let json = overrides
            .as_ref()
            .map(|o| app_domains::overrides_json(new_fqdn.as_deref(), o));
        let old_json = sub
            .raw
            .get("domain_port_overrides")
            .filter(|v| !v.is_null())
            .map(ToString::to_string);
        let same = match (&json, &old_json) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                serde_json::from_str::<Value>(a).ok() == serde_json::from_str::<Value>(b).ok()
            }
            _ => false,
        };
        if !same {
            svc::sub_set_json(
                self.db,
                sub.kind,
                sub.id,
                "domain_port_overrides",
                json.as_deref(),
                self.now,
            )
            .await
            .map_err(db_err)?;
            sub.raw.insert(
                "domain_port_overrides".into(),
                json.as_deref()
                    .and_then(|j| serde_json::from_str(j).ok())
                    .unwrap_or(Value::Null),
            );
        }
        sub.raw
            .insert("fqdn".into(), new_fqdn.map_or(Value::Null, Value::String));
        self.sync_noindex(sub).await
    }

    /// `$part->fqdn = $url; $part->save()`.
    pub async fn save_part_fqdn(&self, part: &SubRow, fqdn: Option<String>) -> Result<(), String> {
        let mut part = part.clone();
        self.save_fqdn(&mut part, fqdn).await
    }

    /// `HasNoindexDomains::syncNoindexDomains()`.
    async fn sync_noindex(&self, sub: &mut SubRow) -> Result<(), String> {
        let Some(PhpValue::Array(current)) = json_to_php(sub.raw.get("noindex_domains")) else {
            return Ok(());
        };
        if current.is_empty() {
            return Ok(());
        }
        let normalize = |d: &str| app_domains::without_port(&app_domains::normalize_url(d));
        let domains: Vec<String> = app_domains::domain_list(sub.str("fqdn"))
            .iter()
            .map(|d| normalize(d))
            .collect();
        let mut out: Vec<String> = Vec::new();
        for (_, d) in current {
            let PhpValue::Str(s) = d else { continue };
            if s.trim().is_empty() {
                continue;
            }
            let n = normalize(&s);
            if domains.contains(&n) && !out.contains(&n) {
                out.push(n);
            }
        }
        let json = php::json_encode(&PhpValue::strings(out));
        svc::sub_set_json(
            self.db,
            sub.kind,
            sub.id,
            "noindex_domains",
            Some(&json),
            self.now,
        )
        .await
        .map_err(db_err)?;
        sub.raw.insert(
            "noindex_domains".into(),
            serde_json::from_str(&json).unwrap_or(Value::Null),
        );
        Ok(())
    }

    /// `findServiceApplicationForEnvName($resource, $envServiceName)`.
    async fn application_for_env_name(&self, name: &str) -> Result<Option<SubRow>, String> {
        let apps = svc::subs(self.db, SubKind::Application, self.service.id)
            .await
            .map_err(db_err)?;
        let names: Vec<String> = apps.iter().map(|a| a.name.clone()).collect();
        let resolved = domains::find_compose_service_name(name, &names)
            .or_else(|| domains::find_compose_service_name(&name.replace('_', "-"), &names));
        Ok(resolved.and_then(|r| apps.into_iter().find(|a| a.name == r)))
    }

    /// `isCopiedServiceUrlDeclaration($resource, $services, $current, $key)`.
    async fn is_copied_declaration(
        &self,
        services: &[(Key, PhpValue)],
        current: &str,
        key: &str,
    ) -> Result<bool, String> {
        let parsed = env::parse_service_environment_variable(key);
        let Some(owner) = self.application_for_env_name(&parsed.service_name).await? else {
            return Ok(false);
        };
        if owner.name == current {
            return Ok(false);
        }
        let of = |name: &str| {
            php::get_key(services, &Key::from_php_string(name))
                .map(declarations)
                .unwrap_or_default()
        };
        let owner_decl = of(&owner.name);
        let current_decl = of(current);
        let Some(value) = php::get_key(&current_decl, &Key::from_php_string(key)) else {
            return Ok(false);
        };
        let twin = if let Some(rest) = key.strip_prefix("SERVICE_URL_") {
            format!("SERVICE_FQDN_{rest}")
        } else {
            format!(
                "SERVICE_URL_{}",
                key.strip_prefix("SERVICE_FQDN_").unwrap_or(key)
            )
        };
        let value = php::to_string(value);
        Ok([key, twin.as_str()].iter().any(|k| {
            php::get_key(&owner_decl, &Key::from_php_string(k))
                .is_some_and(|v| php::to_string(v) == value)
        }))
    }

    /// `generateServiceSpecificFqdns($savedService)`.
    async fn service_specific_fqdns(
        &self,
        sub: &SubRow,
        kind: &str,
    ) -> Result<Vec<String>, String> {
        let spec: &[(&str, &str, &str, bool)] =
            if kind.contains("minio") || kind.contains("pgsty/silo") {
                &[
                    ("MINIO_BROWSER_REDIRECT_URL", "console-", "9001", true),
                    ("MINIO_SERVER_URL", "minio-", "9000", true),
                ]
            } else if kind.contains("logto") {
                &[
                    ("LOGTO_ENDPOINT", "logto-", "3001", false),
                    ("LOGTO_ADMIN_ENDPOINT", "logto-admin-", "3002", false),
                ]
            } else if kind.contains("garage") {
                &[
                    ("GARAGE_S3_API_URL", "s3-", "3900", true),
                    ("GARAGE_WEB_URL", "web-", "3902", true),
                    ("GARAGE_ADMIN_URL", "admin-", "3903", true),
                ]
            } else {
                return Ok(Vec::new());
            };
        let mut rows = Vec::new();
        for (key, ..) in spec {
            match self.env_first(key).await? {
                Some(r) => rows.push(r),
                None => return Ok(Vec::new()),
            }
        }
        let mut out = Vec::new();
        for (row, (_, prefix, port, https)) in rows.iter().zip(spec) {
            let mut value = self.decrypt(row.value_encrypted.as_deref());
            if value.as_deref().unwrap_or_default().is_empty() {
                let generated = {
                    let url = self
                        .server
                        .url_base
                        .generate_url(&format!("{prefix}{}", sub.uuid));
                    if *https {
                        let after = php::after(&url, "://");
                        format!("https://{after}")
                    } else {
                        url
                    }
                };
                let u = EnvUpdate {
                    value_encrypted: Some(Some(self.enc.encrypt_serialized(&generated))),
                    ..EnvUpdate::default()
                };
                store::env_update(self.db, row.id, &u, self.now)
                    .await
                    .map_err(db_err)?;
                value = Some(generated);
            }
            out.push(format!("{}:{port}", value.unwrap_or_default()));
        }
        Ok(out)
    }

    /// `$resource->save()`.
    async fn save(&mut self) -> Result<(), String> {
        let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
        if self.docker_compose != self.original.0 {
            values.push((
                "docker_compose",
                ColumnValue::Text(self.docker_compose.clone()),
            ));
        }
        if self.docker_compose_raw != self.original.1 {
            values.push((
                "docker_compose_raw",
                ColumnValue::Text(Some(self.docker_compose_raw.clone())),
            ));
        }
        svc::update(self.db, self.service.id, &values, self.now)
            .await
            .map_err(db_err)?;
        self.original = (self.docker_compose.clone(), self.docker_compose_raw.clone());
        Ok(())
    }

    fn noindex_of(sub: &SubRow) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if let Some(PhpValue::Array(items)) = json_to_php(sub.raw.get("noindex_domains")) {
            for (_, d) in items {
                if let PhpValue::Str(s) = d
                    && !s.trim().is_empty()
                {
                    let n = app_domains::without_port(&app_domains::normalize_url(&s));
                    if !out.contains(&n) {
                        out.push(n);
                    }
                }
            }
        }
        out
    }
}

async fn load_server(db: &Db, service: &ServiceRow) -> Result<ServerInfo, String> {
    let server_id = service.server_id().ok_or("The service has no server.")?;
    let row = coolify_db::servers::find_by_id(db, server_id)
        .await
        .map_err(db_err)?
        .ok_or("Server not found.")?;
    let settings = row
        .settings
        .clone()
        .map(Value::Object)
        .unwrap_or(Value::Null);
    let ip = row
        .record
        .get("ip")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let proxy = match row.record.get("proxy") {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => Value::Null,
    };
    let instance = coolify_db::instance::deployment_settings(db)
        .await
        .map_err(db_err)?;
    let public = instance.public_ipv4.or(instance.public_ipv6);
    let url_base = UrlBase::for_server(
        settings.get("wildcard_domain").and_then(Value::as_str),
        &ip,
        public.as_deref(),
    )
    .await;
    Ok(ServerInfo {
        ip,
        id: row.id,
        settings,
        proxy,
        url_base,
    })
}

/// `useComposeExternalVolumeAsWritten($resource, $owner, $topLevelVolumes, $source, $legacyName)`.
async fn use_external_as_written(
    run: &mut ServiceRun<'_>,
    owner: &SubRow,
    top_volumes: &[(Key, PhpValue)],
    source: &str,
    legacy_name: &str,
) -> Result<bool, String> {
    let Some(declaration) = volumes::compose_external_volume_declaration(top_volumes, source)?
    else {
        return Ok(false);
    };
    if legacy_name == source {
        return Ok(true);
    }
    let has_legacy =
        store::persistent_volume_exists(run.db, owner.kind.morph(), owner.id, legacy_name)
            .await
            .map_err(db_err)?;
    if !has_legacy {
        return Ok(true);
    }
    let warning =
        volumes::compose_legacy_external_volume_warning(source, &declaration, legacy_name);
    if !run.warnings.contains(&warning) {
        run.warnings.push(warning);
    }
    Ok(false)
}

/// `preserveLegacyDockerComposeNullAssignments($environment, $environmentVariables)`.
fn preserve_legacy_null_assignments(mut environment: Arr, variables: &[EnvRow]) -> Arr {
    for v in variables {
        let k = Key::from_php_string(&v.key);
        let Some(value) = php::get_key(&environment, &k).cloned() else {
            continue;
        };
        if value == PhpValue::Null || value == PhpValue::Str(format!("${}", v.key)) {
            php::put(&mut environment, k, PhpValue::Str(String::new()));
            continue;
        }
        let PhpValue::Str(s) = &value else { continue };
        if !s.starts_with("${") {
            continue;
        }
        let Some((content, _, end)) = env::extract_balanced_brace_content(s, 0) else {
            continue;
        };
        if end != s.len() - 1 {
            continue;
        }
        let name =
            env::split_on_operator_outside_nested(&content).map_or(content, |(var, _, _)| var);
        if name == v.key {
            php::put(&mut environment, k, PhpValue::Str(String::new()));
        }
    }
    environment
}

/// `syncDockerComposeEnvironmentVariableRequiredState($resource)`.
async fn sync_required_state(run: &mut ServiceRun<'_>) -> Result<(), String> {
    let compose = Some(run.docker_compose_raw.clone());
    let rows = store::env_list(run.db, MORPH, run.service.id, false)
        .await
        .map_err(db_err)?;
    for row in rows {
        let preserves =
            row.preserve_compose_null && run.decrypt(row.value_encrypted.as_deref()).is_none();
        let required = !preserves
            && env::docker_compose_environment_variable_is_required(compose.as_deref(), &row.key);
        if row.is_required != required {
            run.env_set_required(&row, required).await?;
        }
    }
    Ok(())
}

/// `serviceParser($resource)`.
#[allow(clippy::too_many_lines)]
pub async fn service_parser(run: &mut ServiceRun<'_>) -> Result<ParsedCompose, String> {
    run.warnings.clear();
    let uuid = run.uuid();
    let compose = run.docker_compose_raw.clone();
    let original_compose = compose.clone();
    if !php::truthy(&PhpValue::Str(compose.clone())) {
        return Ok(ParsedCompose::empty());
    }
    run.comments = extract_yaml_environment_comments(&compose);
    let Ok(yaml) = validate::parse_docker_compose_yaml(&compose) else {
        return Ok(ParsedCompose::empty());
    };
    let services: Arr = match php::get(&yaml, "services") {
        Some(PhpValue::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    svc::env_delete_corrupted(run.db, run.service.id)
        .await
        .map_err(db_err)?;

    let mut top_volumes: Arr = php::collect(php::get(&yaml, "volumes"));
    let mut top_networks: Arr = php::collect(php::get(&yaml, "networks"));
    let top_configs: Arr = php::collect(php::get(&yaml, "configs"));
    let top_secrets: Arr = php::collect(php::get(&yaml, "secrets"));

    // ensureComposeNetworkNameVariables()
    for (_, network) in top_networks.clone() {
        let Some(PhpValue::Str(name)) = php::get(&network, "name") else {
            continue;
        };
        let Some((variable, default)) = validate::compose_network_name_variable(name) else {
            continue;
        };
        if let Some(d) = &default
            && !validate::is_valid_docker_network(d)
        {
            continue;
        }
        run.env_first_or_create(
            &variable,
            Some(default.as_deref().unwrap_or("")),
            None,
            None,
        )
        .await?;
    }
    if !top_volumes.is_empty() {
        top_volumes.retain(|(_, v)| *v != PhpValue::Null);
    }
    let base_network = uuid.clone();

    let legacy_null_variables: Vec<EnvRow> = store::env_list(run.db, MORPH, run.service.id, false)
        .await
        .map_err(db_err)?
        .into_iter()
        .filter(|v| v.preserve_compose_null && v.value_encrypted.is_none())
        .collect();

    // generateDockerComposeServiceName($services)
    let service_name_environments = env::generate_docker_compose_service_name(&services, 0);

    // Presave services
    for (name_key, service) in &services {
        let service_name = name_key.as_string();
        validate::validate_shell_safe_path(&service_name, "service name").map_err(|e| {
            format!("Invalid Docker Compose service name: {e} Service names must not contain shell metacharacters.")
        })?;
        validate::validate_compose_service_scale(&service_name, service)?;
        let image = php::data_get_str(service, "image", "");
        let mut saved = match run.migrated(&service_name).await? {
            Some(s) => s,
            None => {
                let kind = if service_helpers::is_database_image(&image, service) {
                    SubKind::Database
                } else {
                    SubKind::Application
                };
                run.first_or_create(kind, &service_name).await?
            }
        };
        run.set_image(&mut saved, &image).await?;
    }

    let server_is_log_drain = service_helpers::is_log_drain_enabled(&run.server.settings);
    let mut all_magic: Vec<String> = Vec::new();
    for (name_key, service) in &services {
        let service_name = name_key.as_string();
        let image = php::data_get_str(service, "image", "");
        let migrated = run.migrated(&service_name).await?;
        let is_database = match &migrated {
            Some(m) => m.kind == SubKind::Database,
            None => service_helpers::is_database_image(&image, service),
        };
        let mut saved = match migrated {
            Some(m) => m,
            None => run.resolve_sub(&service_name, is_database).await?,
        };
        run.set_image(&mut saved, &image).await?;
        // Pocketbase does not need gzip for SSE.
        if image.contains("pocketbase") && saved.bool_or("is_gzip_enabled", true) {
            svc::sub_update(
                run.db,
                saved.kind,
                saved.id,
                &[("is_gzip_enabled", ColumnValue::Bool(false))],
                run.now,
            )
            .await
            .map_err(db_err)?;
            saved
                .raw
                .insert("is_gzip_enabled".into(), Value::Bool(false));
        }
        let environment = declarations(service);
        let mut magic: Vec<String> = Vec::new();
        for (key, value) in &environment {
            let key = key.as_string();
            let value = php::to_string(value);
            for c in MAGIC_REFERENCE.captures_iter(&value) {
                let m = c[2].to_owned();
                if m.starts_with("SERVICE_") && !magic.contains(&m) {
                    magic.push(m);
                }
            }
            if !(key.starts_with("SERVICE_FQDN_") || key.starts_with("SERVICE_URL_")) {
                continue;
            }
            let parsed = env::parse_service_environment_variable(&key);
            let base = key
                .strip_prefix("SERVICE_URL_")
                .or_else(|| key.strip_prefix("SERVICE_FQDN_"))
                .unwrap_or_default();
            let env_name = if parsed.has_port {
                php::before_last(base, "_")
            } else {
                base.to_owned()
            };
            let port = parsed.port.clone();
            let fqdn_for = parsed.service_name.clone();
            if run
                .is_copied_declaration(&services, &saved.name, &key)
                .await?
            {
                continue;
            }
            let is_app = saved.is_application();
            let current = if is_app {
                saved.str("fqdn").map(str::to_owned)
            } else {
                None
            };
            let version = run.parsing_version();
            let (mut fqdn, mut url);
            if is_app && php::filled_str(current.as_deref()) {
                let first = first_domain_from_list(current.as_deref());
                fqdn = domains::get_fqdn_without_port(&first);
                url = fqdn.clone();
            } else {
                fqdn = run.server.url_base.generate_fqdn(
                    &format!("{fqdn_for}-{uuid}"),
                    version,
                    &run.version,
                );
                url = run
                    .server
                    .url_base
                    .generate_url(&format!("{fqdn_for}-{uuid}"));
            }
            let mut fqdn_env = php::after(&fqdn, "://");
            if value.starts_with('/') && value != "/" {
                if !fqdn.ends_with(&value) {
                    fqdn = format!("{fqdn}{value}");
                }
                if !url.ends_with(&value) {
                    url = format!("{url}{value}");
                }
                if !fqdn_env.ends_with(&value) {
                    fqdn_env = format!("{fqdn_env}{value}");
                }
            }
            let truthy_port = port
                .as_deref()
                .filter(|p| php::truthy(&PhpValue::Str((*p).to_owned())));
            let url_with_port = match truthy_port {
                Some(p) if !url.is_empty() => format!("{url}:{p}"),
                _ => url.clone(),
            };
            let fqdn_env_with_port = match truthy_port {
                Some(p) if !fqdn.is_empty() => format!("{fqdn_env}:{p}"),
                _ => fqdn_env.clone(),
            };
            if is_app && saved.raw.get("fqdn").is_none_or(Value::is_null) {
                run.save_fqdn(&mut saved, Some(url.clone())).await?;
            }
            let fqdn_key = format!("SERVICE_FQDN_{env_name}");
            let comment = run.comment(&fqdn_key);
            run.env_update_or_create(&fqdn_key, Some(&fqdn_env), comment)
                .await?;
            let url_key = format!("SERVICE_URL_{env_name}");
            let comment = run.comment(&url_key);
            run.env_update_or_create(&url_key, Some(&url), comment)
                .await?;
            if parsed.has_port
                && let Some(p) = truthy_port
            {
                let fqdn_port_key = format!("SERVICE_FQDN_{env_name}_{p}");
                let comment = run.comment(&fqdn_port_key);
                run.env_update_or_create(&fqdn_port_key, Some(&fqdn_env_with_port), comment)
                    .await?;
                let url_port_key = format!("SERVICE_URL_{env_name}_{p}");
                let comment = run.comment(&url_port_key);
                run.env_update_or_create(&url_port_key, Some(&url_with_port), comment)
                    .await?;
            }
        }
        for m in &magic {
            if !all_magic.contains(m) {
                all_magic.push(m.clone());
            }
        }
        for key in magic {
            let command = env::parse_command_from_magic_env_variable(&key);
            if command == "FQDN" || command == "URL" {
                let prefix = if command == "FQDN" {
                    "SERVICE_FQDN_"
                } else {
                    "SERVICE_URL_"
                };
                let name_for = php::after(&key, prefix).to_lowercase();
                let random = format!("{}-{uuid}", name_for.replace('_', "-"));
                let url = run.server.url_base.generate_url(&random);
                let fqdn =
                    run.server
                        .url_base
                        .generate_fqdn(&random, run.parsing_version(), &run.version);
                let env_exists = run.env_first(&key).await?.is_some();
                let port_suffixed = svc::env_keys_like(run.db, run.service.id, &key)
                    .await
                    .map_err(db_err)?
                    .iter()
                    .any(|k| {
                        k.strip_prefix(&format!("{key}_")).is_some_and(|rest| {
                            !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
                        })
                    });
                let exists = run.application_for_env_name(&name_for).await?;
                let has_port = exists
                    .as_ref()
                    .is_some_and(|e| fqdn_has_port(e.str("fqdn")));
                if !env_exists
                    && !port_suffixed
                    && !has_port
                    && let Some(mut e) =
                        exists.filter(|e| e.id == saved.id && saved.is_application())
                {
                    run.save_fqdn(&mut e, Some(url.clone())).await?;
                    saved = e;
                }
                let comment = run.comment(&key);
                let own_value = if command == "FQDN" { &fqdn } else { &url };
                run.env_first_or_create(&key, Some(own_value), None, comment)
                    .await?;
                let (pair_key, pair_value) = if command == "FQDN" {
                    (format!("SERVICE_URL_{}", name_for.to_uppercase()), &url)
                } else {
                    (format!("SERVICE_FQDN_{}", name_for.to_uppercase()), &fqdn)
                };
                let comment = run.comment(&pair_key);
                run.env_first_or_create(&pair_key, Some(pair_value), None, comment)
                    .await?;
            } else {
                let secret = if command.starts_with("SUPABASE") {
                    match run.env_first("SERVICE_PASSWORD_JWT").await? {
                        Some(r) => run.decrypt(r.value_encrypted.as_deref()),
                        None => None,
                    }
                } else {
                    None
                };
                let value = env::generate_env_value(
                    &command,
                    secret.as_deref(),
                    run.now.and_utc().timestamp(),
                );
                let comment = run.comment(&key);
                run.env_first_or_create(&key, value.as_deref(), None, comment)
                    .await?;
            }
        }
    }

    let log_drain_map: Vec<(String, bool)> =
        svc::subs(run.db, SubKind::Application, run.service.id)
            .await
            .map_err(db_err)?
            .into_iter()
            .map(|a| {
                let v = a.bool("is_log_drain_enabled");
                (a.name, v)
            })
            .collect();

    let service_project = run
        .service
        .project
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let service_environment = run
        .service
        .environment
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let resource_name = run.service.name().to_owned();
    let label_escape = run
        .label_escape_override
        .unwrap_or_else(|| run.service.bool("is_container_label_escape_enabled"));
    let destination_network = destination_network(run).await?;

    let mut parsed_services: Arr = Vec::new();
    for (name_key, service_value) in &services {
        let service_name = name_key.as_string();
        let mut service = service_value.clone();
        let image = php::data_get_str(&service, "image", "");
        let restart = php::data_get_str(&service, "restart", RESTART_MODE);
        let mut logging = php::get(&service, "logging")
            .cloned()
            .unwrap_or(PhpValue::Null);
        if server_is_log_drain && log_drain_map.iter().any(|(n, v)| *n == service_name && *v) {
            logging = service_helpers::generate_log_drain_configuration(&run.server.settings);
        }
        let volumes_list = php::collect(php::get(&service, "volumes"));
        let mut networks = php::collect(php::get(&service, "networks"));
        let use_network_mode =
            php::get(&service, "network_mode").is_some_and(|v| *v != PhpValue::Null);
        let depends_on = php::collect(php::get(&service, "depends_on"));
        let raw_labels = php::collect(php::get(&service, "labels"));
        let mut labels_list: Vec<PhpValue> = raw_labels.iter().map(|(_, v)| v.clone()).collect();
        if !raw_labels.is_empty() && php::is_associative(&raw_labels) {
            labels_list = raw_labels
                .iter()
                .map(|(k, v)| PhpValue::Str(format!("{}={}", k.as_string(), php::to_string(v))))
                .collect();
        }
        let ports = php::collect(php::get(&service, "ports"));
        let mut environment = declarations(&service);
        let mut coolify_environments: Arr = Vec::new();

        let migrated = run.migrated(&service_name).await?;
        let is_database = match &migrated {
            Some(m) => m.kind == SubKind::Database,
            None => service_helpers::is_database_image(&image, &service),
        };
        let container_name = format!("{service_name}-{uuid}");
        let mut saved = match migrated {
            Some(m) => m,
            None => run.resolve_sub(&service_name, is_database).await?,
        };
        run.set_image(&mut saved, &image).await?;

        let mut volumes_parsed: Arr = Vec::new();
        for (index, volume) in &volumes_list {
            let mut volume = volume.clone();
            let kind: String;
            let mut source: String;
            let target: String;
            let mut content: Option<String> = None;
            let mut is_directory: bool;
            let mut mode: Option<String> = None;
            let found;
            match &volume {
                PhpValue::Str(s) => {
                    let parsed = validate::parse_docker_volume_string(s)?;
                    source = parsed.source.unwrap_or_default();
                    target = parsed.target.unwrap_or_default();
                    mode = parsed.mode;
                    found =
                        store::file_volume_by_mount(run.db, saved.kind.morph(), saved.id, &target)
                            .await
                            .map_err(db_err)?;
                    if volumes::source_is_local(&source) {
                        kind = "bind".into();
                        if let Some(f) = &found {
                            content = f
                                .content_encrypted
                                .as_deref()
                                .and_then(|c| run.enc.decrypt_string(c).ok());
                            is_directory = f.is_directory;
                        } else {
                            is_directory = true;
                        }
                    } else {
                        kind = "volume".into();
                        is_directory = false;
                    }
                }
                PhpValue::Array(_) => {
                    kind = php::data_get_str(&volume, "type", "");
                    source = php::data_get_str(&volume, "source", "");
                    target = php::data_get_str(&volume, "target", "");
                    content = match php::get(&volume, "content") {
                        None | Some(PhpValue::Null) => None,
                        Some(c) => Some(php::to_string(c)),
                    };
                    is_directory = php::get(&volume, "isDirectory").is_some_and(php::truthy)
                        || php::get(&volume, "is_directory").is_some_and(php::truthy);
                    if !php::empty_str(&source) {
                        validate::validate_compose_array_volume_source(&source)?;
                        if kind == "bind" {
                            source = run.resolve_compose_long_bind_source(&source).await?;
                        }
                    }
                    validate::validate_compose_content_volume_source(&volume)?;
                    if !php::empty_str(&target) {
                        validate::validate_shell_safe_path(&target, "volume target").map_err(|e| {
                            format!("Invalid Docker volume definition (array syntax): {e} Please use safe path names without shell metacharacters.")
                        })?;
                    }
                    found =
                        store::file_volume_by_mount(run.db, saved.kind.morph(), saved.id, &target)
                            .await
                            .map_err(db_err)?;
                    if let Some(f) = &found {
                        content = f
                            .content_encrypted
                            .as_deref()
                            .and_then(|c| run.enc.decrypt_string(c).ok());
                        is_directory = f.is_directory;
                    } else if !is_directory && content.is_none() {
                        is_directory = true;
                    }
                }
                _ => return Err("Call to a member function value() on null".into()),
            }
            let with_mode = |source: &str, mode: &Option<String>| {
                let mut s = format!("{source}:{target}");
                if let Some(m) = mode
                    .as_deref()
                    .filter(|m| php::truthy(&PhpValue::Str((*m).to_owned())))
                {
                    s.push(':');
                    s.push_str(m);
                }
                s
            };
            if kind == "bind" {
                if ["/var/run/docker.sock", "/tmp", "/tmp/"].contains(&source.as_str()) {
                    volume = match &volume {
                        PhpValue::Str(_) => PhpValue::Str(with_mode(&source, &mode)),
                        PhpValue::Array(items) => {
                            PhpValue::Array(volumes::compose_long_bind_volume(items, &source))
                        }
                        other => other.clone(),
                    };
                } else {
                    let main_directory = if run.parsing_version() >= 4 {
                        format!("{}/services/{uuid}", volumes::BASE_CONFIGURATION_DIR)
                    } else {
                        format!("{}/applications/{uuid}", volumes::BASE_CONFIGURATION_DIR)
                    };
                    source = volumes::resolve_compose_bind_source(
                        &source,
                        &main_directory,
                        found.as_ref().map(|f| f.fs_path.as_str()),
                    )?;
                    if !volumes::compose_bind_keeps_missing_host_path(&volume) {
                        let content_enc = content.as_deref().map(|c| run.enc.encrypt_string(c));
                        match &found {
                            Some(f) => {
                                let old_plain = f
                                    .content_encrypted
                                    .as_deref()
                                    .and_then(|c| run.enc.decrypt_string(c).ok());
                                let content_change =
                                    (old_plain != content).then_some(content_enc.as_deref());
                                store::file_volume_update(
                                    run.db,
                                    f.id,
                                    &source,
                                    &target,
                                    content_change,
                                    is_directory,
                                    run.now,
                                )
                                .await
                                .map_err(db_err)?;
                            }
                            None => {
                                let id = store::file_volume_insert(
                                    run.db,
                                    saved.kind.morph(),
                                    saved.id,
                                    &source,
                                    &target,
                                    content_enc.as_deref(),
                                    is_directory,
                                    run.now,
                                )
                                .await
                                .map_err(db_err)?;
                                run.effects.new_file_volumes.push(id);
                            }
                        }
                    } else {
                        store::forget_file_volumes(run.db, saved.kind.morph(), saved.id, &target)
                            .await
                            .map_err(db_err)?;
                    }
                    volume = match &volume {
                        PhpValue::Array(items) => {
                            PhpValue::Array(volumes::compose_long_bind_volume(items, &source))
                        }
                        _ => PhpValue::Str(with_mode(&source, &mode)),
                    };
                }
            } else if kind == "volume" {
                let legacy_name = format!("{uuid}_{}", php::slug(&source));
                if use_external_as_written(run, &saved, &top_volumes, &source, &legacy_name).await?
                {
                    php::put(&mut volumes_parsed, index.clone(), volume);
                    continue;
                }
                let declaration =
                    php::get_key(&top_volumes, &Key::from_php_string(&source)).cloned();
                let is_network_volume = matches!(
                    declaration.as_ref().and_then(|d| php::data_get(d, "driver_opts.type")),
                    Some(PhpValue::Str(t)) if t == "cifs" || t == "nfs"
                );
                if is_network_volume {
                    php::put(&mut volumes_parsed, index.clone(), volume);
                    continue;
                }
                let name = legacy_name;
                volume = match &volume {
                    PhpValue::Str(s) => {
                        let parsed = validate::parse_docker_volume_string(s)?;
                        let mut v = format!("{name}:{}", parsed.target.unwrap_or_default());
                        if let Some(m) = parsed
                            .mode
                            .filter(|m| php::truthy(&PhpValue::Str(m.clone())))
                        {
                            v.push(':');
                            v.push_str(&m);
                        }
                        PhpValue::Str(v)
                    }
                    PhpValue::Array(items) => {
                        let mut items = items.clone();
                        php::put_str(&mut items, "source", PhpValue::Str(name.clone()));
                        PhpValue::Array(items)
                    }
                    other => other.clone(),
                };
                let ignores = store::persistent_volume_upsert(
                    run.db,
                    saved.kind.morph(),
                    saved.id,
                    &name,
                    &target,
                    run.now,
                )
                .await
                .map_err(db_err)?;
                php::put_str(
                    &mut top_volumes,
                    &name,
                    PhpValue::Array(volumes::compose_renamed_volume_declaration_for(
                        declaration.as_ref(),
                        &name,
                        ignores,
                        false,
                    )),
                );
            }
            let effect = (saved.kind, saved.id);
            run.effects.files_from_server.push(effect);
            php::put(&mut volumes_parsed, index.clone(), volume);
        }

        if !use_network_mode {
            if !top_networks.is_empty() {
                for (network_name, network) in top_networks.clone() {
                    if network_name == Key::Str("default".into()) {
                        continue;
                    }
                    if php::get(&network, "aliases").is_some_and(php::truthy) {
                        continue;
                    }
                    let exists = networks
                        .iter()
                        .any(|(k, v)| network_matches(k, v, &network_name));
                    if !exists {
                        php::put(&mut networks, network_name.clone(), PhpValue::Null);
                    }
                }
            }
            // `$value == $baseNetwork` compares with a collection: never equal.
            php::put_str(
                &mut top_networks,
                &base_network,
                PhpValue::map([
                    ("name", PhpValue::Str(base_network.clone())),
                    ("external", PhpValue::Bool(true)),
                ]),
            );
        }

        // Collect/create/update ports
        let mut collected: Vec<String> = Vec::new();
        for (_, sport) in &ports {
            match sport {
                PhpValue::Str(_) | PhpValue::Int(_) | PhpValue::Float(_) => {
                    collected.push(php::to_string(sport));
                }
                PhpValue::Array(_) => {
                    let part = |k: &str| php::get(sport, k).map(php::to_string).unwrap_or_default();
                    collected.push(format!(
                        "{}:{}/{}",
                        part("target"),
                        part("published"),
                        part("protocol")
                    ));
                }
                _ => {}
            }
        }
        svc::sub_update(
            run.db,
            saved.kind,
            saved.id,
            &[("ports", ColumnValue::Text(Some(collected.join(","))))],
            run.now,
        )
        .await
        .map_err(db_err)?;

        let mut networks_temp: Arr = Vec::new();
        if !use_network_mode {
            for (key, network) in &networks {
                match network {
                    PhpValue::Str(s) => php::put_str(&mut networks_temp, s, PhpValue::Null),
                    PhpValue::Array(_) => {
                        php::put(&mut networks_temp, key.clone(), network.clone());
                    }
                    _ => {}
                }
            }
            php::put_str(&mut networks_temp, &base_network, PhpValue::Null);
        }

        let passthrough =
            env::extract_docker_compose_passthrough_keys(php::get(&service, "environment"));
        let normal: Arr = environment
            .iter()
            .filter(|(k, _)| !all_magic.contains(&k.as_string()))
            .filter(|(_, v)| !php::to_string(v).starts_with("SERVICE_"))
            .cloned()
            .collect();
        for (key, value) in normal {
            let key = key.as_string();
            let value = php::to_string(&value);
            let parsed_value = env::replace_variables(&value);
            let comment = run.comment(&key);
            if value.is_empty() && passthrough.contains(&key) {
                run.env_first_or_create(&key, None, None, comment).await?;
                continue;
            }
            let primary = env::extract_balanced_brace_content(&value, 0)
                .and_then(|(content, _, _)| env::split_on_operator_outside_nested(&content));
            for reference in env::extract_docker_compose_environment_variable_references(&value) {
                if value.starts_with('$')
                    && primary
                        .as_ref()
                        .is_some_and(|(var, op, _)| *var == reference && op.contains('?'))
                {
                    continue;
                }
                if reference.starts_with("SERVICE_") {
                    continue;
                }
                let row = run
                    .env_first_or_create(&reference, None, None, None)
                    .await?;
                let required = env::docker_compose_environment_variable_is_required(
                    Some(&run.docker_compose_raw),
                    &reference,
                );
                if row.is_required != required {
                    run.env_set_required(&row, required).await?;
                }
            }
            if parsed_value.starts_with("SERVICE_") {
                run.env_update_or_create(&key, Some(&value), comment)
                    .await?;
                continue;
            }
            if !value.starts_with('$') {
                continue;
            }
            if key == parsed_value {
                run.env_first_or_create(&key, None, None, comment).await?;
                continue;
            }
            match env::extract_balanced_brace_content(&value, 0) {
                Some((content, _, _)) => match env::split_on_operator_outside_nested(&content) {
                    Some((var, op, default)) => {
                        let required = op.contains('?');
                        let value_for_create = if required {
                            Some(default.as_str())
                        } else {
                            None
                        };
                        run.env_first_or_create(&var, value_for_create, Some(required), comment)
                            .await?;
                        if default.contains("${") {
                            let mut search = 0usize;
                            let mut nested = env::extract_balanced_brace_content(&default, search);
                            while let Some((nested_content, _, end)) = nested {
                                let nested_split =
                                    env::split_on_operator_outside_nested(&nested_content);
                                let nested_name = nested_split
                                    .as_ref()
                                    .map_or(nested_content.clone(), |s| s.0.clone());
                                let magic_var = nested_name.starts_with("SERVICE_URL_")
                                    || nested_name.starts_with("SERVICE_FQDN_");
                                if !magic_var {
                                    match &nested_split {
                                        Some((v, _, d)) => {
                                            run.env_first_or_create(v, Some(d), None, None).await?;
                                        }
                                        None => {
                                            run.env_first_or_create(
                                                &nested_content,
                                                None,
                                                None,
                                                None,
                                            )
                                            .await?;
                                        }
                                    }
                                }
                                search = end + 1;
                                if search >= default.len() {
                                    break;
                                }
                                nested = env::extract_balanced_brace_content(&default, search);
                            }
                        }
                    }
                    None => {
                        run.env_first_or_create(&content, None, Some(false), comment)
                            .await?;
                    }
                },
                None => {
                    // Fallback to old behavior for malformed input
                    let mut is_required = false;
                    let (mut k2, mut v2) = (key.clone(), value.clone());
                    if value.contains(":-") {
                        let r = env::replace_variables(&value);
                        k2 = php::before(&r, ":");
                        v2 = php::after(&r, ":-");
                    } else if value.contains('-') {
                        let r = env::replace_variables(&value);
                        k2 = php::before(&r, "-");
                        v2 = php::after(&r, "-");
                    } else if value.contains(":?") {
                        let r = env::replace_variables(&value);
                        k2 = php::before(&r, ":");
                        v2 = php::after(&r, ":?");
                        is_required = true;
                    } else if value.contains('?') {
                        let r = env::replace_variables(&value);
                        k2 = php::before(&r, "?");
                        v2 = php::after(&r, "?");
                        is_required = true;
                    }
                    if value == v2 {
                        let parsed_key = env::replace_variables(&v2);
                        let row = run
                            .env_first_or_create(&parsed_key, None, Some(is_required), comment)
                            .await?;
                        let stored = run.decrypt(row.value_encrypted.as_deref());
                        php::put_str(
                            &mut environment,
                            &parsed_key,
                            stored.map_or(PhpValue::Null, PhpValue::Str),
                        );
                        continue;
                    }
                    run.env_first_or_create(&k2, Some(&v2), Some(is_required), comment)
                        .await?;
                }
            }
        }

        let relation = run.env_relation().await?;
        let has = |k: &str| relation.iter().any(|r| r.key == k);
        if !has("COOLIFY_RESOURCE_UUID") {
            php::put_str(
                &mut coolify_environments,
                "COOLIFY_RESOURCE_UUID",
                PhpValue::Str(uuid.clone()),
            );
        }
        if !has("COOLIFY_CONTAINER_NAME") {
            php::put_str(
                &mut coolify_environments,
                "COOLIFY_CONTAINER_NAME",
                PhpValue::Str(container_name.clone()),
            );
        }

        // `$savedService` is the row from firstOrCreate: its fqdn is what the database holds.
        let fresh = svc::sub_by_name(run.db, saved.kind, run.service.id, &saved.name)
            .await
            .map_err(db_err)?
            .unwrap_or_else(|| saved.clone());
        let saved = fresh;
        let specific = if saved.is_application() {
            service_type(saved.str("image"))
        } else {
            None
        };
        let fqdns: Vec<String> = if let Some(t) = specific {
            run.service_specific_fqdns(&saved, t).await?
        } else if saved.is_application() {
            match saved.str("fqdn") {
                Some(f) => f
                    .split(',')
                    .filter(|d| php::truthy(&PhpValue::Str((*d).to_owned())))
                    .map(str::to_owned)
                    .collect(),
                None => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let noindex = if saved.is_application() {
            ServiceRun::noindex_of(&saved)
        } else {
            Vec::new()
        };
        let sub_name = saved.str("human_name").unwrap_or(&saved.name).to_owned();
        let default_labels = service_default_labels(
            &uuid,
            &container_name,
            &service_project,
            &resource_name,
            &service_environment,
            saved.kind.sub_type(),
            &saved.uuid,
            &sub_name,
            &run.version,
        );

        if !is_database && !fqdns.is_empty() {
            php::put_str(
                &mut coolify_environments,
                "COOLIFY_FQDN",
                PhpValue::Str(
                    fqdns
                        .iter()
                        .map(|d| domains::get_host_without_port(d))
                        .collect::<Vec<_>>()
                        .join(","),
                ),
            );
            php::put_str(
                &mut coolify_environments,
                "COOLIFY_URL",
                PhpValue::Str(
                    fqdns
                        .iter()
                        .map(|d| domains::get_fqdn_without_port(d))
                        .collect::<Vec<_>>()
                        .join(","),
                ),
            );
        }
        let mut user_defined: Vec<String> = relation.iter().map(|r| r.key.clone()).collect();
        user_defined.extend(environment.iter().map(|(k, _)| k.as_string()));
        if run.service.raw.get("include_coolify_default_variables") != Some(&Value::Bool(false)) {
            let defaults = [
                ("COOLIFY_APP_NAME", Some(resource_name.clone())),
                ("COOLIFY_PROJECT_NAME", Some(service_project.clone())),
                (
                    "COOLIFY_ENVIRONMENT_NAME",
                    Some(service_environment.clone()),
                ),
                ("COOLIFY_SERVER_IP", Some(run.server.ip.clone())),
            ];
            for (k, v) in defaults {
                if let Some(v) = v
                    && !user_defined.iter().any(|u| u == k)
                {
                    php::put_str(
                        &mut coolify_environments,
                        k,
                        PhpValue::Str(v.replace('$', "$$")),
                    );
                }
            }
        }
        if !environment.is_empty() {
            let mut mapped: Arr = Vec::new();
            for (k, v) in environment.clone() {
                if k.as_string().starts_with("SERVICE_FQDN_") {
                    continue;
                }
                let mut value = v;
                if value == PhpValue::Str(String::new())
                    && let Some(row) = run.env_first(&k.as_string()).await?
                    && let Some(stored) = run
                        .decrypt(row.value_encrypted.as_deref())
                        .filter(|s| !s.is_empty())
                {
                    value = PhpValue::Str(stored);
                }
                if let PhpValue::Str(s) = &value
                    && s.contains("{{")
                {
                    value = PhpValue::Str(run.resolve_shared(s, false).await?);
                }
                mapped.push((k, value));
            }
            environment = mapped;
        }
        let mut service_labels: Vec<PhpValue> = labels_list;
        service_labels.extend(default_labels.into_iter().map(PhpValue::Str));
        if !service_labels.is_empty() && label_escape {
            service_labels = service_labels
                .iter()
                .map(labels::escape_dollar_sign)
                .collect();
        }
        if !is_database && !fqdns.is_empty() {
            let exact = run.server.setting_bool("generate_exact_labels");
            let redirect = match saved.str("redirect") {
                Some(r) if ["www", "non-www", "both"].contains(&r) => r.to_owned(),
                _ => "both".to_owned(),
            };
            let only_port = if saved.is_application() {
                application_required_port(
                    run.service.str("service_type"),
                    Some(&run.docker_compose_raw),
                    run.original.0.as_deref(),
                    &saved.name,
                )
            } else {
                templates::required_port(run.service.str("service_type"))
            };
            let overrides =
                overrides_map(json_to_php(saved.raw.get("domain_port_overrides")).as_ref());
            let traffic = run.server.setting_bool("is_traffic_analytics_enabled");
            let log_append = labels::caddy_supports_log_append(&run.server.proxy);
            let proxy_type = run.server.proxy_type().map(str::to_owned);
            if !use_network_mode && (!exact || proxy_type.as_deref() == Some("TRAEFIK")) {
                labels::add_traefik_docker_network_label(&mut service_labels, &base_network);
            }
            let resolver = run
                .server
                .certificate_resolver(saved.str("traefik_certificate_resolver"));
            let force_https = saved.bool("is_force_https_enabled");
            let permanent = saved.bool("is_permanent_redirect_enabled");
            let gzip = saved.bool_or("is_gzip_enabled", true);
            let stripprefix = saved.bool_or("is_stripprefix_enabled", true);
            macro_rules! proxy_args {
                ($current:expr) => {
                    ProxyLabelArgs {
                        uuid: &uuid,
                        network: &destination_network,
                        domains: &fqdns,
                        force_https,
                        permanent_redirect: permanent,
                        service_labels: Some($current),
                        gzip,
                        stripprefix,
                        service_name: Some(&service_name),
                        image: &image,
                        only_port,
                        noindex_domains: &noindex,
                        redirect_direction: &redirect,
                        port_overrides: &overrides,
                        certificate_resolver: resolver.as_deref(),
                        traffic_analytics: traffic,
                        supports_log_append: log_append,
                    }
                };
            }
            if exact {
                match proxy_type.as_deref() {
                    Some("TRAEFIK") => {
                        let l = labels::fqdn_labels_for_traefik(&proxy_args!(&service_labels));
                        service_labels.extend(l.into_iter().map(PhpValue::Str));
                    }
                    Some("CADDY") => {
                        let l = labels::fqdn_labels_for_caddy(&proxy_args!(&service_labels));
                        service_labels.extend(l.into_iter().map(PhpValue::Str));
                    }
                    _ => {}
                }
            } else {
                let l = labels::fqdn_labels_for_traefik(&proxy_args!(&service_labels));
                service_labels.extend(l.into_iter().map(PhpValue::Str));
                let l = labels::fqdn_labels_for_caddy(&proxy_args!(&service_labels));
                service_labels.extend(l.into_iter().map(PhpValue::Str));
            }
        }
        if php::get(&service, "restart") == Some(&PhpValue::Str("no".into()))
            || php::get(&service, "exclude_from_hc").is_some_and(php::truthy)
        {
            svc::sub_update(
                run.db,
                saved.kind,
                saved.id,
                &[("exclude_from_status", ColumnValue::Bool(true))],
                run.now,
            )
            .await
            .map_err(db_err)?;
        }

        // data_forget($service, 'volumes.*.content' ...) and 'exclude_from_hc'
        if let PhpValue::Array(items) = &mut service {
            if let Some(PhpValue::Array(vols)) = items
                .iter_mut()
                .find(|(k, _)| *k == Key::Str("volumes".into()))
                .map(|(_, v)| v)
            {
                for (_, v) in vols.iter_mut() {
                    if let PhpValue::Array(fields) = v {
                        php::forget(fields, "content");
                        php::forget(fields, "isDirectory");
                        php::forget(fields, "is_directory");
                    }
                }
            }
            php::forget(items, "exclude_from_hc");
        }
        for (_, v) in &mut volumes_parsed {
            if let PhpValue::Array(items) = v {
                php::forget(items, "content");
                php::forget(items, "is_directory");
                php::forget(items, "isDirectory");
            }
        }

        let base: Arr = php::collect(Some(&service));
        let mut payload = php::merge(
            &base,
            &[
                (
                    Key::Str("container_name".into()),
                    PhpValue::Str(container_name.clone()),
                ),
                (Key::Str("restart".into()), PhpValue::Str(restart.clone())),
                (Key::Str("labels".into()), PhpValue::list(service_labels)),
            ],
        );
        if !use_network_mode {
            php::put_str(&mut payload, "networks", PhpValue::Array(networks_temp));
        }
        if !ports.is_empty() {
            php::put_str(&mut payload, "ports", PhpValue::Array(ports.clone()));
        }
        if !volumes_parsed.is_empty() {
            php::put_str(&mut payload, "volumes", PhpValue::Array(volumes_parsed));
        }
        if !environment.is_empty() || !coolify_environments.is_empty() {
            let environment = preserve_legacy_null_assignments(environment, &legacy_null_variables);
            let merged = php::merge(
                &php::merge(&environment, &coolify_environments),
                &service_name_environments,
            );
            php::put_str(&mut payload, "environment", PhpValue::Array(merged));
        }
        if php::truthy(&logging) {
            php::put_str(&mut payload, "logging", logging);
        }
        if !depends_on.is_empty() {
            php::put_str(
                &mut payload,
                "depends_on",
                PhpValue::Array(depends_on.clone()),
            );
        }
        let mut env_files: Vec<PhpValue> = match php::get(&service, "env_file") {
            None | Some(PhpValue::Null) => Vec::new(),
            Some(PhpValue::Array(items)) => items.iter().map(|(_, v)| v.clone()).collect(),
            Some(other) => vec![other.clone()],
        };
        env_files.push(PhpValue::Str(concat!(".", "env").into()));
        let mut unique: Vec<PhpValue> = Vec::new();
        for f in env_files {
            if !unique
                .iter()
                .any(|u| php::loose_eq(u, &f) && php::to_string(u) == php::to_string(&f))
            {
                unique.push(f);
            }
        }
        php::put_str(&mut payload, "env_file", PhpValue::list(unique));
        php::put_str(
            &mut parsed_services,
            &service_name,
            PhpValue::Array(payload),
        );
    }

    let mut top: Arr = vec![(
        Key::Str("services".into()),
        PhpValue::Array(parsed_services),
    )];
    for (name, section) in [
        ("volumes", top_volumes),
        ("networks", top_networks),
        ("configs", top_configs),
        ("secrets", top_secrets),
    ] {
        if !section.is_empty() {
            top.push((Key::Str(name.into()), PhpValue::Array(section)));
        }
    }
    let top = PhpValue::Array(top);
    run.docker_compose = Some(coolify_compat::yaml::dump_with_indent(&top, 10, 2));

    // docker_compose_raw without the one-time volume fields
    if let Ok(original_yaml) = validate::parse_docker_compose_yaml(&original_compose) {
        let mut cleaned = original_yaml.clone();
        if let Some(PhpValue::Array(svcs)) = cleaned.get_mut("services") {
            for (_, s) in svcs.iter_mut() {
                if let Some(PhpValue::Array(vols)) = s.get_mut("volumes") {
                    for (_, vol) in vols.iter_mut() {
                        if let PhpValue::Array(items) = vol {
                            php::forget(items, "content");
                            php::forget(items, "isDirectory");
                            php::forget(items, "is_directory");
                        }
                    }
                }
            }
        }
        if cleaned != original_yaml {
            run.docker_compose_raw = volumes::remove_compose_volume_fields_preserving_comments(
                &original_compose,
                &cleaned,
                &["content", "isDirectory", "is_directory"],
            );
        }
    }
    run.env_cache = None;
    run.save().await?;
    sync_required_state(run).await?;

    Ok(ParsedCompose {
        top_level: top,
        warnings: run.warnings.clone(),
    })
}

/// `data_get($resource, 'destination.network')`.
async fn destination_network(run: &ServiceRun<'_>) -> Result<String, String> {
    svc::destination_network(run.db, &run.service)
        .await
        .map_err(db_err)
}

/// `defaultLabels(uuid, name, projectName, resourceName, 'service', subType, subUuid, subName, environment)`.
#[allow(clippy::too_many_arguments)]
fn service_default_labels(
    uuid: &str,
    name: &str,
    project_name: &str,
    resource_name: &str,
    environment: &str,
    sub_type: &str,
    sub_uuid: &str,
    sub_name: &str,
    version: &str,
) -> Vec<String> {
    let mut labels = vec![
        "coolify.managed=true".to_owned(),
        format!("coolify.version={version}"),
        format!("coolify.serviceUuid={uuid}"),
        "coolify.type=service".to_owned(),
        format!("coolify.name={}", php::slug(name)),
        format!("coolify.resourceName={}", php::slug(resource_name)),
        format!("coolify.projectName={}", php::slug(project_name)),
        format!("coolify.serviceName={}", php::slug(sub_name)),
        format!("coolify.environmentName={}", php::slug(environment)),
        "coolify.pullRequestId=0".to_owned(),
    ];
    if !sub_uuid.is_empty() && sub_uuid != "0" {
        labels.push(format!("coolify.service.subUuid={sub_uuid}"));
    }
    labels.push(format!("coolify.service.subType={sub_type}"));
    if !sub_name.is_empty() && sub_name != "0" {
        labels.push(format!("coolify.service.subName={}", php::slug(sub_name)));
    }
    labels
}

/// `updateCompose($resource)`.
pub async fn update_compose(
    jobs: &Jobs,
    service_id: i64,
    kind: SubKind,
    sub_id_name: &str,
    now: NaiveDateTime,
) -> Result<(), String> {
    let mut run = ServiceRun::load(jobs, service_id, now).await?;
    let Some(sub) = svc::sub_by_name(run.db, kind, service_id, sub_id_name)
        .await
        .map_err(db_err)?
    else {
        return Ok(());
    };
    let name = sub.name.clone();
    let raw = run.docker_compose_raw.clone();
    if raw.is_empty() {
        return Err("No compose file found or not a valid YAML file.".into());
    }
    let Ok(mut compose) = validate::parse_docker_compose_yaml(&raw) else {
        return Err("No compose file found or not a valid YAML file.".into());
    };

    // Switch Image
    let updated_image = sub.str("image").unwrap_or_default().to_owned();
    let current_image = php::get(&compose, "services")
        .and_then(|s| php::get(s, &name))
        .map(|s| php::data_get_str(s, "image", ""))
        .unwrap_or_default();
    // `$currentImage !== $updatedImage` compares two different Stringable objects: always true, so
    // the raw file is always dumped again and the image is always saved.
    let _ = current_image;
    if let Some(PhpValue::Array(svcs)) = compose.get_mut("services")
        && let Some((_, PhpValue::Array(fields))) =
            svcs.iter_mut().find(|(k, _)| k.as_string() == name)
    {
        php::put_str(fields, "image", PhpValue::Str(updated_image.clone()));
    }
    run.docker_compose_raw = coolify_compat::yaml::dump_with_indent(&compose, 10, 2);
    run.save().await?;
    svc::sub_update(
        run.db,
        kind,
        sub.id,
        &[("image", ColumnValue::Text(Some(updated_image)))],
        now,
    )
    .await
    .map_err(db_err)?;

    let services: Arr = match php::get(&compose, "services") {
        Some(PhpValue::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    let Some(config) = php::get_key(&services, &Key::from_php_string(&name)).cloned() else {
        return Ok(());
    };
    let mut template_names: Vec<String> = Vec::new();
    for declared in declared_url_names(&config) {
        if template_names.contains(&declared) {
            continue;
        }
        if run
            .is_copied_declaration(&services, &name, &declared)
            .await?
        {
            continue;
        }
        template_names.push(declared);
    }
    // serviceNamesToProcess: lowercase key => (base, ports)
    let mut to_process: Vec<(String, String, Vec<String>)> = Vec::new();
    for t in &template_names {
        let parsed = env::parse_service_environment_variable(t);
        let base = t
            .strip_prefix("SERVICE_URL_")
            .or_else(|| t.strip_prefix("SERVICE_FQDN_"))
            .unwrap_or_default();
        let service_name = if parsed.has_port {
            php::before_last(base, "_")
        } else {
            base.to_owned()
        };
        let lower = service_name.to_lowercase();
        let idx = match to_process.iter().position(|e| e.0 == lower) {
            Some(i) => i,
            None => {
                to_process.push((lower, service_name.clone(), Vec::new()));
                to_process.len() - 1
            }
        };
        if parsed.has_port
            && let Some(p) = parsed
                .port
                .as_deref()
                .filter(|p| php::truthy(&PhpValue::Str((*p).to_owned())))
        {
            to_process[idx].2.push(p.to_owned());
        }
    }
    for (_, base, ports) in &to_process {
        for prefix in ["SERVICE_URL_", "SERVICE_FQDN_"] {
            svc::env_delete_key(run.db, service_id, &format!("{prefix}{base}"))
                .await
                .map_err(db_err)?;
        }
        for p in ports {
            for prefix in ["SERVICE_URL_", "SERVICE_FQDN_"] {
                svc::env_delete_key(run.db, service_id, &format!("{prefix}{base}_{p}"))
                    .await
                    .map_err(db_err)?;
            }
        }
    }
    let fqdn = sub.str("fqdn").map(str::to_owned);
    if fqdn.as_deref().is_some_and(|f| !f.is_empty() && f != "0") {
        let first = first_domain_from_list(fqdn.as_deref());
        let url = SpatieUrl::from_string(&first)?;
        let port = url.port;
        let url_value = domains::get_fqdn_without_port(&first);
        let fqdn_value = domains::get_host_without_port(&first);
        for (_, base, ports) in &to_process {
            run.env_update_or_create(&format!("SERVICE_URL_{base}"), Some(&url_value), None)
                .await?;
            run.env_update_or_create(&format!("SERVICE_FQDN_{base}"), Some(&fqdn_value), None)
                .await?;
            let mut all: Vec<String> = Vec::new();
            for p in ports {
                if !all.contains(p) {
                    all.push(p.clone());
                }
            }
            if let Some(p) = port
                && !all.contains(&p.to_string())
            {
                all.push(p.to_string());
            }
            for p in all {
                run.env_update_or_create(
                    &format!("SERVICE_URL_{base}_{p}"),
                    Some(&format!("{url_value}:{p}")),
                    None,
                )
                .await?;
                run.env_update_or_create(
                    &format!("SERVICE_FQDN_{base}_{p}"),
                    Some(&format!("{fqdn_value}:{p}")),
                    None,
                )
                .await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_comments() {
        let raw = "services:\n  a:\n    environment:\n      - A=1 # first\n      B: \"x#y\"  # second\n      C: 3\n    image: z # no\n";
        let c = extract_yaml_environment_comments(raw);
        assert_eq!(c.get("A").map(String::as_str), Some("first"));
        assert_eq!(c.get("B").map(String::as_str), Some("second"));
        assert_eq!(c.get("C"), None);
        assert_eq!(c.get("image"), None);
    }

    #[test]
    fn fqdn_port_check() {
        assert!(fqdn_has_port(Some("http://a.b:3000")));
        assert!(!fqdn_has_port(Some("http://a.b")));
        assert!(!fqdn_has_port(None));
    }
}
