//! `Project\Shared\SecretManagerLinks` and `HasSecretManagerAutocomplete`. Secret values never leave
//! this module: a fetch is reduced to its key names.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use coolify_db::resource_envs as re;
use coolify_db::secret_managers as store;
use coolify_jobs::integrations::secret_managers::{self as provider, Link};
use serde_json::{Map, Value, json};

use super::{Class, Res, find};
use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::state::AppState;
use crate::ui_notifications::{toast_error, unauthorized};

type P = Path<(String, String)>;

const KEYS_CACHE_SECONDS: u64 = 60;
const KEYS_REQUESTS_PER_MINUTE: usize = 10;

pub fn routes() -> Router<AppState> {
    let b = "/ui/resources/{kind}/{uuid}/secret-manager";
    Router::new()
        .route(b, get(show).put(update_token).delete(remove_source))
        .route(&format!("{b}/settings"), post(save_settings))
        .route(&format!("{b}/keys"), post(load_keys))
        .route(&format!("{b}/references"), post(add_reference))
        .route(&format!("{b}/import"), post(import_all))
        .route(&format!("{b}/autocomplete"), get(autocomplete))
}

/// `str(class_basename($this->resource))->snake()`.
fn resource_type(res: &Res) -> String {
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

/// `auditSecretManagerAction`.
async fn audit(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    res: &Res,
    action: &str,
    extra: Value,
) {
    let t = resource_type(res);
    let mut context = Map::new();
    context.insert("team_id".into(), json!(res.team_id));
    context.insert(format!("{t}_uuid"), json!(res.uuid));
    context.insert(format!("{t}_name"), json!(res.name));
    if let Some(extra) = extra.as_object() {
        for (k, v) in extra {
            context.insert(k.clone(), v.clone());
        }
    }
    crate::audit::log(
        state,
        ctx,
        meta,
        &format!("ui.{t}.secret_manager.{action}"),
        Value::Object(context),
        "info",
    )
    .await;
}

/// Sections on service sub-resources have no source; every other resource uses `HasSecretManager`.
fn supported(res: &Res) -> Result<(), ApiError> {
    match res.class {
        Class::ServiceApplication | Class::ServiceDatabase => {
            Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."))
        }
        _ => Ok(()),
    }
}

async fn link_of(state: &AppState, res: &Res) -> Result<Option<Link>, ApiError> {
    provider::link_of(&state.db, &res.morph, res.id)
        .await
        .map_err(|_| ApiError::Internal)
}

/// `$link->settings`, as the form binds it.
fn settings_json(link: &Option<Link>) -> Value {
    link.as_ref()
        .map_or(json!({}), |l| Value::Object(l.settings.clone()))
}

async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): P,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    supported(&res)?;
    let link = link_of(&state, &res).await?;
    let tokens = store::tokens_of_team(&state.db, res.team_id).await?;
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    Ok(ok(json!({
        "linked": link.is_some(),
        "can_update": ctx.is_team_admin(),
        "available_tokens": tokens
            .iter()
            .map(|t| json!({
                "value": t.uuid,
                "label": format!("{} ({})", t.name, provider::provider_name(&t.provider)),
            }))
            .collect::<Vec<_>>(),
        "integration_token_uuid": link.as_ref().map(|l| l.token_uuid.clone()),
        "provider": link.as_ref().map(|l| l.provider.clone()),
        "provider_name": link.as_ref().map(Link::provider_name),
        "token_name": link.as_ref().map(|l| l.token_name.clone()),
        "source_summary": link.as_ref().map(Link::summary),
        "doppler_token_type": link.as_ref().and_then(|l| l.doppler_type(enc)),
        "settings": settings_json(&link),
    }))
    .into_response())
}

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

/// `array_filter(data_get($validated, 'settings', []), fn ($value) => filled($value))` for the
/// provider's fields, as `settings` of the link; `None` when nothing is left.
fn filtered_settings(fields: &[&str], body: &Value) -> Option<Map<String, Value>> {
    let input = body.get("settings").and_then(Value::as_object);
    let mut out = Map::new();
    for f in fields {
        if let Some(v) = input.and_then(|m| m.get(*f)).and_then(Value::as_str)
            && !v.trim().is_empty()
        {
            out.insert((*f).to_owned(), json!(v.trim()));
        }
    }
    (!out.is_empty()).then_some(out)
}

/// `rules()`: the provider's required and optional setting fields.
fn setting_rules(
    provider: &str,
    doppler_type: Option<&str>,
) -> (&'static [&'static str], &'static [&'static str]) {
    match provider {
        "doppler" if doppler_type == Some("service_account") => (&["project", "config"], &[]),
        "infisical" => (&["project_id", "environment"], &["secret_path"]),
        "vault" => (&["mount", "path"], &[]),
        _ => (&[], &[]),
    }
}

fn encode_settings(settings: Option<&Map<String, Value>>) -> Option<String> {
    settings.map(|m| coolify_compat::php_json::encode(&Value::Object(m.clone())))
}

/// `updatedIntegrationTokenUuid`.
async fn update_token(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    supported(&res)?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let wanted = body
        .get("integration_token_uuid")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let token = if wanted.is_empty() {
        None
    } else {
        store::token_in_team(&state.db, res.team_id, wanted).await?
    };
    let Some(token) = token.filter(|t| {
        serde_json::from_str::<Vec<String>>(&t.capabilities)
            .is_ok_and(|c| c.iter().any(|x| x == "secrets"))
    }) else {
        return Ok(ok(json!({"saved": false})).into_response());
    };
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let new_type = if token.provider == "doppler" {
        enc.decrypt_string(&token.token_encrypted)
            .ok()
            .and_then(|t| provider::doppler_token_type(&t))
    } else {
        None
    };
    let link = link_of(&state, &res).await?;
    // Another provider or Doppler token type starts with empty settings.
    let keep = link
        .as_ref()
        .is_some_and(|l| l.provider == token.provider && l.doppler_type(enc) == new_type);
    let (required, optional) = setting_rules(&token.provider, new_type);
    let fields: Vec<&str> = required.iter().chain(optional).copied().collect();
    let settings = if keep {
        filtered_settings(&fields, &json!({"settings": settings_json(&link)}))
    } else {
        None
    };
    store::update_or_create_link(
        &state.db,
        &res.morph,
        res.id,
        token.id,
        encode_settings(settings.as_ref()).as_deref(),
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
        "saved": true,
        "message": "Secret manager source saved. References resolve at the next deployment.",
    }))
    .into_response())
}

/// `saveSettings`.
async fn save_settings(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    supported(&res)?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let Some(link) = link_of(&state, &res).await? else {
        return Ok(ok(json!({})).into_response());
    };
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let (required, optional) = setting_rules(&link.provider, link.doppler_type(enc));
    let input = body.get("settings").and_then(Value::as_object);
    let mut errors = Map::new();
    for f in required {
        let filled = input
            .and_then(|m| m.get(*f))
            .and_then(Value::as_str)
            .is_some_and(|v| !v.trim().is_empty());
        if !filled {
            errors.insert(
                format!("settings.{f}"),
                json!([format!(
                    "The settings.{} field is required.",
                    f.replace('_', " ")
                )]),
            );
        }
    }
    if !errors.is_empty() {
        return Ok(field_errors(errors));
    }
    let fields: Vec<&str> = required.iter().chain(optional).copied().collect();
    let settings = filtered_settings(&fields, &body);
    let link_row = store::link_with_token(&state.db, &res.morph, res.id)
        .await?
        .ok_or(ApiError::Internal)?;
    store::update_link_settings(
        &state.db,
        link_row.link_id,
        encode_settings(settings.as_ref()).as_deref(),
        state.now(),
    )
    .await?;
    audit(&state, &ctx, &meta, &res, "settings_updated", json!({})).await;
    Ok(ok(json!({"message": "Secret manager settings saved."})).into_response())
}

/// `removeSource`.
async fn remove_source(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    supported(&res)?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let link = link_of(&state, &res).await?;
    store::delete_link(&state.db, &res.morph, res.id).await?;
    audit(
        &state,
        &ctx,
        &meta,
        &res,
        "source_removed",
        json!({
            "integration_token_uuid": link.as_ref().map(|l| l.token_uuid.clone()),
            "provider": link.as_ref().map(|l| l.provider.clone()),
        }),
    )
    .await;
    Ok(ok(json!({
        "message": "Secret manager source removed. Existing {{vault.*}} references will fail the next deployment until they are removed too.",
    }))
    .into_response())
}

/// `array_keys($this->link->fetchSecrets())`, sorted.
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

/// `loadKeys`.
async fn load_keys(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    supported(&res)?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let Some(link) = link_of(&state, &res).await? else {
        return Ok(ok(json!({"keys": [], "loaded": false})).into_response());
    };
    match fetch_keys(&state, &res, &link).await {
        Ok(keys) => {
            audit(
                &state,
                &ctx,
                &meta,
                &res,
                "keys_viewed",
                json!({"key_count": keys.len()}),
            )
            .await;
            let rows: Vec<Value> = keys
                .iter()
                .map(|k| json!({"key": k, "valid": valid_key(k)}))
                .collect();
            Ok(ok(json!({"keys": rows, "loaded": true})).into_response())
        }
        Err(e) => Ok(toast_error(
            StatusCode::BAD_GATEWAY,
            format!("Could not fetch keys: {e}"),
        )),
    }
}

/// `environment_variables()->create(['key' => $key, 'value' => '{{vault.'.$key.'}}'])`.
async fn create_reference(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    res: &Res,
    key: &str,
) -> Result<(), ApiError> {
    super::envs::create_plain(state, ctx, meta, res, key, &format!("{{{{vault.{key}}}}}")).await
}

/// `addReference`.
async fn add_reference(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    supported(&res)?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let key = body
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let Some(link) = link_of(&state, &res).await? else {
        return Ok(ok(json!({"added": false})).into_response());
    };
    // `in_array($key, $this->keys, true)`: only a key the source has.
    let keys = match fetch_keys(&state, &res, &link).await {
        Ok(k) => k,
        Err(e) => {
            return Ok(toast_error(
                StatusCode::BAD_GATEWAY,
                format!("Could not fetch keys: {e}"),
            ));
        }
    };
    if !keys.contains(&key) {
        return Ok(ok(json!({"added": false})).into_response());
    }
    if !valid_key(&key) {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{key} is not a valid variable name."),
        ));
    }
    if re::by_key(&state.db, &res.morph, res.id, false, &key)
        .await?
        .is_some()
    {
        return Ok(toast_error(
            StatusCode::CONFLICT,
            format!("A variable with the key {key} already exists."),
        ));
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
    Ok(ok(json!({
        "added": true,
        "message": format!("Added {key} as {{{{vault.{key}}}}}."),
    }))
    .into_response())
}

/// `importResultMessage`.
fn import_message(imported: &[String], skipped: &[String]) -> String {
    if imported.is_empty() && skipped.is_empty() {
        return "All remote keys already exist as variables.".to_owned();
    }
    let message = if imported.is_empty() {
        "No keys imported.".to_owned()
    } else {
        format!(
            "Imported {} keys as {{{{vault.KEY}}}} references.",
            imported.len()
        )
    };
    if skipped.is_empty() {
        return message;
    }
    let shown: Vec<String> = skipped
        .iter()
        .take(10)
        .map(|k| {
            k.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace('\'', "&#039;")
        })
        .collect();
    let more = skipped.len() - shown.len();
    let names = format!(
        "{}{}",
        shown.join(", "),
        if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        }
    );
    if skipped.len() == 1 {
        format!("{message} Skipped 1 key that is not a valid variable name: {names}.")
    } else {
        format!(
            "{message} Skipped {} keys that are not valid variable names: {names}.",
            skipped.len()
        )
    }
}

/// `importAll` (`importMissingReferences`).
async fn import_all(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path((kind, uuid)): P,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    supported(&res)?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let Some(link) = link_of(&state, &res).await? else {
        return Ok(ok(json!({})).into_response());
    };
    let keys = match fetch_keys(&state, &res, &link).await {
        Ok(k) => k,
        // `handleError($e, $this)`.
        Err(e) => return Ok(toast_error(StatusCode::BAD_GATEWAY, e)),
    };
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
    Ok(ok(json!({"message": import_message(&imported, &skipped)})).into_response())
}

type KeysCache = Mutex<HashMap<String, (Instant, Vec<String>)>>;
type UserHits = Mutex<HashMap<i64, Vec<Instant>>>;
static KEYS_CACHE: LazyLock<KeysCache> = LazyLock::new(KeysCache::default);
static USER_HITS: LazyLock<UserHits> = LazyLock::new(UserHits::default);

/// `fetchSecretManagerKeys`: only key names are cached; another token or other settings use
/// another entry.
async fn autocomplete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): P,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    supported(&res)?;
    let Some(link) = link_of(&state, &res).await? else {
        return Ok(ok(json!({"keys": []})).into_response());
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let cache_key = format!(
        "{}:{}:{}:{}",
        res.morph,
        res.id,
        link.token_id,
        Value::Object(link.settings.clone())
    );
    if let Ok(cache) = KEYS_CACHE.lock()
        && let Some((at, keys)) = cache.get(&cache_key)
        && at.elapsed() < Duration::from_secs(KEYS_CACHE_SECONDS)
    {
        return Ok(ok(json!({"keys": keys})).into_response());
    }
    if let Ok(mut hits) = USER_HITS.lock() {
        let list = hits.entry(ctx.user_id).or_default();
        list.retain(|t| t.elapsed() < Duration::from_secs(60));
        if list.len() >= KEYS_REQUESTS_PER_MINUTE {
            let wait = list
                .first()
                .map_or(60, |t| 60 - t.elapsed().as_secs().min(59));
            return Ok(toast_error(
                StatusCode::TOO_MANY_REQUESTS,
                format!("Too many secret manager key requests. Try again in {wait} seconds."),
            ));
        }
        list.push(Instant::now());
    }
    let Ok(keys) = fetch_keys(&state, &res, &link).await else {
        return Ok(toast_error(
            StatusCode::BAD_GATEWAY,
            "Unable to fetch secret manager keys.",
        ));
    };
    if let Ok(mut cache) = KEYS_CACHE.lock() {
        cache.insert(cache_key, (Instant::now(), keys.clone()));
    }
    Ok(ok(json!({"keys": keys})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages() {
        assert_eq!(
            import_message(&[], &[]),
            "All remote keys already exist as variables."
        );
        assert_eq!(
            import_message(&["A".into()], &[]),
            "Imported 1 keys as {{vault.KEY}} references."
        );
        assert_eq!(
            import_message(&[], &["a-b".into()]),
            "No keys imported. Skipped 1 key that is not a valid variable name: a-b."
        );
    }

    #[test]
    fn keys() {
        assert!(valid_key("A_b.c"));
        assert!(!valid_key("db-password"));
        assert!(!valid_key("1A"));
    }
}
