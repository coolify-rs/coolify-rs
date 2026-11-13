//! The browser-side parts of image scanning that are not pages:
//!
//! - `ImageScanSbomController`: the SBOM download of a current scan
//!   (`/project/{project}/environment/{environment}/{application|service|database}/{uuid}/vulnerabilities/sbom/{scan_uuid}/{format}`,
//!   session authentication).
//! - The coolify-api side of `UpdateResourceImage` for the Security page ("Update to …", "Pull
//!   latest and restart", a merged service update): the `update` authorization and the
//!   `Auditable` rows of the changed models, around `coolify_jobs::image_scan::update`.
//!   [`update_resource_image`] and [`update_resource_images`] are what the page actions call.

use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Router};
use coolify_db::image_scans::{self, SERVICE_MORPH, ScanOwner};
use coolify_db::records::Record;
use coolify_domain::grants::AccessLevel;
use coolify_jobs::image_scan::sbom_document;
use coolify_jobs::image_scan::update::{self, UpdateError, UpdatePart};
use coolify_jobs::image_scan::views;
use serde_json::{Map, Value, json};

use crate::audit::{self, Meta};
use crate::auth::AuthCtx;
use crate::response::ApiError;
use crate::routes::vulnerabilities::can_access;
use crate::state::AppState;

/// The SBOM routes (`project.{application|service|database}.vulnerabilities.sbom`), behind the
/// session guard. The project segment is named `{uuid}` like `/project/{uuid}/icon` of the
/// canvas, because the router refuses two names at one position.
pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/project/{uuid}/environment/{environment_uuid}/application/{resource_uuid}/vulnerabilities/sbom/{scan_uuid}/{format}",
            get(sbom_application),
        )
        .route(
            "/project/{uuid}/environment/{environment_uuid}/service/{resource_uuid}/vulnerabilities/sbom/{scan_uuid}/{format}",
            get(sbom_service),
        )
        .route(
            "/project/{uuid}/environment/{environment_uuid}/database/{resource_uuid}/vulnerabilities/sbom/{scan_uuid}/{format}",
            get(sbom_database),
        )
        .route_layer(from_fn_with_state(state, crate::session::ui_guard))
}

type SbomPath = Path<(String, String, String, String, String)>;

async fn sbom_application(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(p): SbomPath,
) -> Response {
    sbom(&state, &ctx, "application", p).await
}

async fn sbom_service(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(p): SbomPath,
) -> Response {
    sbom(&state, &ctx, "service", p).await
}

async fn sbom_database(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(p): SbomPath,
) -> Response {
    sbom(&state, &ctx, "database", p).await
}

/// `abort(404)`.
fn not_found() -> Response {
    ApiError::Abort(StatusCode::NOT_FOUND, "Not Found").into_response()
}

/// `ImageScanSbomController::__invoke()`: the SBOM of a current scan of a resource, made from
/// the stored package list only (spec 9). The resource is looked up in the current team of the
/// session (upstream: every team of the user, then the `view` policy).
async fn sbom(
    state: &AppState,
    ctx: &AuthCtx,
    kind: &str,
    (_project_uuid, environment_uuid, resource_uuid, scan_uuid, format): (
        String,
        String,
        String,
        String,
        String,
    ),
) -> Response {
    match sbom_inner(
        state,
        ctx,
        kind,
        &environment_uuid,
        &resource_uuid,
        &scan_uuid,
        &format,
    )
    .await
    {
        Ok(Some(response)) => response,
        Ok(None) => not_found(),
        Err(e) => e.into_response(),
    }
}

async fn sbom_inner(
    state: &AppState,
    ctx: &AuthCtx,
    kind: &str,
    environment_uuid: &str,
    resource_uuid: &str,
    scan_uuid: &str,
    format: &str,
) -> Result<Option<Response>, ApiError> {
    if !sbom_document::FORMATS.contains(&format) {
        return Ok(None);
    }
    let Some(resource) =
        image_scans::find_resource(&state.db, kind, resource_uuid, &[ctx.team_id]).await?
    else {
        return Ok(None);
    };
    let environment_id = resource
        .record
        .get("environment_id")
        .and_then(Value::as_i64);
    let places = match environment_id {
        Some(id) => image_scans::views::places(&state.db, &[id]).await?,
        None => Vec::new(),
    };
    if places.first().map(|p| p.environment_uuid.as_str()) != Some(environment_uuid) {
        return Ok(None);
    }
    if !can_access(ctx, &resource, AccessLevel::Read) {
        return Err(ApiError::Abort(
            StatusCode::FORBIDDEN,
            "This action is unauthorized.",
        ));
    }

    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let scans = views::current_owned(jobs, &resource).await.map_err(|e| {
        tracing::error!(error = %e, "the scans of an SBOM download could not be read");
        ApiError::Internal
    })?;
    let Some(scan) = scans.into_iter().find(|s| s.scan.uuid == scan_uuid) else {
        return Ok(None);
    };
    let Some(sbom_id) = scan.scan.image_sbom_id else {
        return Ok(None);
    };
    let Some(sbom) = image_scans::find_sbom_by_id(&state.db, sbom_id).await? else {
        return Ok(None);
    };
    if sbom.server_id != scan.scan.server_id {
        return Ok(None);
    }

    let now = state.now();
    let (document, extension) = if format == "cyclonedx" {
        (
            sbom_document::cyclone_dx(&scan.scan.image_ref, &sbom.packages, now),
            "cdx.json",
        )
    } else {
        (
            sbom_document::spdx(&scan.scan.image_ref, &sbom.packages, now),
            "spdx.json",
        )
    };
    let slug = coolify_jobs::compose_parser::php::slug(&format!(
        "{}-{}",
        resource.name(),
        scan.scan.image_ref
    ));
    let name = if slug.is_empty() {
        "image".to_owned()
    } else {
        slug
    };
    Ok(Some(
        (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json".to_owned()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{name}-sbom.{extension}\""),
                ),
            ],
            coolify_compat::php_json::encode_pretty(&document),
        )
            .into_response(),
    ))
}

// ---------------------------------------------------------------------------------------------
// UpdateResourceImage
// ---------------------------------------------------------------------------------------------

/// What an image update ended with, for the page's toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageUpdateOutcome {
    /// `$this->dispatch('success', 'Update started.')`.
    Started,
    /// `ImageUpdateRefused`: the message for the error toast.
    Refused(String),
    /// Any other failure: "The update could not start." (reported).
    Failed,
}

/// The `Auditable` models an image update can change: the page resource (an application, a
/// service, or a standalone database) and, for a service, its environment variables.
struct Snapshot {
    resource: Option<Record>,
    variables: Vec<Record>,
}

async fn snapshot(state: &AppState, resource: &ScanOwner) -> Snapshot {
    let resource_row = image_scans::find_owner(&state.db, resource.morph, resource.id)
        .await
        .ok()
        .flatten()
        .map(|o| o.record);
    let variables = if resource.is_service() {
        coolify_db::environment_variables::rows_for_resource(
            &state.db,
            SERVICE_MORPH,
            resource.id,
            false,
        )
        .await
        .unwrap_or_default()
    } else {
        Vec::new()
    };
    Snapshot {
        resource: resource_row,
        variables,
    }
}

/// `Auditable::AUDIT_IGNORED_FIELDS`.
const AUDIT_IGNORED_FIELDS: [&str; 25] = [
    "updated_at",
    "order",
    "status",
    "last_online_at",
    "restart_count",
    "last_restart_at",
    "last_restart_type",
    "restart_limit_reached",
    "container_present",
    "started_at",
    "config_hash",
    "custom_healthcheck_found",
    "domain_dns_statuses",
    "sentinel_updated_at",
    "sentinel_waiting_since",
    "unreachable_count",
    "high_disk_usage_notification_sent",
    "log_drain_notification_sent",
    "validation_logs",
    "is_validating",
    "detected_traefik_version",
    "traefik_outdated_info",
    "hetzner_server_status",
    "vultr_instance_status",
    "digitalocean_droplet_status",
];

/// `Auditable::isExcludedAuditField()` without the casts of the model (an encrypted `value` of an
/// environment variable is excluded by name).
fn excluded(field: &str, is_variable: bool) -> bool {
    matches!(
        field,
        "id" | "uuid" | "created_at" | "updated_at" | "deleted_at" | "order" | "status"
    ) || field.ends_with("_id")
        || field.ends_with("_at")
        || coolify_db::audit::is_sensitive_field(field)
        || (is_variable && field == "value")
}

/// `str(class_basename($model))->snake()`.
fn snake_type(morph: &str) -> String {
    let base = morph.rsplit('\\').next().unwrap_or(morph);
    let mut out = String::new();
    for (i, c) in base.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// One `Auditable::recordAuditMutation('updated'|'created')` row from the rows before and
/// after.
async fn audit_model(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    resource_type: &str,
    before: Option<&Record>,
    after: &Record,
    is_variable: bool,
) {
    let action = if before.is_some() {
        "updated"
    } else {
        "created"
    };
    let changed: Vec<String> = match before {
        Some(before) => after
            .iter()
            .filter(|(k, v)| before.get(k.as_str()) != Some(*v))
            .map(|(k, _)| k.clone())
            .filter(|k| !AUDIT_IGNORED_FIELDS.contains(&k.as_str()))
            .collect(),
        None => Vec::new(),
    };
    if action == "updated" && changed.is_empty() {
        return;
    }
    let fields: Vec<String> = if action == "updated" {
        changed.clone()
    } else {
        after.keys().cloned().collect()
    };
    let mut changes = Map::new();
    for field in fields {
        if excluded(&field, is_variable) {
            continue;
        }
        let old = match (action, before) {
            ("updated", Some(before)) => before.get(&field).cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        };
        let new = after.get(&field).cloned().unwrap_or(Value::Null);
        changes.insert(
            field.clone(),
            json!({
                "old": coolify_db::audit::redact(&old, Some(&field)),
                "new": coolify_db::audit::redact(&new, Some(&field)),
            }),
        );
    }
    let name = after
        .get("name")
        .filter(|v| !v.is_null())
        .or_else(|| after.get("key"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut context = Map::new();
    context.insert("team_id".into(), json!(ctx.team_id));
    context.insert(
        format!("{resource_type}_uuid"),
        after.get("uuid").cloned().unwrap_or(Value::Null),
    );
    context.insert(format!("{resource_type}_name"), name);
    context.insert("changed_fields".into(), json!(changed));
    audit::mutation(
        state,
        ctx,
        meta,
        resource_type,
        action,
        Value::Object(context),
        Value::Object(changes),
    )
    .await;
}

/// Writes the `Auditable` rows of what an image update changed.
async fn audit_changes(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    resource: &ScanOwner,
    before: &Snapshot,
) {
    let after = snapshot(state, resource).await;
    if let Some(row) = &after.resource {
        audit_model(
            state,
            ctx,
            meta,
            &snake_type(resource.morph),
            before.resource.as_ref(),
            row,
            false,
        )
        .await;
    }
    for variable in &after.variables {
        let id = variable.get("id");
        let earlier = before.variables.iter().find(|v| v.get("id") == id);
        audit_model(
            state,
            ctx,
            meta,
            "environment_variable",
            earlier,
            variable,
            true,
        )
        .await;
    }
}

/// The page's handling of `UpdateResourceImage`: an `AuthorizationException` (the resources of
/// Coolify itself) is passed on as a 403; a refusal and any other failure become a toast.
fn outcome(result: Result<(), UpdateError>) -> Result<ImageUpdateOutcome, ApiError> {
    match result {
        Ok(()) => Ok(ImageUpdateOutcome::Started),
        Err(UpdateError::Refused(refused)) => Ok(ImageUpdateOutcome::Refused(refused.0)),
        Err(UpdateError::Forbidden(message)) => Err(ApiError::Status(
            StatusCode::FORBIDDEN,
            json!({"message": message}),
        )),
        Err(UpdateError::Other(e)) => {
            tracing::error!(error = %e, "an image update could not start");
            Ok(ImageUpdateOutcome::Failed)
        }
    }
}

/// `Vulnerabilities::updateImage()` after the page's own checks: `$this->authorize('update',
/// $resource)`, then `UpdateResourceImage::run($target, $candidateRef, auth()->user())` with the
/// `Auditable` rows of the changed models. `resource` is the page resource; `target` is the
/// resource itself, or the application or database of a service that runs the image.
///
/// `Err(ApiError::Unauthorized)` when the user may not update the resource.
pub async fn update_resource_image(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    resource: &ScanOwner,
    target: &ScanOwner,
    candidate_ref: &str,
) -> Result<ImageUpdateOutcome, ApiError> {
    if !can_access(ctx, resource, AccessLevel::Write) {
        return Err(ApiError::Unauthorized);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let before = snapshot(state, resource).await;
    let result = update::handle(jobs, target, candidate_ref, ctx.user_id).await;
    let outcome = outcome(result)?;
    if outcome == ImageUpdateOutcome::Started {
        audit_changes(state, ctx, meta, resource, &before).await;
    }
    Ok(outcome)
}

/// `Vulnerabilities::updateImages()` after the page found the merged action:
/// `$this->authorize('update', $service)`, then `UpdateResourceImage::handleParts()` with the
/// `Auditable` rows of the changed models.
pub async fn update_resource_images(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    service: &ScanOwner,
    parts: &[UpdatePart],
) -> Result<ImageUpdateOutcome, ApiError> {
    if !can_access(ctx, service, AccessLevel::Write) {
        return Err(ApiError::Unauthorized);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let before = snapshot(state, service).await;
    let result = update::handle_parts(jobs, service, parts, ctx.user_id).await;
    let outcome = outcome(result)?;
    if outcome == ImageUpdateOutcome::Started {
        audit_changes(state, ctx, meta, service, &before).await;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_fields_and_names() {
        assert_eq!(
            snake_type(r"App\Models\StandaloneMariadb"),
            "standalone_mariadb"
        );
        assert_eq!(snake_type(r"App\Models\Application"), "application");
        assert!(excluded("environment_id", false));
        assert!(excluded("value", true));
        assert!(!excluded("value", false));
        assert!(!excluded("docker_registry_image_tag", false));
        assert!(excluded("uuid", false));
    }

    #[test]
    fn outcomes() {
        assert_eq!(outcome(Ok(())).unwrap(), ImageUpdateOutcome::Started);
        assert!(outcome(Err(UpdateError::Forbidden("no".into()))).is_err());
        assert_eq!(
            outcome(Err(UpdateError::Other("db".into()))).unwrap(),
            ImageUpdateOutcome::Failed
        );
    }
}
