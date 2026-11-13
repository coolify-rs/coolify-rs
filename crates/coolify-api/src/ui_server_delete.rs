//! What `Livewire\Server\Delete` adds at v4.7.5: the resources a delete removes, the applications
//! it only detaches, and the option to remove the SSH key from the server.

use axum::Extension;
use axum::Router;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::get;
use coolify_db::servers::{self, AdditionalApplication, ServerRow};
use coolify_jobs::server_ops::{Srv, authorized_keys};
use serde_json::{Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::server_settings::find_server;
use crate::state::AppState;
use crate::ui_server_general::SERVER_PAGE;

pub fn routes() -> Router<AppState> {
    Router::new().route(&format!("{SERVER_PAGE}/delete"), get(show))
}

/// The action line of the confirmation dialog for one detached application.
pub(crate) fn detach_action(application: &AdditionalApplication) -> String {
    format!(
        "Application {} will be removed from this server and will keep running on {}.",
        application.name,
        application
            .primary_server_name
            .as_deref()
            .unwrap_or_default()
    )
}

/// `Delete::render()`: the lists and checkboxes that depend on the resources of the server.
async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let transferred = crate::routes::servers::is_transferred(&s);
    let resources_to_delete = servers::primary_resource_names(&state.db, s.id).await?;
    let detached = servers::additional_applications(&state.db, s.id).await?;
    // `definedResources()`: also the applications that use the server as an additional destination.
    let defined = servers::resource_count(&state.db, s.id).await?;
    let can_run = state
        .jobs
        .as_ref()
        .is_some_and(|jobs| authorized_keys::can_run_on(&Srv::from_row(jobs, s.clone())));
    let mut checkboxes: Vec<Value> = Vec::new();
    if !resources_to_delete.is_empty() && !transferred {
        checkboxes.push(json!({
            "id": "force_delete_resources",
            "label": format!(
                "Delete all resources ({} total): {}",
                resources_to_delete.len(),
                resources_to_delete.join(", ")
            ),
            "default_warning": "Server cannot be deleted while it has resources.",
        }));
    }
    if !transferred && defined == 0 && can_run {
        checkboxes.push(json!({
            "id": "remove_ssh_key",
            "label": "Remove the public key of this server's SSH key from ~/.ssh/authorized_keys on the server",
            "default_warning": "The public key stays on the server, so this SSH key can still log in.",
        }));
    }
    Ok(ok(json!({
        "transferred": transferred,
        "resources_to_delete": resources_to_delete,
        "detached_applications": detached
            .iter()
            .map(|a| json!({
                "name": a.name,
                "server_name": a.primary_server_name,
                "action": detach_action(a),
            }))
            .collect::<Vec<_>>(),
        "checkboxes": checkboxes,
    }))
    .into_response())
}

/// `Delete::removeSshKey()`: the key is removed before the server record, while the connection
/// still works. Resources must be deleted first, because their delete jobs need the connection.
/// `Err` is the toast of the page; the server is then not deleted.
pub(crate) async fn remove_ssh_key(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    server: &ServerRow,
) -> Result<(), ApiError> {
    let toast = |message: String| {
        ApiError::Status(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            json!({"message": message}),
        )
    };
    if servers::resource_count(&state.db, server.id).await? > 0 {
        return Err(toast(
            "Delete the resources of the server first, then delete the server and remove the SSH key."
                .to_owned(),
        ));
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let key_uuid = match server.record.get("private_key_id").and_then(Value::as_i64) {
        Some(id) => coolify_db::private_keys::uuid_by_id(&state.db, id).await?,
        None => None,
    };
    let context = |result: &str| {
        json!({
            "team_id": server.team_id,
            "server_uuid": server.uuid,
            "server_name": server.record.get("name"),
            "private_key_uuid": key_uuid,
            "result": result,
        })
    };
    let srv = Srv::from_row(jobs, server.clone());
    match authorized_keys::remove_server_key(&srv).await {
        Ok(removed) => {
            let result = if removed {
                "removed"
            } else {
                "kept_for_other_server"
            };
            crate::audit::log(
                state,
                ctx,
                meta,
                "ui.server.ssh_key_removed",
                context(result),
                "info",
            )
            .await;
            Ok(())
        }
        Err(e) => {
            crate::audit::log(
                state,
                ctx,
                meta,
                "ui.server.ssh_key_removed",
                context("failed"),
                "warning",
            )
            .await;
            // `htmlspecialchars(remove_iip($e->getMessage()), ENT_QUOTES, 'UTF-8')`.
            let error = coolify_compat::redact::redact_text(&e.message, &[], &[])
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace('\'', "&#039;");
            Err(toast(format!(
                "The SSH key was not removed from the server, so the server was not deleted. Clear the option to delete the server without removing the key.<br><br>Error: {error}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_detached_application_names_the_server_it_stays_on() {
        let application = AdditionalApplication {
            id: 1,
            name: "shop".into(),
            primary_server_name: Some("web-1".into()),
        };
        assert_eq!(
            detach_action(&application),
            "Application shop will be removed from this server and will keep running on web-1."
        );
    }
}
