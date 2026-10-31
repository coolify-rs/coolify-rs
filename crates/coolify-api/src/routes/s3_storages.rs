//! `S3StoragesController`.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::database_parts::{self as parts, NewS3Storage, S3_STORAGE};
use coolify_db::records::Record;
use coolify_db::servers::ColumnValue;
use coolify_domain::naming::{DESCRIPTION_PATTERN, NAME_PATTERN};
use coolify_jobs::databases::s3;
use serde_json::{Value, json};

use crate::auth::AuthCtx;
use crate::input::validated_json;
use crate::response::{ApiError, ApiResult, created, ok};
use crate::serialize::serialize_record;
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "S3 storage not found.");
const ALLOWED: &[&str] = &[
    "name",
    "description",
    "endpoint",
    "bucket",
    "region",
    "key",
    "secret",
    "is_usable",
];

/// `ValidS3BucketName`.
fn bucket_rule(v: &Value) -> Option<&'static str> {
    const MESSAGE: &str = "The :attribute must be a valid S3 bucket name: 3-63 letters, numbers, dots, or hyphens; start and end with a letter or number; no consecutive dots, dot-hyphen pairs, or IP address format.";
    match v.as_str() {
        Some(s) if s3::valid_bucket_name(s) => None,
        _ => Some(MESSAGE),
    }
}

/// `Rule::CustomOwned` cannot wait for DNS: the `SafeWebhookUrl` failure is computed before
/// `validate`, and merged into the field's place by [`run_validation`].
async fn run_validation(
    state: &AppState,
    headers: &HeaderMap,
    input: &crate::input::Input,
    required: bool,
) -> Result<(), ApiError> {
    let specs = fields(required);
    let first = validate(input, &specs, CUSTOM_API_MESSAGES, ALLOWED);
    let mut endpoint_error = None;
    if let Some(url) = input.get("endpoint").and_then(Value::as_str)
        && url.chars().count() <= 255
    {
        let policy = s3::UrlPolicy::load(&state.db).await;
        let host = headers
            .get(axum::http::header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("localhost");
        let settings = format!("http://{host}/settings/advanced");
        endpoint_error = s3::endpoint_error(url, "endpoint", &policy, &settings).await;
    }
    let Some(message) = endpoint_error else {
        return first;
    };
    let mut errors = match first {
        Err(ApiError::Validation(m)) => m,
        Err(other) => return Err(other),
        Ok(()) => serde_json::Map::new(),
    };
    match errors.get_mut("endpoint") {
        Some(Value::Array(list)) => list.push(Value::String(message)),
        _ => {
            errors.insert("endpoint".into(), json!([message]));
        }
    }
    // Field order, then the keys that are not allowed.
    let mut ordered = serde_json::Map::new();
    for f in &specs {
        if let Some(v) = errors.shift_remove(f.name) {
            ordered.insert(f.name.to_owned(), v);
        }
    }
    ordered.extend(errors);
    Err(ApiError::Validation(ordered))
}

fn fields(required: bool) -> Vec<Field> {
    let req = |mut rules: Vec<Rule>| {
        if required {
            rules.insert(0, Rule::Required);
        }
        rules
    };
    let mut name = vec![
        Rule::String,
        Rule::Min(3),
        Rule::Max(255),
        Rule::Regex(&NAME_PATTERN),
    ];
    name.insert(
        0,
        if required {
            Rule::Required
        } else {
            Rule::Nullable
        },
    );
    vec![
        Field {
            name: "name",
            rules: name,
        },
        Field {
            name: "description",
            rules: vec![
                Rule::Nullable,
                Rule::String,
                Rule::Max(255),
                Rule::Regex(&DESCRIPTION_PATTERN),
            ],
        },
        Field {
            name: "endpoint",
            rules: req(vec![Rule::String, Rule::Max(255)]),
        },
        Field {
            name: "bucket",
            rules: req(vec![Rule::Custom(bucket_rule)]),
        },
        Field {
            name: "region",
            rules: req(vec![Rule::String, Rule::Max(255)]),
        },
        Field {
            name: "key",
            rules: req(vec![Rule::String, Rule::Max(255)]),
        },
        Field {
            name: "secret",
            rules: req(vec![Rule::String, Rule::Max(255)]),
        },
        Field {
            name: "is_usable",
            rules: vec![Rule::Boolean],
        },
    ]
}

/// `removeSensitiveData(S3Storage)`.
fn present(state: &AppState, ctx: &AuthCtx, raw: &Record) -> Result<Value, ApiError> {
    let mut r = S3_STORAGE.present(raw.clone());
    r.shift_remove("id");
    if ctx.can_read_sensitive {
        let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
        for k in ["key", "secret"] {
            if let Some(Value::String(c)) = r.get(k) {
                let plain = enc.decrypt_string(c).map_err(|_| ApiError::Internal)?;
                r.insert(k.into(), Value::String(plain));
            }
        }
    } else {
        r.shift_remove("key");
        r.shift_remove("secret");
    }
    Ok(Value::Object(serialize_record(r)))
}

/// `GET /s3-storages`.
pub async fn index(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let rows = parts::s3_list(&state.db, ctx.team_id).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(present(&state, &ctx, r)?);
    }
    Ok(ok(Value::Array(out)).into_response())
}

/// `GET /s3-storages/{uuid}`.
pub async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let r = parts::s3_find(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    Ok(ok(present(&state, &ctx, &r)?).into_response())
}

/// `POST /s3-storages`.
pub async fn store(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input = validated_json(&headers, &uri, &body)?;
    run_validation(&state, &headers, &input, true).await?;
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let trimmed = |k: &str| input.str(k).unwrap_or_default().trim().to_owned();
    let is_usable = match input.get("is_usable") {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        Some(Value::String(s)) => s == "1",
        _ => false,
    };
    let uuid = parts::s3_create(
        &state.db,
        &NewS3Storage {
            team_id: ctx.team_id,
            name: &coolify_domain::naming::strip_tags(input.str("name")),
            description: input
                .str("description")
                .map(|d| coolify_domain::naming::strip_tags(Some(d)))
                .as_deref(),
            endpoint: Some(&trimmed("endpoint")),
            bucket: &trimmed("bucket"),
            region: &trimmed("region"),
            key_encrypted: &enc.encrypt_string(&trimmed("key")),
            secret_encrypted: &enc.encrypt_string(&trimmed("secret")),
            is_usable,
        },
        state.now(),
    )
    .await?;
    tracing::info!(target: "audit", event = "api.s3_storage.created", team_id = ctx.team_id, s3_storage_uuid = %uuid, "s3 storage created");
    Ok(created(json!({"uuid": uuid})).into_response())
}

/// `PATCH /s3-storages/{uuid}`.
pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    run_validation(&state, &headers, &input, false).await?;
    let row = parts::s3_find(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    for k in ["name", "description", "endpoint", "bucket", "region"] {
        if !input.has(k) {
            continue;
        }
        let text = input.str(k).map(|s| match k {
            "name" | "description" => coolify_domain::naming::strip_tags(Some(s)),
            _ => s.trim().to_owned(),
        });
        values.push((k, ColumnValue::Text(text)));
    }
    for k in ["key", "secret"] {
        if let Some(plain) = input.str(k) {
            let plain = plain.trim();
            // The `encrypted` cast is dirty only when the decrypted value changes.
            let same = parts::str_of(&row, k)
                .and_then(|c| enc.decrypt_string(c).ok())
                .is_some_and(|c| c == plain);
            if !same {
                values.push((k, ColumnValue::Text(Some(enc.encrypt_string(plain)))));
            }
        }
    }
    match input.get("is_usable") {
        Some(Value::Bool(b)) => values.push(("is_usable", ColumnValue::Bool(*b))),
        Some(Value::Number(n)) => {
            values.push(("is_usable", ColumnValue::Bool(n.as_i64() == Some(1))))
        }
        Some(Value::String(s)) => values.push(("is_usable", ColumnValue::Bool(s == "1"))),
        _ => {}
    }
    parts::update_row(
        &state.db,
        "s3_storages",
        parts::id_of(&row),
        &values,
        state.now(),
    )
    .await?;
    tracing::info!(target: "audit", event = "api.s3_storage.updated", team_id = ctx.team_id, s3_storage_uuid = %uuid, "s3 storage updated");
    Ok(ok(json!({"uuid": uuid})).into_response())
}

/// `DELETE /s3-storages/{uuid}`.
pub async fn destroy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let row = parts::s3_find(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    parts::s3_delete(&state.db, parts::id_of(&row), state.now()).await?;
    tracing::info!(target: "audit", event = "api.s3_storage.deleted", team_id = ctx.team_id, s3_storage_uuid = %uuid, "s3 storage deleted");
    Ok(ok(json!({"message": "S3 storage deleted."})).into_response())
}

/// `POST /s3-storages/{uuid}/validate`.
pub async fn validate_storage(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let row = parts::s3_find(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let result = jobs.test_s3_connection(&row).await;
    tracing::info!(target: "audit", event = "api.s3_storage.validated", team_id = ctx.team_id, s3_storage_uuid = %uuid, valid = result.is_ok(), "s3 storage validated");
    Ok(match result {
        Ok(()) => ok(json!({"valid": true, "message": "S3 storage connection is valid."})),
        Err(error) => ok(json!({"valid": false, "message": error})),
    }
    .into_response())
}
