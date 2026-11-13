//! Session endpoints behind `Livewire\Server\Security\HostKey` (upstream v4.7.5): the pinned SSH
//! host key of a server on its Security page.

use axum::Extension;
use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use coolify_db::servers::ServerRow;
use coolify_db::ssh_host_keys::{self, HostKeySettings};
use coolify_remote::host_key::{sha256_hex, ssh_host_key_fingerprints};
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::server_settings::find_server;
use crate::state::AppState;
use crate::ui_server_destinations::toast_error;
use crate::ui_server_general::SERVER_PAGE;

pub fn routes() -> Router<AppState> {
    let p = |s: &str| format!("{SERVER_PAGE}/security/host-key{s}");
    Router::new()
        .route(&p(""), get(show))
        .route(&p("/trust"), post(trust_pending_key))
        .route(&p("/reset"), post(reset_host_key))
        .route(&p("/check-disabled"), post(set_check_disabled))
        .route(&p("/check-mode"), post(set_check_mode))
}

fn filled(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.trim().is_empty())
}

/// The match of the view on `ssh_host_key_source`.
fn source_label(source: Option<&str>) -> &'static str {
    match source {
        Some("tofu") => "Saved on the first connection",
        Some("upgrade") => "Saved on the first connection after the Coolify upgrade",
        Some("transfer") => "Moved with the server transfer",
        Some("manual") => "Saved by a user",
        _ => "Saved",
    }
}

/// `HostKey::render()`: what `livewire/server/security/host-key.blade.php` shows.
fn page(state: &AppState, s: &ServerRow, settings: &HostKeySettings, can_update: bool) -> Value {
    let instance_mode = state
        .jobs
        .as_ref()
        .map(|j| j.remote().host_key_checking())
        .unwrap_or(coolify_remote::HostKeyMode::Warn);
    let mode = instance_mode.for_server(
        settings.is_ssh_host_key_check_disabled,
        settings.is_ssh_host_key_check_enforced,
    );
    let pending = settings
        .ssh_host_key_pending
        .as_deref()
        .filter(|p| filled(Some(p)));
    json!({
        "server_name": s.record.get("name").cloned().unwrap_or(Value::Null),
        "mode": mode.as_str(),
        "instance_mode": instance_mode.as_str(),
        "fingerprints": ssh_host_key_fingerprints(settings.ssh_host_key.as_deref()),
        "pending_fingerprints": ssh_host_key_fingerprints(pending),
        // `hash('sha256', $server->settings->ssh_host_key_pending)`: what "Trust new key" sends
        // back, so that a key that changed later is not trusted.
        "pending_key_hash": pending.map(sha256_hex),
        "source_label": source_label(settings.ssh_host_key_source.as_deref()),
        // `->toDateTimeString()`.
        "trusted_at": settings
            .ssh_host_key_trusted_at
            .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
        "is_check_disabled": settings.is_ssh_host_key_check_disabled,
        "check_mode": if settings.is_ssh_host_key_check_enforced { "enforce" } else { "warn" },
        "can_update": can_update,
    })
}

async fn load(state: &AppState, s: &ServerRow) -> Result<HostKeySettings, ApiError> {
    ssh_host_keys::load(&state.db, s.id)
        .await?
        .ok_or(ApiError::Internal)
}

async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    // `mount()`: `authorize('update', $server)`, or back to the server list.
    crate::routes::server_settings::authorize_update(&ctx)?;
    let settings = load(&state, &s).await?;
    let mut v = page(&state, &s, &settings, ctx.is_team_admin());
    // The modals pass `temporaryDisableTwoStepConfirmation`: the name is always typed.
    let (_, password) = crate::ui::confirmation_steps(&state, ctx.user_id).await?;
    v["confirm"] = json!({"text": true, "password": password});
    Ok(ok(v).into_response())
}

/// What a change does to the settings (`$change($settings)`).
enum Change {
    TrustPending { pending_key_hash: String },
    Reset,
    CheckDisabled(bool),
    CheckMode { enforced: bool },
}

impl Change {
    /// The last part of `ui.server.ssh_host_key.{action}`.
    fn action(&self) -> &'static str {
        match self {
            Change::TrustPending { .. } => "trusted",
            Change::Reset => "reset",
            Change::CheckDisabled(true) => "check_disabled",
            Change::CheckDisabled(false) => "check_enabled",
            Change::CheckMode { enforced: true } => "check_enforced",
            Change::CheckMode { enforced: false } => "check_not_enforced",
        }
    }

    fn confirms_password(&self) -> bool {
        !matches!(self, Change::CheckMode { .. })
    }
}

/// `HostKey::changeHostKey()`.
async fn change_host_key(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    uuid: &str,
    password: Option<&str>,
    change: Change,
) -> ApiResult {
    let s = find_server(state, ctx, uuid).await?;
    if !ctx.is_team_admin() {
        return toast_error("This action is unauthorized.");
    }
    // `canBeValidated()`.
    if crate::routes::servers::is_transferred(&s) {
        return toast_error("This server was transferred to another Coolify instance.");
    }
    if change.confirms_password()
        && !crate::ui::password_confirmed(state, ctx.user_id, password).await?
    {
        return Err(ApiError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The provided password is incorrect.",
        ));
    }
    let settings = load(state, &s).await?;
    let previous_fingerprints = ssh_host_key_fingerprints(settings.ssh_host_key.as_deref());
    let now = state.now();
    match &change {
        Change::TrustPending { pending_key_hash } => {
            let Some(pending) = settings
                .ssh_host_key_pending
                .as_deref()
                .filter(|p| filled(Some(p)))
            else {
                return toast_error("There is no new host key to trust.");
            };
            let another =
                "The server sent another new host key. Reload the page and compare the key again.";
            if sha256_hex(pending) != *pending_key_hash {
                return toast_error(another);
            }
            if !ssh_host_keys::trust_pending(&state.db, settings.settings_id, pending, now).await? {
                return toast_error(another);
            }
        }
        Change::Reset => ssh_host_keys::forget(&state.db, settings.settings_id, now).await?,
        Change::CheckDisabled(disabled) => {
            let enforced = !*disabled && settings.is_ssh_host_key_check_enforced;
            ssh_host_keys::set_check(&state.db, settings.settings_id, *disabled, enforced, now)
                .await?;
        }
        Change::CheckMode { enforced } => {
            if settings.is_ssh_host_key_check_disabled {
                return toast_error(
                    "The host key check is disabled for this server. Reload the page and enable the check first.",
                );
            }
            ssh_host_keys::set_check(&state.db, settings.settings_id, false, *enforced, now)
                .await?;
        }
    }
    let saved = load(state, &s).await?;
    crate::audit::log(
        state,
        ctx,
        meta,
        &format!("ui.server.ssh_host_key.{}", change.action()),
        json!({
            "team_id": s.team_id,
            "server_uuid": s.uuid,
            "server_name": s.record.get("name").cloned().unwrap_or(Value::Null),
            "previous_fingerprints": previous_fingerprints,
            "fingerprints": ssh_host_key_fingerprints(saved.ssh_host_key.as_deref()),
        }),
        "info",
    )
    .await;
    if let Some(jobs) = &state.jobs {
        // `ServerSetting::updated`: the known_hosts files of older keys go.
        if saved.ssh_host_key != settings.ssh_host_key {
            jobs.remote()
                .delete_stale_known_hosts_files(&s.uuid, saved.ssh_host_key.as_deref());
        }
        // The check can take up to a minute on a slow server, so it runs in the background and
        // the page polls for the result.
        jobs.spawn_connection_check(s.id);
    }
    let mut v = page(state, &s, &saved, true);
    v["message"] =
        json!("The host key settings were saved. Coolify now checks the connection again.");
    Ok(ok(v).into_response())
}

fn password_of(body: &Map<String, Value>) -> Option<&str> {
    body.get("password").and_then(Value::as_str)
}

/// `HostKey::trustPendingKey($pendingKeyHash, $password)`.
async fn trust_pending_key(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    meta: Meta,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let pending_key_hash = body
        .get("pending_key_hash")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    change_host_key(
        &state,
        &ctx,
        &meta,
        &uuid,
        password_of(&body),
        Change::TrustPending { pending_key_hash },
    )
    .await
}

/// `HostKey::resetHostKey($password)`.
async fn reset_host_key(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    meta: Meta,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    change_host_key(
        &state,
        &ctx,
        &meta,
        &uuid,
        password_of(&body),
        Change::Reset,
    )
    .await
}

/// `HostKey::setHostKeyCheckDisabled($disabled, $password)`: `disabled` is the state that the
/// button showed, so that a page open in another tab cannot turn the check back on.
async fn set_check_disabled(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    meta: Meta,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let disabled = match body.get("disabled") {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        _ => false,
    };
    change_host_key(
        &state,
        &ctx,
        &meta,
        &uuid,
        password_of(&body),
        Change::CheckDisabled(disabled),
    )
    .await
}

/// `HostKey::setHostKeyCheckMode($mode)`: saved at once from the dropdown, without a password.
async fn set_check_mode(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    meta: Meta,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let enforced = match body.get("mode").and_then(Value::as_str) {
        Some("enforce") => true,
        Some("warn") => false,
        // `return false`.
        _ => return Ok(ok(json!({"saved": false})).into_response()),
    };
    change_host_key(
        &state,
        &ctx,
        &meta,
        &uuid,
        None,
        Change::CheckMode { enforced },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_actions_follow_upstream() {
        assert_eq!(
            Change::TrustPending {
                pending_key_hash: String::new()
            }
            .action(),
            "trusted"
        );
        assert_eq!(Change::Reset.action(), "reset");
        assert_eq!(Change::CheckDisabled(true).action(), "check_disabled");
        assert_eq!(Change::CheckDisabled(false).action(), "check_enabled");
        assert_eq!(
            Change::CheckMode { enforced: true }.action(),
            "check_enforced"
        );
        assert_eq!(
            Change::CheckMode { enforced: false }.action(),
            "check_not_enforced"
        );
        // Only the dropdown saves without a password.
        assert!(Change::Reset.confirms_password());
        assert!(!Change::CheckMode { enforced: true }.confirms_password());
    }

    #[test]
    fn source_labels_follow_the_view() {
        assert_eq!(source_label(Some("tofu")), "Saved on the first connection");
        assert_eq!(
            source_label(Some("upgrade")),
            "Saved on the first connection after the Coolify upgrade"
        );
        assert_eq!(
            source_label(Some("transfer")),
            "Moved with the server transfer"
        );
        assert_eq!(source_label(Some("manual")), "Saved by a user");
        assert_eq!(source_label(None), "Saved");
    }
}
