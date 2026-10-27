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
}

/// Optional settings; `Default` is suitable for tests.
#[derive(Debug)]
pub struct Options {
    pub encrypter: Option<Encrypter>,
    pub ui_dir: Option<PathBuf>,
    pub secure_cookies: bool,
    pub session_idle: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            encrypter: None,
            ui_dir: None,
            secure_cookies: false,
            // Upstream SESSION_LIFETIME default: 10080 minutes.
            session_idle: Duration::from_secs(10080 * 60),
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
        Self(Arc::new(Inner {
            db,
            version,
            trusted_proxies,
            encrypter: o.encrypter,
            sessions: SessionStore::new(o.session_idle),
            ui_dir: o.ui_dir,
            secure_cookies: o.secure_cookies,
        }))
    }

    /// Current UTC time at second precision, as Laravel writes `timestamp(0)`.
    pub fn now(&self) -> NaiveDateTime {
        Utc::now().naive_utc().trunc_subsecs(0)
    }
}
