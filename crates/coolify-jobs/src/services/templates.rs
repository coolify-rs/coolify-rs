//! `get_service_templates()` and friends (bootstrap/helpers/shared.php).
//!
//! Upstream reads `templates/service-templates-latest.json` from the application directory
//! (`config('constants.services.file_name')`), which its image ships and `PullTemplatesFromCDN`
//! replaces. coolify-rs embeds the same file and reads `COOLIFY_RS_TEMPLATES_DIR` first, where the
//! Docker image installs it, so a newer file can replace it without a rebuild.

use std::collections::BTreeMap;
use std::sync::{LazyLock, RwLock};

use base64::Engine as _;
use serde_json::Value;

const EMBEDDED: &str = include_str!("../../../../templates/service-templates-latest.json");
const FILE_NAME: &str = "service-templates-latest.json";

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
        parse(EMBEDDED)
    } else {
        parsed
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

    #[test]
    fn embedded_templates_load() {
        assert!(get_service_templates().len() > 300);
        assert!(template_compose("uptime-kuma").is_some());
        assert_eq!(resolve_service_template_key("denoKV"), "deno-kv");
    }
}
