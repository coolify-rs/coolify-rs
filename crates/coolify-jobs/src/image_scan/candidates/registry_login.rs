//! `DockerRegistryLogins::registryAuthConfig()` and `singleRegistryConfig()` (upstream v4.7.5):
//! the `auths` login of one registry from the Docker config of a server, for the scanner
//! commands of the version check.
//!
//! The config holds credentials: it is read over SSH and never stored or logged, and only the
//! entry of the asked registry leaves this module.

use std::sync::LazyLock;

use base64::Engine as _;
use coolify_compat::php_json;
use regex::Regex;
use serde_json::{Value, json};

use crate::image_scan::ScanServer;
use crate::server_ops::registries::{DOCKER_HUB, normalize_registry};

/// `DockerRegistryLogins::READ_CONFIG`.
const READ_CONFIG: &str = "cat \"$HOME/.docker/config.json\" 2>/dev/null || true";

static BASE64: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9+/]+={0,2}\z").unwrap_or_else(|_| unreachable!()));

/// `array{status: 'ok'|'helper'|'none', config: ?string}`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum RegistryLogin {
    /// A base64 Docker config that holds only the login of the registry.
    Ok(String),
    /// A credential helper or store holds the login: it cannot be read, and Docker reads it
    /// before `auths`.
    Helper,
    None,
}

/// The config is a credential and is never printed.
impl std::fmt::Debug for RegistryLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            RegistryLogin::Ok(_) => "RegistryLogin::Ok(…)",
            RegistryLogin::Helper => "RegistryLogin::Helper",
            RegistryLogin::None => "RegistryLogin::None",
        })
    }
}

/// `DockerRegistryLogins::registryAuthConfig($server, $registry)`.
pub(crate) async fn registry_auth_config(server: &ScanServer, registry: &str) -> RegistryLogin {
    let config = server
        .srv
        .run_quiet(&[READ_CONFIG.to_owned()], "registry.read")
        .await;
    single_registry_config(config.as_deref(), registry)
}

/// `DockerRegistryLogins::singleRegistryConfig($config, $registry)`.
pub(crate) fn single_registry_config(config: Option<&str>, registry: &str) -> RegistryLogin {
    let registry = normalize_registry(registry);
    let decoded = match serde_json::from_str::<Value>(config.unwrap_or_default()) {
        Ok(decoded @ (Value::Object(_) | Value::Array(_))) => decoded,
        _ => return RegistryLogin::None,
    };

    // Docker reads a credential helper or store before `auths`, so an `auths` entry next to one
    // may be stale.
    let has_helper = decoded
        .get("credHelpers")
        .and_then(Value::as_object)
        .is_some_and(|helpers| {
            helpers
                .keys()
                .any(|key| normalize_registry(key) == registry)
        });
    let has_store = decoded
        .get("credsStore")
        .and_then(Value::as_str)
        .is_some_and(|store| !store.is_empty());
    if has_store || has_helper {
        return RegistryLogin::Helper;
    }

    let auths = decoded.get("auths").and_then(Value::as_object);
    for (key, entry) in auths.into_iter().flatten() {
        if !entry.is_object() || normalize_registry(key) != registry {
            continue;
        }
        if let Some(auth) = basic_auth(entry) {
            let config_key = if registry == DOCKER_HUB {
                "https://index.docker.io/v1/"
            } else {
                registry.as_str()
            };
            // `json_encode(…, JSON_UNESCAPED_SLASHES)`.
            let config = php_json::encode(&json!({"auths": {config_key: {"auth": auth}}}))
                .replace("\\/", "/");
            return RegistryLogin::Ok(base64::engine::general_purpose::STANDARD.encode(config));
        }
    }
    RegistryLogin::None
}

/// `basicAuth($entry)`: the base64 "username:password" of an `auths` entry, from "auth" or from
/// "username" and "password".
fn basic_auth(entry: &Value) -> Option<String> {
    let auth = entry
        .get("auth")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // `base64_decode($auth, true)` does not need the padding.
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );
    if BASE64.is_match(auth)
        && engine
            .decode(auth)
            .is_ok_and(|decoded| decoded.contains(&b':'))
    {
        return Some(auth.to_owned());
    }
    let username = entry.get("username").and_then(Value::as_str)?;
    let password = entry.get("password").and_then(Value::as_str)?;
    (!username.is_empty() && !username.contains(':') && !password.is_empty())
        .then(|| base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(text: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(text)
    }

    fn config_of(login: RegistryLogin) -> Value {
        let RegistryLogin::Ok(config) = login else {
            panic!("no login");
        };
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(config)
            .unwrap();
        serde_json::from_slice(&decoded).unwrap()
    }

    /// `it('uses the login of the image registry only after an anonymous request fails')`: only
    /// the login of the asked registry is passed on.
    #[test]
    fn only_the_login_of_the_registry_is_returned() {
        let config = json!({"auths": {
            "https://index.docker.io/v1/": {"auth": b64("hub:secret-hub")},
            "ghcr.io": {"auth": b64("acme:secret")},
            "registry.example.com": {"auth": b64("other:secret-other")},
        }})
        .to_string();
        assert_eq!(
            config_of(single_registry_config(Some(&config), "ghcr.io")),
            json!({"auths": {"ghcr.io": {"auth": b64("acme:secret")}}})
        );
        // Docker Hub keeps the key that Docker writes, with unescaped slashes.
        let hub = single_registry_config(Some(&config), "docker.io");
        let RegistryLogin::Ok(encoded) = &hub else {
            panic!("no login");
        };
        assert_eq!(
            String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .unwrap()
            )
            .unwrap(),
            format!(
                "{{\"auths\":{{\"https://index.docker.io/v1/\":{{\"auth\":\"{}\"}}}}}}",
                b64("hub:secret-hub")
            )
        );
        assert_eq!(
            single_registry_config(Some(&config), "quay.io"),
            RegistryLogin::None
        );
    }

    /// `it('asks for a login when the registry login uses a credential helper')`.
    #[test]
    fn a_credential_helper_or_store_cannot_be_read() {
        let helper =
            json!({"credHelpers": {"ghcr.io": "gh"}, "auths": {"ghcr.io": {"auth": b64("a:b")}}})
                .to_string();
        assert_eq!(
            single_registry_config(Some(&helper), "ghcr.io"),
            RegistryLogin::Helper
        );
        // A helper of another registry does not hide this login.
        assert!(matches!(
            single_registry_config(
                Some(
                    &json!({"credHelpers": {"quay.io": "q"}, "auths": {"ghcr.io": {"auth": b64("a:b")}}})
                        .to_string()
                ),
                "ghcr.io"
            ),
            RegistryLogin::Ok(_)
        ));
        let store = json!({"credsStore": "desktop", "auths": {"ghcr.io": {"auth": b64("a:b")}}})
            .to_string();
        assert_eq!(
            single_registry_config(Some(&store), "ghcr.io"),
            RegistryLogin::Helper
        );
        let empty_store =
            json!({"credsStore": "", "auths": {"ghcr.io": {"auth": b64("a:b")}}}).to_string();
        assert!(matches!(
            single_registry_config(Some(&empty_store), "ghcr.io"),
            RegistryLogin::Ok(_)
        ));
    }

    #[test]
    fn a_config_without_a_usable_login_gives_none() {
        for config in [
            None,
            Some(""),
            Some("not json"),
            Some("\"text\""),
            Some("{}"),
            Some(r#"{"auths":{"ghcr.io":{}}}"#),
            Some(r#"{"auths":{"ghcr.io":{"auth":"bm8tY29sb24="}}}"#),
            Some(r#"{"auths":{"ghcr.io":{"auth":"not base64!"}}}"#),
            Some(r#"{"auths":{"ghcr.io":{"username":"a:b","password":"c"}}}"#),
            Some(r#"{"auths":{"ghcr.io":{"username":"a","password":""}}}"#),
            Some(r#"{"auths":{"ghcr.io":"text"}}"#),
        ] {
            assert_eq!(
                single_registry_config(config, "ghcr.io"),
                RegistryLogin::None,
                "{config:?}"
            );
        }
    }

    #[test]
    fn a_login_from_username_and_password() {
        let config = r#"{"auths":{"https://GHCR.io/v2/":{"username":"acme","password":"s3cret"}}}"#;
        assert_eq!(
            config_of(single_registry_config(Some(config), "ghcr.io")),
            json!({"auths": {"ghcr.io": {"auth": b64("acme:s3cret")}}})
        );
    }

    #[test]
    fn the_login_is_not_printed() {
        let login = RegistryLogin::Ok(b64("acme:secret"));
        assert_eq!(format!("{login:?}"), "RegistryLogin::Ok(…)");
    }
}
