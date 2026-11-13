//! `App\Livewire\Boarding\Index`: the onboarding wizard. The page holds the wizard's state; each
//! Livewire action is one POST (`skipBoarding` stays in `ui_auth.rs`).

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use coolify_compat::sshkey::{KeyKind, format_private_key, generate, inspect};
use coolify_db::private_keys::{self, NewKey};
use coolify_db::servers::{self, ServerRow};
use coolify_jobs::server_ops::Srv;
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::ui_notifications::{toast_error, unauthorized};

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/onboarding", get(mount).post(action))
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

#[derive(serde::Deserialize)]
struct Action {
    action: String,
    #[serde(default)]
    values: Map<String, Value>,
}

fn error(message: impl Into<String>) -> Response {
    toast_error(StatusCode::BAD_REQUEST, message)
}

/// `PrivateKey::ownedAndOnlySShKeys(['name'])->where('id', '!=', 0)->get()`.
async fn team_keys(state: &AppState, team: i64) -> Result<Vec<Value>, ApiError> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, name FROM private_keys WHERE team_id = $1 AND is_git_related = false AND id <> 0 ORDER BY name",
    )
    .bind(team)
    .fetch_all(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    Ok(rows
        .iter()
        .map(|(id, name)| json!({"value": id, "label": name}))
        .collect())
}

/// `Project::ownedByCurrentTeam(['name'])->get()`.
async fn team_projects(state: &AppState, team: i64) -> Result<Vec<Value>, ApiError> {
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, name FROM projects WHERE team_id = $1 ORDER BY LOWER(name)")
            .bind(team)
            .fetch_all(state.db.pool())
            .await
            .map_err(|_| ApiError::Internal)?;
    Ok(rows
        .iter()
        .map(|(id, name)| json!({"value": id, "label": name}))
        .collect())
}

async fn public_key_of(state: &AppState, s: &ServerRow) -> Option<String> {
    let id = s.record.get("private_key_id").and_then(Value::as_i64)?;
    let cipher: Option<String> =
        sqlx::query_scalar("SELECT private_key FROM private_keys WHERE id = $1")
            .bind(id)
            .fetch_optional(state.db.pool())
            .await
            .ok()
            .flatten();
    let plain = state.encrypter.as_ref()?.decrypt_string(&cipher?).ok()?;
    inspect(&plain).map(|i| i.public_key)
}

fn server_json(s: &ServerRow) -> Value {
    json!({
        "id": s.record.get("id"),
        "uuid": s.record.get("uuid"),
        "name": s.record.get("name"),
        "ip": s.record.get("ip"),
        "port": s.record.get("port"),
        "user": s.record.get("user"),
    })
}

/// `mount`.
async fn mount(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(ok(json!({"redirect": "/"})).into_response());
    }
    let team = ctx.team_id;
    let int = |k: &str| q.get(k).and_then(|v| v.parse::<i64>().ok());
    let kind = q.get("selectedServerType").map_or("", String::as_str);
    let mut res = json!({
        "min_docker_version": "24",
        "private_key_name": crate::routes::private_keys::random_name(),
        "server_name": crate::routes::private_keys::random_name(),
        "private_keys": [],
        "projects": [],
        "server": null,
        "public_key": null,
        "created_project": null,
    });
    if kind == "localhost"
        && int("selectedExistingServer") == Some(0)
        && let Some(s) = servers::find_by_id(&state.db, 0).await?
    {
        res["server"] = server_json(&s);
        res["server_public_key"] = json!(public_key_of(&state, &s).await);
    }
    if kind == "remote" {
        res["private_keys"] = json!(team_keys(&state, team).await?);
        if let Some(id) = int("selectedExistingServer").filter(|i| *i != 0)
            && let Some(s) = servers::find_by_id(&state.db, id)
                .await?
                .filter(|s| s.record.get("team_id").and_then(Value::as_i64) == Some(team))
        {
            res["server"] = server_json(&s);
            res["server_public_key"] = json!(public_key_of(&state, &s).await);
        }
        if q.get("privateKeyType").map(String::as_str) == Some("create") {
            res["generated"] = tokio::task::spawn_blocking(generate_pair)
                .await
                .map_err(|_| ApiError::Internal)?;
        }
    }
    if let Some(p) = int("selectedProject") {
        let row: Option<(i64, String)> =
            sqlx::query_as("SELECT id, name FROM projects WHERE id = $1 AND team_id = $2")
                .bind(p)
                .bind(team)
                .fetch_optional(state.db.pool())
                .await
                .map_err(|_| ApiError::Internal)?;
        match row {
            Some((id, name)) => res["created_project"] = json!({"id": id, "name": name}),
            None => res["projects"] = json!(team_projects(&state, team).await?),
        }
    }
    if q.get("step").map(String::as_str) == Some("create-project") {
        res["projects"] = json!(team_projects(&state, team).await?);
    }
    Ok(ok(res).into_response())
}

/// `createNewPrivateKey`: `generateSSHKey()`, whose default type is Ed25519 (v4.7).
fn generate_pair() -> Value {
    let key = generate(KeyKind::Ed25519).unwrap_or_default();
    let public = inspect(&key).map(|i| i.public_key).unwrap_or_default();
    json!({
        "name": crate::routes::private_keys::random_name(),
        "description": "Created by Coolify",
        "private_key": key,
        "public_key": format!("{public} coolify-generated-ssh-key"),
    })
}

fn text<'a>(v: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn int(v: &Map<String, Value>, k: &str) -> Option<i64> {
    match v.get(k) {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
}

fn validation(errors: Vec<(&str, String)>) -> Response {
    let mut m = Map::new();
    for (k, e) in &errors {
        m.insert((*k).to_owned(), json!([e]));
    }
    ok(json!({"errors": m})).into_response()
}

/// `SERVER_USERNAME_PATTERN`.
fn username_ok(u: &str) -> bool {
    !u.is_empty()
        && u.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `Auditable`: `ui.{resource}.created` or `updated` with the model's name and uuid.
#[allow(clippy::too_many_arguments)]
async fn audit_model(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    resource: &str,
    action: &str,
    uuid: Value,
    name: &str,
    changed: &[&str],
    changes: Value,
) {
    let mut context = Map::new();
    context.insert("team_id".into(), json!(ctx.team_id));
    context.insert(format!("{resource}_uuid"), uuid);
    context.insert(format!("{resource}_name"), json!(name));
    context.insert("changed_fields".into(), json!(changed));
    crate::audit::mutation(
        state,
        ctx,
        meta,
        resource,
        action,
        Value::Object(context),
        changes,
    )
    .await;
}

/// `skipBoarding`: `Team::update(['show_boarding' => false])` with its `Auditable` event.
async fn finish(state: &AppState, ctx: &AuthCtx, meta: &Meta) -> Result<(), ApiError> {
    let read = || async {
        let t: Option<String> =
            sqlx::query_scalar("SELECT row_to_json(t)::text FROM teams t WHERE id = $1")
                .bind(ctx.team_id)
                .fetch_optional(state.db.pool())
                .await
                .map_err(|_| ApiError::Internal)?;
        Ok::<_, ApiError>(t.and_then(|t| serde_json::from_str::<Map<String, Value>>(&t).ok()))
    };
    let old = read().await?;
    coolify_db::auth_pages::finish_boarding(&state.db, ctx.team_id, state.now()).await?;
    let new = read().await?;
    let actor = crate::audit::actor(state, ctx, meta).await;
    crate::audit::team_mutation(
        state,
        &actor,
        ctx.team_id,
        "updated",
        old.as_ref(),
        new.as_ref(),
    )
    .await;
    Ok(())
}

/// `validateServer` and `continueValidation`: on success the wizard goes to the project step.
async fn validate_server(state: &AppState, ctx: &AuthCtx, meta: &Meta, s: ServerRow) -> ApiResult {
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let id = s.record.get("id").and_then(Value::as_i64).unwrap_or(0);
    let srv = Srv::from_row(jobs, s.clone());
    let set = |col: &'static str, on: bool| {
        let db = state.db.pool().clone();
        let now = state.now();
        async move {
            sqlx::query(&format!(
                "UPDATE server_settings SET {col} = $1, updated_at = $2 WHERE server_id = $3"
            ))
            .bind(on)
            .bind(now)
            .bind(id)
            .execute(&db)
            .await
            .map(|_| ())
            .map_err(|_| ApiError::Internal)
        }
    };
    if let Err(e) = srv.run(&["ls /".to_owned()], "server.validate").await {
        set("is_reachable", false).await?;
        return Ok(ok(json!({"server_reachable": false, "error": e})).into_response());
    }
    set("is_reachable", true).await?;
    let missing = coolify_jobs::server_ops::install::missing_prerequisites(&srv).await;
    if !missing.is_empty() {
        return Ok(ok(json!({
            "error": format!("Prerequisites ({}) could not be installed after 3 attempts. Please install them manually.", missing.join(", ")),
        }))
        .into_response());
    }
    let version = srv
        .run(
            &["docker version|head -2|grep -i version| awk '{print $2}'".to_owned()],
            "server.validate",
        )
        .await;
    let new_enough = match &version {
        Ok(v) => coolify_jobs::checks::check_minimum_docker_engine_version(v.as_deref()).is_some(),
        Err(_) => false,
    };
    if !new_enough {
        set("is_usable", false).await?;
        return Ok(ok(json!({
            "state": "validate-server",
            "error": match version {
                Err(e) => e,
                Ok(_) => "Docker not found or old version is installed.".to_owned(),
            },
        }))
        .into_response());
    }
    set("is_usable", true).await?;
    // `getProxyType` -> `selectProxy(TRAEFIK)` keeps the other proxy attributes.
    let proxy = s
        .record
        .get("proxy")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut proxy = proxy;
    proxy.insert("redirect_enabled".into(), json!(true));
    proxy.insert("type".into(), json!("TRAEFIK"));
    proxy.insert("status".into(), json!("exited"));
    proxy.insert("last_saved_settings".into(), Value::Null);
    proxy.insert("last_applied_settings".into(), Value::Null);
    sqlx::query("UPDATE servers SET proxy = $1::jsonb, updated_at = $2 WHERE id = $3")
        .bind(Value::Object(proxy).to_string())
        .bind(state.now())
        .bind(id)
        .execute(state.db.pool())
        .await
        .map_err(|_| ApiError::Internal)?;
    // `validate-and-install` starts the proxy once the server is validated.
    let j = jobs.clone();
    tokio::spawn(async move {
        let Ok(mut srv) = Srv::load(&j, id).await else {
            return;
        };
        // The localhost path of `continueValidation` only saves the proxy type: its proxy already
        // runs since the first start.
        if !srv.is_proxy_should_run() || srv.is_localhost() {
            return;
        }
        if let Err(e) =
            coolify_jobs::server_ops::proxy::start_proxy(&mut srv, true, false, false).await
        {
            tracing::warn!(server_id = id, error = %e, "proxy start failed");
        }
    });
    audit_model(
        state,
        ctx,
        meta,
        "server",
        "updated",
        s.record.get("uuid").cloned().unwrap_or(Value::Null),
        s.record.get("name").and_then(Value::as_str).unwrap_or(""),
        &["proxy"],
        json!({}),
    )
    .await;
    projects_step(state, ctx.team_id, Some(id)).await
}

/// `getProjects`.
async fn projects_step(state: &AppState, team: i64, server: Option<i64>) -> ApiResult {
    let projects = team_projects(state, team).await?;
    let selected = projects.first().and_then(|p| p.get("value")).cloned();
    Ok(ok(json!({
        "state": "create-project",
        "projects": projects,
        "selected_project": selected,
        "selected_server": server,
    }))
    .into_response())
}

#[allow(clippy::too_many_lines)]
async fn action(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Action>,
) -> ApiResult {
    let team = ctx.team_id;
    let v = body.values;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let owned_server = |id: i64| {
        let state = state.clone();
        async move {
            Ok::<_, ApiError>(servers::find_by_id(&state.db, id).await?.filter(|s| {
                // `authorize('update', $createdServer)`: the localhost server belongs to the root team.
                s.record.get("team_id").and_then(Value::as_i64) == Some(team)
            }))
        }
    };
    match body.action.as_str() {
        "skipBoarding" => {
            finish(&state, &ctx, &meta).await?;
            Ok(ok(json!({"message": "ok"})).into_response())
        }
        "setServerType" => match text(&v, "type") {
            Some("localhost") => {
                let Some(s) = servers::find_by_id(&state.db, 0).await? else {
                    return Ok(error(
                        "Localhost server is not found. Something went wrong during installation. Please try to reinstall or contact support.",
                    ));
                };
                if s.record.get("team_id").and_then(Value::as_i64) != Some(team) {
                    return Ok(unauthorized());
                }
                let mut out = json!({"selected_server": 0, "server": server_json(&s)});
                out["server_public_key"] = json!(public_key_of(&state, &s).await);
                let body = validate_server(&state, &ctx, &meta, s).await?;
                let bytes = axum::body::to_bytes(body.into_body(), 1 << 20)
                    .await
                    .map_err(|_| ApiError::Internal)?;
                if let Ok(Value::Object(m)) = serde_json::from_slice::<Value>(&bytes) {
                    for (k, val) in m {
                        out[k] = val;
                    }
                }
                Ok(ok(out).into_response())
            }
            Some("remote") => {
                let keys = team_keys(&state, team).await?;
                let selected = keys.first().and_then(|k| k.get("value")).cloned();
                Ok(ok(json!({
                    "state": "private-key",
                    "private_keys": keys,
                    "selected_private_key": selected,
                }))
                .into_response())
            }
            _ => Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found.")),
        },
        "selectExistingPrivateKey" => {
            let Some(id) = int(&v, "id") else {
                return Ok(error("Please select a private key."));
            };
            if private_keys::find_by_id(&state.db, team, id)
                .await?
                .is_none()
            {
                return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
            }
            Ok(ok(json!({"state": "create-server"})).into_response())
        }
        "setPrivateKey" => {
            let mut res = json!({"state": "create-private-key", "selected_private_key": null});
            if text(&v, "type") == Some("create") {
                res["generated"] = tokio::task::spawn_blocking(generate_pair)
                    .await
                    .map_err(|_| ApiError::Internal)?;
            }
            Ok(ok(res).into_response())
        }
        "savePrivateKey" => {
            let mut e = Vec::new();
            match v.get("privateKeyName") {
                _ if text(&v, "privateKeyName").is_none() => {
                    e.push((
                        "privateKeyName",
                        "The private key name field is required.".to_owned(),
                    ));
                }
                Some(Value::String(s)) if s.chars().count() > 255 => e.push((
                    "privateKeyName",
                    "The private key name field must not be greater than 255 characters."
                        .to_owned(),
                )),
                _ => {}
            }
            if text(&v, "privateKeyDescription").is_some_and(|d| d.chars().count() > 255) {
                e.push((
                    "privateKeyDescription",
                    "The private key description field must not be greater than 255 characters."
                        .to_owned(),
                ));
            }
            if text(&v, "privateKey").is_none() {
                e.push((
                    "privateKey",
                    "The private key field is required.".to_owned(),
                ));
            }
            if !e.is_empty() {
                return Ok(validation(e));
            }
            let raw = text(&v, "privateKey").unwrap_or_default();
            let Some(info) = inspect(raw) else {
                return Ok(
                    ok(json!({"errors": {"private_key": ["The private key is invalid."]}}))
                        .into_response(),
                );
            };
            if private_keys::fingerprint_exists(&state.db, team, &info.fingerprint, None).await? {
                return Ok(ok(
                    json!({"errors": {"private_key": ["This private key already exists."]}}),
                )
                .into_response());
            }
            let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
            let stored = format_private_key(raw);
            let uuid = private_keys::create(
                &state.db,
                NewKey {
                    team_id: team,
                    name: text(&v, "privateKeyName").unwrap_or_default(),
                    description: v
                        .get("privateKeyDescription")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    private_key_encrypted: &enc.encrypt_string(&stored),
                    fingerprint: &info.fingerprint,
                },
                state.now(),
            )
            .await?;
            let id: i64 = sqlx::query_scalar("SELECT id FROM private_keys WHERE uuid = $1")
                .bind(&uuid)
                .fetch_one(state.db.pool())
                .await
                .map_err(|_| ApiError::Internal)?;
            let name = text(&v, "privateKeyName").unwrap_or_default();
            audit_model(
                &state,
                &ctx,
                &meta,
                "private_key",
                "created",
                json!(uuid),
                name,
                &[],
                json!({"name": {"old": null, "new": name}}),
            )
            .await;
            Ok(ok(json!({"state": "create-server", "created_private_key": id})).into_response())
        }
        "saveServer" => {
            let mut e = Vec::new();
            if text(&v, "remoteServerName").is_none() {
                e.push((
                    "remoteServerName",
                    "The remote server name field is required.".to_owned(),
                ));
            }
            match text(&v, "remoteServerHost") {
                None => e.push((
                    "remoteServerHost",
                    "The remote server host field is required.".to_owned(),
                )),
                // `new ValidServerIp`.
                Some(host) => {
                    if let Some(message) = crate::routes::servers::valid_server_ip(&json!(host)) {
                        e.push((
                            "remoteServerHost",
                            message.replace(":attribute", "remote server host"),
                        ));
                    }
                }
            }
            match int(&v, "remoteServerPort") {
                None => e.push((
                    "remoteServerPort",
                    "The remote server port field is required.".to_owned(),
                )),
                Some(p) if p < 1 => e.push((
                    "remoteServerPort",
                    "The remote server port field must be at least 1.".to_owned(),
                )),
                Some(p) if p > 65535 => e.push((
                    "remoteServerPort",
                    "The remote server port field must not be greater than 65535.".to_owned(),
                )),
                _ => {}
            }
            match text(&v, "remoteServerUser") {
                None => e.push(("remoteServerUser", "The remote server user field is required.".to_owned())),
                Some(u) if !username_ok(u) => e.push((
                    "remoteServerUser",
                    "The SSH User may only contain letters, numbers, dots, hyphens, and underscores.".to_owned(),
                )),
                _ => {}
            }
            if !e.is_empty() {
                return Ok(validation(e));
            }
            let ip =
                crate::routes::servers::normalize_ip(text(&v, "remoteServerHost").unwrap_or(""));
            // `Server::sshEndpointInUse()`.
            if coolify_db::servers::ssh_endpoint_in_use(
                &state.db,
                &ip,
                int(&v, "remoteServerPort").unwrap_or(22),
                None,
            )
            .await?
            {
                return Ok(error(
                    "A server with this IP/Domain and SSH port already exists.",
                ));
            }
            let key_id = int(&v, "privateKeyId").unwrap_or(-1);
            if private_keys::find_by_id(&state.db, team, key_id)
                .await?
                .is_none()
            {
                return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
            }
            let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
            let uuid = coolify_domain::new_public_id();
            let name = text(&v, "remoteServerName").unwrap_or_default().to_owned();
            let token = enc
                .encrypt_string(&enc.encrypt_serialized(&json!({"server_uuid": uuid}).to_string()));
            let new = servers::NewServer {
                sentinel_custom_url: coolify_db::instance_settings::row(&state.db)
                    .await
                    .ok()
                    .and_then(|r| {
                        r.get("fqdn")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                            .or_else(|| {
                                r.get("public_ipv4")
                                    .and_then(Value::as_str)
                                    .map(|ip| format!("http://{ip}:8000"))
                            })
                    }),
                sentinel_token_encrypted: token,
                shared_vars: [
                    ("COOLIFY_SERVER_UUID", enc.encrypt_string(&uuid)),
                    ("COOLIFY_SERVER_NAME", enc.encrypt_string(&name)),
                ],
                uuid: uuid.clone(),
                name,
                description: coolify_domain::naming::strip_tags_nullable(text(
                    &v,
                    "remoteServerDescription",
                )),
                ip,
                port: int(&v, "remoteServerPort").unwrap_or(22),
                user: text(&v, "remoteServerUser").unwrap_or("root").to_owned(),
                private_key_id: key_id,
                team_id: team,
                // `Server::create` without a proxy: the attribute stays empty until `selectProxy`.
                proxy: "null".to_owned(),
                docker_uuid: coolify_domain::new_public_id(),
                server_role: "both",
                always_write_role: false,
            };
            servers::create(&state.db, &new, state.now()).await?;
            let s = servers::find(&state.db, team, &uuid)
                .await?
                .ok_or(ApiError::Internal)?;
            let server_name = s.record.get("name").and_then(Value::as_str).unwrap_or("");
            audit_model(
                &state,
                &ctx,
                &meta,
                "server",
                "created",
                json!(uuid),
                server_name,
                &[],
                json!({"name": {"old": null, "new": server_name}}),
            )
            .await;
            for shared in ["COOLIFY_SERVER_UUID", "COOLIFY_SERVER_NAME"] {
                audit_model(
                    &state,
                    &ctx,
                    &meta,
                    "shared_environment_variable",
                    "created",
                    Value::Null,
                    shared,
                    &[],
                    json!({"key": {"old": null, "new": shared}}),
                )
                .await;
            }
            Ok(ok(json!({
                "state": "validate-server",
                "selected_server": s.record.get("id"),
                "server": server_json(&s),
                "server_public_key": public_key_of(&state, &s).await,
            }))
            .into_response())
        }
        "validateServer" => {
            let Some(s) = owned_server(int(&v, "serverId").unwrap_or(-1)).await? else {
                return Ok(unauthorized());
            };
            validate_server(&state, &ctx, &meta, s).await
        }
        "saveAndValidateServer" => {
            let mut e = Vec::new();
            match int(&v, "remoteServerPort") {
                None => e.push((
                    "remoteServerPort",
                    "The remote server port field is required.".to_owned(),
                )),
                Some(p) if p < 1 => e.push((
                    "remoteServerPort",
                    "The remote server port field must be at least 1.".to_owned(),
                )),
                Some(p) if p > 65535 => e.push((
                    "remoteServerPort",
                    "The remote server port field must not be greater than 65535.".to_owned(),
                )),
                _ => {}
            }
            match text(&v, "remoteServerUser") {
                None => e.push(("remoteServerUser", "The remote server user field is required.".to_owned())),
                Some(u) if !username_ok(u) => e.push((
                    "remoteServerUser",
                    "The SSH User may only contain letters, numbers, dots, hyphens, and underscores.".to_owned(),
                )),
                _ => {}
            }
            if !e.is_empty() {
                return Ok(validation(e));
            }
            let id = int(&v, "serverId").unwrap_or(-1);
            if owned_server(id).await?.is_none() {
                return Ok(unauthorized());
            }
            sqlx::query(
                "UPDATE servers SET port = $1, \"user\" = $2, updated_at = $3 WHERE id = $4",
            )
            .bind(int(&v, "remoteServerPort").unwrap_or(22) as i32)
            .bind(text(&v, "remoteServerUser").unwrap_or("root"))
            .bind(state.now())
            .bind(id)
            .execute(state.db.pool())
            .await
            .map_err(|_| ApiError::Internal)?;
            let s = owned_server(id).await?.ok_or(ApiError::Internal)?;
            validate_server(&state, &ctx, &meta, s).await
        }
        "selectExistingProject" => {
            let row: Option<(i64, String)> =
                sqlx::query_as("SELECT id, name FROM projects WHERE id = $1 AND team_id = $2")
                    .bind(int(&v, "id").unwrap_or(-1))
                    .bind(team)
                    .fetch_optional(state.db.pool())
                    .await
                    .map_err(|_| ApiError::Internal)?;
            let Some((id, name)) = row else {
                return Ok(error("Project not found."));
            };
            Ok(ok(json!({"state": "create-resource", "created_project": {"id": id, "name": name}}))
                .into_response())
        }
        "createNewProject" => {
            let p = coolify_db::projects::create(
                &state.db,
                team,
                Some("My first project"),
                None,
                state.now(),
            )
            .await?;
            let env_uuid: Option<String> = sqlx::query_scalar(
                "SELECT uuid FROM environments WHERE project_id = $1 ORDER BY id LIMIT 1",
            )
            .bind(p.id)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|_| ApiError::Internal)?;
            audit_model(
                &state,
                &ctx,
                &meta,
                "project",
                "created",
                json!(p.uuid),
                "My first project",
                &[],
                json!({"name": {"old": null, "new": "My first project"}}),
            )
            .await;
            audit_model(
                &state,
                &ctx,
                &meta,
                "environment",
                "created",
                json!(env_uuid),
                "production",
                &[],
                json!({"name": {"old": null, "new": "production"}}),
            )
            .await;
            Ok(ok(json!({
                "state": "create-resource",
                "created_project": {"id": p.id, "name": "My first project"},
            }))
            .into_response())
        }
        "showNewResource" => {
            let Some(s) = owned_server(int(&v, "serverId").unwrap_or(-1)).await? else {
                return Ok(unauthorized());
            };
            let row: Option<(String, Option<String>)> = sqlx::query_as(
                "SELECT p.uuid, (SELECT e.uuid FROM environments e WHERE e.project_id = p.id ORDER BY e.id LIMIT 1)
                 FROM projects p WHERE p.id = $1 AND p.team_id = $2",
            )
            .bind(int(&v, "projectId").unwrap_or(-1))
            .bind(team)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|_| ApiError::Internal)?;
            let Some((project, Some(env))) = row else {
                return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
            };
            finish(&state, &ctx, &meta).await?;
            Ok(ok(json!({
                "redirect": format!(
                    "/project/{project}/environment/{env}/new?server={}",
                    s.record.get("id").and_then(Value::as_i64).unwrap_or(0)
                ),
            }))
            .into_response())
        }
        _ => Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found.")),
    }
}
