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
