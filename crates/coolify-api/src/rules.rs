//! Validation rule objects shared by the settings and notification endpoints.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::response::ApiError;
use crate::validation::{Field, Messages, validate};

/// `App\Rules\ValidHostname`.
pub fn valid_hostname(v: &Value) -> Option<&'static str> {
    let text = match v {
        Value::String(s) => s.as_str(),
        _ => return None,
    };
    if text.is_empty() || text == "0" {
        return None;
    }
    let host = coolify_remote::text::php_trim(text);
    if host.len() > 253 {
        return Some("The :attribute must not exceed 253 characters.");
    }
    const INVALID: &str = "The :attribute contains invalid characters. Only letters (a-z, A-Z), numbers (0-9), hyphens (-), and dots (.) are allowed.";
    if host.chars().any(|c| {
        (c as u32) < 0x20 || c as u32 == 0x7f || ";|&$`(){}<>\"'\\!*?[]~^:#@%=+, ".contains(c)
    }) {
        return Some(INVALID);
    }
    let host = host.to_lowercase();
    if host.starts_with('.') || host.ends_with('.') {
        return Some("The :attribute cannot start or end with a dot.");
    }
    if host.contains("..") {
        return Some("The :attribute cannot contain consecutive dots.");
    }
    for label in host.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Some(
                "The :attribute contains an invalid label. Each segment must be 1-63 characters.",
            );
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Some(
                "The :attribute contains an invalid label. Labels cannot start or end with a hyphen.",
            );
        }
        if !label
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Some(INVALID);
        }
    }
    None
}

/// Laravel's `email` rule with its default `RFCValidation`.
pub fn is_email(s: &str) -> bool {
    let Some((local, domain)) = s.rsplit_once('@') else {
        return false;
    };
    if local.is_empty() || domain.is_empty() || local.len() > 64 || s.len() > 254 {
        return false;
    }
    if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let quoted = local.len() >= 2 && local.starts_with('"') && local.ends_with('"');
    if !quoted {
        if local.starts_with('.') || local.ends_with('.') || local.contains("..") {
            return false;
        }
        if local.chars().any(|c| {
            c.is_ascii() && !(c.is_ascii_alphanumeric() || "!#$%&'*+-/=?^_`{|}~.".contains(c))
        }) {
            return false;
        }
    }
    if domain.starts_with('[') && domain.ends_with(']') {
        return true;
    }
    domain.split('.').all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars()
                .all(|c| !c.is_ascii() || c.is_ascii_alphanumeric() || c == '-')
    })
}

/// `'email'` as a rule object.
pub fn email_rule(v: &Value) -> Option<&'static str> {
    const MESSAGE: &str = "The :attribute field must be a valid email address.";
    match v {
        Value::String(s) if is_email(s) => None,
        _ => Some(MESSAGE),
    }
}

/// Errors of `validate`, or an empty map when it passed.
pub fn errors_of(
    input: &crate::input::Input,
    fields: &[Field],
    messages: Messages,
) -> Result<Map<String, Value>, ApiError> {
    let keys: Vec<&str> = input.keys().map(String::as_str).collect();
    match validate(input, fields, messages, &keys) {
        Ok(()) => Ok(Map::new()),
        Err(ApiError::Validation(m)) => Ok(m),
        Err(e) => Err(e),
    }
}

/// Merges an error into `errors`, keeping `order` for the field order Laravel reports.
pub fn ordered(errors: Map<String, Value>, order: &[&str]) -> Map<String, Value> {
    let mut out = Map::new();
    for k in order {
        if let Some(v) = errors.get(*k) {
            out.insert((*k).to_string(), v.clone());
        }
    }
    for (k, v) in errors {
        if !out.contains_key(&k) {
            out.insert(k, v);
        }
    }
    out
}

/// The window a key is limited over (keys use different windows), and its hits.
type Hits = (Duration, Vec<Instant>);

/// Hits per key, and when keys whose window has passed were last dropped.
struct Limiter {
    keys: HashMap<String, Hits>,
    pruned: Instant,
}

impl Limiter {
    fn new() -> Self {
        Self {
            keys: HashMap::new(),
            pruned: Instant::now(),
        }
    }

    /// Drops every key whose own window has passed.
    fn prune(&mut self, now: Instant) {
        self.keys
            .retain(|_, (w, v)| v.last().is_some_and(|t| now.duration_since(*t) < *w));
        self.pruned = now;
    }

    /// Records a hit for `key`; the seconds to wait when over `max` in `window`.
    fn hit(&mut self, key: &str, max: usize, window: Duration, now: Instant) -> Option<u64> {
        if now.duration_since(self.pruned) >= PRUNE_EVERY {
            self.prune(now);
        }
        if self.keys.len() >= MAX_KEYS && !self.keys.contains_key(key) {
            self.prune(now);
            if self.keys.len() >= MAX_KEYS
                && let Some(any) = self.keys.keys().next().cloned()
            {
                self.keys.remove(&any);
            }
        }
        let (w, v) = self
            .keys
            .entry(key.to_string())
            .or_insert_with(|| (window, Vec::new()));
        *w = (*w).max(window);
        v.retain(|t| now.duration_since(*t) < window);
        if v.len() >= max {
            let oldest = v.iter().min().copied().unwrap_or(now);
            return Some(
                window
                    .saturating_sub(now.duration_since(oldest))
                    .as_secs()
                    .max(1),
            );
        }
        v.push(now);
        None
    }
}

static HITS: LazyLock<Mutex<Limiter>> = LazyLock::new(|| Mutex::new(Limiter::new()));

/// Keys kept at most; past it, keys whose own window has passed are dropped, then any key.
const MAX_KEYS: usize = 50_000;

/// Keys whose window has passed are dropped at least this often, whatever the key count.
const PRUNE_EVERY: Duration = Duration::from_secs(60);

/// `RateLimiter`: records a hit; the seconds to wait when over `max` in `window`.
pub fn throttle(key: &str, max: usize, window: Duration) -> Option<u64> {
    HITS.lock().ok()?.hit(key, max, window, Instant::now())
}

/// `RateLimiter::tooManyAttempts`: whether `key` is over `max`, without a hit.
pub fn too_many(key: &str, max: usize, window: Duration) -> Option<u64> {
    let now = Instant::now();
    let m = HITS.lock().ok()?;
    let (_, v) = m.keys.get(key)?;
    let live: Vec<&Instant> = v
        .iter()
        .filter(|t| now.duration_since(**t) < window)
        .collect();
    if live.len() < max {
        return None;
    }
    let oldest = live.iter().min().map_or(now, |t| **t);
    Some(
        window
            .saturating_sub(now.duration_since(oldest))
            .as_secs()
            .max(1),
    )
}

/// `normalize_email_identity()`.
pub fn normalize_email_identity(email: Option<&str>) -> Option<String> {
    let email = email?.trim().to_lowercase();
    let (local, domain) = email.split_once('@')?;
    let local = if matches!(domain, "gmail.com" | "googlemail.com") {
        local.split('+').next().unwrap_or_default().replace('.', "")
    } else {
        local.to_string()
    };
    if local.trim().is_empty() || domain.trim().is_empty() {
        return None;
    }
    Some(format!("{local}@{domain}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn email_identities() {
        assert_eq!(
            normalize_email_identity(Some(" A.B+c@GMail.com ")).as_deref(),
            Some("ab@gmail.com")
        );
        assert_eq!(
            normalize_email_identity(Some("a.b+c@example.test")).as_deref(),
            Some("a.b+c@example.test")
        );
        assert_eq!(normalize_email_identity(Some("nope")), None);
        assert_eq!(normalize_email_identity(None), None);
    }

    #[test]
    fn emails() {
        assert!(is_email("a@b.co"));
        assert!(is_email("a.b+c@localhost"));
        assert!(!is_email("a@"));
        assert!(!is_email("a b@c.d"));
        assert!(!is_email("nope"));
        assert!(!is_email("a..b@c.d"));
    }

    #[test]
    fn hostnames() {
        assert_eq!(valid_hostname(&json!("mail.example.com")), None);
        assert!(valid_hostname(&json!("a b")).is_some());
        assert!(valid_hostname(&json!("-a.b")).is_some());
    }

    #[test]
    fn throttling() {
        let w = Duration::from_secs(60);
        assert!(throttle("t-key", 2, w).is_none());
        assert!(throttle("t-key", 2, w).is_none());
        assert!(throttle("t-key", 2, w).is_some());
    }

    #[test]
    fn expired_keys_are_dropped_on_a_timer_below_the_cap() {
        let mut l = Limiter::new();
        let start = Instant::now();
        let short = Duration::from_secs(5);
        for i in 0..100 {
            l.hit(&format!("k{i}"), 1, short, start);
        }
        // Still inside the prune interval: expired keys stay until the next prune.
        let soon = start + Duration::from_secs(10);
        l.hit("other", 1, short, soon);
        assert_eq!(l.keys.len(), 101);
        // Once the interval has passed, the next hit drops every expired key.
        let later = start + PRUNE_EVERY;
        l.hit("fresh", 1, short, later);
        assert_eq!(l.keys.len(), 1);
        assert!(l.keys.contains_key("fresh"));
    }
}
