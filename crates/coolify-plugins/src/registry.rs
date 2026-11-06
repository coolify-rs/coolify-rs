//! The plugin registry (ADR 0010, section 7): a curated Git repository whose CI publishes a signed
//! `index.json`. coolify-rs fetches it, checks the signature against keys compiled in, and installs
//! components whose SHA-256 matches the index. The index can yank versions and revoke plugins; revoked
//! plugins are switched off on every instance at the next refresh.
//!
//! - [`signing`]: Ed25519 signatures and the trusted keys.
//! - [`index`]: the `index.json` format.
//! - [`source`]: the registry repository's files, and building the index from them.
//! - [`review`]: the pull request checks that need no build.
//! - [`conformance`]: `coolify-rs plugin check`, the component in the real sandbox.
//! - [`client`]: refreshing, installing and updating on an instance.

pub mod client;
pub mod conformance;
pub mod db;
pub mod index;
pub mod review;
pub mod signing;
pub mod source;

pub use client::{
    Catalog, CatalogEntry, Refreshed, RegistryError, catalog, install, refresh,
    spawn_daily_refresh, update,
};
pub use signing::TrustedKey;

use coolify_db::Db;

use crate::PluginError;

/// `COOLIFY_RS_PLUGIN_REGISTRY_URL`'s default: the `dist/` folder of the registry repository.
pub const DEFAULT_URL: &str = "https://raw.githubusercontent.com/coolify-rs/plugins/main/dist";

/// Where the registry is and whom to trust.
#[derive(Debug, Clone)]
pub struct Config {
    /// The folder holding `index.json` and `index.json.sig`, without a trailing slash.
    pub url: String,
    /// Keys whose signatures are accepted.
    pub keys: Vec<TrustedKey>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            url: DEFAULT_URL.into(),
            keys: signing::production_keys(),
        }
    }
}

impl Config {
    /// From `COOLIFY_RS_PLUGIN_REGISTRY_URL` and `COOLIFY_RS_PLUGIN_REGISTRY_KEYS`. Extra keys are trusted only
    /// in development mode (`COOLIFY_RS_ALLOW_UNVERIFIED_PLUGINS`); otherwise they are ignored with a
    /// warning, so a stray variable cannot make an instance trust another signer.
    pub fn from_values(
        url: Option<&str>,
        extra_keys: Option<&str>,
        development: bool,
    ) -> Result<Config, String> {
        let mut cfg = Config::default();
        if let Some(url) = url.filter(|u| !u.is_empty()) {
            let parsed = reqwest::Url::parse(url)
                .map_err(|e| format!("COOLIFY_RS_PLUGIN_REGISTRY_URL: {e}"))?;
            if !matches!(parsed.scheme(), "https" | "http") {
                return Err("COOLIFY_RS_PLUGIN_REGISTRY_URL must be an https URL".into());
            }
            cfg.url = url.trim_end_matches('/').to_owned();
        }
        if let Some(extra) = extra_keys.filter(|k| !k.trim().is_empty()) {
            if development {
                let keys = signing::parse_keys(extra)
                    .map_err(|e| format!("COOLIFY_RS_PLUGIN_REGISTRY_KEYS: {e}"))?;
                cfg.keys.extend(keys);
            } else {
                tracing::warn!(
                    "COOLIFY_RS_PLUGIN_REGISTRY_KEYS is ignored: extra registry keys are trusted only with COOLIFY_RS_ALLOW_UNVERIFIED_PLUGINS=true"
                );
            }
        }
        Ok(cfg)
    }
}

/// Fails when the registry revoked `id`.
pub(crate) async fn ensure_not_revoked(db: &Db, id: &str) -> Result<(), PluginError> {
    match db::revoked(db, id).await? {
        Some(reason) => Err(PluginError::Revoked {
            id: id.to_owned(),
            reason,
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_keys_only_in_development_mode() {
        let cfg = Config::from_values(None, Some(signing::DEV_KEY_ENV), false).unwrap();
        assert_eq!(cfg.url, DEFAULT_URL);
        assert!(cfg.keys.iter().all(|k| k.id != signing::DEV_KEY_ID));

        let cfg = Config::from_values(
            Some("https://registry.example.com/dist/"),
            Some(signing::DEV_KEY_ENV),
            true,
        )
        .unwrap();
        assert_eq!(cfg.url, "https://registry.example.com/dist");
        assert!(cfg.keys.iter().any(|k| k.id == signing::DEV_KEY_ID));

        assert!(Config::from_values(Some("file:///etc"), None, false).is_err());
        assert!(Config::from_values(None, Some("bad"), true).is_err());
    }
}
