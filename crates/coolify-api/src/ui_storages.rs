//! Page endpoints of `App\Livewire\Storage\{Index, Create, Form, Show, Resources}`: the backup
//! storages of a team (S3, SFTP, FTPS, and WebDAV).

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use coolify_db::database_parts::{self as parts, NewS3Storage};
use coolify_db::records::Record;
use coolify_db::servers::ColumnValue as C;
use coolify_domain::naming::{DESCRIPTION_PATTERN, NAME_PATTERN, strip_tags};
use coolify_jobs::databases::file_storage;
use coolify_jobs::databases::s3;
use coolify_jobs::databases::storage_type::{self as st, BackupStorageType};
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::state::AppState;
use crate::ui_notifications::{toast_error, unauthorized};

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/storages", get(index).post(create))
        .route(
            "/ui/storages/{uuid}",
            get(show).patch(update).delete(destroy),
        )
        .route("/ui/storages/{uuid}/test", post(test_connection))
        .route("/ui/storages/{uuid}/reset-host-key", post(reset_host_key))
        .route("/ui/storages/{uuid}/resources", get(resources))
        .route("/ui/storages/{uuid}/resources/move", post(move_backup))
        .route(
            "/ui/storages/{uuid}/resources/disable",
            post(disable_backup),
        )
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

fn text<'a>(r: &'a Record, k: &str) -> Option<&'a str> {
    r.get(k).and_then(Value::as_str)
}

/// `e()`.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#039;")
}

fn host_of(headers: &HeaderMap) -> String {
    headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost")
        .to_owned()
}

/// What the validation of a form needs from the storage it edits.
#[derive(Clone, Copy)]
struct FormContext<'a> {
    kind: BackupStorageType,
    team_id: i64,
    /// `Create`: the name is required.
    name_required: bool,
    /// `$this->hasSavedSecret`: an empty secret keeps the saved one.
    has_saved_secret: bool,
    /// `$this->storage->getOriginal('endpoint')`.
    saved_endpoint: Option<&'a str>,
    /// `$this->storage->trustedInternalHosts()`.
    trusted_minio: bool,
}

/// The form's validation, in rule order, with the messages and attribute names of `Create` and
/// `Form` (`rules()`, `messages()`, `validationAttributes()`).
async fn validation_errors(
    state: &AppState,
    headers: &HeaderMap,
    values: &Map<String, Value>,
    context: FormContext<'_>,
) -> Vec<String> {
    let mut e = Vec::new();
    let get = |k: &str| values.get(k).and_then(Value::as_str).unwrap_or_default();
    let is_s3 = context.kind == BackupStorageType::S3;
    let name = get("name");
    if name.trim().is_empty() {
        if context.name_required {
            e.push("The Name field is required.".to_owned());
        }
    } else {
        let len = name.chars().count();
        if len < 3 {
            e.push("The name must be at least 3 characters.".to_owned());
        }
        if len > 255 {
            e.push("The name may not be greater than 255 characters.".to_owned());
        }
        if !NAME_PATTERN.is_match(name) {
            e.push("The name may only contain letters (including Unicode), numbers, spaces, and these characters: - _ . / @ & ( ) # , : +".to_owned());
        }
    }
    let description = get("description");
    if !description.trim().is_empty() {
        if description.chars().count() > 255 {
            e.push("The description may not be greater than 255 characters.".to_owned());
        }
        if !DESCRIPTION_PATTERN.is_match(description) {
            e.push("The description may only contain letters (including Unicode), numbers, spaces, and common punctuation: - _ . , ! ? ( ) ' \" + = * / @ &".to_owned());
        }
    }

    // `connectionRules()`: the endpoint comes first for every type.
    let attribute = if is_s3 { "Endpoint" } else { "Server" };
    let endpoint = get("endpoint");
    if endpoint.trim().is_empty() {
        e.push(format!("The {attribute} field is required."));
    } else {
        if endpoint.chars().count() > 255 {
            e.push(if is_s3 {
                "The Endpoint may not be greater than 255 characters.".to_owned()
            } else {
                "The Server field must not be greater than 255 characters.".to_owned()
            });
        }
        e.extend(file_storage::endpoint_credentials_error(
            endpoint,
            context.saved_endpoint,
            attribute,
        ));
        e.extend(file_storage::endpoint_path_error(endpoint, attribute));
        let mut policy = s3::UrlPolicy::load(&state.db).await;
        if context.trusted_minio {
            policy.allowed.push("coolify-minio".to_owned());
        }
        let settings = format!("http://{}/settings/advanced", host_of(headers));
        if let Some(m) = s3::endpoint_error_for(
            endpoint,
            "endpoint",
            &policy,
            &settings,
            context.kind.endpoint_schemes(),
        )
        .await
        {
            // `:attribute` becomes the validation attribute; the private-target message has the raw name.
            e.push(if m.contains("points to") {
                m
            } else {
                m.replace("The endpoint", &format!("The {attribute}"))
            });
        }
    }

    if is_s3 {
        let bucket = get("bucket");
        if bucket.trim().is_empty() {
            e.push("The Bucket field is required.".to_owned());
        } else if !s3::valid_bucket_name(bucket) {
            e.push("The Bucket must be a valid S3 bucket name: 3-63 letters, numbers, dots, or hyphens; start and end with a letter or number; no consecutive dots, dot-hyphen pairs, or IP address format.".to_owned());
        }
        for (k, label) in [
            ("region", "Region"),
            ("key", "Access Key"),
            ("secret", "Secret Key"),
        ] {
            let v = get(k);
            if v.trim().is_empty() {
                if !(k == "secret" && context.has_saved_secret) {
                    e.push(format!("The {label} field is required."));
                }
            } else if v.chars().count() > 255 {
                e.push(format!(
                    "The {label} may not be greater than 255 characters."
                ));
            }
        }
        return e;
    }

    let user = get("key");
    if user.trim().is_empty() {
        e.push("The Username field is required.".to_owned());
    } else {
        if user.chars().count() > 255 {
            e.push("The Username field must not be greater than 255 characters.".to_owned());
        }
        if !file_storage::valid_user(user) {
            e.push("The Username must not contain line breaks or quotes.".to_owned());
        }
    }
    // `withSftpLoginRules()`: an SFTP storage logs in with a password or a private key of the team.
    let uses_key = context.kind == BackupStorageType::Sftp && get("sftp_auth") == "key";
    let secret = get("secret");
    if secret.trim().is_empty() {
        if !uses_key && !context.has_saved_secret {
            e.push("The Password field is required.".to_owned());
        }
    } else if secret.chars().count() > 255 {
        e.push("The Password field must not be greater than 255 characters.".to_owned());
    }
    if context.kind == BackupStorageType::Sftp {
        let auth = get("sftp_auth");
        if auth.trim().is_empty() {
            e.push("The sftp auth field is required.".to_owned());
        } else if auth != "password" && auth != "key" {
            e.push("The selected sftp auth is invalid.".to_owned());
        }
        match values.get("private_key_id") {
            None | Some(Value::Null) => {
                if uses_key {
                    e.push("Select the private key to log in with.".to_owned());
                }
            }
            Some(v) => match v.as_i64() {
                None => e.push("The Private key field must be an integer.".to_owned()),
                Some(id) => {
                    let known = parts::s3_login_keys(&state.db, context.team_id)
                        .await
                        .is_ok_and(|keys| keys.iter().any(|(key_id, _, _)| *key_id == id));
                    if !known {
                        e.push("The selected Private key is invalid.".to_owned());
                    }
                }
            },
        }
    }
    e
}

fn summarize(errors: &[String]) -> Option<String> {
    crate::ui_notifications::summarize(errors)
}

/// `normalizeEndpoint` of `Create`.
fn normalize_endpoint(endpoint: &str) -> String {
    let endpoint = endpoint.trim();
    let lower = endpoint.to_ascii_lowercase();
    let has_scheme = lower.starts_with("http:")
        || lower.starts_with("https:")
        || regex::Regex::new(r"^[a-z][a-z0-9+.-]*://")
            .map(|r| r.is_match(&lower))
            .unwrap_or(false);
    let endpoint = if has_scheme {
        endpoint.to_owned()
    } else {
        format!("https://{endpoint}")
    };
    if endpoint.contains("digitaloceanspaces.com")
        && let Some(host) = endpoint
            .split("://")
            .nth(1)
            .and_then(|r| r.split(['/', ':', '?', '#']).next())
        && let Some((_, rest)) = host.split_once('.')
        && rest.ends_with(".digitaloceanspaces.com")
        && rest.matches('.').count() == 2
    {
        return format!("https://{rest}");
    }
    endpoint
}

/// `composeEndpoint()`: the endpoint from the form fields of the storage type. An SFTP or FTPS
/// storage has a host, a port and a directory (`server_parts`); the other types send the URL.
fn compose_endpoint(kind: BackupStorageType, values: &mut Map<String, Value>, normalize: bool) {
    let endpoint = if matches!(kind, BackupStorageType::Sftp | BackupStorageType::Ftps) {
        let part = |k: &str| match values.get("server_parts").and_then(|p| p.get(k)) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };
        st::server_endpoint(kind, &part("host"), &part("port"), &part("path"))
    } else {
        let endpoint = values
            .get("endpoint")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if normalize {
            normalize_endpoint(endpoint)
        } else {
            endpoint.to_owned()
        }
    };
    values.insert("endpoint".into(), json!(endpoint));
}

/// `selectedPrivateKeyId()`: the private key ID to save: only when an SFTP storage logs in with a
/// key.
fn selected_private_key_id(kind: BackupStorageType, values: &Map<String, Value>) -> Option<i64> {
    (kind == BackupStorageType::Sftp
        && values.get("sftp_auth").and_then(Value::as_str) == Some("key"))
    .then(|| values.get("private_key_id").and_then(Value::as_i64))
    .flatten()
}

/// The secret to save or test (`secretToUse()`): none with a private key, the new one from the
/// form, else the saved one.
fn secret_to_use(
    state: &AppState,
    base: Option<&Record>,
    values: &Map<String, Value>,
    private_key_id: Option<i64>,
) -> Result<String, ApiError> {
    if private_key_id.is_some() {
        return Ok(String::new());
    }
    let new = values
        .get("secret")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if !new.is_empty() {
        return Ok(new.to_owned());
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    Ok(base
        .and_then(|row| text(row, "secret"))
        .and_then(|c| enc.decrypt_string(c).ok())
        .unwrap_or_default())
}

/// The candidate row `testConnection` works on.
fn candidate(
    state: &AppState,
    kind: BackupStorageType,
    team_id: i64,
    base: Option<&Record>,
    values: &Map<String, Value>,
) -> Result<Record, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let mut r = base.cloned().unwrap_or_default();
    let get = |k: &str| {
        values
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let is_s3 = kind == BackupStorageType::S3;
    r.insert("type".into(), json!(kind.value()));
    r.insert("team_id".into(), json!(team_id));
    let endpoint = get("endpoint");
    // The saved host key applies while the host and the port stay the same; the endpoint setter
    // removes it otherwise.
    if st::endpoint_change_resets_host_key(base.and_then(|b| text(b, "endpoint")), Some(&endpoint))
    {
        r.insert("sftp_host_key".into(), Value::Null);
    }
    r.insert("endpoint".into(), Value::String(endpoint.clone()));
    if is_s3 {
        for k in ["bucket", "region"] {
            r.insert(k.into(), Value::String(get(k)));
        }
    } else if base.is_none() {
        r.insert("bucket".into(), Value::Null);
        r.insert("region".into(), json!("us-east-1"));
    }
    // `usePathStyleEndpoint`: the form's choice, else the saved style, else the detected one.
    let path_style = values
        .get("use_path_style_endpoint")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| match base {
            Some(row) => s3::uses_path_style_endpoint(row),
            None => !is_s3 || s3::detect_path_style_endpoint(Some(&endpoint)),
        });
    r.insert("use_path_style_endpoint".into(), Value::Bool(path_style));
    r.insert(
        "skip_tls_verification".into(),
        Value::Bool(
            is_s3
                && values
                    .get("skip_tls_verification")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
        ),
    );
    r.insert("key".into(), Value::String(enc.encrypt_string(&get("key"))));
    let private_key_id = selected_private_key_id(kind, values);
    r.insert("private_key_id".into(), json!(private_key_id));
    r.insert(
        "secret".into(),
        Value::String(enc.encrypt_string(&secret_to_use(state, base, values, private_key_id)?)),
    );
    Ok(r)
}

fn jobs(state: &AppState) -> Result<&coolify_jobs::Jobs, ApiError> {
    state.jobs.as_ref().ok_or(ApiError::NotPorted)
}

/// `$storage->description ?: ($storage->isS3() ? 'S3-compatible storage' : $storage->type->label().' storage')`
/// is the page's; the list sends the type.
async fn index(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    let rows = parts::s3_list(&state.db, ctx.team_id).await?;
    Ok(ok(json!({
        "storages": rows.iter().map(|r| {
            let kind = BackupStorageType::of(r);
            json!({
                "uuid": r.get("uuid"),
                "name": r.get("name"),
                "description": r.get("description"),
                "type": kind.value(),
                "type_label": kind.label(),
                "is_usable": r.get("is_usable").and_then(Value::as_bool).unwrap_or(false),
            })
        }).collect::<Vec<_>>(),
        "can_create": ctx.is_team_admin(),
        // `BackupStorageType::options()` and the private keys of the SFTP login in `Create`.
        "types": BackupStorageType::options(),
        "private_keys": if ctx.is_team_admin() {
            private_key_options(&state, ctx.team_id).await?
        } else {
            Vec::new()
        },
    }))
    .into_response())
}

/// `privateKeyOptions()` with `selectedPublicKey()` of each key: the public key to add to
/// authorized_keys on the SFTP server.
async fn private_key_options(state: &AppState, team_id: i64) -> Result<Vec<Value>, ApiError> {
    let mut options = Vec::new();
    for (id, _, name) in parts::s3_login_keys(&state.db, team_id).await? {
        let cipher: Option<String> =
            sqlx::query_scalar("SELECT private_key FROM private_keys WHERE id = $1")
                .bind(id)
                .fetch_optional(state.db.pool())
                .await
                .map_err(|_| ApiError::Internal)?;
        let public_key = state
            .encrypter
            .as_ref()
            .zip(cipher)
            .and_then(|(enc, c)| enc.decrypt_string(&c).ok())
            .and_then(|plain| coolify_compat::sshkey::inspect(&plain))
            .map(|info| info.public_key);
        options.push(json!({"value": id, "label": name, "public_key": public_key}));
    }
    Ok(options)
}

/// `Create::submit`: every failure is the same toast.
async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> ApiResult {
    let failed = |description: String| -> Response {
        PhpJson(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": "Failed to create storage.", "details": description}),
        )
        .into_response()
    };
    if !ctx.is_team_admin() {
        return Ok(failed("This action is unauthorized.".to_owned()));
    }
    let Some(values) = body.as_object_mut() else {
        return Err(ApiError::Internal);
    };
    // `storageType()`: `BackupStorageType::tryFrom($this->type) ?? BackupStorageType::S3`.
    let raw_type = values
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("s3")
        .to_owned();
    let kind = BackupStorageType::parse(&raw_type).unwrap_or(BackupStorageType::S3);
    compose_endpoint(kind, values, true);
    let mut errors = Vec::new();
    if BackupStorageType::parse(&raw_type).is_none() {
        errors.push("The selected type is invalid.".to_owned());
    }
    errors.extend(
        validation_errors(
            &state,
            &headers,
            values,
            FormContext {
                kind,
                team_id: ctx.team_id,
                name_required: true,
                has_saved_secret: false,
                saved_endpoint: None,
                trusted_minio: false,
            },
        )
        .await,
    );
    if let Some(m) = summarize(&errors) {
        let settings = format!(
            "http://{}/settings/advanced#endpoint-section",
            host_of(&headers)
        );
        let link = format!(
            "<a class=\"font-medium underline\" href=\"{}\">Set them here.</a>",
            escape_html(&settings)
        );
        let plain = format!("http://{}/settings/advanced", host_of(&headers));
        return Ok(failed(
            escape_html(&m)
                .replace(&escape_html(&settings), &link)
                .replace(&escape_html(&plain), &link),
        ));
    }
    let is_s3 = kind == BackupStorageType::S3;
    let row = candidate(&state, kind, ctx.team_id, None, values)?;
    // `$this->storage->testConnection(acceptNewHostKey: true)`.
    let check = jobs(&state)?.check_storage_connection(&row, true).await;
    if let Err(e) = check.result {
        return Ok(failed(escape_html(&e)));
    }
    let path_style = check.path_style;
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let get = |k: &str| {
        values
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let name = strip_tags(Some(&get("name")));
    let description = values
        .get("description")
        .and_then(Value::as_str)
        .map(|d| strip_tags(Some(d)));
    let private_key_id = selected_private_key_id(kind, values);
    let secret = secret_to_use(&state, None, values, private_key_id)?;
    let bucket = get("bucket");
    let region = if is_s3 {
        get("region")
    } else {
        "us-east-1".to_owned()
    };
    let skip_tls_verification = is_s3
        && values
            .get("skip_tls_verification")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let uuid = parts::s3_create(
        &state.db,
        &NewS3Storage {
            team_id: ctx.team_id,
            name: &name,
            description: description.as_deref(),
            endpoint: Some(&get("endpoint")),
            kind: kind.value(),
            bucket: is_s3.then_some(bucket.as_str()),
            region: &region,
            key_encrypted: &enc.encrypt_string(&get("key")),
            secret_encrypted: &enc.encrypt_string(&secret),
            use_path_style_endpoint: path_style,
            skip_tls_verification,
            private_key_id,
            // The host key that the connection test of the new SFTP storage recorded.
            sftp_host_key: check.new_sftp_host_key.as_deref(),
            is_usable: true,
        },
        state.now(),
    )
    .await?;
    let mut changes = Map::new();
    let mut add = |k: &str, v: Value| {
        changes.insert(k.to_owned(), json!({"old": null, "new": v}));
    };
    add("type", json!(kind.value()));
    add("name", json!(name));
    add("description", json!(description));
    add("region", json!(region));
    if let Some(id) = private_key_id {
        add("private_key_id", json!(id));
    }
    add("bucket", json!(is_s3.then_some(bucket.as_str())));
    add("endpoint", json!(get("endpoint")));
    add("use_path_style_endpoint", json!(path_style));
    add("skip_tls_verification", json!(skip_tls_verification));
    add("team_id", json!(ctx.team_id));
    if let Some(host_key) = &check.new_sftp_host_key {
        add("sftp_host_key", json!(host_key));
    }
    add("unusable_email_sent", json!(false));
    add("is_usable", json!(true));
    add("uuid", json!(uuid));
    crate::audit::mutation(
        &state,
        &ctx,
        &meta,
        "s3_storage",
        "created",
        json!({
            "team_id": ctx.team_id,
            "s3_storage_uuid": uuid,
            "s3_storage_name": name,
            "changed_fields": [],
        }),
        Value::Object(changes),
    )
    .await;
    Ok(ok(json!({"uuid": uuid})).into_response())
}

/// `syncData()` of `Form`, the page header and the backup count of `Show`.
async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let Some(row) = parts::s3_find(&state.db, ctx.team_id, &uuid).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    };
    let can_update = ctx.is_team_admin();
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let plain = |k: &str| -> Result<String, ApiError> {
        text(&row, k).map_or(Ok(String::new()), |c| {
            enc.decrypt_string(c).map_err(|_| ApiError::Internal)
        })
    };
    let kind = BackupStorageType::of(&row);
    let id = parts::id_of(&row);
    let backup_count: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM scheduled_database_backups b WHERE b.s3_storage_id = $1 \
           OR EXISTS (SELECT 1 FROM scheduled_database_backup_s3_storage p WHERE p.scheduled_database_backup_id = b.id AND p.s3_storage_id = $1)) \
         + (SELECT count(*) FROM scheduled_volume_backups b WHERE b.s3_storage_id = $1 \
           OR EXISTS (SELECT 1 FROM scheduled_volume_backup_s3_storage p WHERE p.scheduled_volume_backup_id = b.id AND p.s3_storage_id = $1))",
    )
    .bind(id)
    .fetch_one(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    let all = parts::s3_list(&state.db, ctx.team_id).await?;
    let mut items: Vec<(String, String)> = all
        .iter()
        .map(|r| {
            (
                text(r, "name").unwrap_or_default().to_owned(),
                text(r, "uuid").unwrap_or_default().to_owned(),
            )
        })
        .collect();
    items.sort();
    let (host, port, path) = st::split_server_endpoint(text(&row, "endpoint"));
    let private_key_id = row.get("private_key_id").and_then(Value::as_i64);
    let host_key = text(&row, "sftp_host_key").filter(|k| !k.trim().is_empty());
    let is_sftp = kind == BackupStorageType::Sftp;
    Ok(ok(json!({
        "uuid": uuid,
        "type": kind.value(),
        "type_label": kind.label(),
        "default_port": kind.default_port(),
        "name": row.get("name"),
        "description": row.get("description"),
        "endpoint": row.get("endpoint"),
        "server_parts": {"host": host, "port": port, "path": path},
        "bucket": row.get("bucket"),
        "region": row.get("region"),
        "use_path_style_endpoint": s3::uses_path_style_endpoint(&row),
        // A saved style that detection would not pick is a user choice, so it stays when the
        // endpoint changes.
        "use_path_style_endpoint_changed": s3::uses_path_style_endpoint(&row)
            != s3::detect_path_style_endpoint(text(&row, "endpoint")),
        "skip_tls_verification": parts::bool_of(&row, "skip_tls_verification"),
        "key": if can_update { plain("key")? } else { String::new() },
        // A new secret key or password. The saved one never goes to the browser; empty keeps it.
        "secret": "",
        "has_saved_secret": can_update && !plain("secret")?.trim().is_empty(),
        "private_key_id": private_key_id,
        "sftp_auth": if private_key_id.is_some() { "key" } else { "password" },
        "private_keys": if is_sftp && can_update {
            private_key_options(&state, ctx.team_id).await?
        } else {
            Vec::new()
        },
        // `$storage->sftpHostKeyFingerprint()`.
        "sftp_host_key_fingerprint": host_key.map(st::sftp_host_key_fingerprint),
        "is_usable": row.get("is_usable").and_then(Value::as_bool).unwrap_or(false),
        "can_update": can_update,
        "backup_count": backup_count,
        "storages": items.iter().map(|(n, u)| json!({"name": n, "uuid": u})).collect::<Vec<_>>(),
    }))
    .into_response())
}

fn form_context<'a>(state: &AppState, row: &'a Record) -> FormContext<'a> {
    let has_saved_secret = state
        .encrypter
        .as_ref()
        .zip(text(row, "secret"))
        .and_then(|(enc, c)| enc.decrypt_string(c).ok())
        .is_some_and(|s| !s.trim().is_empty());
    FormContext {
        kind: BackupStorageType::of(row),
        team_id: row.get("team_id").and_then(Value::as_i64).unwrap_or(-1),
        name_required: false,
        has_saved_secret,
        saved_endpoint: text(row, "endpoint"),
        trusted_minio: text(row, "uuid") == Some("minio")
            && st::endpoint_host(text(row, "endpoint").unwrap_or_default()) == "coolify-minio",
    }
}

/// `Form::submit`.
async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    Path(uuid): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let Some(row) = parts::s3_find(&state.db, ctx.team_id, &uuid).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let kind = BackupStorageType::of(&row);
    let is_s3 = kind == BackupStorageType::S3;
    let mut values = body.as_object().cloned().unwrap_or_default();
    compose_endpoint(kind, &mut values, false);
    let errors = validation_errors(&state, &headers, &values, form_context(&state, &row)).await;
    if let Some(m) = summarize(&errors) {
        return Ok(toast_error(StatusCode::UNPROCESSABLE_ENTITY, m));
    }
    // The transaction rolls back when the connection test fails, so test first.
    let test = candidate(&state, kind, ctx.team_id, Some(&row), &values)?;
    let chosen_style = s3::uses_path_style_endpoint(&test);
    // `$this->storage->testConnection(shouldSave: false, acceptNewHostKey: true)`.
    let check = jobs(&state)?.check_storage_connection(&test, true).await;
    if let Err(e) = check.result {
        return Ok(toast_error(StatusCode::UNPROCESSABLE_ENTITY, e));
    }
    let path_style = check.path_style;
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let get = |k: &str| {
        values
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let mut changes: Vec<(&'static str, C)> = Vec::new();
    let mut audit = Map::new();
    let mut changed: Vec<String> = Vec::new();
    let name = strip_tags(Some(&get("name")));
    let description = values
        .get("description")
        .and_then(Value::as_str)
        .filter(|d| !d.is_empty())
        .map(|d| strip_tags(Some(d)));
    let mut text_field = |col: &'static str, new: Option<String>| {
        if text(&row, col) != new.as_deref() {
            audit.insert(col.to_owned(), json!({"old": row.get(col), "new": new}));
            changed.push(col.to_owned());
            changes.push((col, C::Text(new)));
        }
    };
    text_field("name", Some(name.clone()));
    text_field("description", description);
    if is_s3 {
        text_field("region", Some(get("region")));
    }
    let plain = |k: &str| text(&row, k).and_then(|c| enc.decrypt_string(c).ok());
    let private_key_id = selected_private_key_id(kind, &values);
    let new_secret = secret_to_use(&state, Some(&row), &values, private_key_id)?;
    let key_changed = plain("key").as_deref() != Some(get("key").as_str());
    let secret_changed = plain("secret").as_deref() != Some(new_secret.as_str());
    if is_s3 {
        text_field("bucket", Some(get("bucket")));
    }
    text_field("endpoint", Some(get("endpoint")));
    // The `endpoint` mutator: another host or port is another server, so the saved SFTP host key
    // no longer applies. The connection test above recorded the key of the new server.
    let saved_host_key = text(&row, "sftp_host_key")
        .filter(|k| !k.trim().is_empty())
        .map(str::to_owned);
    let kept_host_key = text(&test, "sftp_host_key")
        .filter(|k| !k.trim().is_empty())
        .map(str::to_owned);
    let new_host_key = check.new_sftp_host_key.clone().or(kept_host_key);
    if new_host_key != saved_host_key {
        text_field("sftp_host_key", new_host_key);
    }
    let saved_style = s3::uses_path_style_endpoint(&row);
    if chosen_style != saved_style {
        audit.insert(
            "use_path_style_endpoint".to_owned(),
            json!({"old": saved_style, "new": chosen_style}),
        );
        changed.push("use_path_style_endpoint".to_owned());
    }
    let saved_skip_tls = parts::bool_of(&row, "skip_tls_verification");
    let skip_tls = parts::bool_of(&test, "skip_tls_verification");
    if skip_tls != saved_skip_tls {
        audit.insert(
            "skip_tls_verification".to_owned(),
            json!({"old": saved_skip_tls, "new": skip_tls}),
        );
        changed.push("skip_tls_verification".to_owned());
        changes.push(("skip_tls_verification", C::Bool(skip_tls)));
    }
    let saved_key_id = row.get("private_key_id").and_then(Value::as_i64);
    if private_key_id != saved_key_id {
        audit.insert(
            "private_key_id".to_owned(),
            json!({"old": saved_key_id, "new": private_key_id}),
        );
        changed.push("private_key_id".to_owned());
        changes.push((
            "private_key_id",
            match private_key_id {
                Some(id) => C::Int(id),
                None => C::Text(None),
            },
        ));
    }
    if key_changed {
        changes.push(("key", C::Text(Some(enc.encrypt_string(&get("key"))))));
    }
    if secret_changed {
        changes.push(("secret", C::Text(Some(enc.encrypt_string(&new_secret)))));
    }
    // The second `save()` of the transaction: the connection works, maybe with
    // virtual-hosted-style URLs.
    let mut second: Vec<String> = Vec::new();
    let mut second_audit = Map::new();
    if path_style != saved_style {
        changes.push(("use_path_style_endpoint", C::Bool(path_style)));
    }
    if path_style != chosen_style {
        second_audit.insert(
            "use_path_style_endpoint".into(),
            json!({"old": chosen_style, "new": path_style}),
        );
        second.push("use_path_style_endpoint".to_owned());
    }
    if !row
        .get("is_usable")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        second_audit.insert("is_usable".into(), json!({"old": false, "new": true}));
        second.push("is_usable".to_owned());
        changes.push(("is_usable", C::Bool(true)));
    }
    if row
        .get("unusable_email_sent")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        second_audit.insert(
            "unusable_email_sent".into(),
            json!({"old": true, "new": false}),
        );
        second.push("unusable_email_sent".to_owned());
        changes.push(("unusable_email_sent", C::Bool(false)));
    }
    parts::update_row(
        &state.db,
        "s3_storages",
        parts::id_of(&row),
        &changes,
        state.now(),
    )
    .await?;
    if !changed.is_empty() {
        crate::audit::mutation(
            &state,
            &ctx,
            &meta,
            "s3_storage",
            "updated",
            json!({
                "team_id": ctx.team_id,
                "s3_storage_uuid": uuid,
                "s3_storage_name": name,
                "changed_fields": changed,
            }),
            Value::Object(audit),
        )
        .await;
    }
    if !second.is_empty() {
        crate::audit::mutation(
            &state,
            &ctx,
            &meta,
            "s3_storage",
            "updated",
            json!({
                "team_id": ctx.team_id,
                "s3_storage_uuid": uuid,
                "s3_storage_name": name,
                "changed_fields": second,
            }),
            Value::Object(second_audit),
        )
        .await;
    }
    Ok(ok(json!({
        "message": "Storage settings updated and connection verified.",
        "use_path_style_endpoint": path_style,
    }))
    .into_response())
}

/// `Form::testConnection`.
async fn test_connection(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let Some(row) = parts::s3_find(&state.db, ctx.team_id, &uuid).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    };
    let mut values = body.as_object().cloned().unwrap_or_default();
    let fail = |m: String| {
        PhpJson(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": "Failed to test connection.", "details": m}),
        )
        .into_response()
    };
    if !ctx.is_team_admin() {
        return Ok(fail("This action is unauthorized.".to_owned()));
    }
    let kind = BackupStorageType::of(&row);
    compose_endpoint(kind, &mut values, false);
    let test = candidate(&state, kind, ctx.team_id, Some(&row), &values)?;
    let id = parts::id_of(&row);
    let was_usable = row
        .get("is_usable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let chosen_style = s3::uses_path_style_endpoint(&test);
    // `$testedStorage->testConnection(acceptNewHostKey: true)`.
    let check = jobs(&state)?.check_storage_connection(&test, true).await;
    let path_style = check.path_style;
    let switched = chosen_style && !path_style;
    // `$this->storage->save()` with the tested `is_usable`.
    if was_usable != check.result.is_ok() {
        crate::audit::mutation(
            &state,
            &ctx,
            &meta,
            "s3_storage",
            "updated",
            json!({
                "team_id": ctx.team_id,
                "s3_storage_uuid": uuid,
                "s3_storage_name": row.get("name"),
                "changed_fields": ["is_usable"],
            }),
            json!({"is_usable": {"old": was_usable, "new": check.result.is_ok()}}),
        )
        .await;
    }
    match check.result {
        Ok(()) => {
            parts::s3_set_usable(&state.db, id, true, state.now()).await?;
            // The tested host key is saved while the tested endpoint is the saved one.
            let mut fingerprint = text(&row, "sftp_host_key")
                .filter(|k| !k.trim().is_empty())
                .map(st::sftp_host_key_fingerprint);
            if let Some(host_key) = &check.new_sftp_host_key
                && text(&test, "endpoint") == text(&row, "endpoint")
            {
                parts::update_row(
                    &state.db,
                    "s3_storages",
                    id,
                    &[("sftp_host_key", C::Text(Some(host_key.clone())))],
                    state.now(),
                )
                .await?;
                fingerprint = Some(st::sftp_host_key_fingerprint(host_key));
            }
            Ok(ok(json!({
                "message": "Connection is working.",
                "details": if switched {
                    "The provider rejects path-style URLs, so \"Use path-style URLs\" is now off. Save to keep it."
                } else if kind == BackupStorageType::S3 {
                    "Tested with \"ListObjectsV2\" action."
                } else {
                    "Tested by listing the backup directory."
                },
                "is_usable": true,
                "use_path_style_endpoint": path_style,
                "sftp_host_key_fingerprint": fingerprint,
            }))
            .into_response())
        }
        Err(e) => {
            parts::s3_set_usable(&state.db, id, false, state.now()).await?;
            Ok(fail(e))
        }
    }
}

/// `Form::resetHostKey`: removes the saved SFTP host key, for a server whose key changed on
/// purpose. The next connection test that an admin starts (Validate) saves the key that the
/// server presents then; backups do not.
async fn reset_host_key(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
) -> ApiResult {
    let Some(row) = parts::s3_find(&state.db, ctx.team_id, &uuid).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let was_usable = parts::bool_of(&row, "is_usable");
    let had_key = text(&row, "sftp_host_key").is_some();
    parts::update_row(
        &state.db,
        "s3_storages",
        parts::id_of(&row),
        &[
            ("sftp_host_key", C::Text(None)),
            ("is_usable", C::Bool(false)),
        ],
        state.now(),
    )
    .await?;
    let mut changed = Vec::new();
    let mut audit = Map::new();
    if had_key {
        changed.push("sftp_host_key");
        audit.insert(
            "sftp_host_key".into(),
            json!({"old": row.get("sftp_host_key"), "new": null}),
        );
    }
    if was_usable {
        changed.push("is_usable");
        audit.insert("is_usable".into(), json!({"old": true, "new": false}));
    }
    if !changed.is_empty() {
        crate::audit::mutation(
            &state,
            &ctx,
            &meta,
            "s3_storage",
            "updated",
            json!({
                "team_id": ctx.team_id,
                "s3_storage_uuid": uuid,
                "s3_storage_name": row.get("name"),
                "changed_fields": changed,
            }),
            Value::Object(audit),
        )
        .await;
    }
    Ok(ok(json!({
        "message": "Host key removed.",
        "details": "Validate the connection to save the current host key of the server.",
        "is_usable": false,
    }))
    .into_response())
}

/// `Show::delete`.
async fn destroy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
) -> ApiResult {
    let Some(row) = parts::s3_find(&state.db, ctx.team_id, &uuid).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    };
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    parts::s3_delete(&state.db, parts::id_of(&row), state.now()).await?;
    let old = |k: &str| json!({"old": row.get(k), "new": null});
    crate::audit::mutation(
        &state,
        &ctx,
        &meta,
        "s3_storage",
        "deleted",
        json!({
            "team_id": ctx.team_id,
            "s3_storage_uuid": uuid,
            "s3_storage_name": row.get("name"),
            "changed_fields": [],
        }),
        json!({
            "name": old("name"),
            "description": old("description"),
            "region": old("region"),
            "bucket": old("bucket"),
            "endpoint": old("endpoint"),
            "is_usable": old("is_usable"),
            "team_id": old("team_id"),
            "uuid": old("uuid"),
        }),
    )
    .await;
    Ok(ok(json!({})).into_response())
}

type ServiceDatabaseLink = (String, Option<String>, Option<String>, Option<String>);
type VolumeRow = (Option<String>, Option<String>, Option<String>, Option<i64>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Database,
    Volume,
}

impl Family {
    fn table(self) -> &'static str {
        match self {
            Self::Database => "scheduled_database_backups",
            Self::Volume => "scheduled_volume_backups",
        }
    }
    fn pivot(self) -> (&'static str, &'static str) {
        match self {
            Self::Database => (
                "scheduled_database_backup_s3_storage",
                "scheduled_database_backup_id",
            ),
            Self::Volume => (
                "scheduled_volume_backup_s3_storage",
                "scheduled_volume_backup_id",
            ),
        }
    }
}

/// `$query->usingS3Storage($id)->where('save_s3', true)`.
async fn backups_using(
    state: &AppState,
    family: Family,
    storage_id: i64,
) -> Result<Vec<Record>, ApiError> {
    let (pivot, fk) = family.pivot();
    let table = family.table();
    let sql = format!(
        "SELECT row_to_json(b)::text FROM {table} b WHERE b.save_s3 = true AND (b.s3_storage_id = $1 \
         OR EXISTS (SELECT 1 FROM {pivot} p WHERE p.{fk} = b.id AND p.s3_storage_id = $1))"
    );
    let rows: Vec<String> = sqlx::query_scalar(&sql)
        .bind(storage_id)
        .fetch_all(state.db.pool())
        .await
        .map_err(|_| ApiError::Internal)?;
    rows.iter()
        .map(|t| coolify_db::records::parse_row(t).map_err(|_| ApiError::Internal))
        .collect()
}

fn class_table(class: &str) -> Option<&'static str> {
    Some(match class.rsplit('\\').next()? {
        "StandalonePostgresql" => "standalone_postgresqls",
        "StandaloneRedis" => "standalone_redis",
        "StandaloneMongodb" => "standalone_mongodbs",
        "StandaloneMysql" => "standalone_mysqls",
        "StandaloneMariadb" => "standalone_mariadbs",
        "StandaloneKeydb" => "standalone_keydbs",
        "StandaloneDragonfly" => "standalone_dragonflies",
        "StandaloneClickhouse" => "standalone_clickhouses",
        "StandaloneSqlite" => "standalone_sqlites",
        "ServiceDatabase" => "service_databases",
        _ => return None,
    })
}

/// `$backup->database` with where its links go.
async fn database_of(state: &AppState, r: &Record) -> Result<Value, ApiError> {
    let class = text(r, "database_type").unwrap_or_default();
    let id = r.get("database_id").and_then(Value::as_i64).unwrap_or(0);
    let Some(table) = class_table(class) else {
        return Ok(json!({"name": "Deleted database"}));
    };
    if table == "service_databases" {
        let row: Option<ServiceDatabaseLink> = sqlx::query_as(
            "SELECT d.name, s.uuid, e.uuid, p.uuid FROM service_databases d LEFT JOIN services s ON s.id = d.service_id \
             LEFT JOIN environments e ON e.id = s.environment_id LEFT JOIN projects p ON p.id = e.project_id WHERE d.id = $1",
        )
        .bind(id)
        .fetch_optional(state.db.pool())
        .await
        .map_err(|_| ApiError::Internal)?;
        return Ok(match row {
            None => json!({"name": "Deleted database"}),
            Some((name, s, e, p)) => {
                let link = match (p, e, s) {
                    (Some(p), Some(e), Some(s)) => {
                        Some(format!("/project/{p}/environment/{e}/service/{s}"))
                    }
                    _ => None,
                };
                json!({"name": name, "link": link})
            }
        });
    }
    if table == "standalone_postgresqls" && id == 0 {
        let name: Option<String> =
            sqlx::query_scalar("SELECT name FROM standalone_postgresqls WHERE id = 0")
                .fetch_optional(state.db.pool())
                .await
                .map_err(|_| ApiError::Internal)?;
        return Ok(
            json!({"name": name.unwrap_or_else(|| "Deleted database".to_owned()), "link": "/settings/backup"}),
        );
    }
    let sql = format!(
        "SELECT d.name, d.uuid, e.uuid, p.uuid FROM {table} d LEFT JOIN environments e ON e.id = d.environment_id \
         LEFT JOIN projects p ON p.id = e.project_id WHERE d.id = $1 AND d.deleted_at IS NULL"
    );
    let row: Option<(String, String, Option<String>, Option<String>)> = sqlx::query_as(&sql)
        .bind(id)
        .fetch_optional(state.db.pool())
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(match row {
        None => json!({"name": "Deleted database"}),
        Some((name, uuid, e, p)) => match (p, e) {
            (Some(p), Some(e)) => {
                let base = format!("/project/{p}/environment/{e}/database/{uuid}/backups");
                json!({"name": name, "uuid": uuid, "link": base})
            }
            _ => json!({"name": name, "uuid": uuid}),
        },
    })
}

/// `targetName`, `targetType` and the resource name of a volume backup.
async fn volume_target(state: &AppState, r: &Record) -> Result<Value, ApiError> {
    let class = text(r, "backupable_type").unwrap_or_default();
    let id = r.get("backupable_id").and_then(Value::as_i64).unwrap_or(0);
    let (table, column, kind) = if class.ends_with("LocalFileVolume") {
        ("local_file_volumes", "fs_path", "Directory")
    } else if class.ends_with("LocalPersistentVolume") {
        ("local_persistent_volumes", "name", "Volume")
    } else {
        return Ok(json!({"name": "Unknown storage", "type": "Volume"}));
    };
    let sql =
        format!("SELECT {column}, uuid, resource_type, resource_id FROM {table} WHERE id = $1");
    let row: Option<VolumeRow> = sqlx::query_as(&sql)
        .bind(id)
        .fetch_optional(state.db.pool())
        .await
        .map_err(|_| ApiError::Internal)?;
    let Some((name, uuid, rtype, rid)) = row else {
        return Ok(json!({"name": "Unknown storage", "type": kind}));
    };
    let resource = match (rtype, rid) {
        (Some(t), Some(i)) => {
            let table = match t.rsplit('\\').next().unwrap_or_default() {
                "Application" => Some("applications"),
                "Service" => Some("services"),
                "ServiceApplication" => Some("service_applications"),
                other => class_table(other),
            };
            if let Some(table) = table {
                let sql = format!("SELECT row_to_json(t)::text FROM {table} t WHERE id = $1");
                let text_row: Option<String> = sqlx::query_scalar(&sql)
                    .bind(i)
                    .fetch_optional(state.db.pool())
                    .await
                    .map_err(|_| ApiError::Internal)?;
                text_row
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                    .map(|v| {
                        json!({
                            "name": v.get("human_name").filter(|h| !h.is_null()).or(v.get("name")),
                            "uuid": v.get("uuid"),
                            "type": t,
                        })
                    })
            } else {
                None
            }
        }
        _ => None,
    };
    Ok(json!({"name": name, "uuid": uuid, "type": kind, "resource": resource}))
}

/// The data of `Resources::render`.
async fn resources(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let Some(row) = parts::s3_find(&state.db, ctx.team_id, &uuid).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    };
    let id = parts::id_of(&row);
    let mut backups = Vec::new();
    for r in backups_using(&state, Family::Database, id).await? {
        let db = database_of(&state, &r).await?;
        backups.push(json!({
            "id": r.get("id"),
            "uuid": r.get("uuid"),
            "frequency": r.get("frequency"),
            "enabled": r.get("enabled"),
            "group": format!("{}-{}", text(&r, "database_type").unwrap_or_default(), r.get("database_id").and_then(Value::as_i64).unwrap_or(0)),
            "database": db,
        }));
    }
    // `groupBy` keeps the group's first appearance.
    let mut groups: Vec<String> = Vec::new();
    for b in &backups {
        let g = b["group"].as_str().unwrap_or_default().to_owned();
        if !groups.contains(&g) {
            groups.push(g);
        }
    }
    backups.sort_by_key(|b| {
        groups
            .iter()
            .position(|g| g == b["group"].as_str().unwrap_or_default())
    });
    let mut volumes = Vec::new();
    for r in backups_using(&state, Family::Volume, id).await? {
        volumes.push(json!({
            "id": r.get("id"),
            "uuid": r.get("uuid"),
            "frequency": r.get("frequency"),
            "enabled": r.get("enabled"),
            "target": volume_target(&state, &r).await?,
        }));
    }
    let mut all: Vec<Value> = parts::s3_list(&state.db, ctx.team_id)
        .await?
        .iter()
        .map(|r| {
            json!({
                "id": r.get("id"),
                "name": r.get("name"),
                "is_usable": r.get("is_usable").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect();
    all.sort_by_key(|s| s["name"].as_str().unwrap_or_default().to_owned());
    Ok(ok(json!({
        "storage_id": id,
        "backups": backups,
        "volume_backups": volumes,
        "storages": all,
    }))
    .into_response())
}

/// `replaceStorage`: returns the changed field names.
async fn replace_storage(
    state: &AppState,
    family: Family,
    backup: &Record,
    storage_id: i64,
    new_storage: Option<i64>,
) -> Result<Vec<&'static str>, ApiError> {
    let (pivot, fk) = family.pivot();
    let backup_id = backup.get("id").and_then(Value::as_i64).unwrap_or(0);
    let primary = backup.get("s3_storage_id").and_then(Value::as_i64);
    // `selectedS3Storages`: the pivot by name with the primary first, or the primary alone.
    let sql = format!(
        "SELECT s.id FROM s3_storages s JOIN {pivot} p ON p.s3_storage_id = s.id WHERE p.{fk} = $1 ORDER BY s.name"
    );
    let mut ids: Vec<i64> = sqlx::query_scalar(&sql)
        .bind(backup_id)
        .fetch_all(state.db.pool())
        .await
        .map_err(|_| ApiError::Internal)?;
    if ids.is_empty() {
        ids.extend(primary);
    } else if let Some(p) = primary
        && let Some(i) = ids.iter().position(|x| *x == p)
    {
        let v = ids.remove(i);
        ids.insert(0, v);
    }
    let mut next: Vec<i64> = Vec::new();
    for id in ids {
        let id = if id == storage_id {
            new_storage
        } else {
            Some(id)
        };
        if let Some(id) = id
            && !next.contains(&id)
        {
            next.push(id);
        }
    }
    let now = state.now();
    let new_primary = primary
        .filter(|p| next.contains(p))
        .or_else(|| next.first().copied());
    // `syncS3Storages`.
    let table = family.table();
    let mut tx = state
        .db
        .pool()
        .begin()
        .await
        .map_err(|_| ApiError::Internal)?;
    sqlx::query(&format!(
        "DELETE FROM {pivot} WHERE {fk} = $1 AND NOT (s3_storage_id = ANY($2))"
    ))
    .bind(backup_id)
    .bind(&next)
    .execute(&mut *tx)
    .await
    .map_err(|_| ApiError::Internal)?;
    for id in &next {
        sqlx::query(&format!(
            "INSERT INTO {pivot} ({fk}, s3_storage_id, created_at, updated_at) SELECT $1, $2, $3, $3 \
             WHERE NOT EXISTS (SELECT 1 FROM {pivot} WHERE {fk} = $1 AND s3_storage_id = $2)"
        ))
        .bind(backup_id)
        .bind(id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|_| ApiError::Internal)?;
    }
    let mut changed = vec!["s3_storages"];
    if new_primary != primary {
        changed.push("s3_storage_id");
        sqlx::query(&format!(
            "UPDATE {table} SET s3_storage_id = $1, updated_at = $2 WHERE id = $3"
        ))
        .bind(new_primary)
        .bind(now)
        .bind(backup_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| ApiError::Internal)?;
    }
    if next.is_empty()
        && backup
            .get("save_s3")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        changed.push("save_s3");
        sqlx::query(&format!(
            "UPDATE {table} SET save_s3 = false, updated_at = $1 WHERE id = $2"
        ))
        .bind(now)
        .bind(backup_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| ApiError::Internal)?;
    }
    tx.commit().await.map_err(|_| ApiError::Internal)?;
    Ok(changed)
}

/// `auditDatabaseBackupUpdated` / `auditVolumeBackupUpdated`.
async fn audit_backup(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    family: Family,
    backup: &Record,
    changed: &[&str],
) -> Result<(), ApiError> {
    let backup_uuid = backup.get("uuid").cloned().unwrap_or(Value::Null);
    match family {
        Family::Database => {
            let db = database_of(state, backup).await?;
            crate::audit::log(
                state,
                ctx,
                meta,
                "ui.database.backup_schedule_updated",
                json!({
                    "team_id": ctx.team_id,
                    "database_uuid": db.get("uuid"),
                    "database_name": db.get("uuid").map(|_| db["name"].clone()),
                    "backup_uuid": backup_uuid,
                    "changed_fields": changed,
                }),
                "info",
            )
            .await;
        }
        Family::Volume => {
            let target = volume_target(state, backup).await?;
            let kind = target["resource"]["type"].as_str().unwrap_or_default();
            crate::audit::log(
                state,
                ctx,
                meta,
                "ui.volume_backup.schedule_set",
                json!({
                    "team_id": ctx.team_id,
                    "resource_type": if kind.ends_with("Application") { "application" } else if kind.ends_with("Service") { "service" } else { "database" },
                    "resource_uuid": target["resource"].get("uuid"),
                    "resource_name": target["resource"].get("name"),
                    "storage_uuid": target.get("uuid"),
                    "backup_uuid": backup_uuid,
                    "changed_fields": changed,
                }),
                "info",
            )
            .await;
        }
    }
    Ok(())
}

async fn resolve_backup(
    state: &AppState,
    ctx: &AuthCtx,
    uuid: &str,
    body: &Value,
) -> Result<Option<(i64, Family, Record)>, ApiError> {
    let Some(row) = parts::s3_find(&state.db, ctx.team_id, uuid).await? else {
        return Ok(None);
    };
    let storage_id = parts::id_of(&row);
    let family = if body.get("volume").and_then(Value::as_bool).unwrap_or(false) {
        Family::Volume
    } else {
        Family::Database
    };
    let backup_id = body.get("backup").and_then(Value::as_i64).unwrap_or(0);
    // `usingS3Storage` without the `save_s3` filter: `firstOrFail`.
    let (pivot, fk) = family.pivot();
    let table = family.table();
    let sql = format!(
        "SELECT row_to_json(b)::text FROM {table} b WHERE b.id = $2 AND (b.s3_storage_id = $1 \
         OR EXISTS (SELECT 1 FROM {pivot} p WHERE p.{fk} = b.id AND p.s3_storage_id = $1))"
    );
    let found: Option<String> = sqlx::query_scalar(&sql)
        .bind(storage_id)
        .bind(backup_id)
        .fetch_optional(state.db.pool())
        .await
        .map_err(|_| ApiError::Internal)?;
    let Some(found) = found else {
        return Ok(None);
    };
    let backup = coolify_db::records::parse_row(&found).map_err(|_| ApiError::Internal)?;
    Ok(Some((storage_id, family, backup)))
}

/// `moveBackup` / `moveVolumeBackup`.
async fn move_backup(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let Some((storage_id, family, backup)) = resolve_backup(&state, &ctx, &uuid, &body).await?
    else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    };
    let target = body.get("target").and_then(Value::as_i64);
    let details = |title: &str, d: Option<&str>| {
        PhpJson(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": title, "details": d}),
        )
        .into_response()
    };
    let Some(target) = target.filter(|t| *t != storage_id) else {
        return Ok(details(
            "No change.",
            Some("The backup is already using this storage."),
        ));
    };
    let Some(new) = parts::s3_list(&state.db, ctx.team_id)
        .await?
        .into_iter()
        .find(|r| parts::id_of(r) == target)
    else {
        return Ok(details("Storage not found.", None));
    };
    let changed = replace_storage(&state, family, &backup, storage_id, Some(target)).await?;
    audit_backup(&state, &ctx, &meta, family, &backup, &changed).await?;
    Ok(ok(json!({
        "message": "Backup moved.",
        "details": format!("Moved to {}.", text(&new, "name").unwrap_or_default()),
    }))
    .into_response())
}

/// `disableS3` / `disableVolumeS3`.
async fn disable_backup(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let Some((storage_id, family, backup)) = resolve_backup(&state, &ctx, &uuid, &body).await?
    else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found."));
    };
    let changed = replace_storage(&state, family, &backup, storage_id, None).await?;
    audit_backup(&state, &ctx, &meta, family, &backup, &changed).await?;
    let still_on = !changed.contains(&"save_s3")
        && backup
            .get("save_s3")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    Ok(ok(json!({
        "message": "Storage removed.",
        "details": if still_on {
            "This storage was removed from the schedule destinations."
        } else {
            "S3 backup has been disabled for this schedule."
        },
    }))
    .into_response())
}
