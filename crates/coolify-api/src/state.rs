//! Shared application state.

use std::sync::Arc;

use chrono::{NaiveDateTime, SubsecRound, Utc};
use coolify_db::Db;

use crate::ip::TrustedProxies;

#[derive(Debug, Clone)]
pub struct AppState(Arc<Inner>);

#[derive(Debug)]
pub struct Inner {
    pub db: Db,
    /// Reported by `/api/v1/version`: the upstream version whose API contract
    /// this build implements.
    pub version: String,
    pub trusted_proxies: TrustedProxies,
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl AppState {
    pub fn new(db: Db, version: String, trusted_proxies: TrustedProxies) -> Self {
        Self(Arc::new(Inner {
            db,
            version,
            trusted_proxies,
        }))
    }

    /// Current UTC time at second precision, as Laravel writes `timestamp(0)`.
    pub fn now(&self) -> NaiveDateTime {
        Utc::now().naive_utc().trunc_subsecs(0)
    }
}
