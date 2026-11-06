//! Shared application state.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{NaiveDateTime, SubsecRound, Utc};
use coolify_compat::Encrypter;
use coolify_db::Db;

use crate::ip::TrustedProxies;
use crate::session::SessionStore;

#[derive(Debug, Clone)]
pub struct AppState(Arc<Inner>);

#[derive(Debug)]
pub struct Inner {
    pub db: Db,
    /// Reported by `/api/v1/version`: the upstream version whose API contract
    /// this build implements.
    pub version: String,
    pub trusted_proxies: TrustedProxies,
    /// Laravel encrypter for `APP_KEY`; required by features that read
    /// encrypted columns (private keys, 2FA secrets).
    pub encrypter: Option<Encrypter>,
    pub sessions: SessionStore,
    /// Directory with the built web UI (`frontend/dist`), served at `/`.
    pub ui_dir: Option<PathBuf>,
    /// Add `Secure` to the session cookie (set when served over HTTPS).
    pub secure_cookies: bool,
    /// Background work on servers; `None` when remote execution is not configured.
    pub jobs: Option<coolify_jobs::Jobs>,
    /// Plugins (ADR 0010); `None` when plugins are switched off.
    pub plugins: Option<coolify_plugins::Host>,
    /// `COOLIFY_RS_ALLOW_UNVERIFIED_PLUGINS`: admins may install plugins that are not from the registry.
    pub allow_unverified_plugins: bool,
}

/// Optional settings; `Default` is suitable for tests.
#[derive(Debug)]
pub struct Options {
    pub encrypter: Option<Encrypter>,
    pub ui_dir: Option<PathBuf>,
    pub secure_cookies: bool,
    pub session_idle: Duration,
    pub remote: Option<coolify_remote::Remote>,
    pub plugins: Option<coolify_plugins::Host>,
    pub allow_unverified_plugins: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            encrypter: None,
            ui_dir: None,
            secure_cookies: false,
            // Upstream SESSION_LIFETIME default: 10080 minutes.
            session_idle: Duration::from_secs(10080 * 60),
            remote: None,
            plugins: None,
            allow_unverified_plugins: false,
        }
    }
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl AppState {
    pub fn new(db: Db, version: String, trusted_proxies: TrustedProxies) -> Self {
        Self::with_options(db, version, trusted_proxies, Options::default())
    }

    pub fn with_options(
        db: Db,
        version: String,
        trusted_proxies: TrustedProxies,
        o: Options,
    ) -> Self {
        let jobs = o.remote.map(|r| {
            let jobs = coolify_jobs::Jobs::new(db.clone(), r, o.encrypter.clone());
            jobs.set_version(version.clone());
            jobs
        });
        Self(Arc::new(Inner {
            jobs,
            db,
            version,
            trusted_proxies,
            encrypter: o.encrypter,
            sessions: SessionStore::new(o.session_idle),
            ui_dir: o.ui_dir,
            secure_cookies: o.secure_cookies,
            plugins: o.plugins,
            allow_unverified_plugins: o.allow_unverified_plugins,
        }))
    }

    /// Current UTC time at second precision, as Laravel writes `timestamp(0)`.
    pub fn now(&self) -> NaiveDateTime {
        Utc::now().naive_utc().trunc_subsecs(0)
    }
}
