//! `ApplicationsController`: `logs_by_uuid`, `move_by_uuid`, `migrate_by_uuid`, `clone_by_uuid`,
//! `rollback_images`, `rollback_by_uuid`, `destinations`, `add_destination`, `remove_destination`.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use base64::Engine;
use coolify_db::application_ops::{self as ops, CloneApplication, DestinationRow};
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::databases::STANDALONE_DOCKER_MORPH;
use coolify_db::servers::{self, ColumnValue};
use coolify_jobs::applications::domains;
use coolify_jobs::applications::ops::LogsResult;
use coolify_jobs::applications::queue::{QueueOptions, Queued, validate_git_ref};
use coolify_jobs::storages::{StorageOwner, is_outside_resource_directory};
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, lenient_json, query_param, validated_json};
use crate::response::{ApiError, ApiResult, PhpJson, created, ok};
use crate::routes::application_fields as fields;
use crate::routes::application_previews::{parse_pull_request_id, store_preview_domains};
use crate::routes::applications::{deployable, field, php_trim, random_str};
use crate::routes::databases::input_bool;
use crate::routes::servers::effective_role_is_build;
use crate::routes::storages::{can_deploy, file_storage};
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Rule, validate};
use coolify_domain::grants::AccessLevel;

pub(crate) const NOT_FOUND: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Application not found.");
const DESTINATION_NOT_FOUND: ApiError =
    ApiError::Message(StatusCode::NOT_FOUND, "Destination not found.");

fn unprocessable(message: &'static str) -> ApiResult {
    Err(ApiError::Message(StatusCode::UNPROCESSABLE_ENTITY, message))
}

/// `$destination` of the request: a standalone or Swarm destination of the team, on a server
/// that can host resources.
async fn hosting_destination(
    state: &AppState,
    team_id: i64,
    uuid: &str,
) -> Result<(DestinationRow, &'static str, servers::ServerRow), ApiError> {
    let (destination, morph) =
        match ops::standalone_docker_in_team(&state.db, team_id, uuid).await? {
            Some(d) => (d, STANDALONE_DOCKER_MORPH),
            None => (
                ops::swarm_docker_in_team(&state.db, team_id, uuid)
                    .await?
                    .ok_or(DESTINATION_NOT_FOUND)?,
                ops::SWARM_DOCKER_MORPH,
            ),
        };
    let server = hosting_server(state, destination.server_id).await?;
    Ok((destination, morph, server))
}

/// `$destination->server?->canHostResources()`.
async fn hosting_server(state: &AppState, server_id: i64) -> Result<servers::ServerRow, ApiError> {
    let server = servers::find_by_id(&state.db, server_id)
        .await?
        .ok_or(DESTINATION_NOT_FOUND)?;
    if effective_role_is_build(&server.settings.clone().unwrap_or_default()) {
        return Err(DESTINATION_NOT_FOUND);
    }
    Ok(server)
}

/// `Server::proxyType()`.
fn proxy_type(server: &servers::ServerRow) -> Option<&str> {
    server
        .record
        .get("proxy")
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
}

// ---------------------------------------------------------------------------
// Logs

async fn logs(
    state: &AppState,
    ctx: &AuthCtx,
    uuid: &str,
    pull_request_id: Option<&str>,
    uri: &Uri,
) -> ApiResult {
    let a = apps::find_in_team(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    let pull_request_id = match pull_request_id {
        Some(raw) => {
            let id = parse_pull_request_id(raw)?;
            if ops::preview(&state.db, a.id, id).await?.is_none() {
                return Err(ApiError::Message(
                    StatusCode::NOT_FOUND,
                    "Preview not found.",
                ));
            }
            Some(id)
        }
        None => None,
    };
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let query = |key: &str| {
        query_param(uri, key)
            .map(|v| php_trim(&v))
            .filter(|v| !v.is_empty())
    };
    let service_name = query("service_name");
    let lines = query("lines");
    // parseLogTimestampFlag(): FILTER_VALIDATE_BOOLEAN.
    let timestamps = query("show_timestamps")
        .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes"));
    let result = jobs
        .application_logs(
            &a,
            pull_request_id,
            service_name.as_deref(),
            lines.as_deref(),
            timestamps,
        )
        .await
        .map_err(|e| {
            tracing::error!(application = %a.uuid, error = %e, "logs could not be read");
            ApiError::Internal
        })?;
    match result {
        LogsResult::Logs(logs) => Ok(ok(json!({"logs": logs})).into_response()),
        LogsResult::NotRunning => Ok(PhpJson(
            StatusCode::BAD_REQUEST,
            json!({"message": "Application is not running."}),
        )
        .into_response()),
        LogsResult::NoService => Ok(PhpJson(
            StatusCode::NOT_FOUND,
            json!({"message": format!(
                "No container found for service_name '{}'.",
                service_name.unwrap_or_default()
            )}),
        )
        .into_response()),
    }
}

/// `GET /applications/{uuid}/logs` (`logs_by_uuid`).
pub async fn logs_by_uuid(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    uri: Uri,
) -> ApiResult {
    logs(&state, &ctx, &uuid, None, &uri).await
}

/// `GET /applications/{uuid}/previews/{pull_request_id}/logs` (`logs_by_uuid`).
pub async fn preview_logs(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, pull_request_id)): Path<(String, String)>,
    uri: Uri,
) -> ApiResult {
    logs(&state, &ctx, &uuid, Some(&pull_request_id), &uri).await
}

// ---------------------------------------------------------------------------
// Move and migrate

/// `POST /applications/{uuid}/move` (`move_by_uuid`, `moveResourceToEnvironment()`).
pub async fn move_to_environment(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.can_app(AccessLevel::Write, &a) {
        return Err(ApiError::Unauthorized);
    }
    let input = lenient_json(&headers, &uri, &body);
    validate(
        &input,
        &[field(
            "environment_uuid",
            vec![Rule::Required, Rule::String],
        )],
        &[],
        &["environment_uuid"],
    )
    .or_else(|e| only_rule_errors(e, &input, &["environment_uuid"]))?;
    extras_as_strings(&input, &["environment_uuid"])?;
    let (environment_id, environment_uuid, project_uuid) = ops::environment_in_team(
        &state.db,
        ctx.team_id,
        input.str("environment_uuid").unwrap_or_default(),
    )
    .await?
    .ok_or(ApiError::Message(
        StatusCode::NOT_FOUND,
        "Target environment not found or not owned by your team.",
    ))?;
    if a.int("environment_id") == Some(environment_id) {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            "Application is already in this environment.",
        ));
    }
    apps::update_if_changed(
        &state.db,
        a.id,
        &[("environment_id", ColumnValue::Int(environment_id))],
        state.now(),
    )
    .await?;
    tracing::info!(target: "audit", event = "api.application.moved", team_id = ctx.team_id, resource_uuid = %a.uuid, to_environment_uuid = %environment_uuid, "application moved");
    Ok(ok(json!({
        "message": "Application moved successfully.",
        "uuid": a.uuid,
        "project_uuid": project_uuid,
        "environment_uuid": environment_uuid,
    }))
    .into_response())
}

/// The helpers of `bootstrap/helpers/api.php` run the rules first and answer without looking at
/// extra fields; [`validate`] reports both, so the extra fields are taken out of a failure.
fn only_rule_errors(e: ApiError, input: &Input, allowed: &[&str]) -> Result<(), ApiError> {
    let ApiError::Validation(mut errors) = e else {
        return Err(e);
    };
    for key in input.keys() {
        if !allowed.contains(&key.as_str()) {
            errors.remove(key);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Validation(errors))
    }
}

/// `collect($extraFields)->mapWithKeys(fn ($field) => [$field => 'This field is not allowed.'])`:
/// a string per field, not a list.
fn extras_as_strings(input: &Input, allowed: &[&str]) -> Result<(), ApiError> {
    let mut errors = Map::new();
    for key in input.keys() {
        if !allowed.contains(&key.as_str()) {
            errors.insert(key.clone(), json!("This field is not allowed."));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Validation(errors))
    }
}

/// `POST /applications/{uuid}/migrate` (`migrate_by_uuid`). `migrateResourceToDestination()` starts
/// with `if (! isDev()) abort(404)`: outside a Coolify development instance the route answers 404
/// for everybody who passes the checks, so `MigrateResourceToDestination` is not reachable.
pub async fn migrate(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.can_app(AccessLevel::Write, &a) {
        return Err(ApiError::Unauthorized);
    }
    Err(ApiError::Message(StatusCode::NOT_FOUND, ""))
}

// ---------------------------------------------------------------------------
// Clone

/// `str($name)->limit(20)`.
fn limit_name(name: &str) -> String {
    if name.chars().count() <= 20 {
        return name.to_owned();
    }
    let cut: String = name.chars().take(20).collect();
    format!("{}...", cut.trim_end())
}

/// The fields of the `saving` hook that are dirty on a replicated model: `null` becomes an empty
/// string, as `str(null)->trim()` is what gets saved.
const TRIMMED_ON_SAVE: &[&str] = &[
    "install_command",
    "build_command",
    "start_command",
    "base_directory",
    "publish_directory",
    "git_repository",
    "git_branch",
    "git_commit_sha",
];

/// `POST /applications/{uuid}/clone` (`clone_by_uuid`, `clone_application()`).
pub async fn clone(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let allowed = ["destination_uuid", "name", "clone_volumes"];
    validate(
        &input,
        &[
            field("destination_uuid", vec![Rule::Required, Rule::String]),
            field("name", vec![Rule::String, Rule::Max(255), Rule::Nullable]),
            field("clone_volumes", vec![Rule::Boolean]),
        ],
        CUSTOM_API_MESSAGES,
        &allowed,
    )?;
    let source = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.can_app(AccessLevel::Write, &source) {
        return Err(ApiError::Unauthorized);
    }
    // `$this->authorize('create', [Application::class, $application->environment])`.
    if !ctx.can_create_in(source.int("environment_id")) {
        return Err(ApiError::Unauthorized);
    }
    let clone_volumes = input_bool(&input, "clone_volumes");
    if clone_volumes && !can_deploy(&ctx) {
        return Err(ApiError::Message(
            StatusCode::FORBIDDEN,
            "Missing required permissions: deploy. Cloning volume data stops and starts the source resource, so it needs a token with the deploy permission.",
        ));
    }
    let (destination, morph, server) = hosting_destination(
        &state,
        ctx.team_id,
        input.str("destination_uuid").unwrap_or_default(),
    )
    .await?;
    // The copy redeploys the source, which the job may not be able to do yet.
    if clone_volumes && ops::has_persistent_storage(&state.db, source.id).await? {
        deployable(&state, &source).await?;
        state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let now = state.now();
    let new_uuid = coolify_domain::new_public_id();
    check_preview_volume_names(&state, &source, &new_uuid).await?;
    let name = input.str("name").filter(|n| !n.is_empty()).map_or_else(
        || {
            format!(
                "clone-of-{}-{new_uuid}",
                limit_name(source.str("name").unwrap_or_default())
            )
        },
        str::to_owned,
    );
    let proxied = proxy_type(&server) != Some("NONE");
    let readonly_labels = source.setting_bool("is_container_label_readonly_enabled");
    let mut url = source.str("fqdn").map(str::to_owned);
    if proxied && readonly_labels {
        let instance = coolify_db::instance::deployment_settings(&state.db).await?;
        let public = instance.public_ipv4.or(instance.public_ipv6);
        let settings = server.settings.clone().unwrap_or_default();
        let wildcard = settings.get("wildcard_domain").and_then(Value::as_str);
        let ip = server
            .record
            .get("ip")
            .and_then(Value::as_str)
            .unwrap_or_default();
        url = Some(domains::generate_url(wildcard, ip, public.as_deref(), &new_uuid).await);
    }
    // The `saving` hook, in which every attribute of a replicated model is dirty.
    let url = url.filter(|u| !u.is_empty());
    let is_compose = source.str("build_pack") == Some("dockercompose");
    let mut columns: Vec<(&'static str, ColumnValue)> = vec![
        ("uuid", ColumnValue::Text(Some(new_uuid.clone()))),
        ("name", ColumnValue::Text(Some(name))),
        // `status` has a mutator that adds the health.
        ("status", ColumnValue::Text(Some("exited:unhealthy".into()))),
        ("destination_id", ColumnValue::Int(destination.id)),
        ("destination_type", ColumnValue::Text(Some(morph.into()))),
        ("last_online_at", ColumnValue::Time(now)),
        (
            "compose_parsing_version",
            ColumnValue::Text(Some("5".into())),
        ),
    ];
    let fqdn = if !is_compose || url.is_some() {
        let (f, o) =
            domains::normalize_port_overrides(url.as_deref(), &fields::stored_overrides(&source));
        columns.push((
            "domain_port_overrides",
            ColumnValue::Text(o.map(|o| domains::overrides_json(f.as_deref(), &o))),
        ));
        f
    } else {
        None
    };
    columns.push(("fqdn", ColumnValue::Text(fqdn.clone())));
    if let Some(values) = ops::json_column(&source.raw, "noindex_domains")
        .and_then(|v| v.as_array().cloned())
        .filter(|v| !v.is_empty())
    {
        columns.push((
            "noindex_domains",
            ColumnValue::Text(Some(fields::noindex_json(
                &values,
                fqdn.as_deref(),
                source.str("docker_compose_domains"),
            ))),
        ));
    }
    for col in TRIMMED_ON_SAVE {
        let trimmed = php_trim(source.str(col).unwrap_or_default());
        // The mutators of both directories see a `Stringable`, which is always truthy.
        let value = if matches!(*col, "base_directory" | "publish_directory") {
            format!("/{}", trimmed.trim_start_matches('/'))
        } else {
            trimmed
        };
        columns.push((col, ColumnValue::Text(Some(value))));
    }
    if source.str("custom_nginx_configuration") == Some("") {
        columns.push(("custom_nginx_configuration", ColumnValue::Text(None)));
    }
    // `creating`: `??=` on the encrypted webhook secrets.
    for col in [
        "manual_webhook_secret_github",
        "manual_webhook_secret_gitlab",
        "manual_webhook_secret_bitbucket",
        "manual_webhook_secret_gitea",
    ] {
        if source.str(col).is_none() {
            columns.push((
                col,
                ColumnValue::Text(Some(enc.encrypt_serialized(&random_str(40)))),
            ));
        }
    }
    let nixpacks_default = (source.str("build_pack") == Some("nixpacks")).then(|| {
        (
            enc.encrypt_serialized("22"),
            enc.encrypt_serialized("22"),
            state.version.clone(),
        )
    });
    let plan = CloneApplication {
        source_id: source.id,
        source_uuid: source.uuid.clone(),
        new_uuid: new_uuid.clone(),
        server_team_id: server.team_id,
        columns,
        nixpacks_default,
    };
    let (new_id, previews, volumes) = ops::clone_application(&state.db, &plan, now)
        .await
        .map_err(fields::save_error)?;
    let mut cloned = apps::find_by_id(&state.db, new_id)
        .await?
        .ok_or(ApiError::Internal)?;
    // `ApplicationRow::destination` is only loaded for a standalone destination.
    if cloned.destination.is_none() {
        let mut d = coolify_db::records::Record::new();
        d.insert("network".into(), json!(destination.network));
        d.insert("server_id".into(), json!(destination.server_id));
        cloned.destination = Some(d);
    }
    if proxied && readonly_labels {
        let labels = fields::generated_labels(&cloned, &server, enc)?;
        let labels = base64::engine::general_purpose::STANDARD.encode(labels);
        apps::update_if_changed(
            &state.db,
            new_id,
            &[("custom_labels", ColumnValue::Text(Some(labels)))],
            now,
        )
        .await?;
    }
    for preview_id in previews {
        let preview = ops::preview_by_id(&state.db, preview_id)
            .await?
            .ok_or(ApiError::Internal)?;
        store_preview_domains(&state, &cloned, &preview).await?;
    }
    file_storages_saved(&state, &cloned).await?;
    if clone_volumes
        && !volumes.is_empty()
        && let Some(jobs) = state.jobs.as_ref()
    {
        jobs.spawn_clone_volumes(source.clone(), destination.server_id, volumes)
            .await;
    }
    tracing::info!(target: "audit", event = "api.application.cloned", team_id = ctx.team_id, source_uuid = %source.uuid, application_uuid = %new_uuid, destination_uuid = %destination.uuid, clone_volumes, "application cloned");
    Ok(created(json!({"uuid": new_uuid, "message": "Application cloned."})).into_response())
}

/// Upstream's previews are copied first, and their parse stores `{new uuid}_{volume}-pr-{id}`. The
/// copy of the source's row with that name then violates the unique index: a Compose application
/// with a named volume and a preview cannot be cloned, the answer is a 500. Upstream has no
/// transaction and leaves a half-made clone behind; here nothing is written.
async fn check_preview_volume_names(
    state: &AppState,
    source: &ApplicationRow,
    new_uuid: &str,
) -> Result<(), ApiError> {
    if source.str("build_pack") != Some("dockercompose") {
        return Ok(());
    }
    let raw = source.str("docker_compose_raw").unwrap_or_default();
    let copied: Vec<String> = apps::persistent_volumes(&state.db, source.id)
        .await?
        .into_iter()
        .map(|(name, _, _)| ops::cloned_volume_name(&name, false, &source.uuid, new_uuid))
        .collect();
    for preview in ops::previews(&state.db, source.id).await? {
        let id = preview
            .get("pull_request_id")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let collides = coolify_jobs::applications::ops::compose_preview_volumes(raw, new_uuid, id)
            .iter()
            .any(|(name, _)| copied.contains(name));
        if collides {
            tracing::error!(application = %source.uuid, "clone: a preview volume name comes out twice");
            return Err(ApiError::Internal);
        }
    }
    Ok(())
}

/// `LocalFileVolume::created`: the same check as a new storage, for the copies of the source's.
async fn file_storages_saved(state: &AppState, cloned: &ApplicationRow) -> Result<(), ApiError> {
    let Some(jobs) = state.jobs.as_ref() else {
        return Ok(());
    };
    let owner = StorageOwner::for_application(&state.db, cloned).await?;
    for row in ops::file_storage_rows(&state.db, cloned.id).await? {
        let host_file = row
            .get("is_host_file")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let fs_path = row
            .get("fs_path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !host_file && !is_outside_resource_directory(fs_path, &owner.base_directories) {
            jobs.spawn_storage_save(owner.clone(), file_storage(state, &row)?);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Rollback

/// `GET /applications/{uuid}/rollback-images` (`rollback_images`).
pub async fn rollback_images(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let images = jobs
        .rollback_images(&a)
        .await
        .unwrap_or_else(|_| json!({"current": null, "images": []}));
    Ok(ok(images).into_response())
}

/// `POST /applications/{uuid}/rollback` (`rollback_by_uuid`). The row is what upstream queues:
/// `rollback = true`, `commit` = the requested tag, `force_rebuild = false`, `is_api = true`. An
/// application the job cannot deploy answers 501, like start and restart, instead of queueing a
/// row nothing can run.
pub async fn rollback(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    validate(
        &input,
        &[field("commit", vec![Rule::Required, Rule::String])],
        CUSTOM_API_MESSAGES,
        &["commit"],
    )?;
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.can_app(AccessLevel::Deploy, &a) {
        return Err(ApiError::Unauthorized);
    }
    let commit = validate_git_ref(input.str("commit").unwrap_or_default(), "rollback commit")
        .map_err(|m| {
            let mut errors = Map::new();
            errors.insert("commit".into(), json!([m]));
            ApiError::Validation(errors)
        })?;
    deployable(&state, &a).await?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let deployment_uuid = coolify_domain::new_public_id();
    let result = jobs
        .queue_application_deployment(
            &a,
            &deployment_uuid,
            QueueOptions {
                commit: Some(commit),
                rollback: true,
                is_api: true,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "rollback could not be queued");
            ApiError::Internal
        })?;
    match result {
        Queued::QueueFull => Ok(PhpJson(
            StatusCode::BAD_REQUEST,
            json!({"message": result.message()}),
        )
        .into_response()),
        Queued::Skipped { .. } => Ok(ok(json!({"message": result.message()})).into_response()),
        Queued::Queued { .. } => Ok(ok(json!({
            "message": "Rollback deployment queued.",
            "deployment_uuid": deployment_uuid,
        }))
        .into_response()),
    }
}

// ---------------------------------------------------------------------------
// Destinations

fn present_destination(d: &DestinationRow, is_primary: bool) -> Value {
    json!({
        "uuid": d.uuid,
        "name": d.name,
        "network": d.network,
        "server_uuid": d.server_uuid,
        "server_id": d.server_id,
        "is_primary": is_primary,
    })
}

/// `GET /applications/{uuid}/destinations` (`destinations`).
pub async fn destinations(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    let mut list = Vec::new();
    if let (Some(morph), Some(id)) = (a.str("destination_type"), a.int("destination_id"))
        && let Some(primary) = ops::destination_by_morph(&state.db, morph, id).await?
    {
        list.push(present_destination(&primary, true));
    }
    for d in ops::additional_networks(&state.db, a.id).await? {
        list.push(present_destination(&d, false));
    }
    Ok(ok(Value::Array(list)).into_response())
}

/// `Application::additionalServersUnavailableReason($server)`.
async fn additional_servers_unavailable(
    state: &AppState,
    a: &ApplicationRow,
    server: &servers::ServerRow,
) -> Result<Option<String>, ApiError> {
    if a.str("build_pack") == Some("dockercompose") {
        return Ok(Some(
            "Docker Compose applications cannot use multiple servers.".into(),
        ));
    }
    if ops::has_persistent_storage(&state.db, a.id).await? {
        return Ok(Some("Applications with persistent storage cannot use multiple servers because volumes are not shared between servers.".into()));
    }
    // proxyMismatchReason()
    let Some(primary_id) = a.server_id() else {
        return Ok(None);
    };
    let primary = servers::find_by_id(&state.db, primary_id).await?;
    let primary_proxy = primary.as_ref().and_then(proxy_type);
    let server_proxy = proxy_type(server);
    let routing = |p: Option<&str>| matches!(p, Some("TRAEFIK" | "CADDY"));
    if !routing(primary_proxy) || !routing(server_proxy) || primary_proxy == server_proxy {
        return Ok(None);
    }
    let name = server
        .record
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Ok(Some(format!(
        "The primary server uses {} and {name} uses {}. All servers of an application must use the same proxy, because they share the same proxy labels.",
        primary_proxy.unwrap_or_default(),
        server_proxy.unwrap_or_default()
    )))
}

/// `POST /applications/{uuid}/destinations` (`add_destination`).
pub async fn add_destination(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    validate(
        &input,
        &[field(
            "destination_uuid",
            vec![Rule::Required, Rule::String],
        )],
        CUSTOM_API_MESSAGES,
        &["destination_uuid"],
    )?;
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.can_app(AccessLevel::Write, &a) {
        return Err(ApiError::Unauthorized);
    }
    let destination = ops::standalone_docker_in_team(
        &state.db,
        ctx.team_id,
        input.str("destination_uuid").unwrap_or_default(),
    )
    .await?
    .ok_or(DESTINATION_NOT_FOUND)?;
    let server = hosting_server(&state, destination.server_id).await?;
    if a.int("destination_id") == Some(destination.id)
        && a.str("destination_type") == Some(STANDALONE_DOCKER_MORPH)
    {
        return unprocessable("Destination is already the primary destination.");
    }
    let attached = ops::additional_networks(&state.db, a.id).await?;
    if attached.iter().any(|d| d.id == destination.id) {
        return unprocessable("Destination is already attached.");
    }
    if a.server_id() == Some(destination.server_id) {
        return unprocessable(
            "Cannot attach a destination on the same server as the primary destination.",
        );
    }
    if attached
        .iter()
        .any(|d| d.server_id == destination.server_id)
    {
        return unprocessable("A destination on this server is already attached.");
    }
    if let Some(reason) = additional_servers_unavailable(&state, &a, &server).await? {
        return Ok(
            PhpJson(StatusCode::UNPROCESSABLE_ENTITY, json!({"message": reason})).into_response(),
        );
    }
    ops::attach_destination(&state.db, a.id, destination.id, destination.server_id).await?;
    tracing::info!(target: "audit", event = "api.application.destination_added", team_id = ctx.team_id, application_uuid = %a.uuid, destination_uuid = %destination.uuid, "destination attached");
    Ok(
        created(json!({"message": "Destination attached.", "uuid": destination.uuid}))
            .into_response(),
    )
}

/// `DELETE /applications/{uuid}/destinations/{destination_uuid}` (`remove_destination`).
pub async fn remove_destination(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, destination_uuid)): Path<(String, String)>,
) -> ApiResult {
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.can_app(AccessLevel::Write, &a) {
        return Err(ApiError::Unauthorized);
    }
    let destination = ops::standalone_docker_in_team(&state.db, ctx.team_id, &destination_uuid)
        .await?
        .ok_or(DESTINATION_NOT_FOUND)?;
    if a.int("destination_id") == Some(destination.id)
        && a.str("destination_type") == Some(STANDALONE_DOCKER_MORPH)
    {
        return unprocessable("Cannot remove the primary destination.");
    }
    let attached = ops::additional_networks(&state.db, a.id).await?;
    let attached = attached
        .iter()
        .find(|d| d.id == destination.id)
        .ok_or(DESTINATION_NOT_FOUND)?;
    ops::detach_destination(&state.db, a.id, destination.id, attached.server_id).await?;
    tracing::info!(target: "audit", event = "api.application.destination_removed", team_id = ctx.team_id, application_uuid = %a.uuid, destination_uuid = %destination.uuid, "destination detached");
    Ok(ok(json!({"message": "Destination detached."})).into_response())
}
