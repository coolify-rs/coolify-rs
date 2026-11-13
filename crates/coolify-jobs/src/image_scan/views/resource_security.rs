//! `App\Services\ImageScan\ResourceSecurity`: the security status of one image or one resource
//! (spec 15.6), with what the main card shows (15.7).
//!
//! Counts follow the status the actions give (the status without the check reasons):
//! - update_now: `problem_count` is the relevant findings with an exploit signal that have an
//!   action, `fixed_by_primary` how many of them the primary action fixes, `any_exploited`
//!   whether one of them is `exploited`.
//! - update_available: `problem_count` is the relevant critical, high, and medium findings that
//!   have an action (`critical_count` + `high_count` + `medium_count`), `fixed_by_primary` how
//!   many of them the primary action fixes.
//! - no_fix: `problem_count` is the relevant findings with an exploit signal without an action,
//!   `any_exploited` whether one of them is `exploited`.
//!
//! `exploited_count` is all relevant findings with an exploit signal, with or without an action.
//! An action that leaves an end-of-life OS counts it as one critical relevant fix; `eol.fixed`
//! says so. It is not in `why` or `more_fixed`.
//!
//! `builder_image`: every no_fix finding is an OS package with a fix in an image that a builder
//! (Nixpacks, Railpack, static) builds, so a builder update fixes it.
//!
//! `checked_at` is the time of the oldest current scan of the resource (`None` when one has no
//! time); `stale` uses it.
//!
//! `check_reason` is the check_needed reason (`images_unchecked` {checked, total}, `stale`
//! {time}, `db_outdated`, `image_changed`, `versions_unchecked`, `truncated`; context in
//! `check_context`), or the not_checked reason (`server`, `application` for scanning off,
//! `not_scanned`).
//!
//! `facts` is what `SecurityStatus` needs to combine the statuses of several images without the
//! findings: the status the actions give, the check reasons that do not depend on the time,
//! counts, and per action (in the order of `actions`) what it fixes and its why-list.

use chrono::NaiveDateTime;
use serde_json::{Map, Value, json};

use super::super::text;
use super::action::{SecurityAction, php_bool, php_int, value_string};
use super::{SecurityStatusType, iso8601, parse_time};

/// One row of a why-list (`SecurityStatus::whyFor()`).
#[derive(Debug, Clone, PartialEq)]
pub struct Why {
    pub title: String,
    pub display_id: String,
    pub link: Option<String>,
    pub score: Option<f64>,
    pub severity: String,
    /// `exploited`, `likely`, or none.
    pub signal: Option<String>,
    pub package: String,
    pub version: String,
}

impl Why {
    pub fn to_value(&self) -> Value {
        json!({
            "title": self.title,
            "display_id": self.display_id,
            "link": self.link,
            "score": self.score.and_then(serde_json::Number::from_f64),
            "severity": self.severity,
            "signal": self.signal,
            "package": self.package,
            "version": self.version,
        })
    }

    /// `ResourceSecurity::whyEntry($entry)`: a stored why row, its title cleaned for display.
    pub fn from_value(entry: &Value) -> Self {
        let empty = Map::new();
        let entry = entry.as_object().unwrap_or(&empty);
        let string = |key: &str| entry.get(key).map(value_string).unwrap_or_default();
        let isset = |key: &str| entry.get(key).filter(|v| !v.is_null()).map(value_string);
        let severity = entry
            .get("severity")
            .filter(|v| !v.is_null())
            .map_or_else(|| "unknown".to_owned(), value_string);
        let package = string("package");
        let version = string("version");
        Self {
            title: text::stored_title(&string("title"), &package, &version, &severity),
            display_id: string("display_id"),
            link: isset("link"),
            score: numeric(entry.get("score")),
            severity,
            signal: isset("signal"),
            package,
            version,
        }
    }

    /// The row the notifications keep.
    pub fn to_notify(&self) -> super::super::notify::WhyEntry {
        super::super::notify::WhyEntry {
            title: self.title.clone(),
            display_id: self.display_id.clone(),
            link: self.link.clone(),
            score: self.score,
            severity: self.severity.clone(),
            signal: self.signal.clone(),
        }
    }
}

/// `is_numeric($value) ? (float) $value : null`.
pub(crate) fn numeric(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) if coolify_compat::yaml::php_is_numeric(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// The end-of-life OS of an image, and whether an action leaves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Eol {
    pub name: String,
    pub version: String,
    pub fixed: bool,
}

impl Eol {
    pub fn to_value(&self) -> Value {
        json!({"name": self.name, "version": self.version, "fixed": self.fixed})
    }
}

/// The status.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceSecurity {
    pub status: SecurityStatusType,
    pub check_reason: Option<String>,
    pub check_context: Map<String, Value>,
    /// The primary action first.
    pub actions: Vec<SecurityAction>,
    pub why: Vec<Why>,
    pub more_fixed: i64,
    pub exploited_without_fix: bool,
    pub problem_count: i64,
    pub exploited_count: i64,
    pub any_exploited: bool,
    pub critical_count: i64,
    pub high_count: i64,
    pub fixed_by_primary: i64,
    pub primary_critical: i64,
    pub primary_high: i64,
    pub eol: Option<Eol>,
    pub checked_at: Option<NaiveDateTime>,
    pub facts: Map<String, Value>,
    pub builder_image: bool,
    pub medium_count: i64,
    pub primary_medium: i64,
}

impl ResourceSecurity {
    /// `new ResourceSecurity($status)` with every other field at its default.
    pub fn new(status: SecurityStatusType) -> Self {
        Self {
            status,
            check_reason: None,
            check_context: Map::new(),
            actions: Vec::new(),
            why: Vec::new(),
            more_fixed: 0,
            exploited_without_fix: false,
            problem_count: 0,
            exploited_count: 0,
            any_exploited: false,
            critical_count: 0,
            high_count: 0,
            fixed_by_primary: 0,
            primary_critical: 0,
            primary_high: 0,
            eol: None,
            checked_at: None,
            facts: Map::new(),
            builder_image: false,
            medium_count: 0,
            primary_medium: 0,
        }
    }

    /// `new ResourceSecurity(status: NotChecked, checkReason: $reason)`.
    pub fn not_checked(reason: &str) -> Self {
        let mut security = Self::new(SecurityStatusType::NotChecked);
        security.check_reason = Some(reason.to_owned());
        security
    }

    /// `primary()`: the first action.
    pub fn primary(&self) -> Option<&SecurityAction> {
        self.actions.first()
    }

    /// `data_get($security->facts, $path)` with a dot path (`action_stats.0.eol`).
    pub fn fact(&self, path: &str) -> Option<&Value> {
        let mut parts = path.split('.');
        let mut current = self.facts.get(parts.next()?)?;
        for part in parts {
            current = match current {
                Value::Object(map) => map.get(part)?,
                Value::Array(list) => list.get(part.parse::<usize>().ok()?)?,
                _ => return None,
            };
        }
        Some(current)
    }

    /// `(int) data_get($security->facts, $path, 0)`.
    pub fn fact_int(&self, path: &str) -> i64 {
        php_int(self.fact(path))
    }

    /// `(array) data_get($security->facts, "action_stats.{$index}", [])`.
    pub fn action_stats(&self, index: usize) -> Map<String, Value> {
        match self.fact(&format!("action_stats.{index}")) {
            Some(Value::Object(map)) => map.clone(),
            _ => Map::new(),
        }
    }

    /// `toArray()`, in upstream's key order. An empty `check_context` or `facts` is written as
    /// `[]`, as PHP writes an empty array.
    pub fn to_value(&self) -> Value {
        json!({
            "status": self.status.value(),
            "check_reason": self.check_reason,
            "check_context": php_map(&self.check_context),
            "actions": self.actions.iter().map(SecurityAction::to_value).collect::<Vec<_>>(),
            "why": self.why.iter().map(Why::to_value).collect::<Vec<_>>(),
            "more_fixed": self.more_fixed,
            "exploited_without_fix": self.exploited_without_fix,
            "problem_count": self.problem_count,
            "exploited_count": self.exploited_count,
            "any_exploited": self.any_exploited,
            "critical_count": self.critical_count,
            "high_count": self.high_count,
            "medium_count": self.medium_count,
            "fixed_by_primary": self.fixed_by_primary,
            "primary_critical": self.primary_critical,
            "primary_high": self.primary_high,
            "primary_medium": self.primary_medium,
            "eol": self.eol.as_ref().map(Eol::to_value),
            "checked_at": self.checked_at.map(iso8601),
            "facts": php_map(&self.facts),
            "builder_image": self.builder_image,
        })
    }

    /// `ResourceSecurity::fromArray($data)`: a stored status. Actions that cannot be read are
    /// left out, with their entry of `facts.action_stats`.
    pub fn from_value(data: &Value) -> Self {
        let empty = Map::new();
        let data = data.as_object().unwrap_or(&empty);
        let mut facts = as_map(data.get("facts"));
        let stats = list(facts.get("action_stats"));
        let mut actions = Vec::new();
        let mut action_stats = Vec::new();
        for (index, action_data) in list(data.get("actions")).iter().enumerate() {
            if let Some(action) = SecurityAction::from_value(action_data) {
                actions.push(action);
                action_stats.push(
                    stats
                        .get(index)
                        .cloned()
                        .unwrap_or(Value::Array(Vec::new())),
                );
            }
        }
        if facts.contains_key("action_stats") {
            facts.insert("action_stats".to_owned(), Value::Array(action_stats));
        }
        let int = |key: &str| php_int(data.get(key));
        let flag = |key: &str| php_bool(data.get(key));
        let eol = match data.get("eol") {
            Some(Value::Object(eol)) => Some(Eol {
                name: eol.get("name").map(value_string).unwrap_or_default(),
                version: eol.get("version").map(value_string).unwrap_or_default(),
                fixed: php_bool(eol.get("fixed")),
            }),
            _ => None,
        };
        Self {
            status: data
                .get("status")
                .map(value_string)
                .and_then(|s| SecurityStatusType::from_value(&s))
                .unwrap_or(SecurityStatusType::NotChecked),
            check_reason: data
                .get("check_reason")
                .filter(|v| !v.is_null())
                .map(value_string),
            check_context: as_map(data.get("check_context")),
            actions,
            why: list(data.get("why")).iter().map(Why::from_value).collect(),
            more_fixed: int("more_fixed"),
            exploited_without_fix: flag("exploited_without_fix"),
            problem_count: int("problem_count"),
            exploited_count: int("exploited_count"),
            any_exploited: flag("any_exploited"),
            critical_count: int("critical_count"),
            high_count: int("high_count"),
            fixed_by_primary: int("fixed_by_primary"),
            primary_critical: int("primary_critical"),
            primary_high: int("primary_high"),
            eol,
            checked_at: data
                .get("checked_at")
                .and_then(Value::as_str)
                .and_then(parse_time),
            facts,
            builder_image: flag("builder_image"),
            medium_count: int("medium_count"),
            primary_medium: int("primary_medium"),
        }
    }
}

/// A map as PHP writes an array: `[]` when it is empty.
pub(crate) fn php_map(map: &Map<String, Value>) -> Value {
    if map.is_empty() {
        Value::Array(Vec::new())
    } else {
        Value::Object(map.clone())
    }
}

/// `(array) $value` as a string-keyed map; a list becomes a map keyed by index.
pub(crate) fn as_map(value: Option<&Value>) -> Map<String, Value> {
    match value {
        Some(Value::Object(map)) => map.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v.clone()))
            .collect(),
        _ => Map::new(),
    }
}

/// `array_values((array) $value)`.
pub(crate) fn list(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::Array(items)) => items.clone(),
        Some(Value::Object(map)) => map.values().cloned().collect(),
        None | Some(Value::Null) => Vec::new(),
        Some(scalar) => vec![scalar.clone()],
    }
}

#[cfg(test)]
mod tests {
    use super::super::SecurityActionType;
    use super::*;

    #[test]
    fn a_status_round_trips() {
        let mut security = ResourceSecurity::new(SecurityStatusType::UpdateNow);
        security.actions = vec![SecurityAction {
            action_type: SecurityActionType::PullLatest,
            sentence: "Pull the newer build of nginx:1.27 and restart.".to_owned(),
            target: Some("1.27".to_owned()),
            copy: None,
            fixed_ids: vec!["CVE-1".to_owned()],
            button: true,
            image: Some("nginx:1.27".to_owned()),
            reference: Some("nginx:1.27".to_owned()),
            parts: Vec::new(),
        }];
        security.problem_count = 2;
        security.eol = Some(Eol {
            name: "debian".to_owned(),
            version: "10".to_owned(),
            fixed: true,
        });
        security.checked_at = super::super::parse_time("2026-11-10T12:00:00+00:00");
        security
            .facts
            .insert("action_stats".to_owned(), json!([{"signal": 2, "why": []}]));
        let value = security.to_value();
        assert_eq!(value["check_context"], json!([]));
        assert_eq!(value["checked_at"], json!("2026-11-10T12:00:00+00:00"));
        assert_eq!(ResourceSecurity::from_value(&value), security);
        assert_eq!(security.fact_int("action_stats.0.signal"), 2);
    }

    #[test]
    fn unreadable_actions_drop_their_stats() {
        let stored = json!({
            "status": "update_available",
            "actions": [{"type": "bad"}, {"type": "rebuild", "sentence": "Rebuild."}],
            "facts": {"action_stats": [{"relevant": 1}, {"relevant": 2}]},
        });
        let security = ResourceSecurity::from_value(&stored);
        assert_eq!(security.actions.len(), 1);
        assert_eq!(security.fact_int("action_stats.0.relevant"), 2);
        assert_eq!(
            ResourceSecurity::from_value(&json!({"status": "nope"})).status,
            SecurityStatusType::NotChecked
        );
    }

    #[test]
    fn why_rows_get_a_title() {
        let why = Why::from_value(
            &json!({"display_id": "CVE-1", "package": "openssl", "version": "3.0.1", "severity": "high", "score": "7.5"}),
        );
        assert_eq!(why.score, Some(7.5));
        assert!(!why.title.is_empty());
        assert_eq!(why.link, None);
    }
}
