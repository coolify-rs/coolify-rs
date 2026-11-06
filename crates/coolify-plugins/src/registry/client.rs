//! The registry on an instance: refresh the signed index, show the catalogue, install, update, and apply
//! yanks and revocations (ADR 0010, sections 4 and 7).
//!
//! Every download goes through the plugins' own egress rules: HTTPS only, public addresses only, no
//! redirects, and at most the size of a component.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::index::{Entry, Index, Release, Revoked, compare_versions};
use super::review::capability_items;
use super::{db, signing};
use crate::PluginError;
use crate::egress::{Egress, Outgoing};
use crate::manifest::Manifest;
use crate::runtime::Host;
use crate::store::{self, Installed};

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("the plugin registry has not been fetched yet; refresh the catalogue first")]
    NoIndex,
    #[error("{0} is not in the plugin registry")]
    Unknown(String),
    #[error("{id} was revoked by the plugin registry: {reason}")]
    Revoked { id: String, reason: String },
    #[error("version {version} of {id} is not in the plugin registry")]
    NoSuchVersion { id: String, version: String },
    #[error("version {version} of {id} was yanked and cannot be installed")]
    Yanked { id: String, version: String },
    #[error("version {version} of {id} cannot run on this coolify-rs: {reason}")]
    Incompatible {
        id: String,
        version: String,
        reason: String,
    },
    #[error("{0} has no version this coolify-rs can install")]
    NoInstallableVersion(String),
    #[error("{0} is already installed; update it instead")]
    AlreadyInstalled(String),
    #[error("{0} is not installed")]
    NotInstalled(String),
    #[error("{id} is already at version {version}")]
    UpToDate { id: String, version: String },
    #[error("approve the capabilities of {id} {version} by sending them back as grant")]
    GrantRequired {
        id: String,
        version: String,
        capabilities: Vec<String>,
        /// What the version asks for beyond the current grant.
        added: Vec<String>,
        /// Whether the version is now recorded as a pending update.
        pending: bool,
    },
    #[error("could not fetch {url}: {error}")]
    Fetch { url: String, error: String },
    #[error("the registry index failed verification: {0}")]
    Verify(String),
    #[error(
        "the component of {id} {version} does not match the registry: its SHA-256 is {actual}, the index says {expected}"
    )]
    Integrity {
        id: String,
        version: String,
        expected: String,
        actual: String,
    },
    #[error(transparent)]
    Plugin(#[from] PluginError),
}

impl From<sqlx::Error> for RegistryError {
    fn from(e: sqlx::Error) -> Self {
        RegistryError::Plugin(PluginError::Db(e))
    }
}

/// What a refresh found.
#[derive(Debug, Clone, Serialize)]
pub struct Refreshed {
    pub url: String,
    pub key_id: String,
    pub generated_at: DateTime<Utc>,
    pub fetched_at: DateTime<Utc>,
    pub plugins: usize,
    /// Installed plugins this refresh switched off because the registry revoked them.
    pub revoked: Vec<String>,
    /// Installed plugins with a newer version that asks for more capabilities, found by this refresh.
    pub pending_updates: Vec<String>,
}

fn egress(host: &Host) -> Egress {
    Egress {
        max_body: host.limits().max_component,
        ..host.settings().egress.clone()
    }
}

async fn download(host: &Host, url: &str) -> Result<Vec<u8>, RegistryError> {
    let failed = |error: String| RegistryError::Fetch {
        url: url.to_owned(),
        error,
    };
    let parsed = reqwest::Url::parse(url).map_err(|e| failed(e.to_string()))?;
    let allowed = vec![parsed.host_str().unwrap_or_default().to_owned()];
    let r = egress(host)
        .fetch(
            &allowed,
            Outgoing {
                method: "GET".into(),
                url: url.to_owned(),
                headers: vec![],
                body: vec![],
            },
        )
        .await
        .map_err(failed)?;
    if r.status != 200 {
        return Err(failed(format!("the server answered {}", r.status)));
    }
    Ok(r.body)
}

/// Fetches and verifies the index, stores it, and applies its revocations and pending updates.
pub async fn refresh(host: &Host) -> Result<Refreshed, RegistryError> {
    let cfg = &host.settings().registry;
    let base = cfg.url.trim_end_matches('/');
    let bytes = download(host, &format!("{base}/index.json")).await?;
    let signature = download(host, &format!("{base}/index.json.sig")).await?;
    let key = signing::verify(&bytes, &signature, &cfg.keys, Utc::now())
        .map_err(|e| RegistryError::Verify(e.to_string()))?;
    let index = Index::parse(&bytes).map_err(RegistryError::Verify)?;
    let db = host.db();
    if let Some(stored) = db::load_index(db).await?
        && stored.url == base
        && index.generated_at < stored.generated_at
    {
        // An old signed index would bring back revoked plugins and yanked versions.
        return Err(RegistryError::Verify(format!(
            "the index was generated at {}, before the one already fetched ({}); refusing to go back",
            index.generated_at, stored.generated_at
        )));
    }
    let fetched_at =
        db::save_index(db, base, &bytes, &signature, &key.id, index.generated_at).await?;
    let (revoked, pending_updates) = apply(host, &index).await?;
    tracing::info!(
        plugins = index.plugins.len(),
        key = %key.id,
        generated_at = %index.generated_at,
        ?revoked,
        ?pending_updates,
        "plugin registry refreshed"
    );
    Ok(Refreshed {
        url: base.to_owned(),
        key_id: key.id.clone(),
        generated_at: index.generated_at,
        fetched_at,
        plugins: index.plugins.len(),
        revoked,
        pending_updates,
    })
}

fn revoked_reason(reason: &str) -> String {
    format!("revoked by the plugin registry: {reason}")
}

/// Revocations switch installed plugins off; newer versions with more capabilities become pending updates.
async fn apply(host: &Host, index: &Index) -> Result<(Vec<String>, Vec<String>), RegistryError> {
    let db = host.db();
    let list: Vec<(String, String)> = index
        .plugins
        .iter()
        .filter_map(|e| Some((e.id.clone(), e.revoked.as_ref()?.reason.clone())))
        .collect();
    db::set_revoked(db, &list).await?;
    let mut revoked = Vec::new();
    let mut pending = Vec::new();
    for p in store::list(db).await? {
        let Some(entry) = index.entry(&p.id) else {
            db::clear_pending(db, &p.id).await?;
            continue;
        };
        if let Some(r) = &entry.revoked {
            let reason = revoked_reason(&r.reason);
            if p.enabled || p.disabled_reason.as_deref() != Some(reason.as_str()) {
                host.unload(&p.id);
                store::set_enabled(db, &p.id, false, Some(&reason)).await?;
                store::audit(
                    db,
                    &p.id,
                    None,
                    None,
                    "revoked",
                    json!({ "reason": r.reason }),
                )
                .await?;
                tracing::warn!(plugin = %p.id, reason = %r.reason, "plugin revoked by the registry and switched off");
                revoked.push(p.id.clone());
            }
            db::clear_pending(db, &p.id).await?;
            continue;
        }
        match entry.latest() {
            Some((release, m))
                if compare_versions(&release.version, &p.version).is_gt()
                    && m.capabilities.exceeds(&p.granted) =>
            {
                if record_pending(host, &p, release, &m).await? {
                    pending.push(p.id.clone());
                }
            }
            _ => db::clear_pending(db, &p.id).await?,
        }
    }
    Ok((revoked, pending))
}

/// What `m` asks for beyond what `p` was granted.
fn added(p: &Installed, m: &Manifest) -> Vec<String> {
    let granted = capability_items(&p.granted);
    capability_items(&m.capabilities)
        .difference(&granted)
        .cloned()
        .collect()
}

async fn record_pending(
    host: &Host,
    p: &Installed,
    release: &Release,
    m: &Manifest,
) -> Result<bool, RegistryError> {
    let db = host.db();
    let new = db::set_pending(
        db,
        &p.id,
        &release.version,
        &release.manifest,
        &release.sha256,
        &release.url,
    )
    .await?;
    if new {
        store::audit(
            db,
            &p.id,
            None,
            None,
            "update_pending",
            json!({ "version": release.version, "added": added(p, m) }),
        )
        .await?;
    }
    Ok(new)
}

/// The stored index, verified again with the keys trusted now.
async fn verified_index(host: &Host) -> Result<Index, RegistryError> {
    let stored = db::load_index(host.db())
        .await?
        .ok_or(RegistryError::NoIndex)?;
    signing::verify(
        &stored.bytes,
        &stored.signature,
        &host.settings().registry.keys,
        Utc::now(),
    )
    .map_err(|e| RegistryError::Verify(e.to_string()))?;
    Index::parse(&stored.bytes).map_err(RegistryError::Verify)
}

fn choose<'e>(
    entry: &'e Entry,
    version: Option<&str>,
) -> Result<(&'e Release, Manifest), RegistryError> {
    let id = &entry.id;
    match version {
        Some(v) => {
            let r = entry.find(v).ok_or_else(|| RegistryError::NoSuchVersion {
                id: id.clone(),
                version: v.to_owned(),
            })?;
            if r.yanked {
                return Err(RegistryError::Yanked {
                    id: id.clone(),
                    version: v.to_owned(),
                });
            }
            let m = r.manifest().map_err(|reason| RegistryError::Incompatible {
                id: id.clone(),
                version: v.to_owned(),
                reason,
            })?;
            Ok((r, m))
        }
        None => entry
            .latest()
            .ok_or_else(|| RegistryError::NoInstallableVersion(id.clone())),
    }
}

fn not_revoked(entry: &Entry) -> Result<(), RegistryError> {
    match &entry.revoked {
        Some(r) => Err(RegistryError::Revoked {
            id: entry.id.clone(),
            reason: r.reason.clone(),
        }),
        None => Ok(()),
    }
}

/// Downloads a component and checks it is exactly the one the index names.
async fn component(host: &Host, id: &str, r: &Release) -> Result<Vec<u8>, RegistryError> {
    let bytes = download(host, &r.url).await?;
    let actual = hex::encode(Sha256::digest(&bytes));
    if actual != r.sha256 {
        return Err(RegistryError::Integrity {
            id: id.to_owned(),
            version: r.version.clone(),
            expected: r.sha256.clone(),
            actual,
        });
    }
    Ok(bytes)
}

/// Installs a plugin from the registry: `version`, or the newest that is not yanked. `grant` must repeat
/// the version's capability list exactly; without it nothing is downloaded and the error carries the list.
pub async fn install(
    host: &Host,
    id: &str,
    version: Option<&str>,
    grant: Option<&[String]>,
    user_id: Option<i64>,
) -> Result<Installed, RegistryError> {
    let index = verified_index(host).await?;
    let entry = index
        .entry(id)
        .ok_or_else(|| RegistryError::Unknown(id.to_owned()))?;
    not_revoked(entry)?;
    if store::find(host.db(), id).await?.is_some() {
        return Err(RegistryError::AlreadyInstalled(id.to_owned()));
    }
    let (release, manifest) = choose(entry, version)?;
    let capabilities = manifest.capability_summary();
    if grant != Some(capabilities.as_slice()) {
        return Err(RegistryError::GrantRequired {
            id: id.to_owned(),
            version: release.version.clone(),
            added: capability_items(&manifest.capabilities)
                .into_iter()
                .collect(),
            capabilities,
            pending: false,
        });
    }
    let bytes = component(host, id, release).await?;
    Ok(host
        .install(manifest, &bytes, "registry", true, user_id)
        .await?)
}

/// Updates an installed plugin to `version`, or to the newest that is not yanked.
///
/// A version that asks for no more than was granted is installed directly. One that asks for more is
/// recorded as a pending update and refused until the admin sends its capability list as `grant`; the
/// installed version keeps running meanwhile.
pub async fn update(
    host: &Host,
    id: &str,
    version: Option<&str>,
    grant: Option<&[String]>,
    user_id: Option<i64>,
) -> Result<Installed, RegistryError> {
    let db = host.db();
    let p = store::find(db, id)
        .await?
        .ok_or_else(|| RegistryError::NotInstalled(id.to_owned()))?;
    let index = verified_index(host).await?;
    let entry = index
        .entry(id)
        .ok_or_else(|| RegistryError::Unknown(id.to_owned()))?;
    not_revoked(entry)?;
    let (release, manifest) = choose(entry, version)?;
    if release.version == p.version && p.origin == "registry" && p.sha256 == release.sha256 {
        return Err(RegistryError::UpToDate {
            id: id.to_owned(),
            version: p.version,
        });
    }
    let capabilities = manifest.capability_summary();
    let widens = manifest.capabilities.exceeds(&p.granted);
    let approved = grant == Some(capabilities.as_slice());
    if (widens && !approved) || (grant.is_some() && !approved) {
        let pending = widens && record_pending(host, &p, release, &manifest).await?;
        return Err(RegistryError::GrantRequired {
            id: id.to_owned(),
            version: release.version.clone(),
            added: added(&p, &manifest),
            capabilities,
            pending,
        });
    }
    let more = added(&p, &manifest);
    let bytes = component(host, id, release).await?;
    let installed = host
        .install(manifest, &bytes, "registry", true, user_id)
        .await?;
    db::clear_pending(db, id).await?;
    if widens {
        store::audit(
            db,
            id,
            None,
            user_id,
            "update_approved",
            json!({ "version": release.version, "added": more }),
        )
        .await?;
    }
    Ok(installed)
}

/// The in-app catalogue: every plugin of the stored index, with what this instance has installed.
#[derive(Debug, Clone, Serialize)]
pub struct Catalog {
    pub index: Option<IndexInfo>,
    pub plugins: Vec<CatalogEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexInfo {
    pub url: String,
    pub key_id: String,
    pub generated_at: DateTime<Utc>,
    pub fetched_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub license: String,
    pub source: Option<String>,
    /// The newest version that is not yanked and runs on this coolify-rs.
    pub latest_version: Option<String>,
    /// The capabilities of `latest_version`.
    pub capabilities: Vec<String>,
    /// Signed by a trusted registry key. Always true here; uploads are the unverified ones.
    pub verified: bool,
    pub installed_version: Option<String>,
    /// The installed version has since been yanked.
    pub installed_yanked: bool,
    /// `latest_version` is newer than the installed one.
    pub update_available: bool,
    /// Updating to `latest_version` needs capabilities that were not granted.
    pub update_requires_approval: bool,
    pub pending_update: Option<PendingUpdate>,
    pub revoked: Option<Revoked>,
    /// Newest first.
    pub versions: Vec<VersionInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingUpdate {
    pub version: String,
    pub capabilities: Vec<String>,
    pub added: Vec<String>,
    pub found_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VersionInfo {
    pub version: String,
    pub yanked: bool,
    /// Whether this coolify-rs can run it.
    pub compatible: bool,
    pub capabilities: Vec<String>,
}

pub async fn catalog(host: &Host) -> Result<Catalog, RegistryError> {
    let db = host.db();
    let Some(stored) = db::load_index(db).await? else {
        return Ok(Catalog {
            index: None,
            plugins: vec![],
        });
    };
    let index = Index::parse(&stored.bytes).map_err(RegistryError::Verify)?;
    let installed = store::list(db).await?;
    let pending = db::pending(db).await?;
    let plugins = index
        .plugins
        .iter()
        .map(|e| {
            let p = installed.iter().find(|p| p.id == e.id);
            let pend = pending.iter().find(|x| x.plugin_id == e.id);
            entry(e, p, pend)
        })
        .collect();
    Ok(Catalog {
        index: Some(IndexInfo {
            url: stored.url,
            key_id: stored.key_id,
            generated_at: stored.generated_at,
            fetched_at: stored.fetched_at,
        }),
        plugins,
    })
}

fn entry(e: &Entry, installed: Option<&Installed>, pending: Option<&db::Pending>) -> CatalogEntry {
    let latest = e.latest();
    let newest = e.sorted().into_iter().next();
    let text = |k: &str| {
        latest
            .as_ref()
            .map(|(r, _)| *r)
            .or(newest)
            .and_then(|r| r.manifest.get(k))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };
    let newer = |v: &str| installed.is_some_and(|p| compare_versions(v, &p.version).is_gt());
    let (update_available, update_requires_approval) = match (&latest, installed) {
        (Some((r, m)), Some(p)) if newer(&r.version) => (true, m.capabilities.exceeds(&p.granted)),
        _ => (false, false),
    };
    CatalogEntry {
        id: e.id.clone(),
        name: text("name").unwrap_or_else(|| e.id.clone()),
        description: text("description").unwrap_or_default(),
        license: text("license").unwrap_or_default(),
        source: latest
            .as_ref()
            .map(|(r, _)| r.source.clone())
            .or_else(|| newest.map(|r| r.source.clone())),
        latest_version: latest.as_ref().map(|(r, _)| r.version.clone()),
        capabilities: latest
            .as_ref()
            .map(|(_, m)| m.capability_summary())
            .unwrap_or_default(),
        verified: true,
        installed_version: installed.map(|p| p.version.clone()),
        installed_yanked: installed
            .and_then(|p| e.find(&p.version))
            .is_some_and(|r| r.yanked),
        update_available,
        update_requires_approval,
        pending_update: match (pending, installed) {
            (Some(x), Some(p)) => serde_json::from_value::<Manifest>(x.manifest.clone())
                .ok()
                .map(|m| PendingUpdate {
                    version: x.version.clone(),
                    capabilities: m.capability_summary(),
                    added: added(p, &m),
                    found_at: x.found_at,
                }),
            _ => None,
        },
        revoked: e.revoked.clone(),
        versions: e
            .sorted()
            .into_iter()
            .map(|r| {
                let m = r.manifest().ok();
                VersionInfo {
                    version: r.version.clone(),
                    yanked: r.yanked,
                    compatible: m.is_some(),
                    capabilities: m.map(|m| m.capability_summary()).unwrap_or_default(),
                }
            })
            .collect(),
    }
}

/// Refreshes the index a minute after start and then once a day, as the update check does.
pub fn spawn_daily_refresh(host: Host) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let mut every = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
        loop {
            every.tick().await;
            if let Err(e) = refresh(&host).await {
                tracing::warn!(error = %e, "could not refresh the plugin registry");
            }
        }
    })
}
