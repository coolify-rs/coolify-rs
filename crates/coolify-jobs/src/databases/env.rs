//! Runtime environment variables of a database as its start action uses them:
//! the `EnvironmentVariable` value accessor (decrypt, trim), shared variable
//! references (`get_real_environment_variables_internal`) and the formatting
//! rules of `HasSecretManager` (exact or legacy escaping).
//!
//! Secret managers (`{{vault.KEY}}` references) are not ported: a variable that
//! uses one stops the start with a "not available yet" message.

use std::sync::LazyLock;

use coolify_db::databases::DatabaseRow;
use coolify_db::environment_variables as vars;
use regex::Regex;
use serde_json::Value;

use super::StartContext;
use super::common::escape_dollar;
use coolify_compat::Encrypter;
use coolify_db::Db;

pub const SECRET_MANAGER_NOT_PORTED: &str =
    "Environment variables that read a secret manager are not available in coolify-rs yet.";

/// One runtime variable, decrypted.
#[derive(Clone)]
pub struct Var {
    pub id: i64,
    pub key: String,
    /// The stored value (the accessor's `trim(decrypt($value))`).
    pub value: Option<String>,
    /// The value with shared variable references replaced.
    pub resolved: Option<String>,
    pub is_literal: bool,
    pub is_multiline: bool,
    pub is_shared: bool,
    pub is_shown_once: bool,
    pub uses_legacy_escaping: bool,
    /// The stored ciphertext (upstream's configuration hash sorts by it).
    pub cipher: Option<String>,
}

impl std::fmt::Debug for Var {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Var")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

/// PHP `json_validate($v)` and the value starts like an object or array.
fn is_json_container(v: &str, trim_left: bool) -> bool {
    let start = if trim_left { v.trim_start() } else { v };
    (start.starts_with('{') || start.starts_with('[')) && serde_json::from_str::<Value>(v).is_ok()
}

/// `escapeEnvVariables`.
pub fn escape_env_variables(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
        .replace('\0', "\\0")
        .replace('"', "\\\"")
        .replace('\'', "\\'")
}

impl Var {
    /// `useExactEscaping`.
    pub fn exact(&self) -> bool {
        !self.uses_legacy_escaping
    }

    /// `environmentVariableAllowsInterpolation` (no secret managers here).
    pub fn allows_interpolation(&self, value: &str) -> bool {
        !is_json_container(value, true) && !self.is_literal && !self.is_multiline
    }

    /// `legacyFormatEnvironmentVariableValue`.
    fn legacy(&self, value: &str) -> String {
        if is_json_container(value, false) {
            return value.to_owned();
        }
        if self.is_literal || self.is_multiline {
            format!("'{value}'")
        } else {
            escape_env_variables(value)
        }
    }

    /// `formatEnvironmentVariableValue` for a local value.
    pub fn format(&self, value: Option<&str>) -> Option<String> {
        let value = value?;
        if !self.exact() {
            return Some(self.legacy(value));
        }
        Some(if self.allows_interpolation(value) {
            value.to_owned()
        } else {
            escape_dollar(value)
        })
    }

    /// `resolveSecretManagerEnvironmentVariable`: the value the container line gets.
    pub fn container_value(&self) -> Option<String> {
        self.format(self.resolved.as_deref())
    }

    /// `composeCommandValue`.
    pub fn compose_command_value(&self, value: &str) -> String {
        if !self.exact() || self.allows_interpolation(value) {
            value.to_owned()
        } else {
            escape_dollar(value)
        }
    }

    /// `formatComposeFileValue`; `redis_like` for Redis, KeyDB and Dragonfly.
    pub fn compose_file_value(&self, value: &str, redis_like: bool) -> String {
        if self.key == "REDIS_PASSWORD" && self.exact() && redis_like {
            return escape_dollar(value);
        }
        self.compose_command_value(value)
    }
}

static REFERENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{(.*?)\}\}").unwrap_or_else(|_| unreachable!()));
static VAULT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{\{\s*vault\.([A-Za-z0-9_]+)\s*\}\}").unwrap_or_else(|_| unreachable!())
});
const TYPES: &[&str] = &["team", "project", "environment", "server"];

/// `get_real_environment_variables_internal`.
async fn resolve_references(
    db: &Db,
    enc: &Encrypter,
    d: &DatabaseRow,
    server_id: Option<i64>,
    value: &str,
) -> Result<String, String> {
    let mut out = value.trim().to_owned();
    let found: Vec<String> = REFERENCE
        .captures_iter(&out.clone())
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_owned()))
        .collect();
    for inner in found {
        let trimmed = inner.trim();
        let Some((kind, key)) = trimmed.split_once('.') else {
            continue;
        };
        if !TYPES.contains(&kind) {
            continue;
        }
        let owner = match kind {
            "environment" => d.environment.get("id").and_then(Value::as_i64),
            "project" => d.project.get("id").and_then(Value::as_i64),
            "team" => Some(d.team_id),
            _ => server_id,
        };
        let Some(owner) = owner else { continue };
        let shared = vars::shared(db, d.team_id, kind, key, owner)
            .await
            .map_err(|e| e.to_string())?;
        if let Some((Some(cipher), _shown_once)) = shared {
            let plain = enc
                .decrypt_string(&cipher)
                .map_err(|_| "The MAC is invalid.".to_owned())?;
            out = out.replace(&format!("{{{{{inner}}}}}"), &plain);
        }
    }
    Ok(out)
}

/// The database's `environment_variables`, decrypted (the value accessor), not resolved.
pub async fn load_stored(db: &Db, enc: &Encrypter, d: &DatabaseRow) -> Result<Vec<Var>, String> {
    let rows = vars::for_resource(db, d.kind.morph(), d.id)
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let value = match &r.value_encrypted {
            Some(c) if !c.is_empty() => {
                // The value accessor overrides the `encrypted` cast: one `encrypt()` layer (serialized).
                let plain = enc
                    .decrypt_maybe_serialized(c)
                    .map_err(|_| "The MAC is invalid.".to_owned())?;
                Some(plain.trim().to_owned())
            }
            _ => None,
        };
        out.push(Var {
            id: r.id,
            key: r.key,
            value,
            resolved: None,
            is_literal: r.is_literal,
            is_multiline: r.is_multiline,
            is_shared: r.is_shared,
            is_shown_once: r.is_shown_once,
            uses_legacy_escaping: r.uses_legacy_escaping,
            cipher: r.value_encrypted,
        });
    }
    Ok(out)
}

/// The database's `runtime_environment_variables`, decrypted and resolved.
pub async fn load(
    db: &Db,
    enc: &Encrypter,
    d: &DatabaseRow,
    server_id: Option<i64>,
) -> Result<Vec<Var>, String> {
    let mut out = load_stored(db, enc, d).await?;
    for v in &mut out {
        if v.value.as_deref().is_some_and(|x| VAULT.is_match(x)) {
            return Err(SECRET_MANAGER_NOT_PORTED.into());
        }
        v.resolved = match &v.value {
            Some(x) if !x.is_empty() => Some(resolve_references(db, enc, d, server_id, x).await?),
            other => other.clone(),
        };
    }
    Ok(out)
}

/// Whether a `KEY=value` list already defines the exact key.
pub fn has_key(list: &[String], key: &str) -> bool {
    list.iter().any(|v| v.split('=').next() == Some(key))
}

/// `addCoolifyEnvironmentVariables`: a key the user defines wins.
pub fn add_coolify_variables(list: &mut Vec<String>, ctx: &StartContext<'_>) {
    let d = ctx.db;
    let defined: Vec<String> = list
        .iter()
        .map(|v| v.split('=').next().unwrap_or_default().to_owned())
        .collect();
    let mut add: Vec<(&str, String)> = vec![
        ("COOLIFY_RESOURCE_UUID", d.uuid.clone()),
        ("COOLIFY_CONTAINER_NAME", d.uuid.clone()),
    ];
    if d.raw
        .get("include_coolify_default_variables")
        .and_then(Value::as_bool)
        != Some(false)
    {
        let name = |r: &coolify_db::records::Record| {
            r.get("name").and_then(Value::as_str).map(str::to_owned)
        };
        for (k, v) in [
            ("COOLIFY_APP_NAME", d.str("name").map(str::to_owned)),
            ("COOLIFY_PROJECT_NAME", name(&d.project)),
            ("COOLIFY_ENVIRONMENT_NAME", name(&d.environment)),
            ("COOLIFY_SERVER_IP", Some(ctx.server.ip.clone())),
        ] {
            if let Some(v) = v {
                add.push((k, v));
            }
        }
    }
    for (k, v) in add {
        if !defined.iter().any(|d| d == k) {
            list.push(format!("{k}={}", escape_dollar(&v)));
        }
    }
}

static SHARED_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)^\{\{\s*(?:team|project|environment|server)\..*\}\}$")
        .unwrap_or_else(|_| unreachable!())
});

/// The `json_encode(... ->get('value')->makeVisible('value')->sort())` part of
/// `isConfigurationChanged`: models sorted by their ciphertext (`asort`, SORT_REGULAR),
/// keys kept, so a changed order encodes as an object. Each model carries its appended
/// attributes; with only `value` loaded all but `is_shared` are false (`real_value` is hidden).
pub fn config_hash_values(variables: &[Var]) -> String {
    let mut idx: Vec<usize> = (0..variables.len()).collect();
    idx.sort_by(|a, b| variables[*a].cipher.cmp(&variables[*b].cipher));
    let value = |i: usize| {
        let v = &variables[i].value;
        let shared = v
            .as_deref()
            .is_some_and(|x| SHARED_REFERENCE.is_match(x.trim()));
        serde_json::json!({
            "value": v,
            "is_shared": shared,
            "is_really_required": false,
            "is_buildpack_control": false,
            "is_coolify": false,
        })
    };
    let in_order = idx.iter().enumerate().all(|(pos, i)| pos == *i);
    let encoded = if in_order {
        Value::Array(idx.iter().map(|i| value(*i)).collect())
    } else {
        Value::Object(idx.iter().map(|i| (i.to_string(), value(*i))).collect())
    };
    coolify_compat::php_json::encode(&encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(literal: bool, legacy: bool) -> Var {
        Var {
            id: 1,
            key: "K".into(),
            value: None,
            resolved: None,
            is_literal: literal,
            is_multiline: false,
            is_shared: false,
            is_shown_once: false,
            uses_legacy_escaping: legacy,
            cipher: None,
        }
    }

    #[test]
    fn formatting() {
        assert_eq!(
            var(false, false).format(Some("a$b")).as_deref(),
            Some("a$b")
        );
        assert_eq!(
            var(true, false).format(Some("a$b")).as_deref(),
            Some("a$$b")
        );
        assert_eq!(
            var(false, true).format(Some(r#"a"b'c"#)).as_deref(),
            Some(r#"a\"b\'c"#)
        );
        assert_eq!(var(true, true).format(Some("x")).as_deref(), Some("'x'"));
        assert_eq!(
            var(true, true).format(Some(r#"{"a":1}"#)).as_deref(),
            Some(r#"{"a":1}"#)
        );
        assert_eq!(var(false, false).compose_file_value("p$w", true), "p$w");
        let mut password = var(false, false);
        password.key = "REDIS_PASSWORD".into();
        assert_eq!(password.compose_file_value("p$w", true), "p$$w");
        assert_eq!(password.compose_file_value("p$w", false), "p$w");
    }
}
