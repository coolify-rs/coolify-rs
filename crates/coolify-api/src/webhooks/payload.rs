//! `Webhook\Concerns`: `ReadsWebhookPushPayload`, `DetectsSkipDeployCommits`,
//! `MatchesManualWebhookApplications`, `ThrottlesManualWebhookFailures` and
//! `ValidatesPreviewDeploymentRepository`.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use coolify_db::applications::ApplicationRow;
use hmac::{Hmac, Mac};
use regex::Regex;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::input::lenient_json;

/// `InvalidWebhookPayloadException::forField($key)`.
#[derive(Debug)]
pub struct InvalidPayload(pub String);

impl InvalidPayload {
    fn field(key: &str) -> Self {
        Self(format!("Nothing to do. Invalid '{key}' in the request."))
    }
}

/// `$request->collect()`: JSON or form input with `TrimStrings` and `ConvertEmptyStringsToNull`.
pub fn collect(headers: &HeaderMap, uri: &Uri, body: &Bytes) -> Value {
    if is_json(headers) {
        return Value::Object(lenient_json(headers, uri, body).0);
    }
    let mut map = Map::new();
    let is_form = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.starts_with("application/x-www-form-urlencoded"));
    if is_form {
        for (k, v) in form_pairs(body) {
            let trimmed = v.trim().to_owned();
            map.insert(
                k,
                if trimmed.is_empty() {
                    Value::Null
                } else {
                    Value::String(trimmed)
                },
            );
        }
    }
    Value::Object(map)
}

fn form_pairs(body: &[u8]) -> Vec<(String, String)> {
    let decode = |s: &str| {
        let bytes = s.replace('+', " ");
        let raw = bytes.as_bytes();
        let mut out = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'%' && i + 2 < raw.len() {
                let hex = std::str::from_utf8(&raw[i + 1..i + 3]).unwrap_or("");
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
            out.push(raw[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    String::from_utf8_lossy(body)
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect()
}

/// `$request->isJson()`.
pub fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.contains("/json") || c.contains("+json"))
}

/// `is_string($form_payload) ? json_decode($form_payload, true) : null`.
pub fn form_payload(payload: &Value) -> Value {
    payload
        .get("payload")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null)
}

/// `data_get($payload, 'a.b.0')`.
pub fn get<'a>(payload: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = payload;
    for key in path.split('.') {
        cur = match cur {
            Value::Object(m) => m.get(key)?,
            Value::Array(a) => a.get(key.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    (!cur.is_null()).then_some(cur)
}

/// A header value, or the empty string.
pub fn header_str(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// `Str::after($value, 'sha256=')`.
pub fn after_sha256(value: &str) -> &str {
    value.split_once("sha256=").map_or(value, |(_, r)| r)
}

/// `hash_hmac('sha256', $payload, $secret)`.
pub fn hmac_hex(payload: &[u8], secret: &str) -> String {
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return String::new();
    };
    mac.update(payload);
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `hash_equals($a, $b)`.
pub fn hash_equals(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// `webhookPushBranch($ref)`.
pub fn push_branch(reference: Option<&Value>) -> Option<String> {
    let r = reference?.as_str().filter(|s| !s.is_empty())?;
    let branch = r.split_once("refs/heads/").map_or(r, |(_, b)| b);
    (!branch.is_empty()).then(|| branch.to_owned())
}

/// `webhookString($value)`.
pub fn string_of(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// `webhookPayloadString($payload, $key, $required)`.
pub fn payload_string(
    payload: &Value,
    key: &str,
    required: bool,
) -> Result<Option<String>, InvalidPayload> {
    match get(payload, key) {
        None => {
            if required {
                Err(InvalidPayload::field(key))
            } else {
                Ok(None)
            }
        }
        Some(Value::String(s)) if s.is_empty() => {
            if required {
                Err(InvalidPayload::field(key))
            } else {
                Ok(None)
            }
        }
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(InvalidPayload::field(key)),
    }
}

/// `webhookPayloadId($payload, $key, $required, $max)`.
pub fn payload_id(
    payload: &Value,
    key: &str,
    required: bool,
    max: i64,
) -> Result<Option<i64>, InvalidPayload> {
    static DIGITS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\A[1-9][0-9]{0,18}\z").unwrap_or_else(|_| unreachable!()));
    let Some(value) = get(payload, key) else {
        return if required {
            Err(InvalidPayload::field(key))
        } else {
            Ok(None)
        };
    };
    let number = match value {
        Value::String(s) if DIGITS.is_match(s) => s.parse::<i64>().ok(),
        Value::Number(n) => n.as_i64(),
        _ => None,
    };
    match number {
        Some(n) if n >= 1 && n <= max => Ok(Some(n)),
        _ => Err(InvalidPayload::field(key)),
    }
}

/// `WEBHOOK_MAX_DATABASE_INTEGER`.
pub const MAX_DB_INT: i64 = 2_147_483_647;

/// `webhookPayloadDatabaseId()`.
pub fn database_id(
    payload: &Value,
    key: &str,
    required: bool,
) -> Result<Option<i64>, InvalidPayload> {
    payload_id(payload, key, required, MAX_DB_INT)
}

/// `webhookPullRequestId()`.
pub fn pull_request_id(payload: &Value, key: &str) -> Result<i64, InvalidPayload> {
    Ok(database_id(payload, key, true)?.unwrap_or_default())
}

/// `webhookCommitSha()`.
pub fn commit_sha(payload: &Value, key: &str) -> Result<Option<String>, InvalidPayload> {
    static SHA: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\A[0-9a-fA-F]{7,64}\z").unwrap_or_else(|_| unreachable!()));
    match get(payload, key) {
        None => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) if SHA.is_match(s) => Ok(Some(s.clone())),
        Some(_) => Err(InvalidPayload::field(key)),
    }
}

/// `webhookPayloadUrl()`.
pub fn payload_url(
    payload: &Value,
    key: &str,
    required: bool,
) -> Result<Option<String>, InvalidPayload> {
    let Some(value) = payload_string(payload, key, required)? else {
        return Ok(None);
    };
    let scheme_host = value.split_once("://").map(|(s, r)| {
        let host = r
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default()
            .rsplit('@')
            .next()
            .unwrap_or_default();
        (s.to_lowercase(), host.to_owned())
    });
    let ok = value.len() <= 255
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
        && scheme_host.is_some_and(|(s, h)| (s == "http" || s == "https") && !h.is_empty());
    if ok {
        Ok(Some(value))
    } else {
        Err(InvalidPayload::field(key))
    }
}

/// `webhookPushChangedFiles()`.
pub fn changed_files(payload: &Value) -> Option<Vec<String>> {
    let commits = get(payload, "commits")?.as_array()?;
    let mut out: Vec<String> = Vec::new();
    for commit in commits.iter().filter(|c| c.is_object()) {
        for kind in ["added", "removed", "modified"] {
            if let Some(files) = commit.get(kind).and_then(Value::as_array) {
                for f in files.iter().filter_map(Value::as_str) {
                    if !f.is_empty() && !out.iter().any(|x| x == f) {
                        out.push(f.to_owned());
                    }
                }
            }
        }
    }
    Some(out)
}

/// `webhookPushCommitMessages()`.
pub fn commit_messages(payload: &Value, path: &str) -> Vec<String> {
    get(payload, path)
        .and_then(Value::as_array)
        .map(|c| {
            c.iter()
                .filter_map(|m| m.get("message").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn has_skip_marker(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("[skip cd]") || lower.contains("[skip ci]")
}

/// `shouldSkipDeploy()`: every non-empty message has a marker.
pub fn should_skip_deploy(messages: &[String]) -> bool {
    let filled: Vec<&String> = messages.iter().filter(|m| !m.trim().is_empty()).collect();
    !filled.is_empty() && filled.iter().all(|m| has_skip_marker(m))
}

/// `shouldSkipDeployAny()`: any message has a marker.
pub fn should_skip_deploy_any(messages: &[Option<&str>]) -> bool {
    messages
        .iter()
        .flatten()
        .any(|m| !m.trim().is_empty() && has_skip_marker(m))
}

/// `isWebhookBranchDeletionPush()`.
pub fn is_branch_deletion(payload: &Value) -> bool {
    if get(payload, "deleted") == Some(&Value::Bool(true)) {
        return true;
    }
    get(payload, "after")
        .and_then(Value::as_str)
        .is_some_and(|a| !a.is_empty() && a.bytes().all(|b| b == b'0'))
}

/// `parseWatchPaths()`.
pub fn parse_watch_paths(value: &str) -> String {
    let mut out = Vec::new();
    for line in value.split('\n') {
        let path = line.trim();
        let normalized = if let Some(rest) = path.strip_prefix('!') {
            format!("!{}", rest.trim().trim_start_matches('/'))
        } else {
            path.trim_start_matches('/').to_owned()
        };
        if !normalized.is_empty() {
            out.push(normalized);
        }
    }
    out.join("\n").trim().to_owned()
}

/// `Application::globToRegex()`.
pub fn glob_to_regex(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut regex = String::new();
    let mut in_group = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '*' => {
                if chars.get(i + 1) == Some(&'*') {
                    regex.push_str(".*");
                    i += 1;
                    if chars.get(i + 1) == Some(&'/') {
                        i += 1;
                    }
                } else {
                    regex.push_str("[^/]*");
                }
            }
            '?' => regex.push_str("[^/]"),
            '[' => {
                in_group = true;
                regex.push('[');
                if matches!(chars.get(i + 1), Some('!' | '^')) {
                    regex.push('^');
                    i += 1;
                }
            }
            ']' => {
                if in_group {
                    in_group = false;
                    regex.push(']');
                } else {
                    regex.push_str("\\]");
                }
            }
            '.' | '(' | ')' | '+' | '{' | '}' | '$' | '^' | '|' | '\\' => {
                regex.push('\\');
                regex.push(c);
            }
            other => regex.push(other),
        }
        i += 1;
    }
    format!("^{regex}$")
}

/// `Application::matchPaths()`.
pub fn match_paths(files: &[String], patterns: &[String]) -> Vec<String> {
    if patterns.is_empty() {
        return Vec::new();
    }
    let compiled: Vec<(bool, Option<Regex>)> = patterns
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty() && *p != "0")
        .map(|p| {
            let exclusion = p.starts_with('!');
            let body = if exclusion { &p[1..] } else { p };
            (exclusion, Regex::new(&glob_to_regex(body)).ok())
        })
        .collect();
    let has_inclusion = patterns.iter().any(|p| !p.trim().starts_with('!'));
    files
        .iter()
        .filter(|file| {
            let mut include = None;
            for (exclusion, regex) in &compiled {
                if regex.as_ref().is_some_and(|r| r.is_match(file)) {
                    include = Some(!exclusion);
                }
            }
            include.unwrap_or(!has_inclusion)
        })
        .cloned()
        .collect()
}

/// `webhookPushMatchesWatchPaths()` (`isWatchPathsTriggered()`).
pub fn matches_watch_paths(a: &ApplicationRow, files: Option<&[String]>) -> bool {
    let Some(raw) = a.str("watch_paths").filter(|w| !w.trim().is_empty()) else {
        return true;
    };
    let Some(files) = files else {
        return true;
    };
    let patterns: Vec<String> = parse_watch_paths(raw)
        .split('\n')
        .map(str::to_owned)
        .collect();
    !match_paths(files, &patterns).is_empty()
}

/// The `watch_paths` of the details payload (`str($application->watch_paths)->explode("\n")`).
pub fn watch_path_list(a: &ApplicationRow) -> Vec<String> {
    a.str("watch_paths")
        .unwrap_or_default()
        .split('\n')
        .map(str::to_owned)
        .collect()
}

/// `normalizeManualWebhookRepositoryPath()`.
fn normalize_repository_path(path: &str) -> String {
    let p = path.trim();
    let p = p.split(['?', '#']).find(|s| !s.is_empty()).unwrap_or(p);
    let p = p.trim_matches('/');
    let lower = p.to_ascii_lowercase();
    if lower.ends_with(".git") {
        p[..p.len() - 4].to_owned()
    } else {
        p.to_owned()
    }
}

/// `manualWebhookRepositoryFullName()`.
pub fn repository_full_name(value: Option<&Value>) -> Option<String> {
    static NAME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\A[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)+\z").unwrap_or_else(|_| unreachable!())
    });
    let name = value?
        .as_str()?
        .trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\0' | '\x0B' | '/'));
    if name.is_empty() || !NAME.is_match(name) {
        return None;
    }
    Some(normalize_repository_path(name))
}

/// `canonicalManualWebhookRepository()`.
fn canonical_repository(repository: Option<&str>) -> Option<String> {
    let r = repository?.trim();
    if r.is_empty() {
        return None;
    }
    let path = if let Some((_, rest)) = r.split_once("://") {
        rest.split_once('/').map(|(_, p)| p.to_owned())?
    } else if let Some((_, _, _, path)) = crate::git_urls::parse_scp_style_git_url(r) {
        path
    } else {
        r.to_owned()
    };
    if path.is_empty() {
        return None;
    }
    Some(normalize_repository_path(&path))
}

/// `manualWebhookRepositoryMatches()`.
pub fn repository_matches(repository: Option<&str>, full_name: &str) -> bool {
    let Some(path) = canonical_repository(repository) else {
        return false;
    };
    let (path, name) = (path.to_lowercase(), full_name.to_lowercase());
    path == name || path.ends_with(&format!("/{name}"))
}

/// `isPreviewDeploymentRepositoryTrusted()`.
pub fn preview_repository_trusted(
    source: Option<&Value>,
    target: Option<&Value>,
    webhook: Option<&Value>,
    public_previews: bool,
) -> bool {
    let scalar = |v: Option<&Value>| -> Option<String> {
        match v? {
            Value::String(s) => Some(s.trim().to_owned()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(true) => Some("1".to_owned()),
            Value::Bool(false) => Some(String::new()),
            _ => None,
        }
        .filter(|s| !s.is_empty())
    };
    let (Some(s), Some(t), Some(w)) = (scalar(source), scalar(target), scalar(webhook)) else {
        return false;
    };
    hash_equals(&t, &w) && (hash_equals(&s, &t) || public_previews)
}

// ---------------------------------------------------------------------------
// ThrottlesManualWebhookFailures
// ---------------------------------------------------------------------------

const MAX_FAILURES: u32 = 30;
const DECAY: Duration = Duration::from_secs(60);

#[derive(Debug)]
struct Window {
    hits: u32,
    started: Instant,
    seen: HashSet<[u8; 32]>,
}

static FAILURES: LazyLock<Mutex<HashMap<String, Window>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn window_of<'a>(map: &'a mut HashMap<String, Window>, key: &str) -> Option<&'a mut Window> {
    let expired = map.get(key).is_some_and(|w| w.started.elapsed() >= DECAY);
    if expired {
        map.remove(key);
    }
    map.get_mut(key)
}

/// `manualWebhookFailureRateLimitKey()`.
pub fn failure_key(provider: &str, client: &str, full_name: &str, branch: Option<&str>) -> String {
    let scope = json!([full_name.to_lowercase(), branch]).to_string();
    let digest = Sha256::digest(scope.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("manual-webhook-failures:{provider}:{client}:{hex}")
}

/// `manualWebhookTokenAttempt()`.
pub fn token_attempt(token: &str) -> String {
    format!("token:{token}")
}

/// `manualWebhookSignedPayloadAttempt()`.
pub fn signed_attempt(body: &[u8], signature: &str) -> String {
    let hex: String = Sha256::digest(body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("hmac:{hex}:{signature}")
}

fn too_many(key: &str) -> Option<u64> {
    let mut map = FAILURES.lock().unwrap_or_else(|e| e.into_inner());
    let w = window_of(&mut map, key)?;
    (w.hits >= MAX_FAILURES).then(|| DECAY.saturating_sub(w.started.elapsed()).as_secs().max(1))
}

fn record_failure(key: &str, attempt: &str) {
    let marker: [u8; 32] = Sha256::digest(attempt.as_bytes()).into();
    let mut map = FAILURES.lock().unwrap_or_else(|e| e.into_inner());
    if map.len() > 10_000 {
        map.retain(|_, w| w.started.elapsed() < DECAY);
    }
    let _ = window_of(&mut map, key);
    let w = map.entry(key.to_owned()).or_insert_with(|| Window {
        hits: 0,
        started: Instant::now(),
        seen: HashSet::new(),
    });
    if w.seen.insert(marker) {
        w.hits += 1;
    }
}

/// `unauthenticatedManualWebhookFailurePayload()`.
pub fn failure_payload() -> Value {
    json!({"status": "failed", "message": "No matching application or invalid signature."})
}

/// `unauthenticatedManualWebhookResponse()`.
pub fn unauthenticated_response(key: &str, attempt: &str) -> Response {
    if let Some(retry) = too_many(key) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, retry.to_string())],
            axum::Json(json!({
                "status": "failed",
                "message": "Too many failed webhook authentication attempts. Try again later.",
            })),
        )
            .into_response();
    }
    record_failure(key, attempt);
    json_response(&json!([failure_payload()]))
}

/// `manualWebhookResponse()`.
pub fn manual_response(payloads: Vec<Value>, key: &str, attempt: &str) -> Response {
    let failure = failure_payload();
    let any = !payloads.is_empty();
    let authorized: Vec<Value> = payloads.into_iter().filter(|p| *p != failure).collect();
    if authorized.is_empty() && any {
        return unauthenticated_response(key, attempt);
    }
    json_response(&Value::Array(authorized))
}

/// `response($array)`: JSON without the pretty printing of `response()->json()`.
pub fn json_response(value: &Value) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        value.to_string(),
    )
        .into_response()
}

/// `response($message)`.
pub fn text_response(message: impl Into<String>) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=UTF-8")],
        message.into(),
    )
        .into_response()
}

/// `response($result['message'], 429)->header('Retry-After', 60)`.
pub fn queue_full(message: &str) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [
            (header::CONTENT_TYPE, "text/html; charset=UTF-8".to_owned()),
            (header::RETRY_AFTER, "60".to_owned()),
        ],
        message.to_owned(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        let m = |f: &str, p: &[&str]| {
            !match_paths(
                &[f.to_owned()],
                &p.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            )
            .is_empty()
        };
        assert!(m("src/a.js", &["src/**"]));
        assert!(!m("docs/a.md", &["src/**"]));
        assert!(!m("src/a/b.js", &["src/*.js"]));
        assert!(m("src/a.js", &["src/*.js"]));
        assert!(!m("src/a.md", &["src/**", "!src/*.md"]));
        assert!(m("other.txt", &["!src/**"]));
        assert_eq!(parse_watch_paths(" /a \n\n! /b\n"), "a\n!b");
    }

    #[test]
    fn repositories() {
        assert!(repository_matches(Some("https://h.test/O/R.git"), "o/r"));
        assert!(repository_matches(Some("git@h.test:o/r.git"), "o/r"));
        assert!(repository_matches(Some("https://h.test/sub/o/r"), "o/r"));
        assert!(!repository_matches(Some("https://h.test/o/rx"), "o/r"));
        assert_eq!(
            repository_full_name(Some(&json!("/o/r.git/"))).as_deref(),
            Some("o/r")
        );
        assert_eq!(repository_full_name(Some(&json!("a b"))), None);
    }

    #[test]
    fn skip_markers() {
        assert!(should_skip_deploy(&[
            "[skip ci] x".into(),
            "y [SKIP CD]".into()
        ]));
        assert!(!should_skip_deploy(&["[skip ci] x".into(), "y".into()]));
        assert!(!should_skip_deploy(&[]));
        assert!(should_skip_deploy_any(&[None, Some("a [skip ci]")]));
    }

    #[test]
    fn signature() {
        assert_eq!(
            hmac_hex(b"abc", "key"),
            "9c196e32dc0175f86f4b1cb89289d6619de6bee699e4c378e68309ed97a1a6ab"
        );
    }
}
