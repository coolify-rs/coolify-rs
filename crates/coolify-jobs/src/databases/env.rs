//! Runtime environment variables of a database as its start action uses them:
//! the `EnvironmentVariable` value accessor (decrypt, trim), shared variable
//! references (`get_real_environment_variables_internal`) and the formatting
//! rules of `HasSecretManager` (exact or legacy escaping, remote secret references).

use std::sync::LazyLock;

use crate::integrations::secret_managers;

use coolify_db::databases::DatabaseRow;
use coolify_db::environment_variables as vars;
use regex::Regex;
use serde_json::Value;

use super::StartContext;
use super::common::escape_dollar;
use coolify_compat::Encrypter;
use coolify_db::Db;

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
    pub is_runtime: bool,
    pub is_buildtime: bool,
    pub is_preview: bool,
    pub uses_legacy_escaping: bool,
    /// `environmentVariableUsesSecretManager`: `resolved` holds values of a secret manager.
    pub remote: bool,
    /// `preserve_compose_null`.
    pub preserve_compose_null: bool,
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

/// The rule of `legacyEnvFileLineIsUnreadable()` for a resolved value. `quoted` is whether the
/// variable is literal or multiline, which the old escaping puts in single quotes.
pub fn legacy_line_is_unreadable(value: &str, quoted: bool) -> bool {
    if is_json_container(value, false) {
        return value.contains('\n');
    }
    quoted && value.contains('\'')
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

/// `useExactEscaping()` of a service or an application (Coolify v4.6.0): a variable saved before
/// exact escaping keeps the old escaping, except when its old `.env` line is one Docker Compose
/// cannot read (`legacyEnvFileLineIsUnreadable()`), or when the resource parses its Compose file
/// and the variable is one whose stored value the old parser wrote into the container
/// (`dockerComposeVariablesWithStoredValueBeforeExactEscaping()`). Those switch to exact escaping
/// here, in memory: [`Var::exact`] then answers for the resource.
pub fn apply_resource_exact_escaping(
    vars: &mut [Var],
    parses_compose: bool,
    compose_raw: Option<&str>,
) {
    if !vars.iter().any(|v| v.uses_legacy_escaping) {
        return;
    }
    let stored_value_keys = if parses_compose {
        crate::compose_parser::legacy_escaping::variables_with_stored_value_before_exact_escaping(
            compose_raw,
        )
    } else {
        Vec::new()
    };
    for v in vars.iter_mut().filter(|v| v.uses_legacy_escaping) {
        if v.legacy_env_file_line_is_unreadable() || stored_value_keys.contains(&v.key) {
            v.uses_legacy_escaping = false;
        }
    }
}

impl Var {
    /// `useExactEscaping`. For a service or an application, after [`apply_resource_exact_escaping`].
    pub fn exact(&self) -> bool {
        !self.uses_legacy_escaping
    }

    /// `legacyEnvFileLineIsUnreadable()`: the old escaping writes JSON objects and arrays without
    /// quotes, so a JSON value with a line break breaks the whole env file. It puts other literal
    /// and multiline values in single quotes without escaping, and Docker Compose ends the value
    /// at the next quote.
    pub fn legacy_env_file_line_is_unreadable(&self) -> bool {
        self.resolved.as_deref().is_some_and(|value| {
            legacy_line_is_unreadable(value, self.is_literal || self.is_multiline)
        })
    }

    /// `environmentVariableAllowsInterpolation`.
    pub fn allows_interpolation(&self, value: &str) -> bool {
        !is_json_container(value, true) && !self.is_literal && !self.is_multiline && !self.remote
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
        if self.remote {
            return Some(secret_managers::compose_file(value));
        }
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
        if self.remote {
            return secret_managers::compose_file(value);
        }
        if self.key == "REDIS_PASSWORD" && self.exact() && redis_like {
            return escape_dollar(value);
        }
        self.compose_command_value(value)
    }
}

static REFERENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{(.*?)\}\}").unwrap_or_else(|_| unreachable!()));
const TYPES: &[&str] = &["team", "project", "environment", "server"];

/// The resource that owns variables, with the ids shared references resolve against.
#[derive(Debug, Clone, Copy)]
pub struct Owner<'a> {
    pub morph: &'a str,
    pub id: i64,
    pub team_id: i64,
    pub project_id: Option<i64>,
    pub environment_id: Option<i64>,
}

impl<'a> Owner<'a> {
    pub fn database(d: &'a DatabaseRow) -> Self {
        Owner {
            morph: d.kind.morph(),
            id: d.id,
            team_id: d.team_id,
            project_id: d.project.get("id").and_then(Value::as_i64),
            environment_id: d.environment.get("id").and_then(Value::as_i64),
        }
    }
}

/// `EnvironmentVariable::withoutEdgeWhitespace($value)` (v4.7): single-line values lose their
/// edge whitespace. Multiline content (a private key, a certificate, a config file) is kept as
/// it is, because tools such as OpenSSH reject a key without its trailing newline.
pub fn without_edge_whitespace(value: &str) -> &str {
    let trimmed = value.trim_matches([' ', '\n', '\r', '\t', '\x0B', '\0']);
    if trimmed.contains('\n') {
        value
    } else {
        trimmed
    }
}

/// `get_real_environment_variables_internal`.
pub async fn resolve_references(
    db: &Db,
    enc: &Encrypter,
    o: &Owner<'_>,
    server_id: Option<i64>,
    value: &str,
) -> Result<String, String> {
    let mut out = without_edge_whitespace(value).to_owned();
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
            "environment" => o.environment_id,
            "project" => o.project_id,
            "team" => Some(o.team_id),
            _ => server_id,
        };
        let Some(owner) = owner else { continue };
        let shared = vars::shared(db, o.team_id, kind, key, owner)
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
    load_stored_for(db, enc, &Owner::database(d)).await
}

/// A resource's `environment_variables` (table order), decrypted, not resolved.
pub async fn load_stored_for(db: &Db, enc: &Encrypter, o: &Owner<'_>) -> Result<Vec<Var>, String> {
    let rows = vars::for_resource(db, o.morph, o.id)
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
                Some(without_edge_whitespace(&plain).to_owned())
            }
            _ => None,
        };
        let flag = |k: &str| r.record.get(k).and_then(Value::as_bool).unwrap_or(false);
        out.push(Var {
            id: r.id,
            value,
            resolved: None,
            is_literal: r.is_literal,
            is_multiline: r.is_multiline,
            is_shared: r.is_shared,
            is_shown_once: r.is_shown_once,
            is_runtime: r.is_runtime,
            is_buildtime: flag("is_buildtime"),
            is_preview: r.is_preview,
            uses_legacy_escaping: r.uses_legacy_escaping,
            remote: false,
            preserve_compose_null: flag("preserve_compose_null"),
            cipher: r.value_encrypted,
            key: r.key,
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
    load_for(db, enc, &Owner::database(d), server_id).await
}

/// A resource's variables, decrypted, with shared references resolved and `{{vault.KEY}}`
/// references left as they are.
pub async fn load_raw_for(
    db: &Db,
    enc: &Encrypter,
    o: &Owner<'_>,
    server_id: Option<i64>,
) -> Result<Vec<Var>, String> {
    let mut out = load_stored_for(db, enc, o).await?;
    for v in &mut out {
        v.resolved = match &v.value {
            Some(x) if !x.is_empty() => Some(resolve_references(db, enc, o, server_id, x).await?),
            other => other.clone(),
        };
    }
    Ok(out)
}

/// A resource's variables with shared and remote references resolved
/// (`resolveSecretManagerEnvironmentVariableValue`, `ensureRemoteSecretsResolvable`): an unreachable
/// secret manager or a missing key is an error. The source is read once.
pub async fn load_for(
    db: &Db,
    enc: &Encrypter,
    o: &Owner<'_>,
    server_id: Option<i64>,
) -> Result<Vec<Var>, String> {
    load_for_with_fetched(db, enc, o, server_id)
        .await
        .map(|(out, _)| out)
}

/// `load_for` and every value the source returned (`resolvedSecretManagerValuesForRedaction()`).
pub async fn load_for_with_fetched(
    db: &Db,
    enc: &Encrypter,
    o: &Owner<'_>,
    server_id: Option<i64>,
) -> Result<(Vec<Var>, Vec<String>), String> {
    let mut out = load_raw_for(db, enc, o, server_id).await?;
    if !out
        .iter()
        .any(|v| secret_managers::contains_reference(v.resolved.as_deref()))
    {
        return Ok((out, Vec::new()));
    }
    let link = secret_managers::link_of(db, o.morph, o.id).await?;
    let mut fetched: Option<secret_managers::Secrets> = None;
    for v in &mut out {
        let Some(value) = v
            .resolved
            .clone()
            .filter(|x| secret_managers::contains_reference(Some(x)))
        else {
            continue;
        };
        // Without a source, a literal keeps "{{vault.KEY}}" as text; other references fail closed.
        if v.is_literal && link.is_none() {
            continue;
        }
        let secrets = match &mut fetched {
            Some(s) => &*s,
            slot => {
                let Some(link) = &link else {
                    return Err("Environment variables reference remote secrets, but no secret manager source is configured.".to_owned());
                };
                let got = link
                    .fetch(db, enc, o.team_id, "")
                    .await
                    .map_err(|e| format!("Could not fetch remote secrets: {e}"))?;
                &*slot.insert(got)
            }
        };
        let missing = secret_managers::missing_keys(&value, secrets);
        if !missing.is_empty() {
            return Err(format!(
                "Missing secret keys: {} (referenced by {}).",
                missing.join(", "),
                v.key
            ));
        }
        v.resolved = Some(secret_managers::substitute(&value, secrets));
        v.remote = true;
    }
    let values = fetched
        .map(|s| s.into_values().filter(|v| !v.is_empty()).collect())
        .unwrap_or_default();
    Ok((out, values))
}

/// A resource's variables without contacting a secret manager: a value that needs one has no
/// resolved value. For reads such as the connection URL and log redaction.
pub async fn load_without_remote_for(
    db: &Db,
    enc: &Encrypter,
    o: &Owner<'_>,
    server_id: Option<i64>,
) -> Result<Vec<Var>, String> {
    let mut out = load_raw_for(db, enc, o, server_id).await?;
    if out
        .iter()
        .any(|v| secret_managers::contains_reference(v.resolved.as_deref()))
    {
        let linked = secret_managers::link_of(db, o.morph, o.id).await?.is_some();
        for v in &mut out {
            if secret_managers::contains_reference(v.resolved.as_deref())
                && (linked || !v.is_literal)
            {
                v.resolved = None;
            }
        }
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

    /// `withoutEdgeWhitespace()`: a private key keeps its trailing newline.
    #[test]
    fn multiline_values_keep_their_edge_whitespace() {
        assert_eq!(without_edge_whitespace("  value \n"), "value");
        assert_eq!(without_edge_whitespace("\n"), "");
        let key = "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----\n";
        assert_eq!(without_edge_whitespace(key), key);
        assert_eq!(without_edge_whitespace(" a\nb "), " a\nb ");
    }

    #[test]
    fn old_lines_that_docker_compose_cannot_read() {
        // A JSON object or array is written without quotes: a line break breaks the file.
        assert!(legacy_line_is_unreadable("{\"a\":\n1}", false));
        assert!(legacy_line_is_unreadable("[1,\n2]", true));
        assert!(!legacy_line_is_unreadable("{\"a\":\"it's\"}", true));
        assert!(!legacy_line_is_unreadable("{\"a\":1}", false));
        // Other literal and multiline values are put in single quotes without escaping.
        assert!(legacy_line_is_unreadable("it's", true));
        assert!(!legacy_line_is_unreadable("it's", false));
        assert!(!legacy_line_is_unreadable("plain\nlines", true));
        // Not valid JSON: the quote rule applies.
        assert!(legacy_line_is_unreadable("{it's\n", true));
        assert!(!legacy_line_is_unreadable("{broken\n", false));
    }

    fn var(literal: bool, legacy: bool) -> Var {
        Var {
            id: 1,
            key: "K".into(),
            value: None,
            resolved: None,
            remote: false,
            is_literal: literal,
            is_multiline: false,
            is_shared: false,
            is_shown_once: false,
            is_runtime: true,
            is_buildtime: true,
            is_preview: false,
            uses_legacy_escaping: legacy,
            preserve_compose_null: false,
            cipher: None,
        }
    }

    #[test]
    fn legacy_variables_switch_to_exact_escaping_where_upstream_does() {
        let named = |key: &str, literal: bool, resolved: &str| Var {
            key: key.into(),
            resolved: Some(resolved.into()),
            ..var(literal, true)
        };
        let compose = "services:\n  app:\n    image: nginx\n    environment:\n      - FOO=${FOO:-}\n      - BAR=${BAR}\n";
        let mut vars = vec![
            named("FOO", false, "a b"),
            named("BAR", false, "a b"),
            named("QUOTED", true, "it's"),
            named("JSON", true, r#"{"a":"it's"}"#),
            named("PLAIN", true, "plain"),
        ];
        apply_resource_exact_escaping(&mut vars, true, Some(compose));
        let exact: Vec<bool> = vars.iter().map(Var::exact).collect();
        assert_eq!(exact, [true, false, true, false, false]);

        // Without Compose parsing only unreadable lines switch.
        let mut vars = vec![named("FOO", false, "a b"), named("QUOTED", true, "it's")];
        apply_resource_exact_escaping(&mut vars, false, Some(compose));
        assert_eq!(
            vars.iter().map(Var::exact).collect::<Vec<_>>(),
            [false, true]
        );
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
