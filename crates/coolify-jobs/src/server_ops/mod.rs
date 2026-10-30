//! Port of the upstream actions and jobs that manage a server itself: install,
//! proxy, Sentinel, Docker cleanup, log drains, patches, CA certificate and the
//! scheduled checks. Each submodule cites what it ports.

pub mod certificate;
pub mod checks;
pub mod cleanup;
pub mod install;
pub mod log_drain;
pub mod patches;
pub mod proxy;
pub mod proxy_config;
pub mod registries;
pub mod scheduler;
pub mod sentinel;

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use chrono::NaiveDateTime;
use coolify_compat::php_json;
use coolify_db::destinations::{self, Kind};
use coolify_db::server_ops as ops;
use coolify_db::servers::{self, ColumnValue, Row, ServerRow};
use coolify_remote::{RunOptions, Target};
use serde_json::{Map, Value};

use crate::Jobs;
use crate::server::ServerInfo;

/// `base_configuration_dir()`.
pub const BASE_CONFIG_DIR: &str = "/data/coolify";

/// `escapeshellarg()`.
pub fn shell_arg(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A small `Cache::put` stand-in with expiry, for the per-server marks upstream keeps in Redis.
#[derive(Debug, Default)]
pub struct Marks(Mutex<HashMap<String, (Instant, String)>>);

impl Marks {
    pub fn put(&self, key: String, value: String, ttl: Duration) {
        let mut m = self.0.lock().unwrap_or_else(|p| p.into_inner());
        m.insert(key, (Instant::now() + ttl, value));
    }
    pub fn add(&self, key: String, ttl: Duration) -> bool {
        let mut m = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        if m.get(&key).is_some_and(|(until, _)| *until > now) {
            return false;
        }
        m.insert(key, (now + ttl, String::new()));
        true
    }
    pub fn get(&self, key: &str) -> Option<String> {
        let m = self.0.lock().unwrap_or_else(|p| p.into_inner());
        m.get(key)
            .filter(|(until, _)| *until > Instant::now())
            .map(|(_, v)| v.clone())
    }
    pub fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
    pub fn forget(&self, key: &str) {
        let mut m = self.0.lock().unwrap_or_else(|p| p.into_inner());
        m.remove(key);
    }
}

/// The instance-wide marks.
pub static MARKS: LazyLock<Marks> = LazyLock::new(Marks::default);

/// A loaded server with its `proxy` attribute, as jobs work on the model.
#[derive(Debug)]
pub struct Srv {
    pub jobs: Jobs,
    pub row: ServerRow,
    pub info: ServerInfo,
    /// `$server->proxy` after the `retrieved` hook; `redirect_enabled` defaults to true.
    pub proxy: Map<String, Value>,
    original_proxy: Option<String>,
}

pub(crate) fn proxy_text(p: &Map<String, Value>) -> String {
    php_json::encode(&Value::Object(p.clone()))
}

impl Srv {
    pub fn from_row(jobs: &Jobs, row: ServerRow) -> Self {
        let info = ServerInfo::from_row(&row);
        let raw = row.record.get("proxy");
        let original_proxy = raw
            .filter(|v| !v.is_null())
            .map(|v| php_json::encode(&v.clone()));
        let mut proxy = match raw {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        if !proxy.contains_key("redirect_enabled") {
            proxy.insert("redirect_enabled".into(), Value::Bool(true));
        }
        Self {
            jobs: jobs.clone(),
            row,
            info,
            proxy,
            original_proxy,
        }
    }

    pub async fn load(jobs: &Jobs, id: i64) -> Result<Self, String> {
        let row = servers::find_by_id(&jobs.0.db, id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("Server not found.")?;
        Ok(Self::from_row(jobs, row))
    }

    pub async fn refresh(&mut self) -> Result<(), String> {
        *self = Self::load(&self.jobs.clone(), self.row.id).await?;
        Ok(())
    }

    pub fn db(&self) -> &coolify_db::Db {
        &self.jobs.0.db
    }

    pub fn id(&self) -> i64 {
        self.row.id
    }

    pub fn str_column(&self, k: &str) -> Option<&str> {
        self.row.record.get(k).and_then(Value::as_str)
    }

    /// `proxyType()`.
    pub fn proxy_type(&self) -> Option<&str> {
        self.proxy.get("type").and_then(Value::as_str)
    }

    /// `$server->proxy->force_stop`.
    pub fn force_stop(&self) -> bool {
        self.proxy
            .get("force_stop")
            .is_some_and(|v| !matches!(v, Value::Null | Value::Bool(false)))
    }

    pub fn proxy_set(&mut self, key: &str, value: Value) {
        self.proxy.insert(key.into(), value);
    }

    /// `$server->save()` for the proxy attribute: written only when it changed.
    pub async fn save_proxy(&mut self) -> Result<(), String> {
        let text = proxy_text(&self.proxy);
        if self.original_proxy.as_deref() == Some(text.as_str()) {
            return Ok(());
        }
        ops::save_proxy(self.db(), self.id(), Some(&text), Jobs::now())
            .await
            .map_err(|e| e.to_string())?;
        self.original_proxy = Some(text);
        Ok(())
    }

    pub async fn target(&self) -> Result<Target, String> {
        self.info.target(&self.jobs).await
    }

    /// `instant_remote_process($commands, $server)`.
    pub async fn run(
        &self,
        commands: &[String],
        op: &'static str,
    ) -> Result<Option<String>, String> {
        let t = self.target().await?;
        let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
        self.jobs
            .remote()
            .run(&t, &refs, RunOptions::op(op))
            .await
            .map_err(|e| e.message)
    }

    /// `instant_remote_process($commands, $server, false)`.
    pub async fn run_quiet(&self, commands: &[String], op: &'static str) -> Option<String> {
        let t = self.target().await.ok()?;
        let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
        self.jobs
            .remote()
            .run_quiet(&t, &refs, RunOptions::op(op))
            .await
    }

    pub async fn run_timeout(
        &self,
        commands: &[String],
        op: &'static str,
        timeout: Duration,
    ) -> Option<String> {
        let t = self.target().await.ok()?;
        let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
        self.jobs
            .remote()
            .run_quiet(&t, &refs, RunOptions::op(op).timeout(timeout))
            .await
    }

    pub async fn settings_update(
        &self,
        values: &[(&'static str, ColumnValue)],
    ) -> Result<bool, String> {
        servers::update_if_changed(self.db(), Row::Settings(self.id()), values, Jobs::now())
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn server_update(
        &self,
        values: &[(&'static str, ColumnValue)],
    ) -> Result<bool, String> {
        servers::update_if_changed(self.db(), Row::Server(self.id()), values, Jobs::now())
            .await
            .map_err(|e| e.to_string())
    }

    /// `isSwarmManager()`.
    pub fn is_swarm_manager(&self) -> bool {
        self.info.setting_bool("is_swarm_manager") == Some(true)
    }

    /// `isSwarmWorker()`.
    pub fn is_swarm_worker(&self) -> bool {
        self.info.setting_bool("is_swarm_worker") == Some(true)
    }

    /// `isLocalhost()`.
    pub fn is_localhost(&self) -> bool {
        self.id() == 0 || self.info.ip == "host.docker.internal"
    }

    /// `canHostResources()`: not a build-only server.
    pub fn can_host_resources(&self) -> bool {
        !self.info.is_build_server()
    }

    /// `proxySet()`.
    pub fn proxy_is_set(&self) -> bool {
        self.proxy_type().is_some_and(|t| t != "NONE")
            && self.info.is_functional()
            && !self.is_swarm_worker()
            && self.can_host_resources()
    }

    /// `isProxyShouldRun()`.
    pub fn is_proxy_should_run(&self) -> bool {
        self.proxy_type() != Some("NONE") && !self.info.is_build_server()
    }

    /// `proxyPath()`.
    pub fn proxy_path(&self) -> String {
        let base = format!("{BASE_CONFIG_DIR}/proxy");
        match self.proxy_type() {
            Some("TRAEFIK") => format!("{base}/"),
            Some("CADDY") => format!("{base}/caddy"),
            Some("NGINX") => format!("{base}/nginx"),
            _ => base,
        }
    }

    /// `traefikCertificateResolver()`.
    pub fn traefik_certificate_resolver(&self) -> Option<String> {
        self.info
            .settings
            .get("traefik_certificate_resolver")
            .and_then(Value::as_str)
            .filter(|r| !r.trim().is_empty() && *r != "none")
            .map(str::to_string)
    }

    /// `markProxyTransitionStarted()`.
    pub fn mark_proxy_transition_started(&self) {
        MARKS.put(
            format!("proxy-transition:{}", self.row.uuid),
            "1".into(),
            Duration::from_secs(300),
        );
    }

    /// `markProxyTransitionFinished()`.
    pub fn mark_proxy_transition_finished(&self) {
        MARKS.forget(&format!("proxy-transition:{}", self.row.uuid));
    }

    /// `isProxyTransitionInProgress()`.
    pub fn is_proxy_transition_in_progress(&self) -> bool {
        MARKS.has(&format!("proxy-transition:{}", self.row.uuid))
    }

    /// `$server->standaloneDockers` or `swarmDockers` networks.
    pub async fn destination_networks(&self) -> Result<Vec<String>, String> {
        let kind = if self.info.is_swarm() {
            Kind::Swarm
        } else {
            Kind::Standalone
        };
        destinations::networks(self.db(), kind, self.id())
            .await
            .map_err(|e| e.to_string())
    }

    pub fn setting_int(&self, key: &str) -> Option<i64> {
        self.info.settings.get(key).and_then(Value::as_i64)
    }

    /// `sentinelHeartbeat($isReset)`.
    pub async fn sentinel_heartbeat(&self, is_reset: bool) -> Result<(), String> {
        let now = Jobs::now();
        let value: NaiveDateTime = if is_reset {
            now - chrono::Duration::minutes(6000)
        } else {
            now
        };
        ops::save_sentinel_updated_at(self.db(), self.id(), value, now)
            .await
            .map_err(|e| e.to_string())
    }
}

/// `versions.json`, which `CheckForUpdatesJob` refreshes upstream.
pub fn versions_data() -> Value {
    serde_json::from_str(include_str!("../../versions.json")).unwrap_or(Value::Null)
}

/// `get_latest_sentinel_version()`.
pub fn latest_sentinel_version() -> String {
    versions_data()
        .pointer("/coolify/sentinel/version")
        .and_then(Value::as_str)
        .unwrap_or("0.0.0")
        .to_string()
}

/// `getHelperVersion()`: the constant, unless the stored one is newer.
pub fn helper_version(stored: Option<&str>) -> String {
    let configured = versions_data()
        .pointer("/coolify/helper/version")
        .and_then(Value::as_str)
        .unwrap_or("1.0.19")
        .to_string();
    match stored.filter(|s| !s.is_empty()) {
        Some(v) if version_compare(v, &configured) == std::cmp::Ordering::Greater => v.to_string(),
        _ => configured,
    }
}

/// PHP `version_compare` for dotted numeric versions (the only shapes used here).
pub fn version_compare(a: &str, b: &str) -> std::cmp::Ordering {
    let parts = |s: &str| {
        s.split(['.', '-', '+'])
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    let (x, y) = (parts(a), parts(b));
    for i in 0..x.len().max(y.len()) {
        let c = x
            .get(i)
            .copied()
            .unwrap_or(0)
            .cmp(&y.get(i).copied().unwrap_or(0));
        if c != std::cmp::Ordering::Equal {
            return c;
        }
    }
    std::cmp::Ordering::Equal
}

impl Jobs {
    /// Runs a server job in the background; failures are logged, as a failed queued job.
    pub(crate) fn spawn_server_job<F, Fut>(&self, name: &'static str, server_id: i64, f: F)
    where
        F: FnOnce(Jobs, i64) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        let jobs = self.clone();
        tokio::spawn(async move {
            if let Err(e) = f(jobs, server_id).await {
                tracing::warn!(job = name, server_id, error = %e, "server job failed");
            }
        });
    }

    /// `instant_remote_process($commands, $server, $throwError)` for a loaded row.
    pub async fn instant_remote(
        &self,
        row: &ServerRow,
        commands: &[String],
        throw_error: bool,
        op: &'static str,
    ) -> Result<Option<String>, String> {
        let srv = Srv::from_row(self, row.clone());
        if throw_error {
            srv.run(commands, op).await
        } else {
            Ok(srv.run_quiet(commands, op).await)
        }
    }
}
