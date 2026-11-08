//! Session endpoints behind the application pages of the web UI: what the
//! Livewire components (`Project\New\DockerImage`, `Application\Heading`,
//! `Application\General`, `Deployment\Index`, `Deployment\Show`,
//! `DeploymentNavbar`, `InternalAccess`, `Shared\Danger`) read and do on the server.
//! compat/notes/application-ui.md describes upstream's pages.

use axum::Extension;
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::applications::{self as apps, ApplicationRow, NewApplication};
use coolify_db::servers::{self, ColumnValue};
use coolify_domain::naming::strip_tags;
use coolify_jobs::applications::queue::{QueueOptions, Queued};
use coolify_jobs::applications::{domains, snapshot};
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, query_param};
use crate::response::{ApiError, ApiResult, PhpJson, created, ok};
use crate::state::AppState;
use crate::validation::{Field, Messages, Rule, patterns, validate};
use coolify_domain::grants::AccessLevel;

pub mod compose;
mod general;
pub mod private_git;
pub mod public_git;
pub mod source;

const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Application not found.");

static IMAGE_NAME: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\A(?:(?:(?:[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?|\[[0-9a-f:.]+\])(?::[0-9]+)?/)?[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*(?:/[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*)*)(?:@sha256)?\z")
        .unwrap_or_else(|_| unreachable!())
});
/// `ValidationPatterns::DOCKER_IMAGE_TAG_PATTERN` (`$VAR` references allowed; `max:128` checks the length).
use crate::routes::applications::DOCKER_IMAGE_TAG as IMAGE_TAG;
static SHA256: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)^[a-f0-9]{64}$").unwrap_or_else(|_| unreachable!())
});
static PORTS_EXPOSES: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^(\d+)(,\d+)*$").unwrap_or_else(|_| unreachable!())
});
static PORT_MAPPINGS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    let ip = r"(?:(?:\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}|\[[\da-fA-F:]+\]):)?";
    let host = r"(?:\d+(?:-\d+)?(?:/(?:tcp|udp|sctp))?)?";
    let container = r"\d+(?:-\d+)?(?:/(?:tcp|udp|sctp))?";
    let pair = format!("{ip}{host}:{ip}{container}");
    regex::Regex::new(&format!(r"^{pair}(?:,{pair})*\n?\z")).unwrap_or_else(|_| unreachable!())
});

fn body(bytes: &[u8]) -> Map<String, Value> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

fn s(m: &Map<String, Value>, k: &str) -> String {
    m.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// `DockerImageParser::parse` for the form: `(base name with @sha256 suffix on a digest, tag or digest, is digest)`.
fn parse_reference(reference: &str) -> (String, String, bool) {
    if let Some(c) = regex::Regex::new(r"(?i)^(.+)@sha256:([a-f0-9]{64})$")
        .ok()
        .and_then(|r| r.captures(reference))
    {
        return (c[1].to_owned(), c[2].to_owned(), true);
    }
    // The tag starts at the first colon after the last slash (a `${VAR:-default}` tag has more).
    let slash = reference.rfind('/').map_or(0, |i| i + 1);
    match reference[slash..].find(':') {
        Some(i) => {
            let tag = reference[slash + i + 1..].to_owned();
            let hash = tag.len() == 64 && tag.chars().all(|c| c.is_ascii_hexdigit());
            (reference[..slash + i].to_owned(), tag, hash)
        }
        None => (reference.to_owned(), "latest".into(), false),
    }
}

/// `Project\New\DockerImage::submit` (with `updatedImageName`'s split of a pasted reference).
pub async fn create_docker_image(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    bytes: axum::body::Bytes,
) -> ApiResult {
    let m = body(&bytes);
    let (mut name, mut tag, mut sha) = (
        s(&m, "image_name"),
        s(&m, "image_tag"),
        s(&m, "image_sha256"),
    );
    // updatedImageName(): split `name:tag` / `name@sha256:…` when the other fields are empty.
    if !name.is_empty()
        && tag.is_empty()
        && sha.is_empty()
        && (name.contains(':') || name.contains('@'))
    {
        let (base, t, digest) = parse_reference(&name);
        if base != name {
            if digest {
                sha = t;
            } else if t != "latest" || name.contains(':') {
                tag = t;
            }
            name = base;
        }
    }
    let mut input = Map::new();
    input.insert("image_name".into(), Value::String(name.clone()));
    if !tag.is_empty() {
        input.insert("image_tag".into(), Value::String(tag.clone()));
    }
    if !sha.is_empty() {
        input.insert("image_sha256".into(), Value::String(sha.clone()));
    }
    let i = Input(input);
    let keys: Vec<&str> = vec!["image_name", "image_tag", "image_sha256"];
    validate(
        &i,
        &[
            Field {
                name: "image_name",
                rules: vec![
                    Rule::Required,
                    Rule::String,
                    Rule::Max(255),
                    Rule::Regex(&IMAGE_NAME),
                ],
            },
            Field {
                name: "image_tag",
                rules: vec![
                    Rule::Nullable,
                    Rule::String,
                    Rule::Max(128),
                    Rule::Regex(&IMAGE_TAG),
                ],
            },
            Field {
                name: "image_sha256",
                rules: vec![Rule::Nullable, Rule::String, Rule::Regex(&SHA256)],
            },
        ],
        &[],
        &keys,
    )?;
    if !tag.is_empty() && !sha.is_empty() {
        let mut e = Map::new();
        e.insert(
            "image_tag".into(),
            json!(["Provide either a tag or SHA256 digest, not both."]),
        );
        e.insert(
            "image_sha256".into(),
            json!(["Provide either a tag or SHA256 digest, not both."]),
        );
        return Err(ApiError::Validation(e));
    }
    let sha = sha.trim();
    let sha = if sha.to_lowercase().starts_with("sha256:") {
        &sha[7..]
    } else {
        sha
    };
    let reference = if !sha.is_empty() {
        format!("{name}@sha256:{sha}")
    } else if !tag.is_empty() {
        format!("{name}:{tag}")
    } else {
        format!("{name}:latest")
    };
    let (base, t, digest) = parse_reference(&reference);
    let (image_name, image_tag) = if digest {
        (
            if base.ends_with("@sha256") {
                base
            } else {
                format!("{base}@sha256")
            },
            format!("sha256-{t}"),
        )
    } else {
        (base, t)
    };
    let project = coolify_db::projects::find(&state.db, ctx.team_id, &s(&m, "project_uuid"))
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Project not found.",
        ))?;
    let env =
        coolify_db::projects::find_environment(&state.db, project.id, &s(&m, "environment_uuid"))
            .await?
            .ok_or(ApiError::Message(
                StatusCode::NOT_FOUND,
                "Environment not found.",
            ))?;
    // `$this->authorize('create', [Application::class, $environment])`.
    if !ctx.can_create_in(Some(env.id)) {
        return Err(ApiError::Unauthorized);
    }
    let (destination_id, server_id) =
        coolify_db::databases::team_destination(&state.db, ctx.team_id, &s(&m, "destination_uuid"))
            .await?
            .ok_or(ApiError::Message(
                StatusCode::NOT_FOUND,
                "Destination not found.",
            ))?;
    let server = servers::find_by_id(&state.db, server_id)
        .await?
        .ok_or(ApiError::Internal)?;
    let uuid = coolify_domain::new_public_id();
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let mut columns: Vec<(&'static str, ColumnValue)> = vec![
        (
            "name",
            ColumnValue::Text(Some(format!(
                "docker-image-{}",
                coolify_domain::new_public_id()
            ))),
        ),
        ("repository_project_id", ColumnValue::Int(0)),
        (
            "git_repository",
            ColumnValue::Text(Some("coollabsio/coolify".into())),
        ),
        ("git_branch", ColumnValue::Text(Some("main".into()))),
        ("build_pack", ColumnValue::Text(Some("dockerimage".into()))),
        ("ports_exposes", ColumnValue::Text(Some("80".into()))),
        (
            "docker_registry_image_name",
            ColumnValue::Text(Some(image_name)),
        ),
        (
            "docker_registry_image_tag",
            ColumnValue::Text(Some(image_tag)),
        ),
        ("health_check_enabled", ColumnValue::Bool(false)),
    ];
    for k in [
        "manual_webhook_secret_github",
        "manual_webhook_secret_gitlab",
        "manual_webhook_secret_bitbucket",
        "manual_webhook_secret_gitea",
    ] {
        columns.push((
            k,
            ColumnValue::Text(Some(enc.encrypt_string(&random_str(40)))),
        ));
    }
    let now = state.now();
    let id = apps::create(
        &state.db,
        NewApplication {
            destination_type: coolify_db::databases::destination_morph(
                &state.db,
                &s(&m, "destination_uuid"),
            )
            .await?,
            uuid: uuid.clone(),
            environment_id: env.id,
            destination_id,
            columns,
            settings: Vec::new(),
        },
        now,
    )
    .await?;
    let settings = server.settings.clone().unwrap_or_default();
    let ip = server
        .record
        .get("ip")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let wildcard = settings
        .get("wildcard_domain")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let instance = coolify_db::instance::deployment_settings(&state.db).await?;
    let public = instance.public_ipv4.or(instance.public_ipv6);
    let url = domains::generate_url(wildcard.as_deref(), &ip, public.as_deref(), &uuid).await;
    let (fqdn, overrides) = domains::normalize_port_overrides(Some(&url), &Default::default());
    let mut values = vec![
        (
            "name",
            ColumnValue::Text(Some(format!("docker-image-{uuid}"))),
        ),
        ("fqdn", ColumnValue::Text(fqdn.clone())),
    ];
    if let Some(o) = overrides {
        values.push((
            "domain_port_overrides",
            ColumnValue::Text(Some(domains::overrides_json(fqdn.as_deref(), &o))),
        ));
    }
    apps::update_if_changed(&state.db, id, &values, now).await?;
    tracing::info!(target: "audit", event = "application.created", team_id = ctx.team_id, application_uuid = %uuid, "application created");
    Ok(created(json!({"uuid": uuid})).into_response())
}

pub(crate) fn random_str(n: usize) -> String {
    use rand::Rng as _;
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    (0..n)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}

async fn find(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<ApplicationRow, ApiError> {
    apps::find_in_team(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(NOT_FOUND)
}

/// What the application pages poll: the running deployment, the restart hint, permissions,
/// and the server timezone the dates are shown in.
pub async fn state(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    // General::mount(): Coolify-managed labels are generated when missing.
    if a.str("custom_labels").is_none_or(str::is_empty)
        && a.setting_bool("is_container_label_readonly_enabled")
        && ctx.can_app(AccessLevel::Write, &a)
        && let Some(server) = match a.server_id() {
            Some(id) => servers::find_by_id(&state.db, id).await?,
            None => None,
        }
        && server
            .record
            .get("proxy")
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|t| t != "NONE")
    {
        let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
        let generated = crate::routes::application_fields::generated_labels(&a, &server, enc)?;
        use base64::Engine as _;
        let stored = base64::engine::general_purpose::STANDARD.encode(generated);
        apps::update_if_changed(
            &state.db,
            a.id,
            &[("custom_labels", ColumnValue::Text(Some(stored)))],
            state.now(),
        )
        .await?;
    }
    let running =
        apps::deployments_with_status(&state.db, a.id, &["in_progress", "queued"]).await?;
    let running_url = running
        .iter()
        .max_by_key(|d| d.id)
        .and_then(|d| d.str("deployment_url").map(str::to_owned));
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let fresh = apps::find_by_id(&state.db, a.id).await?.ok_or(NOT_FOUND)?;
    let saved = fresh.str("config_hash").map(str::to_owned);
    let changed = match &saved {
        Some(h) => {
            !fresh
                .str("status")
                .unwrap_or_default()
                .starts_with("exited")
                && snapshot::config_hash(&state.db, enc, &fresh)
                    .await
                    .map_err(|_| ApiError::Internal)?
                    != *h
        }
        None => false,
    };
    let server = match fresh.server_id() {
        Some(id) => servers::find_by_id(&state.db, id).await?,
        None => None,
    };
    let timezone = server
        .as_ref()
        .and_then(|s| s.settings.as_ref())
        .and_then(|s| s.get("server_timezone"))
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .unwrap_or("UTC")
        .to_owned();
    let labels_text = stored_labels_text(&fresh);
    // The build options of the General page: `Server::buildServers($teamId)->exists()` and the
    // team's fallback policy, and what the image registry fields depend on.
    let build: (bool, bool) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM servers s JOIN server_settings ss ON ss.server_id = s.id          WHERE s.team_id = $1 AND s.deleted_at IS NULL AND ss.is_reachable AND ss.is_usable AND NOT ss.is_swarm_worker          AND NOT ss.force_disabled AND (ss.server_role = 'build' OR (ss.server_role IS NULL AND ss.is_build_server))          AND NOT EXISTS (SELECT 1 FROM github_runner_configs g WHERE g.server_id = s.id AND g.is_enabled AND g.is_dedicated)),          COALESCE((SELECT is_build_server_fallback_enabled FROM teams WHERE id = $1), false)",
    )
    .bind(ctx.team_id)
    .fetch_one(state.db.pool())
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "build server lookup failed");
        ApiError::Internal
    })?;
    let server_setting = |k: &str| {
        server
            .as_ref()
            .and_then(|s| s.settings.as_ref())
            .and_then(|s| s.get(k))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let is_swarm = server_setting("is_swarm_manager")
        .as_bool()
        .unwrap_or(false)
        || server_setting("is_swarm_worker").as_bool().unwrap_or(false);
    // effectiveServerRole()->canBuild()
    let can_build = match server_setting("server_role").as_str() {
        Some(role) => role != "deployment",
        None => true,
    };
    // is_public_repository(): `source.is_public`.
    let source_is_public = match (fresh.str("source_type"), fresh.int("source_id")) {
        (Some(r"App\Models\GithubApp"), Some(id)) => {
            coolify_db::git_sources::github_app(&state.db, id)
                .await?
                .and_then(|g| g.get("is_public").and_then(Value::as_bool))
                .unwrap_or(false)
        }
        _ => false,
    };
    Ok(ok(json!({
        "source_is_public": source_is_public,
        "has_build_servers": build.0,
        "build_server_fallback": build.1,
        "server_is_swarm": is_swarm,
        "server_can_build": can_build,
        "can_update": ctx.can_app(AccessLevel::Write, &a),
        "can_deploy": ctx.can_app(AccessLevel::Deploy, &a),
        "running_deployment_url": running_url,
        "configuration_changed": changed,
        "server_timezone": timezone,
        // `syncData()`: only who can update reads these.
        "http_basic_auth_password": if ctx.can_app(AccessLevel::Write, &a) {
            state.encrypter.as_ref().and_then(|enc| basic_auth_password(&fresh, enc))
        } else {
            None
        },
        "supports_argon2id": server.as_ref().is_some_and(coolify_jobs::applications::labels::caddy_supports_argon2id),
        "proxy_type": server.as_ref().and_then(|s| s.record.get("proxy")).and_then(|p| match p {
            Value::String(t) => serde_json::from_str::<Value>(t).ok(),
            other => Some(other.clone()),
        }).and_then(|p| p.get("type").and_then(Value::as_str).map(str::to_owned)),
        // `$application->additional_servers()->exists()`.
        "has_additional_servers": !coolify_db::application_ops::additional_networks(&state.db, fresh.id).await?.is_empty(),
        // `syncData()`: commands and configuration only for who can update.
        "custom_labels": if ctx.can_app(AccessLevel::Write, &a) { labels_text } else { None },
        "server_ip": server.as_ref().and_then(|s| s.record.get("ip")).cloned().unwrap_or(Value::Null),
        "server_id": server.as_ref().map(|s| s.id),
        "is_swarm": a.str("destination_type") == Some(r"App\Models\SwarmDocker") || server.as_ref().is_some_and(|s| coolify_jobs::server::ServerInfo::from_row(s).is_swarm()),
    }))
    .into_response())
}

/// `$application->http_basic_auth_password` (an `encrypted` cast).
fn basic_auth_password(
    a: &ApplicationRow,
    enc: &coolify_compat::encrypter::Encrypter,
) -> Option<String> {
    a.str("http_basic_auth_password")
        .and_then(|c| enc.decrypt_string(c).ok())
        .filter(|p| !p.is_empty())
}

/// `$server->caddySupportsArgon2idBasicAuth()` of the application's server.
async fn caddy_supports_argon2id(state: &AppState, a: &ApplicationRow) -> Result<bool, ApiError> {
    let server = match a.server_id() {
        Some(id) => servers::find_by_id(&state.db, id).await?,
        None => None,
    };
    Ok(server
        .as_ref()
        .is_some_and(coolify_jobs::applications::labels::caddy_supports_argon2id))
}

/// The label text of `custom_labels` (`parseContainerLabels()` decodes the stored base64).
fn stored_labels_text(a: &ApplicationRow) -> Option<String> {
    match coolify_jobs::applications::compose::stored_labels(a.str("custom_labels")) {
        Some(Ok(t)) => Some(t),
        _ => None,
    }
}

/// `Heading::deploy` / `restart`: the deployment uuid to open, or the toast upstream shows.
async fn queue(
    state: &AppState,
    ctx: &AuthCtx,
    uuid: &str,
    restart: bool,
    force: bool,
) -> ApiResult {
    let a = find(state, ctx, uuid).await?;
    if !ctx.can_app(AccessLevel::Deploy, &a) {
        return Err(ApiError::Unauthorized);
    }
    if a.destination.is_none() {
        return Err(ApiError::Internal);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let deployment_uuid = coolify_domain::new_public_id();
    let result = jobs
        .queue_application_deployment(
            &a,
            &deployment_uuid,
            QueueOptions {
                force_rebuild: force,
                restart_only: restart,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "deployment could not be queued");
            ApiError::Internal
        })?;
    Ok(match result {
        Queued::QueueFull => ok(json!({"toast": "error", "title": "Deployment queue full", "message": result.message()})),
        // Heading::restart shows a skipped restart as a success toast.
        Queued::Skipped { .. } => ok(json!({"toast": if restart { "success" } else { "error" }, "title": "Deployment skipped", "message": result.message()})),
        Queued::Queued { deployment_uuid } => ok(json!({"deployment_uuid": deployment_uuid})),
    }
    .into_response())
}

pub async fn deploy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    bytes: axum::body::Bytes,
) -> ApiResult {
    let force = body(&bytes)
        .get("force")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    queue(&state, &ctx, &uuid, false, force).await
}

pub async fn restart(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    queue(&state, &ctx, &uuid, true, false).await
}

/// `Heading::stop`.
pub async fn stop(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    if !ctx.can_app(AccessLevel::Deploy, &a) {
        return Err(ApiError::Unauthorized);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    jobs.spawn_application_stop(a.clone());
    tracing::info!(target: "audit", event = "ui.application.stopped", team_id = ctx.team_id, application_uuid = %a.uuid, "stop queued");
    Ok(ok(json!({"message": "Application stopping request queued."})).into_response())
}

/// `Application::deployments()`'s source of a queue row (`loadDeploymentFilterOptions`).
fn deployment_source(d: &apps::Deployment) -> &'static str {
    if d.int("pull_request_id").unwrap_or(0) > 0 {
        "pull-request"
    } else if d.bool("is_webhook") {
        "webhook"
    } else if d.bool("rollback") {
        "rollback"
    } else if d.bool("is_api") {
        "api"
    } else {
        "manual"
    }
}

/// `Deployment\Index` (`Application::deployments()`): `{count, deployments}` with search,
/// `status:` / `source:` / `server:` filters (OR within a group, AND across groups), the pull
/// request filter, order and paging, plus the filter options present in this app's rows.
pub async fn deployments(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    uri: Uri,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    let num = |k: &str, d: i64| {
        query_param(&uri, k)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(d)
    };
    let skip = num("skip", 0).max(0);
    let take = num("take", 10).clamp(1, 100);
    let pull_request = num("pull_request_id", 0);
    let search = query_param(&uri, "search")
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let oldest = query_param(&uri, "order").as_deref() == Some("oldest");
    let filters: Vec<String> = query_param(&uri, "filters")
        .map(|v| {
            v.split(',')
                .filter(|f| !f.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let group = |prefix: &str| -> Vec<String> {
        filters
            .iter()
            .filter_map(|f| f.strip_prefix(prefix).map(str::to_owned))
            .collect()
    };
    let (statuses, sources) = (group("status:"), group("source:"));
    let server_ids: Vec<i64> = group("server:")
        .iter()
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .filter_map(|v| v.parse().ok())
        .collect();
    let (all, _) = apps::deployments_for_application(&state.db, a.id, 0, i64::MAX).await?;
    let mut rows: Vec<&apps::Deployment> = all
        .iter()
        .filter(|d| {
            if pull_request > 0 && d.int("pull_request_id").unwrap_or(0) != pull_request {
                return false;
            }
            let search_ok = search.as_deref().is_none_or(|q| {
                let lower = q.to_lowercase();
                let alias = match lower.as_str() {
                    "success" => Some("finished"),
                    "in progress" => Some("in_progress"),
                    "cancelled" => Some("cancelled-by-user"),
                    _ => None,
                };
                let pr = d.int("pull_request_id").unwrap_or(0);
                [
                    "deployment_uuid",
                    "commit",
                    "commit_message",
                    "server_name",
                    "status",
                ]
                .iter()
                .any(|k| d.str(k).is_some_and(|v| v.to_lowercase().contains(&lower)))
                    || alias.is_some_and(|al| d.status == al)
                    || q.parse::<f64>()
                        .is_ok_and(|n| n as i64 > 0 && pr == n as i64)
                    || match lower.as_str() {
                        "pull request" | "pull requests" | "pr" => pr > 0,
                        "webhook" | "webhooks" => d.bool("is_webhook"),
                        "rollback" | "rollbacks" => d.bool("rollback"),
                        "api" => d.bool("is_api"),
                        "manual" => deployment_source(d) == "manual",
                        _ => false,
                    }
            });
            search_ok
                && (statuses.is_empty() || statuses.contains(&d.status))
                && (sources.is_empty() || sources.iter().any(|s| s == deployment_source(d)))
                && (server_ids.is_empty()
                    || d.int("server_id")
                        .is_some_and(|id| server_ids.contains(&id)))
        })
        .collect();
    rows.sort_by(|x, y| (x.str("created_at"), x.id).cmp(&(y.str("created_at"), y.id)));
    if !oldest {
        rows.reverse();
    }
    let count = rows.len();
    let items: Vec<Value> = rows
        .iter()
        .skip(usize::try_from(skip).unwrap_or(0))
        .take(usize::try_from(take).unwrap_or(10))
        .map(|d| {
            json!({
                "deployment_uuid": d.uuid,
                "status": d.status,
                "commit": d.str("commit"),
                "commit_message": d.str("commit_message"),
                "is_webhook": d.bool("is_webhook"),
                "is_api": d.bool("is_api"),
                "rollback": d.bool("rollback"),
                "pull_request_id": d.int("pull_request_id").unwrap_or(0),
                "server_id": d.int("server_id"),
                "server_name": d.str("server_name"),
                "created_at": d.str("created_at"),
                "finished_at": d.str("finished_at"),
            })
        })
        .collect();
    let status_options: Vec<Value> = [
        ("finished", "Success"),
        ("failed", "Failed"),
        ("in_progress", "In progress"),
        ("queued", "Queued"),
        ("cancelled-by-user", "Cancelled"),
    ]
    .iter()
    .filter(|(s, _)| all.iter().any(|d| d.status == *s))
    .map(|(s, l)| json!({"value": format!("status:{s}"), "label": l}))
    .collect();
    let source_options: Vec<Value> = [
        ("manual", "Manual"),
        ("pull-request", "Pull requests"),
        ("webhook", "Webhooks"),
        ("api", "API"),
        ("rollback", "Rollbacks"),
    ]
    .iter()
    .filter(|(s, _)| all.iter().any(|d| deployment_source(d) == *s))
    .map(|(s, l)| json!({"value": format!("source:{s}"), "label": l}))
    .collect();
    let mut servers: Vec<(i64, String)> = Vec::new();
    let mut by_name: Vec<&apps::Deployment> = all
        .iter()
        .filter(|d| d.int("server_id").is_some())
        .collect();
    by_name.sort_by(|x, y| x.str("server_name").cmp(&y.str("server_name")));
    for d in by_name {
        let id = d.int("server_id").unwrap_or_default();
        if !servers.iter().any(|(s, _)| *s == id) {
            servers.push((
                id,
                d.str("server_name")
                    .filter(|n| !n.is_empty())
                    .map_or_else(|| format!("Server #{id}"), str::to_owned),
            ));
        }
    }
    let server_options: Vec<Value> = servers
        .iter()
        .map(|(id, name)| json!({"value": format!("server:{id}"), "label": name}))
        .collect();
    let mut prs: Vec<i64> = all
        .iter()
        .filter_map(|d| d.int("pull_request_id"))
        .filter(|p| *p > 0)
        .collect();
    prs.sort_unstable();
    prs.dedup();
    Ok(ok(json!({
        "count": count,
        "deployments": items,
        "status_options": status_options,
        "source_options": source_options,
        "server_options": server_options,
        "pull_requests": prs,
    }))
    .into_response())
}

/// `decode_remote_command_output($queue)`: the visible log lines in order, with a command line
/// before the first output of each command and batch.
fn log_lines(logs: Option<&str>, debug: bool) -> Vec<Value> {
    let entries: Vec<Value> = logs
        .and_then(|l| serde_json::from_str(l).ok())
        .unwrap_or_default();
    let mut entries: Vec<&Value> = entries
        .iter()
        .filter(|e| debug || !e.get("hidden").and_then(Value::as_bool).unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| e.get("order").and_then(Value::as_i64).unwrap_or(0));
    let mut seen: Vec<(String, i64)> = Vec::new();
    let mut out = Vec::new();
    for e in entries {
        let hidden = e.get("hidden").and_then(Value::as_bool).unwrap_or(false);
        let stderr = e.get("type").and_then(Value::as_str) == Some("stderr");
        let timestamp = e.get("timestamp").cloned().unwrap_or(Value::Null);
        if let Some(cmd) = e.get("command").and_then(Value::as_str) {
            let batch = e.get("batch").and_then(Value::as_i64).unwrap_or(0);
            let key = (cmd.to_owned(), batch);
            if !seen.contains(&key) {
                seen.push(key);
                out.push(json!({"text": cmd, "command": true, "hidden": hidden, "stderr": stderr, "timestamp": timestamp}));
            }
        }
        for line in e
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .split('\n')
        {
            out.push(json!({"text": line.trim(), "command": false, "hidden": hidden, "stderr": stderr, "timestamp": timestamp}));
        }
    }
    out
}

/// `Deployment\Show` + `polling()`: the deployment, its lines, and whether to keep polling.
pub async fn deployment(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, dep)): Path<(String, String)>,
    uri: Uri,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    let d = apps::deployment_by_uuid(&state.db, &dep)
        .await?
        .filter(|d| d.application_id == a.id)
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Deployment not found.",
        ))?;
    // `downloadAllLogs()` (`?all=1`) includes the hidden lines for users who can update.
    let all = query_param(&uri, "all").as_deref() == Some("1");
    if all && !ctx.can_app(AccessLevel::Write, &a) {
        return Err(ApiError::Unauthorized);
    }
    let debug = all || (ctx.can_app(AccessLevel::Write, &a) && a.setting_bool("is_debug_enabled"));
    Ok(ok(json!({
        "deployment_uuid": d.uuid,
        "status": d.status,
        "commit": d.str("commit"),
        "server_name": d.str("server_name"),
        "created_at": d.str("created_at"),
        "finished_at": d.str("finished_at"),
        "is_keep_alive": !matches!(d.status.as_str(), "finished" | "failed"),
        "debug": debug,
        "lines": log_lines(d.str("logs"), debug),
    }))
    .into_response())
}

/// `Deployment\Show::toggleDebug`.
pub async fn toggle_debug(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    if !ctx.can_app(AccessLevel::Write, &a) {
        return Err(ApiError::Unauthorized);
    }
    let on = !a.setting_bool("is_debug_enabled");
    apps::update_settings(
        &state.db,
        a.id,
        &[("is_debug_enabled", ColumnValue::Bool(on))],
        state.now(),
    )
    .await?;
    Ok(ok(json!({"debug": on})).into_response())
}

/// `DeploymentNavbar::cancel`.
pub async fn cancel(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, dep)): Path<(String, String)>,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    if !ctx.can_app(AccessLevel::Deploy, &a) {
        return Err(ApiError::Unauthorized);
    }
    let d = apps::deployment_by_uuid(&state.db, &dep)
        .await?
        .filter(|d| d.application_id == a.id)
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Deployment not found.",
        ))?;
    let prev = apps::transition_status(&state.db, d.id, "cancelled-by-user", state.now()).await?;
    if matches!(prev.as_deref(), Some("queued" | "in_progress")) {
        let entry = apps::LogEntry {
            command: None,
            output: "Deployment cancelled by user.".into(),
            kind: "stderr",
            timestamp: chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%S%.6fZ")
                .to_string(),
            hidden: false,
            batch: 1,
        };
        apps::append_log(&state.db, d.id, &[entry], state.now()).await?;
        if let Some(jobs) = &state.jobs {
            jobs.cleanup_cancelled_deployment(&d).await;
            jobs.queue_next_deployment(a.id, d.int("server_id").unwrap_or_default())
                .await;
        }
    }
    Ok(ok(json!({"status": "cancelled-by-user"})).into_response())
}

/// `DeploymentNavbar::force_start`.
pub async fn force_start(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, dep)): Path<(String, String)>,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    if !ctx.can_app(AccessLevel::Deploy, &a) {
        return Err(ApiError::Unauthorized);
    }
    let d = apps::deployment_by_uuid(&state.db, &dep)
        .await?
        .filter(|d| d.application_id == a.id)
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Deployment not found.",
        ))?;
    let started = match &state.jobs {
        Some(jobs) => jobs.force_start_deployment(d.id).await,
        None => false,
    };
    Ok(ok(json!({"started": started})).into_response())
}

/// `InternalAccess::loadCurrentInternalHostname`: the name of the running (else first) container.
pub async fn internal_hostname(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let name = jobs.application_container_name(&a).await;
    Ok(ok(json!({"hostname": name})).into_response())
}

/// `Application\General` messages and `$validationAttributes` (snake_case keys).
const GENERAL_MESSAGES: Messages = &[
    (
        "docker_compose_custom_start_command.regex",
        "The Docker Compose start command contains invalid characters. Allowed: alphanumerics, && / || chaining, balanced quotes, globs (*, ?), !, and safe path/arg chars. Blocked: bare &, bare |, ;, $, backtick, (, ), <, >, \\\\, newlines.",
    ),
    (
        "docker_compose_custom_build_command.regex",
        "The Docker Compose build command contains invalid characters. Allowed: alphanumerics, && / || chaining, balanced quotes, globs (*, ?), !, and safe path/arg chars. Blocked: bare &, bare |, ;, $, backtick, (, ), <, >, \\\\, newlines.",
    ),
    (
        "custom_docker_run_options.regex",
        "The custom Docker run options contain invalid characters. Allowed: alphanumerics, && / || chaining, balanced quotes, globs (*, ?), !, and safe path/arg chars. Blocked: bare &, bare |, ;, $, backtick, (, ), <, >, \\\\, newlines.",
    ),
    (
        "pre_deployment_command_container.regex",
        "The pre-deployment command container name must contain only alphanumeric characters, dots, hyphens, and underscores.",
    ),
    (
        "post_deployment_command_container.regex",
        "The post-deployment command container name must contain only alphanumeric characters, dots, hyphens, and underscores.",
    ),
    (
        "docker_compose_location.regex",
        "The Docker Compose must start with a letter or underscore and may only contain letters, numbers, underscores, and dots.",
    ),
    ("name.required", "The Name field is required."),
    (
        "name.regex",
        "The name may only contain letters (including Unicode), numbers, spaces, and these characters: - _ . / @ & ( ) # , : +",
    ),
    ("name.min", "The name must be at least :min characters."),
    (
        "name.max",
        "The name may not be greater than :max characters.",
    ),
    (
        "description.regex",
        "The description may only contain letters (including Unicode), numbers, spaces, and common punctuation: - _ . , ! ? ( ) ' \" + = * / @ &",
    ),
    (
        "description.max",
        "The description may not be greater than :max characters.",
    ),
    (
        "ports_exposes.regex",
        "Ports exposes must be a comma-separated list of port numbers (e.g. 3000,3001).",
    ),
    (
        "ports_mappings.regex",
        "Port mappings must be a comma-separated list of port pairs or ranges with optional IP and protocol (e.g. 3000:3000, 8080:80/udp, 127.0.0.1:8080:80, [::1]::80).",
    ),
    ("attribute:ports_exposes", "Ports exposes"),
    ("attribute:ports_mappings", "Ports mappings"),
    (
        "attribute:docker_registry_image_name",
        "Docker registry image name",
    ),
    (
        "attribute:docker_registry_image_tag",
        "Docker registry image tag",
    ),
    (
        "attribute:custom_network_aliases",
        "Custom docker network aliases",
    ),
    (
        "install_command.regex",
        "The install command contains invalid characters. Allowed: alphanumerics, && / || chaining, balanced quotes, globs (*, ?), !, and safe path/arg chars. Blocked: bare &, bare |, ;, $, backtick, (, ), <, >, \\, newlines.",
    ),
    (
        "build_command.regex",
        "The build command contains invalid characters. Allowed: alphanumerics, && / || chaining, balanced quotes, globs (*, ?), !, and safe path/arg chars. Blocked: bare &, bare |, ;, $, backtick, (, ), <, >, \\, newlines.",
    ),
    (
        "start_command.regex",
        "The start command contains invalid characters. Allowed: alphanumerics, && / || chaining, balanced quotes, globs (*, ?), !, and safe path/arg chars. Blocked: bare &, bare |, ;, $, backtick, (, ), <, >, \\, newlines.",
    ),
    ("build_pack.required", "The Build Pack field is required."),
    (
        "static_image.required",
        "The Static Image field is required.",
    ),
    (
        "base_directory.required",
        "The Base Directory field is required.",
    ),
    (
        "base_directory.regex",
        "The base directory must be a valid path starting with / and containing only safe characters.",
    ),
    (
        "publish_directory.regex",
        "The publish directory must be a valid path starting with / and containing only safe characters.",
    ),
    (
        "dockerfile_location.regex",
        "The Dockerfile location must be a valid path starting with / and containing only alphanumeric characters, dots, hyphens, underscores, slashes, @, ~, and +.",
    ),
    (
        "dockerfile_target_build.regex",
        "The Dockerfile target build must contain only alphanumeric characters, dots, hyphens, and underscores.",
    ),
    ("attribute:install_command", "Install command"),
    ("attribute:build_command", "Build command"),
    ("attribute:start_command", "Start command"),
    ("attribute:build_pack", "Build pack"),
    ("attribute:static_image", "Static image"),
    ("attribute:base_directory", "Base directory"),
    ("attribute:publish_directory", "Publish directory"),
    ("attribute:dockerfile_location", "Dockerfile location"),
    (
        "attribute:dockerfile_target_build",
        "Dockerfile target build",
    ),
];

/// Laravel's summary of a validation failure (`handleError` shows it as an error toast).
pub(crate) fn summary(errors: &Map<String, Value>) -> String {
    let all: Vec<String> = errors
        .values()
        .flat_map(|v| v.as_array().cloned().unwrap_or_default())
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    match all.len() {
        0 => "The given data was invalid.".into(),
        1 => all[0].clone(),
        2 => format!("{} (and 1 more error)", all[0]),
        n => format!("{} (and {} more errors)", all[0], n - 1),
    }
}

/// `General::submit` (and `updatedBuildPack()` / `generateNginxConfiguration()`, which save the
/// whole form too): name, description, the build fields of the git build packs, image,
/// networking and container options; labels follow the Coolify-managed rules.
pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    bytes: axum::body::Bytes,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    if !ctx.can_app(AccessLevel::Write, &a) {
        return Err(ApiError::Message(
            StatusCode::FORBIDDEN,
            "You need at least admin or owner permissions to update this application.",
        ));
    }
    let mut m = body(&bytes);
    for k in ["ports_exposes", "ports_mappings"] {
        if let Some(Value::String(p)) = m.get(k) {
            let v = p.replace(' ', "").trim().to_owned();
            m.insert(
                k.into(),
                if v.is_empty() && k == "ports_exposes" {
                    Value::Null
                } else {
                    Value::String(v)
                },
            );
        }
    }
    // `required` fails on an empty string.
    for k in ["build_pack", "static_image", "base_directory"] {
        if m.get(k)
            .and_then(Value::as_str)
            .is_some_and(|v| v.trim().is_empty())
        {
            m.insert(k.into(), Value::Null);
        }
    }
    let input = Input(m.clone());
    let keys: Vec<&str> = m.keys().map(String::as_str).collect();
    let mut fields = vec![
        patterns::name(true),
        patterns::description(),
        Field {
            name: "ports_exposes",
            rules: vec![Rule::Nullable, Rule::String, Rule::Regex(&PORTS_EXPOSES)],
        },
        Field {
            name: "ports_mappings",
            rules: vec![Rule::Nullable, Rule::String, Rule::Regex(&PORT_MAPPINGS)],
        },
        Field {
            name: "docker_registry_image_name",
            rules: vec![
                Rule::Nullable,
                Rule::String,
                Rule::Max(255),
                Rule::Regex(&IMAGE_NAME),
            ],
        },
        Field {
            name: "docker_registry_image_tag",
            rules: vec![
                Rule::Nullable,
                Rule::String,
                Rule::Max(128),
                Rule::Regex(&IMAGE_TAG),
            ],
        },
    ];
    // `changedCommandRules()`: a command saved before commands were validated stays valid until it
    // changes (the deployment passes it as one escaped argument, and the field can be hidden).
    fields.extend(general::rules().into_iter().map(|mut f| {
        if matches!(
            f.name,
            "install_command" | "build_command" | "start_command"
        ) {
            let submitted = match m.get(f.name) {
                Some(Value::String(v)) => Some(v.clone()),
                Some(Value::Null) => Some(String::new()),
                Some(Value::Number(n)) => Some(n.to_string()),
                _ => None,
            };
            if submitted.is_some_and(|v| v == a.str(f.name).unwrap_or_default()) {
                f.rules = vec![Rule::Nullable];
            }
        }
        f
    }));
    fields.extend(
        [
            "custom_nginx_configuration",
            "watch_paths",
            "dockerfile",
            "generate_nginx",
            "pre_deployment_command",
            "post_deployment_command",
            "http_basic_auth_username",
        ]
        .map(|name| Field {
            name,
            rules: vec![Rule::Nullable],
        }),
    );
    fields.push(Field {
        name: "http_basic_auth_password",
        rules: vec![Rule::Nullable, Rule::String, Rule::Max(72)],
    });
    let result = validate(&input, &fields, GENERAL_MESSAGES, &keys);
    if let Err(ApiError::Validation(errors)) = result {
        return Ok(PhpJson(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": summary(&errors), "errors": errors}),
        )
        .into_response());
    }
    let text = |k: &str| match m.get(k) {
        Some(Value::String(s)) => Some(s.clone()).filter(|s| !s.is_empty()),
        _ => None,
    };
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    for k in ["name", "description"] {
        if m.contains_key(k) {
            values.push((
                k,
                ColumnValue::Text(Some(strip_tags(m.get(k).and_then(Value::as_str)))),
            ));
        }
    }
    for k in [
        "docker_registry_image_name",
        "docker_registry_image_tag",
        "ports_exposes",
        "ports_mappings",
    ] {
        if m.contains_key(k) {
            values.push((k, ColumnValue::Text(text(k))));
        }
    }
    if m.contains_key("custom_network_aliases") {
        let aliases: Vec<String> = text("custom_network_aliases")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().replace(' ', "-"))
            .filter(|s| !s.is_empty())
            .fold(Vec::new(), |mut out, s| {
                if !out.contains(&s) {
                    out.push(s);
                }
                out
            });
        let stored =
            (!aliases.is_empty()).then(|| coolify_compat::php_json::encode(&json!(aliases)));
        values.push(("custom_network_aliases", ColumnValue::Text(stored)));
    }
    // `syncData(toModel: true)`: what the deployment reads as it is.
    for k in [
        "pre_deployment_command",
        "pre_deployment_command_container",
        "post_deployment_command",
        "post_deployment_command_container",
        "docker_compose_custom_start_command",
        "docker_compose_custom_build_command",
        "http_basic_auth_username",
    ] {
        if m.contains_key(k) {
            values.push((k, ColumnValue::Text(text(k))));
        }
    }
    if m.contains_key("docker_compose_location") {
        values.push((
            "docker_compose_location",
            ColumnValue::Text(text("docker_compose_location")),
        ));
    }
    if m.contains_key("custom_docker_run_options") {
        values.push((
            "custom_docker_run_options",
            ColumnValue::Text(text("custom_docker_run_options").map(|v| v.trim().to_owned())),
        ));
    }
    if m.contains_key("http_basic_auth_password") {
        let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
        let plain = text("http_basic_auth_password");
        if plain != basic_auth_password(&a, enc) {
            values.push((
                "http_basic_auth_password",
                ColumnValue::Text(plain.map(|p| enc.encrypt_string(&p))),
            ));
        }
    }
    let ports_changed = m.contains_key("ports_exposes")
        && text("ports_exposes").as_deref() != a.str("ports_exposes");
    let previous_dns_hostnames =
        crate::ui_shared::dns::previous_hostnames(&state, apps::MORPH, a.id).await;
    let fx = general::apply(&a, &m, &mut values);
    // `isDirty('dockerfile')`: the health check and the port come from the new Dockerfile.
    if m.contains_key("dockerfile") && text("dockerfile").as_deref() != a.str("dockerfile") {
        let dockerfile = text("dockerfile");
        values.retain(|(k, _)| *k != "dockerfile");
        values.push(("dockerfile", ColumnValue::Text(dockerfile.clone())));
        values.extend(
            crate::routes::application_fields::healthcheck_from_dockerfile(
                &a,
                dockerfile.as_deref(),
            ),
        );
        if text("ports_exposes").is_none()
            && let Some(port) = dockerfile
                .as_deref()
                .and_then(crate::routes::application_fields::port_from_dockerfile)
        {
            values.retain(|(k, _)| *k != "ports_exposes");
            values.push(("ports_exposes", ColumnValue::Text(Some(port.to_string()))));
        }
    }
    // syncData(toModel: true) assigns `settings->is_static`, whose mutator saves `ports_exposes = 80`
    // on its own copy of the application: it stays unless this save changes the ports.
    if a.setting_bool("is_static") && !ports_changed {
        values.retain(|(k, _)| *k != "ports_exposes");
        values.push(("ports_exposes", ColumnValue::Text(Some("80".into()))));
    }
    let now = state.now();
    apps::update_settings(&state.db, a.id, &fx.settings, now).await?;
    apps::update_if_changed(&state.db, a.id, &values, now).await?;
    crate::ui_shared::dns::queue_release_of_removed_hostnames(
        &state,
        apps::MORPH,
        a.id,
        previous_dns_hostnames,
        a.team_id,
    );
    // Validates a changed Compose path before it counts, and puts the old values back on failure.
    let form_pack = m
        .get("build_pack")
        .and_then(Value::as_str)
        .or(a.str("build_pack"));
    let changed =
        |key: &str| m.contains_key(key) && m.get(key).and_then(Value::as_str) != a.str(key);
    if form_pack == Some("dockercompose")
        && a.str("build_pack") == Some("dockercompose")
        && (changed("docker_compose_location") || changed("base_directory"))
    {
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        if let Err(e) = coolify_jobs::compose_parser::load_compose_file(jobs, a.id, false).await {
            apps::update_if_changed(
                &state.db,
                a.id,
                &[
                    (
                        "docker_compose_location",
                        ColumnValue::Text(a.str("docker_compose_location").map(str::to_owned)),
                    ),
                    (
                        "base_directory",
                        ColumnValue::Text(a.str("base_directory").map(str::to_owned)),
                    ),
                ],
                now,
            )
            .await?;
            return Ok(PhpJson(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"message": e.message()}),
            )
            .into_response());
        }
    }
    if fx.remove_service_variables {
        sqlx::query(
            "DELETE FROM environment_variables WHERE resourceable_type = $1 AND resourceable_id = $2 AND (key LIKE 'SERVICE_FQDN_%' OR key LIKE 'SERVICE_URL_%')",
        )
        .bind(apps::MORPH)
        .bind(a.id)
        .execute(state.db.pool())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "service variables cleanup failed");
            ApiError::Internal
        })?;
    }
    // resetDefaultLabels(): Coolify-managed labels follow the ports (and a new build pack).
    if ports_changed || fx.reset_labels {
        regenerate_labels(&state, a.id, false).await?;
    }
    let pack = m
        .get("build_pack")
        .and_then(Value::as_str)
        .or(a.str("build_pack"))
        .unwrap_or_default();
    // dockerimage: the image is required (checked after the save).
    if pack == "dockerimage"
        && text("docker_registry_image_name").is_none()
        && m.contains_key("docker_registry_image_name")
    {
        return Ok(PhpJson(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": "The Docker registry image name field is required."}),
        )
        .into_response());
    }
    // `composeDomainsForStorage()`: a changed value resets the labels, and with them the Compose file.
    let mut error = None;
    if pack == "dockercompose" && a.str("build_pack") == Some("dockercompose") {
        use coolify_jobs::compose_parser::domains::rekey_compose_domains_to_service_names;
        use coolify_jobs::compose_parser::php;
        let stored = a.str("docker_compose_domains");
        let names: Vec<String> = a
            .str("docker_compose_raw")
            .and_then(|raw| {
                coolify_jobs::compose_parser::validate::parse_docker_compose_yaml(raw).ok()
            })
            .and_then(|parsed| match php::get(&parsed, "services") {
                Some(coolify_compat::yaml::PhpValue::Array(items)) => {
                    Some(items.iter().map(|(k, _)| k.as_string()).collect())
                }
                _ => None,
            })
            .unwrap_or_default();
        let rekeyed = rekey_compose_domains_to_service_names(&php::json_array(stored), &names);
        let encoded = php::json_encode(&coolify_compat::yaml::PhpValue::Array(rekeyed));
        if stored != Some(encoded.as_str()) {
            apps::update_if_changed(
                &state.db,
                a.id,
                &[("docker_compose_domains", ColumnValue::Text(Some(encoded)))],
                state.now(),
            )
            .await?;
            regenerate_labels(&state, a.id, false).await?;
            if a.setting_bool("is_container_label_readonly_enabled")
                && let Some(jobs) = state.jobs.as_ref()
                && let Err(e) =
                    coolify_jobs::compose_parser::load_compose_file(jobs, a.id, false).await
            {
                error = Some(e.message().to_owned());
            }
        }
    }
    Ok(
        ok(json!({"message": "Application settings updated!", "toasts": fx.toasts, "error": error}))
            .into_response(),
    )
}

/// `resetDefaultLabels($manualReset)`: regenerates Coolify-managed labels (always on a manual reset).
pub(crate) async fn regenerate_labels(
    state: &AppState,
    id: i64,
    manual: bool,
) -> Result<(), ApiError> {
    let a = apps::find_by_id(&state.db, id).await?.ok_or(NOT_FOUND)?;
    if !manual && !a.setting_bool("is_container_label_readonly_enabled") {
        return Ok(());
    }
    let server = match a.server_id() {
        Some(sid) => servers::find_by_id(&state.db, sid).await?,
        None => None,
    };
    let Some(server) = server else {
        return Ok(());
    };
    use base64::Engine as _;
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let generated = crate::routes::application_fields::generated_labels(&a, &server, enc)?;
    let stored = base64::engine::general_purpose::STANDARD.encode(generated);
    apps::update_if_changed(
        &state.db,
        id,
        &[("custom_labels", ColumnValue::Text(Some(stored)))],
        state.now(),
    )
    .await?;
    Ok(())
}

/// `General::instantSave` for the label, site type (`setSiteType()`) and builder settings,
/// and `resetDefaultLabels(true)`.
pub async fn settings(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    bytes: axum::body::Bytes,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    if !ctx.can_app(AccessLevel::Write, &a) {
        return Err(ApiError::Unauthorized);
    }
    let m = body(&bytes);
    if m.get("reset_labels").and_then(Value::as_bool) == Some(true) {
        regenerate_labels(&state, a.id, true).await?;
        return Ok(ok(json!({"message": null})).into_response());
    }
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    for k in [
        "is_container_label_readonly_enabled",
        "is_container_label_escape_enabled",
        "is_static",
        "is_spa",
        "is_build_server_enabled",
        "is_preserve_repository_enabled",
    ] {
        if let Some(b) = m.get(k).and_then(Value::as_bool) {
            values.push((k, ColumnValue::Bool(b)));
        }
    }
    let now = state.now();
    apps::update_settings(&state.db, a.id, &values, now).await?;
    // instantSave(): turning it off detaches the file storages from the repository.
    if m.get("is_preserve_repository_enabled")
        .and_then(Value::as_bool)
        == Some(false)
        && a.setting_bool("is_preserve_repository_enabled")
    {
        sqlx::query(
            "UPDATE local_file_volumes SET is_based_on_git = false, updated_at = $1 \
             WHERE resource_type = $2 AND resource_id = $3 AND is_based_on_git",
        )
        .bind(now)
        .bind(apps::MORPH)
        .bind(a.id)
        .execute(state.db.pool())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "file storages could not be updated");
            ApiError::Internal
        })?;
    }
    // The HTTP Basic Authentication fields of `instantSave()`.
    let mut auth: Vec<(&'static str, ColumnValue)> = Vec::new();
    if let Some(b) = m.get("is_http_basic_auth_enabled").and_then(Value::as_bool) {
        auth.push(("is_http_basic_auth_enabled", ColumnValue::Bool(b)));
    }
    if let Some(algorithm) = m
        .get("http_basic_auth_hash_algorithm")
        .and_then(Value::as_str)
    {
        if algorithm == "argon2id" && !caddy_supports_argon2id(&state, &a).await? {
            return Ok(PhpJson(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"message": "Argon2id needs a server that runs the Caddy proxy, version 2.11 or newer."}),
            )
            .into_response());
        }
        auth.push((
            "http_basic_auth_hash_algorithm",
            ColumnValue::Text(Some(algorithm.to_owned())),
        ));
    }
    for (k, min, max) in [
        ("http_basic_auth_bcrypt_cost", 4, 14),
        ("http_basic_auth_argon2id_memory_cost", 8192, 262_144),
        ("http_basic_auth_argon2id_time_cost", 1, 12),
    ] {
        if let Some(n) = m.get(k).and_then(Value::as_i64) {
            if !(min..=max).contains(&n) {
                return Ok(PhpJson(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    json!({"message": format!("The {} field must be between {min} and {max}.", k.replace('_', " "))}),
                )
                .into_response());
            }
            auth.push((k, ColumnValue::Int(n)));
        }
    }
    if !auth.is_empty() {
        apps::update_if_changed(&state.db, a.id, &auth, now).await?;
    }
    // The `isStatic` mutator of ApplicationSetting: a static site listens on port 80.
    if m.get("is_static").and_then(Value::as_bool) == Some(true) {
        apps::update_if_changed(
            &state.db,
            a.id,
            &[("ports_exposes", ColumnValue::Text(Some("80".into())))],
            now,
        )
        .await?;
    }
    // instantSave(): a changed `is_spa` regenerates the Nginx configuration.
    let mut toasts: Vec<&str> = Vec::new();
    if let Some(spa) = m.get("is_spa").and_then(Value::as_bool)
        && spa != a.setting_bool("is_spa")
    {
        use base64::Engine as _;
        let conf = general::default_nginx_configuration(spa);
        apps::update_if_changed(
            &state.db,
            a.id,
            &[(
                "custom_nginx_configuration",
                ColumnValue::Text(Some(base64::engine::general_purpose::STANDARD.encode(conf))),
            )],
            now,
        )
        .await?;
        toasts.push("Nginx configuration generated.");
    }
    if let Some(labels) = m.get("custom_labels").and_then(Value::as_str) {
        use base64::Engine as _;
        let stored = base64::engine::general_purpose::STANDARD.encode(labels);
        apps::update_if_changed(
            &state.db,
            a.id,
            &[("custom_labels", ColumnValue::Text(Some(stored)))],
            now,
        )
        .await?;
    }
    regenerate_labels(&state, a.id, false).await?;
    // resetDefaultLabels(): a Compose application reads its file again; handleError() shows a failure.
    let mut error = None;
    if a.str("build_pack") == Some("dockercompose")
        && a.setting_bool("is_container_label_readonly_enabled")
        && let Some(jobs) = state.jobs.as_ref()
        && let Err(e) = coolify_jobs::compose_parser::load_compose_file(jobs, a.id, false).await
    {
        error = Some(e.message().to_owned());
    }
    Ok(ok(json!({"message": "Settings saved.", "toasts": toasts, "error": error})).into_response())
}

/// `Shared\Danger::delete` for an application, with the page's confirmation steps.
pub async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    bytes: axum::body::Bytes,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    let m = body(&bytes);
    if !crate::ui::password_confirmed(
        &state,
        ctx.user_id,
        m.get("password").and_then(Value::as_str),
    )
    .await?
    {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The provided password is incorrect.",
        ));
    }
    if !ctx.can_app(AccessLevel::Delete, &a) {
        return Err(ApiError::Unauthorized);
    }
    // `DeleteResourceJob` removes the rows also when the destination is gone.
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let flag = |k: &str| m.get(k).and_then(Value::as_bool).unwrap_or(true);
    apps::soft_delete(&state.db, a.id, state.now()).await?;
    jobs.spawn_application_delete(
        a.clone(),
        flag("delete_volumes"),
        flag("delete_configurations"),
        flag("delete_connected_networks"),
    );
    tracing::info!(target: "audit", event = "application.deleted", team_id = ctx.team_id, application_uuid = %a.uuid, "deletion queued");
    Ok(ok(json!({"message": "Application deletion request queued."})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines() {
        let logs = r#"[{"command":"docker ps","output":"a\nb","type":"stdout","timestamp":"t","hidden":true,"batch":2,"order":2},{"command":null,"output":"Starting","type":"stdout","timestamp":"t","hidden":false,"batch":1,"order":1}]"#;
        let visible = log_lines(Some(logs), false);
        assert_eq!(visible.len(), 1);
        let all = log_lines(Some(logs), true);
        assert_eq!(all.len(), 4);
        assert_eq!(all[1]["command"], true);
        assert_eq!(
            parse_reference("nginx:1.2"),
            ("nginx".into(), "1.2".into(), false)
        );
    }
}
