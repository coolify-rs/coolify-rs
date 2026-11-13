//! `S3StoragesController`: the backup storages (S3, SFTP, FTPS, and WebDAV) of a team.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::database_parts::{self as parts, NewS3Storage, S3_STORAGE};
use coolify_db::records::Record;
use coolify_db::servers::ColumnValue;
use coolify_domain::naming::{DESCRIPTION_PATTERN, NAME_PATTERN};
use coolify_jobs::databases::file_storage;
use coolify_jobs::databases::s3;
use coolify_jobs::databases::storage_type::{self as st, BackupStorageType};
use serde_json::{Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, validated_json};
use crate::response::{ApiError, ApiResult, created, ok};
use crate::serialize::serialize_record;
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "S3 storage not found.");

/// `BackupStorageType::cases()` as values, for `Rule::enum(BackupStorageType::class)`.
const TYPES: &[&str] = &["s3", "sftp", "ftps", "webdav"];

/// `connectionFields()`: the request fields of a storage type. S3 has a bucket, a region, and a URL
/// style; the other types have a URL, a username (key), and a password (secret).
fn connection_fields(kind: BackupStorageType) -> &'static [&'static str] {
    match kind {
        BackupStorageType::S3 => &[
            "endpoint",
            "bucket",
            "region",
            "key",
            "secret",
            "use_path_style_endpoint",
            "skip_tls_verification",
        ],
        BackupStorageType::Sftp => &["endpoint", "key", "secret", "private_key_uuid"],
        _ => &["endpoint", "key", "secret"],
    }
}

/// `ValidS3BucketName`.
fn bucket_rule(v: &Value) -> Option<&'static str> {
    const MESSAGE: &str = "The :attribute must be a valid S3 bucket name: 3-63 letters, numbers, dots, or hyphens; start and end with a letter or number; no consecutive dots, dot-hyphen pairs, or IP address format.";
    match v.as_str() {
        Some(s) if s3::valid_bucket_name(s) => None,
        _ => Some(MESSAGE),
    }
}

/// The `key` rule of the types that are not S3: rclone reads the user from a config file line, so
/// it must not contain line breaks or quotes (`regex:/^[^\x00-\x1f\x7f"]+$/`).
fn user_rule(v: &Value) -> Option<&'static str> {
    let subject = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return Some("The :attribute field format is invalid."),
    };
    (!file_storage::valid_user(&subject)).then_some("The :attribute field format is invalid.")
}

/// Laravel `blank()` of a request value.
fn blank(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::Object(o)) => o.is_empty(),
        _ => false,
    }
}

/// What the rules of a request need from the storage it updates.
struct Saved<'a> {
    /// `$storage->trustedInternalHosts()` as a URL policy, and `$storage->endpoint`.
    row: &'a Record,
}

/// `connectionRules()` of the controller with `validateBody()`. The rules that wait for DNS or the
/// database (`SafeWebhookUrl`, `Rule::exists`) are evaluated before `validate` and take their place
/// in the rule order as [`Rule::Evaluated`].
async fn run_validation(
    state: &AppState,
    ctx: &AuthCtx,
    headers: &HeaderMap,
    input: &Input,
    kind: BackupStorageType,
    saved: Option<Saved<'_>>,
) -> Result<(), ApiError> {
    let partial = saved.is_some();
    let req = |mut rules: Vec<Rule>| {
        if !partial {
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
        if partial {
            Rule::Nullable
        } else {
            Rule::Required
        },
    );

    // The closure and `SafeWebhookUrl` of the endpoint.
    let saved_endpoint = saved
        .as_ref()
        .and_then(|s| parts::str_of(s.row, "endpoint"));
    let mut endpoint_rules = req(vec![Rule::String, Rule::Max(255)]);
    if let Some(url) = input.get("endpoint").and_then(Value::as_str) {
        endpoint_rules.push(Rule::Evaluated(file_storage::endpoint_credentials_error(
            url,
            saved_endpoint,
            "endpoint",
        )));
        endpoint_rules.push(Rule::Evaluated(file_storage::endpoint_path_error(
            url, "endpoint",
        )));
        if url.chars().count() <= 255 {
            let mut policy = s3::UrlPolicy::load(&state.db).await;
            // `trustedInternalHosts()`: the bundled MinIO is not a user-supplied destination.
            if let Some(s) = &saved
                && parts::str_of(s.row, "uuid") == Some("minio")
                && st::endpoint_host(saved_endpoint.unwrap_or_default()) == "coolify-minio"
            {
                policy.allowed.push("coolify-minio".to_owned());
            }
            let host = headers
                .get(axum::http::header::HOST)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("localhost");
            let settings = format!("http://{host}/settings/advanced");
            endpoint_rules.push(Rule::Evaluated(
                s3::endpoint_error_for(
                    url,
                    "endpoint",
                    &policy,
                    &settings,
                    kind.endpoint_schemes(),
                )
                .await,
            ));
        }
    }

    let mut fields = Vec::new();
    if !partial {
        // `['sometimes', Rule::enum(BackupStorageType::class)]`.
        fields.push(Field {
            name: "type",
            rules: vec![Rule::Nullable, Rule::In(TYPES)],
        });
    }
    fields.extend([
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
            rules: endpoint_rules,
        },
    ]);
    // `required_without:private_key_uuid` is an implicit rule: it runs on a missing secret.
    let mut secret_required_without_key = false;
    if kind == BackupStorageType::S3 {
        fields.push(Field {
            name: "bucket",
            rules: req(vec![Rule::Custom(bucket_rule)]),
        });
        for name in ["region", "key", "secret"] {
            fields.push(Field {
                name,
                rules: req(vec![Rule::String, Rule::Max(255)]),
            });
        }
    } else {
        fields.push(Field {
            name: "key",
            rules: req(vec![Rule::String, Rule::Max(255), Rule::Custom(user_rule)]),
        });
        if kind == BackupStorageType::Sftp {
            secret_required_without_key =
                !partial && blank(input.get("secret")) && blank(input.get("private_key_uuid"));
            fields.push(Field {
                name: "secret",
                rules: vec![Rule::Nullable, Rule::String, Rule::Max(255)],
            });
            // `Rule::exists('private_keys', 'uuid')` of the team, without Git deploy keys.
            let exists = match input.get("private_key_uuid") {
                None | Some(Value::Null) => None,
                Some(Value::String(uuid)) => parts::s3_login_key_id(&state.db, ctx.team_id, uuid)
                    .await?
                    .is_none()
                    .then(|| "The selected :attribute is invalid.".to_owned()),
                Some(_) => Some("The selected :attribute is invalid.".to_owned()),
            };
            fields.push(Field {
                name: "private_key_uuid",
                rules: vec![Rule::Nullable, Rule::String, Rule::Evaluated(exists)],
            });
        } else {
            fields.push(Field {
                name: "secret",
                rules: req(vec![Rule::String, Rule::Max(255)]),
            });
        }
    }
    for name in [
        "use_path_style_endpoint",
        "skip_tls_verification",
        "is_usable",
    ] {
        fields.push(Field {
            name,
            rules: vec![Rule::Boolean],
        });
    }

    let mut allowed: Vec<&str> = vec!["name", "description", "is_usable"];
    if !partial {
        allowed.insert(0, "type");
    }
    allowed.extend(connection_fields(kind));
    let first = validate(input, &fields, CUSTOM_API_MESSAGES, &allowed);
    if !secret_required_without_key {
        return first;
    }
    let mut errors = match first {
        Err(ApiError::Validation(m)) => m,
        Err(other) => return Err(other),
        Ok(()) => serde_json::Map::new(),
    };
    // A failed implicit rule is the only failure of its field.
    errors.insert(
        "secret".into(),
        json!(["The secret field is required when private key uuid is not present."]),
    );
    // Field order, then the keys that are not allowed.
    let mut ordered = serde_json::Map::new();
    for f in &fields {
        if let Some(v) = errors.shift_remove(f.name) {
            ordered.insert(f.name.to_owned(), v);
        }
    }
    ordered.extend(errors);
    Err(ApiError::Validation(ordered))
}

/// What `sftpLoginAttributes()` changes.
#[derive(Debug, Default, PartialEq, Eq)]
struct SftpLogin {
    /// `Some(None)` logs in with the password.
    private_key_id: Option<Option<i64>>,
    /// The new password; an empty one with a private key.
    secret: Option<String>,
}

/// `sftpLoginAttributes()`: the login columns of an SFTP storage from the request: a private key
/// UUID logs in with that key, a password logs in with the password. Nothing when the request
/// changes neither. `current_secret_blank`: the saved password is empty (a new storage has none).
async fn sftp_login_attributes(
    state: &AppState,
    ctx: &AuthCtx,
    input: &Input,
    current_secret_blank: bool,
) -> Result<SftpLogin, ApiError> {
    if !blank(input.get("private_key_uuid")) {
        let uuid = input.str("private_key_uuid").unwrap_or_default();
        return Ok(SftpLogin {
            private_key_id: Some(parts::s3_login_key_id(&state.db, ctx.team_id, uuid).await?),
            secret: Some(String::new()),
        });
    }
    if !blank(input.get("secret")) {
        return Ok(SftpLogin {
            private_key_id: Some(None),
            secret: input.str("secret").map(str::to_owned),
        });
    }
    if input.has("private_key_uuid") {
        if current_secret_blank {
            let mut errors = serde_json::Map::new();
            errors.insert(
                "secret".into(),
                json!(["Send a password (secret) to log in without a private key."]),
            );
            return Err(ApiError::Validation(errors));
        }
        return Ok(SftpLogin {
            private_key_id: Some(None),
            secret: None,
        });
    }
    Ok(SftpLogin::default())
}

/// `removeSensitiveData(S3Storage)`.
async fn present(state: &AppState, ctx: &AuthCtx, raw: &Record) -> Result<Value, ApiError> {
    let private_key_id = raw.get("private_key_id").and_then(Value::as_i64);
    let mut r = S3_STORAGE.present(raw.clone());
    r.shift_remove("id");
    r.shift_remove("private_key_id");
    if BackupStorageType::of(raw) == BackupStorageType::Sftp {
        let uuid = match private_key_id {
            Some(id) => parts::s3_private_key_uuid(&state.db, id).await?,
            None => None,
        };
        r.insert("private_key_uuid".into(), json!(uuid));
    }
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

fn flag(input: &Input, k: &str) -> Option<bool> {
    match input.get(k) {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::Number(n)) => Some(n.as_i64() == Some(1)),
        Some(Value::String(s)) => Some(s == "1"),
        _ => None,
    }
}

/// `GET /s3-storages`.
pub async fn index(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let rows = parts::s3_list(&state.db, ctx.team_id).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(present(&state, &ctx, r).await?);
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
    Ok(ok(present(&state, &ctx, &r).await?).into_response())
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
    // `$body['type'] ?? 's3'`; a value that is not a type validates as S3 and fails the type rule.
    let kind = match input.get("type") {
        None | Some(Value::Null) => Some(BackupStorageType::S3),
        Some(Value::String(s)) => BackupStorageType::parse(s),
        Some(_) => None,
    };
    let rules_kind = kind.unwrap_or(BackupStorageType::S3);
    run_validation(&state, &ctx, &headers, &input, rules_kind, None).await?;
    let kind = rules_kind;
    let is_s3 = kind == BackupStorageType::S3;
    let login = if kind == BackupStorageType::Sftp {
        sftp_login_attributes(&state, &ctx, &input, true).await?
    } else {
        SftpLogin::default()
    };
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let trimmed = |k: &str| input.str(k).unwrap_or_default().trim().to_owned();
    let is_usable = flag(&input, "is_usable").unwrap_or(false);
    // `$body['use_path_style_endpoint'] ?? S3Storage::detectPathStyleEndpoint($body['endpoint'])`
    let use_path_style_endpoint = !is_s3
        || flag(&input, "use_path_style_endpoint")
            .unwrap_or_else(|| s3::detect_path_style_endpoint(input.str("endpoint")));
    let secret = login.secret.clone().unwrap_or_else(|| trimmed("secret"));
    let bucket = trimmed("bucket");
    let region = if is_s3 {
        trimmed("region")
    } else {
        "us-east-1".to_owned()
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
            kind: kind.value(),
            bucket: is_s3.then_some(bucket.as_str()),
            region: &region,
            key_encrypted: &enc.encrypt_string(&trimmed("key")),
            secret_encrypted: &enc.encrypt_string(secret.trim()),
            use_path_style_endpoint,
            skip_tls_verification: is_s3 && flag(&input, "skip_tls_verification").unwrap_or(false),
            private_key_id: login.private_key_id.flatten(),
            sftp_host_key: None,
            is_usable,
        },
        state.now(),
    )
    .await?;
    tracing::info!(target: "audit", event = "api.s3_storage.created", team_id = ctx.team_id, s3_storage_uuid = %uuid, "s3 storage created");
    Ok(created(json!({"uuid": uuid})).into_response())
}

/// `PATCH /s3-storages/{uuid}`. The type cannot change, and only the fields of the storage type
/// are accepted.
pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let row = parts::s3_find(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let kind = BackupStorageType::of(&row);
    run_validation(
        &state,
        &ctx,
        &headers,
        &input,
        kind,
        Some(Saved { row: &row }),
    )
    .await?;
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let saved_plain = |k: &str| parts::str_of(&row, k).and_then(|c| enc.decrypt_string(c).ok());
    let login = if kind == BackupStorageType::Sftp {
        let current_blank = saved_plain("secret").is_none_or(|s| s.trim().is_empty());
        sftp_login_attributes(&state, &ctx, &input, current_blank).await?
    } else {
        SftpLogin::default()
    };
    let allowed = connection_fields(kind);
    let mut values: Vec<(&'static str, ColumnValue)> = Vec::new();
    let mut host_key_reset = false;
    for k in ["name", "description", "endpoint", "bucket", "region"] {
        if !input.has(k) || (!matches!(k, "name" | "description") && !allowed.contains(&k)) {
            continue;
        }
        let text = input.str(k).map(|s| match k {
            "name" | "description" => coolify_domain::naming::strip_tags(Some(s)),
            _ => s.trim().to_owned(),
        });
        // The `endpoint` mutator: another host or port is another server, so the saved SFTP host
        // key no longer applies, and the storage needs a new connection test before it is used.
        if k == "endpoint"
            && st::endpoint_change_resets_host_key(parts::str_of(&row, "endpoint"), text.as_deref())
        {
            host_key_reset = parts::str_of(&row, "sftp_host_key").is_some();
            values.push(("sftp_host_key", ColumnValue::Text(None)));
        }
        values.push((k, ColumnValue::Text(text)));
    }
    let mut encrypted = |k: &'static str, plain: &str| {
        let plain = plain.trim();
        // The `encrypted` cast is dirty only when the decrypted value changes.
        if saved_plain(k).as_deref() != Some(plain) {
            values.push((k, ColumnValue::Text(Some(enc.encrypt_string(plain)))));
        }
    };
    if let Some(plain) = input.str("key") {
        encrypted("key", plain);
    }
    match &login.secret {
        Some(secret) => encrypted("secret", secret),
        // An SFTP storage keeps its password when the request sends an empty one.
        None if kind == BackupStorageType::Sftp => {}
        None => {
            if let Some(plain) = input.str("secret") {
                encrypted("secret", plain);
            }
        }
    }
    if let Some(private_key_id) = login.private_key_id {
        values.push((
            "private_key_id",
            match private_key_id {
                Some(id) => ColumnValue::Int(id),
                None => ColumnValue::Text(None),
            },
        ));
    }
    for k in [
        "use_path_style_endpoint",
        "skip_tls_verification",
        "is_usable",
    ] {
        if k != "is_usable" && !allowed.contains(&k) {
            continue;
        }
        if let Some(b) = flag(&input, k) {
            values.push((k, ColumnValue::Bool(b)));
        } else if k == "is_usable" && host_key_reset {
            // Without the host key, the storage needs a new connection test before it is used.
            values.push((k, ColumnValue::Bool(false)));
        }
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

/// `POST /s3-storages/{uuid}/validate`. S3: lists the bucket (ListObjectsV2). SFTP, FTPS, and
/// WebDAV: lists the backup directory; for SFTP, the first successful validation pins the host key
/// of the server.
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
    // `$storage->testConnection(shouldSave: true, acceptNewHostKey: true)`.
    let result = jobs.test_storage_connection(&row, true).await;
    tracing::info!(target: "audit", event = "api.s3_storage.validated", team_id = ctx.team_id, s3_storage_uuid = %uuid, valid = result.is_ok(), "s3 storage validated");
    Ok(match result {
        Ok(_) => ok(json!({
            "valid": true,
            "message": format!("{} storage connection is valid.", BackupStorageType::of(&row).label()),
        })),
        Err(error) => ok(json!({"valid": false, "message": error})),
    }
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_type_accepts_its_own_fields() {
        assert_eq!(
            connection_fields(BackupStorageType::S3),
            [
                "endpoint",
                "bucket",
                "region",
                "key",
                "secret",
                "use_path_style_endpoint",
                "skip_tls_verification"
            ]
        );
        assert_eq!(
            connection_fields(BackupStorageType::Sftp),
            ["endpoint", "key", "secret", "private_key_uuid"]
        );
        assert_eq!(
            connection_fields(BackupStorageType::Ftps),
            ["endpoint", "key", "secret"]
        );
        assert_eq!(
            connection_fields(BackupStorageType::Webdav),
            ["endpoint", "key", "secret"]
        );
    }

    #[test]
    fn usernames_must_fit_on_a_config_line() {
        assert_eq!(user_rule(&json!("bob@example.com")), None);
        assert_eq!(
            user_rule(&json!("bob\nhost = evil")),
            Some("The :attribute field format is invalid.")
        );
        assert_eq!(
            user_rule(&json!("bo\"b")),
            Some("The :attribute field format is invalid.")
        );
        assert!(blank(None));
        assert!(blank(Some(&json!("  "))));
        assert!(blank(Some(&Value::Null)));
        assert!(!blank(Some(&json!("x"))));
        assert!(!blank(Some(&json!(0))));
    }
}
