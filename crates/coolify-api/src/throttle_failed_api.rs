//! `App\Http\Middleware\ThrottleFailedApiRequests`: slows down API clients that keep sending
//! requests that fail (401, 403, and 404 without a token).
//!
//! Global middleware, so requests to API routes that do not exist count too. Each failed request
//! counts for its bearer token (or the source IP without a token). A 404 counts only without a
//! token: a client with a token often checks if a resource exists. When a client reaches the
//! limit in one minute, all its requests get 429. The block is 1 minute, then 5 and 15 minutes
//! when the client reaches the limit again within one hour. The team of the token gets one
//! notification each day.
//!
//! Upstream keeps the counters in the cache (Redis); here they live in the application state.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use coolify_compat::sanctum::{hash_token, parse_bearer_token, verify_token};
use coolify_jobs::notifications::Notification;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::response::PhpJson;
use crate::state::AppState;

/// `DECAY_SECONDS`: the window in which the failed requests are counted.
const DECAY_SECONDS: u64 = 60;
/// `BLOCK_SECONDS`.
const BLOCK_SECONDS: [u64; 3] = [60, 300, 900];
/// `STRIKE_MEMORY_SECONDS`.
const STRIKE_MEMORY_SECONDS: u64 = 3600;
/// `NOTIFICATION_INTERVAL_SECONDS`.
const NOTIFICATION_INTERVAL_SECONDS: u64 = 86400;
/// The default of `config('api.failed_request_rate_limit')`.
const DEFAULT_LIMIT: i64 = 30;
/// More clients than this are not remembered at once; expired entries go first.
const MAX_CLIENTS: usize = 50_000;

/// `config('api.failed_request_rate_limit')`: failed requests (401, 403, 404) per minute for one
/// token or source IP before it gets 429. 0 turns it off.
fn configured_limit() -> i64 {
    match std::env::var("API_FAILED_REQUEST_RATE_LIMIT") {
        Ok(v) if !v.trim().is_empty() => v.trim().parse().unwrap_or(0),
        _ => DEFAULT_LIMIT,
    }
}

/// What the cache holds for one client: the `RateLimiter` counter, `:strikes` and
/// `:blocked-until`, each with its own expiry (Unix seconds).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Client {
    hits: u32,
    hits_expire: u64,
    strikes: u32,
    strikes_expire: u64,
    blocked_until: u64,
}

impl Client {
    fn expired(&self, now: u64) -> bool {
        self.hits_expire <= now && self.strikes_expire <= now && self.blocked_until <= now
    }
}

/// The counters of every client.
#[derive(Debug, Default)]
struct FailedRequests(HashMap<String, Client>);

impl FailedRequests {
    /// `blockedFor($key)`: the seconds until the block of the client ends, or 0.
    fn blocked_for(&self, key: &str, now: u64) -> u64 {
        self.0
            .get(key)
            .map_or(0, |c| c.blocked_until.saturating_sub(now))
    }

    /// `RateLimiter::hit($key, DECAY_SECONDS) >= $limit`, then `block($key)`: counts a failed
    /// request and returns true when it blocked the client.
    fn record_failure(&mut self, key: &str, limit: u32, now: u64) -> bool {
        if self.0.len() >= MAX_CLIENTS && !self.0.contains_key(key) {
            self.0.retain(|_, c| !c.expired(now));
            if self.0.len() >= MAX_CLIENTS {
                return false;
            }
        }
        let client = self.0.entry(key.to_owned()).or_default();
        // The counter starts with the first failed request and ends after the window.
        if client.hits_expire <= now {
            client.hits = 0;
            client.hits_expire = now + DECAY_SECONDS;
        }
        client.hits += 1;
        if client.hits < limit {
            return false;
        }
        // `block()`: longer each time the client reaches the limit again within one hour.
        let strikes = if client.strikes_expire <= now {
            1
        } else {
            client.strikes + 1
        };
        client.strikes = strikes;
        client.strikes_expire = now + STRIKE_MEMORY_SECONDS;
        let index = (strikes as usize).min(BLOCK_SECONDS.len()) - 1;
        client.blocked_until = now + BLOCK_SECONDS[index];
        // `RateLimiter::clear($key)`.
        client.hits = 0;
        client.hits_expire = 0;
        true
    }
}

/// The throttle of one application: its limit and the counters of its clients.
#[derive(Debug)]
pub struct FailedApiRequests {
    limit: i64,
    clients: Mutex<FailedRequests>,
}

impl FailedApiRequests {
    /// `limit` is `config('api.failed_request_rate_limit')`; `None` reads the environment.
    pub fn new(limit: Option<i64>) -> Self {
        Self {
            limit: limit.unwrap_or_else(configured_limit),
            clients: Mutex::new(FailedRequests::default()),
        }
    }

    fn blocked_for(&self, key: &str) -> u64 {
        self.clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .blocked_for(key, unix_now())
    }

    fn record_failure(&self, key: &str, limit: u32) -> bool {
        self.clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record_failure(key, limit, unix_now())
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `$request->is('api/*') && ! $request->is('api/health', 'api/v1/health')`.
fn is_throttled_path(path: &str) -> bool {
    let path = path.trim_matches('/');
    path.starts_with("api/") && path != "api/health" && path != "api/v1/health"
}

/// `isCountedFailure()`: 401 and 403 always, 404 only without a token.
fn is_counted_failure(status: u16, has_bearer: bool) -> bool {
    matches!(status, 401 | 403) || (status == 404 && !has_bearer)
}

/// `key($request)`: the hash of the bearer token, or the source IP without a token.
fn client_key(bearer: Option<&str>, ip: &str) -> String {
    match bearer {
        Some(token) => {
            let digest = Sha256::digest(token.as_bytes());
            let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            format!("api-failed-requests:token:{hex}")
        }
        None => format!("api-failed-requests:ip:{ip}"),
    }
}

/// `$request->method().' /'.$request->path().' → '.$statusCode`.
fn last_request(method: &str, path: &str, status: u16) -> String {
    format!("{method} /{} → {status}", path.trim_matches('/'))
}

/// The 429 of a blocked client.
fn blocked_response(retry_after: u64) -> Response {
    let mut response = PhpJson(
        StatusCode::TOO_MANY_REQUESTS,
        json!({
            "message": format!("Too many failed API requests. Retry after {retry_after} seconds."),
        }),
    )
    .into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(retry_after));
    response
}

/// The middleware. It wraps every route, as upstream's global middleware does.
pub async fn middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let limit = state.failed_api_requests.limit;
    let path = req.uri().path().to_owned();
    if limit <= 0 || !is_throttled_path(&path) {
        return next.run(req).await;
    }
    let limit = u32::try_from(limit).unwrap_or(u32::MAX);
    let bearer = crate::auth::bearer(req.headers());
    let ip = crate::auth::client_ip(&state, &req).to_string();
    let key = client_key(bearer.as_deref(), &ip);
    let method = req.method().as_str().to_owned();

    let retry_after = state.failed_api_requests.blocked_for(&key);
    if retry_after > 0 {
        return blocked_response(retry_after);
    }

    let response = next.run(req).await;
    let status = response.status().as_u16();
    if !is_counted_failure(status, bearer.is_some()) {
        return response;
    }
    let blocked = state.failed_api_requests.record_failure(&key, limit);
    if blocked && let Some(token) = bearer {
        notify_team(
            &state,
            &token,
            i64::from(limit),
            last_request(&method, &path, status),
        )
        .await;
    }
    response
}

/// `PersonalAccessToken::findToken($bearerToken)`: by id and hash, without the checks of the
/// guard (an expired or paused token is found too). Returns the id, the name and the team.
async fn find_token(
    state: &AppState,
    bearer: &str,
) -> Result<Option<(i64, String, Option<String>)>, sqlx::Error> {
    let Some(parsed) = parse_bearer_token(bearer) else {
        return Ok(None);
    };
    type Row = (i64, String, Option<String>, String);
    let row: Option<Row> = match parsed.id {
        Some(id) => sqlx::query_as(
            "SELECT id, name::text, team_id::text, token::text FROM personal_access_tokens WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(state.db.pool())
        .await?
        .filter(|r: &Row| verify_token(&r.3, parsed.plaintext)),
        None => {
            sqlx::query_as(
                "SELECT id, name::text, team_id::text, token::text FROM personal_access_tokens WHERE token = $1",
            )
            .bind(hash_token(parsed.plaintext))
            .fetch_optional(state.db.pool())
            .await?
        }
    };
    Ok(row.map(|(id, name, team_id, _)| (id, name, team_id)))
}

/// `notifyTeam()`: the team of the token gets one notification each day.
async fn notify_team(state: &AppState, bearer: &str, failed_requests: i64, last_request: String) {
    let token = match find_token(state, bearer).await {
        Ok(Some(token)) => token,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(error = %e, "could not look up the failing API token");
            return;
        }
    };
    let (token_id, token_name, team_id) = token;
    // `! $token->team_id`: no team, or team "0" (a falsy string).
    let Some(team_id) = team_id
        .as_deref()
        .and_then(|t| t.trim().parse::<i64>().ok())
        .filter(|id| *id != 0)
    else {
        return;
    };
    // `Cache::add("api-token-failing-notified:{$token->id}", true, NOTIFICATION_INTERVAL_SECONDS)`.
    let fresh = coolify_jobs::server_ops::MARKS.add(
        format!("api-token-failing-notified:{token_id}"),
        Duration::from_secs(NOTIFICATION_INTERVAL_SECONDS),
    );
    if !fresh {
        return;
    }
    coolify_jobs::notifications::notify(
        &state.db,
        state.encrypter.as_ref(),
        team_id,
        Notification::ApiTokenFailing {
            token_name,
            failed_requests,
            last_request,
        },
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_api_requests_are_counted() {
        assert!(is_throttled_path("/api/v1/applications"));
        assert!(is_throttled_path("/api/v1/does-not-exist"));
        assert!(is_throttled_path("/api/feedback"));
        assert!(!is_throttled_path("/api/health"));
        assert!(!is_throttled_path("/api/v1/health"));
        assert!(!is_throttled_path("/api"));
        assert!(!is_throttled_path("/ui/session"));
        assert!(!is_throttled_path("/apiary"));
    }

    #[test]
    fn a_missing_resource_counts_only_without_a_token() {
        assert!(is_counted_failure(401, true));
        assert!(is_counted_failure(401, false));
        assert!(is_counted_failure(403, true));
        assert!(is_counted_failure(404, false));
        assert!(!is_counted_failure(404, true));
        assert!(!is_counted_failure(200, false));
        assert!(!is_counted_failure(422, true));
        assert!(!is_counted_failure(500, false));
        assert!(!is_counted_failure(429, true));
    }

    #[test]
    fn a_client_is_its_token_or_its_address() {
        assert_eq!(
            client_key(None, "203.0.114.7"),
            "api-failed-requests:ip:203.0.114.7"
        );
        // sha256("abc").
        assert_eq!(
            client_key(Some("abc"), "203.0.114.7"),
            "api-failed-requests:token:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_ne!(client_key(Some("a"), "x"), client_key(Some("b"), "x"));
    }

    #[test]
    fn the_last_request_names_the_method_path_and_status() {
        assert_eq!(
            last_request("GET", "/api/v1/servers", 401),
            "GET /api/v1/servers → 401"
        );
    }

    #[test]
    fn a_client_is_blocked_when_it_reaches_the_limit_in_one_minute() {
        let mut failed = FailedRequests::default();
        let now = 1_000_000;
        for i in 0..2 {
            assert!(!failed.record_failure("k", 3, now + i));
            assert_eq!(failed.blocked_for("k", now + i), 0);
        }
        assert!(failed.record_failure("k", 3, now + 2));
        assert_eq!(failed.blocked_for("k", now + 2), 60);
        assert_eq!(failed.blocked_for("k", now + 32), 30);
        assert_eq!(failed.blocked_for("k", now + 62), 0);
        // Other clients are not affected.
        assert_eq!(failed.blocked_for("other", now + 2), 0);
    }

    #[test]
    fn failures_in_different_minutes_do_not_add_up() {
        let mut failed = FailedRequests::default();
        let now = 1_000_000;
        assert!(!failed.record_failure("k", 3, now));
        assert!(!failed.record_failure("k", 3, now + 1));
        // The window started with the first failure and is over.
        assert!(!failed.record_failure("k", 3, now + 60));
        assert!(!failed.record_failure("k", 3, now + 61));
        assert!(failed.record_failure("k", 3, now + 62));
    }

    #[test]
    fn the_block_gets_longer_each_time_within_one_hour() {
        let mut failed = FailedRequests::default();
        let mut now = 1_000_000;
        let mut blocks = Vec::new();
        for _ in 0..4 {
            assert!(failed.record_failure("k", 1, now));
            let seconds = failed.blocked_for("k", now);
            blocks.push(seconds);
            now += seconds;
        }
        assert_eq!(blocks, [60, 300, 900, 900]);
        // After an hour without reaching the limit the client starts again with one minute.
        now += STRIKE_MEMORY_SECONDS;
        assert!(failed.record_failure("k", 1, now));
        assert_eq!(failed.blocked_for("k", now), 60);
    }

    #[test]
    fn the_counter_starts_again_after_a_block() {
        let mut failed = FailedRequests::default();
        let now = 1_000_000;
        assert!(!failed.record_failure("k", 2, now));
        assert!(failed.record_failure("k", 2, now));
        // `RateLimiter::clear`: the next failure is the first of a new window.
        assert!(!failed.record_failure("k", 2, now + 60));
        assert!(failed.record_failure("k", 2, now + 61));
        assert_eq!(failed.blocked_for("k", now + 61), 300);
    }

    #[test]
    fn the_blocked_answer_tells_when_to_retry() {
        let response = blocked_response(42);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[header::RETRY_AFTER], "42");
    }

    #[test]
    fn the_limit_comes_from_the_options_or_the_environment() {
        assert_eq!(FailedApiRequests::new(Some(0)).limit, 0);
        assert_eq!(FailedApiRequests::new(Some(5)).limit, 5);
        // Not set in the test process: the default of `config/api.php`.
        if std::env::var("API_FAILED_REQUEST_RATE_LIMIT").is_err() {
            assert_eq!(FailedApiRequests::new(None).limit, 30);
        }
    }

    #[test]
    fn each_application_counts_its_own_clients() {
        let one = FailedApiRequests::new(Some(1));
        let other = FailedApiRequests::new(Some(1));
        assert!(one.record_failure("k", 1));
        assert!(one.blocked_for("k") > 0);
        assert_eq!(other.blocked_for("k"), 0);
    }
}
