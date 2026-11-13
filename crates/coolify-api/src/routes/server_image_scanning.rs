//! `ServerImageScanningController` (app/Http/Controllers/Api): the image vulnerability scanning
//! settings of a server and the state of its scanner.
//!
//! - `GET /servers/{uuid}/image-scanning` (`viewSecurity`: a team admin or owner).
//! - `PATCH /servers/{uuid}/image-scanning` (`update`: a team admin or owner): turns scanning
//!   on or off, or changes the limits of the scanner.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::image_scans::{self, ServerScanSettings};
use coolify_db::servers::{self, ServerRow};
use coolify_jobs::image_scan::ScanServer;
use coolify_jobs::image_scan::commands::IMAGE_SCAN_TIMEOUT_OPTIONS_SECONDS;
use coolify_jobs::image_scan::prepare::{self, SetScanningError};
use coolify_jobs::image_scan::views::{iso8601, parse_time};
use serde_json::{Map, Value, json};

use crate::audit::{self, Meta};
use crate::auth::AuthCtx;
use crate::input::validated_json;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::state::AppState;

/// Scanner limits that the API can change; each one has fixed options.
const LIMIT_FIELDS: [&str; 4] = [
    "image_scan_memory_mb",
    "image_scan_syft_timeout",
    "image_scan_grype_timeout",
    "image_scan_cpus",
];

const ALLOWED_FIELDS: [&str; 5] = [
    "is_image_scan_enabled",
    "image_scan_memory_mb",
    "image_scan_syft_timeout",
    "image_scan_grype_timeout",
    "image_scan_cpus",
];

/// The audit action of each scanner limit.
fn limit_audit_action(field: &str) -> &'static str {
    match field {
        "image_scan_memory_mb" => "api.server.image_scanner_memory_changed",
        "image_scan_syft_timeout" => "api.server.image_scanner_syft_timeout_changed",
        "image_scan_grype_timeout" => "api.server.image_scanner_grype_timeout_changed",
        _ => "api.server.image_scanner_cpus_changed",
    }
}

const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Server not found.");

fn internal(e: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %e, "server image scanning request failed");
    ApiError::Internal
}

/// `transform($server)`.
async fn transform(state: &AppState, server_id: i64) -> Result<Value, ApiError> {
    let settings = image_scans::server_scan_settings(&state.db, server_id)
        .await?
        .unwrap_or_else(ServerScanSettings::default);
    Ok(json!({
        "is_image_scan_enabled": settings.is_image_scan_enabled,
        "image_scan_memory_mb": settings.image_scan_memory_mb,
        "image_scan_syft_timeout": settings.image_scan_syft_timeout,
        "image_scan_grype_timeout": settings.image_scan_grype_timeout,
        "image_scan_cpus": settings.image_scan_cpus,
        "scanner_state": prepare::state(server_id),
        "scanner_failed_step": prepare::failed_step(server_id),
        "scanner_built_at": prepare::database_built_at(server_id)
            .and_then(|t| parse_time(&t))
            .map(iso8601),
        "scanner_disk_used_bytes": prepare::disk_used_bytes(server_id),
    }))
}

async fn find_server(state: &AppState, ctx: &AuthCtx, uuid: &str) -> Result<ServerRow, ApiError> {
    servers::find(&state.db, ctx.team_id, uuid)
        .await?
        .ok_or(NOT_FOUND)
}

/// `GET /api/v1/servers/{uuid}/image-scanning`.
pub async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let server = find_server(&state, &ctx, &uuid).await?;
    // `ServerPolicy::viewSecurity`: an admin or owner of the server's team.
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    Ok(ok(transform(&state, server.id).await?).into_response())
}

/// `(string) $value` of a JSON number as `Rule::in` compares it.
fn number_text(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

/// The value of an `integer` or `numeric` field, as Laravel reads it: a number, or a string that
/// is one.
fn number(value: &Value, integer: bool) -> Option<f64> {
    let parsed = match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) if coolify_compat::yaml::php_is_numeric(s) => s.trim().parse().ok(),
        _ => None,
    }?;
    (!integer || parsed.fract() == 0.0).then_some(parsed)
}

/// `boolean`: true, false, 0, 1, "0", "1".
fn boolean(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        Value::Number(n) if n.as_i64() == Some(0) => Some(false),
        Value::Number(n) if n.as_i64() == Some(1) => Some(true),
        Value::String(s) if s == "0" => Some(false),
        Value::String(s) if s == "1" => Some(true),
        _ => None,
    }
}

fn join_options(options: &[f64]) -> String {
    options
        .iter()
        .map(|o| number_text(*o))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The validation of `update()`: errors by field, in rule order, then the fields that are not
/// allowed.
fn validate_update(
    input: &Map<String, Value>,
    memory_options: &[i64],
    cpu_options: &[f64],
) -> Map<String, Value> {
    let mut errors = Map::new();
    let mut push = |field: &str, message: String| {
        if let Some(list) = errors
            .entry(field.to_owned())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
        {
            list.push(Value::String(message));
        }
    };
    let present = |key: &str| input.get(key).is_some_and(|v| !v.is_null());

    match input.get("is_image_scan_enabled") {
        None | Some(Value::Null) if !LIMIT_FIELDS.iter().any(|f| present(f)) => push(
            "is_image_scan_enabled",
            "The is image scan enabled field is required when none of image scan memory mb / image scan syft timeout / image scan grype timeout / image scan cpus are present.".to_owned(),
        ),
        Some(v) if !v.is_null() && boolean(v).is_none() => push(
            "is_image_scan_enabled",
            "The is image scan enabled field must be true or false.".to_owned(),
        ),
        _ => {}
    }

    let memory: Vec<f64> = memory_options.iter().map(|o| *o as f64).collect();
    let timeouts: Vec<f64> = IMAGE_SCAN_TIMEOUT_OPTIONS_SECONDS
        .iter()
        .map(|o| *o as f64)
        .collect();
    let checks: [(&str, bool, &[f64], Option<String>); 4] = [
        (
            "image_scan_memory_mb",
            true,
            &memory,
            Some(format!(
                "This server allows: {}. A higher limit is more than the memory of the server.",
                join_options(&memory)
            )),
        ),
        ("image_scan_syft_timeout", true, &timeouts, None),
        ("image_scan_grype_timeout", true, &timeouts, None),
        (
            "image_scan_cpus",
            false,
            cpu_options,
            Some(format!(
                "This server allows: {}. A higher limit is more than the CPU count of the server.",
                join_options(cpu_options)
            )),
        ),
    ];
    for (field, integer, options, in_message) in checks {
        let Some(value) = input.get(field).filter(|v| !v.is_null()) else {
            continue;
        };
        let attribute = field.replace('_', " ");
        let Some(n) = number(value, integer) else {
            push(
                field,
                if integer {
                    format!("The {attribute} field must be an integer.")
                } else {
                    format!("The {attribute} field must be a number.")
                },
            );
            continue;
        };
        if !options.iter().any(|o| number_text(*o) == number_text(n)) {
            push(
                field,
                in_message.unwrap_or_else(|| format!("The selected {attribute} is invalid.")),
            );
        }
    }

    for key in input.keys() {
        if !ALLOWED_FIELDS.contains(&key.as_str()) {
            push(key, "This field is not allowed.".to_owned());
        }
    }
    errors
}

/// `PATCH /api/v1/servers/{uuid}/image-scanning`.
pub async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    meta: Meta,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    let server = find_server(&state, &ctx, &uuid).await?;
    // `ServerPolicy::update`: an admin or owner of the server's team.
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let scan_server = ScanServer::load(jobs, server.id).await.map_err(internal)?;
    let host = scan_server.host();
    let errors = validate_update(&input.0, &host.memory_options_mb(), &host.cpu_options());
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }

    let context = json!({
        "team_id": ctx.team_id,
        "server_uuid": server.uuid,
        "server_name": server.record.get("name"),
    });
    let limits: Vec<&str> = LIMIT_FIELDS
        .iter()
        .copied()
        .filter(|f| input.has(f))
        .collect();
    if !limits.is_empty() {
        if let Err(refused) = prepare::ensure_managed_here(&scan_server) {
            return Ok(PhpJson(StatusCode::CONFLICT, json!({"message": refused.0})).into_response());
        }
        let int = |key: &str| {
            input
                .get(key)
                .and_then(|v| number(v, true))
                .map(|n| n as i64)
        };
        image_scans::views::update_scanner_limits(
            &state.db,
            server.id,
            int("image_scan_memory_mb"),
            int("image_scan_syft_timeout"),
            int("image_scan_grype_timeout"),
            input.get("image_scan_cpus").and_then(|v| number(v, false)),
            state.now(),
        )
        .await?;
        for field in &limits {
            audit::log(
                &state,
                &ctx,
                &meta,
                limit_audit_action(field),
                context.clone(),
                "info",
            )
            .await;
        }
    }
    if let Some(value) = input.get("is_image_scan_enabled") {
        let enabled = value.as_bool().or_else(|| boolean(value)).unwrap_or(false);
        match prepare::set_server_image_scanning(jobs, &scan_server, enabled).await {
            Ok(()) => {}
            Err(SetScanningError::Refused(refused)) => {
                return Ok(
                    PhpJson(StatusCode::CONFLICT, json!({"message": refused.0})).into_response()
                );
            }
            Err(SetScanningError::Other(e)) => return Err(internal(e)),
        }
        let event = if enabled {
            "api.server.image_scanning_enabled"
        } else {
            "api.server.image_scanning_disabled"
        };
        audit::log(&state, &ctx, &meta, event, context.clone(), "info").await;
    }

    Ok(ok(transform(&state, server.id).await?).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_messages() {
        let input = |v: Value| v.as_object().cloned().unwrap();
        let errors = validate_update(&input(json!({})), &[1024, 2048], &[0.5, 1.0]);
        assert!(errors.contains_key("is_image_scan_enabled"));
        let errors = validate_update(
            &input(json!({"image_scan_memory_mb": 4096, "image_scan_cpus": 2, "foo": 1})),
            &[1024, 2048],
            &[0.5, 1.0],
        );
        assert_eq!(
            errors["image_scan_memory_mb"],
            json!([
                "This server allows: 1024, 2048. A higher limit is more than the memory of the server."
            ])
        );
        assert_eq!(
            errors["image_scan_cpus"],
            json!([
                "This server allows: 0.5, 1. A higher limit is more than the CPU count of the server."
            ])
        );
        assert_eq!(errors["foo"], json!(["This field is not allowed."]));
        assert!(!errors.contains_key("is_image_scan_enabled"));
        let ok = validate_update(
            &input(
                json!({"image_scan_cpus": "1.0", "image_scan_syft_timeout": 300, "is_image_scan_enabled": true}),
            ),
            &[1024],
            &[0.5, 1.0],
        );
        assert!(ok.is_empty(), "{ok:?}");
        let bad = validate_update(
            &input(json!({"image_scan_grype_timeout": 301})),
            &[1024],
            &[1.0],
        );
        assert_eq!(
            bad["image_scan_grype_timeout"],
            json!(["The selected image scan grype timeout is invalid."])
        );
    }
}
