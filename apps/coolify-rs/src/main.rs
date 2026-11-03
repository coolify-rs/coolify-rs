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
use coolify_api::state::Options;
use coolify_compat::Encrypter;
use coolify_db::{Db, DbConfig};
use coolify_remote::{HostKeyPolicy, Remote, RemoteConfig};
use std::path::PathBuf;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::{DefaultOnFailure, DefaultOnResponse, TraceLayer};

/// glibc's malloc keeps freed memory in per-thread arenas, so after a burst of large responses the
/// process stayed at its peak. mimalloc returns it to the system.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// The upstream release whose API contract and schema this build implements.
const UPSTREAM_COMPAT_VERSION: &str = "4.5.1-dev.b5823c777";

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
    /// What an installed server runs: prepare the database and first-run rows, then serve.
    /// An empty database gets the baseline schema; a database with tables keeps what it has.
    Boot,
    /// Create the baseline schema. Refuses any database that already has a table.
    CreateSchema,
    /// Ask the running server whether it answers (the container's health check).
    Healthcheck,
    /// Inspect the database and print the schema guard result as JSON.
    CheckSchema,
    /// Run one scheduled server job now and print what it returns (operators and differential tests).
    ServerJob {
        /// connection-check, storage-check, server-check, patch-check, traefik-check, ca-certificate.
        job: String,
        server_uuid: String,
    },
    /// `dns:release-orphaned-records`: release managed DNS records no live resource references.
    #[command(name = "dns:release-orphaned-records")]
    ReleaseOrphanedDnsRecords,
}

#[derive(Debug)]
struct Config {
    listen: SocketAddr,
    db: DbConfig,
    version: String,
    trusted_proxies: TrustedProxies,
    json_logs: bool,
    encrypter: Option<Encrypter>,
    ui_dir: Option<PathBuf>,
    secure_cookies: bool,
    session_idle: Duration,
    remote: RemoteConfig,
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

/// Remote execution (ADR 0009). `SSH_COMMAND_TIMEOUT` is upstream's variable.
fn remote_config() -> Result<RemoteConfig> {
    let mut r =
        RemoteConfig::new(env("COOLIFY_RS_DATA_DIR").unwrap_or_else(|| "/data/coolify-rs".into()));
    r.host_keys = match env("COOLIFY_RS_SSH_HOST_KEYS").as_deref() {
        None | Some("pin") => HostKeyPolicy::Pin,
        Some("ignore") => HostKeyPolicy::Ignore,
        Some(other) => anyhow::bail!("COOLIFY_RS_SSH_HOST_KEYS must be pin or ignore, not {other}"),
    };
    if let Some(n) = env("COOLIFY_RS_SSH_MAX_CONCURRENCY") {
        r.max_concurrency = n.parse().context("COOLIFY_RS_SSH_MAX_CONCURRENCY")?;
    }
    if let Some(secs) = env("SSH_COMMAND_TIMEOUT") {
        let secs: i64 = secs.parse().context("SSH_COMMAND_TIMEOUT (seconds)")?;
        // Upstream falls back to 3600 for zero or negative values.
        r.command_timeout = Duration::from_secs(if secs > 0 { secs.unsigned_abs() } else { 3600 });
    }
    if let Some(bin) = env("COOLIFY_RS_SSH_BINARY") {
        r.ssh_binary = PathBuf::from(bin);
    }
    Ok(r)
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
            // Upstream's variables: APP_KEY (base64:...) and APP_PREVIOUS_KEYS.
            encrypter: match env("APP_KEY") {
                None => None,
                Some(key) => Some(
                    Encrypter::new(&key)
                        .and_then(|e| {
                            e.with_previous_keys(&env("APP_PREVIOUS_KEYS").unwrap_or_default())
                        })
                        .map_err(|e| anyhow::anyhow!("APP_KEY: {e}"))?,
                ),
            },
            ui_dir: env("COOLIFY_RS_UI_DIR")
                .map(PathBuf::from)
                .or_else(|| Some(PathBuf::from("frontend/dist")))
                .filter(|p| p.join("index.html").is_file()),
            secure_cookies: env("COOLIFY_RS_SECURE_COOKIES").is_some_and(|v| v == "true"),
            session_idle: Duration::from_secs(
                env("SESSION_LIFETIME")
                    .map(|v| v.parse::<u64>())
                    .transpose()
                    .context("SESSION_LIFETIME (minutes)")?
                    .unwrap_or(10080)
                    * 60,
            ),
            remote: remote_config()?,
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
            })
            .on_response(DefaultOnResponse::new().level(tracing::Level::INFO))
            .on_failure(DefaultOnFailure::new().level(tracing::Level::WARN)),
        )
        .layer(PropagateRequestIdLayer::new(request_id.clone()))
        .layer(SetRequestIdLayer::new(request_id, MakeRequestUuid))
        .layer(axum::middleware::from_fn(request_timeout))
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

fn main() -> Result<()> {
    // The deployment job is a deep chain of large futures; an unoptimized build overflows the
    // default 2 MiB worker stack while polling it.
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(16 * 1024 * 1024)
        .build()?
        .block_on(run())
}

/// `curl --fail http://127.0.0.1:8080/api/health` of upstream's Compose file, without curl's exit
/// codes: non-zero unless the server answers 200 within two seconds.
async fn healthcheck() -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listen = env("COOLIFY_RS_LISTEN").unwrap_or_else(|| "0.0.0.0:8080".into());
    let port = listen
        .parse::<SocketAddr>()
        .context("COOLIFY_RS_LISTEN must be host:port")?
        .port();
    let answer = tokio::time::timeout(Duration::from_secs(2), async move {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
        stream
            .write_all(b"GET /api/health HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
            .await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        Ok::<_, std::io::Error>(response)
    })
    .await
    .context("the server did not answer in time")?
    .context("the server is not listening")?;
    let line = String::from_utf8_lossy(&answer);
    let line = line.lines().next().unwrap_or_default();
    anyhow::ensure!(line.contains(" 200 "), "the server answered: {line}");
    Ok(())
}

/// The folder the installer's key is in, inside the container.
fn ssh_keys_dir(cfg: &Config) -> PathBuf {
    cfg.remote.data_dir.join("ssh").join("keys")
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    if matches!(cli.cmd, Some(Cmd::Healthcheck)) {
        return healthcheck().await;
    }
    let cfg = Config::from_env()?;
    init_tracing(cfg.json_logs);

    if matches!(cli.cmd, Some(Cmd::CreateSchema)) {
        coolify_db::bootstrap::create_schema(&cfg.db.url)
            .await
            .context("creating the schema")?;
        println!("schema created");
        return Ok(());
    }
    let boot = matches!(cli.cmd, Some(Cmd::Boot));
    if boot {
        let prepared = coolify_db::bootstrap::prepare(&cfg.db.url)
            .await
            .context("preparing the database")?;
        tracing::info!(?prepared, "database prepared");
        let ran =
            coolify_db::bootstrap::apply_migrations(&cfg.db.url, coolify_db::bootstrap::MIGRATIONS)
                .await
                .context("applying schema changes")?;
        if !ran.is_empty() {
            tracing::info!(?ran, "schema changes applied");
        }
    }

    let db = Db::connect(&cfg.db)
        .await
        .context("connecting to the Coolify database")?;
    let schema = db.schema();
    let seeded = if boot && schema.read_only_reason.is_none() {
        let encrypter = cfg
            .encrypter
            .as_ref()
            .context("APP_KEY must be set to install")?;
        Some(
            coolify_jobs::first_run::production_seed(&db, encrypter, &ssh_keys_dir(&cfg))
                .await
                .context("first-run rows")?,
        )
    } else {
        None
    };
    match cli.cmd.unwrap_or(Cmd::Serve) {
        Cmd::Healthcheck | Cmd::CreateSchema => Ok(()),
        Cmd::ServerJob { job, server_uuid } => {
            let remote = Remote::new(cfg.remote.clone()).context("remote execution")?;
            let jobs = coolify_jobs::Jobs::new(db.clone(), remote, cfg.encrypter.clone());
            let out = coolify_jobs::server_ops::run_named(&jobs, &job, &server_uuid)
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
            println!("{out}");
            Ok(())
        }
        Cmd::ReleaseOrphanedDnsRecords => {
            let remote = Remote::new(cfg.remote.clone()).context("remote execution")?;
            let jobs = coolify_jobs::Jobs::new(db.clone(), remote, cfg.encrypter.clone());
            let out = coolify_jobs::kernel::release_orphaned_dns_records(&jobs)
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
            println!("{out}");
            Ok(())
        }
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
        Cmd::Serve | Cmd::Boot => {
            if let Some(reason) = &schema.read_only_reason {
                tracing::warn!(%reason, "starting in read-only mode");
            }
            tracing::info!(
                listen = %cfg.listen,
                api_version = %cfg.version,
                applied_migrations = schema.applied,
                "coolify-rs starting"
            );
            if cfg.encrypter.is_none() {
                tracing::warn!(
                    "APP_KEY not set: private keys and two-factor logins are unavailable"
                );
            }
            match &cfg.ui_dir {
                Some(dir) => tracing::info!(dir = %dir.display(), "serving web UI"),
                None => {
                    tracing::warn!("web UI not found (build frontend/ or set COOLIFY_RS_UI_DIR)")
                }
            }
            if !cfg.secure_cookies && !cfg.listen.ip().is_loopback() {
                tracing::warn!(
                    "session cookies are not marked Secure; set COOLIFY_RS_SECURE_COOKIES=true behind HTTPS"
                );
            }
            let remote = match Remote::new(cfg.remote.clone()) {
                Ok(r) => {
                    tracing::info!(data_dir = %cfg.remote.data_dir.display(), host_keys = ?cfg.remote.host_keys, "remote execution enabled");
                    match coolify_db::servers::live_uuids(&db).await {
                        Ok(live) => {
                            let dropped = r.retain_host_keys(&live.into_iter().collect());
                            if dropped > 0 {
                                tracing::info!(dropped, "removed host key pins of deleted servers");
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "could not list servers to prune host key pins")
                        }
                    }
                    Some(r)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "remote execution disabled (set COOLIFY_RS_DATA_DIR to a writable directory)");
                    None
                }
            };
            let state = AppState::with_options(
                db,
                cfg.version,
                cfg.trusted_proxies,
                Options {
                    encrypter: cfg.encrypter,
                    ui_dir: cfg.ui_dir,
                    secure_cookies: cfg.secure_cookies,
                    session_idle: cfg.session_idle,
                    remote,
                },
            );
            if seeded.is_some()
                && let Err(e) = coolify_api::ui_auth::seed_root_user(&state).await
            {
                tracing::error!(error = ?e, "could not create the root user");
            }
            if let Some(jobs) = &state.jobs {
                // Container statuses of databases, every minute (upstream's ServerManagerJob).
                jobs.spawn_status_poller();
                // Several dev instances on one host must not fail each other's deployments.
                if env("COOLIFY_RS_SKIP_STARTUP_CLEANUP").is_none() {
                    jobs.fail_interrupted_deployments().await;
                    jobs.fail_interrupted_scheduled_tasks().await;
                }
                // `Kernel::schedule()`. Off in dev: the reference shares the database.
                let scheduler_setting = env("COOLIFY_RS_SCHEDULER");
                if coolify_jobs::scheduler::enabled_by_setting(scheduler_setting.as_deref()) {
                    coolify_jobs::scheduler::Scheduler::with_default_sources(jobs.clone()).spawn();
                    tracing::info!("scheduler started");
                } else {
                    tracing::info!("scheduler is off (COOLIFY_RS_SCHEDULER)");
                }
            }
            let listener = tokio::net::TcpListener::bind(cfg.listen).await?;
            if let (Some(seeded), Some(jobs)) = (seeded, &state.jobs) {
                let lookup = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = coolify_api::ui_settings::get_public_ips(&lookup).await {
                        tracing::warn!(error = ?e, "could not save the public IPs");
                    }
                });
                coolify_jobs::first_run::spawn_after_start(jobs, seeded);
                coolify_jobs::updates::spawn_schedule(jobs, state.version.to_string());
            }
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

/// 30 seconds, as `TimeoutLayer` gave every request. A server transfer connects to the server over
/// SSH and imports a whole bundle, so it gets `ServerTransferMigrator::IMPORT_TIMEOUT_SECONDS`.
async fn request_timeout(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let path = request.uri().path();
    let transfer = path == "/api/v1/servers/import"
        || (path.starts_with("/api/v1/servers/") && path.ends_with("/migrate"))
        || path.contains("/transfer/");
    let limit = Duration::from_secs(if transfer { 300 } else { 30 });
    match tokio::time::timeout(limit, next.run(request)).await {
        Ok(response) => response,
        Err(_) => StatusCode::REQUEST_TIMEOUT.into_response(),
    }
}
