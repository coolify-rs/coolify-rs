//! Page endpoints of `/profile`: `App\Livewire\Profile\{Index, Passkeys}`, Fortify's two-factor
//! and password confirmation routes, `laravel/passkeys` registration and `ProfileAvatarController`.
//! Upstream's flash values (`session('status')`, `session('errors')`) are kept per session and
//! read once by `GET /ui/profile`, so a reload shows what a reload shows upstream.

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::TimeDelta;
use coolify_compat::{totp, webauthn};
use coolify_db::{profile as db, team_pages as tp};
use coolify_jobs::notifications::{self, Mail};
use rand::Rng;
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::session::{Flash, NO_TEAM, SessionUser};
use crate::state::AppState;
use crate::ui_notifications::{summarize, toast_error};
use crate::ui_teams::{
    confirm_steps, image_response, password_incorrect, prepare_avatar, remove_image,
    session_password_ok, toast_with_errors, valid_email, write_image,
};

/// `constants.email_change.verification_code_expiry_minutes`.
const EMAIL_CODE_MINUTES: i64 = 10;

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ui/profile", get(show).post(submit))
        .route(
            "/ui/profile/avatar",
            post(upload_avatar)
                .delete(remove_avatar)
                .layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route("/profile/avatar", get(own_avatar))
        .route("/ui/profile/email/request", post(request_email_change))
        .route("/ui/profile/email/verify", post(verify_email_change))
        .route("/ui/profile/email/resend", post(resend_code))
        .route("/ui/profile/email/cancel", post(cancel_email_change))
        .route("/ui/profile/password", post(reset_password))
        .route("/ui/profile/delete", post(delete_account))
        .route("/ui/profile/confirm-password", post(confirm_password))
        .route(
            "/ui/profile/two-factor",
            post(enable_two_factor).delete(disable_two_factor),
        )
        .route("/ui/profile/two-factor/confirm", post(confirm_two_factor))
        .route(
            "/ui/profile/two-factor/recovery-codes",
            post(recovery_codes),
        )
        .route("/ui/profile/passkeys/confirm", post(passkey_confirm))
        .route("/ui/profile/passkeys/options", get(passkey_options))
        .route("/ui/profile/passkeys", post(passkey_store))
        .route("/ui/profile/passkeys/{id}/rename", post(passkey_rename))
        .route("/ui/profile/passkeys/{id}/delete", post(passkey_delete))
        .route_layer(from_fn_with_state(state, crate::session::user_guard))
}

async fn load(state: &AppState, user_id: i64) -> Result<db::ProfileUser, ApiError> {
    db::get(&state.db, user_id)
        .await?
        .ok_or(ApiError::Unauthenticated)
}

fn str_of<'a>(body: &'a Value, key: &str) -> &'a str {
    body.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn field_error(field: &str, message: &str) -> Response {
    let mut errors = Map::new();
    errors.insert(field.into(), json!([message]));
    PhpJson(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": message, "errors": errors}),
    )
    .into_response()
}

/// `$this->validate()` inside `try`: the toast, and the message under the field.
fn invalid(field: &str, message: &str) -> Response {
    let mut errors = Map::new();
    errors.insert(field.into(), json!([message]));
    toast_with_errors(message.to_string(), errors)
}

fn error(message: impl Into<String>) -> Response {
    toast_error(StatusCode::UNPROCESSABLE_ENTITY, message)
}

fn success(message: impl Into<String>) -> Response {
    ok(json!({"message": message.into()})).into_response()
}

/// `$this->auditContext()`.
fn audit_context(team_id: i64, name: &str) -> Value {
    json!({
        "team_id": (team_id != NO_TEAM).then_some(team_id),
        "resource": "user",
        "user_name": name,
    })
}

/// `$this->providerLabel()`.
fn provider_label(provider: &str) -> String {
    match provider {
        "oidc" => "OIDC".to_string(),
        other => coolify_db::audit::headline(other),
    }
}

/// `User::requiresPasswordConfirmation`, as `RequirePassword` uses it.
async fn requires_confirmation(
    state: &AppState,
    headers: &HeaderMap,
    user: &db::ProfileUser,
) -> Result<bool, ApiError> {
    if user.password.as_deref().is_none_or(str::is_empty) {
        return Ok(false);
    }
    if db::sso_provider(&state.db, user.id).await?.is_some() {
        return Ok(false);
    }
    let recent = crate::session::session_id(headers)
        .is_some_and(|id| state.sessions.password_recently_confirmed(&id));
    Ok(!recent)
}

/// The `password.confirm` middleware for a JSON request.
fn confirmation_required() -> Response {
    PhpJson(
        StatusCode::LOCKED,
        json!({"message": "Password confirmation required."}),
    )
    .into_response()
}

/// `PasskeyRelyingParty::resolve`.
fn relying_party(fqdn: Option<&str>, headers: &HeaderMap) -> Option<(String, Vec<String>)> {
    let host_header = headers.get(header::HOST)?.to_str().ok()?;
    let (host, port) = match host_header.rsplit_once(':') {
        Some((h, p)) if p.bytes().all(|b| b.is_ascii_digit()) => (h, Some(p)),
        _ => (host_header, None),
    };
    let host = host.to_lowercase();
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    let configured = std::env::var("APP_URL").ok();
    for url in [fqdn, configured.as_deref()].into_iter().flatten() {
        let Some(rest) = url.trim().strip_prefix("https://") else {
            continue;
        };
        let authority = rest.split('/').next().unwrap_or_default();
        let (url_host, url_port) = match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_lowercase(), Some(p)),
            None => (authority.to_lowercase(), None),
        };
        if url_host != host {
            continue;
        }
        if url_host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok()
        {
            continue;
        }
        let mut origins = vec![match url_port {
            Some(p) => format!("https://{url_host}:{p}"),
            None => format!("https://{url_host}"),
        }];
        if url_port.is_none()
            && let Some(p) = port.filter(|p| *p != "80" && *p != "443")
        {
            origins.push(format!("https://{url_host}:{p}"));
        }
        return Some((url_host, origins));
    }
    (host == "localhost").then(|| {
        (
            "localhost".to_string(),
            vec![format!("{scheme}://{host_header}")],
        )
    })
}

pub(crate) async fn passkey_settings(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(bool, Option<(String, Vec<String>)>), ApiError> {
    let settings = coolify_db::instance_settings::row(&state.db).await?;
    let enabled = settings
        .get("is_passkey_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let rp = enabled
        .then(|| relying_party(settings.get("fqdn").and_then(Value::as_str), headers))
        .flatten();
    Ok((enabled, rp))
}

/// `DeleteUserAccount::blockers` outside Coolify Cloud.
pub(crate) async fn account_blockers(
    state: &AppState,
    user_id: i64,
) -> Result<Vec<String>, ApiError> {
    if user_id == 0 {
        return Ok(vec!["The root user cannot be deleted.".to_string()]);
    }
    let mut out = Vec::new();
    for team in tp::user_teams(&state.db, user_id).await? {
        let alone = team.members == 1;
        if team.id == 0 {
            if alone {
                out.push("You are the only member of the root team.".to_string());
            }
            continue;
        }
        if team.role != "owner" {
            continue;
        }
        if alone {
            if !tp::deletion_blockers(&state.db, team.id).await?.is_empty() {
                out.push(format!(
                    "Delete all projects, servers, and Git sources of the team \"{}\".",
                    team.name
                ));
            }
            continue;
        }
        let members = coolify_db::teams::member_rows(&state.db, team.id).await?;
        let replacement = members
            .iter()
            .any(|m| m.id != user_id && (m.role == "owner" || m.role == "admin"));
        if !replacement {
            out.push(format!(
                "Make another member of the team \"{}\" an admin or owner.",
                team.name
            ));
        }
    }
    Ok(out)
}

pub(crate) fn decrypted(state: &AppState, stored: Option<&str>) -> Option<String> {
    let e = state.encrypter.as_ref()?;
    e.decrypt_maybe_serialized(stored?).ok()
}

/// `Profile\Index::mount` and `render`, with the flash for this view.
async fn show(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    headers: HeaderMap,
) -> ApiResult {
    let user = load(&state, session.user_id).await?;
    let flash = state.sessions.take_flash(&session.id);
    let status = flash.status.as_deref();
    let sso = db::sso_provider(&state.db, user.id).await?;
    let now = state.now();
    let pending = sso.is_none()
        && user.pending_email.is_some()
        && user.email_change_code.is_some()
        && user.email_change_code_expires_at.is_some_and(|t| now < t);
    let mut two_factor = Map::new();
    if status == Some("two-factor-authentication-enabled")
        && let Some(secret) = decrypted(&state, user.two_factor_secret.as_deref())
    {
        let url = totp::otpauth_url(&app_name(), &user.email, &secret);
        two_factor.insert("qr_svg".into(), json!(totp::qr_svg(&url)));
        two_factor.insert("secret".into(), json!(secret));
        two_factor.insert("url".into(), json!(url));
    }
    if user.two_factor_confirmed_at.is_some()
        && matches!(
            status,
            Some("two-factor-authentication-confirmed" | "recovery-codes-generated")
        )
        && let Some(codes) = decrypted(&state, user.two_factor_recovery_codes.as_deref())
    {
        two_factor.insert(
            "recovery_codes".into(),
            serde_json::from_str(&codes).unwrap_or(Value::Null),
        );
    }
    let (enabled, rp) = passkey_settings(&state, &headers).await?;
    let passkeys = if enabled {
        db::passkeys(&state.db, user.id).await?
    } else {
        Vec::new()
    };
    let (text, password) = confirm_steps(&state, &headers, user.id).await?;
    let blockers = account_blockers(&state, user.id).await?;
    Ok(ok(json!({
        "id": user.id,
        "name": user.name,
        "email": user.email,
        "avatar_url": user.avatar_path.as_ref().map(|_| format!("/profile/avatar?v={}", coolify_db::users::timestamp(user.updated_at))),
        "uses_sso": sso.is_some(),
        "sso_provider_label": sso.as_deref().map(provider_label),
        "show_verification": pending,
        "new_email": if pending { user.pending_email.clone() } else { None },
        "code_expiry_minutes": EMAIL_CODE_MINUTES,
        "two_factor_confirmed": user.two_factor_confirmed_at.is_some(),
        "status": status,
        "has_errors": flash.errors,
        "two_factor": Value::Object(two_factor),
        "passkeys": {
            "enabled": enabled,
            "available": rp.is_some(),
            "show_form": status == Some("passkey-add"),
            "list": passkeys.iter().map(|p| json!({
                "id": p.id,
                "name": p.name,
                "authenticator": p.aaguid.as_deref().and_then(webauthn::authenticator_label),
                "created_at": p.created_at.map(coolify_db::records::laravel_datetime_from),
                "last_used_at": p.last_used_at.map(coolify_db::records::laravel_datetime_from),
            })).collect::<Vec<_>>(),
        },
        "account_blockers": blockers,
        "account_actions": ["Your account will be permanently deleted from Coolify."],
        "confirm": {"text": text, "password": password},
    }))
    .into_response())
}

/// `config('app.name')`, the issuer of the one-time password.
pub(crate) fn app_name() -> String {
    std::env::var("APP_NAME").unwrap_or_else(|_| "Coolify".to_string())
}

/// `Profile\Index::submit`.
async fn submit(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Value>,
) -> ApiResult {
    let name = str_of(&body, "name");
    if name.trim().is_empty() {
        return Ok(invalid("name", "The name field is required."));
    }
    db::set_name(&state.db, session.user_id, name, state.now()).await?;
    let mut context = audit_context(session.team_id, name);
    context["changed_fields"] = json!(["name"]);
    crate::audit::log_as(
        &state,
        (session.team_id != NO_TEAM).then_some(session.team_id),
        &crate::audit::actor(&state, &ctx, &meta).await,
        "ui.user.profile_updated",
        context,
    )
    .await;
    Ok(success("Profile updated."))
}

// ---- Avatar ----

async fn upload_avatar(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    body: Bytes,
) -> ApiResult {
    let contents = match prepare_avatar(&body) {
        Ok(c) => c,
        Err(errors) => return Ok(error(summarize(&errors).unwrap_or_default())),
    };
    let path = format!("avatars/{}/avatar.jpg", session.user_id);
    if write_image(&path, &contents).await.is_err() {
        return Ok(toast_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to store the profile picture.",
        ));
    }
    db::set_avatar(&state.db, session.user_id, Some(&path), state.now()).await?;
    Ok(success("Profile picture updated."))
}

async fn remove_avatar(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
) -> ApiResult {
    if let Some((Some(p), _)) = db::avatar_of(&state.db, session.user_id).await? {
        remove_image(&p).await;
    }
    db::set_avatar(&state.db, session.user_id, None, state.now()).await?;
    Ok(success("Profile picture removed."))
}

async fn own_avatar(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
) -> Result<Response, ApiError> {
    let row = db::avatar_of(&state.db, session.user_id).await?;
    let (path, kind) = row.unwrap_or((None, None));
    image_response(
        path.as_deref(),
        kind.is_none() || kind.as_deref() == Some("local"),
    )
    .await
}

// ---- Email change ----

const SSO_EMAIL: &str = "Email addresses managed by SSO cannot be changed in Coolify.";

fn six_digit_code() -> String {
    format!("{:06}", rand::thread_rng().gen_range(0..=999_999))
}

/// `User::requestEmailChange`: the mail is a queued notification, so a failure is not reported.
async fn store_and_send_code(state: &AppState, user_id: i64, email: &str) -> Result<(), ApiError> {
    let code = six_digit_code();
    let now = state.now();
    db::set_email_change(
        &state.db,
        user_id,
        email,
        &code,
        now + TimeDelta::minutes(EMAIL_CODE_MINUTES),
        now,
    )
    .await?;
    let db = state.db.clone();
    let encrypter = state.encrypter.clone();
    let to = email.to_string();
    tokio::spawn(async move {
        let mail = Mail::EmailChangeVerification {
            new_email: to.clone(),
            verification_code: code,
        };
        if let Err(e) = notifications::send_transactional(&db, encrypter.as_ref(), &to, mail).await
        {
            tracing::warn!(error = %e, "email change verification failed");
        }
    });
    Ok(())
}

fn too_many(key: &str, max: usize, secs: u64) -> Option<u64> {
    crate::rules::throttle(key, max, Duration::from_secs(secs))
}

async fn request_email_change(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Value>,
) -> ApiResult {
    use md5::{Digest, Md5};
    if db::sso_provider(&state.db, session.user_id)
        .await?
        .is_some()
    {
        return Ok(error(SSO_EMAIL));
    }
    let settings = coolify_db::instance_settings::row(&state.db).await?;
    let flag = |k: &str| settings.get(k).and_then(Value::as_bool).unwrap_or(false);
    if !flag("smtp_enabled") && !flag("resend_enabled") {
        return Ok(error(
            "Email functionality is not configured. Please contact your administrator.",
        ));
    }
    let new_email = str_of(&body, "new_email");
    if new_email.trim().is_empty() {
        return Ok(invalid("new_email", "The new email field is required."));
    }
    if !valid_email(new_email) {
        return Ok(invalid(
            "new_email",
            "The new email field must be a valid email address.",
        ));
    }
    if db::email_taken(&state.db, new_email).await? {
        return Ok(invalid(
            "new_email",
            "The new email has already been taken.",
        ));
    }
    let new_email = new_email.to_lowercase();
    if let Some(s) = too_many(&format!("email-change:user:{}", session.user_id), 1, 120) {
        return Ok(error(format!(
            "Too many requests. Please wait {s} seconds before trying again."
        )));
    }
    let hashed: String = Md5::digest(new_email.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if too_many(&format!("email-change:email:{hashed}"), 3, 3600).is_some() {
        return Ok(error(
            "This email address has received too many verification requests. Please try again later.",
        ));
    }
    let ip = meta.ip.clone().unwrap_or_default();
    if too_many(&format!("email-change:ip:{ip}"), 5, 3600).is_some() {
        return Ok(error(
            "Too many requests from your IP address. Please try again later.",
        ));
    }
    store_and_send_code(&state, session.user_id, &new_email).await?;
    let user = load(&state, session.user_id).await?;
    crate::audit::log_as(
        &state,
        (session.team_id != NO_TEAM).then_some(session.team_id),
        &crate::audit::actor(&state, &ctx, &meta).await,
        "ui.user.email_change_requested",
        audit_context(session.team_id, &user.name),
    )
    .await;
    Ok(success(format!("Verification code sent to {new_email}")))
}

async fn verify_email_change(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Value>,
) -> ApiResult {
    if db::sso_provider(&state.db, session.user_id)
        .await?
        .is_some()
    {
        return Ok(error(SSO_EMAIL));
    }
    let code = str_of(&body, "code");
    if code.trim().is_empty() {
        return Ok(invalid(
            "email_verification_code",
            "The email verification code field is required.",
        ));
    }
    if code.chars().count() != 6 {
        return Ok(invalid(
            "email_verification_code",
            "The email verification code field must be 6 characters.",
        ));
    }
    let key = format!("email-verify:user:{}", session.user_id);
    if let Some(s) = too_many(&key, 5, 600) {
        return Ok(error(format!(
            "Too many verification attempts. Please wait {} minutes before trying again.",
            s.div_ceil(60)
        )));
    }
    let user = load(&state, session.user_id).await?;
    let valid = user.email_change_code.as_deref() == Some(code)
        && user
            .email_change_code_expires_at
            .is_some_and(|t| state.now() < t);
    if !valid {
        return Ok(error("Invalid or expired verification code."));
    }
    let Some(new_email) = user.pending_email.as_deref() else {
        return Ok(error("Failed to update email address."));
    };
    db::confirm_email_change(&state.db, session.user_id, new_email, state.now()).await?;
    crate::audit::log_as(
        &state,
        (session.team_id != NO_TEAM).then_some(session.team_id),
        &crate::audit::actor(&state, &ctx, &meta).await,
        "ui.user.email_changed",
        audit_context(session.team_id, &user.name),
    )
    .await;
    Ok(success("Email address updated successfully."))
}

async fn resend_code(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
) -> ApiResult {
    use md5::{Digest, Md5};
    if db::sso_provider(&state.db, session.user_id)
        .await?
        .is_some()
    {
        return Ok(error(SSO_EMAIL));
    }
    let user = load(&state, session.user_id).await?;
    let now = state.now();
    let (Some(pending), Some(_), Some(expires)) = (
        user.pending_email.as_deref(),
        user.email_change_code.as_deref(),
        user.email_change_code_expires_at.filter(|t| now < *t),
    ) else {
        return Ok(error("No pending email change request."));
    };
    let half = EMAIL_CODE_MINUTES as f64 / 2.0;
    let since =
        (now - (expires - TimeDelta::minutes(EMAIL_CODE_MINUTES))).num_seconds() as f64 / 60.0;
    if since < half {
        let wait = (half - since).ceil() as i64;
        return Ok(error(format!(
            "Please wait {wait} more minutes before requesting a new code."
        )));
    }
    let hashed: String = Md5::digest(pending.to_lowercase().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if too_many(&format!("email-change:email:{hashed}"), 3, 3600).is_some() {
        return Ok(error(
            "This email address has received too many verification requests. Please try again later.",
        ));
    }
    store_and_send_code(&state, session.user_id, pending).await?;
    Ok(success(format!("New verification code sent to {pending}")))
}

async fn cancel_email_change(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
) -> ApiResult {
    db::clear_email_change(&state.db, session.user_id, state.now()).await?;
    Ok(success("Email change request cancelled."))
}

// ---- Password ----

/// `Password::defaults()` in production, without `uncompromised()`, which asks an external
/// service about the password; and `confirmed`.
fn password_errors(new: &str, confirmation: &str) -> Vec<String> {
    password_errors_for("new password", new, confirmation)
}

/// The same rules for the attribute `field`.
pub(crate) fn password_errors_for(field: &str, new: &str, confirmation: &str) -> Vec<String> {
    let label = format!("The {field} field");
    #[allow(non_snake_case)]
    let FIELD = label.as_str();
    let mut out = Vec::new();
    if new.is_empty() {
        out.push(format!("{FIELD} is required."));
        return out;
    }
    if new.chars().count() < 8 {
        out.push(format!("{FIELD} must be at least 8 characters."));
    }
    let lower = new.chars().any(char::is_lowercase);
    let upper = new.chars().any(char::is_uppercase);
    if !lower || !upper {
        out.push(format!(
            "{FIELD} must contain at least one uppercase and one lowercase letter."
        ));
    }
    if !new.chars().any(char::is_alphabetic) {
        out.push(format!("{FIELD} must contain at least one letter."));
    }
    if !new
        .chars()
        .any(|c| !c.is_alphanumeric() && !c.is_whitespace())
    {
        out.push(format!("{FIELD} must contain at least one symbol."));
    }
    if !new.chars().any(char::is_numeric) {
        out.push(format!("{FIELD} must contain at least one number."));
    }
    if new != confirmation {
        out.push(format!("{FIELD} confirmation does not match."));
    }
    out
}

async fn reset_password(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Value>,
) -> ApiResult {
    let current = str_of(&body, "current_password");
    let new = str_of(&body, "new_password");
    let mut errors = Vec::new();
    let mut fields = Map::new();
    if current.is_empty() {
        errors.push("The current password field is required.".to_string());
        fields.insert(
            "current_password".to_string(),
            json!(["The current password field is required."]),
        );
    }
    let new_errors = password_errors(new, str_of(&body, "new_password_confirmation"));
    if !new_errors.is_empty() {
        fields.insert("new_password".to_string(), json!(new_errors));
    }
    errors.extend(new_errors);
    if let Some(m) = summarize(&errors) {
        return Ok(toast_with_errors(m, fields));
    }
    let user = load(&state, session.user_id).await?;
    if !user
        .password
        .as_deref()
        .is_some_and(|h| coolify_compat::password::verify(current, h))
    {
        return Ok(error("Current password is incorrect."));
    }
    let hash = coolify_compat::password::hash(new).ok_or(ApiError::Internal)?;
    let actor = crate::audit::actor(&state, &ctx, &meta).await;
    tp::set_password(&state.db, session.user_id, &hash, state.now()).await?;
    crate::audit::log_as(
        &state,
        (session.team_id != NO_TEAM).then_some(session.team_id),
        &actor,
        "ui.user.password_changed",
        audit_context(session.team_id, &user.name),
    )
    .await;
    // `deleteAllSessions`: every session ends, this one too.
    state.sessions.remove_user(session.user_id);
    Ok(success("Password updated."))
}

/// `Profile\Index::deleteAccount`.
async fn delete_account(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    if !session_password_ok(&state, &headers, session.user_id, &body).await? {
        return Ok(password_incorrect());
    }
    let blockers = account_blockers(&state, session.user_id).await?;
    if !blockers.is_empty() {
        return Ok(error(blockers.join(" ")));
    }
    let user = load(&state, session.user_id).await?;
    let actor = crate::audit::actor(&state, &ctx, &meta).await;
    let deleted = tp::delete_account(&state.db, session.user_id, &user.email).await?;
    for row in &deleted {
        let id = row.get("id").and_then(Value::as_i64).unwrap_or_default();
        crate::audit::team_mutation(&state, &actor, id, "deleted", Some(row), None).await;
    }
    if let Some(p) = &user.avatar_path {
        remove_image(p).await;
    }
    // `currentTeam()` finds no team on a user who left them all.
    crate::audit::log_as(
        &state,
        None,
        &actor,
        "ui.user.account_deleted",
        json!({"resource": "user"}),
    )
    .await;
    state.sessions.remove_user(session.user_id);
    Ok(ok(json!({"message": "ok"})).into_response())
}

/// `ConfirmablePasswordController::store`.
async fn confirm_password(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    Json(body): Json<Value>,
) -> ApiResult {
    let user = load(&state, session.user_id).await?;
    let good = user
        .password
        .as_deref()
        .is_some_and(|h| coolify_compat::password::verify(str_of(&body, "password"), h));
    if !good {
        return Ok(field_error(
            "password",
            "The provided password was incorrect.",
        ));
    }
    state.sessions.mark_password_confirmed(&session.id);
    Ok(ok(json!({"message": "ok"})).into_response())
}

// ---- Fortify two-factor routes ----

/// `auditLog($event, $this->authContext($event->user))` of `EventServiceProvider`.
async fn audit_auth_event(state: &AppState, session: &SessionUser, meta: &Meta, event: &str) {
    if let Ok(Some(user)) = coolify_db::users::get(&state.db, session.user_id).await {
        crate::session::audit_auth(
            state,
            (session.team_id != NO_TEAM).then_some(session.team_id),
            (user.id, &user.name, &user.email),
            meta,
            event,
        )
        .await;
    }
}

fn set_status(state: &AppState, session: &SessionUser, status: &str) {
    state.sessions.set_flash(
        &session.id,
        Flash {
            status: Some(status.to_string()),
            errors: false,
        },
    );
}

fn generate_recovery_codes(encrypter: &coolify_compat::Encrypter) -> String {
    let codes: Vec<String> = (0..8).map(|_| totp::recovery_code()).collect();
    encrypter.encrypt_serialized(&Value::from(codes).to_string())
}

/// `EnableTwoFactorAuthentication`: an existing unconfirmed secret stays.
async fn enable_two_factor(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    meta: Meta,
    headers: HeaderMap,
) -> ApiResult {
    let user = load(&state, session.user_id).await?;
    if requires_confirmation(&state, &headers, &user).await? {
        return Ok(confirmation_required());
    }
    let encrypter = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    if user.two_factor_secret.as_deref().is_none_or(str::is_empty) {
        db::set_two_factor(
            &state.db,
            user.id,
            &encrypter.encrypt_serialized(&totp::generate_secret(16)),
            &generate_recovery_codes(encrypter),
            state.now(),
        )
        .await?;
        audit_auth_event(&state, &session, &meta, "auth.user.two_factor_enabled").await;
    }
    set_status(&state, &session, "two-factor-authentication-enabled");
    Ok(ok(json!({"message": "ok"})).into_response())
}

/// `ConfirmTwoFactorAuthentication`.
async fn confirm_two_factor(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    meta: Meta,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    let user = load(&state, session.user_id).await?;
    if requires_confirmation(&state, &headers, &user).await? {
        return Ok(confirmation_required());
    }
    let secret = decrypted(&state, user.two_factor_secret.as_deref());
    let code = str_of(&body, "code");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let valid = secret
        .as_deref()
        .is_some_and(|s| !code.is_empty() && totp::verify(s, code, now, 1, None).is_some());
    if !valid {
        // The named error bag of a redirect back: only the page's general callout shows it.
        state.sessions.set_flash(
            &session.id,
            Flash {
                status: None,
                errors: true,
            },
        );
        return Ok(ok(json!({"message": "invalid"})).into_response());
    }
    db::confirm_two_factor(&state.db, user.id, state.now()).await?;
    audit_auth_event(&state, &session, &meta, "auth.user.two_factor_confirmed").await;
    set_status(&state, &session, "two-factor-authentication-confirmed");
    Ok(ok(json!({"message": "ok"})).into_response())
}

/// `DisableTwoFactorAuthentication`.
async fn disable_two_factor(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    meta: Meta,
    headers: HeaderMap,
) -> ApiResult {
    let user = load(&state, session.user_id).await?;
    if requires_confirmation(&state, &headers, &user).await? {
        return Ok(confirmation_required());
    }
    db::disable_two_factor(&state.db, user.id, state.now()).await?;
    audit_auth_event(&state, &session, &meta, "auth.user.two_factor_disabled").await;
    set_status(&state, &session, "two-factor-authentication-disabled");
    Ok(ok(json!({"message": "ok"})).into_response())
}

/// `GenerateNewRecoveryCodes`.
async fn recovery_codes(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    meta: Meta,
    headers: HeaderMap,
) -> ApiResult {
    let user = load(&state, session.user_id).await?;
    if requires_confirmation(&state, &headers, &user).await? {
        return Ok(confirmation_required());
    }
    let encrypter = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    db::set_recovery_codes(
        &state.db,
        user.id,
        &generate_recovery_codes(encrypter),
        state.now(),
    )
    .await?;
    audit_auth_event(
        &state,
        &session,
        &meta,
        "auth.user.recovery_codes_regenerated",
    )
    .await;
    set_status(&state, &session, "recovery-codes-generated");
    Ok(ok(json!({"message": "ok"})).into_response())
}

// ---- Passkeys ----

/// `/profile/passkeys/confirm`: the form shows on the next `/profile` view.
async fn passkey_confirm(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    headers: HeaderMap,
) -> ApiResult {
    let user = load(&state, session.user_id).await?;
    if requires_confirmation(&state, &headers, &user).await? {
        return Ok(confirmation_required());
    }
    set_status(&state, &session, "passkey-add");
    Ok(ok(json!({"message": "ok"})).into_response())
}

const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Not Found");

/// `User::getPasskeyUserHandle`, encoded for the browser as upstream's options are.
fn user_handle(state: &AppState, user_id: i64) -> Option<String> {
    let secret = std::env::var("PASSKEYS_USER_HANDLE_SECRET").ok();
    let hex = match secret {
        Some(s) => {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;
            let mut mac = Hmac::<Sha256>::new_from_slice(s.as_bytes()).ok()?;
            mac.update(format!("users|{user_id}").as_bytes());
            mac.finalize()
                .into_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        }
        None => state
            .encrypter
            .as_ref()?
            .hmac_with_app_key(format!("users|{user_id}").as_bytes()),
    };
    Some(webauthn::b64(hex.as_bytes()))
}

/// `throttle:6,1` of the passkey routes.
fn passkey_throttle(user_id: i64) -> Result<(), ApiError> {
    match crate::rules::throttle(&format!("passkeys:{user_id}"), 6, Duration::from_secs(60)) {
        Some(s) => Err(ApiError::Throttled(s)),
        None => Ok(()),
    }
}

/// `PasskeyRegistrationController::index`.
async fn passkey_options(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    headers: HeaderMap,
) -> ApiResult {
    let (_, rp) = passkey_settings(&state, &headers).await?;
    let (rp_id, _) = rp.ok_or(NOT_FOUND)?;
    let user = load(&state, session.user_id).await?;
    if requires_confirmation(&state, &headers, &user).await? {
        return Ok(confirmation_required());
    }
    passkey_throttle(user.id)?;
    let mut challenge = [0u8; 32];
    rand::thread_rng().fill(&mut challenge);
    let challenge = webauthn::b64(&challenge);
    let handle = user_handle(&state, user.id).ok_or(ApiError::Internal)?;
    let exclude: Vec<Value> = db::passkeys(&state.db, user.id)
        .await?
        .iter()
        .map(|p| json!({"type": "public-key", "id": p.credential_id}))
        .collect();
    state.sessions.set_passkey_challenge(
        &session.id,
        json!({"challenge": challenge, "user_handle": handle}).to_string(),
    );
    Ok(ok(json!({
        "options": {
            "challenge": challenge,
            "timeout": 60000,
            "rp": {"id": rp_id, "name": rp_id},
            "user": {"id": handle, "name": user.email, "displayName": user.name},
            "pubKeyCredParams": [{"type": "public-key", "alg": -7}, {"type": "public-key", "alg": -257}],
            "authenticatorSelection": {"requireResidentKey": true, "userVerification": "required", "residentKey": "required"},
            "attestation": "none",
            "excludeCredentials": exclude,
        }
    }))
    .into_response())
}

/// `PasskeyRegistrationController::store` and `StorePasskey`.
async fn passkey_store(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    meta: Meta,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    let (_, rp) = passkey_settings(&state, &headers).await?;
    let (rp_id, origins) = rp.ok_or(NOT_FOUND)?;
    let user = load(&state, session.user_id).await?;
    if requires_confirmation(&state, &headers, &user).await? {
        return Ok(confirmation_required());
    }
    passkey_throttle(user.id)?;
    let name = str_of(&body, "name");
    if name.trim().is_empty() {
        return Ok(field_error("name", "The name field is required."));
    }
    if name.chars().count() > 255 {
        return Ok(field_error(
            "name",
            "The name field must not be greater than 255 characters.",
        ));
    }
    let credential = body.get("credential").cloned().unwrap_or(Value::Null);
    if credential.get("type").and_then(Value::as_str) != Some("public-key") {
        return Ok(field_error("credential", "Invalid credential format."));
    }
    let stored = state.sessions.passkey_challenge(&session.id);
    let stored: Option<Value> = stored.as_deref().and_then(|s| serde_json::from_str(s).ok());
    let Some(stored) = stored else {
        return Ok(error("Unable to register passkey. Please try again."));
    };
    let checked = webauthn::verify_registration(
        &credential,
        &webauthn::Expected {
            challenge: str_of(&stored, "challenge"),
            rp_id: &rp_id,
            origins: &origins,
            user_handle: str_of(&stored, "user_handle"),
        },
    );
    let registered = match checked {
        Ok(r) => r,
        Err(m) => return Ok(error(m)),
    };
    if db::passkey_exists(&state.db, &registered.credential_id).await? {
        return Ok(error("Unable to register this passkey."));
    }
    let id = db::create_passkey(
        &state.db,
        user.id,
        name,
        &registered.credential_id,
        &registered.credential.to_string(),
        state.now(),
    )
    .await?;
    audit_auth_event(&state, &session, &meta, "auth.user.passkey_registered").await;
    Ok(
        ok(json!({"status": "passkey-registered", "id": id.to_string(), "name": name}))
            .into_response(),
    )
}

/// `Profile\Passkeys::rename`.
async fn passkey_rename(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult {
    let (enabled, _) = passkey_settings(&state, &headers).await?;
    if !enabled || !db::passkey_owned(&state.db, session.user_id, id).await? {
        return Err(NOT_FOUND);
    }
    let name = str_of(&body, "name").trim();
    if name.is_empty() {
        return Ok(field_error("name", "The name field is required."));
    }
    if name.chars().count() > 255 {
        return Ok(field_error(
            "name",
            "The name field must not be greater than 255 characters.",
        ));
    }
    db::rename_passkey(&state.db, session.user_id, id, name, state.now()).await?;
    Ok(success("Passkey renamed."))
}

/// `Profile\Passkeys::delete`.
async fn passkey_delete(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    meta: Meta,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult {
    let (enabled, _) = passkey_settings(&state, &headers).await?;
    if !enabled || !db::passkey_owned(&state.db, session.user_id, id).await? {
        return Err(NOT_FOUND);
    }
    if !session_password_ok(&state, &headers, session.user_id, &body).await? {
        return Ok(password_incorrect());
    }
    db::delete_passkey(&state.db, session.user_id, id).await?;
    audit_auth_event(&state, &session, &meta, "auth.user.passkey_deleted").await;
    Ok(success("Passkey deleted."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_rules() {
        assert_eq!(
            password_errors("", ""),
            vec!["The new password field is required."]
        );
        assert!(password_errors("Abcdef1!", "Abcdef1!").is_empty());
        let weak = password_errors("abc", "x");
        assert!(weak.iter().any(|e| e.contains("at least 8 characters")));
        assert!(
            weak.iter()
                .any(|e| e.contains("confirmation does not match"))
        );
    }

    #[test]
    fn relying_parties() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, "localhost:18086".parse().unwrap());
        let (id, origins) = relying_party(None, &h).unwrap();
        assert_eq!(id, "localhost");
        assert_eq!(origins, vec!["http://localhost:18086"]);
        h.insert(header::HOST, "127.0.0.1:18086".parse().unwrap());
        assert!(relying_party(None, &h).is_none());
        h.insert(header::HOST, "app.example.test".parse().unwrap());
        let (id, origins) = relying_party(Some("https://app.example.test"), &h).unwrap();
        assert_eq!(id, "app.example.test");
        assert_eq!(origins, vec!["https://app.example.test"]);
        assert!(relying_party(Some("http://app.example.test"), &h).is_none());
    }
}
