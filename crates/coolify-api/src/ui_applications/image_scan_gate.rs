//! The image scan gate of a deployment (`Deployment\Show::getImageScanGateProperty()` and
//! `deployAnyway()`): the "Blocked" status label and the popover of
//! `components/image-scan/gate-status.blade.php`.

use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use coolify_db::applications::{self as apps, Deployment};
use coolify_db::image_scans::{self, APPLICATION_MORPH};
use coolify_domain::grants::AccessLevel;
use coolify_jobs::applications::queue::{QueueOptions, Queued};
use coolify_jobs::image_scan::views::card;
use serde_json::{Value, json};

use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::ui_server_destinations::toast_error;

/// `scan_gate_result` with the `array` cast.
pub(crate) fn gate_result(d: &Deployment) -> Value {
    match d.raw.get("scan_gate_result") {
        Some(Value::String(text)) => serde_json::from_str(text).unwrap_or(Value::Null),
        Some(other) => other.clone(),
        None => Value::Null,
    }
}

/// `ApplicationDeploymentQueue::isImageScanBlocked()`.
pub(crate) fn is_blocked(result: &Value) -> bool {
    result
        .get("blocked")
        .is_some_and(crate::serialize::php_truthy)
}

/// `isImageScanSkipped()`: the scan could not run and the deployment went on without it.
pub(crate) fn is_skipped(result: &Value) -> bool {
    !is_blocked(result)
        && result.get("error").is_some_and(|e| match e {
            Value::Null => false,
            Value::String(s) => !s.trim().is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
            _ => true,
        })
}

/// `getImageScanGateProperty()`: the gate result of the shown deployment, `None` when the gate
/// passed or did not run. `action` is the sentence of the action that fixes the blocked image.
pub(crate) async fn gate(
    state: &AppState,
    ctx: &AuthCtx,
    a: &apps::ApplicationRow,
    d: &Deployment,
) -> Value {
    let result = gate_result(d);
    let blocked = is_blocked(&result);
    if !blocked && !is_skipped(&result) {
        return Value::Null;
    }
    let mut action = None;
    if blocked
        && let Some(jobs) = state.jobs.as_ref()
        && let Ok(Some(owner)) = image_scans::find_owner(&state.db, APPLICATION_MORPH, a.id).await
    {
        match card::gate_action(jobs, &owner, d.id, Some(&result)).await {
            Ok(found) => action = found.map(|g| g.sentence),
            Err(e) => {
                tracing::warn!(error = %e, "the action of a blocked deployment could not be read")
            }
        }
    }
    json!({
        "result": result,
        "blocked": blocked,
        "action": action,
        "can_deploy_anyway": ctx.can_app(AccessLevel::Write, a),
    })
}

/// `Heading::imageScanGate()`: the gate of the latest deployment of the app (not a preview), for
/// the badge in the heading. `None` when the app does not scan its images or the gate passed.
pub(crate) async fn heading_gate(state: &AppState, a: &apps::ApplicationRow) -> Value {
    let Ok(Some(owner)) = image_scans::find_owner(&state.db, APPLICATION_MORPH, a.id).await else {
        return Value::Null;
    };
    // The gate never runs without both. Scanning off on the server still shows: the gate
    // reports it.
    if !coolify_jobs::image_scan::policy::is_allowed_for(&owner) {
        return Value::Null;
    }
    let latest: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM application_deployment_queues WHERE application_id = $1 AND pull_request_id = 0 ORDER BY id DESC LIMIT 1",
    )
    .bind(a.id)
    .fetch_optional(state.db.pool())
    .await
    .ok()
    .flatten();
    let Some(id) = latest else {
        return Value::Null;
    };
    let Ok(Some(d)) = apps::deployment_by_id(&state.db, id).await else {
        return Value::Null;
    };
    let result = gate_result(&d);
    let blocked = is_blocked(&result);
    if !blocked && !is_skipped(&result) {
        return Value::Null;
    }
    let action = match (blocked, state.jobs.as_ref()) {
        (true, Some(jobs)) => card::gate_action(jobs, &owner, d.id, Some(&result))
            .await
            .ok()
            .flatten()
            .map(|g| g.sentence),
        _ => None,
    };
    json!({
        "result": result,
        "action": action,
        "deployment_uuid": d.uuid,
    })
}

/// `Deployment\Show::deployAnyway()`: queue the blocked deployment again for the same commit, on
/// the same server and destination, with the image scan gate bypassed.
pub async fn deploy_anyway(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, dep)): Path<(String, String)>,
) -> ApiResult {
    let a = apps::find_in_team(&state.db, ctx.team_id, &uuid)
        .await?
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Application not found.",
        ))?;
    if !ctx.can_app(AccessLevel::Write, &a) {
        return Err(ApiError::Unauthorized);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let d = apps::deployment_by_uuid(&state.db, &dep)
        .await?
        .filter(|d| d.application_id == a.id)
        .ok_or(ApiError::Message(
            StatusCode::NOT_FOUND,
            "Deployment not found.",
        ))?;
    let result = gate_result(&d);
    if !is_blocked(&result) {
        return toast_error("This deployment was not blocked.");
    }
    // The newest deployment of the same pull request.
    let pull_request_id = d.int("pull_request_id").unwrap_or(0);
    let latest: Option<i64> = sqlx::query_scalar(
        "SELECT MAX(id) FROM application_deployment_queues WHERE application_id = $1 AND pull_request_id = $2",
    )
    .bind(a.id)
    .bind(pull_request_id)
    .fetch_one(state.db.pool())
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "the latest deployment could not be read");
        ApiError::Internal
    })?;
    if latest != Some(d.id) {
        return toast_error("A newer deployment exists.");
    }

    // `$this->application->deploymentServer($deployment)`.
    let server_id = match d.int("server_id") {
        Some(id) if a.server_id() == Some(id) => Some(id),
        Some(id) => apps::additional_pivots(&state.db, a.id)
            .await?
            .iter()
            .any(|(_, s, _)| *s == id)
            .then_some(id),
        None => None,
    };
    let destination_id = match server_id {
        Some(server_id) => {
            crate::ui_image_scan::vulnerabilities::standalone_destination(
                &state,
                d.int("destination_id"),
                server_id,
            )
            .await?
        }
        None => None,
    };
    let pinned_image = result
        .get("repo_digest")
        .and_then(Value::as_str)
        .filter(|_| a.str("build_pack") == Some("dockerimage"))
        .map(str::to_owned);
    let deployment_uuid = coolify_domain::new_public_id();
    let queued = jobs
        .queue_application_deployment(
            &a,
            &deployment_uuid,
            QueueOptions {
                pull_request_id,
                commit: d.str("commit").map(str::to_owned),
                force_rebuild: false,
                git_type: d.str("git_type").map(str::to_owned),
                server_id,
                destination_id,
                only_this_server: d.bool("only_this_server"),
                docker_registry_image_tag: d.str("docker_registry_image_tag").map(str::to_owned),
                scan_gate_bypassed_by: Some(ctx.user_id),
                scan_gate_pinned_image: pinned_image,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "deploy anyway could not be queued");
            ApiError::Internal
        })?;
    match queued {
        Queued::Queued { deployment_uuid } => Ok(ok(json!({
            "message": "Deploy started.",
            "deployment_uuid": deployment_uuid,
        }))
        .into_response()),
        other => Ok(crate::ui_server_destinations::toast_error(format!(
            "Deploy did not start. {}",
            other.message()
        ))?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_and_skipped_gates() {
        assert!(is_blocked(&json!({"blocked": true})));
        assert!(!is_blocked(&json!({"blocked": false, "error": "x"})));
        assert!(is_skipped(
            &json!({"blocked": false, "error": "scan_failed"})
        ));
        assert!(!is_skipped(
            &json!({"blocked": true, "error": "scan_failed"})
        ));
        assert!(!is_skipped(&json!({"error": null})));
        assert!(!is_skipped(&Value::Null));
    }
}
