//! `get_service_templates()` and friends (bootstrap/helpers/shared.php).
//!
//! Upstream reads `templates/service-templates-latest.json` from the application directory
//! (`config('constants.services.file_name')`), which its image ships and `PullTemplatesFromCDN`
//! replaces. coolify-rs embeds the same file and reads `COOLIFY_RS_TEMPLATES_DIR` first, where the
//! Docker image installs it, so a newer file can replace it without a rebuild.

use std::collections::BTreeMap;
use std::sync::LazyLock;

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

static TEMPLATES: LazyLock<BTreeMap<String, Value>> = LazyLock::new(|| {
    if let Ok(dir) = std::env::var("COOLIFY_RS_TEMPLATES_DIR")
        && let Ok(text) = std::fs::read_to_string(std::path::Path::new(&dir).join(FILE_NAME))
    {
        let parsed = parse(&text);
        if !parsed.is_empty() {
            return parsed;
        }
    }
    parse(EMBEDDED)
});

/// `get_service_templates()`: sorted by key (`sortKeys()`).
pub fn get_service_templates() -> &'static BTreeMap<String, Value> {
    &TEMPLATES
}

/// `serviceKeys()`.
pub fn service_keys() -> Vec<String> {
    TEMPLATES.keys().cloned().collect()
}

/// `resolve_service_template_key($type)`: a renamed template keeps its old key as an alias.
pub fn resolve_service_template_key(kind: &str) -> String {
    if kind == "denoKV" && !TEMPLATES.contains_key(kind) {
        return "deno-kv".to_owned();
    }
    kind.to_owned()
}

fn field<'a>(kind: &str, name: &str) -> Option<&'a str> {
    TEMPLATES.get(kind)?.get(name)?.as_str()
}

fn decode(encoded: &str) -> String {
    base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// `base64_decode(data_get($services, "$type.compose"))`.
pub fn template_compose(kind: &str) -> Option<String> {
    field(kind, "compose")
        .filter(|c| !c.is_empty())
        .map(decode)
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
    let port = TEMPLATES.get(&kind)?.get("port")?;
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
