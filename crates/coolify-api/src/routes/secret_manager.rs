//! `SecretManagerController` (v4.6.0) and `ResourceSecretManager`: the secret manager source of an
//! application, a service or a database (`/{resource_type}/{uuid}/secret-manager`). It replaces
//! `ApplicationSecretManagerController`. Secret values never leave this module: a fetch is reduced
//! to its key names.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_compat::php_json;
use coolify_db::resource_envs as re;
use coolify_db::secret_managers as store;
use coolify_jobs::integrations::secret_managers::{self as provider, Link};
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::input::validated_json;
use crate::response::{ApiError, ApiResult, created, ok};
use crate::state::AppState;
use crate::ui_shared::Res;

/// `filled($value)`.
fn filled(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => true,
    }
}

fn message(status: StatusCode, text: impl Into<String>) -> ApiError {
    ApiError::Status(status, json!({"message": text.into()}))
}

/// `{resource_type}` of the request path (`applications`, `services` or `databases`).
fn resource_type(uri: &Uri) -> &'static str {
    let path = uri.path();
    let after = path.split_once("/api/v1/").map_or(path, |(_, rest)| rest);
    match after.split('/').next().unwrap_or_default() {
        "applications" => "applications",
        "services" => "services",
        _ => "databases",
    }
}

/// `findResource($request, $ability)`: the resource of the token team; `update` needs an admin.
async fn find_resource(
    state: &AppState,
    ctx: &AuthCtx,
    uri: &Uri,
    uuid: &str,
    update: bool,
) -> Result<Res, ApiError> {
    let (kind, name) = match resource_type(uri) {
        "applications" => ("application", "Application not found."),
        "services" => ("service", "Service not found."),
        _ => ("database", "Database not found."),
    };
    let res = match crate::ui_shared::find(state, ctx, kind, uuid).await {
        Ok(res) => res,
        Err(ApiError::Message(status, _)) if status == StatusCode::NOT_FOUND => {
            return Err(ApiError::Message(StatusCode::NOT_FOUND, name));
        }
        Err(e) => return Err(e),
    };
    if update && !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    Ok(res)
}

async fn source_link(state: &AppState, res: &Res) -> Result<Option<Link>, ApiError> {
    provider::link_of(&state.db, &res.morph, res.id)
        .await
        .map_err(|_| ApiError::Internal)
}

fn no_source() -> ApiError {
    ApiError::Message(
        StatusCode::NOT_FOUND,
        "The resource has no secret manager source.",
    )
}

/// `str(class_basename($resource))->snake()`.
fn snake_type(res: &Res) -> String {
    let base = res.morph.rsplit('\\').next().unwrap_or_default();
    let mut out = String::new();
    for (i, c) in base.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// `ResourceSecretManager::audit('api', ...)`.
async fn audit(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    res: &Res,
    action: &str,
    extra: Value,
) {
    let t = snake_type(res);
    let mut context = Map::new();
    context.insert("team_id".into(), json!(res.team_id));
    context.insert(format!("{t}_uuid"), json!(res.uuid));
    context.insert(format!("{t}_name"), json!(res.name));
    if let Value::Object(extra) = extra {
        context.extend(extra);
    }
    crate::audit::log(
        state,
        ctx,
        meta,
        &format!("api.{t}.secret_manager.{action}"),
        Value::Object(context),
        "info",
    )
    .await;
}

/// `$request->json()->all()` after `validateIncomingRequest`.
fn json_body(headers: &HeaderMap, uri: &Uri, body: &Bytes) -> Result<Map<String, Value>, ApiError> {
    validated_json(headers, uri, body)?;
    Ok(validated_json(headers, &Uri::from_static("/"), body)?.0)
}

/// `$link->settings` (`array` cast): null when empty.
fn settings_of(link: Option<&Link>) -> Value {
    match link {
        Some(l) if !l.settings.is_empty() => Value::Object(l.settings.clone()),
        _ => Value::Null,
    }
}

/// `GET /{resource_type}/{uuid}/secret-manager`: `sourceData()`.
pub async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    uri: Uri,
) -> ApiResult {
    let res = find_resource(&state, &ctx, &uri, &uuid, false).await?;
    let link = source_link(&state, &res).await?;
    // `availableTokens()`: without write access on the whole team, only the linked token.
    let tokens = store::tokens_of_team(&state.db, res.team_id).await?;
    let available: Vec<Value> = tokens
        .iter()
        .filter(|t| ctx.is_team_admin() || link.as_ref().is_some_and(|l| l.token_id == t.id))
        .map(|t| json!({"uuid": t.uuid, "name": t.name, "provider": t.provider}))
        .collect();
    Ok(ok(json!({
        "linked": link.is_some(),
        "integration_token_uuid": link.as_ref().map(|l| l.token_uuid.clone()),
        "provider": link.as_ref().map(|l| l.provider.clone()),
        "settings": settings_of(link.as_ref()),
        "available_tokens": available,
    }))
    .into_response())
}

/// `settingsRules($token)`: `(key, required)` in rule order.
fn settings_rules(
    provider_name: &str,
    doppler_type: Option<&str>,
) -> &'static [(&'static str, bool)] {
    match provider_name {
        "doppler" if doppler_type == Some("service_account") => {
            &[("project", true), ("config", true)]
        }
        "infisical" => &[
            ("project_id", true),
            ("environment", true),
            ("secret_path", false),
        ],
        "vault" => &[("mount", true), ("path", true)],
        _ => &[],
    }
}

fn add(errors: &mut Map<String, Value>, field: &str, text: impl Into<String>) {
    let text = Value::String(text.into());
    match errors.get_mut(field) {
        Some(Value::Array(list)) => list.push(text),
        _ => {
            errors.insert(field.to_owned(), Value::Array(vec![text]));
        }
    }
}

/// `PATCH /{resource_type}/{uuid}/secret-manager`.
pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let res = find_resource(&state, &ctx, &uri, &uuid, true).await?;
    let body = json_body(&headers, &uri, &body)?;
    let not_found = ApiError::Message(
        StatusCode::NOT_FOUND,
        "Secret manager integration token not found.",
    );
    let Some(token) = (match body.get("integration_token_uuid").and_then(Value::as_str) {
        Some(u) => store::token_in_team(&state.db, res.team_id, u).await?,
        None => None,
    }) else {
        return Err(not_found);
    };
    let capabilities: Vec<Value> = serde_json::from_str(&token.capabilities).unwrap_or_default();
    if !capabilities.iter().any(|c| c.as_str() == Some("secrets")) {
        return Err(not_found);
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let doppler_type = if token.provider == "doppler" {
        enc.decrypt_string(&token.token_encrypted)
            .ok()
            .and_then(|t| provider::doppler_token_type(&t))
    } else {
        None
    };
    let rules = settings_rules(&token.provider, doppler_type);
    // `validateBody()`: the rules in order, then the fields that are not allowed.
    let mut errors = Map::new();
    if !body.get("integration_token_uuid").is_some_and(filled) {
        add(
            &mut errors,
            "integration_token_uuid",
            "This field is required.",
        );
    }
    let settings_input = body.get("settings");
    if settings_input.is_some_and(|s| !s.is_object() && !s.is_array()) {
        add(
            &mut errors,
            "settings",
            "The settings field must be an array.",
        );
    }
    let given = settings_input.and_then(Value::as_object);
    for (key, required) in rules {
        let field = format!("settings.{key}");
        match given.and_then(|s| s.get(*key)) {
            v if v.is_none_or(|v| !filled(v)) => {
                if *required {
                    add(&mut errors, &field, "This field is required.");
                } else if v.is_some_and(|v| !v.is_null() && !v.is_string()) {
                    add(
                        &mut errors,
                        &field,
                        format!("The {field} field must be a string."),
                    );
                }
            }
            Some(Value::String(_)) => {}
            Some(_) => add(
                &mut errors,
                &field,
                format!("The {field} field must be a string."),
            ),
        }
    }
    for k in body.keys() {
        if !["integration_token_uuid", "settings"].contains(&k.as_str()) {
            add(&mut errors, k, "This field is not allowed.");
        }
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    // `Arr::only($body['settings'] ?? [], $settingKeys)`, then `link()` drops empty values.
    let mut settings = Map::new();
    if let Some(given) = given {
        for (key, _) in rules {
            if let Some(v) = given.get(*key).filter(|v| filled(v)) {
                settings.insert((*key).to_owned(), v.clone());
            }
        }
    }
    let settings = (!settings.is_empty()).then_some(Value::Object(settings));
    let encoded = settings.as_ref().map(php_json::encode);
    store::update_or_create_link(
        &state.db,
        &res.morph,
        res.id,
        token.id,
        encoded.as_deref(),
        state.now(),
    )
    .await?;
    audit(
        &state,
        &ctx,
        &meta,
        &res,
        "source_updated",
        json!({"integration_token_uuid": token.uuid, "provider": token.provider}),
    )
    .await;
    Ok(ok(json!({
        "integration_token_uuid": token.uuid,
        "provider": token.provider,
        "settings": settings,
    }))
    .into_response())
}

/// `DELETE /{resource_type}/{uuid}/secret-manager`.
pub async fn destroy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    uri: Uri,
) -> ApiResult {
    let res = find_resource(&state, &ctx, &uri, &uuid, true).await?;
    let Some(link) = source_link(&state, &res).await? else {
        return Err(no_source());
    };
    store::delete_link(&state.db, &res.morph, res.id).await?;
    audit(
        &state,
        &ctx,
        &meta,
        &res,
        "source_removed",
        json!({"integration_token_uuid": link.token_uuid, "provider": link.provider}),
    )
    .await;
    Ok(ok(json!({
        "message": "Secret manager source removed. Existing {{vault.*}} variables fail the next deployment until they are removed.",
    }))
    .into_response())
}

/// `array_keys($link->fetchSecrets())`.
async fn fetch_keys(state: &AppState, res: &Res, link: &Link) -> Result<Vec<String>, String> {
    let enc = state
        .encrypter
        .as_ref()
        .ok_or_else(|| "No application encryption key has been specified.".to_owned())?;
    let secrets = link.fetch(&state.db, enc, res.team_id, "").await?;
    Ok(secrets.into_keys().collect())
}

/// `ValidationPatterns::isValidEnvironmentVariableKey`.
fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

fn fetch_failed(e: &str) -> ApiError {
    message(
        StatusCode::BAD_REQUEST,
        format!("Could not fetch keys: {e}"),
    )
}

/// `GET /{resource_type}/{uuid}/secret-manager/keys`: `keyNames()`, sorted.
pub async fn keys(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    uri: Uri,
) -> ApiResult {
    let res = find_resource(&state, &ctx, &uri, &uuid, true).await?;
    let Some(link) = source_link(&state, &res).await? else {
        return Err(no_source());
    };
    let mut keys = fetch_keys(&state, &res, &link)
        .await
        .map_err(|e| fetch_failed(&e))?;
    keys.sort();
    audit(
        &state,
        &ctx,
        &meta,
        &res,
        "keys_viewed",
        json!({"key_count": keys.len()}),
    )
    .await;
    Ok(ok(json!({"keys": keys})).into_response())
}

/// `environment_variables()->create(['key' => $key, 'value' => '{{vault.'.$key.'}}'])`.
async fn create_reference(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    res: &Res,
    key: &str,
) -> Result<(), ApiError> {
    crate::ui_shared::envs::create_plain(
        state,
        ctx,
        meta,
        res,
        key,
        &format!("{{{{vault.{key}}}}}"),
    )
    .await
}

/// `POST /{resource_type}/{uuid}/secret-manager/references`.
pub async fn add_reference(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let res = find_resource(&state, &ctx, &uri, &uuid, true).await?;
    let Some(link) = source_link(&state, &res).await? else {
        return Err(no_source());
    };
    let body = json_body(&headers, &uri, &body)?;
    let mut errors = Map::new();
    match body.get("key") {
        v if v.is_none_or(|v| !filled(v)) => add(&mut errors, "key", "This field is required."),
        Some(Value::String(k)) if k.chars().count() > 255 => add(
            &mut errors,
            "key",
            "The key field must not be greater than 255 characters.",
        ),
        Some(Value::String(_)) => {}
        Some(_) => add(&mut errors, "key", "The key field must be a string."),
    }
    for k in body.keys() {
        if k != "key" {
            add(&mut errors, k, "This field is not allowed.");
        }
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let key = body
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let keys = fetch_keys(&state, &res, &link)
        .await
        .map_err(|e| fetch_failed(&e))?;
    let invalid = |text: String| {
        let mut errors = Map::new();
        errors.insert("key".into(), json!([text]));
        ApiError::Validation(errors)
    };
    if !keys.contains(&key) {
        return Err(invalid(format!(
            "{key} is not a key of the secret manager source."
        )));
    }
    if !valid_key(&key) {
        return Err(invalid(format!("{key} is not a valid variable name.")));
    }
    if re::by_key(&state.db, &res.morph, res.id, false, &key)
        .await?
        .is_some()
    {
        return Err(invalid(format!(
            "A variable with the key {key} already exists."
        )));
    }
    create_reference(&state, &ctx, &meta, &res, &key).await?;
    audit(
        &state,
        &ctx,
        &meta,
        &res,
        "reference_created",
        json!({"secret_key": key}),
    )
    .await;
    Ok(created(json!({"key": key, "value": format!("{{{{vault.{key}}}}}")})).into_response())
}

/// `POST /{resource_type}/{uuid}/secret-manager/import`: `importAll()`.
pub async fn import(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    uri: Uri,
) -> ApiResult {
    let res = find_resource(&state, &ctx, &uri, &uuid, true).await?;
    let Some(link) = source_link(&state, &res).await? else {
        return Err(no_source());
    };
    let keys = fetch_keys(&state, &res, &link)
        .await
        .map_err(|e| fetch_failed(&e))?;
    let (mut imported, mut skipped) = (Vec::new(), Vec::new());
    for key in keys {
        if !valid_key(&key) {
            skipped.push(key);
            continue;
        }
        if re::by_key(&state.db, &res.morph, res.id, false, &key)
            .await?
            .is_some()
        {
            continue;
        }
        create_reference(&state, &ctx, &meta, &res, &key).await?;
        imported.push(key);
    }
    audit(
        &state,
        &ctx,
        &meta,
        &res,
        "references_imported",
        json!({
            "key_count": imported.len(),
            "secret_keys": imported,
            "skipped_key_count": skipped.len(),
        }),
    )
    .await;
    Ok(ok(json!({"imported": imported, "skipped": skipped})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_type_of_paths() {
        let t = |p: &str| resource_type(&p.parse::<Uri>().unwrap_or_default());
        assert_eq!(t("/api/v1/applications/x/secret-manager"), "applications");
        assert_eq!(t("/api/v1/services/x/secret-manager/keys"), "services");
        assert_eq!(t("/api/v1/databases/x/secret-manager"), "databases");
        assert_eq!(
            t("/ui-api/api/v1/services/x/secret-manager/import"),
            "services"
        );
    }

    #[test]
    fn settings_rules_of_providers() {
        assert!(settings_rules("doppler", Some("service")).is_empty());
        assert_eq!(
            settings_rules("doppler", Some("service_account")),
            &[("project", true), ("config", true)]
        );
        assert_eq!(
            settings_rules("vault", None),
            &[("mount", true), ("path", true)]
        );
    }
}
