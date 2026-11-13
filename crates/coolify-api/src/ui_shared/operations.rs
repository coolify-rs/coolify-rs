//! `Project\Shared\ResourceOperations` and `Project\CloneMe`: cloning and moving a resource.
//!
//! `cloneVolumeData` copies the persistent volumes through `CloneVolumeDataWithSourceStopped`
//! (`coolify_jobs::clone_volume_data`). `migrateTo` only renders in a Coolify development instance (`isDev()`), and the reference
//! is not one: the section and its action are not offered.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::database_parts as parts;
use coolify_db::databases::{self as dbs, DatabaseRow};
use coolify_db::destinations;
use coolify_db::projects;
use coolify_db::records::Record;
use coolify_db::resource_ops as ops;
use coolify_db::servers::{self, ColumnValue};
use coolify_db::services::{self as svc, ServiceRow, SubKind};
use coolify_domain::new_public_id;
use coolify_jobs::applications::domains;
use coolify_jobs::server::ServerInfo;
use coolify_remote::text::php_trim;
use serde_json::{Map, Value, json};

use super::{Class, Res, find};
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::servers::effective_role_is_build;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ui/resources/{kind}/{uuid}/operations", get(show))
        .route(
            "/ui/resources/{kind}/{uuid}/operations/clone",
            post(clone_to),
        )
        .route("/ui/resources/{kind}/{uuid}/operations/move", post(move_to))
        .route("/ui/resources/{kind}/{uuid}/delete", post(delete_service))
        .route(
            "/ui/projects/{project}/environments/{env}/clone",
            get(clone_me_page).post(clone_me),
        )
}

const fn bad(message: &'static str) -> ApiError {
    ApiError::Message(StatusCode::UNPROCESSABLE_ENTITY, message)
}

/// `$server->isBuildServer()`.
fn is_build(row: &servers::ServerRow) -> bool {
    effective_role_is_build(&row.settings.clone().unwrap_or_default())
}

/// `$server->destinations()`.
async fn destinations_json(state: &AppState, server_id: i64) -> Result<Vec<Value>, ApiError> {
    Ok(destinations::list_for_server(&state.db, server_id)
        .await?
        .iter()
        .map(|d| {
            let t = d.transform();
            json!({"id": t.get("id"), "uuid": t.get("uuid"), "name": t.get("name"), "server_id": server_id})
        })
        .collect())
}

/// `mount()` and the `x-data` of `resource-operations.blade.php`.
async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    let mut server_list = Vec::new();
    let mut build_servers = Vec::new();
    let mut rows = servers::list(&state.db, res.team_id).await?;
    rows.sort_by_key(|r| r.id);
    for row in &rows {
        let name = row.record.get("name").cloned().unwrap_or(Value::Null);
        if is_build(row) {
            build_servers.push(json!({"id": row.id, "name": name}));
            continue;
        }
        // `->filter(fn ($server) => Gate::allows('placeResource', $server))`.
        if !crate::place_resource::allows(&state, &ctx, row).await? {
            continue;
        }
        server_list.push(json!({
            "id": row.id,
            "name": name,
            "ip": row.record.get("ip"),
            "is_functional": ServerInfo::from_row(row).is_functional(),
            "destinations": destinations_json(&state, row.id).await?,
        }));
    }
    let mut project_list = Vec::new();
    for p in ops::team_projects(&state.db, res.team_id).await? {
        let envs = ops::project_environments(&state.db, p.0).await?;
        project_list.push(json!({
            "id": p.0,
            "name": p.1,
            "environments": envs.iter().map(|e| json!({"id": e.0, "name": e.1, "project_id": p.0})).collect::<Vec<_>>(),
        }));
    }
    let destination_uuid = res
        .destination_uuid(&state)
        .await?
        .ok_or(ApiError::NotPorted)?;
    Ok(ok(json!({
        "can_update": ctx.is_team_admin(),
        "current_project_id": res.project_id,
        "current_environment_id": res.environment_id,
        "current_server_id": res.server_id,
        "current_destination_uuid": destination_uuid,
        "servers": server_list,
        "build_servers": build_servers,
        "projects": project_list,
    }))
    .into_response())
}

impl Res {
    /// `$resource->destination->uuid`.
    async fn destination_uuid(&self, state: &AppState) -> Result<Option<String>, ApiError> {
        Ok(ops::destination_uuid(&state.db, self.table, self.id).await?)
    }
}

/// `findResourceTeamDestination()`: `(id, morph, server)`.
async fn team_destination(
    state: &AppState,
    team_id: i64,
    uuid: &str,
) -> Result<Option<(i64, &'static str, servers::ServerRow)>, ApiError> {
    if uuid.trim().is_empty() {
        return Ok(None);
    }
    let Some((id, morph, server_id)) =
        svc::team_destination_by_uuid(&state.db, team_id, uuid).await?
    else {
        return Ok(None);
    };
    Ok(servers::find_by_id(&state.db, server_id)
        .await?
        .filter(|s| !is_build(s))
        .map(|s| (id, morph, s)))
}

/// `route('project.{segment}.configuration', ...)`; the page adds `#resource-operations`.
fn resource_route(project: &str, env: &str, segment: &str, uuid: &str) -> String {
    format!("/project/{project}/environment/{env}/{segment}/{uuid}")
}

/// `cloneTo($destination_uuid, $environment_id)`.
async fn clone_to(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let environment_id = body
        .get("environment_id")
        .and_then(Value::as_i64)
        .or(res.environment_id)
        .unwrap_or_default();
    let Some((env_uuid, project_uuid)) =
        ops::team_environment(&state.db, res.team_id, environment_id).await?
    else {
        return Err(bad("Environment not found."));
    };
    let wanted = body
        .get("destination_uuid")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(destination) = team_destination(&state, res.team_id, wanted).await? else {
        return Err(bad("Destination not found."));
    };
    // `findResourceTeamDestination()`: `$this->authorize('placeResource', $destination->server)`.
    crate::place_resource::authorize(&state, &ctx, &destination.2).await?;
    // `authorize(CloneVolumeDataWithSourceStopped::sourceAbility(...))`: an admin ability, checked above.
    let clone_volume_data = body
        .get("clone_volume_data")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    tracing::info!(target: "audit", event = "ui.resource.clone_started", team_id = res.team_id, resource_uuid = %res.uuid, destination_uuid = %wanted, environment_id, "clone started");
    let (segment, new_uuid) = match res.class {
        Class::Application => {
            let source = apps::find_in_team(&state.db, res.team_id, &res.uuid)
                .await?
                .ok_or(super::NOT_FOUND)?;
            (
                "application",
                clone_application(
                    &state,
                    &source,
                    &destination,
                    environment_id,
                    clone_volume_data,
                )
                .await?,
            )
        }
        Class::Database => {
            let source = dbs::find_in_team(&state.db, res.team_id, &res.uuid)
                .await?
                .ok_or(super::NOT_FOUND)?;
            let new_uuid = new_public_id();
            let name = coolify_jobs::notifications::php::clone_resource_name(&res.name, &new_uuid);
            clone_database(
                &state,
                &source,
                &new_uuid,
                &name,
                &destination,
                environment_id,
                clone_volume_data,
            )
            .await?;
            ("database", new_uuid)
        }
        Class::Service => {
            let source = svc::find_in_team(&state.db, res.team_id, &res.uuid)
                .await?
                .ok_or(super::NOT_FOUND)?;
            let new_uuid = new_public_id();
            let name = coolify_jobs::notifications::php::clone_resource_name(&res.name, &new_uuid);
            clone_service(
                &state,
                &source,
                &new_uuid,
                &name,
                &destination,
                environment_id,
                clone_volume_data,
            )
            .await?;
            ("service", new_uuid)
        }
        _ => return Err(super::NOT_FOUND),
    };
    Ok(ok(json!({
        "redirect": resource_route(&project_uuid, &env_uuid, segment, &new_uuid),
    }))
    .into_response())
}

/// `moveTo($environment_id)`.
async fn move_to(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let environment_id = body
        .get("environment_id")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let Some((env_uuid, project_uuid)) =
        ops::team_environment(&state.db, res.team_id, environment_id).await?
    else {
        return Err(bad("Environment not found."));
    };
    let segment = match res.class {
        Class::Application => "application",
        Class::Database => "database",
        Class::Service => "service",
        _ => return Err(super::NOT_FOUND),
    };
    ops::set_environment(&state.db, res.table, res.id, environment_id, state.now()).await?;
    Ok(ok(json!({
        "redirect": resource_route(&project_uuid, &env_uuid, segment, &res.uuid),
    }))
    .into_response())
}

/// `Shared\\Danger::delete` for a service: soft delete, then `DeleteResourceJob`.
async fn delete_service(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    if kind != "service" {
        return Err(super::NOT_FOUND);
    }
    let s = svc::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(super::NOT_FOUND)?;
    if !crate::ui::password_confirmed(
        &state,
        ctx.user_id,
        body.get("password").and_then(Value::as_str),
    )
    .await?
    {
        return Err(bad("The provided password is incorrect."));
    }
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let flag = |k: &str| body.get(k).and_then(Value::as_bool).unwrap_or(true);
    svc::soft_delete(&state.db, s.id, state.now()).await?;
    jobs.spawn_service_delete(
        s.clone(),
        flag("delete_volumes"),
        flag("delete_connected_networks"),
        flag("delete_configurations"),
    );
    tracing::info!(target: "audit", event = "service.deleted", team_id = ctx.team_id, service_uuid = %s.uuid, "deletion queued");
    Ok(ok(json!({"message": "Service deletion request queued."})).into_response())
}

// ---------------------------------------------------------------------------
// Clones

type Destination = (i64, &'static str, servers::ServerRow);

/// The new name of a database volume.
fn cloned_database_volume(original: &str, source_uuid: &str, new_uuid: &str) -> String {
    for prefix in [
        "postgres-data-",
        "mysql-data-",
        "redis-data-",
        "clickhouse-data-",
        "mariadb-data-",
        "mongodb-data-",
        "keydb-data-",
        "dragonfly-data-",
        "sqlite-data-",
    ] {
        if original.starts_with(prefix) {
            return format!("{prefix}{new_uuid}");
        }
    }
    if original.starts_with(source_uuid) {
        return original.replace(source_uuid, new_uuid);
    }
    format!("{new_uuid}-{original}")
}

/// The database branch of `cloneTo()` and of `cloneInto()`.
async fn clone_database(
    state: &AppState,
    d: &DatabaseRow,
    new_uuid: &str,
    name: &str,
    destination: &Destination,
    environment_id: i64,
    clone_volume_data: bool,
) -> Result<(), ApiError> {
    let (destination_id, morph_destination, server) = destination;
    let now = state.now();
    let morph = d.kind.morph();
    let new_id = parts::replicate_database(
        &state.db,
        d.kind.table(),
        d.id,
        new_uuid,
        &coolify_domain::naming::strip_tags(Some(name)),
        *destination_id,
        morph_destination,
        now,
    )
    .await?;
    ops::set_environment_only(&state.db, d.kind.table(), new_id, environment_id).await?;
    parts::copy_tags(&state.db, morph, d.id, new_id).await?;
    let mut pending: Vec<(String, String)> = Vec::new();
    for volume in parts::persistent_storages(&state.db, morph, d.id).await? {
        let original = parts::str_of(&volume, "name").unwrap_or_default();
        let new_name = cloned_database_volume(original, &d.uuid, new_uuid);
        parts::replicate_persistent_volume(
            &state.db,
            parts::id_of(&volume),
            &new_name,
            new_id,
            now,
        )
        .await?;
        if clone_volume_data {
            pending.push((original.to_owned(), new_name));
        }
    }
    if !pending.is_empty() {
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        jobs.spawn_volume_clones(d.clone(), server.id, pending)
            .await;
    }
    let fresh = dbs::find_in_team(&state.db, d.team_id, new_uuid)
        .await?
        .ok_or(ApiError::Internal)?;
    for file in parts::file_storages(&state.db, morph, d.id).await? {
        let copy =
            parts::replicate_file_volume(&state.db, parts::id_of(&file), new_id, now).await?;
        if let Some(jobs) = state.jobs.as_ref()
            && !parts::bool_of(&copy, "is_host_file")
        {
            let dir = format!("/data/coolify/databases/{new_uuid}");
            let fs = parts::str_of(&copy, "fs_path").unwrap_or_default();
            if !coolify_jobs::databases::paths::is_outside_resource_directory(fs, &dir) {
                jobs.spawn_file_storage_save(copy.clone(), fresh.clone());
            }
        }
    }
    parts::copy_backups_to(&state.db, morph, d.id, new_id, d.team_id, now).await?;
    parts::replicate_variables(&state.db, morph, d.id, new_id, now).await?;
    ops::clone_secret_link(&state.db, morph, d.id, new_id, now).await?;
    Ok(())
}

/// `clone_service($source, $destination, [...], $cloneVolumeData)`.
async fn clone_service(
    state: &AppState,
    s: &ServiceRow,
    new_uuid: &str,
    name: &str,
    destination: &Destination,
    environment_id: i64,
    clone_volume_data: bool,
) -> Result<(), ApiError> {
    let (destination_id, morph_destination, server) = destination;
    let now = state.now();
    let new_id = svc::replicate(
        &state.db,
        s.id,
        new_uuid,
        &coolify_domain::naming::strip_tags(Some(name)),
        *destination_id,
        morph_destination,
        server.id,
        now,
    )
    .await?;
    ops::set_environment_only(&state.db, "services", new_id, environment_id).await?;
    svc::replicate_children(&state.db, s.id, new_id, s.team_id, now).await?;
    ops::clone_secret_link(&state.db, svc::MORPH, s.id, new_id, now).await?;
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    jobs.parse_service(new_id).await.map_err(|e| {
        tracing::warn!(service = %s.uuid, error = %e, "cloned service could not be parsed");
        ApiError::Internal
    })?;
    let mut pending = Vec::new();
    for kind in [SubKind::Application, SubKind::Database] {
        let sources = svc::subs(&state.db, kind, s.id).await?;
        for sub in svc::subs(&state.db, kind, new_id).await? {
            svc::sub_set_status(&state.db, kind, sub.id, "exited", state.now()).await?;
            if !clone_volume_data {
                continue;
            }
            if let Some(source) = sources.iter().find(|x| x.name == sub.name) {
                pending.extend(
                    crate::routes::services::volume_pairs(state, kind, source.id, sub.id).await?,
                );
            }
        }
    }
    // The scheduled backups of each database go by name.
    let sources = svc::subs(&state.db, SubKind::Database, s.id).await?;
    for sub in svc::subs(&state.db, SubKind::Database, new_id).await? {
        if let Some(source) = sources.iter().find(|x| x.name == sub.name) {
            parts::copy_backups_to(
                &state.db,
                SubKind::Database.morph(),
                source.id,
                sub.id,
                s.team_id,
                state.now(),
            )
            .await?;
        }
    }
    if !pending.is_empty()
        && let Some(source_server) = s.server_id()
    {
        jobs.spawn_service_volume_clones(s.clone(), source_server, server.id, pending)
            .await;
    }
    Ok(())
}

/// The stored `domain_port_overrides`.
fn stored_overrides(a: &ApplicationRow) -> BTreeMap<String, i64> {
    let parsed = match a.raw.get("domain_port_overrides") {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).ok(),
        Some(v) => Some(v.clone()),
        None => None,
    };
    let Some(Value::Object(o)) = parsed else {
        return BTreeMap::new();
    };
    o.iter()
        .filter_map(|(k, v)| {
            v.as_i64()
                .or_else(|| v.as_str()?.parse().ok())
                .map(|p| (k.clone(), p))
        })
        .collect()
}

/// `str($name)->limit(20)`.
fn limit_name(name: &str) -> String {
    if name.chars().count() > 20 {
        format!("{}...", name.chars().take(20).collect::<String>())
    } else {
        name.to_owned()
    }
}

/// The `saving` hook sets these to the trimmed value (`str(null)->trim()` is an empty string).
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

/// `clone_application($source, $destination, ['environment_id' => ...], false)`. Previews are copied
/// without their domains until the preview helpers of the application API are available here.
async fn clone_application(
    state: &AppState,
    source: &ApplicationRow,
    destination: &Destination,
    environment_id: i64,
    clone_volume_data: bool,
) -> Result<String, ApiError> {
    let (destination_id, morph, server) = destination;
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let now = state.now();
    let new_uuid = new_public_id();
    let name = format!(
        "clone-of-{}-{new_uuid}",
        limit_name(source.str("name").unwrap_or_default())
    );
    let proxy = server
        .record
        .get("proxy")
        .and_then(|p| match p {
            Value::String(s) => serde_json::from_str::<Value>(s).ok(),
            other => Some(other.clone()),
        })
        .and_then(|p| p.get("type").and_then(Value::as_str).map(str::to_owned));
    let proxied = proxy.as_deref() != Some("NONE");
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
    let url = url.filter(|u| !u.is_empty());
    let is_compose = source.str("build_pack") == Some("dockercompose");
    let mut columns: Vec<(&'static str, ColumnValue)> = vec![
        ("uuid", ColumnValue::Text(Some(new_uuid.clone()))),
        ("name", ColumnValue::Text(Some(name))),
        ("status", ColumnValue::Text(Some("exited:unhealthy".into()))),
        ("environment_id", ColumnValue::Int(environment_id)),
        ("destination_id", ColumnValue::Int(*destination_id)),
        ("destination_type", ColumnValue::Text(Some((*morph).into()))),
        ("last_online_at", ColumnValue::Time(now)),
        (
            "compose_parsing_version",
            ColumnValue::Text(Some("5".into())),
        ),
    ];
    let fqdn = if !is_compose || url.is_some() {
        let (f, o) = domains::normalize_port_overrides(url.as_deref(), &stored_overrides(source));
        columns.push((
            "domain_port_overrides",
            ColumnValue::Text(o.map(|o| domains::overrides_json(f.as_deref(), &o))),
        ));
        f
    } else {
        None
    };
    columns.push(("fqdn", ColumnValue::Text(fqdn)));
    for col in TRIMMED_ON_SAVE {
        let trimmed = php_trim(source.str(col).unwrap_or_default());
        let value = if matches!(*col, "base_directory" | "publish_directory") {
            format!("/{}", trimmed.trim_start_matches('/'))
        } else {
            trimmed.to_owned()
        };
        columns.push((col, ColumnValue::Text(Some(value))));
    }
    if source.str("custom_nginx_configuration") == Some("") {
        columns.push(("custom_nginx_configuration", ColumnValue::Text(None)));
    }
    for col in [
        "manual_webhook_secret_github",
        "manual_webhook_secret_gitlab",
        "manual_webhook_secret_bitbucket",
        "manual_webhook_secret_gitea",
    ] {
        if source.str(col).is_none() {
            columns.push((
                col,
                ColumnValue::Text(Some(
                    enc.encrypt_serialized(&crate::ui_applications::random_str(40)),
                )),
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
    let plan = coolify_db::application_ops::CloneApplication {
        source_id: source.id,
        source_uuid: source.uuid.clone(),
        new_uuid: new_uuid.clone(),
        server_team_id: server.team_id,
        columns,
        nixpacks_default,
    };
    let (new_id, _previews, volumes) =
        coolify_db::application_ops::clone_application(&state.db, &plan, now).await?;
    if clone_volume_data && !volumes.is_empty() {
        let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
        jobs.spawn_clone_volumes(source.clone(), server.id, volumes)
            .await;
    }
    if proxied && readonly_labels {
        crate::ui_applications::regenerate_labels(state, new_id, false).await?;
    }
    Ok(new_uuid)
}

// ---------------------------------------------------------------------------
// CloneMe

/// `str($value)->slug()`.
fn slug(value: &str) -> String {
    let replaced = value.replace('_', "-").replace('@', "-at-").to_lowercase();
    let kept: String = replaced
        .chars()
        .filter(|c| *c == '-' || c.is_alphanumeric() || c.is_whitespace())
        .collect();
    kept.split(|c: char| c == '-' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// The rows of the page's table: `{name, description}`.
fn row_json(r: &Record) -> Value {
    json!({"name": r.get("name"), "description": r.get("description")})
}

fn by_name(mut rows: Vec<Value>) -> Vec<Value> {
    rows.sort_by(|a, b| {
        a.get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .cmp(b.get("name").and_then(Value::as_str).unwrap_or_default())
    });
    rows
}

/// `mount()` and `clone-me.blade.php`.
async fn clone_me_page(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((project, env)): Path<(String, String)>,
) -> ApiResult {
    let p = projects::find(&state.db, ctx.team_id, &project)
        .await?
        .ok_or(super::NOT_FOUND)?;
    let e = projects::find_environment(&state.db, p.id, &env)
        .await?
        .ok_or(super::NOT_FOUND)?;
    let mut rows = servers::list(&state.db, p.team_id).await?;
    rows.sort_by_key(|r| r.id);
    let mut server_list = Vec::new();
    for row in rows.iter().filter(|r| !is_build(r)) {
        // `->filter(fn ($server) => Gate::allows('placeResource', $server))`.
        if !crate::place_resource::allows(&state, &ctx, row).await? {
            continue;
        }
        server_list.push(json!({
            "id": row.id,
            "name": row.record.get("name"),
            "destinations": destinations_json(&state, row.id).await?,
        }));
    }
    let applications = apps::list_for_environment(&state.db, e.id).await?;
    let databases = dbs::list_for_environment(&state.db, e.id).await?;
    let services = svc::list_for_environment(&state.db, e.id).await?;
    Ok(ok(json!({
        "can_update": ctx.is_team_admin(),
        "project_name": p.name,
        "environment_name": e.name,
        "new_name": slug(&coolify_jobs::notifications::php::clone_resource_name(&p.name, &new_public_id())),
        "servers": server_list,
        "applications": by_name(applications.iter().map(|a| row_json(&a.raw)).collect()),
        "databases": by_name(databases.iter().map(|d| row_json(&d.raw)).collect()),
        "services": by_name(services.iter().map(|s| row_json(&s.raw)).collect()),
    }))
    .into_response())
}

/// `clone($type)` and `cloneInto()`.
async fn clone_me(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((project, env)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult {
    let p = projects::find(&state.db, ctx.team_id, &project)
        .await?
        .ok_or(super::NOT_FOUND)?;
    let e = projects::find_environment(&state.db, p.id, &env)
        .await?
        .ok_or(super::NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let new_name = body
        .get("new_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let selected = body
        .get("selected_destination")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut errors = Map::new();
    if selected.is_empty() {
        errors.insert(
            "selectedDestination".into(),
            json!(["Please select a server & destination."]),
        );
    }
    if let Some(message) = name_error(new_name) {
        errors.insert("newName".into(), json!([message]));
    }
    if !errors.is_empty() {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"message": "The given data was invalid.", "errors": errors})),
        )
            .into_response());
    }
    let Some(destination) = team_destination(&state, p.team_id, selected).await? else {
        return Err(bad("Destination not found."));
    };
    // `$this->authorize('placeResource', $selectedDestination->server)`.
    crate::place_resource::authorize(&state, &ctx, &destination.2).await?;
    let is_project = body.get("type").and_then(Value::as_str) == Some("project");
    // `authorize(sourceAbility($source))` for each resource: admin abilities, checked above.
    let clone_volume_data = body
        .get("clone_volume_data")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let now = state.now();
    let (project_uuid, target_env_id, target_env_uuid) = if is_project {
        if ops::team_project_named(&state.db, p.team_id, new_name).await? {
            return Err(bad("Project with the same name already exists."));
        }
        let description = format!("{} (clone)", p.description.as_deref().unwrap_or_default());
        let created = projects::create(
            &state.db,
            p.team_id,
            Some(new_name),
            Some(&description),
            now,
        )
        .await?;
        let env_name = e.name.as_str();
        if env_name != "production" {
            ops::create_environment(&state.db, created.id, env_name, now).await?;
        }
        let target = projects::find_environment(&state.db, created.id, env_name)
            .await?
            .ok_or(ApiError::Internal)?;
        (created.uuid, target.id, target.uuid)
    } else {
        if ops::environment_named(&state.db, p.id, new_name).await? {
            return Err(bad("Environment with the same name already exists."));
        }
        let (id, uuid) = ops::create_environment(&state.db, p.id, new_name, now).await?;
        (p.uuid.clone(), id, uuid)
    };
    for a in apps::list_for_environment(&state.db, e.id).await? {
        clone_application(&state, &a, &destination, target_env_id, clone_volume_data).await?;
    }
    for d in dbs::list_for_environment(&state.db, e.id).await? {
        let name = d
            .raw
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        clone_database(
            &state,
            &d,
            &new_public_id(),
            &name,
            &destination,
            target_env_id,
            clone_volume_data,
        )
        .await?;
    }
    for s in svc::list_for_environment(&state.db, e.id).await? {
        let name = s.name().to_owned();
        clone_service(
            &state,
            &s,
            &new_public_id(),
            &name,
            &destination,
            target_env_id,
            clone_volume_data,
        )
        .await?;
    }
    Ok(ok(json!({
        "redirect": format!("/project/{project_uuid}/environment/{target_env_uuid}"),
    }))
    .into_response())
}

/// `ValidationPatterns::nameRules()` for `newName`, with its messages.
fn name_error(name: &str) -> Option<String> {
    use coolify_domain::naming::{NAME_MAX, NAME_MIN, NAME_PATTERN};
    if name.is_empty() {
        return Some("Please enter a name for the new project or environment.".into());
    }
    let count = name.chars().count();
    if (count as u64) < u64::try_from(NAME_MIN).unwrap_or(3) {
        return Some(format!(
            "The new name field must be at least {NAME_MIN} characters."
        ));
    }
    if (count as u64) > u64::try_from(NAME_MAX).unwrap_or(255) {
        return Some(format!(
            "The new name field must not be greater than {NAME_MAX} characters."
        ));
    }
    if !NAME_PATTERN.is_match(name) {
        return Some("The new name field format is invalid.".to_owned());
    }
    None
}
