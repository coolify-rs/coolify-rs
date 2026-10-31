//! `DopplerService`, `InfisicalService`, `VaultService`, `SecretManagerLink::fetchSecrets()`,
//! `RemoteSecretReferences` and `RemoteSecretValueFormatter`. Fetched values live only in memory.

use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::Duration;

use coolify_compat::encrypter::Encrypter;
use coolify_compat::php_json;
use coolify_db::Db;
use coolify_db::secret_managers as store;
use regex::Regex;
use reqwest::Client;
use serde_json::{Map, Value, json};

use super::provider_urls::{base_url_error, hosted, hosted_client, pinned_client};
use crate::git_http::path_str;

/// The secrets of a source. `BTreeMap`: callers sort the key names anyway.
pub type Secrets = BTreeMap<String, String>;

static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{\{\s*vault\.([A-Za-z0-9_]+)\s*\}\}").unwrap_or_else(|_| unreachable!())
});

/// `RemoteSecretReferences::containsReference`.
pub fn contains_reference(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.trim().is_empty() && PATTERN.is_match(v))
}

/// `RemoteSecretReferences::referencedKeys`.
pub fn referenced_keys(value: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in PATTERN.captures_iter(value) {
        let key = c[1].to_owned();
        if !out.contains(&key) {
            out.push(key);
        }
    }
    out
}

/// `RemoteSecretReferences::missingKeys`.
pub fn missing_keys(value: &str, secrets: &Secrets) -> Vec<String> {
    referenced_keys(value)
        .into_iter()
        .filter(|k| !secrets.contains_key(k))
        .collect()
}

/// `RemoteSecretReferences::substitute`.
pub fn substitute(value: &str, secrets: &Secrets) -> String {
    PATTERN
        .replace_all(value, |c: &regex::Captures<'_>| {
            secrets
                .get(&c[1])
                .cloned()
                .unwrap_or_else(|| c[0].to_owned())
        })
        .into_owned()
}

/// `RemoteSecretValueFormatter::dotenv`.
pub fn dotenv(value: &str) -> String {
    if !value.contains('\'') && !value.ends_with('\\') {
        return format!("'{value}'");
    }
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "$$");
    format!("\"{escaped}\"")
}

/// `RemoteSecretValueFormatter::composeFile`.
pub fn compose_file(value: &str) -> String {
    value.replace('$', "$$")
}

/// `is_string($value) ? $value : json_encode($value)`.
fn to_secret(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => php_json::encode(other),
    }
}

fn text_of(meta: &Map<String, Value>, key: &str) -> String {
    match meta.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// `IntegrationToken::requestTimeout`.
fn request_timeout(meta: &Map<String, Value>) -> Option<u64> {
    match meta.get("timeout") {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().parse().ok(),
        _ => None,
    }
}

fn parse_body(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// `Http::...->connectTimeout($timeout ?? 5)->timeout($timeout ?? 10)` on a checked base URL.
async fn self_hosted_client(
    db: &Db,
    base: &str,
    meta: &Map<String, Value>,
) -> Result<Client, String> {
    let timeout = request_timeout(meta);
    pinned_client(
        db,
        base,
        Duration::from_secs(timeout.unwrap_or(5)),
        Duration::from_secs(timeout.unwrap_or(10)),
    )
    .await
}

fn trimmed_base(url: &str) -> String {
    url.trim_end_matches('/').to_owned()
}

/// The base URL `InfisicalService` uses.
fn infisical_base(meta: &Map<String, Value>) -> String {
    match text_of(meta, "base_url") {
        b if b.is_empty() => "https://app.infisical.com".to_owned(),
        b => trimmed_base(&b),
    }
}

/// `DopplerService::fetchSecrets`.
async fn doppler_fetch(
    token: &str,
    project: Option<&str>,
    config: Option<&str>,
) -> Result<Secrets, String> {
    let base = hosted("doppler", "https://api.doppler.com");
    let mut query = vec![("format", "json")];
    if let Some(p) = project.filter(|p| !p.trim().is_empty()) {
        query.push(("project", p));
    }
    if let Some(c) = config.filter(|c| !c.trim().is_empty()) {
        query.push(("config", c));
    }
    let response = hosted_client(10)?
        .get(format!("{base}/v3/configs/config/secrets/download"))
        .bearer_auth(token)
        .header("accept", "application/json")
        .query(&query)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status();
    let body = parse_body(&response.text().await.unwrap_or_default());
    if !status.is_success() {
        let message = body
            .pointer("/messages/0")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
        return Err(format!("Doppler API error: {message}"));
    }
    Ok(body
        .as_object()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), to_secret(v))).collect())
        .unwrap_or_default())
}

/// `InfisicalService::login`.
async fn infisical_login(
    client: &Client,
    base: &str,
    client_id: &str,
    secret: &str,
) -> Result<String, String> {
    let response = client
        .post(format!("{base}/api/v1/auth/universal-auth/login"))
        .header("accept", "application/json")
        .json(&json!({"clientId": client_id, "clientSecret": secret}))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status();
    let body = parse_body(&response.text().await.unwrap_or_default());
    let access = path_str(&body, "accessToken").filter(|t| !t.trim().is_empty());
    match access {
        Some(t) if status.is_success() => Ok(t.to_owned()),
        _ => Err(format!(
            "Infisical login failed: {}",
            path_str(&body, "message")
                .map(str::to_owned)
                .unwrap_or_else(|| format!("HTTP {}", status.as_u16()))
        )),
    }
}

/// `keyValues()`, merged over `merged`.
fn merge_infisical(merged: &mut Secrets, secrets: Option<&Value>) {
    for s in secrets.and_then(Value::as_array).into_iter().flatten() {
        let key = match s.get("secretKey") {
            Some(Value::String(k)) => k.clone(),
            _ => String::new(),
        };
        let value = match s.get("secretValue") {
            Some(Value::String(v)) => v.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };
        merged.insert(key, value);
    }
}

/// `InfisicalService::fetchSecrets`.
async fn infisical_fetch(
    client: &Client,
    base: &str,
    client_id: &str,
    secret: &str,
    project_id: &str,
    environment: &str,
    secret_path: &str,
) -> Result<Secrets, String> {
    let access = infisical_login(client, base, client_id, secret).await?;
    let secret_path = if secret_path.is_empty() {
        "/"
    } else {
        secret_path
    };
    let options = [
        ("expandSecretReferences", "true"),
        ("viewSecretValue", "true"),
    ];
    let mut response = client
        .get(format!("{base}/api/v4/secrets"))
        .bearer_auth(&access)
        .header("accept", "application/json")
        .query(&[
            ("projectId", project_id),
            ("environment", environment),
            ("secretPath", secret_path),
            ("includeImports", "true"),
        ])
        .query(&options)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if response.status().as_u16() == 404 {
        response = client
            .get(format!("{base}/api/v3/secrets/raw"))
            .bearer_auth(&access)
            .header("accept", "application/json")
            .query(&[
                ("workspaceId", project_id),
                ("environment", environment),
                ("secretPath", secret_path),
                ("include_imports", "true"),
            ])
            .query(&options)
            .send()
            .await
            .map_err(|e| e.to_string())?;
    }
    let status = response.status();
    let body = parse_body(&response.text().await.unwrap_or_default());
    if !status.is_success() {
        return Err(format!(
            "Infisical API error: {}",
            path_str(&body, "message")
                .map(str::to_owned)
                .unwrap_or_else(|| format!("HTTP {}", status.as_u16()))
        ));
    }
    let mut merged = Secrets::new();
    for import in body
        .get("imports")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        merge_infisical(&mut merged, import.get("secrets"));
    }
    merge_infisical(&mut merged, body.get("secrets"));
    Ok(merged)
}

/// `VaultService::fetchSecrets`.
async fn vault_fetch(
    client: &Client,
    base: &str,
    token: &str,
    namespace: &str,
    mount: &str,
    path: &str,
) -> Result<Secrets, String> {
    let mount = mount.trim_matches('/');
    let path = path.trim_matches('/');
    let mut request = client
        .get(format!("{base}/v1/{mount}/data/{path}"))
        .header("X-Vault-Token", token)
        .header("accept", "application/json");
    if !namespace.trim().is_empty() {
        request = request.header("X-Vault-Namespace", namespace);
    }
    let response = request.send().await.map_err(|e| e.to_string())?;
    let status = response.status();
    let body = parse_body(&response.text().await.unwrap_or_default());
    if !status.is_success() {
        let message = body
            .pointer("/errors/0")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
        return Err(format!("Vault API error: {message}"));
    }
    Ok(body
        .pointer("/data/data")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), to_secret(v))).collect())
        .unwrap_or_default())
}

/// What `fetch` needs from an `integration_tokens` row.
#[allow(missing_debug_implementations)]
pub struct Source<'a> {
    pub provider: &'a str,
    /// Decrypted.
    pub token: &'a str,
    pub metadata: &'a Map<String, Value>,
    pub settings: &'a Map<String, Value>,
}

/// `SecretManagerLink::fetchSecrets()` once the team check passed.
pub async fn fetch(db: &Db, s: &Source<'_>, settings_url: &str) -> Result<Secrets, String> {
    let setting = |k: &str| text_of(s.settings, k);
    match s.provider {
        "doppler" => {
            let (project, config) = (setting("project"), setting("config"));
            doppler_fetch(s.token, Some(&project), Some(&config)).await
        }
        "infisical" => {
            let base = infisical_base(s.metadata);
            if let Some(message) = base_url_error(db, &base, settings_url).await {
                return Err(message);
            }
            let client = self_hosted_client(db, &base, s.metadata).await?;
            let path = match s.settings.get("secret_path") {
                Some(_) => setting("secret_path"),
                None => "/".to_owned(),
            };
            infisical_fetch(
                &client,
                &base,
                &text_of(s.metadata, "client_id"),
                s.token,
                &setting("project_id"),
                &setting("environment"),
                &path,
            )
            .await
        }
        "vault" => {
            let base = trimmed_base(&text_of(s.metadata, "base_url"));
            if let Some(message) = base_url_error(db, &base, settings_url).await {
                return Err(message);
            }
            let client = self_hosted_client(db, &base, s.metadata).await?;
            let mount = match s.settings.get("mount") {
                Some(_) => setting("mount"),
                None => "secret".to_owned(),
            };
            vault_fetch(
                &client,
                &base,
                s.token,
                &text_of(s.metadata, "namespace"),
                &mount,
                &setting("path"),
            )
            .await
        }
        other => Err(format!("Unsupported secret manager provider [{other}].")),
    }
}

/// `IntegrationTokenValidator::validate` for the secret managers: `Err` is the `ValidationException`
/// of a base URL that `SafeExternalUrl` refuses.
pub async fn validate(
    db: &Db,
    provider: &str,
    token: &str,
    meta: &Map<String, Value>,
    settings_url: &str,
) -> Result<bool, String> {
    match provider {
        "doppler" => {
            let base = hosted("doppler", "https://api.doppler.com");
            let Ok(client) = hosted_client(10) else {
                return Ok(false);
            };
            Ok(client
                .get(format!("{base}/v3/me"))
                .bearer_auth(token)
                .header("accept", "application/json")
                .send()
                .await
                .is_ok_and(|r| r.status().is_success()))
        }
        "infisical" => {
            let base = infisical_base(meta);
            if let Some(message) = base_url_error(db, &base, settings_url).await {
                return Err(message);
            }
            let Ok(client) = self_hosted_client(db, &base, meta).await else {
                return Ok(false);
            };
            Ok(
                infisical_login(&client, &base, &text_of(meta, "client_id"), token)
                    .await
                    .is_ok(),
            )
        }
        "vault" => {
            let base = trimmed_base(&text_of(meta, "base_url"));
            if let Some(message) = base_url_error(db, &base, settings_url).await {
                return Err(message);
            }
            let Ok(client) = self_hosted_client(db, &base, meta).await else {
                return Ok(false);
            };
            let mut request = client
                .get(format!("{base}/v1/auth/token/lookup-self"))
                .header("X-Vault-Token", token)
                .header("accept", "application/json");
            let namespace = text_of(meta, "namespace");
            if !namespace.trim().is_empty() {
                request = request.header("X-Vault-Namespace", namespace);
            }
            Ok(request.send().await.is_ok_and(|r| r.status().is_success()))
        }
        _ => Ok(false),
    }
}

/// `SecretManagerLink::sourceSummary()`.
pub fn source_summary(provider: &str, settings: &Map<String, Value>) -> String {
    let get = |k: &str| text_of(settings, k);
    match provider {
        "doppler" => {
            let joined = [get("project"), get("config")]
                .into_iter()
                .filter(|v| !v.is_empty() && v != "0")
                .collect::<Vec<_>>()
                .join("/");
            let trimmed = joined.trim_matches('/');
            if trimmed.is_empty() || trimmed == "0" {
                "token scope".to_owned()
            } else {
                trimmed.to_owned()
            }
        }
        "infisical" => {
            let path = match settings.get("secret_path") {
                Some(_) => get("secret_path"),
                None => "/".to_owned(),
            };
            format!("{}/{}{path}", get("project_id"), get("environment"))
        }
        "vault" => {
            let mount = match settings.get("mount") {
                Some(_) => get("mount"),
                None => "secret".to_owned(),
            };
            format!("{mount}/{}", get("path"))
        }
        _ => String::new(),
    }
}

/// `IntegrationToken::PROVIDER_NAMES`.
pub fn provider_name(provider: &str) -> String {
    match provider {
        "cloudflare" => "Cloudflare".into(),
        "doppler" => "Doppler".into(),
        "infisical" => "Infisical".into(),
        "vault" => "HashiCorp Vault".into(),
        other => {
            let mut c = other.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        }
    }
}

/// `dopplerTokenType()`.
pub fn doppler_token_type(token: &str) -> Option<&'static str> {
    if token.starts_with("dp.st.") {
        Some("service")
    } else if token.starts_with("dp.sa.") {
        Some("service_account")
    } else {
        None
    }
}

fn object_of(text: Option<&str>) -> Map<String, Value> {
    text.and_then(|t| serde_json::from_str::<Value>(t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// A resource's `secretManagerLink` with its token: everything a fetch and its log lines need.
pub struct Link {
    pub token_uuid: String,
    pub token_id: i64,
    pub token_name: String,
    pub provider: String,
    pub token_team_id: i64,
    pub token_encrypted: String,
    pub metadata: Map<String, Value>,
    pub settings: Map<String, Value>,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("token_uuid", &self.token_uuid)
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

impl Link {
    /// `sourceSummary()`.
    pub fn summary(&self) -> String {
        source_summary(&self.provider, &self.settings)
    }

    /// `providerName()`.
    pub fn provider_name(&self) -> String {
        provider_name(&self.provider)
    }

    /// `dopplerTokenType()`.
    pub fn doppler_type(&self, enc: &Encrypter) -> Option<&'static str> {
        if self.provider != "doppler" {
            return None;
        }
        enc.decrypt_string(&self.token_encrypted)
            .ok()
            .and_then(|t| doppler_token_type(&t))
    }

    /// `fetchSecrets()`.
    pub async fn fetch(
        &self,
        db: &Db,
        enc: &Encrypter,
        resource_team_id: i64,
        settings_url: &str,
    ) -> Result<Secrets, String> {
        if self.token_team_id != resource_team_id {
            return Err(
                "The secret manager token does not belong to the team of this resource.".to_owned(),
            );
        }
        let token = enc
            .decrypt_string(&self.token_encrypted)
            .map_err(|_| "The MAC is invalid.".to_owned())?;
        fetch(
            db,
            &Source {
                provider: &self.provider,
                token: &token,
                metadata: &self.metadata,
                settings: &self.settings,
            },
            settings_url,
        )
        .await
    }
}

/// `$resource->secretManagerLink()->with('integrationToken')->first()`.
pub async fn link_of(db: &Db, morph: &str, resource_id: i64) -> Result<Option<Link>, String> {
    let row = store::link_with_token(db, morph, resource_id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(row.map(|r| Link {
        metadata: object_of(r.metadata.as_deref()),
        settings: object_of(r.settings.as_deref()),
        token_uuid: r.token_uuid,
        token_id: r.token_id,
        token_name: r.token_name,
        provider: r.provider,
        token_team_id: r.token_team_id,
        token_encrypted: r.token_encrypted,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references() {
        let v = "a{{vault.ONE}}b{{ vault.TWO }}c{{vault.ONE}}{{team.X}}";
        assert!(contains_reference(Some(v)));
        assert!(!contains_reference(Some("{{team.X}}")));
        assert!(!contains_reference(None));
        assert_eq!(referenced_keys(v), ["ONE", "TWO"]);
        let mut s = Secrets::new();
        s.insert("ONE".into(), "1".into());
        assert_eq!(missing_keys(v, &s), ["TWO"]);
        assert_eq!(substitute(v, &s), "a1b{{ vault.TWO }}c1{{team.X}}");
    }

    #[test]
    fn formatter_matches_upstream() {
        assert_eq!(dotenv("a$b#c"), "'a$b#c'");
        assert_eq!(dotenv("it's $x"), "\"it's $$x\"");
        assert_eq!(dotenv(r"tail\"), r#""tail\\""#);
        assert_eq!(dotenv(r#"q'"z"#), r#""q'\"z""#);
        assert_eq!(compose_file("a$b"), "a$$b");
    }

    #[test]
    fn summaries() {
        let map = |v: Value| v.as_object().cloned().unwrap_or_default();
        assert_eq!(source_summary("doppler", &Map::new()), "token scope");
        assert_eq!(
            source_summary("doppler", &map(json!({"project":"p","config":"c"}))),
            "p/c"
        );
        assert_eq!(
            source_summary(
                "infisical",
                &map(json!({"project_id":"p","environment":"prod"}))
            ),
            "p/prod/"
        );
        assert_eq!(
            source_summary("vault", &map(json!({"path":"app/x"}))),
            "secret/app/x"
        );
    }

    #[test]
    fn non_strings_are_json() {
        assert_eq!(to_secret(&json!("x")), "x");
        assert_eq!(to_secret(&json!({"a":1})), r#"{"a":1}"#);
        assert_eq!(to_secret(&json!(true)), "true");
    }
}
