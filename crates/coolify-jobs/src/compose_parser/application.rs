//! `applicationParser(Application $resource, int $pull_request_id, ?int $preview_id, ?string $commit)`
//! (bootstrap/helpers/parsers.php): parses `docker_compose_raw`, writes the environment variables,
//! storages and domains it derives, saves `docker_compose` and returns the compose structure.
//!
//! Production deployments (`pull_request_id = 0`) only.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use chrono::NaiveDateTime;
use coolify_compat::Encrypter;
use coolify_compat::yaml::{Key, PhpValue};
use coolify_db::Db;
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::compose::{self as store, ApplicationComposeUpdate, EnvRow, EnvUpdate, NewEnv};
use serde_json::Value;

use super::ParsedCompose;
use super::domains::{self, UrlBase};
use super::env;
use super::labels::{self, ProxyLabelArgs};
use super::php::{self, Arr, SpatieUrl};
use super::service;
use super::validate;
use super::volumes;
use crate::Jobs;

const MORPH: &str = apps::MORPH;
const RESTART_MODE: &str = "unless-stopped";

/// `ValidationPatterns::ENVIRONMENT_VARIABLE_KEY_PATTERN`.
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

/// The compose columns of the application as the model holds them in memory.
#[derive(Debug, Clone, PartialEq)]
struct Attrs {
    fqdn: Option<String>,
    domain_port_overrides: Option<PhpValue>,
    noindex_domains: Option<PhpValue>,
    docker_compose_domains: Option<String>,
    docker_compose: Option<String>,
    docker_compose_raw: Option<String>,
}

/// The server the application deploys to, as the parser reads it.
struct ServerInfo {
    id: i64,
    ip: String,
    settings: Value,
    proxy: Value,
    url_base: UrlBase,
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

/// One run of the parser: the application model as loaded (and changed in memory), its server
/// and the database.
pub struct Run<'a> {
    db: &'a Db,
    enc: &'a Encrypter,
    now: NaiveDateTime,
    version: String,
    app: ApplicationRow,
    attrs: Attrs,
    original: Attrs,
    server: ServerInfo,
    /// `$resource->environment_variables` once loaded (the relation is cached until `data_forget`).
    env_cache: Option<Vec<EnvRow>>,
    warnings: Vec<String>,
    jobs: Jobs,
}

impl std::fmt::Debug for Run<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Run")
            .field("application", &self.app.uuid)
            .finish_non_exhaustive()
    }
}

fn json_value_to_php(v: Option<&Value>) -> Option<PhpValue> {
    match v {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => php::json_decode(s),
        Some(other) => php::json_decode(&other.to_string()),
    }
}

impl<'a> Run<'a> {
    /// Loads the application, its server and the instance settings.
    pub async fn load(jobs: &'a Jobs, app_id: i64, now: NaiveDateTime) -> Result<Run<'a>, String> {
        let db = &jobs.0.db;
        let enc = jobs
            .0
            .encrypter
            .as_ref()
            .ok_or("APP_KEY is not configured.")?;
        let app = apps::find_by_id(db, app_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Application {app_id} not found."))?;
        let server_id = app
            .server_id()
            .ok_or("The application has no standalone Docker destination.")?;
        let server_row = coolify_db::servers::find_by_id(db, server_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("Server not found.")?;
        let settings = server_row
            .settings
            .clone()
            .map(Value::Object)
            .unwrap_or(Value::Null);
        let ip = server_row
            .record
            .get("ip")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let proxy = match server_row.record.get("proxy") {
            Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
            Some(v) => v.clone(),
            None => Value::Null,
        };
        let instance = coolify_db::instance::deployment_settings(db)
            .await
            .map_err(|e| e.to_string())?;
        let public = instance.public_ipv4.or(instance.public_ipv6);
        let url_base = UrlBase::for_server(
            settings.get("wildcard_domain").and_then(Value::as_str),
            &ip,
            public.as_deref(),
        )
        .await;
        let attrs = Attrs {
            fqdn: app.str("fqdn").map(str::to_owned),
            domain_port_overrides: json_value_to_php(app.raw.get("domain_port_overrides")),
            noindex_domains: json_value_to_php(app.raw.get("noindex_domains")),
            docker_compose_domains: app.str("docker_compose_domains").map(str::to_owned),
            docker_compose: app.str("docker_compose").map(str::to_owned),
            docker_compose_raw: app.str("docker_compose_raw").map(str::to_owned),
        };
        Ok(Run {
            db,
            enc,
            now,
            version: jobs.version(),
            original: attrs.clone(),
            attrs,
            app,
            server: ServerInfo {
                id: server_row.id,
                ip,
                settings,
                proxy,
                url_base,
            },
            env_cache: None,
            warnings: Vec::new(),
            jobs: jobs.clone(),
        })
    }

    /// The application row as loaded.
    pub(crate) fn app(&self) -> &ApplicationRow {
        &self.app
    }

    pub(crate) fn db(&self) -> &Db {
        self.db
    }

    pub(crate) fn encrypter(&self) -> &Encrypter {
        self.enc
    }

    pub(crate) fn docker_compose_raw(&self) -> Option<&str> {
        self.attrs.docker_compose_raw.as_deref()
    }

    pub(crate) fn set_docker_compose_raw(&mut self, raw: Option<String>) {
        self.attrs.docker_compose_raw = raw;
    }

    pub(crate) fn docker_compose_domains(&self) -> Option<&str> {
        self.attrs.docker_compose_domains.as_deref()
    }

    pub(crate) fn set_docker_compose_domains(&mut self, domains: Option<String>) {
        self.attrs.docker_compose_domains = domains;
    }

    fn uuid(&self) -> String {
        self.app.uuid.clone()
    }

    fn build_pack_is_compose(&self) -> bool {
        self.app.str("build_pack") == Some("dockercompose")
    }

    pub(crate) fn parsing_version(&self) -> i64 {
        self.app
            .str("compose_parsing_version")
            .and_then(|v| v.trim().parse().ok())
            .or_else(|| self.app.int("compose_parsing_version"))
            .unwrap_or(5)
    }

    fn setting_default(&self, key: &str, default: bool) -> bool {
        self.app
            .settings
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(default)
    }

    fn db_err(e: coolify_db::DbError) -> String {
        e.to_string()
    }

    // --- the Application model -------------------------------------------------------------

    /// `$resource->save()`: the `saving` hook (fqdn normalization and noindex sync), then the
    /// dirty columns.
    pub(crate) async fn save(&mut self) -> Result<(), String> {
        if self.attrs.fqdn != self.original.fqdn {
            if self.attrs.fqdn.as_deref() == Some("") {
                self.attrs.fqdn = None;
            }
            if !self.build_pack_is_compose() || php::filled_str(self.attrs.fqdn.as_deref()) {
                let existing = overrides_map(self.attrs.domain_port_overrides.as_ref());
                let (fqdn, overrides) = crate::applications::domains::normalize_port_overrides(
                    self.attrs.fqdn.as_deref(),
                    &existing,
                );
                self.attrs.domain_port_overrides = overrides.map(|o| {
                    php::json_decode(&crate::applications::domains::overrides_json(
                        fqdn.as_deref(),
                        &o,
                    ))
                    .unwrap_or(PhpValue::Null)
                });
                self.attrs.fqdn = fqdn;
            }
            self.sync_noindex_domains();
        }
        let mut u = ApplicationComposeUpdate::default();
        let a = &self.attrs;
        let o = &self.original;
        if a.fqdn != o.fqdn {
            u.fqdn = Some(a.fqdn.clone());
        }
        if a.domain_port_overrides != o.domain_port_overrides {
            u.domain_port_overrides = Some(a.domain_port_overrides.as_ref().map(php::json_encode));
        }
        if a.noindex_domains != o.noindex_domains {
            u.noindex_domains = Some(a.noindex_domains.as_ref().map(php::json_encode));
        }
        if a.docker_compose_domains != o.docker_compose_domains {
            u.docker_compose_domains = Some(a.docker_compose_domains.clone());
        }
        if a.docker_compose != o.docker_compose {
            u.docker_compose = Some(a.docker_compose.clone());
        }
        if a.docker_compose_raw != o.docker_compose_raw {
            u.docker_compose_raw = Some(a.docker_compose_raw.clone());
        }
        store::application_update(self.db, self.app.id, &u, self.now)
            .await
            .map_err(Self::db_err)?;
        self.original = self.attrs.clone();
        Ok(())
    }

    /// `HasNoindexDomains::syncNoindexDomains()`.
    fn sync_noindex_domains(&mut self) {
        let Some(PhpValue::Array(current)) = self.attrs.noindex_domains.clone() else {
            return;
        };
        if current.is_empty() {
            return;
        }
        let normalize = |d: &str| {
            crate::applications::domains::without_port(
                &crate::applications::domains::normalize_url(d),
            )
        };
        let mut domains: Vec<String> =
            crate::applications::domains::domain_list(self.attrs.fqdn.as_deref());
        for (_, entry) in php::json_array(self.attrs.docker_compose_domains.as_deref()) {
            domains.extend(crate::applications::domains::domain_list(
                domains::compose_domain_entry_string(&entry).as_deref(),
            ));
        }
        let domains: Vec<String> = domains.iter().map(|d| normalize(d)).collect();
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
        self.attrs.noindex_domains = Some(PhpValue::strings(out));
    }

    /// `$resource->noindexDomains()`.
    fn noindex_domains(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if let Some(PhpValue::Array(items)) = &self.attrs.noindex_domains {
            for (_, d) in items {
                if let PhpValue::Str(s) = d
                    && !s.trim().is_empty()
                {
                    let n = crate::applications::domains::without_port(
                        &crate::applications::domains::normalize_url(s),
                    );
                    if !out.contains(&n) {
                        out.push(n);
                    }
                }
            }
        }
        out
    }

    // --- environment variables -------------------------------------------------------------

    fn decrypt(&self, raw: Option<&str>) -> Option<String> {
        let raw = raw.filter(|r| !r.is_empty() && *r != "0")?;
        self.enc.decrypt_maybe_serialized(raw).ok().map(|v| {
            v.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B'])
                .to_owned()
        })
    }

    fn encrypt(&self, value: &str) -> String {
        self.enc.encrypt_serialized(value)
    }

    fn is_shared_reference(value: Option<&str>) -> bool {
        value.is_some_and(|v| {
            !v.trim().is_empty()
                && SHARED_REFERENCE.is_match(v.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B']))
        })
    }

    /// `$resource->environment_variables()->firstOrCreate(['key' => ...], $values)` with the
    /// model's `created` hook (the preview copy). `value`: `None` when not given, `Some(None)`
    /// for null.
    async fn env_first_or_create(
        &mut self,
        key: &str,
        value: Option<Option<&str>>,
        is_required: Option<bool>,
    ) -> Result<EnvRow, String> {
        if let Some(row) = store::env_first(self.db, MORPH, self.app.id, key, false)
            .await
            .map_err(Self::db_err)?
        {
            return Ok(row);
        }
        self.env_create(key, value.flatten(), is_required).await
    }

    async fn env_create(
        &mut self,
        key: &str,
        value: Option<&str>,
        is_required: Option<bool>,
    ) -> Result<EnvRow, String> {
        let key = key.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B']);
        if !ENV_KEY.is_match(key) {
            return Err("The key must start with a letter or underscore and may only contain letters, numbers, underscores, and dots.".into());
        }
        let trimmed = value.map(|v| {
            v.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B'])
                .to_owned()
        });
        let new = NewEnv {
            key: key.to_owned(),
            value_encrypted: trimmed.as_deref().map(|v| self.encrypt(v)),
            is_preview: false,
            is_required: is_required.unwrap_or(false),
            is_multiline: false,
            is_literal: false,
            is_shown_once: false,
            is_shared: Self::is_shared_reference(trimmed.as_deref()),
            comment: None,
        };
        let id = store::env_insert(self.db, MORPH, self.app.id, &new, &self.version, self.now)
            .await
            .map_err(Self::db_err)?;
        // The `created` hook: a preview copy unless one exists.
        if store::env_first(self.db, MORPH, self.app.id, key, true)
            .await
            .map_err(Self::db_err)?
            .is_none()
        {
            let copy = NewEnv {
                is_preview: true,
                value_encrypted: trimmed.as_deref().map(|v| self.encrypt(v)),
                ..new.clone()
            };
            store::env_insert(self.db, MORPH, self.app.id, &copy, &self.version, self.now)
                .await
                .map_err(Self::db_err)?;
        }
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

    /// `$resource->environment_variables()->updateOrCreate(['key' => ...], ['value' => ..., 'is_preview' => false])`.
    async fn env_update_or_create(&mut self, key: &str, value: Option<&str>) -> Result<(), String> {
        let Some(row) = store::env_first(self.db, MORPH, self.app.id, key, false)
            .await
            .map_err(Self::db_err)?
        else {
            self.env_create(key, value, None).await?;
            return Ok(());
        };
        let new = value.map(|v| {
            v.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B'])
                .to_owned()
        });
        let old = self.decrypt_raw_exact(row.value_encrypted.as_deref());
        let mut u = EnvUpdate::default();
        if old != new {
            u.value_encrypted = Some(new.as_deref().map(|v| self.encrypt(v)));
            // `saving`: a legacy null marker ends when the value is set
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
            .map_err(Self::db_err)
    }

    /// The stored plaintext for the dirty check (`null` stays null, untrimmed casts compare
    /// the trimmed value the mutator stored).
    fn decrypt_raw_exact(&self, raw: Option<&str>) -> Option<String> {
        let raw = raw?;
        self.enc.decrypt_maybe_serialized(raw).ok()
    }

    /// `$variable->update(['is_required' => $required])`.
    async fn env_set_required(&mut self, row: &EnvRow, required: bool) -> Result<(), String> {
        let value = self.decrypt(row.value_encrypted.as_deref());
        let shared = Self::is_shared_reference(value.as_deref());
        let u = EnvUpdate {
            is_required: (row.is_required != required).then_some(required),
            is_shared: (shared != row.is_shared).then_some(shared),
            ..EnvUpdate::default()
        };
        store::env_update(self.db, row.id, &u, self.now)
            .await
            .map_err(Self::db_err)
    }

    /// `$resource->environment_variables` (loaded once, then cached).
    async fn env_relation(&mut self) -> Result<Vec<EnvRow>, String> {
        if self.env_cache.is_none() {
            self.env_cache = Some(
                store::env_list(self.db, MORPH, self.app.id, false)
                    .await
                    .map_err(Self::db_err)?,
            );
        }
        Ok(self.env_cache.clone().unwrap_or_default())
    }

    /// `resolveSharedEnvironmentVariables($value, $resource)` (`server` resolves too for
    /// `real_value`).
    async fn resolve_shared(&self, value: &str, with_server: bool) -> Result<String, String> {
        let mut value = value
            .trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B'])
            .to_owned();
        let found: Vec<String> = SHARED_PLACEHOLDER
            .captures_iter(&value)
            .map(|c| c[1].to_owned())
            .collect();
        for shared in found {
            let t = shared.trim();
            let kind = match t.find('.') {
                Some(p) => t[..p].to_owned(),
                None => continue,
            };
            let variable = t[t.find('.').map_or(0, |p| p + 1)..].to_owned();
            let id = match kind.as_str() {
                "environment" => self.app.environment.get("id").and_then(Value::as_i64),
                "project" => self.app.project.get("id").and_then(Value::as_i64),
                "team" => Some(self.app.team_id),
                "server" if with_server => Some(self.server.id),
                _ => None,
            };
            let Some(id) = id else { continue };
            let row = if kind == "server" {
                coolify_db::environment_variables::shared(
                    self.db,
                    self.app.team_id,
                    &kind,
                    &variable,
                    id,
                )
                .await
                .map_err(Self::db_err)?
                .map(|(v, _)| v)
            } else {
                store::shared_variable_value(self.db, self.app.team_id, &kind, &variable, id)
                    .await
                    .map_err(Self::db_err)?
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
        let row = store::env_first(self.db, MORPH, self.app.id, &key, false)
            .await
            .map_err(Self::db_err)?;
        let runtime_ok = match &row {
            Some(_) => {
                let is_control = key.starts_with("NIXPACKS_") || key.starts_with("RAILPACK_");
                !is_control
                    || [
                        "NIXPACKS_PHP_ROOT_DIR",
                        "NIXPACKS_PHP_FALLBACK_PATH",
                        "NIXPACKS_SPA_OUTPUT_DIR",
                        "RAILPACK_SKIP_MIGRATIONS",
                    ]
                    .contains(&key.as_str())
            }
            None => false,
        };
        let value = match row.filter(|_| runtime_ok) {
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
    let name_v = match name {
        Key::Int(i) => PhpValue::Int(*i),
        Key::Str(s) => PhpValue::Str(s.clone()),
    };
    let key_v = match key {
        Key::Int(i) => PhpValue::Int(*i),
        Key::Str(s) => PhpValue::Str(s.clone()),
    };
    php::loose_eq(value, &name_v) || php::loose_eq(&key_v, &name_v)
}

/// `applicationParser($resource, $pull_request_id, $preview_id, $commit)`.
#[allow(clippy::too_many_lines)]
pub(crate) async fn application_parser(
    run: &mut Run<'_>,
    pull_request_id: i64,
    commit: Option<&str>,
) -> Result<ParsedCompose, String> {
    if pull_request_id != 0 {
        return Err(
            "Preview deployments of Docker Compose applications are not ported yet.".into(),
        );
    }
    run.warnings.clear();
    let uuid = run.uuid();
    let Some(compose) = run
        .attrs
        .docker_compose_raw
        .clone()
        .filter(|c| php::truthy(&PhpValue::Str(c.clone())))
    else {
        return Ok(ParsedCompose::empty());
    };
    let original_compose = compose.clone();
    let Ok(yaml) = validate::parse_docker_compose_yaml(&compose) else {
        return Ok(ParsedCompose::empty());
    };
    let services: Arr = match php::get(&yaml, "services") {
        Some(PhpValue::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    let service_names: Vec<String> = services.iter().map(|(k, _)| k.as_string()).collect();
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
            Some(Some(default.as_deref().unwrap_or(""))),
            None,
        )
        .await?;
    }
    if !top_volumes.is_empty() {
        top_volumes.retain(|(_, v)| *v != PhpValue::Null);
    }
    let base_network = uuid.clone();

    let legacy_null_variables = store::env_list(run.db, MORPH, run.app.id, false)
        .await
        .map_err(Run::db_err)?;

    let mut all_magic: Arr = Vec::new();
    for (name_key, service) in &services {
        let service_name = name_key.as_string();
        validate::validate_shell_safe_path(&service_name, "service name").map_err(|e| {
            format!("Invalid Docker Compose service name: {e} Service names must not contain shell metacharacters.")
        })?;
        validate::validate_compose_service_scale(&service_name, service)?;

        let environment = env::convert_to_key_value_collection(&php::merge(
            &php::collect(php::get(service, "environment")),
            &php::collect(php::data_get(service, "build.args")),
        ));
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
            if key.starts_with("SERVICE_FQDN_") {
                let parsed = env::parse_service_environment_variable(&key);
                let mut fqdn = run.attrs.fqdn.clone().unwrap_or_default();
                if !php::filled_str(run.attrs.fqdn.as_deref()) {
                    fqdn = run.server.url_base.generate_fqdn(
                        &uuid,
                        run.parsing_version(),
                        &run.version,
                    );
                }
                if value.starts_with('/') && value != "/" {
                    fqdn = format!("{fqdn}{value}");
                }
                let fqdn_with_port = match &parsed.port {
                    Some(p) => format!("{fqdn}:{p}"),
                    None => fqdn.clone(),
                };
                if run.attrs.fqdn.is_none() {
                    run.env_cache = None;
                    run.attrs.fqdn = Some(fqdn_with_port);
                    run.save().await?;
                }
                let env_key = if parsed.has_port {
                    php::before_last(&key, "_")
                } else {
                    key.clone()
                };
                run.env_update_or_create(&env_key, Some(&fqdn)).await?;
            }
            if run.build_pack_is_compose()
                && (key.starts_with("SERVICE_FQDN_") || key.starts_with("SERVICE_URL_"))
            {
                let parsed = env::parse_service_environment_variable(&key);
                let normalized = domains::normalize_compose_service_name(&parsed.service_name);
                if let Some(original) =
                    domains::find_compose_service_name(&normalized, &service_names)
                {
                    let current = php::json_array(run.attrs.docker_compose_domains.as_deref());
                    if !domains::has_compose_service_domain_entry(&current, &original) {
                        let for_domain = parsed.service_name.replace('_', "-");
                        let mut domain_value = run
                            .server
                            .url_base
                            .generate_url(&format!("{for_domain}-{uuid}"));
                        if value.starts_with('/') && value != "/" {
                            domain_value = format!("{domain_value}{value}");
                        }
                        if let Some(p) = parsed
                            .port
                            .as_deref()
                            .filter(|p| coolify_compat::yaml::php_is_numeric(p))
                        {
                            domain_value = format!("{domain_value}:{p}");
                        }
                        let updated = domains::put_compose_service_domain(
                            &current,
                            &original,
                            Some(&domain_value),
                            &service_names,
                        );
                        run.attrs.docker_compose_domains =
                            Some(php::json_encode(&PhpValue::Array(updated)));
                        run.save().await?;
                    }
                }
            }
        }
        for m in &magic {
            php::put_str(&mut all_magic, m, PhpValue::Str(String::new()));
        }
        for key in magic {
            let command = env::parse_command_from_magic_env_variable(&key);
            if command == "FQDN" || command == "URL" {
                let parsed = env::parse_service_environment_variable(&key);
                let preserved = match (parsed.has_port, key.starts_with("SERVICE_URL_")) {
                    (true, true) => php::before_last(&php::after(&key, "SERVICE_URL_"), "_"),
                    (true, false) => php::before_last(&php::after(&key, "SERVICE_FQDN_"), "_"),
                    (false, true) => php::after(&key, "SERVICE_URL_"),
                    (false, false) => php::after(&key, "SERVICE_FQDN_"),
                };
                let original_name = parsed.service_name.replace('_', "-");
                let normalized = domains::normalize_compose_service_name(&parsed.service_name);
                let fqdn = run.server.url_base.generate_fqdn(
                    &format!("{original_name}-{uuid}"),
                    run.parsing_version(),
                    &run.version,
                );
                let url = run
                    .server
                    .url_base
                    .generate_url(&format!("{original_name}-{uuid}"));
                let fqdn_env = php::after(&fqdn, "://");
                let port = parsed
                    .port
                    .clone()
                    .filter(|p| coolify_compat::yaml::php_is_numeric(p));
                let (url_with_port, fqdn_env_with_port) = match &port {
                    Some(p) => (format!("{url}:{p}"), format!("{fqdn_env}:{p}")),
                    None => (url.clone(), fqdn_env.clone()),
                };
                run.env_first_or_create(
                    &format!("SERVICE_FQDN_{preserved}"),
                    Some(Some(&fqdn_env)),
                    None,
                )
                .await?;
                run.env_first_or_create(
                    &format!("SERVICE_URL_{preserved}"),
                    Some(Some(&url)),
                    None,
                )
                .await?;
                if let (true, Some(p)) = (
                    parsed.has_port,
                    parsed
                        .port
                        .as_deref()
                        .filter(|p| php::truthy(&PhpValue::Str((*p).to_owned()))),
                ) {
                    run.env_first_or_create(
                        &format!("SERVICE_FQDN_{preserved}_{p}"),
                        Some(Some(&fqdn_env_with_port)),
                        None,
                    )
                    .await?;
                    run.env_first_or_create(
                        &format!("SERVICE_URL_{preserved}_{p}"),
                        Some(Some(&url_with_port)),
                        None,
                    )
                    .await?;
                }
                if run.build_pack_is_compose()
                    && let Some(compose_service) =
                        domains::find_compose_service_name(&normalized, &service_names)
                {
                    let current = php::json_array(run.attrs.docker_compose_domains.as_deref());
                    let domain_value = if parsed
                        .port
                        .as_deref()
                        .is_some_and(|p| php::truthy(&PhpValue::Str(p.to_owned())))
                    {
                        url_with_port.clone()
                    } else {
                        url.clone()
                    };
                    if !domains::has_compose_service_domain_entry(&current, &compose_service) {
                        let updated = domains::put_compose_service_domain(
                            &current,
                            &compose_service,
                            Some(&domain_value),
                            &service_names,
                        );
                        run.attrs.docker_compose_domains =
                            Some(php::json_encode(&PhpValue::Array(updated)));
                        run.save().await?;
                    }
                }
            } else {
                let secret = if command.starts_with("SUPABASE") {
                    match store::env_first(run.db, MORPH, run.app.id, "SERVICE_PASSWORD_JWT", false)
                        .await
                        .map_err(Run::db_err)?
                    {
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
                run.env_first_or_create(&key, Some(value.as_deref()), None)
                    .await?;
            }
        }
    }

    let service_name_environments = if run.build_pack_is_compose() {
        env::generate_docker_compose_service_name(&services, pull_request_id)
    } else {
        Vec::new()
    };

    let mut parsed_services: Arr = Vec::new();
    for (name_key, service_value) in &services {
        let service_name = name_key.as_string();
        let mut service = service_value.clone();
        let image = php::data_get_str(&service, "image", "");
        let restart = php::data_get_str(&service, "restart", RESTART_MODE);
        let mut logging = php::get(&service, "logging")
            .cloned()
            .unwrap_or(PhpValue::Null);
        if service::is_log_drain_enabled(&run.server.settings)
            && run.setting_default("is_log_drain_enabled", false)
        {
            logging = service::generate_log_drain_configuration(&run.server.settings);
        }
        let volumes = php::collect(php::get(&service, "volumes"));
        let mut networks = php::collect(php::get(&service, "networks"));
        let use_network_mode =
            php::get(&service, "network_mode").is_some_and(|v| *v != PhpValue::Null);
        let depends_on = php::collect(php::get(&service, "depends_on"));
        let mut labels: Vec<PhpValue> = php::collect(php::get(&service, "labels"))
            .into_iter()
            .map(|(_, v)| v)
            .collect();
        let raw_labels = php::collect(php::get(&service, "labels"));
        if !raw_labels.is_empty() && php::is_associative(&raw_labels) {
            labels = raw_labels
                .iter()
                .map(|(k, v)| PhpValue::Str(format!("{}={}", k.as_string(), php::to_string(v))))
                .collect();
        }
        let ports = php::collect(php::get(&service, "ports"));
        let mut environment = env::convert_to_key_value_collection(&php::merge(
            &php::collect(php::get(&service, "environment")),
            &php::collect(php::data_get(&service, "build.args")),
        ));
        let mut coolify_environments: Arr = Vec::new();

        let base_name = container_base_name(run, pull_request_id);
        let container_name = format!("{service_name}-{base_name}");

        let mut volumes_parsed: Arr = Vec::new();
        for (index, volume) in &volumes {
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
                    found = store::file_volume_by_mount(run.db, MORPH, run.app.id, &target)
                        .await
                        .map_err(Run::db_err)?;
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
                    found = store::file_volume_by_mount(run.db, MORPH, run.app.id, &target)
                        .await
                        .map_err(Run::db_err)?;
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
            if kind == "bind" {
                if ["/var/run/docker.sock", "/tmp", "/tmp/"].contains(&source.as_str()) {
                    volume = match &volume {
                        PhpValue::Str(_) => {
                            let mut s = format!("{source}:{target}");
                            if let Some(m) = mode
                                .as_deref()
                                .filter(|m| php::truthy(&PhpValue::Str((*m).to_owned())))
                            {
                                s.push(':');
                                s.push_str(m);
                            }
                            PhpValue::Str(s)
                        }
                        PhpValue::Array(items) => {
                            PhpValue::Array(volumes::compose_long_bind_volume(items, &source))
                        }
                        other => other.clone(),
                    };
                } else {
                    let main_directory =
                        format!("{}/applications/{uuid}", volumes::BASE_CONFIGURATION_DIR);
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
                                .map_err(Run::db_err)?;
                            }
                            None => {
                                let id = store::file_volume_insert(
                                    run.db,
                                    MORPH,
                                    run.app.id,
                                    &source,
                                    &target,
                                    content_enc.as_deref(),
                                    is_directory,
                                    run.now,
                                )
                                .await
                                .map_err(Run::db_err)?;
                                // `LocalFileVolume::created`: ServerStorageSaveJob.
                                run.jobs.spawn_created_application_file(run.app.id, id);
                            }
                        }
                    } else {
                        store::forget_file_volumes(run.db, MORPH, run.app.id, &target)
                            .await
                            .map_err(Run::db_err)?;
                    }
                    volume = match &volume {
                        PhpValue::Array(items) => {
                            PhpValue::Array(volumes::compose_long_bind_volume(items, &source))
                        }
                        _ => {
                            let mut s = format!("{source}:{target}");
                            if let Some(m) = mode
                                .as_deref()
                                .filter(|m| php::truthy(&PhpValue::Str((*m).to_owned())))
                            {
                                s.push(':');
                                s.push_str(m);
                            }
                            PhpValue::Str(s)
                        }
                    };
                }
            } else if kind == "volume" {
                let declaration =
                    php::get_key(&top_volumes, &Key::from_php_string(&source)).cloned();
                let is_network_volume = matches!(
                    declaration.as_ref().and_then(|d| php::data_get(d, "driver_opts.type")),
                    Some(PhpValue::Str(t)) if t == "cifs" || t == "nfs"
                );
                let legacy_name = format!("{uuid}_{}", php::slug(&source));
                if use_external_as_written(run, &top_volumes, &source, &legacy_name).await? {
                    php::put(&mut volumes_parsed, index.clone(), volume);
                    continue;
                }
                if is_network_volume {
                    php::put(&mut volumes_parsed, index.clone(), volume);
                    continue;
                }
                let name = format!("{uuid}_{}", php::slug(&source));
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
                    run.db, MORPH, run.app.id, &name, &target, run.now,
                )
                .await
                .map_err(Run::db_err)?;
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
            php::put(&mut volumes_parsed, index.clone(), volume);
        }

        let mut networks_temp: Arr = Vec::new();
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
            for (key, network) in &networks {
                match network {
                    PhpValue::Str(s) => php::put_str(&mut networks_temp, s, PhpValue::Null),
                    PhpValue::Array(_) => {
                        php::put(&mut networks_temp, key.clone(), network.clone())
                    }
                    _ => {}
                }
            }
            php::put_str(&mut networks_temp, &base_network, PhpValue::Null);
            if run.setting_default("connect_to_docker_network", false) {
                let network = run.app.network().to_owned();
                php::put_str(&mut networks_temp, &network, PhpValue::Null);
                php::put_str(
                    &mut top_networks,
                    &network,
                    PhpValue::map([
                        ("name", PhpValue::Str(network.clone())),
                        ("external", PhpValue::Bool(true)),
                    ]),
                );
            }
        }

        let passthrough =
            env::extract_docker_compose_passthrough_keys(php::get(&service, "environment"));
        let magic_keys: Vec<Key> = all_magic.iter().map(|(k, _)| k.clone()).collect();
        let normal: Arr = environment
            .iter()
            .filter(|(k, _)| !magic_keys.iter().any(|m| m.as_string() == k.as_string()))
            .filter(|(_, v)| !php::to_string(v).starts_with("SERVICE_"))
            .cloned()
            .collect();
        for (key, value) in normal {
            let key = key.as_string();
            let value = php::to_string(&value);
            let parsed_value = env::replace_variables(&value);
            if value.is_empty() && passthrough.contains(&key) {
                run.env_first_or_create(&key, None, None).await?;
                continue;
            }
            for reference in env::extract_docker_compose_environment_variable_references(&value) {
                if reference.starts_with("SERVICE_") {
                    continue;
                }
                let row = run.env_first_or_create(&reference, None, None).await?;
                let compose_text = run
                    .attrs
                    .docker_compose_raw
                    .clone()
                    .or_else(|| run.attrs.docker_compose.clone());
                let required = env::docker_compose_environment_variable_is_required(
                    compose_text.as_deref(),
                    &reference,
                );
                if row.is_required != required {
                    run.env_set_required(&row, required).await?;
                }
            }
            if value.starts_with("$SERVICE_") {
                run.env_first_or_create(&key, Some(Some(&value)), None)
                    .await?;
                continue;
            }
            if !value.starts_with('$') {
                continue;
            }
            if key == parsed_value {
                run.env_first_or_create(&key, None, None).await?;
                continue;
            }
            match env::extract_balanced_brace_content(&value, 0) {
                Some((content, _, _)) => match env::split_on_operator_outside_nested(&content) {
                    Some((var, op, default)) => {
                        let required = op.contains('?');
                        let value_for_create = if required {
                            None
                        } else {
                            Some(default.as_str())
                        };
                        run.env_first_or_create(&var, Some(value_for_create), Some(required))
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
                                            run.env_first_or_create(v, Some(Some(d)), None).await?;
                                        }
                                        None => {
                                            run.env_first_or_create(&nested_content, None, None)
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
                        run.env_first_or_create(&content, None, Some(false)).await?;
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
                            .env_first_or_create(&parsed_key, None, Some(is_required))
                            .await?;
                        let stored = run.decrypt(row.value_encrypted.as_deref());
                        php::put_str(
                            &mut environment,
                            &parsed_key,
                            stored.map_or(PhpValue::Null, PhpValue::Str),
                        );
                        continue;
                    }
                    run.env_first_or_create(&k2, Some(Some(&v2)), Some(is_required))
                        .await?;
                }
            }
        }

        let branch = run.app.str("git_branch").unwrap_or_default().to_owned();
        let relation = run.env_relation().await?;
        let has = |k: &str| relation.iter().any(|r| r.key == k);
        if !has("COOLIFY_BRANCH") {
            php::put_str(
                &mut coolify_environments,
                "COOLIFY_BRANCH",
                PhpValue::Str(format!("\"{branch}\"")),
            );
        }
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

        let domains_map: Arr = if run.build_pack_is_compose() {
            php::json_array(run.attrs.docker_compose_domains.as_deref())
        } else {
            Vec::new()
        };
        let fqdns_string = domains::get_compose_service_domain_string(&domains_map, &service_name);
        if run.build_pack_is_compose() {
            for (for_service, domain) in &domains_map {
                let parsed_domain = domains::compose_domain_entry_string(domain);
                let formatted = php::upper(&service_name).replace(['-', '.'], "_");
                if php::filled_str(parsed_domain.as_deref()) {
                    let first = parsed_domain
                        .unwrap_or_default()
                        .split(',')
                        .next()
                        .unwrap_or_default()
                        .to_owned();
                    let url = SpatieUrl::from_string(&first)?;
                    let host = url.host.clone();
                    let mut without_port = url.clone();
                    without_port.port = None;
                    let url_text = without_port.to_string();
                    let env_key = php::upper(&domains::normalize_compose_service_name(
                        &for_service.as_string(),
                    ));
                    php::put_str(
                        &mut coolify_environments,
                        &format!("SERVICE_URL_{env_key}"),
                        PhpValue::Str(url_text.clone()),
                    );
                    php::put_str(
                        &mut coolify_environments,
                        &format!("SERVICE_FQDN_{env_key}"),
                        PhpValue::Str(host.clone()),
                    );
                    run.env_update_or_create(&format!("SERVICE_URL_{env_key}"), Some(&url_text))
                        .await?;
                    run.env_update_or_create(&format!("SERVICE_FQDN_{env_key}"), Some(&host))
                        .await?;
                } else {
                    for prefix in ["SERVICE_FQDN_", "SERVICE_URL_"] {
                        store::env_null_values_like(
                            run.db,
                            MORPH,
                            run.app.id,
                            &format!("{prefix}{formatted}%"),
                            run.now,
                        )
                        .await
                        .map_err(Run::db_err)?;
                    }
                }
            }
        }
        let fqdns: Option<Vec<String>> = fqdns_string
            .filter(|f| !f.trim().is_empty())
            .map(|f| f.split(',').map(str::to_owned).collect());

        let project_name = run
            .app
            .project
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let environment_name = run
            .app
            .environment
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let resource_name = run.app.str("name").unwrap_or_default().to_owned();
        let default_labels = labels::default_labels(
            &uuid,
            &container_name,
            &project_name,
            &resource_name,
            &environment_name,
            pull_request_id,
            "application",
            &run.version,
        );
        let is_database = service::is_database_image(&image, &service);
        if let Some(f) = fqdns.as_ref().filter(|f| !is_database && !f.is_empty()) {
            php::put_str(
                &mut coolify_environments,
                "COOLIFY_URL",
                PhpValue::Str(
                    f.iter()
                        .map(|d| domains::get_fqdn_without_port(d))
                        .collect::<Vec<_>>()
                        .join(","),
                ),
            );
            php::put_str(
                &mut coolify_environments,
                "COOLIFY_FQDN",
                PhpValue::Str(
                    f.iter()
                        .map(|d| domains::get_host_without_port(d))
                        .collect::<Vec<_>>()
                        .join(","),
                ),
            );
        }
        let mut user_defined: Vec<String> = relation.iter().map(|r| r.key.clone()).collect();
        user_defined.extend(environment.iter().map(|(k, _)| k.as_string()));
        if run
            .app
            .settings
            .get("include_coolify_default_variables")
            .and_then(Value::as_bool)
            != Some(false)
        {
            let defaults = [
                ("COOLIFY_APP_NAME", Some(resource_name.clone())),
                (
                    "COOLIFY_PROJECT_NAME",
                    run.app
                        .project
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                ),
                (
                    "COOLIFY_ENVIRONMENT_NAME",
                    run.app
                        .environment
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
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
                    && let Some(row) =
                        store::env_first(run.db, MORPH, run.app.id, &k.as_string(), false)
                            .await
                            .map_err(Run::db_err)?
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
        let mut service_labels: Vec<PhpValue> = labels;
        service_labels.extend(default_labels.into_iter().map(PhpValue::Str));
        if !service_labels.is_empty()
            && run.setting_default("is_container_label_escape_enabled", true)
        {
            service_labels = service_labels
                .iter()
                .map(labels::escape_dollar_sign)
                .collect();
        }
        if let Some(fqdn_list) = fqdns.as_ref().filter(|f| !is_database && !f.is_empty()) {
            let exact = run.server.setting_bool("generate_exact_labels");
            let redirect = php::get_key(
                &domains_map,
                &Key::from_php_string(
                    &domains::find_compose_service_name(
                        &service_name,
                        &domains_map
                            .iter()
                            .map(|(k, _)| k.as_string())
                            .collect::<Vec<_>>(),
                    )
                    .unwrap_or_default(),
                ),
            )
            .and_then(|e| php::get(e, "redirect"))
            .and_then(|r| match r {
                PhpValue::Str(s) if ["www", "non-www", "both"].contains(&s.as_str()) => {
                    Some(s.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| "both".into());
            let overrides = overrides_map(run.attrs.domain_port_overrides.as_ref());
            let noindex = run.noindex_domains();
            let only_port = service::first_docker_compose_service_port(&service);
            let traffic = run.server.setting_bool("is_traffic_analytics_enabled");
            let log_append = labels::caddy_supports_log_append(&run.server.proxy);
            let proxy_type = run.server.proxy_type().map(str::to_owned);
            if !use_network_mode && (!exact || proxy_type.as_deref() == Some("TRAEFIK")) {
                labels::add_traefik_docker_network_label(&mut service_labels, &base_network);
            }
            let resolver = run
                .server
                .certificate_resolver(run.app.setting_str("traefik_certificate_resolver"));
            let network = run.app.network().to_owned();
            let force_https = run.setting_default("is_force_https_enabled", false);
            let permanent = run.setting_default("is_permanent_redirect_enabled", false);
            let gzip = run.setting_default("is_gzip_enabled", true);
            let stripprefix = run.setting_default("is_stripprefix_enabled", true);
            let traefik = |current: &[PhpValue]| {
                labels::fqdn_labels_for_traefik(&ProxyLabelArgs {
                    uuid: &uuid,
                    network: &network,
                    domains: fqdn_list,
                    force_https,
                    permanent_redirect: permanent,
                    service_labels: Some(current),
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
                })
            };
            let caddy = |current: &[PhpValue]| {
                labels::fqdn_labels_for_caddy(&ProxyLabelArgs {
                    uuid: &uuid,
                    network: &network,
                    domains: fqdn_list,
                    force_https,
                    permanent_redirect: permanent,
                    service_labels: Some(current),
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
                })
            };
            if exact {
                match proxy_type.as_deref() {
                    Some("TRAEFIK") => {
                        let l = traefik(&service_labels);
                        service_labels.extend(l.into_iter().map(PhpValue::Str));
                    }
                    Some("CADDY") => {
                        let l = caddy(&service_labels);
                        service_labels.extend(l.into_iter().map(PhpValue::Str));
                    }
                    _ => {}
                }
            } else {
                let l = traefik(&service_labels);
                service_labels.extend(l.into_iter().map(PhpValue::Str));
                let l = caddy(&service_labels);
                service_labels.extend(l.into_iter().map(PhpValue::Str));
            }
        }

        // data_forget($service, 'volumes.*.content' ...) and 'exclude_from_hc'
        if let PhpValue::Array(items) = &mut service {
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
            let environment =
                preserve_legacy_null_assignments(run, environment, &legacy_null_variables);
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
        let has_build = php::get(&service, "build").is_some_and(|b| *b != PhpValue::Null);
        let has_image = php::get(&service, "image").is_some_and(|b| *b != PhpValue::Null);
        if has_build
            && !has_image
            && let Some(c) = commit.filter(|c| php::truthy(&PhpValue::Str((*c).to_owned())))
        {
            let tag: String = c.chars().take(128).collect();
            php::put_str(
                &mut payload,
                "image",
                PhpValue::Str(format!("{uuid}_{service_name}:{tag}")),
            );
        }
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
    run.attrs.docker_compose = Some(coolify_compat::yaml::dump_with_indent(&top, 10, 2));

    // docker_compose_raw without the one-time volume fields
    if let Ok(original_yaml) = validate::parse_docker_compose_yaml(&original_compose) {
        let mut cleaned = original_yaml.clone();
        if let Some(PhpValue::Array(svcs)) = cleaned.get_mut("services") {
            for (_, svc) in svcs.iter_mut() {
                if let Some(PhpValue::Array(vols)) = svc.get_mut("volumes") {
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
            run.attrs.docker_compose_raw =
                Some(volumes::remove_compose_volume_fields_preserving_comments(
                    &original_compose,
                    &cleaned,
                    &["content", "isDirectory", "is_directory"],
                ));
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

/// `generateApplicationContainerName($application, $pull_request_id)`.
fn container_base_name(run: &Run<'_>, pull_request_id: i64) -> String {
    let consistent = run.setting_default("is_consistent_container_name_enabled", false);
    let internal = run
        .app
        .setting_str("custom_internal_name")
        .filter(|n| php::truthy(&PhpValue::Str((*n).to_owned())));
    let name = if consistent {
        internal.unwrap_or(&run.app.uuid).to_owned()
    } else {
        run.app.uuid.clone()
    };
    if pull_request_id != 0 {
        return format!("{name}-pr-{pull_request_id}");
    }
    if consistent {
        return name;
    }
    let prefix = run
        .app
        .setting_str("custom_container_name_prefix")
        .filter(|n| php::truthy(&PhpValue::Str((*n).to_owned())))
        .unwrap_or(&run.app.uuid);
    format!("{prefix}-{}", run.now.format("%Y%m%dT%H%M%S"))
}

/// `useComposeExternalVolumeAsWritten($resource, $owner, $topLevelVolumes, $source, $legacyName)`.
async fn use_external_as_written(
    run: &mut Run<'_>,
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
    let has_legacy = store::persistent_volume_exists(run.db, MORPH, run.app.id, legacy_name)
        .await
        .map_err(Run::db_err)?;
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
fn preserve_legacy_null_assignments(
    run: &Run<'_>,
    mut environment: Arr,
    variables: &[EnvRow],
) -> Arr {
    for v in variables {
        let preserves =
            v.preserve_compose_null && run.decrypt(v.value_encrypted.as_deref()).is_none();
        let k = Key::from_php_string(&v.key);
        let Some(value) = php::get_key(&environment, &k).cloned() else {
            continue;
        };
        if !preserves {
            continue;
        }
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
async fn sync_required_state(run: &mut Run<'_>) -> Result<(), String> {
    let compose = run
        .attrs
        .docker_compose_raw
        .clone()
        .or_else(|| run.attrs.docker_compose.clone());
    let mut rows = store::env_list(run.db, MORPH, run.app.id, false)
        .await
        .map_err(Run::db_err)?;
    rows.extend(
        store::env_list(run.db, MORPH, run.app.id, true)
            .await
            .map_err(Run::db_err)?,
    );
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
