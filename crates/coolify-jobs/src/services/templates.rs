//! `get_service_templates()` and friends (bootstrap/helpers/shared.php).
//!
//! Upstream reads `templates/service-templates-latest.json` from the application directory
//! (`config('constants.services.file_name')`), which its image ships and `PullTemplatesFromCDN`
//! replaces. coolify-rs embeds the same file and reads `COOLIFY_RS_TEMPLATES_DIR` first, where the
//! Docker image installs it, so a newer file can replace it without a rebuild.
//!
//! coolify-rs can ship a template newer than upstream's list (a later `template_last_updated_at`).
//! Such a built-in entry wins over a stored or downloaded bundle, so a pull from upstream's CDN does
//! not bring the older version back; every other template follows upstream.

use std::collections::BTreeMap;
use std::sync::{LazyLock, RwLock};

use base64::Engine as _;
use serde_json::Value;

const EMBEDDED: &str = include_str!("../../../../templates/service-templates-latest.json");
const FILE_NAME: &str = "service-templates-latest.json";

/// The templates this build ships.
static BUILTIN: LazyLock<BTreeMap<String, Value>> = LazyLock::new(|| parse(EMBEDDED));

/// `NEEDS_TO_CONNECT_TO_PREDEFINED_NETWORK`.
pub const NEEDS_TO_CONNECT_TO_PREDEFINED_NETWORK: [&str; 3] =
    ["pgadmin", "databasus", "redis-insight"];

fn parse(text: &str) -> BTreeMap<String, Value> {
    serde_json::from_str(text).unwrap_or_default()
}

/// `service_templates_path()`.
fn templates_path() -> std::path::PathBuf {
    std::env::var_os("COOLIFY_RS_TEMPLATES_DIR")
        .map_or_else(crate::kernel::data_dir, std::path::PathBuf::from)
        .join(FILE_NAME)
}

/// The current bundle. A bundle lives as long as the process, so readers keep `&'static` references;
/// one is only added when `PullTemplatesFromCDN` brings a different file.
static TEMPLATES: LazyLock<RwLock<&'static BTreeMap<String, Value>>> = LazyLock::new(|| {
    let parsed = std::fs::read_to_string(templates_path())
        .map(|text| parse(&text))
        .unwrap_or_default();
    let bundle = if parsed.is_empty() {
        BUILTIN.clone()
    } else {
        with_newer_builtin(parsed, &BUILTIN)
    };
    RwLock::new(Box::leak(Box::new(bundle)))
});

fn current() -> &'static BTreeMap<String, Value> {
    *TEMPLATES.read().unwrap_or_else(|p| p.into_inner())
}

/// `store_service_templates_bundle($json)`: a 200 response can still be an empty body or an error
/// page, which keeps the current bundle. Whether the file was written.
pub fn store_bundle(json: &str) -> bool {
    let parsed = parse(json);
    if parsed.is_empty() {
        return false;
    }
    let parsed = with_newer_builtin(parsed, &BUILTIN);
    let path = templates_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let written = std::fs::write(&path, json).is_ok();
    let mut slot = TEMPLATES.write().unwrap_or_else(|p| p.into_inner());
    if **slot != parsed {
        *slot = Box::leak(Box::new(parsed));
    }
    written
}

/// `template_last_updated_at` of a template entry.
fn updated_at(entry: &Value) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(entry.get("template_last_updated_at")?.as_str()?).ok()
}

/// The bundle with each template replaced by the built-in one when that is newer. A template only
/// one side has is left as the bundle has it (upstream may have removed or renamed it).
fn with_newer_builtin(
    mut bundle: BTreeMap<String, Value>,
    builtin: &BTreeMap<String, Value>,
) -> BTreeMap<String, Value> {
    for (key, ours) in builtin {
        if let Some(theirs) = bundle.get_mut(key)
            && let (Some(ours_at), Some(theirs_at)) = (updated_at(ours), updated_at(theirs))
            && ours_at > theirs_at
        {
            *theirs = ours.clone();
        }
    }
    bundle
}

/// `get_service_templates()`: sorted by key (`sortKeys()`).
pub fn get_service_templates() -> &'static BTreeMap<String, Value> {
    current()
}

/// `serviceKeys()`.
pub fn service_keys() -> Vec<String> {
    current().keys().cloned().collect()
}

/// `resolve_service_template_key($type)`: a renamed template keeps its old key as an alias.
pub fn resolve_service_template_key(kind: &str) -> String {
    if kind == "denoKV" && !current().contains_key(kind) {
        return "deno-kv".to_owned();
    }
    kind.to_owned()
}

fn field<'a>(kind: &str, name: &str) -> Option<&'a str> {
    current().get(kind)?.get(name)?.as_str()
}

fn decode(encoded: &str) -> String {
    base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// `base64_decode(data_get($services, "$type.compose"))`.
pub fn template_compose(kind: &str) -> Option<String> {
    field(kind, "compose").filter(|c| !c.is_empty()).map(decode)
}

/// `str(base64_decode(data_get($services, "$type.envs")))->split('/\r\n|\r|\n/')->filter(...)`.
pub fn template_envs(kind: &str) -> Vec<String> {
    let Some(envs) = field(kind, "envs").filter(|e| !e.is_empty()) else {
        return Vec::new();
    };
    decode(envs)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .filter(|v| !v.is_empty() && *v != "0")
        .map(str::to_owned)
        .collect()
}

/// `data_get($services, "$type.port")` as an int (`Service::getRequiredPort()`).
pub fn required_port(service_type: Option<&str>) -> Option<i64> {
    let kind = service_type.filter(|t| !t.trim().is_empty())?;
    let kind = resolve_service_template_key(kind);
    let port = current().get(&kind)?.get("port")?;
    match port {
        Value::Number(n) => n.as_i64().filter(|p| *p != 0),
        Value::String(s) => {
            if s.is_empty() || s == "0" {
                None
            } else {
                Some(s.trim().parse::<i64>().unwrap_or(0))
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(image: &str, updated: &str) -> Value {
        serde_json::json!({"compose": image, "template_last_updated_at": updated})
    }

    #[test]
    fn a_newer_builtin_template_wins_over_the_downloaded_one() {
        let builtin: BTreeMap<String, Value> = [
            (
                "umami".to_owned(),
                entry("3.5.0", "2026-11-14T03:23:45+05:00"),
            ),
            ("ghost".to_owned(), entry("5", "2025-01-01T00:00:00+00:00")),
            (
                "only-ours".to_owned(),
                entry("1", "2026-01-01T00:00:00+00:00"),
            ),
        ]
        .into();
        let downloaded: BTreeMap<String, Value> = [
            (
                "umami".to_owned(),
                entry("3.0.3", "2025-12-17T22:34:31+05:30"),
            ),
            ("ghost".to_owned(), entry("6", "2026-02-01T00:00:00+00:00")),
            (
                "only-theirs".to_owned(),
                entry("2", "2026-01-01T00:00:00+00:00"),
            ),
        ]
        .into();
        let merged = with_newer_builtin(downloaded, &builtin);
        // Ours is newer: kept. Theirs is newer: upstream's update applies.
        assert_eq!(merged["umami"]["compose"], "3.5.0");
        assert_eq!(merged["ghost"]["compose"], "6");
        // Templates only one side has are not added back.
        assert!(merged.contains_key("only-theirs"));
        assert!(!merged.contains_key("only-ours"));
    }

    #[test]
    fn a_template_without_a_readable_date_follows_the_bundle() {
        let builtin: BTreeMap<String, Value> =
            [("a".to_owned(), entry("ours", "2026-11-14T00:00:00+00:00"))].into();
        let downloaded: BTreeMap<String, Value> =
            [("a".to_owned(), serde_json::json!({"compose": "theirs"}))].into();
        assert_eq!(
            with_newer_builtin(downloaded, &builtin)["a"]["compose"],
            "theirs"
        );
    }

    #[test]
    fn embedded_templates_load() {
        assert!(get_service_templates().len() > 300);
        assert!(template_compose("uptime-kuma").is_some());
        assert_eq!(resolve_service_template_key("denoKV"), "deno-kv");
    }
}
