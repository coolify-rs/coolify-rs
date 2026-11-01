//! `App\Livewire\Server\Transfer` and `App\Livewire\Server\TransferFinish`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use coolify_db::servers;
use serde_json::{Value, json};

use crate::audit::{self, Meta};
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson};
use crate::routes::server_settings::find_server;
use crate::server_transfer::{checklist, dg, instance_url, migrator, text};
use crate::state::AppState;
use crate::ui_server_general::SERVER_PAGE;

pub fn routes() -> Router<AppState> {
    let p = |s: &str| format!("{SERVER_PAGE}/{s}");
    Router::new()
        .route(&p("transfer"), get(show))
        .route(&p("transfer/migrate"), post(migrate))
        .route(&p("transfer/back"), post(transfer_back))
        .route(&p("transfer/enable-management"), post(enable_management))
        .route(&p("transfer-finish"), get(finish_view))
        .route(&p("transfer-finish/check-all"), post(check_all))
        .route(&p("transfer-finish/check"), post(check_step))
        .route(&p("transfer-finish/fix"), post(fix_step))
        .route(&p("transfer-finish/done"), post(mark_done))
        .route(&p("transfer-finish/finish"), post(finish))
}

fn failed(message: String) -> axum::response::Response {
    use axum::response::IntoResponse;
    PhpJson(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": message}),
    )
    .into_response()
}

fn ok(value: Value) -> ApiResult {
    use axum::response::IntoResponse;
    Ok(PhpJson(StatusCode::OK, value).into_response())
}

/// `Transfer::mount`, and the properties the view reads.
async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    // The transfer exports every credential of the server, so only admins and owners may open it.
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let metadata = s
        .record
        .get("server_metadata")
        .cloned()
        .unwrap_or(Value::Null);
    let mut manager_url = "";
    // Transfer back sends a new team token to this URL, so it is prefilled only when this instance chose it.
    if let Some(url) = dg(&metadata, "transfer.target_instance_url").and_then(Value::as_str)
        && dg(&metadata, "transfer.claimed_by_marker") != Some(&json!(true))
        && migrator::validate_instance_url(&state, url).await.is_ok()
    {
        manager_url = url;
    }
    let migration = dg(&metadata, "migration")
        .filter(|m| m.is_object())
        .cloned();
    let target_server_url = migration
        .as_ref()
        .and_then(|m| m.get("target_server_url"))
        .filter(|u| migrator::is_linkable_url(Some(u)))
        .cloned();
    ok(json!({
        "is_localhost": s.id == 0,
        "is_owner": ctx.role == coolify_domain::Role::Owner,
        "is_transferred": dg(&metadata, "transfer.status").and_then(Value::as_str) == Some("transferred"),
        "export_id": dg(&metadata, "transfer.export_id"),
        "owner_marker_conflict": dg(&metadata, "transfer.owner_marker_conflict"),
        "transfer_back": dg(&metadata, "transfer.transfer_back"),
        "manager_url": manager_url,
        "this_instance_url": instance_url(&state.db).await,
        "migration": migration,
        "target_server_url": target_server_url,
    }))
}

/// `Transfer::migrateServer`.
async fn migrate(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    Json(input): Json<Value>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if ctx.role != coolify_domain::Role::Owner {
        return Err(ApiError::Unauthorized);
    }
    if s.id == 0 {
        return Ok(failed(
            "The Coolify host (localhost) cannot be transferred.".into(),
        ));
    }
    let target = input.get("target_url").map(text).unwrap_or_default();
    let token = input.get("target_token").map(text).unwrap_or_default();
    let ip = s.record.get("ip").map(text).unwrap_or_default();
    let port = s.record.get("port").and_then(Value::as_i64).unwrap_or(22);
    if let Err(e) = migrator::start(&state, s.id, &s.uuid, &ip, port, &target, &token).await {
        return Ok(failed(e));
    }
    audit::log(
        &state,
        &ctx,
        &meta,
        "ui.server.migrate",
        json!({"team_id": ctx.team_id, "server_uuid": s.uuid, "target_url": target}),
        "info",
    )
    .await;
    ok(json!({"message": "All checks passed. The transfer started."}))
}

/// `User::createToken`: the plain text token, valid for `TRANSFER_BACK_TOKEN_SECONDS`.
async fn create_transfer_back_token(
    state: &AppState,
    user_id: i64,
    team_id: i64,
    server_uuid: &str,
) -> Result<(i64, String), ApiError> {
    let plain = coolify_compat::sanctum::generate_plaintext("");
    let now = state.now();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO personal_access_tokens (tokenable_type, tokenable_id, name, token, abilities, team_id, expires_at, created_at, updated_at) \
         VALUES ('App\\Models\\User', $1, $2, $3, $4, $5, $6, $7, $7) RETURNING id",
    )
    .bind(user_id)
    .bind(format!("transfer back {server_uuid}"))
    .bind(coolify_compat::sanctum::hash_token(&plain))
    .bind(json!(["write", "deploy"]).to_string())
    .bind(team_id.to_string())
    .bind(now + chrono::Duration::seconds(3600))
    .bind(now)
    .fetch_one(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    Ok((id, format!("{id}|{plain}")))
}

/// `Transfer::transferBack` and `ServerTransferMigrator::requestTransferBack`.
async fn transfer_back(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    Json(input): Json<Value>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if !crate::routes::servers::is_transferred(&s) {
        return Ok(failed(
            "Only a server that was transferred to another instance can be transferred back."
                .into(),
        ));
    }
    let manager = input.get("manager_url").map(text).unwrap_or_default();
    let (id, token) = create_transfer_back_token(&state, ctx.user_id, ctx.team_id, &s.uuid).await?;
    let result = migrator::request_transfer_back(
        &state,
        s.id,
        &s.uuid,
        &manager,
        &input.get("manager_token").map(text).unwrap_or_default(),
        &input.get("this_instance_url").map(text).unwrap_or_default(),
        &token,
    )
    .await;
    if let Err(e) = result {
        let _ = sqlx::query("DELETE FROM personal_access_tokens WHERE id = $1")
            .bind(id)
            .execute(state.db.pool())
            .await;
        return Ok(failed(e));
    }
    audit::log(
        &state,
        &ctx,
        &meta,
        "ui.server.transfer_back",
        json!({"team_id": ctx.team_id, "server_uuid": s.uuid, "from": manager}),
        "info",
    )
    .await;
    ok(json!({"message": "The other instance started the transfer back."}))
}

/// `Show::enableManagement` with `ServerTransferClaimer::restoreAfterTakeBack`.
async fn enable_management(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    if s.id == 0 || !crate::routes::servers::is_transferred(&s) {
        return ok(json!({}));
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    let url = instance_url(&state.db).await;
    if let Err(e) = coolify_jobs::server_transfer::claim(jobs, s.id, &url).await {
        return Ok(failed(e));
    }
    // Provider calls run here, after the claim committed.
    let parked: Vec<i64> = sqlx::query_scalar::<_, Option<String>>(
        "SELECT server_metadata::jsonb #>> '{transfer,parked_dns_record_ids}' FROM servers WHERE id = $1",
    )
    .bind(s.id)
    .fetch_one(state.db.pool())
    .await
    .ok()
    .flatten()
    .and_then(|t| serde_json::from_str(&t).ok())
    .unwrap_or_default();
    if let Some(enc) = state.encrypter.as_ref() {
        let dns = coolify_jobs::integrations::cloudflare::Dns {
            db: &state.db,
            enc,
            now: state.now(),
            source: "system",
        };
        for id in parked {
            if let Ok(Some(record)) = coolify_db::managed_dns::record_by_id(&state.db, id).await
                && dns.reclaim_record(&record).await
            {
                let _ = sqlx::query(
                    "UPDATE managed_dns_records SET owned = true, updated_at = $2 WHERE id = $1",
                )
                .bind(id)
                .bind(state.now())
                .execute(state.db.pool())
                .await;
            }
        }
    }
    let mut tx = state
        .db
        .pool()
        .begin()
        .await
        .map_err(|_| ApiError::Internal)?;
    let mut metadata = coolify_jobs::server_transfer::locked_metadata(&mut tx, s.id)
        .await
        .map_err(|_| ApiError::Internal)?;
    if let Some(t) = metadata.get_mut("transfer").and_then(Value::as_object_mut) {
        t.remove("parked_dns_record_ids");
        t.insert(
            "taken_back_at".into(),
            json!(crate::server_transfer::now_iso()),
        );
    }
    coolify_jobs::server_transfer::write_metadata(&mut tx, s.id, &metadata)
        .await
        .map_err(|_| ApiError::Internal)?;
    tx.commit().await.map_err(|_| ApiError::Internal)?;
    let _ = checklist::start(&state, s.id, &s.uuid).await;
    // Sentinel must report here, so the other instance reads the owner marker and stops.
    let (bg, id) = (jobs.clone(), s.id);
    tokio::spawn(async move {
        if let Err(e) = coolify_jobs::server_ops::sentinel::check_and_start_sentinel(&bg, id).await
        {
            tracing::warn!(error = %e, "Restart Sentinel failed");
        }
    });
    audit::log(
        &state,
        &ctx,
        &meta,
        "ui.server.management_enabled",
        json!({"team_id": ctx.team_id, "server_uuid": s.uuid, "server_name": s.record.get("name")}),
        "info",
    )
    .await;
    ok(json!({"message": "This Coolify instance now manages the server."}))
}

/// `TransferFinish::refreshSteps` and `render`.
async fn finish_view(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    let view = checklist::view(&state, s.id)
        .await
        .map_err(|_| ApiError::Internal)?;
    if !view.is_null() && ctx.is_team_admin() {
        let _ = checklist::check_when_stale(&state, s.id).await;
    }
    ok(json!({"finish": view, "can_update": ctx.is_team_admin()}))
}

/// `TransferFinish::checkAllNow`.
async fn check_all(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let bg = state.clone();
    tokio::spawn(async move {
        if let Err(e) = checklist::check_all(&bg, s.id).await {
            tracing::warn!(error = %e, "CheckTransferFinishStepsJob failed");
        }
    });
    ok(json!({}))
}

/// `TransferFinish::checkStep`.
async fn check_step(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(input): Json<Value>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    match checklist::check(
        &state,
        s.id,
        &input.get("key").map(text).unwrap_or_default(),
    )
    .await
    {
        Ok(v) => ok(v),
        Err(e) => Ok(failed(e)),
    }
}

/// `TransferFinish::fixStep`.
async fn fix_step(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    Json(input): Json<Value>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    match checklist::fix(
        &state,
        s.id,
        &input.get("key").map(text).unwrap_or_default(),
    )
    .await
    {
        Ok(v) => ok(v),
        Err(e) => Ok(failed(e)),
    }
}

/// `TransferFinish::markStepDone`.
async fn mark_done(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
    Json(input): Json<Value>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let name = audit::actor(&state, &ctx, &meta)
        .await
        .name
        .unwrap_or_else(|| "a user".to_owned());
    match checklist::mark_done(
        &state,
        s.id,
        &input.get("key").map(text).unwrap_or_default(),
        &name,
    )
    .await
    {
        Ok(v) => ok(v),
        Err(e) => Ok(failed(e)),
    }
}

/// `TransferFinish::finishTransfer`.
async fn finish(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(uuid): Path<String>,
) -> ApiResult {
    let s = find_server(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let _ = servers::find_by_id(&state.db, s.id).await?;
    match checklist::finish(&state, s.id, ctx.user_id).await {
        Ok(skipped) => {
            audit::log(
                &state,
                &ctx,
                &meta,
                "ui.server.transfer_finished",
                json!({"team_id": ctx.team_id, "server_uuid": s.uuid, "skipped_steps": skipped}),
                "info",
            )
            .await;
            ok(json!({"message": "The transfer is finished."}))
        }
        Err(e) => Ok(failed(e)),
    }
}
