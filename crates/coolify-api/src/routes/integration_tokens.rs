//! `IntegrationTokensController` and `IntegrationTokenValidator`.

use axum::Extension;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::integration_tokens as db;
use regex::Regex;
use serde_json::{Map, Value, json};
use std::sync::LazyLock;
use std::time::Duration;

use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::input::validated_json;
use crate::response::{ApiError, ApiResult, created};
use crate::rules::errors_of;
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule};

pub const SECRET_MANAGER_PROVIDERS: &[&str] = &["doppler", "infisical", "vault"];

static DOPPLER_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^dp\.(st|sa)\.").unwrap_or_else(|_| unreachable!()));

/// `url:http,https`.
fn http_url(v: &str) -> bool {
    (v.starts_with("http://") || v.starts_with("https://")) && crate::laravel_url::is_url(v)
}

fn filled(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => true,
    }
}

/// `IntegrationTokenValidator::errorMessage`.
pub fn error_message(provider: &str) -> &'static str {
    match provider {
        "cloudflare" => {
            "The token could not access the selected Cloudflare capabilities. Check its permissions and zone resources."
        }
        "doppler" => "The Doppler token could not be verified. Check the token and its access.",
        "infisical" => {
            "Infisical login failed. Check the base URL, the client ID, and the client secret."
        }
        "vault" => {
            "The Vault token could not be verified. Check the base URL, the namespace, and the token."
        }
        _ => "The token could not be verified.",
    }
}

fn timeout(meta: &Map<String, Value>, default: u64) -> Duration {
    let t = meta
        .get("timeout")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(default);
    Duration::from_secs(t)
}

/// `IntegrationTokenValidator::validate` for the secret managers. `DopplerService`,
/// `InfisicalService` and `VaultService` issue one authenticated request each.
pub async fn verify(provider: &str, token: &str, meta: &Map<String, Value>) -> bool {
    let client = reqwest::Client::new();
    let text = |k: &str| {
        meta.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let result = match provider {
        "doppler" => {
            client
                .get("https://api.doppler.com/v3/me")
                .bearer_auth(token)
                .timeout(Duration::from_secs(30))
                .send()
                .await
        }
        "infisical" => {
            let base = text("base_url");
            let base = if base.is_empty() {
                "https://app.infisical.com".to_string()
            } else {
                base
            };
            client
                .post(format!(
                    "{}/api/v1/auth/universal-auth/login",
                    base.trim_end_matches('/')
                ))
                .header("accept", "application/json")
                .json(&json!({"clientId": text("client_id"), "clientSecret": token}))
                .timeout(timeout(meta, 30))
                .send()
                .await
        }
        "vault" => {
            let mut r = client
                .get(format!(
                    "{}/v1/auth/token/lookup-self",
                    text("base_url").trim_end_matches('/')
                ))
                .header("X-Vault-Token", token)
                .timeout(timeout(meta, 30));
            let ns = text("namespace");
            if !ns.is_empty() {
                r = r.header("X-Vault-Namespace", ns);
            }
            r.send().await
        }
        _ => return false,
    };
    result.is_ok_and(|r| r.status().is_success())
}

pub async fn store(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let no_query: Uri = uri.path().parse().unwrap_or(uri);
    let input = validated_json(&headers, &no_query, &body)?;
    let provider = input.str("provider").map(str::to_string);
    let mut token_rules = vec![Rule::Required, Rule::String];
    if provider.as_deref() == Some("doppler") {
        token_rules.push(Rule::Regex(&DOPPLER_TOKEN));
    }
    let fields = vec![
        Field {
            name: "provider",
            rules: vec![
                Rule::Required,
                Rule::String,
                Rule::In(SECRET_MANAGER_PROVIDERS),
            ],
        },
        Field {
            name: "name",
            rules: vec![Rule::Required, Rule::String, Rule::Max(255)],
        },
        Field {
            name: "token",
            rules: token_rules,
        },
        Field {
            name: "metadata",
            rules: vec![Rule::Array],
        },
    ];
    let mut errors = errors_of(&input, &fields, CUSTOM_API_MESSAGES)?;
    let metadata = input
        .get("metadata")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut add = |k: &str, m: String| {
        errors.insert(k.to_string(), json!([m]));
    };
    let meta_str = |k: &str| metadata.get(k).filter(|v| filled(v));
    if matches!(provider.as_deref(), Some("infisical" | "vault")) {
        match meta_str("base_url") {
            None => add("metadata.base_url", "This field is required.".into()),
            Some(Value::String(u)) if http_url(u) => {}
            Some(_) => add(
                "metadata.base_url",
                "The metadata.base url field must be a valid URL.".into(),
            ),
        }
    }
    if provider.as_deref() == Some("infisical") {
        match meta_str("client_id") {
            None => add("metadata.client_id", "This field is required.".into()),
            Some(Value::String(_)) => {}
            Some(_) => add(
                "metadata.client_id",
                "The metadata.client id field must be a string.".into(),
            ),
        }
    }
    if provider.as_deref() == Some("vault")
        && let Some(v) = meta_str("namespace")
        && !v.is_string()
    {
        add(
            "metadata.namespace",
            "The metadata.namespace field must be a string.".into(),
        );
    }
    if matches!(provider.as_deref(), Some("infisical" | "vault"))
        && let Some(v) = meta_str("timeout")
    {
        let n = v
            .as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()));
        match n {
            None => add(
                "metadata.timeout",
                "The metadata.timeout field must be an integer.".into(),
            ),
            Some(n) if n < 1 => add(
                "metadata.timeout",
                "The metadata.timeout field must be at least 1.".into(),
            ),
            Some(n) if n > 300 => add(
                "metadata.timeout",
                "The metadata.timeout field must not be greater than 300.".into(),
            ),
            _ => {}
        }
    }
    for k in input.keys() {
        if !["provider", "name", "token", "metadata"].contains(&k.as_str()) {
            errors.insert(k.clone(), json!(["This field is not allowed."]));
        }
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let provider = provider.unwrap_or_default();
    let token = input.str("token").unwrap_or_default().to_string();
    let filtered: Map<String, Value> = metadata.into_iter().filter(|(_, v)| filled(v)).collect();
    if !verify(&provider, &token, &filtered).await {
        return Err(ApiError::Message(
            StatusCode::BAD_REQUEST,
            error_message(&provider),
        ));
    }
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let uuid = coolify_domain::new_public_id();
    let metadata_text = (!filtered.is_empty()).then(|| Value::Object(filtered).to_string());
    db::create(
        &state.db,
        &db::NewToken {
            uuid: &uuid,
            team_id: ctx.team_id,
            provider: &provider,
            name: input.str("name").unwrap_or_default(),
            token_encrypted: &enc.encrypt_string(&token),
            capabilities: r#"["secrets"]"#,
            metadata: metadata_text.as_deref(),
        },
        state.now(),
    )
    .await?;
    crate::audit::log(
        &state,
        &ctx,
        &meta,
        "api.integration_token.created",
        json!({"team_id": ctx.team_id, "integration_token_uuid": uuid, "provider": provider}),
        "info",
    )
    .await;
    Ok(created(json!({"uuid": uuid})).into_response())
}
