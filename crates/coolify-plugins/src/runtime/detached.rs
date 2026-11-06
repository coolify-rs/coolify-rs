//! The sandbox without a database, for `coolify-rs plugin check` (the registry's conformance check).
//!
//! The engine, linker, import check, limits and deadlines are the ones installed plugins run with. Only
//! what needs a database or the network differs: storage lives in memory, every team counts as having
//! enabled the plugin, `http` requests fail, and failures never switch the plugin off.

use std::collections::BTreeMap;
use std::sync::Mutex;

use super::*;

pub(super) type Memory = Arc<Mutex<BTreeMap<(i64, String), Vec<u8>>>>;

pub(super) const NO_NETWORK: &str = "network requests are not made during a conformance check";

/// One call made by a conformance check.
#[derive(Debug)]
pub struct Probe<T> {
    pub result: Result<T, CallError>,
    pub elapsed: Duration,
    /// The largest linear memory the call grew to, in bytes.
    pub peak_memory: usize,
}

impl Host {
    /// A host with no database. Components are loaded with [`Host::load_detached`] and called with
    /// [`Host::probe_event`] and [`Host::probe_request`].
    pub fn detached(settings: Settings) -> Result<Host, PluginError> {
        let host = Host(Arc::new(Inner {
            engine: engine()?,
            db: Db::unconnected(),
            settings,
            loaded: RwLock::new(HashMap::new()),
            memory: Some(Memory::default()),
            bridges: RwLock::new(Bridges::default()),
            encrypter: RwLock::new(None),
        }));
        tick(Arc::downgrade(&host.0));
        Ok(host)
    }

    /// Compiles a component with exactly the capabilities its manifest asks for, as installing does, and
    /// checks its imports against them. Returns the imports.
    pub fn load_detached(
        &self,
        manifest: Manifest,
        component: &[u8],
    ) -> Result<Vec<String>, PluginError> {
        if self.0.memory.is_none() {
            return Err(PluginError::Engine(
                "only a detached host loads components without installing them".into(),
            ));
        }
        let max = self.0.settings.limits.max_component;
        if component.len() > max {
            return Err(PluginError::Component(format!(
                "the component is over {max} bytes"
            )));
        }
        let engine = &self.0.engine;
        let parsed = Component::new(engine, component)
            .map_err(|e| PluginError::Component(format!("{e:#}")))?;
        let imports = parsed
            .component_type()
            .imports(engine)
            .map(|(name, _)| name.to_owned())
            .collect();
        let installed = Installed {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            granted: manifest.capabilities.clone(),
            sha256: hex::encode(Sha256::digest(component)),
            manifest,
            origin: "check".into(),
            verified: false,
            enabled: true,
            disabled_reason: None,
        };
        let loaded = self.compile(component, installed)?;
        self.0
            .loaded
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(loaded.installed.id.clone(), Arc::new(loaded));
        Ok(imports)
    }

    pub async fn probe_event(&self, id: &str, event: Event) -> Probe<()> {
        let Some(p) = self.get(id) else {
            return unavailable(id);
        };
        let deadline = self.0.settings.limits.event_deadline;
        let (result, elapsed, peak_memory) = self
            .call_measured(&p, event.team_id, None, deadline, async |store, plugin| {
                plugin.call_on_event(store, &event).await
            })
            .await;
        Probe {
            result: result.and_then(|r| r.map_err(|e| CallError::Plugin(id.to_owned(), e))),
            elapsed,
            peak_memory,
        }
    }

    pub async fn probe_request(
        &self,
        id: &str,
        caller: Caller,
        request: Request,
    ) -> Probe<Response> {
        let Some(p) = self.get(id) else {
            return unavailable(id);
        };
        let deadline = self.0.settings.limits.request_deadline;
        let team_id = caller.team_id;
        let who = caller.clone();
        let (result, elapsed, peak_memory) = self
            .call_measured(&p, team_id, Some(who), deadline, async |store, plugin| {
                plugin.call_handle_request(store, &caller, &request).await
            })
            .await;
        Probe {
            result,
            elapsed,
            peak_memory,
        }
    }
}

fn unavailable<T>(id: &str) -> Probe<T> {
    Probe {
        result: Err(CallError::Unavailable(id.to_owned())),
        elapsed: Duration::ZERO,
        peak_memory: 0,
    }
}

fn lock(m: &Memory) -> std::sync::MutexGuard<'_, BTreeMap<(i64, String), Vec<u8>>> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn get(m: &Memory, team: i64, key: &str) -> Option<Vec<u8>> {
    lock(m).get(&(team, key.to_owned())).cloned()
}

pub(super) fn set(
    m: &Memory,
    team: i64,
    key: String,
    value: Vec<u8>,
    quota: i64,
) -> Result<(), String> {
    let mut map = lock(m);
    let used: usize = map
        .iter()
        .filter(|((t, k), _)| *t == team && *k != key)
        .map(|((_, k), v)| k.len() + v.len())
        .sum();
    if (used + key.len() + value.len()) as i64 > quota {
        return Err(format!("storage is limited to {quota} bytes per team"));
    }
    map.insert((team, key), value);
    Ok(())
}

pub(super) fn delete(m: &Memory, team: i64, key: &str) {
    lock(m).remove(&(team, key.to_owned()));
}

pub(super) fn keys(m: &Memory, team: i64, prefix: &str) -> Vec<String> {
    lock(m)
        .keys()
        .filter(|(t, k)| *t == team && k.starts_with(prefix))
        .map(|(_, k)| k.clone())
        .take(1000)
        .collect()
}
