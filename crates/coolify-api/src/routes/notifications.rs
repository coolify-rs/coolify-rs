//! `NotificationsController`: read and update the six notification channels of the token's team.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::notification_settings::{self as ns, Channel};
use coolify_db::records::Record;
use coolify_db::servers::ColumnValue;
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::input::{Input, validated_json};
use crate::response::{ApiError, ApiResult, ok};
use crate::rules::{email_rule, errors_of, ordered, valid_hostname};
use crate::serialize::serialize_record;
use crate::state::AppState;
use crate::validation::{Field, Rule};

const INTEGER_COLUMNS: &[&str] = &["smtp_port", "smtp_timeout"];

/// `serializeSettings`: hidden fields only for `read:sensitive`, decrypted.
pub(crate) fn present(
    state: &AppState,
    ch: &Channel,
    can_read_sensitive: bool,
    mut r: Record,
) -> Result<Value, ApiError> {
    for k in ch.hidden {
        if can_read_sensitive {
            let plain = decrypted(state, ch, &r, k)?;
            if r.contains_key(*k) {
                r.insert((*k).to_string(), plain);
            }
        } else {
            r.shift_remove(*k);
        }
    }
    Ok(Value::Object(serialize_record(r)))
}

/// An `encrypted` attribute as Eloquent returns it.
fn decrypted(state: &AppState, ch: &Channel, r: &Record, k: &str) -> Result<Value, ApiError> {
    let Some(v) = r.get(k) else {
        return Ok(Value::Null);
    };
    if !ch.encrypted.contains(&k) {
        return Ok(v.clone());
    }
    let Some(c) = v.as_str() else {
        return Ok(Value::Null);
    };
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    enc.decrypt_string(c)
        .map(Value::String)
        .map_err(|_| ApiError::Internal)
}

/// `channelConfig(...)['rules']`.
fn fields(ch: &Channel) -> Vec<Field> {
    let string = |max: usize| vec![Rule::Nullable, Rule::String, Rule::Max(max)];
    let mut out: Vec<Field> = Vec::new();
    let text = |name: &'static str, rules: Vec<Rule>| Field { name, rules };
    match ch.name {
        "email" => {
            out.push(text("smtp_enabled", vec![Rule::Boolean]));
            out.push(text(
                "smtp_from_address",
                vec![Rule::Nullable, Rule::Custom(email_rule)],
            ));
            out.push(text("smtp_from_name", string(255)));
            out.push(text("smtp_recipients", string(1000)));
            out.push(text("smtp_host", string(255)));
            out.push(text(
                "smtp_port",
                vec![
                    Rule::Nullable,
                    Rule::Integer,
                    Rule::MinNum(1),
                    Rule::MaxNum(65535),
                ],
            ));
            out.push(text(
                "smtp_encryption",
                vec![
                    Rule::Nullable,
                    Rule::String,
                    Rule::In(&["starttls", "tls", "none"]),
                ],
            ));
            out.push(text("smtp_username", string(255)));
            out.push(text("smtp_password", string(255)));
            out.push(text(
                "smtp_timeout",
                vec![Rule::Nullable, Rule::Integer, Rule::MinNum(0)],
            ));
            let mut ehlo = string(255);
            ehlo.push(Rule::Custom(valid_hostname));
            out.push(text("smtp_ehlo_domain", ehlo));
            out.push(text("resend_enabled", vec![Rule::Boolean]));
            out.push(text("resend_api_key", string(255)));
            out.push(text("use_instance_email_settings", vec![Rule::Boolean]));
        }
        "discord" => {
            out.push(text("discord_enabled", vec![Rule::Boolean]));
            out.push(text(
                "discord_webhook_url",
                vec![Rule::Nullable, Rule::String],
            ));
        }
        "slack" => {
            out.push(text("slack_enabled", vec![Rule::Boolean]));
            out.push(text(
                "slack_webhook_url",
                vec![Rule::Nullable, Rule::String],
            ));
        }
        "telegram" => {
            out.push(text("telegram_enabled", vec![Rule::Boolean]));
            out.push(text("telegram_token", string(255)));
            out.push(text("telegram_chat_id", string(255)));
        }
        "pushover" => {
            out.push(text("pushover_enabled", vec![Rule::Boolean]));
            out.push(text("pushover_user_key", string(255)));
            out.push(text("pushover_api_token", string(255)));
        }
        _ => {
            out.push(text("webhook_enabled", vec![Rule::Boolean]));
            out.push(text("webhook_url", vec![Rule::Nullable, Rule::String]));
        }
    }
    for f in ch.fillable {
        if out.iter().any(|x| x.name == *f) {
            continue;
        }
        let rules = if f.ends_with("_thread_id") {
            string(255)
        } else {
            vec![Rule::Boolean]
        };
        out.push(text(f, rules));
    }
    out
}

/// The `$rules` order of the controller, used to order errors.
fn rule_order(ch: &Channel) -> Vec<&'static str> {
    fields(ch).iter().map(|f| f.name).collect()
}

fn url_field(ch: &Channel) -> Option<&'static str> {
    match ch.name {
        "discord" => Some("discord_webhook_url"),
        "slack" => Some("slack_webhook_url"),
        "webhook" => Some("webhook_url"),
        _ => None,
    }
}

/// Eloquent `fill()` for one attribute.
fn column_value(
    state: &AppState,
    ch: &Channel,
    k: &'static str,
    v: &Value,
) -> Result<ColumnValue, ApiError> {
    if ch.encrypted.contains(&k) {
        let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
        return Ok(ColumnValue::Text(match v {
            Value::Null => None,
            Value::String(s) => Some(enc.encrypt_string(s)),
            other => Some(enc.encrypt_string(&other.to_string())),
        }));
    }
    Ok(match v {
        Value::Bool(b) => ColumnValue::Bool(*b),
        Value::Null => ColumnValue::Text(None),
        Value::Number(n) if INTEGER_COLUMNS.contains(&k) => {
            ColumnValue::Int(n.as_i64().unwrap_or(0))
        }
        Value::Number(n) => ColumnValue::Bool(n.as_i64() == Some(1)),
        Value::String(s) if INTEGER_COLUMNS.contains(&k) => {
            ColumnValue::Int(s.parse().unwrap_or(0))
        }
        Value::String(s) if k.ends_with("_enabled") || k.ends_with("_notifications") => {
            ColumnValue::Bool(s == "1")
        }
        Value::String(s) => ColumnValue::Text(Some(s.clone())),
        other => ColumnValue::Text(Some(other.to_string())),
    })
}

async fn show(state: AppState, ctx: AuthCtx, ch: &'static Channel) -> ApiResult {
    let (row, created) = ns::first_or_create(&state.db, ch, ctx.team_id).await?;
    // `firstOrCreate` returns the new model with only `team_id` and `id` set.
    let row = if created {
        let mut r = Record::new();
        r.insert("team_id".into(), json!(ctx.team_id));
        r.insert("id".into(), row.get("id").cloned().unwrap_or(Value::Null));
        r
    } else {
        row
    };
    Ok(ok(present(&state, ch, ctx.can_read_sensitive, row)?).into_response())
}

/// `$request->json()->all()` without query parameters.
fn body_keys_only(input: Input, body: &[u8]) -> Input {
    let keys: Vec<String> = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.as_object().map(|m| m.keys().cloned().collect()))
        .unwrap_or_default();
    Input(
        input
            .0
            .into_iter()
            .filter(|(k, _)| keys.contains(k))
            .collect(),
    )
}

async fn update(
    state: AppState,
    ctx: AuthCtx,
    meta: Meta,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
    ch: &'static Channel,
) -> ApiResult {
    let input = body_keys_only(validated_json(&headers, &uri, &body)?, &body);
    let mut errors = errors_of(&input, &fields(ch), &[])?;
    if let Some(f) = url_field(ch)
        && let Some(url) = input.get(f).filter(|v| !v.is_null())
    {
        // The rule object runs after a failed `string` rule too.
        let msg = match url.as_str() {
            Some(text) => crate::safe_url::check(&state, f, text, &headers).await?,
            None => Some(format!("The {} must be a valid URL.", f.replace('_', " "))),
        };
        if let Some(msg) = msg
            && let Value::Array(list) = errors.entry(f.to_string()).or_insert_with(|| json!([]))
        {
            list.push(json!(msg));
        }
    }
    let mut errors: Map<String, Value> = ordered(errors, &rule_order(ch));
    for k in input.keys() {
        if !ch.fillable.contains(&k.as_str()) {
            errors.insert(k.clone(), json!(["This field is not allowed."]));
        }
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    ns::first_or_create(&state.db, ch, ctx.team_id).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let mut changes: Vec<(&'static str, ColumnValue)> = Vec::new();
    for f in ch.fillable {
        if let Some(v) = input.get(f) {
            changes.push((f, column_value(&state, ch, f, v)?));
        }
    }
    ns::update(&state.db, ch, ctx.team_id, &changes).await?;
    let changed: Vec<&str> = ch
        .fillable
        .iter()
        .copied()
        .filter(|f| input.has(f))
        .collect();
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        &format!("api.notifications.{}.updated", ch.name),
        json!({"team_id": ctx.team_id_raw, "changed_fields": changed}),
        "info",
    )
    .await;
    let row = ns::find(&state.db, ch, ctx.team_id)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "No query results for model.",
        ))?;
    Ok(ok(present(&state, ch, ctx.can_read_sensitive, row)?).into_response())
}

macro_rules! channel {
    ($show:ident, $update:ident, $ch:expr) => {
        pub async fn $show(
            State(state): State<AppState>,
            Extension(ctx): Extension<AuthCtx>,
        ) -> ApiResult {
            show(state, ctx, &$ch).await
        }
        pub async fn $update(
            State(state): State<AppState>,
            Extension(ctx): Extension<AuthCtx>,
            meta: Meta,
            headers: HeaderMap,
            uri: Uri,
            body: Bytes,
        ) -> ApiResult {
            update(state, ctx, meta, headers, uri, body, &$ch).await
        }
    };
}

channel!(email, update_email, ns::EMAIL);
channel!(discord, update_discord, ns::DISCORD);
channel!(slack, update_slack, ns::SLACK);
channel!(telegram, update_telegram, ns::TELEGRAM);
channel!(pushover, update_pushover, ns::PUSHOVER);
channel!(webhook, update_webhook, ns::WEBHOOK);
