//! coolify-rs server.
//!
//! Configuration uses upstream's variable names where they exist (`DB_*`,
//! `COOLIFY_VERSION`) so it can run beside an existing installation; settings
//! specific to coolify-rs are prefixed `COOLIFY_RS_`.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderName, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use clap::{Parser, Subcommand};
use coolify_api::AppState;
use coolify_api::ip::TrustedProxies;
use coolify_db::{Db, DbConfig};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

/// The upstream release whose API contract and schema this build implements.
const UPSTREAM_COMPAT_VERSION: &str = "4.5.0";

#[derive(Parser)]
#[command(name = "coolify-rs", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve the API (default).
    Serve,
    /// Inspect the database and print the schema guard result as JSON.
    CheckSchema,
}

#[derive(Debug)]
struct Config {
    listen: SocketAddr,
    db: DbConfig,
    version: String,
    trusted_proxies: TrustedProxies,
    json_logs: bool,
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn database_url() -> Result<String> {
    if let Some(url) = env("DATABASE_URL") {
        return Ok(url);
    }
    // Upstream's variables (docker-compose.prod.yml / install.sh).
    let host = env("DB_HOST").unwrap_or_else(|| "127.0.0.1".into());
    let port = env("DB_PORT").unwrap_or_else(|| "5432".into());
    let db = env("DB_DATABASE").unwrap_or_else(|| "coolify".into());
    let user = env("DB_USERNAME").context("set DATABASE_URL or DB_USERNAME/DB_PASSWORD")?;
    let pass = env("DB_PASSWORD").unwrap_or_default();
    Ok(format!(
        "postgres://{}:{}@{host}:{port}/{db}",
        urlencode(&user),
        urlencode(&pass)
    ))
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

impl Config {
    fn from_env() -> Result<Self> {
        Ok(Self {
            listen: env("COOLIFY_RS_LISTEN")
                .unwrap_or_else(|| "0.0.0.0:8080".into())
                .parse()
                .context("COOLIFY_RS_LISTEN must be host:port")?,
            db: DbConfig {
                url: database_url()?,
                max_connections: env("COOLIFY_RS_DB_MAX_CONNECTIONS")
                    .map(|v| v.parse())
                    .transpose()
                    .context("COOLIFY_RS_DB_MAX_CONNECTIONS")?
                    .unwrap_or(10),
                acquire_timeout: Duration::from_secs(5),
                allow_unknown_schema: env("COOLIFY_RS_ALLOW_UNKNOWN_SCHEMA")
                    .is_some_and(|v| v == "true"),
            },
            version: env("COOLIFY_VERSION").unwrap_or_else(|| UPSTREAM_COMPAT_VERSION.into()),
            trusted_proxies: TrustedProxies::parse(
                &env("COOLIFY_RS_TRUSTED_PROXIES").unwrap_or_default(),
            )
            .map_err(anyhow::Error::msg)
            .context("COOLIFY_RS_TRUSTED_PROXIES")?,
            json_logs: env("COOLIFY_RS_LOG_FORMAT").is_none_or(|v| v != "pretty"),
        })
    }
}

fn init_tracing(json: bool) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,sqlx=warn,tower_http=info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if json {
        builder.json().flatten_event(true).init();
    } else {
        builder.init();
    }
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    match state.db.ping().await {
        Ok(()) => {
            let read_only = state.db.schema().read_only_reason.clone();
            (
                StatusCode::OK,
                axum::Json(serde_json::json!({"status": "ready", "read_only": read_only})),
            )
        }
        Err(e) => {
            tracing::warn!(error = %e, "readiness check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(serde_json::json!({"status": "unavailable"})),
            )
        }
    }
}

fn app(state: AppState) -> Router {
    let request_id = HeaderName::from_static("x-request-id");
    let ops = Router::new()
        .route("/livez", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
        .with_state(state.clone());
    coolify_api::router(state)
        .merge(ops)
        .layer(
            TraceLayer::new_for_http().make_span_with(|req: &axum::http::Request<_>| {
                // Never record headers: Authorization carries bearer tokens.
                let id = req
                    .headers()
                    .get("x-request-id")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("-");
                tracing::info_span!("request", method = %req.method(), path = %req.uri().path(), request_id = %id)
            }),
        )
        .layer(PropagateRequestIdLayer::new(request_id.clone()))
        .layer(SetRequestIdLayer::new(request_id, MakeRequestUuid))
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_secs(30)))
        .layer(axum::extract::DefaultBodyLimit::max(2 * 1024 * 1024))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { () = ctrl_c => {}, () = term => {} }
    tracing::info!("shutdown signal received, draining connections");
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::from_env()?;
    init_tracing(cfg.json_logs);

    let db = Db::connect(&cfg.db)
        .await
        .context("connecting to the Coolify database")?;
    let schema = db.schema();
    match cli.cmd.unwrap_or(Cmd::Serve) {
        Cmd::CheckSchema => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "applied_migrations": schema.applied,
                    "unknown_migrations": schema.unknown,
                    "missing_migrations": schema.missing,
                    "read_only_reason": schema.read_only_reason,
                }))?
            );
            Ok(())
        }
        Cmd::Serve => {
            if let Some(reason) = &schema.read_only_reason {
                tracing::warn!(%reason, "starting in read-only mode");
            }
            tracing::info!(
                listen = %cfg.listen,
                api_version = %cfg.version,
                applied_migrations = schema.applied,
                "coolify-rs starting"
            );
            let state = AppState::new(db, cfg.version, cfg.trusted_proxies);
            let listener = tokio::net::TcpListener::bind(cfg.listen).await?;
            axum::serve(
                listener,
                app(state).into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown_signal())
            .await?;
            Ok(())
        }
    }
}
