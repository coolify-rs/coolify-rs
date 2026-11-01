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
use coolify_db::audit::Actor;
use coolify_db::users::{self, LoginUser, Membership};
use coolify_domain::{Abilities, Ability, Role, TeamId};
use rand::RngCore;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, PhpJson};
use crate::state::AppState;

pub const COOKIE: &str = "coolify_rs_session";
/// The session of a visitor who is not signed in yet (`login.id`, flash, OAuth state).
const GUEST_COOKIE: &str = "coolify_rs_guest";
const GUEST_LIFETIME: Duration = Duration::from_secs(2 * 3600);
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

/// What upstream's session holds before `Auth::login`.
#[derive(Debug, Clone, Default)]
pub(crate) struct Guest {
    /// `login.id`: the user of a two-factor challenge.
    pub login_id: Option<i64>,
    /// `passkey.verification_options`.
    pub verification_options: Option<String>,
    /// Socialite's `state`.
    pub oauth_state: Option<String>,
    /// `oidc.nonce.{state}` and `oidc.code_verifier.{state}`: value and expiry.
    pub oidc: HashMap<String, (String, i64)>,
    /// `session('status')`, `session('error')` and `$errors`, read once.
    pub status: Option<String>,
    pub error: Option<String>,
    pub errors: Vec<String>,
    last_seen: Option<Instant>,
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
    guests: Mutex<HashMap<[u8; 32], Guest>>,
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
            guests: Mutex::new(HashMap::new()),
            idle,
        }
    }

    /// Runs `f` on the guest session `id`, creating it when needed.
    pub(crate) fn guest<R>(&self, id: &str, f: impl FnOnce(&mut Guest) -> R) -> Option<R> {
        let now = Instant::now();
        let mut m = self.guests.lock().ok()?;
        if m.len() >= MAX_SESSIONS {
            m.retain(|_, g| {
                g.last_seen
                    .is_some_and(|t| now.duration_since(t) < GUEST_LIFETIME)
            });
        }
        let g = m.entry(digest(id)).or_default();
        if g.last_seen
            .is_some_and(|t| now.duration_since(t) >= GUEST_LIFETIME)
        {
            *g = Guest::default();
        }
        g.last_seen = Some(now);
        Some(f(g))
    }

    /// `session()->pull('passkey.verification_options')` of a signed-in user.
    pub(crate) fn take_passkey_challenge(&self, id: &str) -> Option<String> {
        self.sessions
            .lock()
            .ok()?
            .get_mut(&digest(id))?
            .passkey_challenge
            .take()
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
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|p| p.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
        .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_value(headers, COOKIE)
}

fn cookie_header(state: &AppState, name: &str, value: &str, max_age: Option<u64>) -> HeaderValue {
    let secure = if state.secure_cookies { "; Secure" } else { "" };
    let age = max_age
        .map(|a| format!("; Max-Age={a}"))
        .unwrap_or_default();
    HeaderValue::from_str(&format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax{secure}{age}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn set_cookie(state: &AppState, value: &str, max_age: Option<u64>) -> HeaderValue {
    cookie_header(state, COOKIE, value, max_age)
}

/// The guest session of a request, and the cookie that starts a new one.
pub(crate) fn guest_id(state: &AppState, headers: &HeaderMap) -> (String, Option<HeaderValue>) {
    match cookie_value(headers, GUEST_COOKIE) {
        Some(id) => (id, None),
        None => {
            let id = random_hex(32);
            let cookie = cookie_header(state, GUEST_COOKIE, &id, None);
            (id, Some(cookie))
        }
    }
}

/// Adds the cookie of a new guest session to a response.
pub(crate) fn with_guest_cookie(mut res: Response, cookie: Option<HeaderValue>) -> Response {
    if let Some(c) = cookie {
        res.headers_mut().append(header::SET_COOKIE, c);
    }
    res
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

/// The signed-in user of a browser callback (`Route::middleware(['web', 'auth'])`).
#[derive(Debug)]
pub struct WebUser {
    session_id: String,
    pub user_id: i64,
    pub team_id: TeamId,
    memberships: Vec<Membership>,
}

impl WebUser {
    pub fn member_of(&self, team: TeamId) -> bool {
        self.memberships.iter().any(|m| m.team_id == team)
    }

    /// `refreshSession($team)`.
    pub fn switch_team(&self, state: &AppState, team: TeamId) {
        if team != self.team_id {
            state.sessions.set_team(&self.session_id, team);
        }
    }

    /// The request context the UI handlers read.
    pub fn ctx(&self) -> AuthCtx {
        let role = self
            .memberships
            .iter()
            .find(|m| m.team_id == self.team_id)
            .map_or(Role::Member, |m| m.role);
        AuthCtx {
            token_id: 0,
            user_id: self.user_id,
            team_id: self.team_id,
            team_id_raw: self.team_id.to_string(),
            role,
            can_read_sensitive: role.is_admin(),
            abilities: abilities_for(role),
        }
    }
}

/// `auth` for a browser request: signed-out visitors go to the login page.
pub async fn web_user(state: &AppState, headers: &HeaderMap) -> Result<WebUser, Response> {
    match live_session(state, headers).await {
        Ok(live) => Ok(WebUser {
            session_id: live.id,
            user_id: live.user.id,
            team_id: live.team_id,
            memberships: live.memberships,
        }),
        Err(_) => Err((
            StatusCode::FOUND,
            [(axum::http::header::LOCATION, "/login")],
        )
            .into_response()),
    }
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

const FORCE_RESET_REQUIRED: &str = "A password reset is required for this account.";

/// `allowedPathsForTwoFactorSetup()` for the requests behind those pages.
const TWO_FACTOR_SETUP_PATHS: &[&str] = &[
    "/ui/auth/two-factor-required",
    "/ui/auth/confirm-password/state",
    "/ui/profile/confirm-password",
    "/ui/profile/two-factor",
    "/ui/profile/two-factor/confirm",
    "/ui/profile/two-factor/recovery-codes",
];

/// `CheckForcePasswordReset`, then `RequireTeamTwoFactor`, for a JSON request. The pages redirect
/// on the flags of `/ui/session`, in `App.svelte`.
fn web_gates(live: &Live, path: &str) -> Result<(), ApiError> {
    if live.user.force_password_reset && path != "/ui/auth/force-password-reset" {
        return Err(ApiError::Message(
            StatusCode::FORBIDDEN,
            FORCE_RESET_REQUIRED,
        ));
    }
    let required = live
        .memberships
        .iter()
        .find(|m| m.team_id == live.team_id)
        .is_some_and(|m| m.two_factor_required);
    if required
        && live.user.two_factor_confirmed_at.is_none()
        && !TWO_FACTOR_SETUP_PATHS.contains(&path)
    {
        return Err(ApiError::Message(StatusCode::FORBIDDEN, TEAM_2FA_REQUIRED));
    }
    Ok(())
}

/// Guard for pages that need a signed-in user but no usable team.
pub async fn user_guard(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let headers = req.headers().clone();
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let result = async {
        let live = live_session(&state, &headers).await?;
        if needs_csrf(&method) && !csrf_ok(&headers, &live.csrf) {
            return Err(ApiError::Message(
                StatusCode::from_u16(419).unwrap_or(StatusCode::FORBIDDEN),
                "CSRF token mismatch.",
            ));
        }
        web_gates(&live, &path)?;
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

const TEAM_2FA_REQUIRED: &str =
    "Your team requires two-factor authentication. Configure it to continue.";

/// The team context of a browser session behind the web gates.
fn web_ctx(live: &Live, path: &str) -> Result<AuthCtx, ApiError> {
    web_gates(live, path)?;
    let m = live
        .memberships
        .iter()
        .find(|m| m.team_id == live.team_id)
        .ok_or(ApiError::Message(
            StatusCode::FORBIDDEN,
            "You are no longer a member of this team.",
        ))?;
    Ok(AuthCtx {
        token_id: 0,
        user_id: live.user.id,
        team_id: m.team_id,
        team_id_raw: m.team_id.to_string(),
        role: m.role,
        can_read_sensitive: m.role.is_admin(),
        abilities: abilities_for(m.role),
    })
}

/// Whether the request carries a session cookie at all, valid or not.
pub(crate) fn has_session_cookie(headers: &HeaderMap) -> bool {
    session_cookie(headers).is_some()
}

/// What [`ui_guard`] gives the requests of the terminal server to Coolify (`/terminal/auth`,
/// `/terminal/session`), for a websocket that has no such requests of its own.
pub(crate) async fn terminal_ctx(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<AuthCtx, ApiError> {
    let live = live_session(state, headers).await?;
    web_ctx(&live, "/terminal/ws")
}

/// Guard for `/ui/api/v1/...`: session → CSRF → forced reset → membership → team 2FA policy.
pub async fn ui_guard(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let headers = req.headers().clone();
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let result = async {
        let live = live_session(&state, &headers).await?;
        if needs_csrf(&method) && !csrf_ok(&headers, &live.csrf) {
            return Err(ApiError::Message(
                StatusCode::from_u16(419).unwrap_or(StatusCode::FORBIDDEN),
                "CSRF token mismatch.",
            ));
        }
        web_ctx(&live, &path)
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
        .route("/ui/session/two-factor", post(two_factor_challenge))
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
        // `CheckForcePasswordReset` and `showBoarding()`, which the pages redirect on.
        "force_password_reset": user.force_password_reset,
        "show_boarding": current.is_some_and(|m| m.show_boarding && m.role.is_admin())
            && std::env::var("APP_ENV").map_or(true, |e| e != "local"),
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

/// `authContext()` of `EventServiceProvider`.
pub(crate) fn auth_context(team_id: Option<TeamId>, id: i64, name: &str, email: &str) -> Value {
    json!({
        "team_id": team_id,
        "resource": "user",
        "user_name": name,
        "actor_id": id,
        "actor_name": name,
        "actor_email": email,
    })
}

/// `auditLog($event, $this->authContext($user))`: the actor comes from the context.
pub(crate) async fn audit_auth(
    state: &AppState,
    team_id: Option<TeamId>,
    who: (i64, &str, &str),
    meta: &Meta,
    event: &str,
) {
    audit_user_event(state, team_id, who, meta, event, false).await;
}

/// The same for an event from before `Auth::login`: `actor_type` is `system`.
pub(crate) async fn audit_guest(
    state: &AppState,
    team_id: Option<TeamId>,
    who: (i64, &str, &str),
    meta: &Meta,
    event: &str,
) {
    audit_user_event(state, team_id, who, meta, event, true).await;
}

async fn audit_user_event(
    state: &AppState,
    team_id: Option<TeamId>,
    (id, name, email): (i64, &str, &str),
    meta: &Meta,
    event: &str,
    from_context: bool,
) {
    let actor = Actor {
        from_context,
        user_id: Some(id),
        name: Some(name.to_string()),
        email: Some(email.to_string()),
        ip: meta.ip.clone(),
        user_agent: meta.user_agent.clone(),
        ..Actor::default()
    };
    crate::audit::log_as(
        state,
        team_id,
        &actor,
        event,
        auth_context(team_id, id, name, email),
    )
    .await;
}

/// `Auth::login($user)` with the team choice of `Fortify::authenticateUsing`: restores the last
/// team, recreates the personal team of a user without teams, or leaves the choice open. Records
/// `auth.user.login_succeeded` for `audit_team`: the team in the session at that moment.
pub(crate) async fn establish(
    state: &AppState,
    user_id: i64,
    audit_team: impl FnOnce(Option<TeamId>) -> Option<TeamId>,
    meta: &Meta,
) -> Result<(HeaderValue, Value), ApiError> {
    let user = users::get(&state.db, user_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;
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
    let hash = user.password.clone().unwrap_or_default();
    let (id, csrf) = state.sessions.create(user.id, team_id, &hash);
    tracing::info!(user_id = user.id, "ui login");
    audit_auth(
        state,
        audit_team((team_id != NO_TEAM).then_some(team_id)),
        (user.id, &user.name, &user.email),
        meta,
        "auth.user.login_succeeded",
    )
    .await;
    Ok((
        set_cookie(state, &id, None),
        session_json(state, &user, &memberships, team_id, &csrf),
    ))
}

/// A JSON response that starts the session.
pub(crate) fn signed_in(cookie: HeaderValue, body: Value) -> Response {
    let mut res = PhpJson(StatusCode::OK, body).into_response();
    res.headers_mut().insert(header::SET_COOKIE, cookie);
    res
}

/// `auditLog('auth.user.login_failed', ...)`, once a minute per address and attempted email.
async fn audit_failed_login(state: &AppState, email: &str, meta: &Meta) {
    let key = format!(
        "audit-failure-dedup:{}|{}",
        meta.ip.as_deref().unwrap_or_default(),
        email.trim().to_lowercase()
    );
    if crate::rules::throttle(&key, 1, Duration::from_secs(60)).is_some() {
        return;
    }
    // Users sometimes type a password into the email field: only a valid address is stored.
    let attempted = if email.is_empty() {
        Value::Null
    } else if crate::rules::is_email(email) {
        json!(email)
    } else {
        json!("[invalid]")
    };
    crate::audit::log_system(
        state,
        None,
        "auth.user.login_failed",
        json!({"attempted_email": attempted, "guard": "web"}),
        "warning",
        meta,
    )
    .await;
}

pub async fn login(
    State(state): State<AppState>,
    meta: Meta,
    req: Request,
) -> Result<Response, ApiError> {
    let ip = meta.ip.clone().unwrap_or_default();
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
    // `throttle:login`: every attempt counts, also a successful one.
    let mut keys = vec![format!("{email}|{ip}")];
    if let Some(identity) = crate::rules::normalize_email_identity(Some(&email)) {
        keys.push(format!("email:{}", hex_digest(&identity)));
    }
    if let Some(secs) = state.sessions.throttled(&keys) {
        return Err(ApiError::Throttled(secs));
    }
    state.sessions.record_failure(&keys);
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

    let user = users::find_by_email(&state.db, &email).await?;
    let ok = match user
        .as_ref()
        .and_then(|u| u.password.as_deref().map(|p| (u, p)))
    {
        Some((_, hash)) => password::verify(pass, hash),
        None => password::verify_dummy(pass),
    };
    let Some(user) = user.filter(|_| ok) else {
        audit_failed_login(&state, &email, &meta).await;
        return Err(failed_login());
    };

    // `Fortify::authenticateUsing`: touches the user before anything else.
    if let Err(e) = users::touch(&state.db, user.id, state.now()).await {
        tracing::warn!(error = %e, "could not touch user on login");
    }
    // `RedirectIfTwoFactorAuthenticatable`: `login.id`, and the challenge page.
    if user.has_two_factor() {
        let (guest, cookie) = guest_id(&state, &headers);
        state.sessions.guest(&guest, |g| g.login_id = Some(user.id));
        return Ok(with_guest_cookie(
            PhpJson(StatusCode::OK, json!({"two_factor": true})).into_response(),
            cookie,
        ));
    }
    let (cookie, body) = establish(&state, user.id, |team| team, &meta).await?;
    Ok(signed_in(cookie, body))
}

/// `TwoFactorAuthenticatedSessionController::store`, with `throttle:two-factor`.
async fn two_factor_challenge(
    State(state): State<AppState>,
    meta: Meta,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let text = |k: &str| {
        input
            .get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let (code, recovery) = (text("code"), text("recovery_code"));
    let failed = || {
        // `FailedTwoFactorLoginResponse`.
        let (key, message) = if recovery.is_some() {
            (
                "recovery_code",
                "The provided two factor recovery code was invalid.",
            )
        } else {
            (
                "code",
                "The provided two factor authentication code was invalid.",
            )
        };
        let mut errors = serde_json::Map::new();
        errors.insert(key.into(), json!([message]));
        ApiError::ValidationWithMessage(message, errors)
    };
    let (guest, _) = guest_id(&state, &headers);
    let login_id = state.sessions.guest(&guest, |g| g.login_id).flatten();
    if let Some(secs) = crate::rules::throttle(
        &format!("two-factor:{}", login_id.unwrap_or(-1)),
        5,
        LOGIN_WINDOW,
    ) {
        return Err(ApiError::Throttled(secs));
    }
    let user = match login_id {
        Some(id) => users::get(&state.db, id).await?,
        None => None,
    };
    let Some(user) = user else {
        return Err(failed());
    };
    let encrypter = state.encrypter.as_ref().ok_or(ApiError::Message(
        StatusCode::SERVICE_UNAVAILABLE,
        "APP_KEY is not configured; two-factor accounts cannot sign in.",
    ))?;
    if let Some(given) = recovery {
        // `validRecoveryCode` and `replaceRecoveryCode`.
        let stored = coolify_db::profile::get(&state.db, user.id)
            .await?
            .and_then(|p| p.two_factor_recovery_codes)
            .and_then(|c| encrypter.decrypt_maybe_serialized(&c).ok())
            .unwrap_or_default();
        let codes: Vec<String> = serde_json::from_str(&stored).unwrap_or_default();
        let Some(found) = codes
            .iter()
            .find(|c| bool::from(c.as_bytes().ct_eq(given.as_bytes())))
        else {
            return Err(failed());
        };
        let replaced = stored.replacen(found.as_str(), &totp::recovery_code(), 1);
        coolify_db::profile::set_recovery_codes(
            &state.db,
            user.id,
            &encrypter.encrypt_serialized(&replaced),
            state.now(),
        )
        .await?;
    } else {
        let Some(code) = code else {
            return Err(failed());
        };
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
        let Some(step) = totp::verify(&secret, code, now, 1, last) else {
            return Err(failed());
        };
        if let Ok(mut m) = state.sessions.totp_steps.lock() {
            m.insert(user.id, step);
        }
    }
    state.sessions.guest(&guest, |g| g.login_id = None);
    let (cookie, body) = establish(&state, user.id, |team| team, &meta).await?;
    Ok(signed_in(cookie, body))
}

fn hex_digest(s: &str) -> String {
    digest(s).iter().map(|b| format!("{b:02x}")).collect()
}

pub async fn logout(
    State(state): State<AppState>,
    meta: Meta,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let live = live_session(&state, &headers).await?;
    if !csrf_ok(&headers, &live.csrf) {
        return Err(ApiError::Message(
            StatusCode::from_u16(419).unwrap_or(StatusCode::FORBIDDEN),
            "CSRF token mismatch.",
        ));
    }
    audit_auth(
        &state,
        (live.team_id != NO_TEAM).then_some(live.team_id),
        (live.user.id, &live.user.name, &live.user.email),
        &meta,
        "auth.user.logged_out",
    )
    .await;
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
    }

    #[test]
    fn guest_sessions_keep_their_values() {
        let s = SessionStore::new(Duration::from_secs(60));
        let id = "a".repeat(64);
        s.guest(&id, |g| g.login_id = Some(7));
        assert_eq!(s.guest(&id, |g| g.login_id).flatten(), Some(7));
        assert_eq!(s.guest(&"b".repeat(64), |g| g.login_id).flatten(), None);
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
