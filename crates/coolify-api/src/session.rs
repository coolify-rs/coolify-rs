//! Browser sessions for the web UI.
//!
//! Login reproduces Fortify's checks (lowercased email, bcrypt `Hash::check`,
//! TOTP challenge for confirmed 2FA, 5 attempts per minute per email+IP and
//! per email). A session gives the UI access to the main API group through
//! `/ui/api/v1/...` with the abilities upstream's `UseSessionTeamApiToken`
//! grants: admins/owners `read, write, deploy, read:sensitive`, members `read`.
//!
//! Sessions are kept in memory (lost on restart; see KNOWN_LIMITATIONS).
//! Cookies are `HttpOnly; SameSite=Lax`; state-changing requests also need
//! the per-session `X-CSRF-Token` header.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use coolify_compat::{password, totp};
use coolify_db::users::{self, LoginUser, Membership};
use coolify_domain::{Abilities, Ability, Role, TeamId};
use rand::RngCore;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::auth::AuthCtx;
use crate::response::{ApiError, PhpJson};
use crate::state::AppState;

pub const COOKIE: &str = "coolify_rs_session";
const MAX_SESSIONS: usize = 10_000;
const ABSOLUTE_LIFETIME: Duration = Duration::from_secs(30 * 24 * 3600);
const LOGIN_LIMIT: usize = 5;
const LOGIN_WINDOW: Duration = Duration::from_secs(60);

struct Session {
    user_id: i64,
    team_id: TeamId,
    csrf: String,
    /// sha256 of the password hash at login: a password change ends the session.
    password_fp: [u8; 32],
    created: Instant,
    last_seen: Instant,
    /// `auth.password_confirmed_at`.
    password_confirmed: Option<Instant>,
    /// What upstream flashes with `session('status')` and `session('errors')`; read once.
    flash: Option<Flash>,
    /// `passkey.registration_options`: the challenge and handle of a passkey registration.
    passkey_challenge: Option<String>,
}

/// A value upstream keeps in the session for the next page.
#[derive(Debug, Clone, Default)]
pub struct Flash {
    pub status: Option<String>,
    pub errors: bool,
}

/// `config('auth.password_timeout')`.
const PASSWORD_TIMEOUT: Duration = Duration::from_secs(10_800);

/// A session whose user has several teams and no stored choice (`/select-team`).
pub const NO_TEAM: TeamId = -1;

/// In-memory session store, login throttle and TOTP replay guard.
pub struct SessionStore {
    sessions: Mutex<HashMap<[u8; 32], Session>>,
    attempts: Mutex<HashMap<String, Vec<Instant>>>,
    totp_steps: Mutex<HashMap<i64, u64>>,
    idle: Duration,
}

impl std::fmt::Debug for SessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionStore").finish_non_exhaustive()
    }
}

fn digest(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}

fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl SessionStore {
    /// `idle` mirrors upstream `SESSION_LIFETIME` (default 7 days).
    pub fn new(idle: Duration) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            attempts: Mutex::new(HashMap::new()),
            totp_steps: Mutex::new(HashMap::new()),
            idle,
        }
    }

    fn create(&self, user_id: i64, team_id: TeamId, password_hash: &str) -> (String, String) {
        let id = random_hex(32);
        let csrf = random_hex(32);
        let now = Instant::now();
        if let Ok(mut m) = self.sessions.lock() {
            let idle = self.idle;
            m.retain(|_, s| {
                now.duration_since(s.last_seen) < idle
                    && now.duration_since(s.created) < ABSOLUTE_LIFETIME
            });
            if m.len() >= MAX_SESSIONS
                && let Some(oldest) = m.iter().min_by_key(|(_, s)| s.last_seen).map(|(k, _)| *k)
            {
                m.remove(&oldest);
            }
            m.insert(
                digest(&id),
                Session {
                    user_id,
                    team_id,
                    csrf: csrf.clone(),
                    password_fp: digest(password_hash),
                    created: now,
                    last_seen: now,
                    password_confirmed: None,
                    flash: None,
                    passkey_challenge: None,
                },
            );
        }
        (id, csrf)
    }

    /// `session()->passwordConfirmed()`.
    pub(crate) fn mark_password_confirmed(&self, id: &str) {
        if let Ok(mut m) = self.sessions.lock()
            && let Some(s) = m.get_mut(&digest(id))
        {
            s.password_confirmed = Some(Instant::now());
        }
    }

    /// `hasRecentPasswordConfirmation()`.
    pub(crate) fn password_recently_confirmed(&self, id: &str) -> bool {
        self.sessions
            .lock()
            .ok()
            .and_then(|m| m.get(&digest(id)).and_then(|s| s.password_confirmed))
            .is_some_and(|t| t.elapsed() < PASSWORD_TIMEOUT)
    }

    pub(crate) fn set_flash(&self, id: &str, flash: Flash) {
        if let Ok(mut m) = self.sessions.lock()
            && let Some(s) = m.get_mut(&digest(id))
        {
            s.flash = Some(flash);
        }
    }

    pub(crate) fn take_flash(&self, id: &str) -> Flash {
        self.sessions
            .lock()
            .ok()
            .and_then(|mut m| m.get_mut(&digest(id)).and_then(|s| s.flash.take()))
            .unwrap_or_default()
    }

    pub(crate) fn set_passkey_challenge(&self, id: &str, challenge: String) {
        if let Ok(mut m) = self.sessions.lock()
            && let Some(s) = m.get_mut(&digest(id))
        {
            s.passkey_challenge = Some(challenge);
        }
    }

    pub(crate) fn passkey_challenge(&self, id: &str) -> Option<String> {
        self.sessions
            .lock()
            .ok()
            .and_then(|m| m.get(&digest(id)).and_then(|s| s.passkey_challenge.clone()))
    }

    /// Ends every session of a user (a password change is noticed on the next request anyway;
    /// `deleteAllSessions` also drops the ones of a deleted user).
    pub(crate) fn remove_user(&self, user_id: i64) {
        if let Ok(mut m) = self.sessions.lock() {
            m.retain(|_, s| s.user_id != user_id);
        }
    }

    /// (user, team, csrf, password fingerprint) for a live session; touches it.
    fn get(&self, id: &str) -> Option<(i64, TeamId, String, [u8; 32])> {
        let now = Instant::now();
        let mut m = self.sessions.lock().ok()?;
        let key = digest(id);
        let expired = m.get(&key).map(|s| {
            now.duration_since(s.last_seen) >= self.idle
                || now.duration_since(s.created) >= ABSOLUTE_LIFETIME
        })?;
        if expired {
            m.remove(&key);
            return None;
        }
        let s = m.get_mut(&key)?;
        s.last_seen = now;
        Some((s.user_id, s.team_id, s.csrf.clone(), s.password_fp))
    }

    fn set_team(&self, id: &str, team_id: TeamId) {
        if let Ok(mut m) = self.sessions.lock()
            && let Some(s) = m.get_mut(&digest(id))
        {
            s.team_id = team_id;
        }
    }

    fn remove(&self, id: &str) {
        if let Ok(mut m) = self.sessions.lock() {
            m.remove(&digest(id));
        }
    }

    /// Seconds until the key may try again, if throttled.
    fn throttled(&self, keys: &[String]) -> Option<u64> {
        let now = Instant::now();
        let mut m = self.attempts.lock().ok()?;
        keys.iter()
            .filter_map(|k| {
                let v = m.entry(k.clone()).or_default();
                v.retain(|t| now.duration_since(*t) < LOGIN_WINDOW);
                (v.len() >= LOGIN_LIMIT).then(|| {
                    let oldest = v.iter().min().copied().unwrap_or(now);
                    (LOGIN_WINDOW.saturating_sub(now.duration_since(oldest)))
                        .as_secs()
                        .max(1)
                })
            })
            .max()
    }

    fn record_failure(&self, keys: &[String]) {
        if let Ok(mut m) = self.attempts.lock() {
            for k in keys {
                m.entry(k.clone()).or_default().push(Instant::now());
            }
        }
    }

    fn clear_failures(&self, keys: &[String]) {
        if let Ok(mut m) = self.attempts.lock() {
            for k in keys {
                m.remove(k);
            }
        }
    }
}

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|p| p.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v.to_string())
        .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn set_cookie(state: &AppState, value: &str, max_age: Option<u64>) -> HeaderValue {
    let secure = if state.secure_cookies { "; Secure" } else { "" };
    let age = max_age
        .map(|a| format!("; Max-Age={a}"))
        .unwrap_or_default();
    HeaderValue::from_str(&format!(
        "{COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax{secure}{age}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn needs_csrf(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn csrf_ok(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|got| bool::from(got.as_bytes().ct_eq(expected.as_bytes())))
}

/// UI abilities derived from the team role (`UseSessionTeamApiToken`).
fn abilities_for(role: Role) -> Abilities {
    let list: &[Ability] = if role.is_admin() {
        &[
            Ability::Read,
            Ability::Write,
            Ability::Deploy,
            Ability::ReadSensitive,
        ]
    } else {
        &[Ability::Read]
    };
    Abilities::new(list.iter().map(|a| a.as_str().to_string()).collect())
}

struct Live {
    id: String,
    user: LoginUser,
    memberships: Vec<Membership>,
    team_id: TeamId,
    csrf: String,
}

/// Loads and validates the session; ends it if the user or password changed.
async fn live_session(state: &AppState, headers: &HeaderMap) -> Result<Live, ApiError> {
    let id = session_cookie(headers).ok_or(ApiError::Unauthenticated)?;
    let (user_id, team_id, csrf, password_fp) =
        state.sessions.get(&id).ok_or(ApiError::Unauthenticated)?;
    let user = users::get(&state.db, user_id).await?;
    let valid = user
        .as_ref()
        .and_then(|u| u.password.as_deref())
        .is_some_and(|p| bool::from(digest(p).ct_eq(&password_fp)));
    let Some(user) = user.filter(|_| valid) else {
        state.sessions.remove(&id);
        return Err(ApiError::Unauthenticated);
    };
    let mut memberships = users::memberships(&state.db, user.id).await?;
    // `DecideWhatToDoWithUser`: a user without teams gets a personal team again, and a session
    // whose team is gone restores the stored or sole team, or waits on the selection screen.
    if memberships.is_empty() && state.db.schema().read_only_reason.is_none() {
        let team = coolify_db::team_pages::recreate_personal_team(
            &state.db,
            user.id,
            &user.name,
            state.now(),
        )
        .await?;
        coolify_db::notification_settings::create_defaults(&state.db, team).await?;
        memberships = users::memberships(&state.db, user.id).await?;
    }
    let mut team_id = team_id;
    if !memberships.iter().any(|m| m.team_id == team_id) {
        team_id = resolve_stored_team(&user, &memberships).unwrap_or(NO_TEAM);
        state.sessions.set_team(&id, team_id);
        if team_id != NO_TEAM
            && user.current_team_id != Some(team_id)
            && let Err(e) = users::set_current_team(&state.db, user.id, team_id).await
        {
            tracing::warn!(error = %e, "could not persist current team");
        }
    }
    Ok(Live {
        id,
        user,
        memberships,
        team_id,
        csrf,
    })
}

/// The session behind a request, for pages upstream serves in any team state (`/profile`,
/// invitations): the guard's checks without the team requirement. `team_id` may be [`NO_TEAM`].
#[derive(Debug, Clone)]
pub struct SessionUser {
    pub id: String,
    pub user_id: i64,
    pub team_id: TeamId,
    pub ctx: AuthCtx,
}

/// Guard for pages that need a signed-in user but no usable team.
pub async fn user_guard(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let headers = req.headers().clone();
    let method = req.method().clone();
    let result = async {
        let live = live_session(&state, &headers).await?;
        if needs_csrf(&method) && !csrf_ok(&headers, &live.csrf) {
            return Err(ApiError::Message(
                StatusCode::from_u16(419).unwrap_or(StatusCode::FORBIDDEN),
                "CSRF token mismatch.",
            ));
        }
        let role = live
            .memberships
            .iter()
            .find(|m| m.team_id == live.team_id)
            .map_or(Role::Member, |m| m.role);
        Ok(SessionUser {
            id: live.id,
            user_id: live.user.id,
            team_id: live.team_id,
            ctx: AuthCtx {
                token_id: 0,
                user_id: live.user.id,
                team_id: live.team_id,
                team_id_raw: live.team_id.to_string(),
                role,
                can_read_sensitive: role.is_admin(),
                abilities: abilities_for(role),
            },
        })
    }
    .await;
    match result {
        Ok(user) => {
            req.extensions_mut().insert(user.ctx.clone());
            req.extensions_mut().insert(user);
            next.run(req).await
        }
        Err(e) => e.into_response(),
    }
}

/// `refreshSession($team)`: the session's team and `users.current_team_id`.
pub(crate) async fn activate_team(
    state: &AppState,
    session_id: &str,
    user_id: i64,
    team_id: TeamId,
) -> Result<(), ApiError> {
    users::set_current_team(&state.db, user_id, team_id).await?;
    state.sessions.set_team(session_id, team_id);
    Ok(())
}

/// A session for `Auth::login($user)` outside the login form (invitation magic link).
pub(crate) fn start_session(
    state: &AppState,
    user_id: i64,
    team_id: TeamId,
    password_hash: &str,
) -> (HeaderValue, String) {
    let (id, csrf) = state.sessions.create(user_id, team_id, password_hash);
    (set_cookie(state, &id, None), csrf)
}

/// The session id of a request, for handlers behind a guard.
pub(crate) fn session_id(headers: &HeaderMap) -> Option<String> {
    session_cookie(headers)
}

const TEAM_2FA_REQUIRED: &str = "Two-factor authentication is required by this team.";

/// Guard for `/ui/api/v1/...`: session → CSRF → membership → team 2FA policy.
pub async fn ui_guard(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let headers = req.headers().clone();
    let method = req.method().clone();
    let result = async {
        let live = live_session(&state, &headers).await?;
        if needs_csrf(&method) && !csrf_ok(&headers, &live.csrf) {
            return Err(ApiError::Message(
                StatusCode::from_u16(419).unwrap_or(StatusCode::FORBIDDEN),
                "CSRF token mismatch.",
            ));
        }
        let m = live
            .memberships
            .iter()
            .find(|m| m.team_id == live.team_id)
            .ok_or(ApiError::Message(
                StatusCode::FORBIDDEN,
                "You are no longer a member of this team.",
            ))?;
        // Upstream `RequireTeamTwoFactor` (web group, since 2026-10-25).
        if m.two_factor_required && live.user.two_factor_confirmed_at.is_none() {
            return Err(ApiError::Message(StatusCode::FORBIDDEN, TEAM_2FA_REQUIRED));
        }
        let abilities = abilities_for(m.role);
        Ok(AuthCtx {
            token_id: 0,
            user_id: live.user.id,
            team_id: m.team_id,
            team_id_raw: m.team_id.to_string(),
            role: m.role,
            can_read_sensitive: m.role.is_admin(),
            abilities,
        })
    }
    .await;
    match result {
        Ok(ctx) => {
            req.extensions_mut().insert(ctx);
            next.run(req).await
        }
        Err(e) => e.into_response(),
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ui/session", get(current).post(login).delete(logout))
        .route("/ui/session/team", post(switch_team))
}

fn session_json(
    state: &AppState,
    user: &LoginUser,
    memberships: &[Membership],
    team_id: TeamId,
    csrf: &str,
) -> Value {
    let current = memberships.iter().find(|m| m.team_id == team_id);
    json!({
        "user": {
            "id": user.id,
            "name": user.name,
            "email": user.email,
            "two_factor_enabled": user.has_two_factor(),
            "avatar_url": user.avatar_url(),
        },
        "teams": memberships.iter().map(|m| json!({"id": m.team_id, "name": m.team_name, "role": m.role.as_str(), "avatar_url": m.avatar_url()})).collect::<Vec<_>>(),
        "current_team": current.map(|m| json!({
            "id": m.team_id,
            "name": m.team_name,
            "role": m.role.as_str(),
            "avatar_url": m.avatar_url(),
            "two_factor_required": m.two_factor_required,
            "two_factor_blocked": m.two_factor_required && user.two_factor_confirmed_at.is_none(),
        })),
        "csrf_token": csrf,
        "version": state.version,
        "features": {
            "private_keys": state.encrypter.is_some(),
            "read_only": state.db.schema().read_only_reason.is_some(),
        },
    })
}

pub async fn current(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let live = live_session(&state, &headers).await?;
    Ok(PhpJson(
        StatusCode::OK,
        session_json(
            &state,
            &live.user,
            &live.memberships,
            live.team_id,
            &live.csrf,
        ),
    )
    .into_response())
}

fn failed_login() -> ApiError {
    let mut errors = serde_json::Map::new();
    errors.insert(
        "email".into(),
        json!(["These credentials do not match our records."]),
    );
    ApiError::ValidationWithMessage("These credentials do not match our records.", errors)
}

/// `User::resolveStoredTeam`: the stored team if still a member, the sole team, or none when
/// the choice is ambiguous and the user has to pick one on `/select-team`.
pub(crate) fn resolve_stored_team(user: &LoginUser, memberships: &[Membership]) -> Option<TeamId> {
    user.current_team_id
        .filter(|t| memberships.iter().any(|m| m.team_id == *t))
        .or(match memberships {
            [only] => Some(only.team_id),
            _ => None,
        })
}

pub async fn login(State(state): State<AppState>, req: Request) -> Result<Response, ApiError> {
    let ip = crate::auth::client_ip(&state, &req).to_string();
    let headers = req.headers().clone();
    let body = axum::body::to_bytes(req.into_body(), 64 * 1024)
        .await
        .map_err(|_| ApiError::InvalidRequest("Invalid JSON."))?;
    // JSON only: a cross-site form cannot send it without a CORS preflight.
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.contains("application/json"));
    if !is_json {
        return Err(ApiError::InvalidRequest(
            "Content-Type must be application/json.",
        ));
    }
    let input: Value =
        serde_json::from_slice(&body).map_err(|_| ApiError::InvalidRequest("Invalid JSON."))?;
    let email = input
        .get("email")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let pass = input.get("password").and_then(Value::as_str).unwrap_or("");
    let code = input.get("code").and_then(Value::as_str);
    if email.is_empty() || pass.is_empty() {
        let mut errors = serde_json::Map::new();
        if email.is_empty() {
            errors.insert("email".into(), json!(["The email field is required."]));
        }
        if pass.is_empty() {
            errors.insert(
                "password".into(),
                json!(["The password field is required."]),
            );
        }
        return Err(ApiError::Validation(errors));
    }
    let keys = vec![
        format!("{email}|{ip}"),
        format!("email:{}", hex_digest(&email)),
    ];
    if let Some(secs) = state.sessions.throttled(&keys) {
        return Err(ApiError::Throttled(secs));
    }

    let user = users::find_by_email(&state.db, &email).await?;
    let ok = match user
        .as_ref()
        .and_then(|u| u.password.as_deref().map(|p| (u, p)))
    {
        Some((_, hash)) => password::verify(pass, hash),
        None => password::verify_dummy(pass),
    };
    let Some(user) = user.filter(|_| ok) else {
        state.sessions.record_failure(&keys);
        return Err(failed_login());
    };

    if user.has_two_factor() {
        let Some(code) = code.filter(|c| !c.trim().is_empty()) else {
            return Ok(PhpJson(StatusCode::OK, json!({"two_factor": true})).into_response());
        };
        let encrypter = state.encrypter.as_ref().ok_or(ApiError::Message(
            StatusCode::SERVICE_UNAVAILABLE,
            "APP_KEY is not configured; two-factor accounts cannot sign in.",
        ))?;
        let secret = user
            .two_factor_secret
            .as_deref()
            .and_then(|s| encrypter.decrypt_maybe_serialized(s).ok())
            .ok_or(ApiError::Internal)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let last = state
            .sessions
            .totp_steps
            .lock()
            .ok()
            .and_then(|m| m.get(&user.id).copied());
        match totp::verify(&secret, code, now, 1, last) {
            Some(step) => {
                if let Ok(mut m) = state.sessions.totp_steps.lock() {
                    m.insert(user.id, step);
                }
            }
            None => {
                state.sessions.record_failure(&keys);
                let mut errors = serde_json::Map::new();
                errors.insert(
                    "code".into(),
                    json!(["The provided two factor authentication code was invalid."]),
                );
                return Err(ApiError::ValidationWithMessage(
                    "The provided two factor authentication code was invalid.",
                    errors,
                ));
            }
        }
    }
    if user.force_password_reset {
        return Err(ApiError::Message(
            StatusCode::FORBIDDEN,
            "A password reset is required for this account. Set a new password in Coolify first.",
        ));
    }
    // `Fortify::authenticateUsing`: touches the user, restores the last team, and recreates the
    // personal team of a user without teams. Without a clear choice the session has no team yet.
    if let Err(e) = users::touch(&state.db, user.id, state.now()).await {
        tracing::warn!(error = %e, "could not touch user on login");
    }
    let mut memberships = users::memberships(&state.db, user.id).await?;
    if memberships.is_empty() {
        let team = coolify_db::team_pages::recreate_personal_team(
            &state.db,
            user.id,
            &user.name,
            state.now(),
        )
        .await?;
        coolify_db::notification_settings::create_defaults(&state.db, team).await?;
        memberships = users::memberships(&state.db, user.id).await?;
    }
    let team_id = resolve_stored_team(&user, &memberships).unwrap_or(NO_TEAM);
    if team_id != NO_TEAM
        && user.current_team_id != Some(team_id)
        && let Err(e) = users::set_current_team(&state.db, user.id, team_id).await
    {
        tracing::warn!(error = %e, "could not persist current team");
    }
    state.sessions.clear_failures(&keys);
    let hash = user.password.clone().unwrap_or_default();
    let (id, csrf) = state.sessions.create(user.id, team_id, &hash);
    tracing::info!(user_id = user.id, "ui login");
    let mut res = PhpJson(
        StatusCode::OK,
        session_json(&state, &user, &memberships, team_id, &csrf),
    )
    .into_response();
    res.headers_mut()
        .insert(header::SET_COOKIE, set_cookie(&state, &id, None));
    Ok(res)
}

fn hex_digest(s: &str) -> String {
    digest(s).iter().map(|b| format!("{b:02x}")).collect()
}

pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let live = live_session(&state, &headers).await?;
    if !csrf_ok(&headers, &live.csrf) {
        return Err(ApiError::Message(
            StatusCode::from_u16(419).unwrap_or(StatusCode::FORBIDDEN),
            "CSRF token mismatch.",
        ));
    }
    state.sessions.remove(&live.id);
    let mut res = PhpJson(StatusCode::OK, json!({"message": "Signed out."})).into_response();
    res.headers_mut()
        .insert(header::SET_COOKIE, set_cookie(&state, "", Some(0)));
    Ok(res)
}

pub async fn switch_team(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let live = live_session(&state, &headers).await?;
    if !csrf_ok(&headers, &live.csrf) {
        return Err(ApiError::Message(
            StatusCode::from_u16(419).unwrap_or(StatusCode::FORBIDDEN),
            "CSRF token mismatch.",
        ));
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let team_id = input
        .get("team_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            let mut e = serde_json::Map::new();
            e.insert("team_id".into(), json!(["The team id field is required."]));
            ApiError::Validation(e)
        })?;
    if !live.memberships.iter().any(|m| m.team_id == team_id) {
        return Err(ApiError::Message(
            StatusCode::FORBIDDEN,
            "You are not a member of this team.",
        ));
    }
    // Persisting the choice is best-effort in read-only mode.
    if let Err(e) = users::set_current_team(&state.db, live.user.id, team_id).await {
        tracing::warn!(error = %e, "could not persist current team");
    }
    state.sessions.set_team(&live.id, team_id);
    Ok(PhpJson(
        StatusCode::OK,
        session_json(&state, &live.user, &live.memberships, team_id, &live.csrf),
    )
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttles_after_five_failures() {
        let s = SessionStore::new(Duration::from_secs(60));
        let keys = vec!["a|ip".to_string()];
        for _ in 0..5 {
            assert!(s.throttled(&keys).is_none());
            s.record_failure(&keys);
        }
        assert!(s.throttled(&keys).is_some());
        s.clear_failures(&keys);
        assert!(s.throttled(&keys).is_none());
    }

    #[test]
    fn sessions_expire_and_end() {
        let s = SessionStore::new(Duration::from_millis(30));
        let (id, csrf) = s.create(1, 2, "$2y$hash");
        let got = s.get(&id).unwrap();
        assert_eq!((got.0, got.1, got.2.as_str()), (1, 2, csrf.as_str()));
        std::thread::sleep(Duration::from_millis(40));
        assert!(s.get(&id).is_none());
        let (id2, _) = s.create(1, 2, "$2y$hash");
        s.remove(&id2);
        assert!(s.get(&id2).is_none());
    }

    #[test]
    fn cookie_parsing_is_strict() {
        let mut h = HeaderMap::new();
        let id = "a".repeat(64);
        h.insert(
            header::COOKIE,
            format!("x=1; {COOKIE}={id}").parse().unwrap(),
        );
        assert_eq!(session_cookie(&h).as_deref(), Some(id.as_str()));
        h.insert(header::COOKIE, format!("{COOKIE}=../etc").parse().unwrap());
        assert_eq!(session_cookie(&h), None);
    }

    #[test]
    fn role_abilities() {
        assert!(abilities_for(Role::Admin).can(Ability::Write));
        assert!(abilities_for(Role::Owner).can(Ability::ReadSensitive));
        assert!(!abilities_for(Role::Member).can(Ability::Write));
        assert!(!abilities_for(Role::Member).can(Ability::Root));
    }
}
