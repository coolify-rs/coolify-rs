//! Page endpoints of `App\Livewire\Security\{CloudProviderTokens, CloudProviderTokenForm,
//! CloudProviderToken\Show, CloudInitScripts, CloudInitScriptForm, CloudInitScript\Show,
//! IntegrationTokens, IntegrationTokenForm, IntegrationTokenEditor}`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use coolify_db::cloud;
use coolify_db::integration_tokens as it;
use coolify_db::records::Record;
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::routes::cloud_tokens::cloud_init_rule;
use crate::routes::integration_tokens::{error_message, verify};
use crate::state::AppState;
use crate::ui_notifications::{blank, toast_error, unauthorized};

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/ui/security/cloud-tokens",
            get(cloud_tokens).post(add_cloud_token),
        )
        .route(
            "/ui/security/cloud-tokens/{uuid}",
            get(cloud_token)
                .patch(update_cloud_token)
                .delete(delete_cloud_token),
        )
        .route(
            "/ui/security/cloud-tokens/{uuid}/validate",
            post(validate_cloud_token),
        )
        .route(
            "/ui/security/cloud-init-scripts",
            get(scripts).post(create_script),
        )
        .route(
            "/ui/security/cloud-init-scripts/{uuid}",
            get(script).patch(update_script).delete(delete_script),
        )
        .route(
            "/ui/security/integration-tokens",
            get(integration_tokens).post(add_integration_token),
        )
        .route(
            "/ui/security/integration-tokens/{uuid}",
            axum::routing::patch(update_integration_token).delete(delete_integration_token),
        )
        .route(
            "/ui/security/integration-tokens/{uuid}/refresh-zones",
            post(refresh_zones),
        )
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

fn text<'a>(r: &'a Record, k: &str) -> Option<&'a str> {
    r.get(k).and_then(Value::as_str)
}

fn body_str<'a>(b: &'a Value, k: &str) -> &'a str {
    b.get(k).and_then(Value::as_str).unwrap_or_default()
}

/// Validation errors a page shows under its fields (`$this->validate()` outside the `try`).
fn field_errors(errors: Map<String, Value>) -> Response {
    let first = errors
        .values()
        .next()
        .and_then(|v| v.get(0))
        .and_then(Value::as_str)
        .unwrap_or("The given data was invalid.")
        .to_owned();
    PhpJson(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": first, "errors": errors}),
    )
    .into_response()
}

fn present_cloud_token(r: &Record) -> Value {
    json!({
        "uuid": r.get("uuid"),
        "name": r.get("name"),
        "provider": r.get("provider"),
        "description": r.get("description"),
        "created_at": r.get("created_at"),
    })
}

async fn cloud_tokens(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let rows = cloud::list_tokens(&state.db, ctx.team_id).await?;
    Ok(
        ok(json!({"tokens": rows.iter().map(present_cloud_token).collect::<Vec<_>>()}))
            .into_response(),
    )
}

async fn cloud_token(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let Some(r) = cloud::find_token(&state.db, ctx.team_id, &uuid).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    Ok(ok(present_cloud_token(&r)).into_response())
}

fn cloud_audit(ctx: &AuthCtx, r: &Record) -> Value {
    json!({
        "team_id": ctx.team_id,
        "cloud_token_uuid": r.get("uuid"),
        "cloud_token_name": r.get("name"),
        "provider": r.get("provider"),
    })
}

/// `CloudProviderTokenForm::addToken`.
async fn add_cloud_token(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Value>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let mut errors = Map::new();
    let provider = body_str(&body, "provider");
    if provider.trim().is_empty() {
        errors.insert(
            "provider".into(),
            json!(["Please select a cloud provider."]),
        );
    } else if !["hetzner", "digitalocean", "vultr", "hostinger"].contains(&provider) {
        errors.insert(
            "provider".into(),
            json!(["Invalid cloud provider selected."]),
        );
    }
    if blank(body.get("token")) {
        errors.insert("token".into(), json!(["API token is required."]));
    }
    let name = body_str(&body, "name");
    if name.trim().is_empty() {
        errors.insert("name".into(), json!(["Token name is required."]));
    } else if name.chars().count() > 255 {
        errors.insert(
            "name".into(),
            json!(["The name field must not be greater than 255 characters."]),
        );
    }
    if body_str(&body, "description").chars().count() > 1000 {
        errors.insert(
            "description".into(),
            json!(["The description field must not be greater than 1000 characters."]),
        );
    }
    if !errors.is_empty() {
        return Ok(field_errors(errors));
    }
    let token = body_str(&body, "token");
    if coolify_jobs::cloud::validate_provider_token(provider, token)
        .await
        .is_err()
    {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid API token. Please check your token and try again.",
        ));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let uuid = coolify_domain::new_public_id();
    let id = cloud::create_token(
        &state.db,
        ctx.team_id,
        &uuid,
        provider,
        &enc.encrypt_string(token),
        name,
        state.now(),
    )
    .await?;
    let description = body_str(&body, "description").trim();
    if !description.is_empty() {
        set_cloud_description(&state, id, Some(description)).await?;
    }
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.cloud_token.created",
        json!({"team_id": ctx.team_id, "cloud_token_uuid": uuid, "cloud_token_name": name, "provider": provider}),
        "info",
    )
    .await;
    Ok(ok(json!({"message": "Cloud provider token added successfully."})).into_response())
}

async fn set_cloud_description(
    state: &AppState,
    id: i64,
    description: Option<&str>,
) -> Result<(), ApiError> {
    sqlx::query("UPDATE cloud_provider_tokens SET description = $1 WHERE id = $2")
        .bind(description)
        .bind(id)
        .execute(state.db.pool())
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(())
}

/// `CloudProviderToken\Show::save`.
async fn update_cloud_token(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let Some(r) = cloud::find_token(&state.db, ctx.team_id, &uuid).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let name = body_str(&body, "name");
    let mut errors = Map::new();
    if name.trim().is_empty() {
        errors.insert("name".into(), json!(["Token name is required."]));
    } else if name.chars().count() > 255 {
        errors.insert(
            "name".into(),
            json!(["The name field must not be greater than 255 characters."]),
        );
    }
    if body_str(&body, "description").chars().count() > 1000 {
        errors.insert(
            "description".into(),
            json!(["The description field must not be greater than 1000 characters."]),
        );
    }
    if !errors.is_empty() {
        return Ok(field_errors(errors));
    }
    let description = body_str(&body, "description").trim();
    let id = coolify_db::database_parts::id_of(&r);
    let new_description = (!description.is_empty()).then_some(description);
    if text(&r, "name") != Some(name) {
        cloud::update_token_name(&state.db, id, name, state.now()).await?;
    }
    if text(&r, "description") != new_description {
        set_cloud_description(&state, id, new_description).await?;
    }
    let mut after = r.clone();
    after.insert("name".into(), json!(name));
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.cloud_token.updated",
        cloud_audit(&ctx, &after),
        "info",
    )
    .await;
    Ok(ok(json!({"message": "Cloud provider token updated."})).into_response())
}

/// `validateToken`.
async fn validate_cloud_token(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
) -> ApiResult {
    let Some(r) = cloud::find_token(&state.db, ctx.team_id, &uuid).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let token = text(&r, "token")
        .and_then(|c| enc.decrypt_string(c).ok())
        .unwrap_or_default();
    let provider = text(&r, "provider").unwrap_or_default();
    let valid = coolify_jobs::cloud::validate_provider_token(provider, &token)
        .await
        .is_ok();
    let label = match provider {
        "digitalocean" => "DigitalOcean",
        "vultr" => "Vultr",
        "hostinger" => "Hostinger",
        _ => "Hetzner",
    };
    let mut context = cloud_audit(&ctx, &r);
    context["valid"] = json!(valid);
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.cloud_token.validated",
        context,
        "info",
    )
    .await;
    if valid {
        Ok(ok(json!({"message": format!("{label} token is valid.")})).into_response())
    } else {
        Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{label} token validation failed. Please check the token."),
        ))
    }
}

/// `deleteToken` / `Show::delete`.
async fn delete_cloud_token(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
) -> ApiResult {
    let Some(r) = cloud::find_token(&state.db, ctx.team_id, &uuid).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let id = coolify_db::database_parts::id_of(&r);
    let servers: i64 =
        sqlx::query_scalar("SELECT count(*) FROM servers WHERE cloud_provider_token_id = $1")
            .bind(id)
            .fetch_one(state.db.pool())
            .await
            .map_err(|_| ApiError::Internal)?;
    if servers > 0 {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "Cannot delete this token. It is currently used by {servers} server(s). Please reassign those servers to a different token first."
            ),
        ));
    }
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.cloud_token.deleted",
        cloud_audit(&ctx, &r),
        "info",
    )
    .await;
    cloud::delete_token(&state.db, id).await?;
    Ok(ok(json!({})).into_response())
}

// ── Cloud-init scripts ──────────────────────────────────────────────────

fn present_script(state: &AppState, r: &Record, with_script: bool) -> Result<Value, ApiError> {
    let script = if with_script {
        let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
        text(r, "script")
            .map(|c| enc.decrypt_string(c).map_err(|_| ApiError::Internal))
            .transpose()?
    } else {
        None
    };
    Ok(json!({
        "uuid": r.get("uuid"),
        "name": r.get("name"),
        "updated_at": r.get("updated_at"),
        "created_at": r.get("created_at"),
        "script": script,
    }))
}

async fn scripts(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let mut rows = cloud::list_scripts(&state.db, ctx.team_id).await?;
    rows.sort_by_key(|r| std::cmp::Reverse(text(r, "created_at").unwrap_or_default().to_owned()));
    let list = rows
        .iter()
        .map(|r| present_script(&state, r, true))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ok(json!({"scripts": list})).into_response())
}

async fn script(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let Some(r) = cloud::find_script(&state.db, ctx.team_id, &uuid).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    Ok(ok(present_script(&state, &r, true)?).into_response())
}

fn script_errors(body: &Value) -> Map<String, Value> {
    let mut errors = Map::new();
    let name = body_str(body, "name");
    if name.trim().is_empty() {
        errors.insert("name".into(), json!(["Script name is required."]));
    } else if name.chars().count() > 255 {
        errors.insert(
            "name".into(),
            json!(["Script name cannot exceed 255 characters."]),
        );
    }
    let script = body.get("script").cloned().unwrap_or(Value::Null);
    if blank(Some(&script)) {
        errors.insert(
            "script".into(),
            json!(["Cloud-init script content is required."]),
        );
    } else if let Some(m) = cloud_init_rule(&script) {
        errors.insert("script".into(), json!([m.replace(":attribute", "script")]));
    }
    errors
}

fn script_audit(ctx: &AuthCtx, id: i64, name: &Value) -> Value {
    json!({"team_id": ctx.team_id, "cloud_init_script_id": id, "cloud_init_script_name": name})
}

/// `CloudInitScriptForm::save`.
async fn create_script(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Value>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let errors = script_errors(&body);
    if !errors.is_empty() {
        return Ok(field_errors(errors));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let name = body_str(&body, "name");
    let uuid = coolify_domain::new_public_id();
    let id = cloud::create_script(
        &state.db,
        ctx.team_id,
        &uuid,
        name,
        &enc.encrypt_string(body_str(&body, "script")),
        state.now(),
    )
    .await?;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.cloud_init_script.created",
        script_audit(&ctx, id, &json!(name)),
        "info",
    )
    .await;
    Ok(ok(json!({"message": "Cloud-init script created successfully."})).into_response())
}

/// `CloudInitScript\Show::save`.
async fn update_script(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let Some(r) = cloud::find_script(&state.db, ctx.team_id, &uuid).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let errors = script_errors(&body);
    if !errors.is_empty() {
        return Ok(field_errors(errors));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let name = body_str(&body, "name");
    let id = coolify_db::database_parts::id_of(&r);
    let plain = text(&r, "script").and_then(|c| enc.decrypt_string(c).ok());
    let new_script = body_str(&body, "script");
    let changed_script = plain.as_deref() != Some(new_script);
    if text(&r, "name") != Some(name) || changed_script {
        let cipher = changed_script.then(|| enc.encrypt_string(new_script));
        cloud::update_script(&state.db, id, Some(name), cipher.as_deref(), state.now()).await?;
    }
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.cloud_init_script.updated",
        script_audit(&ctx, id, &json!(name)),
        "info",
    )
    .await;
    Ok(ok(json!({"message": "Cloud-init script updated successfully."})).into_response())
}

/// `deleteScript` / `Show::delete`.
async fn delete_script(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
) -> ApiResult {
    let Some(r) = cloud::find_script(&state.db, ctx.team_id, &uuid).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let id = coolify_db::database_parts::id_of(&r);
    cloud::delete_script(&state.db, id).await?;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.cloud_init_script.deleted",
        script_audit(&ctx, id, r.get("name").unwrap_or(&Value::Null)),
        "info",
    )
    .await;
    Ok(ok(json!({})).into_response())
}

// ── Integration tokens ──────────────────────────────────────────────────

fn parse_json_column(r: &Record, k: &str) -> Value {
    match r.get(k) {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => Value::Null,
    }
}

async fn integration_tokens(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let mut rows = it::list(&state.db, ctx.team_id).await?;
    rows.sort_by_key(|r| std::cmp::Reverse(text(r, "created_at").unwrap_or_default().to_owned()));
    let mut list = Vec::new();
    for r in &rows {
        let id = coolify_db::database_parts::id_of(r);
        let zones: Vec<String> = sqlx::query_scalar(
            "SELECT json_build_object('id', z.id, 'name', z.name, 'account_name', z.account_name, \
               'managed_records_count', (SELECT count(*) FROM managed_dns_records d WHERE d.dns_provider_zone_id = z.id))::text \
             FROM dns_provider_zones z WHERE z.integration_token_id = $1 ORDER BY z.name",
        )
        .bind(id)
        .fetch_all(state.db.pool())
        .await
        .unwrap_or_default();
        list.push(json!({
            "uuid": r.get("uuid"),
            "name": r.get("name"),
            "provider": r.get("provider"),
            "capabilities": parse_json_column(r, "capabilities"),
            "metadata": parse_json_column(r, "metadata"),
            "zones": zones.iter().filter_map(|z| serde_json::from_str::<Value>(z).ok()).collect::<Vec<_>>(),
        }));
    }
    Ok(ok(json!({"tokens": list})).into_response())
}

/// `url:http,https`.
fn http_url(v: &str) -> bool {
    (v.starts_with("http://") || v.starts_with("https://")) && crate::laravel_url::is_url(v)
}

/// The `metadata.*` rules of the form and the editor.
fn metadata_errors(provider: &str, meta: &Map<String, Value>, errors: &mut Map<String, Value>) {
    let filled = |k: &str| meta.get(k).filter(|v| !blank(Some(v)));
    if matches!(provider, "infisical" | "vault") {
        match filled("base_url") {
            None => {
                errors.insert(
                    "metadata.base_url".into(),
                    json!(["The metadata.base url field is required."]),
                );
            }
            Some(v) if !v.as_str().is_some_and(http_url) => {
                errors.insert(
                    "metadata.base_url".into(),
                    json!(["The metadata.base url field must be a valid URL."]),
                );
            }
            _ => {}
        }
        if let Some(v) = filled("timeout") {
            let n = v
                .as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()));
            match n {
                None => {
                    errors.insert(
                        "metadata.timeout".into(),
                        json!(["The metadata.timeout field must be an integer."]),
                    );
                }
                Some(n) if n < 1 => {
                    errors.insert(
                        "metadata.timeout".into(),
                        json!(["The metadata.timeout field must be at least 1."]),
                    );
                }
                Some(n) if n > 300 => {
                    errors.insert(
                        "metadata.timeout".into(),
                        json!(["The metadata.timeout field must not be greater than 300."]),
                    );
                }
                _ => {}
            }
        }
    }
    if provider == "infisical" && filled("client_id").is_none() {
        errors.insert(
            "metadata.client_id".into(),
            json!(["The metadata.client id field is required."]),
        );
    }
}

fn capabilities_errors(
    body: &Value,
    provider: &str,
    errors: &mut Map<String, Value>,
) -> Vec<String> {
    let caps: Vec<String> = body
        .get("capabilities")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if caps.is_empty() {
        errors.insert(
            "capabilities".into(),
            json!(["Select at least one capability."]),
        );
    } else if caps.iter().any(|c| c == "tunnel") && !caps.iter().any(|c| c == "dns") {
        errors.insert(
            "capabilities".into(),
            json!(["Cloudflare Tunnel also needs the DNS capability."]),
        );
    }
    let allowed: &[&str] = if provider == "cloudflare" {
        &["dns", "tunnel"]
    } else {
        &["secrets"]
    };
    for (i, c) in caps.iter().enumerate() {
        if !allowed.contains(&c.as_str()) {
            errors.insert(
                format!("capabilities.{i}"),
                json!([format!("The selected capabilities.{i} is invalid.")]),
            );
        }
    }
    caps
}

fn filtered_metadata(body: &Value) -> Map<String, Value> {
    body.get("metadata")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter(|(_, v)| !blank(Some(v)))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn integration_audit(ctx: &AuthCtx, uuid: &Value, name: &str, provider: &str) -> Value {
    json!({
        "team_id": ctx.team_id,
        "integration_token_uuid": uuid,
        "integration_token_name": name,
        "provider": provider,
    })
}

/// `IntegrationTokenForm::addToken`.
async fn add_integration_token(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Value>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let provider = body_str(&body, "provider");
    let name = body_str(&body, "name");
    let token = body_str(&body, "token");
    let mut errors = Map::new();
    if !["cloudflare", "doppler", "infisical", "vault"].contains(&provider) {
        errors.insert(
            "provider".into(),
            json!(["The selected provider is invalid."]),
        );
    }
    if name.trim().is_empty() {
        errors.insert("name".into(), json!(["The name field is required."]));
    } else if name.chars().count() > 255 {
        errors.insert(
            "name".into(),
            json!(["The name field must not be greater than 255 characters."]),
        );
    }
    if token.trim().is_empty() {
        errors.insert("token".into(), json!(["The token field is required."]));
    } else if provider == "doppler" && !(token.starts_with("dp.st.") || token.starts_with("dp.sa."))
    {
        errors.insert(
            "token".into(),
            json!(["Use a Doppler service token (dp.st.*) or service account token (dp.sa.*)."]),
        );
    }
    let caps = capabilities_errors(&body, provider, &mut errors);
    let mut metadata = filtered_metadata(&body);
    metadata_errors(provider, &metadata, &mut errors);
    if !errors.is_empty() {
        return Ok(field_errors(errors));
    }
    if provider == "cloudflare" {
        if !body
            .get("automatic_dns")
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            metadata.insert("automatic_dns".into(), json!(false));
        }
        if body
            .get("proxied_dns")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            metadata.insert("proxied_dns".into(), json!(true));
        }
    }
    match verify_with_capabilities(&state.db, provider, token, &caps, &metadata).await {
        Ok(true) => {}
        Ok(false) => {
            return Ok(toast_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                error_message(provider),
            ));
        }
        Err(message) => return Ok(toast_error(StatusCode::UNPROCESSABLE_ENTITY, message)),
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let uuid = coolify_domain::new_public_id();
    let cipher = enc.encrypt_string(token);
    let caps_json = coolify_compat::php_json::encode(&json!(caps));
    let meta_json = (!metadata.is_empty())
        .then(|| coolify_compat::php_json::encode(&Value::Object(metadata.clone())));
    it::create(
        &state.db,
        &it::NewToken {
            uuid: &uuid,
            team_id: ctx.team_id,
            provider,
            name,
            token_encrypted: &cipher,
            capabilities: &caps_json,
            metadata: meta_json.as_deref(),
        },
        state.now(),
    )
    .await?;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.integration_token.created",
        integration_audit(&ctx, &json!(uuid), name, provider),
        "info",
    )
    .await;
    Ok(ok(json!({"message": "Integration token added successfully."})).into_response())
}

/// `IntegrationTokenValidator::validate`.
async fn verify_with_capabilities(
    db: &coolify_db::Db,
    provider: &str,
    token: &str,
    caps: &[String],
    metadata: &Map<String, Value>,
) -> Result<bool, String> {
    if provider == "cloudflare" {
        use coolify_jobs::integrations::cloudflare;
        // `catch (ConnectionException)`: `connectionErrorMessage`.
        return cloudflare::validate_token(token, caps)
            .await
            .map_err(|_| cloudflare::connection_error_message(provider));
    }
    verify(db, provider, token, metadata).await
}

/// `baseUrlForAudit`.
fn base_url_for_audit(v: Option<&Value>) -> Value {
    let Some(url) = v.and_then(Value::as_str).filter(|u| !u.is_empty()) else {
        return Value::Null;
    };
    let Some((scheme, rest)) = url.split_once("://") else {
        return json!("[invalid-url]");
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest[..end].rsplit('@').next().unwrap_or_default();
    let path_end = rest[end..].find(['?', '#']).map_or(rest.len(), |i| end + i);
    json!(format!("{scheme}://{authority}{}", &rest[end..path_end]))
}

/// `IntegrationTokenEditor::save`.
async fn update_integration_token(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let Some(r) = it::find(&state.db, ctx.team_id, &uuid).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let provider = text(&r, "provider").unwrap_or_default().to_owned();
    let name = body_str(&body, "name");
    let new_token = body_str(&body, "new_token");
    let mut errors = Map::new();
    if name.trim().is_empty() {
        errors.insert("name".into(), json!(["The name field is required."]));
    } else if name.chars().count() > 255 {
        errors.insert(
            "name".into(),
            json!(["The name field must not be greater than 255 characters."]),
        );
    }
    let caps = capabilities_errors(&body, &provider, &mut errors);
    let mut metadata = filtered_metadata(&body);
    metadata_errors(&provider, &metadata, &mut errors);
    if !errors.is_empty() {
        return Ok(field_errors(errors));
    }
    if provider == "cloudflare" {
        if body
            .get("automatic_dns")
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            metadata.remove("automatic_dns");
        } else {
            metadata.insert("automatic_dns".into(), json!(false));
        }
        if body
            .get("proxied_dns")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            metadata.insert("proxied_dns".into(), json!(true));
        } else {
            metadata.remove("proxied_dns");
        }
    }
    let stored = parse_json_column(&r, "metadata");
    let stored_map = stored.as_object().cloned().unwrap_or_default();
    let fields: &[&str] = match provider.as_str() {
        "vault" => &["base_url", "namespace"],
        "infisical" => &["base_url", "client_id"],
        _ => &[],
    };
    let as_text = |m: &Map<String, Value>, k: &str| match m.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(v) => v.to_string(),
    };
    let changed: Vec<&str> = fields
        .iter()
        .copied()
        .filter(|k| as_text(&stored_map, k) != as_text(&metadata, k))
        .collect();
    let new_filled = !new_token.trim().is_empty();
    if !changed.is_empty() && !new_filled {
        let m = match provider.as_str() {
            "infisical" => {
                "Enter the client secret again when you change the base URL or the client ID."
            }
            "vault" => "Enter the token again when you change the base URL or the namespace.",
            _ => "Enter the token again when you change the connection settings.",
        };
        let mut e = Map::new();
        e.insert("new_token".into(), json!([m]));
        return Ok(field_errors(e));
    }
    let mut sorted_new = caps.clone();
    sorted_new.sort();
    let mut sorted_old: Vec<String> = parse_json_column(&r, "capabilities")
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    sorted_old.sort();
    let caps_changed = sorted_new != sorted_old;
    let meta_changed = Value::Object(metadata.clone()) != Value::Object(stored_map.clone());
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    if new_filled || caps_changed || meta_changed {
        let current = if new_filled {
            new_token.to_owned()
        } else {
            text(&r, "token")
                .and_then(|c| enc.decrypt_string(c).ok())
                .unwrap_or_default()
        };
        match verify_with_capabilities(&state.db, &provider, &current, &caps, &metadata).await {
            Ok(true) => {}
            Ok(false) => {
                return Ok(toast_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    error_message(&provider),
                ));
            }
            Err(message) => return Ok(toast_error(StatusCode::UNPROCESSABLE_ENTITY, message)),
        }
    }
    let id = coolify_db::database_parts::id_of(&r);
    let caps_json = coolify_compat::php_json::encode(&json!(caps));
    let meta_json = (!metadata.is_empty())
        .then(|| coolify_compat::php_json::encode(&Value::Object(metadata.clone())));
    sqlx::query(
        "UPDATE integration_tokens SET name = $1, capabilities = $2::json, metadata = $3::json, \
         token = COALESCE($4, token), updated_at = $5 WHERE id = $6",
    )
    .bind(name)
    .bind(&caps_json)
    .bind(meta_json)
    .bind(new_filled.then(|| enc.encrypt_string(new_token)))
    .bind(state.now())
    .bind(id)
    .execute(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    let mut context = integration_audit(&ctx, &json!(uuid), name, &provider);
    context["rotated"] = json!(new_filled);
    context["connection_changed_fields"] = json!(changed);
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.integration_token.updated",
        context,
        "info",
    )
    .await;
    if changed.contains(&"base_url") {
        let mut context = integration_audit(&ctx, &json!(uuid), name, &provider);
        context["previous_base_url"] = base_url_for_audit(stored_map.get("base_url"));
        context["base_url"] = base_url_for_audit(metadata.get("base_url"));
        crate::audit::log(
            &state,
            &ctx,
            &meta,
            "ui.integration_token.base_url_changed",
            context,
            "warning",
        )
        .await;
    }
    Ok(ok(json!({"message": "Integration token updated successfully."})).into_response())
}

/// `IntegrationTokenEditor::delete`.
async fn delete_integration_token(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
) -> ApiResult {
    let Some(r) = it::find(&state.db, ctx.team_id, &uuid).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let id = coolify_db::database_parts::id_of(&r);
    let links: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM secret_manager_links WHERE integration_token_id = $1",
    )
    .bind(id)
    .fetch_one(state.db.pool())
    .await
    .unwrap_or(0);
    if links > 0 {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "This token is used by one or more resources as a secret manager source. Remove those links first.",
        ));
    }
    let records: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM managed_dns_records WHERE integration_token_id = $1",
    )
    .bind(id)
    .fetch_one(state.db.pool())
    .await
    .unwrap_or(0);
    if records > 0 {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "This token manages DNS records. Remove those domains or records first.",
        ));
    }
    it::delete(&state.db, id).await?;
    let provider = text(&r, "provider").unwrap_or_default();
    let name = text(&r, "name").unwrap_or_default();
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "ui.integration_token.deleted",
        integration_audit(&ctx, &json!(uuid), name, provider),
        "info",
    )
    .await;
    Ok(ok(json!({"message": "Integration token deleted successfully."})).into_response())
}

/// `IntegrationTokenEditor::refreshZones`.
async fn refresh_zones(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let Some(r) = it::find(&state.db, ctx.team_id, &uuid).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let token = text(&r, "token")
        .and_then(|c| enc.decrypt_string(c).ok())
        .unwrap_or_default();
    let id = coolify_db::database_parts::id_of(&r);
    match coolify_jobs::integrations::cloudflare::sync_zones(&state.db, id, &token, state.now())
        .await
    {
        Ok(_) => {
            let count = coolify_db::managed_dns::zones_of_token(&state.db, id)
                .await?
                .len();
            Ok(ok(json!({
                "message": format!("Cloudflare zones refreshed. {count} accessible zones found."),
            }))
            .into_response())
        }
        Err(message) => Ok(toast_error(StatusCode::UNPROCESSABLE_ENTITY, message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_for_audit_strips_credentials() {
        assert_eq!(
            base_url_for_audit(Some(&json!("https://u:p@vault.test:8200/a?x=1#f"))),
            json!("https://vault.test:8200/a")
        );
        assert_eq!(
            base_url_for_audit(Some(&json!("nope"))),
            json!("[invalid-url]")
        );
        assert_eq!(base_url_for_audit(None), Value::Null);
    }
}
