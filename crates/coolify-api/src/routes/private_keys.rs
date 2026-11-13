//! `SecurityController`: private keys (`/api/v1/security/keys`).
//!
//! `private_key` is stored with the Laravel `encrypted` cast, hidden from
//! responses unless `can_read_sensitive`, and `public_key` is appended
//! (derived from the private key, like `getPublicKeyAttribute`).
//! Policy (`PrivateKeyPolicy`): view = team member (team 0 keys: admin of
//! team 0), create/update/delete = admin or owner.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use coolify_compat::Encrypter;
use coolify_compat::sshkey::{KeyKind, format_private_key, generate, inspect, validation_error};
use coolify_db::private_keys::{self, KeyRow, KeyUpdate, NewKey};
use coolify_domain::ROOT_TEAM_ID;
use serde_json::{Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, validated_json};
use crate::response::{ApiError, ApiResult, created, ok};
use crate::serialize::serialize_record;
use crate::state::AppState;
use crate::validation::{Field, Rule, validate};

const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Private Key not found.");
/// `PrivateKey::validationError()` (4.7.5): `create_key` answers 422 with the reason (a key that
/// cannot be loaded, PuTTY format, DSA, RSA below 2048 bits, an unsupported key type).
const fn invalid(reason: &'static str) -> ApiError {
    ApiError::Message(StatusCode::UNPROCESSABLE_ENTITY, reason)
}

fn encrypter(state: &AppState) -> ApiResult<&Encrypter> {
    state.encrypter.as_ref().ok_or(ApiError::Message(
        StatusCode::SERVICE_UNAVAILABLE,
        "APP_KEY is not configured; private keys cannot be read or written.",
    ))
}

fn require_admin(ctx: &AuthCtx) -> Result<(), ApiError> {
    if ctx.is_team_admin() {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

/// `view`: team-0 (system) keys need admin of team 0; the key is already in
/// the token's team, so that is the token team's admin check.
fn can_view(ctx: &AuthCtx, key: &KeyRow) -> bool {
    key.team_id != ROOT_TEAM_ID || ctx.is_team_admin()
}

fn present(state: &AppState, ctx: &AuthCtx, key: &KeyRow) -> ApiResult<Value> {
    let enc = encrypter(state)?;
    let plain = enc.decrypt_string(&key.private_key_encrypted).map_err(|e| {
        tracing::error!(key = %key.uuid, error = %e, "cannot decrypt private key (APP_KEY mismatch?)");
        ApiError::Internal
    })?;
    let mut record = key.record.clone();
    let public = inspect(&plain)
        .map(|i| i.public_key)
        .unwrap_or_else(|| "Error loading private key".into());
    record.insert("public_key".into(), Value::String(public));
    if ctx.can_read_sensitive {
        record.insert("private_key".into(), Value::String(plain));
    }
    Ok(Value::Object(serialize_record(record)))
}

/// `$key->toArray()`: `private_key` stays hidden, `public_key` is appended, columns keep their order.
pub(crate) fn model_array(state: &AppState, key: &KeyRow) -> ApiResult<Value> {
    let enc = encrypter(state)?;
    let plain = enc
        .decrypt_string(&key.private_key_encrypted)
        .map_err(|_| ApiError::Internal)?;
    let mut record = key.record.clone();
    let public = inspect(&plain)
        .map(|i| i.public_key)
        .unwrap_or_else(|| "Error loading private key".into());
    record.insert("public_key".into(), Value::String(public));
    Ok(Value::Object(record))
}

pub async fn list(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let keys = private_keys::list(&state.db, ctx.team_id).await?;
    let items = keys
        .iter()
        .map(|k| present(&state, &ctx, k))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ok(Value::Array(items)).into_response())
}

pub async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let key = private_keys::find(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !can_view(&ctx, &key) {
        return Err(ApiError::Unauthorized);
    }
    Ok(ok(present(&state, &ctx, &key)?).into_response())
}

fn string_max(name: &'static str, required: bool) -> Field {
    let mut rules = Vec::new();
    if required {
        rules.push(Rule::Required);
    }
    rules.push(Rule::String);
    if name != "private_key" {
        rules.push(Rule::Max(255));
    }
    Field { name, rules }
}

/// PHP `base64_decode($s, true)` (strict, `php_base64_decode_ex`): whitespace is skipped, any
/// other character outside the alphabet fails, as does data after padding, a truncated last
/// group, or a wrong padding length. `None` is PHP's `false`.
fn php_base64_decode_strict(s: &str) -> Option<String> {
    let mut sextets = String::new();
    let mut padding = 0usize;
    for c in s.chars() {
        if c == '=' {
            padding += 1;
            continue;
        }
        if matches!(c, ' ' | '\t' | '\n' | '\r') {
            continue;
        }
        if !(c.is_ascii_alphanumeric() || c == '+' || c == '/') || padding > 0 {
            return None;
        }
        sextets.push(c);
    }
    if sextets.len() % 4 == 1 {
        return None;
    }
    if padding > 0 && (padding > 2 || !(sextets.len() + padding).is_multiple_of(4)) {
        return None;
    }
    // The leftover bits of the last group are dropped, whatever they are.
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::RequireNone)
            .with_decode_allow_trailing_bits(true),
    );
    let bytes = engine.decode(&sextets).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn truthy_str(input: &Input, key: &str) -> Option<String> {
    input
        .str(key)
        .filter(|s| !s.is_empty() && *s != "0")
        .map(str::to_string)
}

pub async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    require_admin(&ctx)?;
    let input = validated_json(&headers, &uri, &body)?;
    // `nameRules(required: false, nullable: false)`, `descriptionRules()` (4.7.5).
    let fields = [
        Field {
            name: "name",
            rules: crate::validation::patterns::present_name_rules(),
        },
        crate::validation::patterns::description(),
        string_max("private_key", true),
    ];
    let allowed: Vec<&str> = input.keys().map(String::as_str).collect();
    validate(
        &input,
        &fields,
        crate::validation::patterns::API_COMBINED_MESSAGES,
        &allowed,
    )?;
    let enc = encrypter(&state)?;

    let name = truthy_str(&input, "name").unwrap_or_else(random_name);
    let description =
        truthy_str(&input, "description").unwrap_or_else(|| "Created by Coolify via API".into());
    // 4.7.5: a key that is not PEM text can be sent base64 encoded. Other text is validated as
    // it is (`base64_decode($request->private_key, true)`).
    let raw = input.str("private_key").unwrap_or_default();
    let key = if raw.starts_with("-----BEGIN") {
        raw.to_string()
    } else {
        php_base64_decode_strict(raw).unwrap_or_else(|| raw.to_string())
    };
    if let Some(reason) = validation_error(&key, true) {
        return Err(invalid(reason));
    }
    let info = inspect(&key).ok_or(invalid(
        "The private key is invalid. Paste an unencrypted private key, not a public key or a certificate.",
    ))?;
    if private_keys::fingerprint_exists(&state.db, ctx.team_id, &info.fingerprint, None).await? {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Private key already exists.",
        ));
    }
    let stored = format_private_key(&key);
    let uuid = private_keys::create(
        &state.db,
        NewKey {
            team_id: ctx.team_id,
            name: &name,
            description: &description,
            private_key_encrypted: &enc.encrypt_string(&stored),
            fingerprint: &info.fingerprint,
        },
        state.now(),
    )
    .await?;
    Ok(created(json!({"uuid": uuid})).into_response())
}

/// Web UI list (`Security\PrivateKey\Index`): names, descriptions and
/// whether each key is in use, without the key material.
pub async fn ui_list(state: &AppState, ctx: &AuthCtx) -> ApiResult<Value> {
    let keys = private_keys::list(&state.db, ctx.team_id).await?;
    let mut items = Vec::with_capacity(keys.len());
    for k in &keys {
        items.push(json!({
            "uuid": k.uuid,
            "name": k.name,
            "description": k.description,
            "in_use": private_keys::in_use(&state.db, k.id).await?,
            "can_view": can_view(ctx, k),
        }));
    }
    Ok(json!({"keys": items, "can_create": ctx.is_team_admin()}))
}

/// `Index::generatePrivateKey`: a new key named like upstream's
/// `generate_random_name()`, described "Created by Coolify".
pub async fn ui_generate(state: &AppState, ctx: &AuthCtx, kind: KeyKind) -> ApiResult<String> {
    require_admin(ctx)?;
    let enc = encrypter(state)?;
    let key = generate(kind).ok_or(ApiError::Internal)?;
    let info = inspect(&key).ok_or(ApiError::Internal)?;
    let name = random_name();
    Ok(private_keys::create(
        &state.db,
        NewKey {
            team_id: ctx.team_id,
            name: &name,
            description: "Created by Coolify",
            private_key_encrypted: &enc.encrypt_string(&format_private_key(&key)),
            fingerprint: &info.fingerprint,
        },
        state.now(),
    )
    .await?)
}

/// `PrivateKey::cleanupUnusedKeys`: `safeDelete` every key of the team.
pub async fn ui_cleanup(state: &AppState, ctx: &AuthCtx) -> ApiResult<usize> {
    require_admin(ctx)?;
    let mut deleted = 0;
    for k in private_keys::list(&state.db, ctx.team_id).await? {
        if !private_keys::in_use(&state.db, k.id).await? {
            private_keys::delete(&state.db, k.id).await?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

fn model_validation(msg: &'static str) -> ApiError {
    let mut errors = serde_json::Map::new();
    errors.insert("private_key".into(), json!([msg]));
    ApiError::ValidationWithMessage(msg, errors)
}

pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Result<Response, ApiError> {
    let input = validated_json(&headers, &uri, &body)?;
    // `nameRules(required: false, nullable: false)`, `descriptionRules()` (4.7.5).
    let fields = [
        Field {
            name: "name",
            rules: crate::validation::patterns::present_name_rules(),
        },
        crate::validation::patterns::description(),
        string_max("private_key", false),
    ];
    validate(
        &input,
        &fields,
        crate::validation::patterns::API_COMBINED_MESSAGES,
        &["name", "description", "private_key"],
    )?;
    let key = private_keys::find(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    require_admin(&ctx)?;
    let enc = encrypter(&state)?;

    // The model's `saving` hook: format, validate, recompute the fingerprint.
    let current = enc
        .decrypt_string(&key.private_key_encrypted)
        .map_err(|_| ApiError::Internal)?;
    let new_plain = input.str("private_key").map(format_private_key);
    let effective = new_plain
        .clone()
        .unwrap_or_else(|| format_private_key(&current));
    // v4.7.5: `validationError()` checks only new or changed keys, so older keys can still be
    // renamed.
    let normalize = |k: &str| format_private_key(k).replace("\r\n", "\n");
    let is_changed = new_plain
        .as_deref()
        .is_some_and(|n| normalize(n) != normalize(&current));
    if is_changed && let Some(error) = validation_error(&effective, true) {
        return Err(model_validation(error));
    }
    let info = inspect(&effective).ok_or_else(|| {
        model_validation(
            "The private key is invalid. Paste an unencrypted private key, not a public key or a certificate.",
        )
    })?;
    if private_keys::fingerprint_exists(&state.db, ctx.team_id, &info.fingerprint, Some(key.id))
        .await?
    {
        return Err(model_validation("This private key already exists."));
    }
    let name = input
        .str("name")
        .map(str::to_string)
        .filter(|n| *n != key.name);
    let description = input
        .str("description")
        .map(str::to_string)
        .filter(|d| Some(d) != key.description.as_ref());
    let key_changed = new_plain.as_ref().is_some_and(|p| *p != current);
    if name.is_some() || description.is_some() || key_changed {
        private_keys::update(
            &state.db,
            key.id,
            KeyUpdate {
                name,
                description,
                private_key_encrypted: key_changed.then(|| enc.encrypt_string(&effective)),
                fingerprint: Some(info.fingerprint),
            },
            state.now(),
        )
        .await?;
    }
    Ok(created(json!({"uuid": key.uuid})).into_response())
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let key = private_keys::find(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    require_admin(&ctx)?;
    if private_keys::in_use(&state.db, key.id).await? {
        return Ok(crate::response::PhpJson(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({
                "message": "Private Key is in use and cannot be deleted.",
                "details": "This private key is currently being used by servers, applications, or Git integrations.",
            }),
        )
        .into_response());
    }
    private_keys::delete(&state.db, key.id).await?;
    Ok(ok(json!({"message": "Private Key deleted."})).into_response())
}

/// `generate_random_name()`: upstream uses an alliterative adjective-noun
/// generator plus a public id; the words differ here but the shape matches.
pub(crate) fn random_name() -> String {
    use rand::seq::SliceRandom;
    const PAIRS: &[(&str, &str)] = &[
        ("brave", "badger"),
        ("calm", "cobra"),
        ("daring", "dolphin"),
        ("eager", "eagle"),
        ("fancy", "falcon"),
        ("gentle", "gecko"),
        ("happy", "heron"),
        ("jolly", "jaguar"),
        ("lucky", "lynx"),
        ("mighty", "moose"),
        ("noble", "newt"),
        ("quick", "quail"),
        ("rapid", "raven"),
        ("sunny", "salmon"),
        ("tidy", "tiger"),
        ("witty", "walrus"),
    ];
    let (a, n) = PAIRS
        .choose(&mut rand::thread_rng())
        .copied()
        .unwrap_or(("calm", "cobra"));
    format!("{a}-{n}-{}", coolify_domain::new_public_id())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `base64_decode($value, true)` (4.7.5 `create_key`).
    #[test]
    fn php_base64_strict_rejects_what_is_not_base64() {
        let d = |s: &str| php_base64_decode_strict(s);
        assert_eq!(d("aGVsbG8=").as_deref(), Some("hello"));
        // Padding is optional, whitespace is skipped.
        assert_eq!(d("aGVsbG8").as_deref(), Some("hello"));
        assert_eq!(d("aGVs\nbG8=\n").as_deref(), Some("hello"));
        assert_eq!(d("").as_deref(), Some(""));
        // Characters outside the alphabet, data after padding, truncated groups, wrong padding.
        assert_eq!(d("!!"), None);
        assert_eq!(d("ssh-ed25519 AAAA"), None);
        assert_eq!(d("aGVs=bG8"), None);
        assert_eq!(d("aGVsb"), None);
        assert_eq!(d("aGVsbG8=="), None);
        assert_eq!(d("aGVsbG8==="), None);
        // "garbage" is seven alphabet characters: PHP decodes it, and the key check then fails.
        assert!(d("garbage").is_some());
    }

    #[test]
    fn random_names_have_upstream_shape() {
        let n = random_name();
        assert_eq!(n.split('-').count(), 3);
        assert_eq!(n.rsplit('-').next().map(str::len), Some(24));
    }
}
