//! `IntegrationTokensController` (v4.6.0: list, show, create, update, delete and the Cloudflare zone
//! sync) and `IntegrationTokenValidator`.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::integration_tokens as db;
use coolify_db::records::Record;
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::input::validated_json;
use crate::integration_token_actions::{
    self as actions, ActionError, Messages, automatic_dns_enabled, json_column, proxied_dns_enabled,
};
use crate::response::{ApiError, ApiResult, created, ok};
use crate::state::AppState;

pub const SECRET_MANAGER_PROVIDERS: &[&str] = &["doppler", "infisical", "vault"];

/// `IntegrationTokenValidator::errorMessage`.
pub fn error_message(provider: &str) -> &'static str {
    match provider {
        "cloudflare" => {
            "The token could not access the selected Cloudflare capabilities. Check its permissions and zone resources."
        }
        "doppler" => "The Doppler token could not be verified. Check the token and its access.",
        "infisical" => {
            "Infisical login failed. Check the base URL, the client ID, and the client secret."
        }
        "vault" => {
            "The Vault token could not be verified. Check the base URL, the namespace, and the token."
        }
        _ => "The token could not be verified.",
    }
}

/// `IntegrationTokenValidator::validate`: `Err` is the `ValidationException` of a base URL that
/// `SafeExternalUrl` refuses.
pub async fn verify(
    db: &coolify_db::Db,
    provider: &str,
    token: &str,
    meta: &Map<String, Value>,
) -> Result<bool, String> {
    coolify_jobs::integrations::secret_managers::validate(db, provider, token, meta, "").await
}

fn message(status: StatusCode, text: impl Into<String>) -> ApiError {
    ApiError::Status(status, json!({"message": text.into()}))
}

/// `$errors->add($field, 'This field is not allowed.')`.
fn not_allowed(errors: &mut Map<String, Value>, field: &str) {
    let entry = errors.entry(field.to_owned()).or_insert_with(|| json!([]));
    if let Value::Array(list) = entry {
        list.push(json!("This field is not allowed."));
    }
}

/// A boolean request value as `(bool)` would read it after the `boolean` rule.
fn truthy(v: &Value) -> bool {
    matches!(v, Value::Bool(true)) || v == &json!(1) || v == &json!("1") || v == &json!("true")
}

/// The `catch` blocks of the controller.
fn action_error(e: ActionError) -> ApiError {
    match e {
        ActionError::Validation(errors) => ApiError::Validation(errors),
        // `connectionErrorResponse()`.
        ActionError::Connection(m) => message(StatusCode::BAD_GATEWAY, m),
        ActionError::Runtime(m) => message(StatusCode::BAD_REQUEST, m),
        ActionError::Api(e) => e,
    }
}

/// `dns_zones_count` (`withCount('dnsZones')`).
async fn zones_count(state: &AppState, id: i64) -> Result<i64, ApiError> {
    sqlx::query_scalar("SELECT count(*) FROM dns_provider_zones WHERE integration_token_id = $1")
        .bind(id)
        .fetch_one(state.db.pool())
        .await
        .map_err(|_| ApiError::Internal)
}

fn timestamp(r: &Record, k: &str) -> Value {
    r.get(k).and_then(Value::as_str).map_or(Value::Null, |t| {
        json!(coolify_db::records::laravel_datetime(t))
    })
}

/// `tokenData()`: the fields that are safe to return. The secret and the provider settings (a base
/// URL can carry credentials) are left out.
async fn token_data(state: &AppState, r: &Record) -> Result<Value, ApiError> {
    let provider = r
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let capabilities = match json_column(r, "capabilities") {
        Value::Null => json!([]),
        v => v,
    };
    let mut data = Map::new();
    data.insert("uuid".into(), r.get("uuid").cloned().unwrap_or(Value::Null));
    data.insert("name".into(), r.get("name").cloned().unwrap_or(Value::Null));
    data.insert("provider".into(), json!(provider));
    data.insert("capabilities".into(), capabilities);
    if provider == "cloudflare" {
        let metadata = json_column(r, "metadata");
        let id = coolify_db::database_parts::id_of(r);
        data.insert(
            "automatic_dns".into(),
            json!(automatic_dns_enabled(provider, &metadata)),
        );
        data.insert(
            "proxied_dns".into(),
            json!(proxied_dns_enabled(provider, &metadata)),
        );
        data.insert(
            "dns_zones_count".into(),
            json!(zones_count(state, id).await?),
        );
        data.insert(
            "zones_synced_at".into(),
            metadata
                .get("zones_synced_at")
                .cloned()
                .unwrap_or(Value::Null),
        );
    }
    data.insert("created_at".into(), timestamp(r, "created_at"));
    data.insert("updated_at".into(), timestamp(r, "updated_at"));
    Ok(Value::Object(data))
}

/// `findToken()` then `authorize($ability, $token)`.
async fn find_token(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<Record, ApiError> {
    let r = db::find(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Integration token not found.",
        ))?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    Ok(r)
}

/// `$request->json()->all()`: the query string does not count.
fn json_body(headers: &HeaderMap, uri: Uri, body: &Bytes) -> Result<Map<String, Value>, ApiError> {
    validated_json(headers, &uri, body)?;
    let no_query: Uri = uri.path().parse().unwrap_or(uri);
    Ok(validated_json(headers, &no_query, body)?.0)
}

/// `GET /security/integration-tokens`.
pub async fn index(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let mut rows = db::list(&state.db, ctx.team_id).await?;
    // `latest()`: newest first.
    rows.sort_by(|a, b| {
        let at = |r: &Record| {
            r.get("created_at")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        at(b).cmp(&at(a))
    });
    let mut list = Vec::with_capacity(rows.len());
    for r in &rows {
        list.push(token_data(&state, r).await?);
    }
    Ok(ok(Value::Array(list)).into_response())
}

/// `GET /security/integration-tokens/{uuid}`.
pub async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let r = find_token(&state, &ctx, &uuid).await?;
    Ok(ok(token_data(&state, &r).await?).into_response())
}

fn string_list(v: Option<&Value>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in v
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        // `array_values(array_unique(...))`.
        if !out.iter().any(|o| o == c) {
            out.push(c.to_owned());
        }
    }
    out
}

/// `POST /security/integration-tokens`.
pub async fn store(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let mut body = json_body(&headers, uri, &body)?;
    let provider = body
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let cloudflare = provider == "cloudflare";
    // `$body['capabilities'] ??= ...`.
    if body.get("capabilities").is_none_or(Value::is_null) {
        body.insert(
            "capabilities".into(),
            if cloudflare {
                json!(["dns"])
            } else {
                json!(["secrets"])
            },
        );
    }
    let mut errors = actions::rule_errors(&provider, &body, true, Messages::Api);
    actions::flag_errors(&provider, &body, &mut errors);
    for k in body.keys() {
        if ![
            "provider",
            "name",
            "token",
            "capabilities",
            "automatic_dns",
            "proxied_dns",
            "metadata",
        ]
        .contains(&k.as_str())
        {
            not_allowed(&mut errors, k);
        }
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let flag = |k: &str, default: bool| body.get(k).map_or(default, truthy);
    let metadata = body
        .get("metadata")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let uuid = actions::create(
        &state,
        ctx.team_id,
        &provider,
        body.get("name").and_then(Value::as_str).unwrap_or_default(),
        body.get("token")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        &string_list(body.get("capabilities")),
        &metadata,
        flag("automatic_dns", true),
        flag("proxied_dns", false),
    )
    .await
    .map_err(action_error)?;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "api.integration_token.created",
        json!({"team_id": ctx.team_id, "integration_token_uuid": uuid, "provider": provider}),
        "info",
    )
    .await;
    Ok(created(json!({"uuid": uuid})).into_response())
}

/// `PATCH /security/integration-tokens/{uuid}`: omitted fields keep their value.
pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let r = find_token(&state, &ctx, &uuid).await?;
    let body = json_body(&headers, uri, &body)?;
    let provider = r
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let stored_metadata = json_column(&r, "metadata")
        .as_object()
        .cloned()
        .unwrap_or_default();
    let given = |k: &str| body.get(k).filter(|v| !v.is_null()).cloned();
    let mut metadata = stored_metadata.clone();
    if let Some(Value::Object(m)) = body.get("metadata") {
        metadata.extend(m.clone());
    }
    let mut merged = Map::new();
    merged.insert(
        "name".into(),
        given("name").unwrap_or_else(|| r.get("name").cloned().unwrap_or(Value::Null)),
    );
    merged.insert("token".into(), given("token").unwrap_or(Value::Null));
    merged.insert(
        "capabilities".into(),
        given("capabilities").unwrap_or_else(|| json_column(&r, "capabilities")),
    );
    merged.insert("metadata".into(), Value::Object(metadata.clone()));
    let mut errors = actions::rule_errors(&provider, &merged, false, Messages::Api);
    let mut flags = Map::new();
    actions::flag_errors(&provider, &body, &mut flags);
    for (k, v) in flags {
        errors.entry(k).or_insert(v);
    }
    for k in body.keys() {
        if ![
            "name",
            "token",
            "capabilities",
            "automatic_dns",
            "proxied_dns",
            "metadata",
        ]
        .contains(&k.as_str())
        {
            not_allowed(&mut errors, k);
        }
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let stored_meta_value = Value::Object(stored_metadata);
    // `$body['automatic_dns'] ?? $token->automaticDnsEnabled()`.
    let flag = |k: &str, stored: bool| match body.get(k) {
        None | Some(Value::Null) => stored,
        Some(v) => truthy(v),
    };
    let name = merged
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let new_token = merged
        .get("token")
        .and_then(Value::as_str)
        .map(str::to_owned);
    actions::update(
        &state,
        &ctx,
        &meta,
        &r,
        actions::Update {
            name: &name,
            new_token: new_token.as_deref(),
            capabilities: string_list(merged.get("capabilities")),
            metadata,
            automatic_dns: flag(
                "automatic_dns",
                automatic_dns_enabled(&provider, &stored_meta_value),
            ),
            proxied_dns: flag(
                "proxied_dns",
                proxied_dns_enabled(&provider, &stored_meta_value),
            ),
            audit_source: "api",
        },
    )
    .await
    .map_err(action_error)?;
    let fresh = find_token(&state, &ctx, &uuid).await?;
    Ok(ok(token_data(&state, &fresh).await?).into_response())
}

/// `DELETE /security/integration-tokens/{uuid}`.
pub async fn destroy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
) -> ApiResult {
    let r = find_token(&state, &ctx, &uuid).await?;
    let id = coolify_db::database_parts::id_of(&r);
    if let Some(blocker) = actions::deletion_blocker(&state, id).await? {
        return Err(ApiError::Message(StatusCode::CONFLICT, blocker));
    }
    db::delete(&state.db, id).await?;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "api.integration_token.deleted",
        json!({
            "team_id": r.get("team_id"),
            "integration_token_uuid": r.get("uuid"),
            "integration_token_name": r.get("name"),
            "provider": r.get("provider"),
        }),
        "info",
    )
    .await;
    Ok(ok(json!({"message": "Integration token deleted."})).into_response())
}

/// `POST /security/integration-tokens/{uuid}/sync-zones`.
pub async fn sync_zones(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let r = find_token(&state, &ctx, &uuid).await?;
    let provider = r
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if provider != "cloudflare" {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Only Cloudflare tokens have DNS zones.",
        ));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let token = r
        .get("token")
        .and_then(Value::as_str)
        .and_then(|c| enc.decrypt_string(c).ok())
        .unwrap_or_default();
    let id = coolify_db::database_parts::id_of(&r);
    use coolify_jobs::integrations::cloudflare;
    match cloudflare::sync_zones_checked(&state.db, id, &token, state.now()).await {
        Ok(count) => Ok(ok(json!({"zones_count": count})).into_response()),
        Err(cloudflare::SyncZonesError::Connection(_)) => Err(message(
            StatusCode::BAD_GATEWAY,
            cloudflare::connection_error_message(provider),
        )),
        Err(cloudflare::SyncZonesError::Failed(m)) => Err(message(StatusCode::BAD_REQUEST, m)),
    }
}
