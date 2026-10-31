//! `App\Livewire\SettingsOauth`, `OauthController`, `OauthLoginService`, `OauthIdentityIssuer`,
//! `get_socialite_provider`, the Socialite providers' URLs and user mappings, and the OIDC
//! provider (`OidcProvider`, `OidcDiscoveryService`, `OidcTokenValidator`).

use std::time::Duration;

use axum::Extension;
use axum::Json;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use coolify_compat::{password, webauthn};
use coolify_db::auth_pages::{self as db, OauthRow};
use coolify_db::users;
use coolify_jobs::notifications::Ctx;
use serde_json::{Map, Value, json};
use std::collections::HashMap;

use super::random_text;
use crate::audit::Meta;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::session;
use crate::state::AppState;
use crate::ui_notifications::{toast_error, unauthorized};

// ---- OauthSetting ----

fn filled(v: Option<&str>) -> bool {
    v.is_some_and(|s| !s.trim().is_empty())
}

/// `OauthSetting::couldBeEnabled`.
pub(super) fn could_be_enabled(r: &OauthRow) -> bool {
    let creds = filled(r.client_id.as_deref()) && filled(r.client_secret.as_deref());
    match r.provider.as_str() {
        "azure" => creds && filled(r.tenant.as_deref()),
        "authentik" | "clerk" | "oidc" => creds && filled(r.base_url.as_deref()),
        _ => creds,
    }
}

/// `OauthSetting::allowsUserCreation`.
pub(super) fn allows_user_creation(r: &OauthRow, registration_enabled: bool) -> bool {
    registration_enabled || (r.provider == "oidc" && r.allow_registration)
}

/// `OauthSetting::scopeList`.
fn scope_list(scopes: Option<&str>) -> Vec<String> {
    let raw = scopes
        .filter(|s| !s.is_empty() && *s != "0")
        .unwrap_or("openid email profile")
        .replace(',', " ");
    let mut out: Vec<String> = Vec::new();
    for s in raw.split(' ').map(str::trim).filter(|s| !s.is_empty()) {
        if !out.iter().any(|o| o == s) {
            out.push(s.to_string());
        }
    }
    if out.is_empty() {
        return vec!["openid".into(), "email".into(), "profile".into()];
    }
    out
}

/// `OauthSetting::loginLabel` with `lang/en.json`.
pub(super) fn login_label(r: &OauthRow) -> String {
    if filled(r.custom_label.as_deref()) {
        return r.custom_label.clone().unwrap_or_default();
    }
    if r.provider == "oidc"
        && let Ok(label) = std::env::var("OIDC_LOGIN_LABEL")
        && !label.trim().is_empty()
    {
        return label;
    }
    match r.provider.as_str() {
        "authentik" => "Login with Authentik",
        "azure" => "Login with Microsoft",
        "bitbucket" => "Login with Bitbucket",
        "clerk" => "Login with Clerk",
        "discord" => "Login with Discord",
        "github" => "Login with GitHub",
        "gitlab" => "Login with Gitlab",
        "google" => "Login with Google",
        "oidc" => "Login with SSO",
        "infomaniak" => "Login with Infomaniak",
        "zitadel" => "Login with Zitadel",
        other => other,
    }
    .to_string()
}

/// `SettingsOauth::providerLabel`.
fn provider_label(provider: &str) -> String {
    match provider {
        "oidc" => "OpenID Connect".to_string(),
        "gitlab" => "GitLab".to_string(),
        other => coolify_db::audit::headline(other),
    }
}

// ---- OauthIdentityIssuer ----

/// `OauthIdentityIssuer::normalizeBaseUrl`.
fn normalize_base_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let with_scheme = if url.contains("://") {
        url.to_string()
    } else {
        format!("https://{url}")
    };
    let (scheme, rest) = with_scheme.split_once("://")?;
    let scheme = scheme.to_lowercase();
    if scheme.is_empty() {
        return None;
    }
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let authority = authority.rsplit('@').next().unwrap_or_default();
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if p.bytes().all(|b| b.is_ascii_digit()) && !p.is_empty() => (h, Some(p)),
        _ => (authority, None),
    };
    if host.is_empty() {
        return None;
    }
    let port = port.filter(|p| !matches!((scheme.as_str(), *p), ("https", "443") | ("http", "80")));
    Some(format!(
        "{scheme}://{}{}{}",
        host.to_lowercase(),
        port.map(|p| format!(":{p}")).unwrap_or_default(),
        path.trim_end_matches('/')
    ))
}

/// `OauthIdentityIssuer::forProvider`.
fn issuer_for(provider: &str, base_url: Option<&str>, tenant: Option<&str>) -> Option<String> {
    let default = match provider {
        "oidc" => return None,
        "authentik" | "clerk" | "zitadel" => "",
        "gitlab" => "https://gitlab.com",
        "azure" => {
            let t = tenant.unwrap_or_default().trim().to_lowercase();
            return Some(
                if t.is_empty() || matches!(t.as_str(), "common" | "organizations" | "consumers") {
                    "azure".to_string()
                } else {
                    t
                },
            );
        }
        other => return Some(other.to_string()),
    };
    let base = base_url.unwrap_or_default().trim();
    normalize_base_url(if base.is_empty() { default } else { base })
}

// ---- Socialite providers ----

/// `http_build_query` with `PHP_QUERY_RFC1738`.
fn query_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
            out.push(b as char);
        } else if b == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn build_query(fields: &[(&str, String)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{k}={}", query_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// The URLs and scopes of a provider that needs no discovery.
#[derive(Debug, Clone, PartialEq)]
struct Spec {
    auth_url: String,
    token_url: String,
    scope: String,
}

/// `getAuthUrl`, `getTokenUrl` and `$scopes` of each provider class.
fn spec(r: &OauthRow) -> Option<Spec> {
    let base = r.base_url.as_deref().unwrap_or_default();
    let s = |auth: String, token: String, scope: &str| {
        Some(Spec {
            auth_url: auth,
            token_url: token,
            scope: scope.to_string(),
        })
    };
    match r.provider.as_str() {
        "azure" => {
            let tenant = r.tenant.as_deref().unwrap_or("common");
            let b = format!("https://login.microsoftonline.com/{tenant}");
            s(
                format!("{b}/oauth2/v2.0/authorize"),
                format!("{b}/oauth2/v2.0/token"),
                "User.Read",
            )
        }
        "authentik" => {
            let b = base.trim_end_matches('/');
            s(
                format!("{b}/application/o/authorize/"),
                format!("{b}/application/o/token/"),
                "openid goauthentik.io/api profile email",
            )
        }
        "clerk" => s(
            format!("{base}/oauth/authorize"),
            format!("{base}/oauth/token"),
            "email profile",
        ),
        "zitadel" => s(
            format!("{base}/oauth/v2/authorize"),
            format!("{base}/oauth/v2/token"),
            "openid profile email",
        ),
        "discord" => s(
            "https://discord.com/api/oauth2/authorize".into(),
            "https://discord.com/api/oauth2/token".into(),
            "identify email",
        ),
        "google" => s(
            "https://accounts.google.com/o/oauth2/auth".into(),
            "https://oauth2.googleapis.com/token".into(),
            "openid profile email",
        ),
        "infomaniak" => s(
            "https://login.infomaniak.com/authorize".into(),
            "https://login.infomaniak.com/token".into(),
            "openid email profile",
        ),
        "github" => s(
            "https://github.com/login/oauth/authorize".into(),
            "https://github.com/login/oauth/access_token".into(),
            "user:email",
        ),
        "bitbucket" => s(
            "https://bitbucket.org/site/oauth2/authorize".into(),
            "https://bitbucket.org/site/oauth2/access_token".into(),
            "email",
        ),
        "gitlab" => {
            let host = gitlab_host(r);
            s(
                format!("{host}/oauth/authorize"),
                format!("{host}/oauth/token"),
                "read_user",
            )
        }
        _ => None,
    }
}

fn gitlab_host(r: &OauthRow) -> String {
    match r.base_url.as_deref().filter(|b| !b.is_empty()) {
        Some(b) => b.trim_end_matches('/').to_string(),
        None => "https://gitlab.com".to_string(),
    }
}

/// `redirect_uri` or `oauth_default_redirect_uri($provider)`.
fn redirect_uri(r: &OauthRow, base: &str) -> String {
    match r.redirect_uri.as_deref().filter(|u| !u.trim().is_empty()) {
        Some(u) => u.to_string(),
        None => format!(
            "{}/auth/{}/callback",
            base.trim_end_matches('/'),
            r.provider
        ),
    }
}

/// `buildAuthUrlFromBase` with `getCodeFields`, `->with(['hd' => ...])` and Discord's `prompt`.
fn authorization_url(r: &OauthRow, spec: &Spec, redirect: &str, state: &str) -> String {
    let mut fields = vec![
        ("client_id", r.client_id.clone().unwrap_or_default()),
        ("redirect_uri", redirect.to_string()),
        ("scope", spec.scope.clone()),
        ("response_type", "code".to_string()),
        ("state", state.to_string()),
    ];
    if r.provider == "google"
        && let Some(hd) = r.tenant.as_deref().filter(|t| !t.is_empty())
    {
        fields.push(("hd", hd.to_string()));
    }
    if r.provider == "discord" {
        fields.push(("prompt", "none".to_string()));
    }
    format!("{}?{}", spec.auth_url, build_query(&fields))
}

/// A signed-in provider user: `mapUserToObject`.
#[derive(Debug, Clone, Default)]
struct OauthUser {
    id: Value,
    name: Option<String>,
    email: Option<String>,
    raw: Value,
    /// `OidcUser`: the `iss` and `sub` of the verified id_token, and its email claim.
    issuer: Option<String>,
    subject: Option<String>,
    email_verified: bool,
}

/// What `OauthController::callback` catches.
#[derive(Debug, Clone, PartialEq)]
enum Fail {
    /// `HttpException`.
    Http,
    /// `OauthLoginException`, with its `userMessageKey`.
    Login(&'static str),
    /// Anything else: the exception's class.
    Other(&'static str),
}

const REQUEST_FAILED: Fail = Fail::Other("GuzzleHttp\\Exception\\ClientException");
const MISSING_KEY: Fail = Fail::Other("ErrorException");

impl From<ApiError> for Fail {
    fn from(_: ApiError) -> Self {
        Fail::Other("Illuminate\\Database\\QueryException")
    }
}

impl From<coolify_db::DbError> for Fail {
    fn from(_: coolify_db::DbError) -> Self {
        Fail::Other("Illuminate\\Database\\QueryException")
    }
}

fn text_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// `$user['key']` of a PHP array: a missing key throws, a null one does not.
fn required<'a>(v: &'a Value, key: &str) -> Result<&'a Value, Fail> {
    v.get(key).ok_or(MISSING_KEY)
}

/// `mapUserToObject` of each provider class.
fn map_user(provider: &str, raw: Value) -> Result<OauthUser, Fail> {
    let (id, name, email) = match provider {
        "authentik" => (
            required(&raw, "sub")?.clone(),
            text_of(&raw, "name"),
            text_of(&raw, "email"),
        ),
        "zitadel" => (
            raw.get("sub").cloned().unwrap_or(Value::Null),
            text_of(&raw, "name"),
            text_of(&raw, "email"),
        ),
        "clerk" => (
            required(&raw, "user_id")?.clone(),
            required(&raw, "name")?.as_str().map(str::to_string),
            required(&raw, "email")?.as_str().map(str::to_string),
        ),
        "discord" => {
            required(&raw, "discriminator")?;
            (
                required(&raw, "id")?.clone(),
                required(&raw, "username")?.as_str().map(str::to_string),
                text_of(&raw, "email"),
            )
        }
        "google" | "infomaniak" => {
            required(&raw, "picture")?;
            (
                required(&raw, "sub")?.clone(),
                required(&raw, "name")?.as_str().map(str::to_string),
                required(&raw, "email")?.as_str().map(str::to_string),
            )
        }
        "azure" => {
            required(&raw, "mail")?;
            (
                required(&raw, "id")?.clone(),
                required(&raw, "displayName")?.as_str().map(str::to_string),
                required(&raw, "userPrincipalName")?
                    .as_str()
                    .map(str::to_string),
            )
        }
        "github" => {
            required(&raw, "node_id")?;
            required(&raw, "login")?;
            required(&raw, "avatar_url")?;
            (
                required(&raw, "id")?.clone(),
                text_of(&raw, "name"),
                text_of(&raw, "email"),
            )
        }
        "bitbucket" => {
            required(&raw, "username")?;
            (
                required(&raw, "uuid")?.clone(),
                text_of(&raw, "display_name"),
                text_of(&raw, "email"),
            )
        }
        "gitlab" => {
            required(&raw, "username")?;
            required(&raw, "avatar_url")?;
            (
                required(&raw, "id")?.clone(),
                required(&raw, "name")?.as_str().map(str::to_string),
                required(&raw, "email")?.as_str().map(str::to_string),
            )
        }
        "oidc" => {
            let name = text_of(&raw, "name").filter(|n| !n.is_empty()).or_else(|| {
                let joined = format!(
                    "{} {}",
                    text_of(&raw, "given_name").unwrap_or_default(),
                    text_of(&raw, "family_name").unwrap_or_default()
                );
                let joined = joined.trim();
                (!joined.is_empty()).then(|| joined.to_string())
            });
            (
                raw.get("sub").cloned().unwrap_or(Value::Null),
                name,
                text_of(&raw, "email"),
            )
        }
        _ => return Err(Fail::Http),
    };
    Ok(OauthUser {
        id,
        name,
        email,
        raw,
        ..OauthUser::default()
    })
}

fn http() -> Result<reqwest::Client, Fail> {
    reqwest::Client::builder()
        .user_agent("GuzzleHttp/7")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| REQUEST_FAILED)
}

async fn json_of(res: reqwest::RequestBuilder) -> Result<Value, Fail> {
    let res = res
        .send()
        .await
        .map_err(|_| Fail::Other("GuzzleHttp\\Exception\\ConnectException"))?;
    if !res.status().is_success() {
        return Err(REQUEST_FAILED);
    }
    res.json().await.map_err(|_| Fail::Other("TypeError"))
}

/// `getAccessTokenResponse`.
async fn token_response(
    client: &reqwest::Client,
    r: &OauthRow,
    secret: &str,
    token_url: &str,
    redirect: &str,
    code: &str,
    verifier: Option<&str>,
) -> Result<Value, Fail> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("client_id", r.client_id.as_deref().unwrap_or_default()),
        ("client_secret", secret),
        ("code", code),
        ("redirect_uri", redirect),
    ];
    if let Some(v) = verifier {
        form.push(("code_verifier", v));
    }
    json_of(
        client
            .post(token_url)
            .header(header::ACCEPT, "application/json")
            .form(&form),
    )
    .await
}

/// `getUserByToken` of each provider class.
async fn user_by_token(client: &reqwest::Client, r: &OauthRow, token: &str) -> Result<Value, Fail> {
    let bearer = format!("Bearer {token}");
    let get = |url: String| {
        client
            .get(url)
            .header(header::AUTHORIZATION, bearer.clone())
    };
    let base = r.base_url.as_deref().unwrap_or_default();
    match r.provider.as_str() {
        "authentik" => {
            json_of(get(format!(
                "{}/application/o/userinfo/",
                base.trim_end_matches('/')
            )))
            .await
        }
        "clerk" => json_of(get(format!("{base}/oauth/userinfo"))).await,
        "zitadel" => json_of(get(format!("{base}/oidc/v1/userinfo"))).await,
        "discord" => json_of(get("https://discord.com/api/users/@me".into())).await,
        "google" => json_of(get("https://www.googleapis.com/oauth2/v3/userinfo".into())).await,
        "infomaniak" => json_of(get("https://login.infomaniak.com/oauth2/userinfo".into())).await,
        "azure" => {
            json_of(
                get("https://graph.microsoft.com/v1.0/me".into())
                    .header(header::ACCEPT, "application/json"),
            )
            .await
        }
        "gitlab" => json_of(get(format!("{}/api/v4/user", gitlab_host(r)))).await,
        "github" => {
            let headers = |b: reqwest::RequestBuilder| {
                b.header(header::ACCEPT, "application/vnd.github.v3+json")
                    .header(header::AUTHORIZATION, format!("token {token}"))
            };
            let mut user = json_of(headers(client.get("https://api.github.com/user"))).await?;
            let emails = json_of(headers(client.get("https://api.github.com/user/emails")))
                .await
                .unwrap_or(Value::Null);
            let email = emails.as_array().and_then(|list| {
                list.iter().find_map(|e| {
                    (e.get("primary") == Some(&json!(true))
                        && e.get("verified") == Some(&json!(true)))
                    .then(|| text_of(e, "email"))
                    .flatten()
                })
            });
            user["email"] = json!(email);
            Ok(user)
        }
        "bitbucket" => {
            let mut user = json_of(get("https://api.bitbucket.org/2.0/user".into())).await?;
            let emails = json_of(get("https://api.bitbucket.org/2.0/user/emails".into()))
                .await
                .unwrap_or(Value::Null);
            let email = emails
                .get("values")
                .and_then(Value::as_array)
                .and_then(|list| {
                    list.iter().find_map(|e| {
                        (e.get("type") == Some(&json!("email"))
                            && e.get("is_primary") == Some(&json!(true))
                            && e.get("is_confirmed") == Some(&json!(true)))
                        .then(|| text_of(e, "email"))
                        .flatten()
                    })
                });
            user["email"] = json!(email);
            Ok(user)
        }
        _ => Err(Fail::Http),
    }
}

// ---- OIDC ----

/// `OidcDiscoveryDocument`.
#[derive(Debug, Clone)]
struct Discovery {
    issuer: String,
    authorization: String,
    token: String,
    userinfo: String,
    jwks: String,
}

const OIDC_FAILED: Fail = Fail::Other("App\\Auth\\Oidc\\Exceptions\\OidcException");

/// `assertHttpsUrl`.
fn is_https(url: &str) -> bool {
    url.strip_prefix("https://")
        .is_some_and(|r| !r.is_empty() && !r.starts_with('/'))
}

/// `OidcDiscoveryService::discover`.
async fn discover(client: &reqwest::Client, issuer: &str) -> Result<Discovery, Fail> {
    if !is_https(issuer) {
        return Err(OIDC_FAILED);
    }
    let issuer = issuer.trim_end_matches('/');
    let doc = json_of(
        client
            .get(format!("{issuer}/.well-known/openid-configuration"))
            .header(header::ACCEPT, "application/json"),
    )
    .await
    .map_err(|_| OIDC_FAILED)?;
    let field = |k: &str| {
        doc.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .ok_or(OIDC_FAILED)
    };
    let found = Discovery {
        issuer: field("issuer")?,
        authorization: field("authorization_endpoint")?,
        token: field("token_endpoint")?,
        userinfo: field("userinfo_endpoint")?,
        jwks: field("jwks_uri")?,
    };
    if found.issuer.trim_end_matches('/') != issuer {
        return Err(OIDC_FAILED);
    }
    Ok(found)
}

/// `OidcTokenValidator::validate`. Upstream retries an unknown `kid` on a fresh JWKS; this port
/// reads the JWKS on every sign-in, so it always has the fresh one.
fn validate_id_token(
    token: &str,
    jwks: &Value,
    issuer: &str,
    client_id: &str,
    nonce: &str,
    skew: i64,
    now: i64,
) -> Result<Value, Fail> {
    let parts: Vec<&str> = token.split('.').collect();
    let [head, body, signature] = parts.as_slice() else {
        return Err(OIDC_FAILED);
    };
    let decode = |s: &str| -> Result<Value, Fail> {
        webauthn::from_b64(s)
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or(OIDC_FAILED)
    };
    let header_json = decode(head)?;
    if header_json.get("alg") != Some(&json!("RS256")) {
        return Err(OIDC_FAILED);
    }
    let kid = header_json
        .get("kid")
        .and_then(Value::as_str)
        .filter(|k| !k.is_empty())
        .ok_or(OIDC_FAILED)?;
    let key = jwks
        .get("keys")
        .and_then(Value::as_array)
        .and_then(|keys| {
            keys.iter().find(|k| {
                k.get("kid").and_then(Value::as_str) == Some(kid)
                    && k.get("use").is_none_or(|u| u == "sig")
            })
        })
        .ok_or(OIDC_FAILED)?;
    let part = |k: &str| {
        key.get(k)
            .and_then(Value::as_str)
            .and_then(webauthn::from_b64)
            .ok_or(OIDC_FAILED)
    };
    let signature = webauthn::from_b64(signature).ok_or(OIDC_FAILED)?;
    let message = format!("{head}.{body}");
    if !webauthn::rs256_verify(&part("n")?, &part("e")?, message.as_bytes(), &signature) {
        return Err(OIDC_FAILED);
    }
    let claims = decode(body)?;
    let number = |k: &str| claims.get(k).and_then(Value::as_f64);
    let Some(exp) = number("exp") else {
        return Err(OIDC_FAILED);
    };
    let now_f = now as f64;
    let skew_f = skew as f64;
    if now_f - skew_f >= exp {
        return Err(OIDC_FAILED);
    }
    if number("nbf").is_some_and(|t| t > now_f + skew_f)
        || number("iat").is_some_and(|t| t > now_f + skew_f)
    {
        return Err(OIDC_FAILED);
    }
    if claims.get("iss").and_then(Value::as_str) != Some(issuer) {
        return Err(OIDC_FAILED);
    }
    let audience: Vec<&str> = match claims.get("aud") {
        Some(Value::String(a)) => vec![a.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => return Err(OIDC_FAILED),
    };
    if !audience.contains(&client_id) {
        return Err(OIDC_FAILED);
    }
    let azp = claims.get("azp").and_then(Value::as_str);
    if (audience.len() > 1 && azp != Some(client_id)) || azp.is_some_and(|a| a != client_id) {
        return Err(OIDC_FAILED);
    }
    if claims.get("nonce").and_then(Value::as_str) != Some(nonce) {
        return Err(OIDC_FAILED);
    }
    if claims
        .get("sub")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(OIDC_FAILED);
    }
    Ok(claims)
}

/// `OidcUser::claimsVerifyEmail`.
fn claims_verify_email(claims: &Value) -> bool {
    claims.get("email_verified") == Some(&json!(true))
        || claims.get("xms_edov") == Some(&json!(true))
}

/// `OidcProvider::user`.
#[allow(clippy::too_many_arguments)]
async fn oidc_user(
    state: &AppState,
    guest: &str,
    client: &reqwest::Client,
    r: &OauthRow,
    secret: &str,
    redirect: &str,
    code: &str,
    flow_state: &str,
) -> Result<OauthUser, Fail> {
    let found = discover(client, r.base_url.as_deref().unwrap_or_default()).await?;
    let now = state.now().and_utc().timestamp();
    let pull = |key: String| {
        state
            .sessions
            .guest(guest, |g| g.oidc.remove(&key))
            .flatten()
            .filter(|(_, expires)| *expires >= now)
            .map(|(v, _)| v)
    };
    let verifier = if r.use_pkce {
        Some(pull(format!("verifier.{flow_state}")).ok_or(OIDC_FAILED)?)
    } else {
        None
    };
    let tokens = token_response(
        client,
        r,
        secret,
        &found.token,
        redirect,
        code,
        verifier.as_deref(),
    )
    .await?;
    let access = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty());
    let id_token = tokens
        .get("id_token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty());
    let (Some(access), Some(id_token)) = (access, id_token) else {
        return Err(OIDC_FAILED);
    };
    let nonce = pull(format!("nonce.{flow_state}")).ok_or(OIDC_FAILED)?;
    if !is_https(&found.jwks) {
        return Err(OIDC_FAILED);
    }
    let jwks = json_of(
        client
            .get(&found.jwks)
            .header(header::ACCEPT, "application/json"),
    )
    .await
    .map_err(|_| OIDC_FAILED)?;
    let claims = validate_id_token(
        id_token,
        &jwks,
        &found.issuer,
        r.client_id.as_deref().unwrap_or_default(),
        &nonce,
        i64::from(r.clock_skew_seconds),
        now,
    )?;
    let userinfo = json_of(
        client
            .get(&found.userinfo)
            .header(header::ACCEPT, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {access}")),
    )
    .await?;
    if let Some(sub) = userinfo
        .get("sub")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        && Some(sub) != claims.get("sub").and_then(Value::as_str)
    {
        return Err(OIDC_FAILED);
    }
    // `array_merge($userinfo, $claims)`.
    let mut merged = userinfo.as_object().cloned().unwrap_or_default();
    if let Some(c) = claims.as_object() {
        for (k, v) in c {
            merged.insert(k.clone(), v.clone());
        }
    }
    let mut user = map_user("oidc", Value::Object(merged))?;
    user.issuer = text_of(&claims, "iss");
    user.subject = text_of(&claims, "sub");
    user.email_verified = claims_verify_email(&claims);
    Ok(user)
}

fn pkce_challenge(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    webauthn::b64(&Sha256::digest(verifier.as_bytes()))
}

// ---- OauthController ----

fn decrypted_secret(state: &AppState, r: &OauthRow) -> Option<String> {
    let stored = r.client_secret.as_deref().filter(|s| !s.is_empty())?;
    state.encrypter.as_ref()?.decrypt_string(stored).ok()
}

/// `OauthController::enabledProvider`.
async fn enabled_provider(state: &AppState, provider: &str) -> Result<Option<OauthRow>, ApiError> {
    Ok(db::oauth_setting(&state.db, provider)
        .await?
        .filter(|r| r.enabled && could_be_enabled(r)))
}

/// `redirect()`: Laravel answers 302.
fn found(to: &str) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, to.to_string())]).into_response()
}

/// The page of `abort(403)`: the application renders it for the path.
async fn denied(state: AppState, req: Request) -> Response {
    let mut res = crate::fallback::fallback(State(state), req).await;
    *res.status_mut() = StatusCode::FORBIDDEN;
    res
}

/// `OauthController::redirect`.
pub(super) async fn redirect(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    req: Request,
) -> Result<Response, ApiError> {
    let Some(r) = enabled_provider(&state, &provider).await? else {
        return Ok(denied(state, req).await);
    };
    let base = Ctx::load(&state.db).await.base_url;
    let redirect_to = redirect_uri(&r, &base);
    let flow = random_text(40);
    let (guest, cookie) = session::guest_id(&state, req.headers());
    let url = if provider == "oidc" {
        let client = http().map_err(|_| ApiError::Internal)?;
        let found = discover(&client, r.base_url.as_deref().unwrap_or_default())
            .await
            .map_err(|_| ApiError::Internal)?;
        let nonce = random_text(40);
        let expires = state.now().and_utc().timestamp() + 600;
        let spec = Spec {
            auth_url: found.authorization,
            token_url: found.token,
            scope: scope_list(r.scopes.as_deref()).join(" "),
        };
        let mut url = authorization_url(&r, &spec, &redirect_to, &flow);
        let mut extra = vec![("nonce", nonce.clone())];
        let verifier = r.use_pkce.then(|| {
            let mut bytes = [0u8; 64];
            rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
            webauthn::b64(&bytes)
        });
        if let Some(v) = &verifier {
            extra.push(("code_challenge", pkce_challenge(v)));
            extra.push(("code_challenge_method", "S256".to_string()));
        }
        url.push('&');
        url.push_str(&build_query(&extra));
        state.sessions.guest(&guest, |g| {
            g.oidc.insert(format!("nonce.{flow}"), (nonce, expires));
            if let Some(v) = verifier {
                g.oidc.insert(format!("verifier.{flow}"), (v, expires));
            }
        });
        url
    } else {
        let Some(spec) = spec(&r) else {
            return Ok(denied(state, req).await);
        };
        authorization_url(&r, &spec, &redirect_to, &flow)
    };
    state.sessions.guest(&guest, |g| g.oauth_state = Some(flow));
    Ok(session::with_guest_cookie(found(&url), cookie))
}

/// The provider's user for a callback: `get_socialite_provider($provider)->user()`.
async fn provider_user(
    state: &AppState,
    guest: &str,
    r: &OauthRow,
    query: &HashMap<String, String>,
) -> Result<OauthUser, Fail> {
    const INVALID_STATE: Fail = Fail::Other("Laravel\\Socialite\\Two\\InvalidStateException");
    use subtle::ConstantTimeEq;
    let stored = state
        .sessions
        .guest(guest, |g| g.oauth_state.take())
        .flatten()
        .filter(|s| !s.is_empty())
        .ok_or(INVALID_STATE)?;
    let given = query.get("state").map(String::as_str).unwrap_or_default();
    if !bool::from(stored.as_bytes().ct_eq(given.as_bytes())) {
        return Err(INVALID_STATE);
    }
    let base = Ctx::load(&state.db).await.base_url;
    let redirect_to = redirect_uri(r, &base);
    let code = query.get("code").map(String::as_str).unwrap_or_default();
    let secret = decrypted_secret(state, r).ok_or(Fail::Other("DecryptException"))?;
    let client = http()?;
    if r.provider == "oidc" {
        return oidc_user(state, guest, &client, r, &secret, &redirect_to, code, given).await;
    }
    let s = spec(r).ok_or(Fail::Http)?;
    let tokens =
        token_response(&client, r, &secret, &s.token_url, &redirect_to, code, None).await?;
    let token = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let raw = user_by_token(&client, r, token).await?;
    if !raw.is_object() {
        return Err(Fail::Other("TypeError"));
    }
    map_user(&r.provider, raw)
}

// ---- OauthLoginService ----

/// `isInGoogleWorkspace`.
fn in_google_workspace(hosted: Option<&str>, hd: Option<&str>) -> bool {
    let hosted = hosted.unwrap_or_default().trim().to_lowercase();
    if hosted.is_empty() {
        return true;
    }
    let hd = hd.unwrap_or_default().trim().to_lowercase();
    if hd.is_empty() {
        return false;
    }
    hosted == "*" || hd == hosted
}

/// `hasVerifiedEmail`.
fn has_verified_email(provider: &str, raw: &Value, email: &str) -> bool {
    match provider {
        "github" | "bitbucket" => true,
        "discord" => raw.get("verified") == Some(&json!(true)),
        "azure" => text_of(raw, "userPrincipalName").is_some_and(|u| u.to_lowercase() == email),
        "gitlab" => match raw.get("confirmed_at") {
            None | Some(Value::Null) => false,
            Some(Value::String(s)) => !s.trim().is_empty(),
            Some(_) => true,
        },
        _ => raw.get("email_verified") == Some(&json!(true)),
    }
}

/// `provisionUser` and `createUser`.
async fn create_user(
    state: &AppState,
    r: &OauthRow,
    name: &str,
    email: &str,
    meta: &Meta,
) -> Result<i64, Fail> {
    let hash = password::hash(&random_text(64)).ok_or(Fail::Other("RuntimeException"))?;
    let now = state.now();
    let (id, team) = if db::user_count(&state.db).await? == 0 {
        if db::create_root_user(&state.db, name, email, &hash, now).await? {
            coolify_db::notification_settings::create_defaults(&state.db, 0).await?;
        }
        (0, Some(0))
    } else if r.auto_join_root_team {
        let id = db::create_root_team_member(&state.db, name, email, &hash, now)
            .await?
            .ok_or(Fail::Http)?;
        (id, Some(0))
    } else {
        let (id, team) = db::create_user(&state.db, name, email, &hash, now).await?;
        coolify_db::notification_settings::create_defaults(&state.db, team).await?;
        (id, Some(team))
    };
    let mut context = session::auth_context(team, id, name, email);
    context["provider"] = json!(r.provider);
    context["via"] = json!("oauth");
    log_user_event(
        state,
        team,
        (id, name, email),
        meta,
        "auth.user.registered",
        context,
    )
    .await;
    Ok(id)
}

async fn log_user_event(
    state: &AppState,
    team: Option<i64>,
    (id, name, email): (i64, &str, &str),
    meta: &Meta,
    event: &str,
    context: Value,
) {
    let actor = coolify_db::audit::Actor {
        from_context: event != "auth.user.oauth_login_succeeded",
        user_id: Some(id),
        name: Some(name.to_string()),
        email: Some(email.to_string()),
        ip: meta.ip.clone(),
        user_agent: meta.user_agent.clone(),
        ..coolify_db::audit::Actor::default()
    };
    crate::audit::log_as(state, team, &actor, event, context).await;
}

/// `auditIdentityLinked`.
async fn audit_linked(
    state: &AppState,
    user: &users::LoginUser,
    provider: &str,
    issuer: &str,
    meta: &Meta,
) -> Result<(), Fail> {
    let memberships = users::memberships(&state.db, user.id).await?;
    let team = session::resolve_stored_team(user, &memberships)
        .or_else(|| memberships.first().map(|m| m.team_id));
    let mut context = session::auth_context(team, user.id, &user.name, &user.email);
    context["provider"] = json!(provider);
    context["issuer"] = json!(issuer);
    log_user_event(
        state,
        team,
        (user.id, &user.name, &user.email),
        meta,
        "auth.user.oauth_identity_linked",
        context,
    )
    .await;
    Ok(())
}

/// The shared part of `resolveOauthUser` and `resolveOidcUser`. `check` is the email rule for
/// linking an existing account.
#[allow(clippy::too_many_arguments)]
async fn resolve_identity(
    state: &AppState,
    r: &OauthRow,
    oauth: &OauthUser,
    issuer: &str,
    subject: &str,
    email: &str,
    raw: &str,
    registration_enabled: bool,
    verified_for_link: impl FnOnce(bool) -> bool,
    meta: &Meta,
) -> Result<i64, Fail> {
    const UNVERIFIED: Fail = Fail::Login("auth.failed.oauth_email_unverified");
    let now = state.now();
    if let Some((identity, user)) = db::identity(&state.db, &r.provider, issuer, subject).await? {
        db::touch_identity(&state.db, identity, email, raw, now).await?;
        return Ok(user);
    }
    let existing = users::find_by_email(&state.db, email).await?;
    let mut pre_upgrade = false;
    if let Some(user) = &existing {
        pre_upgrade = db::created_before_identities(&state.db, user.id).await?
            && user.password.as_deref().is_none_or(str::is_empty);
        if !verified_for_link(pre_upgrade) {
            return Err(UNVERIFIED);
        }
        if db::has_identity(&state.db, user.id).await? {
            return Err(Fail::Login("auth.failed.oauth_already_linked"));
        }
    }
    let links_existing = existing.is_some();
    let user_id = match &existing {
        Some(u) => u.id,
        None => {
            if !allows_user_creation(r, registration_enabled) {
                return Err(Fail::Login("auth.registration_disabled"));
            }
            let name = oauth
                .name
                .as_deref()
                .filter(|n| !n.is_empty() && *n != "0")
                .unwrap_or(email);
            create_user(state, r, name, email, meta).await?
        }
    };
    let created = db::create_identity(
        &state.db,
        user_id,
        &r.provider,
        issuer,
        subject,
        email,
        raw,
        now,
    )
    .await?;
    if !created {
        // `UniqueConstraintViolationException`: another request linked it first.
        return db::identity(&state.db, &r.provider, issuer, subject)
            .await?
            .map(|(_, u)| u)
            .ok_or(Fail::Other(
                "Illuminate\\Database\\UniqueConstraintViolationException",
            ));
    }
    if let Some(user) = &existing
        && links_existing
    {
        audit_linked(state, user, &r.provider, issuer, meta).await?;
    }
    if r.provider != "oidc"
        && (pre_upgrade || db::created_before_identities(&state.db, user_id).await?)
    {
        db::clear_created_before_identities(&state.db, user_id).await?;
    }
    Ok(user_id)
}

/// `OauthLoginService::login` up to the choice of the session: the user.
async fn resolve_user(
    state: &AppState,
    r: &OauthRow,
    oauth: &OauthUser,
    meta: &Meta,
) -> Result<i64, Fail> {
    let email = oauth
        .email
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    if email.is_empty() || !crate::rules::is_email(&email) {
        return Err(Fail::Http);
    }
    let settings = coolify_db::instance_settings::row(&state.db).await?;
    let registration_enabled = settings
        .get("is_registration_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let raw = coolify_compat::php_json::encode(&oauth.raw);
    if r.provider == "oidc" {
        let issuer = oauth
            .issuer
            .clone()
            .filter(|i| !i.is_empty())
            .or_else(|| text_of(&oauth.raw, "iss"))
            .unwrap_or_default();
        let subject = oauth
            .subject
            .clone()
            .filter(|i| !i.is_empty())
            .or_else(|| text_of(&oauth.raw, "sub"))
            .unwrap_or_default();
        let verified = oauth.email_verified || claims_verify_email(&oauth.raw);
        if issuer.is_empty() || subject.is_empty() {
            return Err(Fail::Http);
        }
        if r.require_email_verified && !verified {
            return Err(Fail::Login("auth.failed.oauth_email_unverified"));
        }
        return resolve_identity(
            state,
            r,
            oauth,
            &issuer,
            &subject,
            &email,
            &raw,
            registration_enabled,
            |_| verified,
            meta,
        )
        .await;
    }
    let subject = match &oauth.id {
        Value::String(s) if !s.trim().is_empty() => s.clone(),
        Value::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
        _ => return Err(Fail::Http),
    };
    if r.provider == "google"
        && !in_google_workspace(
            r.tenant.as_deref(),
            oauth.raw.get("hd").and_then(Value::as_str),
        )
    {
        return Err(Fail::Http);
    }
    let issuer =
        issuer_for(&r.provider, r.base_url.as_deref(), r.tenant.as_deref()).ok_or(Fail::Http)?;
    let verified = has_verified_email(&r.provider, &oauth.raw, &email);
    resolve_identity(
        state,
        r,
        oauth,
        &issuer,
        &subject,
        &email,
        &raw,
        registration_enabled,
        |pre_upgrade| pre_upgrade || verified,
        meta,
    )
    .await
}

/// `lang/en.json` for the keys the callback reports.
fn message_for(fail: &Fail) -> &'static str {
    match fail {
        Fail::Http => {
            "Sign-in with this provider failed. Contact your administrator if the problem continues."
        }
        Fail::Login("auth.failed.oauth_already_linked") => {
            "This account already uses another sign-in method. Sign in with that method."
        }
        Fail::Login("auth.failed.oauth_email_unverified") => {
            "Your sign-in provider did not verify your email address. Verify it with the provider and try again."
        }
        Fail::Login(_) => "Registration is disabled. Please contact the administrator.",
        Fail::Other(_) => "Failed to process callback from login provider.",
    }
}

/// `OauthController::callback`.
pub(super) async fn callback(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    meta: Meta,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (guest, cookie) = session::guest_id(&state, &headers);
    let attempt = async {
        let r = enabled_provider(&state, &provider)
            .await?
            .ok_or(Fail::Http)?;
        let oauth = provider_user(&state, &guest, &r, &query).await?;
        let id = resolve_user(&state, &r, &oauth, &meta).await?;
        Ok::<(i64, String), Fail>((id, r.provider))
    }
    .await;
    match attempt {
        Ok((id, provider)) => {
            let user = users::get(&state.db, id).await?.ok_or(ApiError::Internal)?;
            if user.has_two_factor() {
                state.sessions.guest(&guest, |g| g.login_id = Some(id));
                return Ok(session::with_guest_cookie(
                    found("/two-factor-challenge"),
                    cookie,
                ));
            }
            let (session_cookie, info) = session::establish(&state, id, |team| team, &meta).await?;
            let team = info
                .get("current_team")
                .and_then(|t| t.get("id"))
                .and_then(Value::as_i64);
            let mut context = session::auth_context(team, id, &user.name, &user.email);
            context["provider"] = json!(provider);
            log_user_event(
                &state,
                team,
                (id, &user.name, &user.email),
                &meta,
                "auth.user.oauth_login_succeeded",
                context,
            )
            .await;
            let mut res = found("/");
            res.headers_mut().insert(header::SET_COOKIE, session_cookie);
            Ok(session::with_guest_cookie(res, cookie))
        }
        Err(fail) => {
            let (class, reason) = match &fail {
                Fail::Http => (
                    "Symfony\\Component\\HttpKernel\\Exception\\HttpException",
                    "access_denied",
                ),
                // `OauthLoginException` is an `HttpException`.
                Fail::Login(_) => ("App\\Exceptions\\OauthLoginException", "access_denied"),
                Fail::Other(c) => (*c, "callback_error"),
            };
            crate::audit::log_system(
                &state,
                None,
                "auth.oauth.callback_failed",
                json!({"provider": provider, "exception_class": class, "reason": reason}),
                "warning",
                &meta,
            )
            .await;
            tracing::error!(provider, class, "OAuth callback failed.");
            state.sessions.guest(&guest, |g| {
                g.errors = vec![message_for(&fail).to_string()];
            });
            Ok(session::with_guest_cookie(found("/login"), cookie))
        }
    }
}

// ---- SettingsOauth ----

/// `oauthSettingToArray`.
fn setting_json(state: &AppState, r: &OauthRow) -> Value {
    json!({
        "id": r.id,
        "provider": r.provider,
        "enabled": r.enabled,
        "client_id": r.client_id,
        "client_secret": decrypted_secret(state, r),
        "redirect_uri": r.redirect_uri,
        "tenant": r.tenant,
        "base_url": r.base_url,
        "custom_label": r.custom_label,
        "scopes": r.scopes.as_deref().filter(|s| !s.is_empty() && *s != "0").unwrap_or("openid email profile"),
        "allow_registration": r.allow_registration,
        "auto_join_root_team": r.auto_join_root_team,
        "require_email_verified": r.require_email_verified,
        "use_pkce": r.use_pkce,
        "clock_skew_seconds": r.clock_skew_seconds,
        "label": provider_label(&r.provider),
    })
}

/// `SettingsOauth::mount`: OIDC first, then by provider.
async fn settings_map(state: &AppState) -> Result<Vec<Value>, ApiError> {
    let mut rows = db::oauth_settings(&state.db).await?;
    rows.sort_by_key(|r| {
        if r.provider == "oidc" {
            String::new()
        } else {
            r.provider.clone()
        }
    });
    let base = Ctx::load(&state.db).await.base_url;
    Ok(rows
        .iter()
        .map(|r| {
            let mut v = setting_json(state, r);
            v["default_redirect_uri"] = json!(format!(
                "{}/auth/{}/callback",
                base.trim_end_matches('/'),
                r.provider
            ));
            v
        })
        .collect())
}

async fn instance_admin(state: &AppState, ctx: &AuthCtx) -> Result<bool, ApiError> {
    Ok(coolify_db::instance_settings::is_instance_admin(&state.db, ctx.user_id).await?)
}

pub(super) async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
) -> ApiResult {
    if !instance_admin(&state, &ctx).await? {
        return Ok(ok(json!({"redirect": "/"})).into_response());
    }
    let settings = coolify_db::instance_settings::row(&state.db).await?;
    Ok(ok(json!({
        "disable_registration_when_oauth_enabled": settings
            .get("disable_registration_when_oauth_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "providers": settings_map(&state).await?,
    }))
    .into_response())
}

fn data_text(data: &Value, key: &str) -> Option<String> {
    data.get(key).and_then(Value::as_str).map(str::to_string)
}

fn nullable_trimmed(data: &Value, key: &str) -> Option<String> {
    data_text(data, key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// `validationRules($provider)`.
fn validation_errors(provider: &str, data: &Value) -> Map<String, Value> {
    let mut errors = Map::new();
    let key = |f: &str| format!("oauth_settings_map.{provider}.{f}");
    let label = |f: &str| format!("oauth settings map.{provider}.{}", f.replace('_', " "));
    for f in ["redirect_uri", "base_url"] {
        let Some(v) = data_text(data, f).filter(|v| !v.trim().is_empty()) else {
            continue;
        };
        let v = v.trim();
        let mut messages = Vec::new();
        if v.chars().count() > 2048 {
            messages.push(format!(
                "The {} field must not be greater than 2048 characters.",
                label(f)
            ));
        }
        let scheme_ok = v.starts_with("http://") || v.starts_with("https://");
        if !scheme_ok || !crate::laravel_url::is_url(v) {
            messages.push(format!("The {} field must be a valid URL.", label(f)));
        }
        if !messages.is_empty() {
            errors.insert(key(f), json!(messages));
        }
    }
    for (f, max) in [("custom_label", 255), ("scopes", 1000)] {
        if data_text(data, f).is_some_and(|v| v.chars().count() > max) {
            errors.insert(
                key(f),
                json!([format!(
                    "The {} field must not be greater than {max} characters.",
                    label(f)
                )]),
            );
        }
    }
    let skew = data.get("clock_skew_seconds");
    let as_int = match skew {
        None | Some(Value::Null) => Some(None),
        Some(Value::Number(n)) => n.as_i64().map(Some),
        Some(Value::String(s)) if s.is_empty() => Some(None),
        Some(Value::String(s)) => s.parse::<i64>().ok().map(Some),
        _ => None,
    };
    let name = label("clock_skew_seconds");
    match as_int {
        None => {
            errors.insert(
                key("clock_skew_seconds"),
                json!([format!("The {name} field must be an integer.")]),
            );
        }
        Some(Some(n)) if n < 0 => {
            errors.insert(
                key("clock_skew_seconds"),
                json!([format!("The {name} field must be at least 0.")]),
            );
        }
        Some(Some(n)) if n > 600 => {
            errors.insert(
                key("clock_skew_seconds"),
                json!([format!("The {name} field must not be greater than 600.")]),
            );
        }
        _ => {}
    }
    errors
}

/// `fillOauthSetting`. The secret is encrypted again on every fill, as the cast does.
fn fill(state: &AppState, row: &OauthRow, data: &Value) -> OauthRow {
    let flag = |k: &str, default: bool| data.get(k).and_then(Value::as_bool).unwrap_or(default);
    let secret = data_text(data, "client_secret").filter(|s| !s.is_empty());
    OauthRow {
        id: row.id,
        provider: row.provider.clone(),
        enabled: flag("enabled", false),
        client_id: data_text(data, "client_id"),
        client_secret: secret.and_then(|s| state.encrypter.as_ref().map(|e| e.encrypt_string(&s))),
        redirect_uri: nullable_trimmed(data, "redirect_uri"),
        tenant: data_text(data, "tenant"),
        base_url: nullable_trimmed(data, "base_url"),
        custom_label: data_text(data, "custom_label"),
        scopes: data_text(data, "scopes"),
        allow_registration: flag("allow_registration", false),
        auto_join_root_team: flag("auto_join_root_team", false),
        require_email_verified: flag("require_email_verified", true),
        use_pkce: flag("use_pkce", true),
        clock_skew_seconds: match data.get("clock_skew_seconds") {
            Some(Value::Number(n)) => n.as_i64().unwrap_or(60) as i16,
            Some(Value::String(s)) => s.parse::<i16>().unwrap_or(0),
            _ => 60,
        },
    }
}

/// `array_keys($oauth->getChanges())` without `client_secret` and `updated_at`, in the order of
/// the table's columns.
fn changed_fields(old: &OauthRow, new: &OauthRow) -> Vec<&'static str> {
    let mut out = Vec::new();
    if old.enabled != new.enabled {
        out.push("enabled");
    }
    if old.client_id != new.client_id {
        out.push("client_id");
    }
    if old.redirect_uri != new.redirect_uri {
        out.push("redirect_uri");
    }
    if old.tenant != new.tenant {
        out.push("tenant");
    }
    if old.base_url != new.base_url {
        out.push("base_url");
    }
    if old.custom_label != new.custom_label {
        out.push("custom_label");
    }
    if old.scopes != new.scopes {
        out.push("scopes");
    }
    if old.allow_registration != new.allow_registration {
        out.push("allow_registration");
    }
    if old.require_email_verified != new.require_email_verified {
        out.push("require_email_verified");
    }
    if old.use_pkce != new.use_pkce {
        out.push("use_pkce");
    }
    if old.clock_skew_seconds != new.clock_skew_seconds {
        out.push("clock_skew_seconds");
    }
    if old.auto_join_root_team != new.auto_join_root_team {
        out.push("auto_join_root_team");
    }
    out
}

/// `auditOauthSettings`.
async fn audit_setting(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    old: &OauthRow,
    new: &OauthRow,
) {
    let actor = crate::audit::actor(state, ctx, meta).await;
    crate::audit::log_as(
        state,
        None,
        &actor,
        "ui.oauth_setting.updated",
        json!({
            "team_id": null,
            "resource": "oauth_setting",
            "oauth_setting_name": new.provider,
            "provider": new.provider,
            "enabled": new.enabled,
            "changed_fields": changed_fields(old, new),
        }),
    )
    .await;
}

/// `oidc scopes must include openid`.
fn oidc_without_openid(r: &OauthRow) -> bool {
    r.provider == "oidc"
        && !scope_list(r.scopes.as_deref())
            .iter()
            .any(|s| s == "openid")
}

fn incomplete(provider: &str) -> String {
    format!(
        "OAuth settings are not complete for {provider}.<br/>Please fill in all required fields."
    )
}

fn validation_response(errors: Map<String, Value>) -> Response {
    let message = errors
        .values()
        .next()
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    PhpJson(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": message, "errors": errors}),
    )
    .into_response()
}

/// `updateOauthSettings($provider)` with `ensureProviderCanBeEnabled`.
async fn update_one(
    state: &AppState,
    ctx: &AuthCtx,
    meta: &Meta,
    provider: &str,
    data: &Value,
) -> ApiResult {
    let Some(old) = db::oauth_setting(&state.db, provider).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"));
    };
    let errors = validation_errors(provider, data);
    if !errors.is_empty() {
        return Ok(validation_response(errors));
    }
    let mut new = fill(state, &old, data);
    if new.enabled {
        let problem = if !could_be_enabled(&new) {
            Some(incomplete(provider))
        } else if oidc_without_openid(&new) {
            Some("OIDC scopes must include 'openid'.".to_string())
        } else {
            None
        };
        if let Some(message) = problem {
            // `$oauth->update(['enabled' => false])` keeps what was filled.
            new.enabled = false;
            db::save_oauth_setting(&state.db, &new, state.now()).await?;
            return Ok(toast_error(StatusCode::UNPROCESSABLE_ENTITY, message));
        }
    }
    db::save_oauth_setting(&state.db, &new, state.now()).await?;
    audit_setting(state, ctx, meta, &old, &new).await;
    Ok(ok(json!({
        "message": format!("OAuth settings for {provider} updated successfully!"),
        "setting": setting_json(state, &new),
    }))
    .into_response())
}

/// `submit` on `/settings/oauth/{provider}`, and `instantSave`.
pub(super) async fn submit_one(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(provider): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    if !instance_admin(&state, &ctx).await? {
        return Ok(unauthorized());
    }
    update_one(&state, &ctx, &meta, &provider, &body).await
}

/// `toggleProvider`.
pub(super) async fn toggle(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Path(provider): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    if !instance_admin(&state, &ctx).await? {
        return Ok(unauthorized());
    }
    let Some(old) = db::oauth_setting(&state.db, &provider).await? else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not Found"));
    };
    let enabled = body
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !enabled {
        // `validateProviderCanBeEnabled`: nothing is saved when this fails.
        let errors = validation_errors(&provider, &body);
        if !errors.is_empty() {
            return Ok(validation_response(errors));
        }
        let mut candidate = fill(&state, &old, &body);
        candidate.enabled = true;
        if !could_be_enabled(&candidate) {
            return Ok(toast_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                incomplete(&provider),
            ));
        }
        if oidc_without_openid(&candidate) {
            return Ok(toast_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "OIDC scopes must include 'openid'.",
            ));
        }
    }
    let mut data = body.clone();
    data["enabled"] = json!(!enabled);
    update_one(&state, &ctx, &meta, &provider, &data).await
}

/// `ui.instance.authentication.updated`.
async fn audit_policy(state: &AppState, ctx: &AuthCtx, meta: &Meta) {
    let actor = crate::audit::actor(state, ctx, meta).await;
    crate::audit::log_as(
        state,
        None,
        &actor,
        "ui.instance.authentication.updated",
        json!({
            "team_id": null,
            "resource": "instance",
            "changed_fields": ["disable_registration_when_oauth_enabled"],
        }),
    )
    .await;
}

/// `saveRegistrationPolicy`.
pub(super) async fn registration_policy(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Value>,
) -> ApiResult {
    if !instance_admin(&state, &ctx).await? {
        return Ok(unauthorized());
    }
    let value = body.get("value").and_then(Value::as_bool).unwrap_or(false);
    db::set_disable_registration(&state.db, value, state.now()).await?;
    audit_policy(&state, &ctx, &meta).await;
    Ok(ok(json!({"message": "Authentication settings updated successfully!"})).into_response())
}

/// `submit` on `/settings/oauth`: every provider, then the registration policy.
pub(super) async fn submit_all(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    meta: Meta,
    Json(body): Json<Value>,
) -> ApiResult {
    if !instance_admin(&state, &ctx).await? {
        return Ok(unauthorized());
    }
    let providers = body
        .get("providers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut all_errors = Map::new();
    for data in &providers {
        let provider = data
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_default();
        all_errors.extend(validation_errors(provider, data));
    }
    if !all_errors.is_empty() {
        return Ok(validation_response(all_errors));
    }
    let mut problems: Vec<String> = Vec::new();
    for data in &providers {
        let provider = data
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(old) = db::oauth_setting(&state.db, provider).await? else {
            problems.push(format!(
                "OAuth setting for provider '{provider}' not found. It may have been deleted."
            ));
            continue;
        };
        let mut new = fill(&state, &old, data);
        if new.enabled && !could_be_enabled(&new) {
            new.enabled = false;
            problems.push(format!(
                "OAuth settings are incomplete for '{provider}'. Required fields are missing. The provider has been disabled."
            ));
        }
        if new.enabled && oidc_without_openid(&new) {
            new.enabled = false;
            problems.push(
                "OIDC scopes must include 'openid'. The provider has been disabled.".to_string(),
            );
        }
        db::save_oauth_setting(&state.db, &new, state.now()).await?;
        audit_setting(&state, &ctx, &meta, &old, &new).await;
    }
    let value = body
        .get("disable_registration_when_oauth_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    db::set_disable_registration(&state.db, value, state.now()).await?;
    audit_policy(&state, &ctx, &meta).await;
    Ok(ok(json!({
        "message": "Instance settings updated successfully!",
        "error": (!problems.is_empty()).then(|| problems.join("<br/>")),
        "providers": settings_map(&state).await?,
    }))
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(provider: &str) -> OauthRow {
        OauthRow {
            id: 1,
            provider: provider.to_string(),
            enabled: true,
            client_id: Some("cid".to_string()),
            client_secret: Some("enc".to_string()),
            redirect_uri: None,
            tenant: None,
            base_url: None,
            custom_label: None,
            scopes: None,
            allow_registration: true,
            auto_join_root_team: false,
            require_email_verified: true,
            use_pkce: true,
            clock_skew_seconds: 60,
        }
    }

    #[test]
    fn issuers() {
        assert_eq!(
            normalize_base_url("HTTPS://User:x@Auth.Example.com:443/path/?q=1#f").as_deref(),
            Some("https://auth.example.com/path")
        );
        assert_eq!(
            normalize_base_url("auth.example.com").as_deref(),
            Some("https://auth.example.com")
        );
        assert_eq!(
            normalize_base_url("http://127.0.0.1:9000/").as_deref(),
            Some("http://127.0.0.1:9000")
        );
        assert_eq!(issuer_for("github", None, None).as_deref(), Some("github"));
        assert_eq!(
            issuer_for("gitlab", None, None).as_deref(),
            Some("https://gitlab.com")
        );
        assert_eq!(issuer_for("authentik", None, None), None);
        assert_eq!(
            issuer_for("azure", None, Some(" Common ")).as_deref(),
            Some("azure")
        );
        assert_eq!(
            issuer_for("azure", None, Some("ABC")).as_deref(),
            Some("abc")
        );
        assert_eq!(issuer_for("oidc", Some("https://x"), None), None);
    }

    #[test]
    fn enabling_rules() {
        let mut r = row("authentik");
        assert!(!could_be_enabled(&r));
        r.base_url = Some("http://x".into());
        assert!(could_be_enabled(&r));
        assert!(could_be_enabled(&row("github")));
        assert!(!could_be_enabled(&row("azure")));
        assert!(!allows_user_creation(&row("github"), false));
        assert!(allows_user_creation(&row("oidc"), false));
        assert!(allows_user_creation(&row("github"), true));
    }

    #[test]
    fn scopes() {
        assert_eq!(scope_list(None), ["openid", "email", "profile"]);
        assert_eq!(scope_list(Some("a,b  a")), ["a", "b"]);
        assert_eq!(scope_list(Some(" , ")), ["openid", "email", "profile"]);
    }

    #[test]
    fn authorization_urls() {
        let mut r = row("github");
        let url = authorization_url(&r, &spec(&r).unwrap(), "http://h/cb?a b", "S");
        assert_eq!(
            url,
            "https://github.com/login/oauth/authorize?client_id=cid&redirect_uri=http%3A%2F%2Fh%2Fcb%3Fa+b&scope=user%3Aemail&response_type=code&state=S"
        );
        r.provider = "discord".into();
        assert!(
            authorization_url(&r, &spec(&r).unwrap(), "x", "S")
                .ends_with("scope=identify+email&response_type=code&state=S&prompt=none")
        );
        r.provider = "google".into();
        r.tenant = Some("example.com".into());
        assert!(
            authorization_url(&r, &spec(&r).unwrap(), "x", "S").ends_with("state=S&hd=example.com")
        );
        r.provider = "authentik".into();
        r.base_url = Some("http://a:1/".into());
        assert_eq!(
            spec(&r).unwrap().auth_url,
            "http://a:1/application/o/authorize/"
        );
        r.provider = "azure".into();
        r.tenant = None;
        assert!(
            spec(&r)
                .unwrap()
                .auth_url
                .contains("/common/oauth2/v2.0/authorize")
        );
    }

    #[test]
    fn user_mapping() {
        let u = map_user("authentik", json!({"sub": "s", "email": "a@b.co"})).unwrap();
        assert_eq!(u.id, json!("s"));
        assert!(map_user("authentik", json!({"email": "a@b.co"})).is_err());
        assert!(map_user("github", json!({"id": 1})).is_err());
        let u = map_user(
            "oidc",
            json!({"sub": "s", "given_name": "A", "family_name": "B"}),
        )
        .unwrap();
        assert_eq!(u.name.as_deref(), Some("A B"));
    }

    #[test]
    fn verified_emails() {
        assert!(has_verified_email("github", &json!({}), "a@b.co"));
        assert!(!has_verified_email("authentik", &json!({}), "a@b.co"));
        assert!(has_verified_email(
            "authentik",
            &json!({"email_verified": true}),
            "a@b.co"
        ));
        assert!(!has_verified_email(
            "gitlab",
            &json!({"confirmed_at": null}),
            "a@b.co"
        ));
        assert!(has_verified_email(
            "azure",
            &json!({"userPrincipalName": "A@b.co"}),
            "a@b.co"
        ));
        assert!(in_google_workspace(None, None));
        assert!(!in_google_workspace(Some("x.co"), None));
        assert!(in_google_workspace(Some("*"), Some("y.co")));
        assert!(!in_google_workspace(Some("x.co"), Some("y.co")));
    }

    /// `OidcTokenValidator`: a signed token and what it has to match.
    #[test]
    fn id_tokens() {
        use rsa::traits::PublicKeyParts;
        use sha2::{Digest, Sha256};
        const PREFIX: [u8; 19] = [
            0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
            0x01, 0x05, 0x00, 0x04, 0x20,
        ];
        let key = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let jwks = json!({"keys": [{
            "kid": "k1", "use": "sig",
            "n": webauthn::b64(&key.n().to_bytes_be()),
            "e": webauthn::b64(&key.e().to_bytes_be()),
        }]});
        let sign = |head: &Value, claims: &Value| {
            let message = format!(
                "{}.{}",
                webauthn::b64(head.to_string().as_bytes()),
                webauthn::b64(claims.to_string().as_bytes())
            );
            let scheme = rsa::Pkcs1v15Sign {
                hash_len: Some(32),
                prefix: PREFIX.to_vec().into_boxed_slice(),
            };
            let signature = key
                .sign(scheme, &Sha256::digest(message.as_bytes()))
                .unwrap();
            format!("{message}.{}", webauthn::b64(&signature))
        };
        let head = json!({"alg": "RS256", "kid": "k1"});
        let good = json!({"iss": "https://i", "aud": "cid", "sub": "s", "nonce": "n", "exp": 1000, "iat": 900});
        let check = |token: &str, now: i64| {
            validate_id_token(token, &jwks, "https://i", "cid", "n", 60, now)
        };
        assert_eq!(check(&sign(&head, &good), 950).unwrap()["sub"], "s");
        // Within the skew, and past it.
        assert!(check(&sign(&head, &good), 1059).is_ok());
        assert!(check(&sign(&head, &good), 1060).is_err());
        assert!(check(&sign(&head, &good), 800).is_err());
        for (field, value) in [
            ("iss", json!("https://other")),
            ("aud", json!("other")),
            ("aud", json!(["cid", "other"])),
            ("azp", json!("other")),
            ("nonce", json!("other")),
            ("sub", json!("")),
        ] {
            let mut claims = good.clone();
            claims[field] = value;
            assert!(check(&sign(&head, &claims), 950).is_err(), "{field}");
        }
        // Several audiences need the matching azp.
        let mut claims = good.clone();
        claims["aud"] = json!(["cid", "other"]);
        claims["azp"] = json!("cid");
        assert!(check(&sign(&head, &claims), 950).is_ok());
        // Another algorithm, an unknown key, a changed payload.
        assert!(check(&sign(&json!({"alg": "none", "kid": "k1"}), &good), 950).is_err());
        assert!(check(&sign(&json!({"alg": "RS256", "kid": "k2"}), &good), 950).is_err());
        let token = sign(&head, &good);
        let parts: Vec<&str> = token.split('.').collect();
        let forged = format!(
            "{}.{}.{}",
            parts[0],
            webauthn::b64(
                json!({"iss": "https://i", "aud": "cid", "sub": "root", "nonce": "n", "exp": 1000})
                    .to_string()
                    .as_bytes()
            ),
            parts[2]
        );
        assert!(check(&forged, 950).is_err());
    }

    #[test]
    fn settings_validation() {
        let e = validation_errors("github", &json!({"redirect_uri": "ftp://x"}));
        assert_eq!(
            e["oauth_settings_map.github.redirect_uri"][0],
            "The oauth settings map.github.redirect uri field must be a valid URL."
        );
        assert!(validation_errors("github", &json!({"redirect_uri": "https://x.co/a"})).is_empty());
        let e = validation_errors("oidc", &json!({"clock_skew_seconds": "601"}));
        assert_eq!(e.len(), 1);
    }
}
