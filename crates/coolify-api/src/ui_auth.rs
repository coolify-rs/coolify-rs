//! The sign-in screens other than the login form itself: `FortifyServiceProvider`'s views and
//! `CreateNewUser`, `RootSetupWindow`, `Controller::forgot_password` and the password broker,
//! `ResetUserPassword`, `App\Livewire\ForcePasswordReset`, `App\Livewire\VerifyEmail`,
//! `Controller::email_verify`, `App\Livewire\TwoFactorRequired`, `laravel/passkeys` sign-in and
//! confirmation (`PasskeyLoginController`, `PasskeyConfirmationController`, `VerifyPasskey`).
//! `SettingsOauth` and `OauthController` are in [`oauth`].

mod oauth;

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::TimeDelta;
use coolify_compat::{password, totp, webauthn};
use coolify_db::{auth_pages as db, profile, users};
use coolify_jobs::notifications::{self, Ctx, Mail};
use rand::Rng;
use serde_json::{Map, Value, json};

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::rules::{is_email, normalize_email_identity, throttle, too_many};
use crate::session::{self, Flash, NO_TEAM, SessionUser};
use crate::state::AppState;
use crate::ui_notifications::{summarize, toast_error};
use crate::ui_profile::{app_name, decrypted, passkey_settings, password_errors_for};
use crate::ui_teams::toast_with_errors;

/// `RootSetupWindow::MINUTES`.
const ROOT_SETUP_MINUTES: u64 = 10;
/// `config('auth.passwords.users.expire')`, in minutes.
const RESET_EXPIRE_MINUTES: i64 = 10;
/// `config('auth.passwords.users.throttle')`, in seconds.
const RESET_THROTTLE_SECONDS: i64 = 60;
/// `config('auth.verification.expire')`, in minutes.
const VERIFICATION_EXPIRE_MINUTES: i64 = 60;

const PASSWORD_LINK_SENT: &str =
    "If an account exists with this email address, you will receive a password reset link shortly.";
const PASSWORD_TOKEN_INVALID: &str = "This password reset token is invalid.";
const PASSWORD_RESET_DONE: &str = "Your password has been reset.";
const NOT_ACTIVE: &str = "Transactional emails are not active";

static STARTED: OnceLock<Instant> = OnceLock::new();

/// `RootSetupWindow::isOpen`: the first minutes after Coolify starts.
fn root_setup_open() -> bool {
    STARTED.get_or_init(Instant::now).elapsed().as_secs() < ROOT_SETUP_MINUTES * 60
}

pub fn routes(state: AppState) -> Router<AppState> {
    STARTED.get_or_init(Instant::now);
    let guest = Router::new()
        .route("/ui/auth/state", get(guest_state))
        .route("/ui/auth/register", post(register))
        .route("/ui/auth/forgot-password", post(forgot_password))
        .route("/ui/auth/reset-password", post(reset_password))
        .route(
            "/ui/auth/passkeys/login/options",
            get(passkey_login_options),
        )
        .route("/ui/auth/passkeys/login", post(passkey_login))
        .route("/auth/{provider}/redirect", get(oauth::redirect))
        .route("/auth/{provider}/callback", get(oauth::callback));
    let signed_in = Router::new()
        .route("/ui/auth/force-password-reset", post(force_password_reset))
        .route("/ui/auth/verify/resend", post(resend_verification))
        .route("/ui/auth/email-verify", post(email_verify))
        .route("/ui/auth/two-factor-required", get(two_factor_required))
        .route("/ui/auth/confirm-password/state", get(confirm_state))
        .route(
            "/ui/auth/passkeys/confirm/options",
            get(passkey_confirm_options),
        )
        .route("/ui/auth/passkeys/confirm", post(passkey_confirm))
        .route("/ui/onboarding/skip", post(skip_boarding))
        .route(
            "/ui/settings/oauth",
            get(oauth::show).post(oauth::submit_all),
        )
        .route(
            "/ui/settings/oauth/registration-policy",
            post(oauth::registration_policy),
        )
        .route("/ui/settings/oauth/{provider}", post(oauth::submit_one))
        .route("/ui/settings/oauth/{provider}/toggle", post(oauth::toggle))
        .route_layer(from_fn_with_state(state, session::user_guard));
    guest.merge(signed_in)
}

fn str_of<'a>(body: &'a Value, key: &str) -> &'a str {
    body.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// A `ValidationException`, for the page's error list.
fn invalid(errors: Map<String, Value>) -> Response {
    let message = errors
        .values()
        .next()
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .unwrap_or("The given data was invalid.")
        .to_string();
    PhpJson(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": message, "errors": errors}),
    )
    .into_response()
}

fn one_error(field: &str, message: &str) -> Response {
    let mut errors = Map::new();
    errors.insert(field.into(), json!([message]));
    invalid(errors)
}

/// `isPasswordRegistrationAllowed`.
fn password_registration_allowed(settings: &Map<String, Value>, rows: &[db::OauthRow]) -> bool {
    let flag = |k: &str| settings.get(k).and_then(Value::as_bool).unwrap_or(false);
    if !flag("is_registration_enabled") {
        return false;
    }
    !flag("disable_registration_when_oauth_enabled") || !rows.iter().any(|r| r.enabled)
}

/// What the views get from `Fortify::loginView`, `registerView`, `requestPasswordResetLinkView`,
/// with the flash for this view.
async fn guest_state(State(state): State<AppState>, headers: HeaderMap) -> ApiResult {
    let settings = coolify_db::instance_settings::row(&state.db).await?;
    let rows = db::oauth_settings(&state.db).await?;
    let users_count = db::user_count(&state.db).await?;
    let allowed = password_registration_allowed(&settings, &rows);
    let flag = |k: &str| settings.get(k).and_then(Value::as_bool).unwrap_or(false);
    let enabled: Vec<&db::OauthRow> = rows.iter().filter(|r| r.enabled).collect();
    let registration_enabled = flag("is_registration_enabled");
    let can_register_with_oauth = enabled.iter().any(|r| {
        oauth::could_be_enabled(r) && oauth::allows_user_creation(r, registration_enabled)
    });
    let (_, rp) = passkey_settings(&state, &headers).await?;
    let (guest, cookie) = session::guest_id(&state, &headers);
    let (status, error, errors, challenge) = state
        .sessions
        .guest(&guest, |g| {
            (
                g.status.take(),
                g.error.take(),
                std::mem::take(&mut g.errors),
                g.login_id.is_some(),
            )
        })
        .unwrap_or_default();
    let first = users_count == 0;
    Ok(session::with_guest_cookie(
        ok(json!({
            "is_registration_enabled": allowed,
            "can_register_with_oauth": can_register_with_oauth,
            "enabled_oauth_providers": enabled.iter().map(|r| json!({
                "provider": r.provider,
                "label": oauth::login_label(r),
            })).collect::<Vec<_>>(),
            "passkeys_available": rp.is_some(),
            "transactional_emails": flag("smtp_enabled") || flag("resend_enabled"),
            "redirect_to_register": first && allowed,
            "is_first_user": first,
            "is_root_setup_window_closed": first && !root_setup_open(),
            "root_setup_minutes": ROOT_SETUP_MINUTES,
            "two_factor_challenge": challenge,
            "status": status,
            "error": error,
            "errors": errors,
        }))
        .into_response(),
        cookie,
    ))
}

// ---- Registration ----

/// `CreateNewUser::ensureRegistrationIsNotRateLimited`.
fn registration_limited(email: &str, ip: &str) -> bool {
    use sha1::{Digest, Sha1};
    let hex = |s: &str| {
        Sha1::digest(s.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let mut keys = vec![(
        format!("registration:ip:{}", hex(ip)),
        3,
        Duration::from_secs(600),
    )];
    if let Some(identity) = normalize_email_identity(Some(email)) {
        keys.push((
            format!("registration:email-identity:{}", hex(&identity)),
            3,
            Duration::from_secs(3600),
        ));
    }
    if keys.iter().any(|(k, m, w)| too_many(k, *m, *w).is_some()) {
        return true;
    }
    for (k, m, w) in &keys {
        let _ = throttle(k, *m, *w);
    }
    false
}

/// `RootUserSeeder`: `ROOT_USER_EMAIL` and `ROOT_USER_PASSWORD` create the root user at startup and
/// close registration. Invalid values are reported and change nothing. Its breach check of the
/// password is not run: a server does not send anything to a third party while it starts.
pub async fn seed_root_user(state: &AppState) -> Result<(), ApiError> {
    if db::user_count(&state.db).await? != 0 {
        return Ok(());
    }
    let setting = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
    let (Some(email), Some(pass)) = (setting("ROOT_USER_EMAIL"), setting("ROOT_USER_PASSWORD"))
    else {
        return Ok(());
    };
    // The installer writes an empty ROOT_USERNAME line, so an empty value must also use the default.
    let name = setting("ROOT_USERNAME").unwrap_or_else(|| "Root User".to_owned());
    let name_valid = (3..=255).contains(&name.chars().count())
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c.is_whitespace());
    let email = email.trim().to_lowercase();
    let password_errors = password_errors_for("password", &pass, &pass);
    if !is_email(&email) || !name_valid || !password_errors.is_empty() {
        tracing::error!("Invalid Root User Environment Variables");
        return Ok(());
    }
    let hash = password::hash(&pass).ok_or(ApiError::Internal)?;
    db::create_root_user(&state.db, &name, &email, &hash, state.now()).await?;
    tracing::info!("Root user created successfully.");
    Ok(())
}

/// `RegisteredUserController::store` with `CreateNewUser::create`.
async fn register(State(state): State<AppState>, meta: Meta, Json(body): Json<Value>) -> ApiResult {
    let settings = coolify_db::instance_settings::row(&state.db).await?;
    let rows = db::oauth_settings(&state.db).await?;
    if !password_registration_allowed(&settings, &rows) {
        return Err(ApiError::Abort(StatusCode::FORBIDDEN, "Forbidden"));
    }
    let first = db::user_count(&state.db).await? == 0;
    if first && !root_setup_open() {
        return Ok(one_error(
            "email",
            "The setup window for the root account is closed. Restart Coolify on the server (docker restart coolify) to open it again.",
        ));
    }
    let name = str_of(&body, "name").trim();
    let email = str_of(&body, "email").trim().to_lowercase();
    if registration_limited(&email, meta.ip.as_deref().unwrap_or_default()) {
        return Err(ApiError::Abort(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many registration attempts. Please try again later.",
        ));
    }
    let mut errors = Map::new();
    if name.is_empty() {
        errors.insert("name".into(), json!(["The name field is required."]));
    } else if name.chars().count() > 255 {
        errors.insert(
            "name".into(),
            json!(["The name field must not be greater than 255 characters."]),
        );
    }
    let mut email_errors: Vec<&str> = Vec::new();
    if email.is_empty() {
        email_errors.push("The email field is required.");
    } else {
        if !is_email(&email) {
            email_errors.push("The email field must be a valid email address.");
        }
        if email.chars().count() > 255 {
            email_errors.push("The email field must not be greater than 255 characters.");
        }
        if users::find_by_email(&state.db, &email).await?.is_some() {
            email_errors.push("The email has already been taken.");
        }
    }
    if !email_errors.is_empty() {
        errors.insert("email".into(), json!(email_errors));
    }
    let pass = str_of(&body, "password");
    let password_errors =
        password_errors_for("password", pass, str_of(&body, "password_confirmation"));
    if !password_errors.is_empty() {
        errors.insert("password".into(), json!(password_errors));
    }
    if !errors.is_empty() {
        return Ok(invalid(errors));
    }
    let hash = password::hash(pass).ok_or(ApiError::Internal)?;
    let now = state.now();
    let (id, team) = if first {
        if db::create_root_user(&state.db, name, &email, &hash, now).await? {
            coolify_db::notification_settings::create_defaults(&state.db, 0).await?;
        }
        (0, 0)
    } else {
        let (id, team) = db::create_user(&state.db, name, &email, &hash, now).await?;
        coolify_db::notification_settings::create_defaults(&state.db, team).await?;
        db::mark_email_verified(&state.db, id, now).await?;
        (id, team)
    };
    session::audit_guest(
        &state,
        Some(team),
        (id, name, &email),
        &meta,
        "auth.user.registered",
    )
    .await;
    let (cookie, mut info) = session::establish(&state, id, |_| Some(team), &meta).await?;
    // `RegisterResponse`: the root user continues on the settings page.
    info["redirect"] = json!(if team == 0 { "/settings" } else { "/" });
    Ok(session::signed_in(cookie, info))
}

// ---- Password reset ----

/// `throttle:forgot-password`.
fn forgot_limited(email: &str, ip: &str) -> Option<u64> {
    use sha1::{Digest, Sha1};
    let hex = |s: &str| {
        Sha1::digest(s.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let mut keys = vec![(
        format!("forgot-password:ip:{}", hex(ip)),
        Duration::from_secs(600),
    )];
    if let Some(identity) = normalize_email_identity(Some(email)) {
        keys.push((
            format!("forgot-password:email-identity:{}", hex(&identity)),
            Duration::from_secs(3600),
        ));
    }
    if let Some(secs) = keys.iter().find_map(|(k, w)| too_many(k, 3, *w)) {
        return Some(secs);
    }
    for (k, w) in &keys {
        let _ = throttle(k, 3, *w);
    }
    None
}

fn random_text(len: usize) -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}

/// `Controller::forgot_password` and `PasswordBroker::sendResetLink`.
async fn forgot_password(
    State(state): State<AppState>,
    meta: Meta,
    Json(body): Json<Value>,
) -> ApiResult {
    let email = str_of(&body, "email").trim().to_lowercase();
    if let Some(secs) = forgot_limited(&email, meta.ip.as_deref().unwrap_or_default()) {
        return Err(ApiError::Throttled(secs));
    }
    let settings = coolify_db::instance_settings::row(&state.db).await?;
    let flag = |k: &str| settings.get(k).and_then(Value::as_bool).unwrap_or(false);
    if !flag("smtp_enabled") && !flag("resend_enabled") {
        return Ok(
            PhpJson(StatusCode::BAD_REQUEST, json!({"message": NOT_ACTIVE})).into_response(),
        );
    }
    if email.is_empty() {
        return Ok(one_error("email", "The email field is required."));
    }
    if !is_email(&email) {
        return Ok(one_error(
            "email",
            "The email field must be a valid email address.",
        ));
    }
    let Some(user) = users::find_by_email(&state.db, &email).await? else {
        return Ok(one_error("email", PASSWORD_LINK_SENT));
    };
    let now = state.now();
    if let Some((_, Some(created))) = db::reset_token(&state.db, &user.email).await?
        && created + TimeDelta::seconds(RESET_THROTTLE_SECONDS) > now
    {
        return Ok(PhpJson(
            StatusCode::BAD_REQUEST,
            json!({"message": "Already requested a password reset in the past minutes."}),
        )
        .into_response());
    }
    let encrypter = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let token = encrypter.hmac_with_app_key(random_text(40).as_bytes());
    let hash = password::hash(&token).ok_or(ApiError::Internal)?;
    db::put_reset_token(&state.db, &user.email, &hash, now).await?;
    // `ResetPassword` is a queued notification: a mail failure does not reach the page.
    let st = state.clone();
    let to = user.email.clone();
    tokio::spawn(async move {
        let mail = Mail::ResetPassword {
            token,
            email: to.clone(),
        };
        if let Err(e) =
            notifications::send_transactional(&st.db, st.encrypter.as_ref(), &to, mail).await
        {
            tracing::warn!(error = %e, "password reset mail failed");
        }
    });
    Ok(ok(json!({"status": PASSWORD_LINK_SENT})).into_response())
}

/// `NewPasswordController::store`, `PasswordBroker::reset`, `ResetUserPassword::reset` and
/// `CompletePasswordReset`.
async fn reset_password(
    State(state): State<AppState>,
    meta: Meta,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    let email = str_of(&body, "email").trim().to_lowercase();
    let token = str_of(&body, "token");
    let pass = str_of(&body, "password");
    let mut errors = Map::new();
    if token.is_empty() {
        errors.insert("token".into(), json!(["The token field is required."]));
    }
    if email.is_empty() {
        errors.insert("email".into(), json!(["The email field is required."]));
    } else if !is_email(&email) {
        errors.insert(
            "email".into(),
            json!(["The email field must be a valid email address."]),
        );
    }
    if pass.is_empty() {
        errors.insert(
            "password".into(),
            json!(["The password field is required."]),
        );
    }
    if !errors.is_empty() {
        return Ok(invalid(errors));
    }
    let Some(user) = users::find_by_email(&state.db, &email).await? else {
        return Ok(one_error("email", PASSWORD_LINK_SENT));
    };
    let now = state.now();
    let valid = db::reset_token(&state.db, &user.email)
        .await?
        .is_some_and(|(hash, created)| {
            created.is_some_and(|c| c + TimeDelta::minutes(RESET_EXPIRE_MINUTES) >= now)
                && password::verify(token, &hash)
        });
    if !valid {
        return Ok(one_error("email", PASSWORD_TOKEN_INVALID));
    }
    let password_errors =
        password_errors_for("password", pass, str_of(&body, "password_confirmation"));
    if !password_errors.is_empty() {
        let mut errors = Map::new();
        errors.insert("password".into(), json!(password_errors));
        return Ok(invalid(errors));
    }
    let hash = password::hash(pass).ok_or(ApiError::Internal)?;
    db::reset_password(&state.db, user.id, &hash, &random_text(60), now).await?;
    state.sessions.remove_user(user.id);
    session::audit_guest(
        &state,
        None,
        (user.id, &user.name, &user.email),
        &meta,
        "auth.user.password_reset",
    )
    .await;
    db::delete_reset_token(&state.db, &user.email).await?;
    let (guest, cookie) = session::guest_id(&state, &headers);
    state
        .sessions
        .guest(&guest, |g| g.status = Some(PASSWORD_RESET_DONE.to_string()));
    Ok(session::with_guest_cookie(
        ok(json!({"message": PASSWORD_RESET_DONE})).into_response(),
        cookie,
    ))
}

// ---- Forced password change ----

/// `ForcePasswordReset::submit`. The password change ends every session of the user, so the
/// redirect to the dashboard arrives at the login page.
async fn force_password_reset(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Value>,
) -> ApiResult {
    let Some(user) = users::get(&state.db, session.user_id).await? else {
        return Err(ApiError::Unauthenticated);
    };
    if !user.force_password_reset {
        return Ok(ok(json!({"message": "ok", "signed_out": false})).into_response());
    }
    // `$this->rateLimit(10)`.
    if let Some(secs) = throttle(
        &format!("force-password-reset:{}", user.id),
        10,
        Duration::from_secs(60),
    ) {
        return Ok(toast_error(
            StatusCode::TOO_MANY_REQUESTS,
            format!("Too many requests. Please try again in {secs} seconds."),
        ));
    }
    let email = str_of(&body, "email");
    let mut messages: Vec<String> = Vec::new();
    let mut fields = Map::new();
    if email.is_empty() {
        messages.push("The email field is required.".to_string());
        fields.insert("email".into(), json!(["The email field is required."]));
    } else if !is_email(email) {
        messages.push("The email field must be a valid email address.".to_string());
        fields.insert(
            "email".into(),
            json!(["The email field must be a valid email address."]),
        );
    }
    let pass = str_of(&body, "password");
    let password_errors =
        password_errors_for("password", pass, str_of(&body, "password_confirmation"));
    if !password_errors.is_empty() {
        fields.insert("password".into(), json!(password_errors));
    }
    messages.extend(password_errors);
    if let Some(m) = summarize(&messages) {
        return Ok(toast_with_errors(m, fields));
    }
    let actor = crate::audit::actor(&state, &ctx, &meta).await;
    let hash = password::hash(pass).ok_or(ApiError::Internal)?;
    db::finish_forced_reset(&state.db, user.id, &hash, state.now()).await?;
    let team = (session.team_id != NO_TEAM).then_some(session.team_id);
    crate::audit::log_as(
        &state,
        team,
        &actor,
        "ui.user.password_changed",
        json!({
            "team_id": team,
            "resource": "user",
            "user_name": user.name,
            "reason": "forced_reset",
        }),
    )
    .await;
    state.sessions.remove_user(user.id);
    Ok(ok(json!({"message": "ok", "signed_out": true})).into_response())
}

/// `Boarding\Index::skipBoarding`.
async fn skip_boarding(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
) -> ApiResult {
    if session.team_id != NO_TEAM {
        db::finish_boarding(&state.db, session.team_id, state.now()).await?;
    }
    Ok(ok(json!({"message": "ok"})).into_response())
}

// ---- Email verification ----

/// `URL::temporarySignedRoute('verify.verify', ...)` on the instance URL.
fn signed_verify_url(
    encrypter: &coolify_compat::Encrypter,
    base: &str,
    id: i64,
    email: &str,
    expires: i64,
) -> String {
    let hash = sha256_hex(email);
    let url = format!(
        "{}/email/verify/{id}/{hash}?expires={expires}",
        base.trim_end_matches('/')
    );
    let signature = encrypter.hmac_with_app_key(url.as_bytes());
    format!("{url}&signature={signature}")
}

fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `VerifyEmail::again` and `User::sendVerificationEmail`.
async fn resend_verification(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
) -> ApiResult {
    if let Some(secs) = throttle(
        &format!("verify-email:{}", session.user_id),
        1,
        Duration::from_secs(300),
    ) {
        return Ok(toast_error(
            StatusCode::TOO_MANY_REQUESTS,
            format!("Too many requests. Please try again in {secs} seconds."),
        ));
    }
    let Some(user) = users::get(&state.db, session.user_id).await? else {
        return Err(ApiError::Unauthenticated);
    };
    let encrypter = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let base = Ctx::load(&state.db).await.base_url;
    let expires = (state.now() + TimeDelta::minutes(VERIFICATION_EXPIRE_MINUTES))
        .and_utc()
        .timestamp();
    let url = signed_verify_url(encrypter, &base, user.id, &user.email, expires);
    if let Err(e) = notifications::send_transactional(
        &state.db,
        state.encrypter.as_ref(),
        &user.email,
        Mail::EmailVerification { url },
    )
    .await
    {
        return Ok(toast_error(StatusCode::INTERNAL_SERVER_ERROR, e));
    }
    Ok(ok(json!({"message": "Email verification link sent!"})).into_response())
}

/// The URL of the request as `$request->url()` sees it.
fn request_root(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    format!("{scheme}://{host}")
}

/// `$request->hasValidSignature()`.
fn has_valid_signature(
    encrypter: &coolify_compat::Encrypter,
    root: &str,
    path: &str,
    query: &str,
    now: i64,
) -> bool {
    let mut signature = None;
    let mut expires = None;
    let mut kept = Vec::new();
    for part in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        if key == "signature" {
            signature = Some(value);
            continue;
        }
        if key == "expires" {
            expires = value.parse::<i64>().ok();
        }
        kept.push(part);
    }
    let Some(signature) = signature else {
        return false;
    };
    let joined = kept.join("&");
    let original = if joined.is_empty() {
        format!("{root}{path}")
    } else {
        format!("{root}{path}?{joined}")
    };
    let expected = encrypter.hmac_with_app_key(original.as_bytes());
    use subtle::ConstantTimeEq;
    if !bool::from(expected.as_bytes().ct_eq(signature.as_bytes())) {
        return false;
    }
    expires.is_none_or(|e| now <= e)
}

/// `Controller::email_verify`, for the page at `/email/verify/{id}/{hash}`.
async fn email_verify(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    meta: Meta,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    const FORBIDDEN: ApiError = ApiError::Message(StatusCode::FORBIDDEN, "Forbidden");
    let path = str_of(&body, "path");
    let query = str_of(&body, "query").trim_start_matches('?');
    let encrypter = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let now = state.now().and_utc().timestamp();
    if !has_valid_signature(encrypter, &request_root(&headers), path, query, now) {
        return Err(FORBIDDEN);
    }
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    let ["email", "verify", id, hash] = parts.as_slice() else {
        return Err(FORBIDDEN);
    };
    let Some(user) = users::get(&state.db, session.user_id).await? else {
        return Err(FORBIDDEN);
    };
    if *id != user.id.to_string() || *hash != sha256_hex(&user.email) {
        return Err(FORBIDDEN);
    }
    if db::mark_email_verified(&state.db, user.id, state.now()).await? {
        session::audit_auth(
            &state,
            (session.team_id != NO_TEAM).then_some(session.team_id),
            (user.id, &user.name, &user.email),
            &meta,
            "auth.user.email_verified",
        )
        .await;
    }
    Ok(ok(json!({"message": "ok"})).into_response())
}

// ---- Two factor required ----

/// `TwoFactorRequired::mount` and `render`, with the flash of the Fortify routes.
async fn two_factor_required(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
) -> ApiResult {
    let Some(user) = profile::get(&state.db, session.user_id).await? else {
        return Err(ApiError::Unauthenticated);
    };
    let flash: Flash = state.sessions.take_flash(&session.id);
    let status = flash.status.as_deref();
    let confirmed = user.two_factor_confirmed_at.is_some();
    if confirmed && status != Some("two-factor-authentication-confirmed") {
        return Ok(ok(json!({"redirect": "/"})).into_response());
    }
    let team_name = users::memberships(&state.db, user.id)
        .await?
        .into_iter()
        .find(|m| m.team_id == session.team_id)
        .map(|m| m.team_name);
    let mut out = json!({
        "team_name": team_name,
        "confirmed": confirmed,
        "code_error": flash.errors.then_some("The provided two factor authentication code was invalid."),
    });
    if confirmed {
        out["recovery_codes"] = decrypted(&state, user.two_factor_recovery_codes.as_deref())
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or(Value::Null);
    } else if let Some(secret) = decrypted(&state, user.two_factor_secret.as_deref()) {
        let url = totp::otpauth_url(&app_name(), &user.email, &secret);
        out["qr_svg"] = json!(totp::qr_svg(&url));
        out["secret"] = json!(secret);
        out["url"] = json!(url);
    }
    Ok(ok(out).into_response())
}

// ---- Passkeys ----

const NOT_FOUND: ApiError = ApiError::Message(StatusCode::NOT_FOUND, "Not Found");
const NOT_RECOGNIZED: &str = "Passkey not recognized. It may have been removed from your account.";
const SESSION_EXPIRED: &str = "Passkey verification session expired. Please try again.";

/// `throttle:passkeys`.
fn passkeys_throttle(meta: &Meta) -> Result<(), ApiError> {
    match throttle(
        &format!("passkeys:{}", meta.ip.as_deref().unwrap_or_default()),
        30,
        Duration::from_secs(60),
    ) {
        Some(s) => Err(ApiError::Throttled(s)),
        None => Ok(()),
    }
}

/// `GenerateVerificationOptions`.
fn verification_options(rp_id: &str, allow: Vec<Value>) -> (String, Value) {
    let mut challenge = [0u8; 32];
    rand::thread_rng().fill(&mut challenge);
    let challenge = webauthn::b64(&challenge);
    let options = json!({
        "challenge": challenge,
        "rpId": rp_id,
        "userVerification": "required",
        "allowCredentials": allow,
        "timeout": 60000,
    });
    (challenge, options)
}

/// `PasskeyLoginController::index`.
async fn passkey_login_options(
    State(state): State<AppState>,
    meta: Meta,
    headers: HeaderMap,
) -> ApiResult {
    let (_, rp) = passkey_settings(&state, &headers).await?;
    let (rp_id, _) = rp.ok_or(NOT_FOUND)?;
    passkeys_throttle(&meta)?;
    let (challenge, options) = verification_options(&rp_id, Vec::new());
    let (guest, cookie) = session::guest_id(&state, &headers);
    state
        .sessions
        .guest(&guest, |g| g.verification_options = Some(challenge));
    Ok(session::with_guest_cookie(
        ok(json!({"options": options})).into_response(),
        cookie,
    ))
}

/// `PasskeyVerificationRequest::rules`: the credential's id as it is stored.
fn credential_id(body: &Value) -> Result<String, Box<Response>> {
    let credential = body.get("credential");
    let field = |k: &str| {
        credential
            .and_then(|c| c.get(k))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    if credential.is_none_or(|c| !c.is_object()) {
        return Err(Box::new(one_error(
            "credential",
            "The credential field is required.",
        )));
    }
    let raw = match (field("id"), field("rawId"), field("type")) {
        (Some(_), Some(raw), Some("public-key")) => raw,
        _ => {
            return Err(Box::new(one_error(
                "credential",
                "Invalid credential format.",
            )));
        }
    };
    if credential.and_then(|c| c.get("response")).is_none() {
        return Err(Box::new(one_error(
            "credential",
            "Invalid credential format.",
        )));
    }
    webauthn::from_b64(raw)
        .map(|b| webauthn::b64(&b))
        .ok_or_else(|| Box::new(one_error("credential", "Invalid credential format.")))
}

/// `VerifyPasskey::__invoke`: checks the assertion and stores what changed. The owner, or the
/// response for the page.
async fn verify_passkey(
    state: &AppState,
    id: &str,
    credential: &Value,
    challenge: &str,
    rp: &(String, Vec<String>),
    owner: Option<i64>,
) -> Result<i64, Response> {
    let row = db::passkey_by_credential(&state.db, id)
        .await
        .map_err(|e| ApiError::from(e).into_response())?;
    let Some((passkey_id, user_id, stored)) = row else {
        return Err(one_error("credential", NOT_RECOGNIZED));
    };
    if owner.is_some_and(|o| o != user_id) {
        return Err(one_error("credential", NOT_RECOGNIZED));
    }
    let stored: Value =
        serde_json::from_str(&stored).map_err(|_| ApiError::Internal.into_response())?;
    let updated = webauthn::verify_assertion(
        credential,
        &stored,
        &webauthn::AssertionExpected {
            challenge,
            rp_id: &rp.0,
            origins: &rp.1,
        },
    )
    .map_err(|m| one_error("credential", &m))?;
    db::update_passkey_use(&state.db, passkey_id, &updated.to_string(), state.now())
        .await
        .map_err(|e| ApiError::from(e).into_response())?;
    Ok(user_id)
}

/// `PasskeyLoginController::store`. A passkey sign-in skips the two-factor challenge.
async fn passkey_login(
    State(state): State<AppState>,
    meta: Meta,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    let (_, rp) = passkey_settings(&state, &headers).await?;
    let rp = rp.ok_or(NOT_FOUND)?;
    passkeys_throttle(&meta)?;
    let id = match credential_id(&body) {
        Ok(id) => id,
        Err(r) => return Ok(*r),
    };
    let (guest, _) = session::guest_id(&state, &headers);
    let challenge = state
        .sessions
        .guest(&guest, |g| g.verification_options.take())
        .flatten();
    let Some(challenge) = challenge else {
        return Ok(one_error("credential", SESSION_EXPIRED));
    };
    let credential = body.get("credential").cloned().unwrap_or(Value::Null);
    let user_id = match verify_passkey(&state, &id, &credential, &challenge, &rp, None).await {
        Ok(u) => u,
        Err(r) => return Ok(r),
    };
    let Some(user) = users::get(&state.db, user_id).await? else {
        return Ok(one_error("credential", NOT_RECOGNIZED));
    };
    // `PasskeyVerified` and `Login` come before anything put a team in the session.
    session::audit_guest(
        &state,
        None,
        (user.id, &user.name, &user.email),
        &meta,
        "auth.user.passkey_verified",
    )
    .await;
    let (cookie, mut info) = session::establish(&state, user_id, |_| None, &meta).await?;
    info["redirect"] = json!("/");
    Ok(session::signed_in(cookie, info))
}

/// `can_confirm_with_passkey` of `Fortify::confirmPasswordView`.
async fn confirm_state(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    headers: HeaderMap,
) -> ApiResult {
    let (_, rp) = passkey_settings(&state, &headers).await?;
    let has = !profile::passkeys(&state.db, session.user_id)
        .await?
        .is_empty();
    Ok(ok(json!({"can_confirm_with_passkey": rp.is_some() && has})).into_response())
}

/// `PasskeyConfirmationController::index`.
async fn passkey_confirm_options(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    meta: Meta,
    headers: HeaderMap,
) -> ApiResult {
    let (_, rp) = passkey_settings(&state, &headers).await?;
    let (rp_id, _) = rp.ok_or(NOT_FOUND)?;
    passkeys_throttle(&meta)?;
    let allow: Vec<Value> = profile::passkeys(&state.db, session.user_id)
        .await?
        .iter()
        .map(|p| json!({"type": "public-key", "id": p.credential_id}))
        .collect();
    let (challenge, options) = verification_options(&rp_id, allow);
    state
        .sessions
        .set_passkey_challenge(&session.id, json!({"challenge": challenge}).to_string());
    Ok(ok(json!({"options": options})).into_response())
}

/// `PasskeyConfirmationController::store`.
async fn passkey_confirm(
    State(state): State<AppState>,
    Extension(session): Extension<SessionUser>,
    meta: Meta,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    let (_, rp) = passkey_settings(&state, &headers).await?;
    let rp = rp.ok_or(NOT_FOUND)?;
    passkeys_throttle(&meta)?;
    let id = match credential_id(&body) {
        Ok(id) => id,
        Err(r) => return Ok(*r),
    };
    let stored: Option<Value> = state
        .sessions
        .take_passkey_challenge(&session.id)
        .and_then(|s| serde_json::from_str(&s).ok());
    let Some(stored) = stored else {
        return Ok(one_error("credential", SESSION_EXPIRED));
    };
    let credential = body.get("credential").cloned().unwrap_or(Value::Null);
    let verified = verify_passkey(
        &state,
        &id,
        &credential,
        str_of(&stored, "challenge"),
        &rp,
        Some(session.user_id),
    )
    .await;
    if let Err(r) = verified {
        return Ok(r);
    }
    let Some(user) = users::get(&state.db, session.user_id).await? else {
        return Err(ApiError::Unauthenticated);
    };
    session::audit_auth(
        &state,
        (session.team_id != NO_TEAM).then_some(session.team_id),
        (user.id, &user.name, &user.email),
        &meta,
        "auth.user.passkey_verified",
    )
    .await;
    state.sessions.mark_password_confirmed(&session.id);
    Ok(ok(json!({"message": "ok"})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encrypter() -> coolify_compat::Encrypter {
        coolify_compat::Encrypter::new("base64:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .unwrap()
    }

    #[test]
    fn signed_urls() {
        let e = encrypter();
        let url = signed_verify_url(&e, "http://localhost:8000/", 5, "a@b.co", 1000);
        let rest = url.strip_prefix("http://localhost:8000").unwrap();
        let (path, query) = rest.split_once('?').unwrap();
        assert!(path.starts_with("/email/verify/5/"));
        assert!(has_valid_signature(
            &e,
            "http://localhost:8000",
            path,
            query,
            999
        ));
        assert!(has_valid_signature(
            &e,
            "http://localhost:8000",
            path,
            query,
            1000
        ));
        assert!(!has_valid_signature(
            &e,
            "http://localhost:8000",
            path,
            query,
            1001
        ));
        assert!(!has_valid_signature(
            &e,
            "http://other:8000",
            path,
            query,
            999
        ));
        assert!(!has_valid_signature(
            &e,
            "http://localhost:8000",
            path,
            &query.replace("1000", "2000"),
            999
        ));
        assert!(!has_valid_signature(
            &e,
            "http://localhost:8000",
            path,
            "expires=1000",
            999
        ));
    }

    #[test]
    fn registration_policy() {
        let settings = |a: bool, b: bool| {
            let mut m = Map::new();
            m.insert("is_registration_enabled".into(), json!(a));
            m.insert("disable_registration_when_oauth_enabled".into(), json!(b));
            m
        };
        assert!(password_registration_allowed(&settings(true, false), &[]));
        assert!(!password_registration_allowed(&settings(false, false), &[]));
        assert!(password_registration_allowed(&settings(true, true), &[]));
    }
}
