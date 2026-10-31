//! `InstanceEmailSettingsController`: the instance's SMTP and Resend settings.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::instance_settings as settings;
use coolify_db::records::Record;
use coolify_db::servers::ColumnValue;
use serde_json::{Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::input::{Input, lenient_json};
use crate::response::{ApiError, ApiResult, ok};
use crate::rules::{email_rule, errors_of, valid_hostname};
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule};

const FIELDS: &[&str] = &[
    "smtp_enabled",
    "smtp_from_address",
    "smtp_from_name",
    "smtp_host",
    "smtp_port",
    "smtp_encryption",
    "smtp_username",
    "smtp_password",
    "smtp_timeout",
    "smtp_ehlo_domain",
    "resend_enabled",
    "resend_api_key",
];

/// `authorizeRootTeam`.
async fn authorize(state: &AppState, ctx: &AuthCtx) -> Result<(), ApiError> {
    if ctx.team_id != 0 {
        return Err(ApiError::Abort(
            StatusCode::FORBIDDEN,
            "Instance email settings require a root-team API token.",
        ));
    }
    if !settings::is_instance_admin(&state.db, ctx.user_id).await? {
        return Err(ApiError::Unauthorized);
    }
    Ok(())
}

/// `serialize`: the fields in attribute order, secrets for `read:sensitive`.
fn present(state: &AppState, ctx: &AuthCtx, row: &Record) -> Result<Value, ApiError> {
    let mut out = Record::new();
    for (k, v) in row {
        if !FIELDS.contains(&k.as_str()) {
            continue;
        }
        if settings::SECRET_COLUMNS.contains(&k.as_str()) {
            if !ctx.can_read_sensitive {
                continue;
            }
            let plain = match v.as_str() {
                Some(c) => {
                    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
                    Value::String(enc.decrypt_string(c).map_err(|_| ApiError::Internal)?)
                }
                None => Value::Null,
            };
            out.insert(k.clone(), plain);
        } else {
            out.insert(k.clone(), v.clone());
        }
    }
    Ok(Value::Object(out))
}

pub async fn show(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    authorize(&state, &ctx).await?;
    let row = settings::row(&state.db).await?;
    Ok(ok(present(&state, &ctx, &row)?).into_response())
}

pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    authorize(&state, &ctx).await?;
    let no_query: Uri = uri.path().parse().unwrap_or(uri);
    let input: Input = lenient_json(&headers, &no_query, &body);
    let string = |max: usize| vec![Rule::Nullable, Rule::String, Rule::Max(max)];
    let mut ehlo = string(255);
    ehlo.push(Rule::Custom(valid_hostname));
    let f = |name: &'static str, rules: Vec<Rule>| Field { name, rules };
    let fields = vec![
        f("smtp_enabled", vec![Rule::Boolean]),
        f(
            "smtp_from_address",
            vec![Rule::Nullable, Rule::Custom(email_rule)],
        ),
        f("smtp_from_name", string(255)),
        f("smtp_host", string(255)),
        f(
            "smtp_port",
            vec![
                Rule::Nullable,
                Rule::Integer,
                Rule::MinNum(1),
                Rule::MaxNum(65535),
            ],
        ),
        f(
            "smtp_encryption",
            vec![
                Rule::Nullable,
                Rule::String,
                Rule::In(&["starttls", "tls", "none"]),
            ],
        ),
        f("smtp_username", string(255)),
        f("smtp_password", string(255)),
        f(
            "smtp_timeout",
            vec![Rule::Nullable, Rule::Integer, Rule::MinNum(0)],
        ),
        f("smtp_ehlo_domain", ehlo),
        f("resend_enabled", vec![Rule::Boolean]),
        f("resend_api_key", string(255)),
    ];
    let errors = errors_of(&input, &fields, CUSTOM_API_MESSAGES)?;
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    let mut changed: Vec<&str> = Vec::new();
    for field in &fields {
        let name = field.name;
        let Some(v) = input.get(name) else {
            continue;
        };
        changed.push(name);
        let value = if settings::SECRET_COLUMNS.contains(&name) {
            ColumnValue::Text(v.as_str().map(|s| enc.encrypt_string(s)))
        } else {
            match v {
                Value::Bool(b) => ColumnValue::Bool(*b),
                Value::Null => ColumnValue::Text(None),
                Value::Number(n) if name == "smtp_port" || name == "smtp_timeout" => {
                    ColumnValue::Int(n.as_i64().unwrap_or(0))
                }
                Value::Number(n) => ColumnValue::Bool(n.as_i64() == Some(1)),
                Value::String(s) if name == "smtp_port" || name == "smtp_timeout" => {
                    ColumnValue::Int(s.parse().unwrap_or(0))
                }
                Value::String(s) if name.ends_with("_enabled") => ColumnValue::Bool(s == "1"),
                Value::String(s) => ColumnValue::Text(Some(s.clone())),
                _ => continue,
            }
        };
        values.push((name, value));
    }
    settings::update(&state.db, &values, state.now()).await?;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "api.settings.email.updated",
        json!({"changed_fields": changed}),
        "info",
    )
    .await;
    let row = settings::row(&state.db).await?;
    Ok(ok(present(&state, &ctx, &row)?).into_response())
}
