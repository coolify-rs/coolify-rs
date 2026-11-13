//! `App\Services\DockerRegistryLogins`.

use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::Duration;

use base64::Engine as _;
use coolify_remote::RunOptions;
use regex::Regex;
use serde_json::{Value, json};

use super::{Srv, shell_arg};

pub const DOCKER_HUB: &str = "docker.io";

/// `REGISTRY_PATTERN`.
pub static REGISTRY_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:[a-z0-9]([a-z0-9.-]*[a-z0-9])?|\[[0-9a-f:.]+\])(:[0-9]{1,5})?\n?\z")
        .unwrap_or_else(|_| unreachable!())
});

/// `normalizeRegistry`.
pub fn normalize_registry(key: &str) -> String {
    let lower = coolify_remote::text::php_trim(key).to_lowercase();
    let scheme = Regex::new(r"^[a-z][a-z0-9+.-]*://").unwrap_or_else(|_| unreachable!());
    let host = scheme.replace(&lower, "").to_string();
    let host = host.split('/').next().unwrap_or_default().to_string();
    if [
        "index.docker.io",
        "registry-1.docker.io",
        "registry.hub.docker.com",
        "docker.io",
    ]
    .contains(&host.as_str())
    {
        DOCKER_HUB.to_string()
    } else {
        host
    }
}

/// `registryFromImage`.
pub fn registry_from_image(image: &str) -> String {
    let image = image.trim();
    if image.is_empty() || !image.contains('/') {
        return DOCKER_HUB.to_string();
    }
    let first = image.split('/').next().unwrap_or_default();
    if first.contains('.') || first.contains(':') || first == "localhost" {
        normalize_registry(first)
    } else {
        DOCKER_HUB.to_string()
    }
}

fn username_from_auth(entry: &Value) -> Option<String> {
    if let Some(u) = entry
        .get("username")
        .and_then(Value::as_str)
        .filter(|u| !u.is_empty())
    {
        return Some(u.to_string());
    }
    let auth = entry.get("auth").and_then(Value::as_str)?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(auth)
        .ok()?;
    let pos = decoded.iter().position(|b| *b == b':')?;
    let name = String::from_utf8(decoded[..pos].to_vec()).ok()?;
    (!name.is_empty()).then_some(name)
}

/// One login: `(source, username)`.
pub type Logins = BTreeMap<String, (String, Option<String>)>;

/// `parseConfig`.
pub fn parse_config(config: Option<&str>) -> Logins {
    let mut out = Logins::new();
    let Some(decoded) = config.and_then(|c| serde_json::from_str::<Value>(c).ok()) else {
        return out;
    };
    for source in ["auths", "credHelpers"] {
        let Some(entries) = decoded.get(source).and_then(Value::as_object) else {
            continue;
        };
        for (key, entry) in entries {
            let registry = normalize_registry(key);
            if REGISTRY_PATTERN.is_match(&registry) && !out.contains_key(&registry) {
                let username = if source == "auths" && entry.is_object() {
                    username_from_auth(entry)
                } else {
                    None
                };
                out.insert(registry, (source.to_string(), username));
            }
        }
    }
    out
}

/// `forServer`.
pub async fn for_server(srv: &Srv) -> Result<Logins, String> {
    let config = srv
        .run(
            &["cat \"$HOME/.docker/config.json\" 2>/dev/null || true".to_string()],
            "registry.read",
        )
        .await?;
    Ok(parse_config(config.as_deref()))
}

/// One resource that uses a registry: `(registry, type, name, link)`.
pub type RegistryUser = (String, &'static str, String, Option<String>);

/// `rows`.
pub fn rows(logins: &Logins, users: &[RegistryUser]) -> Vec<Value> {
    let mut names: Vec<String> = logins.keys().cloned().collect();
    for (r, ..) in users {
        if !names.contains(r) {
            names.push(r.clone());
        }
    }
    names.sort();
    names
        .into_iter()
        .map(|registry| {
            let login = logins.get(&registry);
            let used: Vec<Value> = users
                .iter()
                .filter(|(r, ..)| *r == registry)
                .map(|(_, t, n, l)| json!({"type": t, "name": n, "link": l}))
                .collect();
            json!({
                "registry": registry,
                "logged_in": login.is_some(),
                "source": login.map(|(s, _)| s.clone()),
                "username": login.and_then(|(_, u)| u.clone()),
                "used_by": used,
            })
        })
        .collect()
}

fn registry_argument(registry: &str) -> String {
    if registry == DOCKER_HUB {
        String::new()
    } else {
        format!(" {}", shell_arg(registry))
    }
}

/// `run`: a failure is the useful line of Docker's output, without secrets.
async fn run(
    srv: &Srv,
    command: String,
    secrets: &[String],
    empty_error: Option<String>,
) -> Result<(), String> {
    let t = srv.target().await?;
    let result = srv
        .jobs
        .remote()
        .run(
            &t,
            &[command.as_str()],
            RunOptions::op("registry.command")
                .attempts(1)
                .timeout(Duration::from_secs(60)),
        )
        .await;
    let Err(e) = result else {
        return Ok(());
    };
    if e.message.contains("timed out") {
        return Err("The command on the server timed out after 60 seconds.".to_string());
    }
    let lines: Vec<String> = e
        .message
        .lines()
        .map(|l| {
            l.replace("Login did not succeed, error:", "")
                .trim()
                .to_string()
        })
        .filter(|l| {
            !(l.is_empty()
                || l.starts_with("WARNING!")
                || l.contains("credential-stores")
                || l.starts_with("Info ->")
                || l.starts_with("Authenticating with existing credentials")
                || l.contains("cannot perform an interactive login")
                || l.starts_with("Control socket connect")
                || l.starts_with("ControlSocket ")
                || l.starts_with("Warning: Permanently added"))
        })
        .collect();
    let errors: Vec<&String> = lines.iter().filter(|l| l.starts_with("Error")).collect();
    let mut text = if errors.is_empty() {
        lines.join(" ")
    } else {
        errors
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    for s in secrets.iter().filter(|s| !s.is_empty()) {
        text = text.replace(s, "***");
    }
    Err(if !text.is_empty() {
        text
    } else {
        empty_error.unwrap_or_else(|| {
            format!("The command failed with exit code {}.", e.code.unwrap_or(1))
        })
    })
}

/// `login`.
pub async fn login(
    srv: &Srv,
    registry: &str,
    username: &str,
    password: &str,
) -> Result<(), String> {
    let secret = base64::engine::general_purpose::STANDARD.encode(password);
    let command = format!(
        "echo '{secret}' | base64 -d | docker --config \"$HOME/.docker\" login --username {} --password-stdin{}",
        shell_arg(username),
        registry_argument(registry)
    );
    run(srv, command, &[password.to_string(), secret], None).await
}

/// `checkLogin`.
pub async fn check_login(srv: &Srv, registry: &str) -> Result<(), String> {
    let command = format!(
        "docker --config \"$HOME/.docker\" login{} </dev/null",
        registry_argument(registry)
    );
    run(
        srv,
        command,
        &[],
        Some(format!(
            "There is no saved login for {registry} on this server."
        )),
    )
    .await
}

/// `logout`.
pub async fn logout(srv: &Srv, registry: &str) -> Result<(), String> {
    let command = format!(
        "docker --config \"$HOME/.docker\" logout{}",
        registry_argument(registry)
    );
    run(srv, command, &[], None).await
}

/// What `DockerRegistryLogins::registryAuthConfig()` found for one registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryAuth {
    /// A base64 Docker config that holds only the `auths` login of the registry.
    Ok(String),
    /// A credential helper or store holds the login; it cannot be read.
    Helper,
    None,
}

/// `DockerRegistryLogins::registryAuthConfig()` (v4.7.5): the `auths` login of one registry from
/// the server's Docker config, for the scanner commands. The result is never stored or logged.
pub async fn registry_auth_config(srv: &Srv, registry: &str) -> RegistryAuth {
    let config = srv
        .run_quiet(
            &[r#"cat "$HOME/.docker/config.json" 2>/dev/null || true"#.to_string()],
            "server.registries",
        )
        .await;
    single_registry_config(config.as_deref(), registry)
}

/// `DockerRegistryLogins::singleRegistryConfig()`.
pub fn single_registry_config(config: Option<&str>, registry: &str) -> RegistryAuth {
    use base64::Engine as _;
    let registry = normalize_registry(registry);
    let Ok(Value::Object(decoded)) = serde_json::from_str::<Value>(config.unwrap_or_default())
    else {
        return RegistryAuth::None;
    };
    // Docker reads a credential helper or store before `auths`, so an `auths` entry next to one
    // may be stale.
    let has_helper = decoded
        .get("credHelpers")
        .and_then(Value::as_object)
        .is_some_and(|helpers| helpers.keys().any(|k| normalize_registry(k) == registry));
    let has_store = decoded
        .get("credsStore")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    if has_store || has_helper {
        return RegistryAuth::Helper;
    }
    let Some(auths) = decoded.get("auths").and_then(Value::as_object) else {
        return RegistryAuth::None;
    };
    for (key, entry) in auths {
        if !entry.is_object() || normalize_registry(key) != registry {
            continue;
        }
        if let Some(auth) = basic_auth(entry) {
            let config_key = if registry == "docker.io" {
                "https://index.docker.io/v1/"
            } else {
                registry.as_str()
            };
            // `json_encode(..., JSON_UNESCAPED_SLASHES)`.
            let config = serde_json::json!({"auths": {config_key: {"auth": auth}}}).to_string();
            return RegistryAuth::Ok(base64::engine::general_purpose::STANDARD.encode(config));
        }
    }
    RegistryAuth::None
}

/// `basicAuth()`: the base64 "username:password" of an `auths` entry, from "auth" or from
/// "username" and "password".
fn basic_auth(entry: &Value) -> Option<String> {
    use base64::Engine as _;
    let auth = entry
        .get("auth")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let well_formed = !auth.is_empty()
        && auth.trim_end_matches('=').len() + 2 >= auth.len()
        && auth
            .trim_end_matches('=')
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/');
    if well_formed
        && base64::engine::general_purpose::STANDARD
            .decode(auth)
            .is_ok_and(|decoded| decoded.contains(&b':'))
    {
        return Some(auth.to_owned());
    }
    let username = entry.get("username").and_then(Value::as_str)?;
    let password = entry.get("password").and_then(Value::as_str)?;
    if username.is_empty() || username.contains(':') || password.is_empty() {
        return None;
    }
    Some(base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}")))
}

#[cfg(test)]
mod registry_auth_tests {
    use super::*;

    fn decode(config: &RegistryAuth) -> String {
        use base64::Engine as _;
        match config {
            RegistryAuth::Ok(c) => String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(c)
                    .unwrap_or_default(),
            )
            .unwrap_or_default(),
            other => format!("{other:?}"),
        }
    }

    // `DockerRegistryLogins::singleRegistryConfig()` (upstream tests/Unit/DockerRegistryLoginsTest).
    #[test]
    fn one_registry_login_becomes_a_config_of_its_own() {
        // "user:pass".
        let config = r#"{"auths":{"ghcr.io":{"auth":"dXNlcjpwYXNz"},"https://index.docker.io/v1/":{"username":"me","password":"secret"}}}"#;
        assert_eq!(
            decode(&single_registry_config(Some(config), "ghcr.io")),
            r#"{"auths":{"ghcr.io":{"auth":"dXNlcjpwYXNz"}}}"#
        );
        // Docker Hub keeps its legacy key; "me:secret".
        assert_eq!(
            decode(&single_registry_config(Some(config), "docker.io")),
            r#"{"auths":{"https://index.docker.io/v1/":{"auth":"bWU6c2VjcmV0"}}}"#
        );
        assert_eq!(
            single_registry_config(Some(config), "quay.io"),
            RegistryAuth::None
        );
        assert_eq!(single_registry_config(None, "ghcr.io"), RegistryAuth::None);
        assert_eq!(
            single_registry_config(Some("not json"), "ghcr.io"),
            RegistryAuth::None
        );
    }

    #[test]
    fn a_credential_helper_or_store_wins_over_auths() {
        let store = r#"{"credsStore":"desktop","auths":{"ghcr.io":{"auth":"dXNlcjpwYXNz"}}}"#;
        assert_eq!(
            single_registry_config(Some(store), "ghcr.io"),
            RegistryAuth::Helper
        );
        let helper = r#"{"credHelpers":{"ghcr.io":"gh"},"auths":{"ghcr.io":{"auth":"dXNlcjpwYXNz"},"quay.io":{"auth":"dXNlcjpwYXNz"}}}"#;
        assert_eq!(
            single_registry_config(Some(helper), "ghcr.io"),
            RegistryAuth::Helper
        );
        assert!(matches!(
            single_registry_config(Some(helper), "quay.io"),
            RegistryAuth::Ok(_)
        ));
    }

    #[test]
    fn an_entry_without_a_usable_login_is_no_login() {
        // "auth" that is not base64 of "user:password", and no username.
        let config = r#"{"auths":{"ghcr.io":{"auth":"bm9jb2xvbg=="},"quay.io":{"username":"a:b","password":"x"},"r.example":{"identitytoken":"t"}}}"#;
        for registry in ["ghcr.io", "quay.io", "r.example"] {
            assert_eq!(
                single_registry_config(Some(config), registry),
                RegistryAuth::None
            );
        }
    }
}
