//! Access to an upstream-compatible Coolify PostgreSQL database.
//!
//! coolify-rs adopts the upstream schema as-is and never runs DDL against it
//! (docs/DATABASE_COMPATIBILITY.md). Every query names its table explicitly,
//! scopes by team where upstream does, and writes rows exactly as upstream's
//! models would (including model-event side effects such as the default
//! `production` environment of a new project).
//!
//! Queries add no `ORDER BY` where upstream has none: results then come in the
//! same (heap) order upstream returns, which differential tests compare byte
//! for byte.

pub mod applications;
pub mod auth;
pub mod cloud;
pub mod compose;
pub mod dashboard;
pub mod database_parts;
pub mod database_specs;
pub mod databases;
pub mod destinations;
pub mod environment_variables;
pub mod git_sources;
pub mod github_runners;
pub mod instance;
pub mod private_keys;
pub mod projects;
pub mod records;
pub mod schema;
pub mod server_ops;
pub mod servers;
pub mod services;
pub mod teams;
#[cfg(feature = "testing")]
pub mod testing;
pub mod ui_servers;
pub mod users;

use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, PgPool};

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("schema guard: {0}")]
    Schema(String),
    #[error("writes are disabled: {0}")]
    ReadOnly(String),
}

pub type DbResult<T> = Result<T, DbError>;

/// Handle to the database plus what the schema guard learned at startup.
#[derive(Debug, Clone)]
pub struct Db {
    pool: PgPool,
    schema: schema::SchemaInfo,
}

#[derive(Debug, Clone)]
pub struct DbConfig {
    pub url: String,
    pub max_connections: u32,
    pub acquire_timeout: Duration,
    /// Permit writes even if the database has upstream migrations this build
    /// does not know about (COOLIFY_RS_ALLOW_UNKNOWN_SCHEMA).
    pub allow_unknown_schema: bool,
}

impl Db {
    pub async fn connect(cfg: &DbConfig) -> DbResult<Self> {
        let opts: PgConnectOptions = cfg
            .url
            .parse::<PgConnectOptions>()?
            // Upstream writes naive timestamps in UTC (config/database.php:37).
            .options([("timezone", "UTC")])
            .log_statements(tracing::log::LevelFilter::Debug);
        let pool = PgPoolOptions::new()
            .max_connections(cfg.max_connections)
            .acquire_timeout(cfg.acquire_timeout)
            .connect_with(opts)
            .await?;
        let schema = schema::inspect(&pool, cfg.allow_unknown_schema).await?;
        Ok(Self { pool, schema })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn schema(&self) -> &schema::SchemaInfo {
        &self.schema
    }

    /// Fails unless the schema guard allows writes.
    pub fn ensure_writable(&self) -> DbResult<()> {
        match &self.schema.read_only_reason {
            None => Ok(()),
            Some(reason) => Err(DbError::ReadOnly(reason.clone())),
        }
    }

    pub async fn ping(&self) -> DbResult<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }
}
