//! Loading and calling plugins (ADR 0010, section 2).
//!
//! Each plugin is compiled once. Each call gets a fresh instance in its own store, with a memory cap and
//! a deadline: epoch interruption stops guest code that runs too long, and a wall-clock timeout covers
//! time spent in host calls. Only the granted interfaces are linked, and the component's imports are
//! checked against the grant first, so a plugin cannot call what it was not given even by accident of
//! linking. WASI is linked with an empty context: no files, environment, arguments or sockets.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use coolify_db::Db;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::PluginError;
use crate::egress::{Egress, Outgoing};
use crate::manifest::{Capabilities, Manifest};
use crate::store::{self, Installed, NewPlugin};

mod detached;
pub use detached::Probe;

mod bindings {
    wasmtime::component::bindgen!({
        world: "plugin",
        path: "wit",
        imports: { default: async },
        exports: { default: async },
    });
}

pub use bindings::coolify_rs::plugin::types::{Caller, Event, Request, Response};
use bindings::coolify_rs::plugin::{http, log, storage, types};

/// Resource limits for plugins.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Linear memory of one instance.
    pub memory: usize,
    pub event_deadline: Duration,
    pub request_deadline: Duration,
    /// Storage per plugin and team, keys included.
    pub storage_quota: i64,
    pub max_key: usize,
    pub max_value: usize,
    /// Size of a component.
    pub max_component: usize,
    /// Traps or timeouts in a row before the plugin is switched off.
    pub max_failures: u32,
    /// Calls of one plugin running at once.
    pub concurrency: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory: 64 * 1024 * 1024,
            event_deadline: Duration::from_secs(5),
            request_deadline: Duration::from_secs(10),
            storage_quota: 10 * 1024 * 1024,
            max_key: 256,
            max_value: 1024 * 1024,
            max_component: 20 * 1024 * 1024,
            max_failures: 5,
            concurrency: 4,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub limits: Limits,
    pub egress: Egress,
    /// Where the plugin registry is and which keys sign it.
    pub registry: crate::registry::Config,
}

/// Why a call did not complete.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    #[error("plugin {0} is not installed or not enabled")]
    Unavailable(String),
    #[error("plugin {0} is not enabled for this team")]
    NotEnabledForTeam(String),
    #[error("plugin {0} was not granted the {1} capability")]
    NotGranted(String, &'static str),
    #[error("plugin {0} took longer than {1:?}")]
    Timeout(String, Duration),
    #[error("plugin {0} failed: {1}")]
    Trap(String, String),
    #[error("plugin {0} returned an error: {1}")]
    Plugin(String, String),
    #[error(transparent)]
    Store(#[from] PluginError),
}

/// The plugin host. Cheap to clone.
#[derive(Clone)]
pub struct Host(Arc<Inner>);

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("loaded", &self.loaded_ids())
            .finish()
    }
}

struct Inner {
    engine: Engine,
    db: Db,
    settings: Settings,
    loaded: RwLock<HashMap<String, Arc<Loaded>>>,
    /// Set for `coolify-rs plugin check`: no database, storage in memory, no network.
    memory: Option<detached::Memory>,
}

struct Loaded {
    installed: Installed,
    pre: bindings::PluginPre<State>,
    failures: AtomicU32,
    permits: Semaphore,
}

/// The data of one call's store.
struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: Limiter,
    call: CallCtx,
}

/// The store's limits, remembering the largest linear memory the call reached.
struct Limiter {
    inner: StoreLimits,
    peak_memory: usize,
}

impl wasmtime::ResourceLimiter for Limiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let ok = self.inner.memory_growing(current, desired, maximum)?;
        if ok {
            self.peak_memory = self.peak_memory.max(desired);
        }
        Ok(ok)
    }

    fn memory_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        self.inner.memory_grow_failed(error)
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.inner.table_growing(current, desired, maximum)
    }

    fn table_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        self.inner.table_grow_failed(error)
    }

    fn instances(&self) -> usize {
        self.inner.instances()
    }

    fn tables(&self) -> usize {
        self.inner.tables()
    }

    fn memories(&self) -> usize {
        self.inner.memories()
    }
}

#[derive(Clone)]
struct CallCtx {
    plugin: Arc<str>,
    team_id: i64,
    db: Db,
    egress: Egress,
    hosts: Arc<[String]>,
    limits: Limits,
    memory: Option<detached::Memory>,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// How often the epoch advances; deadlines are counted in these ticks.
const TICK: Duration = Duration::from_millis(10);

/// What a component may import: always these, plus the interfaces of granted capabilities.
const ALWAYS: &[&str] = &["coolify-rs:plugin/types", "coolify-rs:plugin/log"];
/// WASI interfaces linked with an empty context, so standard libraries work and reach nothing.
const WASI: &[&str] = &[
    "wasi:io/",
    "wasi:cli/",
    "wasi:clocks/",
    "wasi:random/",
    "wasi:filesystem/",
];

impl Host {
    /// Creates the schema if needed and loads every enabled plugin. Plugins that no longer load are
    /// switched off with the reason, so one broken plugin never stops coolify-rs from starting.
    pub async fn start(db: Db, settings: Settings) -> Result<Host, PluginError> {
        store::migrate(&db).await?;
        let host = Host(Arc::new(Inner {
            engine: engine()?,
            db,
            settings,
            loaded: RwLock::new(HashMap::new()),
            memory: None,
        }));
        tick(Arc::downgrade(&host.0));
        for p in store::list(&host.0.db).await? {
            if !p.enabled {
                continue;
            }
            if let Err(e) = host.load(p.clone()).await {
                tracing::warn!(plugin = %p.id, error = %e, "plugin could not be loaded and is switched off");
                store::set_enabled(&host.0.db, &p.id, false, Some(&e.to_string())).await?;
            }
        }
        Ok(host)
    }

    pub fn db(&self) -> &Db {
        &self.0.db
    }

    pub fn limits(&self) -> &Limits {
        &self.0.settings.limits
    }

    pub fn settings(&self) -> &Settings {
        &self.0.settings
    }

    async fn load(&self, installed: Installed) -> Result<(), PluginError> {
        let bytes = store::component(&self.0.db, &installed.id)
            .await?
            .ok_or_else(|| PluginError::NotFound(installed.id.clone()))?;
        let loaded = self.compile(&bytes, installed)?;
        let id = loaded.installed.id.clone();
        self.0
            .loaded
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, Arc::new(loaded));
        Ok(())
    }

    pub(crate) fn unload(&self, id: &str) {
        self.0
            .loaded
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }

    fn get(&self, id: &str) -> Option<Arc<Loaded>> {
        self.0
            .loaded
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    /// Compiles a component and checks its imports against the grant.
    fn compile(&self, bytes: &[u8], installed: Installed) -> Result<Loaded, PluginError> {
        let engine = &self.0.engine;
        let component =
            Component::new(engine, bytes).map_err(|e| PluginError::Component(format!("{e:#}")))?;
        let imports: Vec<String> = component
            .component_type()
            .imports(engine)
            .map(|(name, _)| name.to_owned())
            .collect();
        check_imports(&imports, &installed.granted)?;
        let mut linker = Linker::<State>::new(engine);
        let link = |e: wasmtime::Error| PluginError::Engine(format!("{e:#}"));
        wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(link)?;
        log::add_to_linker::<State, HasSelf<State>>(&mut linker, |s| s).map_err(link)?;
        if installed.granted.storage {
            storage::add_to_linker::<State, HasSelf<State>>(&mut linker, |s| s).map_err(link)?;
        }
        if !installed.granted.http.is_empty() {
            http::add_to_linker::<State, HasSelf<State>>(&mut linker, |s| s).map_err(link)?;
        }
        let pre = linker
            .instantiate_pre(&component)
            .and_then(bindings::PluginPre::new)
            .map_err(|e| PluginError::Component(format!("{e:#}")))?;
        Ok(Loaded {
            installed,
            pre,
            failures: AtomicU32::new(0),
            permits: Semaphore::new(self.0.settings.limits.concurrency),
        })
    }

    /// Installs or updates a plugin, granting exactly the capabilities its manifest lists. The caller
    /// must have shown those capabilities to an instance admin and received their approval.
    pub async fn install(
        &self,
        manifest: Manifest,
        component: &[u8],
        origin: &str,
        verified: bool,
        user_id: Option<i64>,
    ) -> Result<Installed, PluginError> {
        if component.len() > self.0.settings.limits.max_component {
            return Err(PluginError::Component(format!(
                "the component is over {} bytes",
                self.0.settings.limits.max_component
            )));
        }
        crate::registry::ensure_not_revoked(&self.0.db, &manifest.id).await?;
        let sha256 = hex::encode(Sha256::digest(component));
        let previous = store::find(&self.0.db, &manifest.id).await?;
        let installed = Installed {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            granted: manifest.capabilities.clone(),
            manifest: manifest.clone(),
            sha256: sha256.clone(),
            origin: origin.to_owned(),
            verified,
            enabled: true,
            disabled_reason: None,
        };
        // Compile first: nothing is stored unless the component loads with this grant.
        let loaded = self.compile(component, installed.clone())?;
        store::upsert(
            &self.0.db,
            NewPlugin {
                manifest: &manifest,
                granted: &manifest.capabilities,
                sha256: &sha256,
                component,
                origin,
                verified,
                user_id,
            },
        )
        .await?;
        self.0
            .loaded
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(manifest.id.clone(), Arc::new(loaded));
        let action = if previous.is_some() {
            "updated"
        } else {
            "installed"
        };
        store::audit(
            &self.0.db,
            &manifest.id,
            None,
            user_id,
            action,
            serde_json::json!({
                "version": manifest.version,
                "previous_version": previous.as_ref().map(|p| &p.version),
                "sha256": sha256,
                "origin": origin,
                "verified": verified,
                "granted": manifest.capability_summary(),
            }),
        )
        .await?;
        Ok(installed)
    }

    pub async fn uninstall(&self, id: &str, user_id: Option<i64>) -> Result<bool, PluginError> {
        self.unload(id);
        let removed = store::delete(&self.0.db, id).await?;
        if removed {
            store::audit(
                &self.0.db,
                id,
                None,
                user_id,
                "uninstalled",
                serde_json::json!({}),
            )
            .await?;
        }
        Ok(removed)
    }

    /// Switches a plugin on or off for the whole instance.
    pub async fn set_enabled(
        &self,
        id: &str,
        enabled: bool,
        user_id: Option<i64>,
    ) -> Result<(), PluginError> {
        let p = store::find(&self.0.db, id)
            .await?
            .ok_or_else(|| PluginError::NotFound(id.to_owned()))?;
        if enabled {
            crate::registry::ensure_not_revoked(&self.0.db, id).await?;
            store::set_enabled(&self.0.db, id, true, None).await?;
            self.load(Installed {
                enabled: true,
                disabled_reason: None,
                ..p
            })
            .await?;
        } else {
            self.unload(id);
            store::set_enabled(&self.0.db, id, false, None).await?;
        }
        let action = if enabled { "enabled" } else { "disabled" };
        store::audit(&self.0.db, id, None, user_id, action, serde_json::json!({})).await
    }

    /// Switches a plugin on or off for one team.
    pub async fn set_team(
        &self,
        id: &str,
        team_id: i64,
        enabled: bool,
        user_id: Option<i64>,
    ) -> Result<(), PluginError> {
        if store::find(&self.0.db, id).await?.is_none() {
            return Err(PluginError::NotFound(id.to_owned()));
        }
        store::set_team(&self.0.db, id, team_id, enabled, user_id).await?;
        let action = if enabled {
            "team_enabled"
        } else {
            "team_disabled"
        };
        store::audit(
            &self.0.db,
            id,
            Some(team_id),
            user_id,
            action,
            serde_json::json!({}),
        )
        .await
    }

    /// Offers an event to every plugin enabled for the team that subscribed to its kind. Returns at once;
    /// plugins run on their own tasks and never delay the caller.
    pub fn dispatch(&self, team_id: i64, kind: &str, payload: serde_json::Value) {
        let host = self.clone();
        let kind = kind.to_owned();
        tokio::spawn(async move {
            let ids = match store::ids_for_team(&host.0.db, team_id).await {
                Ok(ids) => ids,
                Err(e) => {
                    tracing::warn!(error = %e, "could not read the team's plugins");
                    return;
                }
            };
            let payload = payload.to_string();
            for id in ids {
                let Some(p) = host.get(&id) else { continue };
                if !p.installed.granted.events.contains(&kind) {
                    continue;
                }
                let event = Event {
                    kind: kind.clone(),
                    version: coolify_domain::plugin_events::PAYLOAD_VERSION,
                    team_id,
                    payload: payload.clone(),
                };
                let host = host.clone();
                tokio::spawn(async move {
                    if let Err(e) = host.call_event(&id, event).await {
                        tracing::warn!(plugin = %id, error = %e, "plugin event failed");
                    }
                });
            }
        });
    }

    /// Delivers one event to one plugin. The team must have enabled it and it must hold `events`.
    pub async fn call_event(&self, id: &str, event: Event) -> Result<(), CallError> {
        let p = self.ready(id, event.team_id).await?;
        if p.installed.granted.events.is_empty() {
            return Err(CallError::NotGranted(id.to_owned(), "events"));
        }
        let deadline = self.0.settings.limits.event_deadline;
        let result = self
            .call(&p, event.team_id, deadline, async |store, plugin| {
                plugin.call_on_event(store, &event).await
            })
            .await?;
        result.map_err(|e| CallError::Plugin(id.to_owned(), e))
    }

    /// Passes an API request to a plugin's routes.
    pub async fn handle_request(
        &self,
        id: &str,
        caller: Caller,
        request: Request,
    ) -> Result<Response, CallError> {
        let p = self.ready(id, caller.team_id).await?;
        if !p.installed.granted.routes {
            return Err(CallError::NotGranted(id.to_owned(), "routes"));
        }
        let deadline = self.0.settings.limits.request_deadline;
        let team_id = caller.team_id;
        self.call(&p, team_id, deadline, async |store, plugin| {
            plugin.call_handle_request(store, &caller, &request).await
        })
        .await
    }

    async fn ready(&self, id: &str, team_id: i64) -> Result<Arc<Loaded>, CallError> {
        let p = self
            .get(id)
            .ok_or_else(|| CallError::Unavailable(id.to_owned()))?;
        if self.0.memory.is_none() && !store::enabled_for_team(&self.0.db, id, team_id).await? {
            return Err(CallError::NotEnabledForTeam(id.to_owned()));
        }
        Ok(p)
    }

    /// One sandboxed call: a fresh instance, limits, a deadline, and failure accounting.
    async fn call<R>(
        &self,
        p: &Loaded,
        team_id: i64,
        deadline: Duration,
        f: impl AsyncFnOnce(&mut Store<State>, &bindings::Plugin) -> wasmtime::Result<R>,
    ) -> Result<R, CallError> {
        self.call_measured(p, team_id, deadline, f).await.0
    }

    /// [`Host::call`], also returning how long it took and the largest memory it reached.
    async fn call_measured<R>(
        &self,
        p: &Loaded,
        team_id: i64,
        deadline: Duration,
        f: impl AsyncFnOnce(&mut Store<State>, &bindings::Plugin) -> wasmtime::Result<R>,
    ) -> (Result<R, CallError>, Duration, usize) {
        let id = p.installed.id.clone();
        let Ok(_permit) = p.permits.acquire().await else {
            return (Err(CallError::Unavailable(id)), Duration::ZERO, 0);
        };
        let limits = &self.0.settings.limits;
        let mut wasi = WasiCtxBuilder::new();
        wasi.allow_tcp(false)
            .allow_udp(false)
            .allow_ip_name_lookup(false);
        let state = State {
            wasi: wasi.build(),
            table: ResourceTable::new(),
            limits: Limiter {
                inner: StoreLimitsBuilder::new()
                    .memory_size(limits.memory)
                    .instances(16)
                    .tables(32)
                    .memories(4)
                    .trap_on_grow_failure(true)
                    .build(),
                peak_memory: 0,
            },
            call: CallCtx {
                plugin: Arc::from(id.as_str()),
                team_id,
                db: self.0.db.clone(),
                egress: self.0.settings.egress.clone(),
                hosts: Arc::from(p.installed.granted.http.clone()),
                limits: limits.clone(),
                memory: self.0.memory.clone(),
            },
        };
        let mut store = Store::new(&self.0.engine, state);
        store.limiter(|s| &mut s.limits);
        let ticks = (deadline.as_millis() / TICK.as_millis()).max(1) as u64;
        store.set_epoch_deadline(ticks);
        store.epoch_deadline_trap();
        let run = async {
            let plugin = p.pre.instantiate_async(&mut store).await?;
            f(&mut store, &plugin).await
        };
        let started = std::time::Instant::now();
        let timed = tokio::time::timeout(deadline, run).await;
        let elapsed = started.elapsed();
        let peak_memory = store.data().limits.peak_memory;
        let outcome = match timed {
            Ok(Ok(r)) => Ok(r),
            Ok(Err(e)) => {
                let interrupted = e
                    .downcast_ref::<wasmtime::Trap>()
                    .is_some_and(|t| *t == wasmtime::Trap::Interrupt);
                if interrupted {
                    Err(CallError::Timeout(id.clone(), deadline))
                } else {
                    Err(CallError::Trap(id.clone(), format!("{e:#}")))
                }
            }
            Err(_) => Err(CallError::Timeout(id.clone(), deadline)),
        };
        match &outcome {
            Ok(_) => p.failures.store(0, Ordering::Relaxed),
            Err(_) if self.0.memory.is_some() => {}
            Err(e) => {
                let n = p.failures.fetch_add(1, Ordering::Relaxed) + 1;
                if n >= limits.max_failures {
                    self.auto_disable(&id, n, e).await;
                }
            }
        }
        (outcome, elapsed, peak_memory)
    }

    async fn auto_disable(&self, id: &str, failures: u32, last: &CallError) {
        self.unload(id);
        let reason = format!("switched off after {failures} failures in a row; the last: {last}");
        tracing::warn!(plugin = %id, %reason, "plugin switched off");
        let db = &self.0.db;
        let result = async {
            store::set_enabled(db, id, false, Some(&reason)).await?;
            store::audit(
                db,
                id,
                None,
                None,
                "auto_disabled",
                serde_json::json!({ "reason": reason }),
            )
            .await
        }
        .await;
        if let Err(e) = result {
            tracing::error!(plugin = %id, error = %e, "could not record that the plugin was switched off");
        }
    }

    /// Plugins loaded right now, by id.
    pub fn loaded_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .0
            .loaded
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        ids.sort();
        ids
    }
}

/// The engine every plugin runs on.
fn engine() -> Result<Engine, PluginError> {
    let mut config = Config::new();
    config.epoch_interruption(true);
    config.wasm_component_model(true);
    config.max_wasm_stack(1024 * 1024);
    Engine::new(&config).map_err(|e| PluginError::Engine(e.to_string()))
}

/// Advances the engine's epoch every `TICK` until the host is dropped. This is a plain thread, not a
/// tokio task: guest code that never yields would starve a task on the same runtime thread, and the
/// deadline would never arrive.
fn tick(host: Weak<Inner>) {
    let engine = match host.upgrade() {
        Some(inner) => inner.engine.clone(),
        None => return,
    };
    let spawned = std::thread::Builder::new()
        .name("plugin-epoch".into())
        .spawn(move || {
            while host.strong_count() > 0 {
                std::thread::sleep(TICK);
                engine.increment_epoch();
            }
        });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "plugins: could not start the epoch thread");
    }
}

/// Refuses imports outside the grant, naming them, so the admin and the author see what is wrong.
pub(crate) fn check_imports(imports: &[String], granted: &Capabilities) -> Result<(), PluginError> {
    let mut refused = Vec::new();
    for name in imports {
        let (interface, version) = name.split_once('@').unwrap_or((name.as_str(), ""));
        let ours = |i: &str| interface == i && version.starts_with("0.1.");
        let ok = ALWAYS.iter().any(|i| ours(i))
            || WASI.iter().any(|w| interface.starts_with(w))
            || (granted.storage && ours("coolify-rs:plugin/storage"))
            || (!granted.http.is_empty() && ours("coolify-rs:plugin/http"));
        if !ok {
            refused.push(name.clone());
        }
    }
    if refused.is_empty() {
        Ok(())
    } else {
        Err(PluginError::Capability(format!(
            "the component imports {} without a grant for it",
            refused.join(", ")
        )))
    }
}

impl types::Host for State {}

impl log::Host for State {
    async fn write(&mut self, level: log::Level, message: String) {
        let message: String = message.chars().take(4096).collect();
        let plugin = &*self.call.plugin;
        let team = self.call.team_id;
        match level {
            log::Level::Debug => tracing::debug!(plugin, team, "{message}"),
            log::Level::Info => tracing::info!(plugin, team, "{message}"),
            log::Level::Warn => tracing::warn!(plugin, team, "{message}"),
            log::Level::Error => tracing::error!(plugin, team, "{message}"),
        }
    }
}

impl storage::Host for State {
    async fn get(&mut self, key: String) -> Result<Option<Vec<u8>>, String> {
        let c = &self.call;
        check_key(&key, &c.limits)?;
        if let Some(m) = &c.memory {
            return Ok(detached::get(m, c.team_id, &key));
        }
        store::kv_get(&c.db, &c.plugin, c.team_id, &key)
            .await
            .map_err(|e| storage_error(&c.plugin, e))
    }

    async fn set(&mut self, key: String, value: Vec<u8>) -> Result<(), String> {
        let c = &self.call;
        check_key(&key, &c.limits)?;
        if value.len() > c.limits.max_value {
            return Err(format!(
                "values are limited to {} bytes",
                c.limits.max_value
            ));
        }
        if let Some(m) = &c.memory {
            return detached::set(m, c.team_id, key, value, c.limits.storage_quota);
        }
        let stored = store::kv_set(
            &c.db,
            &c.plugin,
            c.team_id,
            &key,
            &value,
            c.limits.storage_quota,
        )
        .await
        .map_err(|e| storage_error(&c.plugin, e))?;
        if stored {
            Ok(())
        } else {
            Err(format!(
                "storage is limited to {} bytes per team",
                c.limits.storage_quota
            ))
        }
    }

    async fn delete(&mut self, key: String) -> Result<(), String> {
        let c = &self.call;
        check_key(&key, &c.limits)?;
        if let Some(m) = &c.memory {
            detached::delete(m, c.team_id, &key);
            return Ok(());
        }
        store::kv_delete(&c.db, &c.plugin, c.team_id, &key)
            .await
            .map_err(|e| storage_error(&c.plugin, e))
    }

    async fn keys(&mut self, prefix: String) -> Result<Vec<String>, String> {
        let c = &self.call;
        if prefix.len() > c.limits.max_key {
            return Err(format!("keys are limited to {} bytes", c.limits.max_key));
        }
        if let Some(m) = &c.memory {
            return Ok(detached::keys(m, c.team_id, &prefix));
        }
        store::kv_keys(&c.db, &c.plugin, c.team_id, &prefix)
            .await
            .map_err(|e| storage_error(&c.plugin, e))
    }
}

fn check_key(key: &str, limits: &Limits) -> Result<(), String> {
    if key.is_empty() || key.len() > limits.max_key {
        return Err(format!("keys must be 1 to {} bytes", limits.max_key));
    }
    Ok(())
}

/// The plugin sees that storage failed, not the database's error text.
fn storage_error(plugin: &str, e: PluginError) -> String {
    tracing::warn!(plugin, error = %e, "plugin storage failed");
    "storage is unavailable".into()
}

impl http::Host for State {
    async fn fetch(&mut self, request: http::Outgoing) -> Result<Response, String> {
        let c = &self.call;
        if c.memory.is_some() {
            return Err(detached::NO_NETWORK.into());
        }
        let r = c
            .egress
            .fetch(
                &c.hosts,
                Outgoing {
                    method: request.method,
                    url: request.url,
                    headers: request.headers,
                    body: request.body,
                },
            )
            .await?;
        Ok(Response {
            status: r.status,
            headers: r.headers,
            body: r.body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn imports_must_be_granted() {
        let none = Capabilities::default();
        let base = names(&[
            "coolify-rs:plugin/types@0.1.0",
            "coolify-rs:plugin/log@0.1.0",
            "wasi:cli/environment@0.2.6",
            "wasi:io/streams@0.2.6",
        ]);
        assert!(check_imports(&base, &none).is_ok());

        let with_storage = [base.clone(), names(&["coolify-rs:plugin/storage@0.1.0"])].concat();
        let err = check_imports(&with_storage, &none).unwrap_err().to_string();
        assert!(err.contains("coolify-rs:plugin/storage@0.1.0"), "{err}");
        let granted = Capabilities {
            storage: true,
            ..Default::default()
        };
        assert!(check_imports(&with_storage, &granted).is_ok());

        for bad in [
            "wasi:sockets/tcp@0.2.6",
            "wasi:http/outgoing-handler@0.2.6",
            "coolify-rs:plugin/storage@1.0.0",
            "evil:host/everything",
        ] {
            let err = check_imports(&names(&[bad]), &granted)
                .unwrap_err()
                .to_string();
            assert!(err.contains(bad), "{bad}: {err}");
        }
    }
}
