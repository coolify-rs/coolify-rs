//! `ServerTransferController`.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::servers;
use serde_json::{Map, Value, json};

use crate::audit::{self, Meta};
use crate::auth::AuthCtx;
use crate::input::{Input, lenient_json, query_param, validated_json};
use crate::response::{ApiError, ApiResult, PhpJson};
use crate::routes::servers::{normalize_ip, valid_server_ip};
use crate::server_transfer::{TransferError, bundle, dg, importer, migrator, text};
use crate::state::AppState;

fn answer(code: StatusCode, value: Value) -> axum::response::Response {
    PhpJson(code, value).into_response()
}

const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Server not found.");

fn validation_failed(errors: Map<String, Value>) -> axum::response::Response {
    answer(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": "Validation failed.", "errors": errors}),
    )
}

fn extra_fields(input: &Input, allowed: &[&str], errors: &mut Map<String, Value>) {
    for key in input.keys() {
        if !allowed.contains(&key.as_str()) {
            errors.insert(key.clone(), json!(["This field is not allowed."]));
        }
    }
}

/// `POST /servers/{uuid}/migrate`.
pub async fn migrate(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    if !ctx.can_read_sensitive {
        return Ok(answer(
            StatusCode::FORBIDDEN,
            json!({"message": "Migrating a server requires a token with read:sensitive (or root) ability and the owner team role."}),
        ));
    }
    let s = servers::find(&state.db, ctx.team_id, &uuid).await?.ok_or(NOT_FOUND)?;
    // `ServerPolicy::transfer`: owner.
    if ctx.role != coolify_domain::Role::Owner {
        return Err(ApiError::Unauthorized);
    }
    let input = validated_json(&headers, &uri, &body)?;
    let mut errors = Map::new();
    for field in ["target_url", "target_token"] {
        let name = field.replace('_', " ");
        match input.get(field) {
            None | Some(Value::Null) => {
                errors.insert(field.to_owned(), json!([format!("The {name} field is required.")]));
            }
            Some(Value::String(v)) if v.trim().is_empty() => {
                errors.insert(field.to_owned(), json!([format!("The {name} field is required.")]));
            }
            Some(Value::String(v)) => {
                if field == "target_url" && !crate::laravel_url::is_url(v) {
                    errors.insert(field.to_owned(), json!([format!("The {name} field must be a valid URL.")]));
                }
            }
            Some(_) => {
                let mut messages = vec![format!("The {name} field must be a string.")];
                if field == "target_url" {
                    messages.push(format!("The {name} field must be a valid URL."));
                }
                errors.insert(field.to_owned(), json!(messages));
            }
        }
    }
    extra_fields(&input, &["target_url", "target_token"], &mut errors);
    if !errors.is_empty() {
        return Ok(validation_failed(errors));
    }
    let ip = s.record.get("ip").map(text).unwrap_or_default();
    let port = s.record.get("port").and_then(Value::as_i64).unwrap_or(22);
    if let Err(e) = migrator::start(
        &state,
        s.id,
        &s.uuid,
        &ip,
        port,
        input.str("target_url").unwrap_or_default(),
        input.str("target_token").unwrap_or_default(),
    )
    .await
    {
        return Ok(answer(StatusCode::UNPROCESSABLE_ENTITY, json!({"message": e})));
    }
    let fresh = servers::find(&state.db, ctx.team_id, &uuid).await?.ok_or(NOT_FOUND)?;
    let migration = fresh
        .record
        .get("server_metadata")
        .and_then(|m| m.get("migration"))
        .cloned()
        .unwrap_or(Value::Null);
    audit::log(
        &state,
        &ctx,
        &meta,
        "api.server.migrate",
        json!({"team_id": ctx.team_id, "server_uuid": s.uuid, "target_url": migration["target_url"]}),
        "info",
    )
    .await;
    Ok(answer(
        StatusCode::ACCEPTED,
        json!({
            "server_uuid": s.uuid,
            "migration": migration,
            "message": format!("Transfer started. Follow it with GET /api/v1/servers/{}/transfer.", s.uuid),
        }),
    ))
}

/// `GET /servers/{uuid}/transfer`.
pub async fn status(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = servers::find(&state.db, ctx.team_id, &uuid).await?.ok_or(NOT_FOUND)?;
    let metadata = s.record.get("server_metadata").cloned().unwrap_or(Value::Null);
    // The log and the error can name hosts and credentials problems, so members see only the state.
    let mut migration = dg(&metadata, "migration").cloned().unwrap_or(Value::Null);
    if !ctx.is_team_admin()
        && let Value::Object(m) = &mut migration
    {
        m.remove("log");
        m.remove("error");
    }
    let mut transfer = Map::new();
    if let Some(Value::Object(t)) = dg(&metadata, "transfer") {
        // `only()` keeps the order of its list.
        for key in ["status", "export_id", "imported_at", "claimed_at", "transferred_at", "target_instance_url"] {
            if let Some(v) = t.get(key) {
                transfer.insert(key.to_owned(), v.clone());
            }
        }
    }
    Ok(answer(
        StatusCode::OK,
        json!({
            "server_uuid": s.uuid,
            "transfer": if transfer.is_empty() { json!([]) } else { Value::Object(transfer) },
            "migration": migration,
        }),
    ))
}

/// All values of a repeated query parameter `name[]`, as `$request->query($name)`.
fn query_list(uri: &Uri, name: &str) -> Option<Vec<String>> {
    let query = uri.query()?;
    let mut out = None;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(k);
        if key == format!("{name}[]") || key.starts_with(&format!("{name}[")) {
            out.get_or_insert_with(Vec::new).push(percent_decode(v));
        }
    }
    out
}

fn percent_decode(v: &str) -> String {
    let bytes = v.replace('+', " ").into_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `GET /servers/import/check`.
pub async fn check(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    uri: Uri,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let server_uuid = query_param(&uri, "server_uuid");
    let ip = query_param(&uri, "ip");
    let port = query_param(&uri, "port");
    let schema = query_param(&uri, "schema_version");
    let mut errors = Map::new();
    if server_uuid.as_deref().is_none_or(|s| s.trim().is_empty()) {
        errors.insert("server_uuid".into(), json!(["The server uuid field is required."]));
    }
    match ip.as_deref() {
        None => {
            errors.insert("ip".into(), json!(["The ip field is required."]));
        }
        Some(v) if v.trim().is_empty() => {
            errors.insert("ip".into(), json!(["The ip field is required."]));
        }
        Some(v) => {
            if let Some(m) = valid_server_ip(&json!(v)) {
                errors.insert("ip".into(), json!([m.replace(":attribute", "ip")]));
            }
        }
    }
    if let Some(p) = port.as_deref().filter(|p| !p.is_empty()) {
        match p.parse::<i64>() {
            Err(_) => {
                errors.insert("port".into(), json!(["The port field must be an integer.", "The port field must be between 1 and 65535."]));
            }
            Ok(n) if !(1..=65535).contains(&n) => {
                errors.insert("port".into(), json!(["The port field must be between 1 and 65535."]));
            }
            Ok(_) => {}
        }
    }
    if schema.as_deref().filter(|s| !s.is_empty()).is_some_and(|s| s.parse::<i64>().is_err()) {
        errors.insert("schema_version".into(), json!(["The schema version field must be an integer."]));
    }
    let github = query_list(&uri, "system_github_apps").unwrap_or_default();
    let gitlab = query_list(&uri, "system_gitlab_apps").unwrap_or_default();
    for (name, label, list) in [("system_github_apps", "system github apps", &github), ("system_gitlab_apps", "system gitlab apps", &gitlab)] {
        if list.len() > 100 {
            errors.insert(name.to_owned(), json!([format!("The {label} field must not have more than 100 items.")]));
        }
        for (i, v) in list.iter().enumerate() {
            if v.chars().count() > 64 {
                errors.insert(format!("{name}.{i}"), json!([format!("The {name}.{i} field must not be greater than 64 characters.")]));
            }
        }
    }
    if !errors.is_empty() {
        return Ok(validation_failed(errors));
    }
    let (server_uuid, ip) = (server_uuid.unwrap_or_default(), ip.unwrap_or_default());

    // System-wide git sources are not transferred. Applications re-link only to one with the same uuid here.
    let missing = |table: &'static str, list: Vec<String>| {
        let state = state.clone();
        async move {
            let mut out = Vec::new();
            for uuid in list {
                let found: bool = sqlx::query_scalar(&format!(
                    "SELECT EXISTS (SELECT 1 FROM {table} WHERE is_system_wide = true AND uuid = $1)"
                ))
                .bind(&uuid)
                .fetch_one(state.db.pool())
                .await
                .unwrap_or(false);
                if !found {
                    out.push(uuid);
                }
            }
            out
        }
    };
    let missing_sources = json!({
        "github": missing("github_apps", github).await,
        "gitlab": missing("gitlab_apps", gitlab).await,
    });

    let version = schema.filter(|s| !s.is_empty()).map_or(bundle::SCHEMA_VERSION, |s| s.parse().unwrap_or(0));
    let ip = ip.trim().to_owned();
    let port = port.filter(|p| !p.is_empty()).and_then(|p| p.parse::<i64>().ok()).unwrap_or(22);

    // The target connects to the server over SSH when it takes over management.
    let probe_ip = normalize_ip(&ip);
    let probe_host = if probe_ip.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{probe_ip}]")
    } else {
        probe_ip
    };
    let connected = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect(format!("{probe_host}:{port}")),
    )
    .await
    .is_ok_and(|r| r.is_ok());

    let mut checks = vec![json!({
        "label": "the transfer bundle version is supported",
        "ok": version == bundle::SCHEMA_VERSION,
        "message": (version != bundle::SCHEMA_VERSION).then(|| format!(
            "This instance reads transfer bundle version {}, but the source sends version {version}. Upgrade both instances to the same Coolify version.",
            bundle::SCHEMA_VERSION
        )),
    })];
    let remote = importer::import_checks(&state, &server_uuid, &ip, ctx.team_id, true)
        .await
        .map_err(|_| ApiError::Message(StatusCode::INTERNAL_SERVER_ERROR, "Server Error"))?;
    for c in remote {
        checks.push(json!({"label": c["label"], "ok": c["ok"], "message": c["message"]}));
    }
    checks.push(json!({
        "label": format!("the target can reach the SSH port of the server ({ip}:{port})"),
        "ok": connected,
        "message": (!connected).then(|| format!("This instance cannot reach the server at {ip}:{port}. Allow SSH from this instance to the server, then transfer again.")),
    }));
    let failed: Vec<String> = checks
        .iter()
        .filter(|c| c["ok"] == json!(false))
        .map(|c| text(&c["message"]))
        .collect();
    let code = if failed.is_empty() { StatusCode::OK } else { StatusCode::UNPROCESSABLE_ENTITY };
    Ok(answer(
        code,
        json!({
            "message": if failed.is_empty() { "This instance can import the server.".to_owned() } else { failed.join(" ") },
            "checks": checks,
            "missing_system_sources": missing_sources,
        }),
    ))
}

/// `$request->boolean($key, $default)`.
fn request_boolean(input: &Input, key: &str, default: bool) -> bool {
    match input.get(key) {
        None => default,
        Some(Value::Bool(b)) => *b,
        Some(Value::Null) => false,
        Some(v) => matches!(text(v).to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes"),
    }
}

/// `POST /servers/import`.
pub async fn import(
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
    let input = validated_json(&headers, &uri, &body)?;
    let mut errors = Map::new();
    match input.get("bundle") {
        Some(Value::Object(m)) if !m.is_empty() => {}
        Some(Value::Array(a)) if !a.is_empty() => {}
        None | Some(Value::Null) | Some(Value::Array(_)) | Some(Value::Object(_)) => {
            errors.insert("bundle".into(), json!(["The bundle field is required."]));
        }
        Some(Value::String(s)) if s.trim().is_empty() => {
            errors.insert("bundle".into(), json!(["The bundle field is required."]));
        }
        Some(_) => {
            errors.insert("bundle".into(), json!(["The bundle field must be an array."]));
        }
    }
    for field in ["dry_run", "preserve_uuids", "adopt_mode"] {
        if let Some(v) = input.get(field)
            && !v.is_null()
            && !matches!(v, Value::Bool(_))
            && !matches!(text(v).as_str(), "0" | "1")
        {
            errors.insert(field.to_owned(), json!([format!("The {} field must be true or false.", field.replace('_', " "))]));
        }
    }
    extra_fields(&input, &["bundle", "dry_run", "preserve_uuids", "adopt_mode"], &mut errors);
    if !errors.is_empty() {
        return Ok(validation_failed(errors));
    }
    let bundle_value = match input.get("bundle") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    let result = importer::import(
        &state,
        &bundle_value,
        ctx.team_id,
        request_boolean(&input, "dry_run", false),
        request_boolean(&input, "preserve_uuids", true),
        request_boolean(&input, "adopt_mode", true),
    )
    .await;
    match result {
        Err(e) => {
            let mut payload = Map::new();
            payload.insert("message".into(), json!(e.message()));
            if let TransferError::Validation(_) = e {
                payload.insert("errors".into(), Value::Object(e.errors().unwrap_or_default()));
            }
            Ok(answer(StatusCode::UNPROCESSABLE_ENTITY, Value::Object(payload)))
        }
        Ok(result) => {
            let dry = result.get("dry_run") == Some(&json!(true));
            audit::log(
                &state,
                &ctx,
                &meta,
                "api.server.import",
                json!({"team_id": ctx.team_id, "server_uuid": result["server_uuid"], "export_id": result["export_id"], "dry_run": dry}),
                "info",
            )
            .await;
            Ok(answer(
                if dry { StatusCode::OK } else { StatusCode::CREATED },
                Value::Object(result),
            ))
        }
    }
}

/// `POST /servers/{uuid}/transfer/complete`.
pub async fn complete(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let s = servers::find(&state.db, ctx.team_id, &uuid).await?.ok_or(NOT_FOUND)?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let input = lenient_json(&headers, &uri, &body);
    let mut errors = Map::new();
    for field in ["export_id", "target_instance_url"] {
        if let Some(v) = input.get(field)
            && !v.is_null()
            && !v.is_string()
        {
            errors.insert(field.to_owned(), json!([format!("The {} field must be a string.", field.replace('_', " "))]));
        }
    }
    if !errors.is_empty() {
        return Ok(validation_failed(errors));
    }
    let url = input.str("target_instance_url").filter(|u| !u.trim().is_empty());
    let target = match url {
        None => None,
        Some(u) => match migrator::validate_instance_url(&state, u).await {
            Ok(v) => Some(v),
            Err(e) => {
                let mut errors = Map::new();
                errors.insert("target_instance_url".into(), json!([e.text()]));
                return Ok(validation_failed(errors));
            }
        },
    };
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let export_id = input.str("export_id");
    let result = coolify_jobs::server_transfer::mark_transferred(jobs, s.id, export_id, target.as_deref(), false).await;
    match result {
        Err(e) => Ok(answer(StatusCode::UNPROCESSABLE_ENTITY, json!({"message": e}))),
        Ok(v) => {
            audit::log(
                &state,
                &ctx,
                &meta,
                "api.server.transfer_complete",
                json!({"team_id": ctx.team_id, "server_uuid": s.uuid, "export_id": export_id}),
                "info",
            )
            .await;
            Ok(answer(StatusCode::OK, v))
        }
    }
}
