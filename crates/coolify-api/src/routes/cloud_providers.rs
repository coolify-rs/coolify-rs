//! `HetznerController`, `VultrController`, `DigitalOceanController` and `HostingerController`.

use std::net::{IpAddr, SocketAddr};

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use coolify_db::private_keys::{self, KeyRow};
use coolify_db::records::Record;
use coolify_db::servers::{self, ColumnValue as C, NewServer};
use coolify_domain::naming::strip_tags;
use coolify_domain::new_public_id;
use coolify_jobs::cloud::{self, Client, CloudError, Provider};
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{Input, lenient_json, validated_json};
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::server_settings::{authorize_update, json_status, validation_errors};
use crate::routes::servers::{as_php_string, deploy_forbidden, input_bool, sentinel_url};
use crate::serialize::php_truthy;
use crate::state::AppState;
use crate::validation::{CUSTOM_API_MESSAGES, Field, Rule, validate};

const DEPLOY_REASON: &str = "Missing required permissions: deploy. A cloud-init script or extra SSH keys run as root on the new server, so it needs a token with the deploy permission.";
const DEPLOY_REASON_HOSTINGER: &str = "Missing required permissions: deploy. A post-install script or extra SSH keys run as root on the new server, so it needs a token with the deploy permission.";

/// `required_without:...|string` on the two token fields.
fn token_rule_errors(input: &Input, errors: &mut Map<String, Value>) {
    let present = |k: &str| input.get(k).is_some_and(|v| !v.is_null());
    let pairs = [
        (
            "cloud_provider_token_uuid",
            "cloud_provider_token_id",
            "cloud provider token uuid",
            "cloud provider token id",
        ),
        (
            "cloud_provider_token_id",
            "cloud_provider_token_uuid",
            "cloud provider token id",
            "cloud provider token uuid",
        ),
    ];
    for (name, other, label, other_label) in pairs {
        if !present(name) && !present(other) {
            errors.insert(
                name.into(),
                json!([format!(
                    "The {label} field is required when {other_label} is not present."
                )]),
            );
        } else if input.get(name).is_some_and(|v| !v.is_string()) {
            errors.insert(
                name.into(),
                json!([format!("The {label} field must be a string.")]),
            );
        }
    }
}

fn token_uuid(input: &Input) -> String {
    input
        .get("cloud_provider_token_uuid")
        .filter(|v| !v.is_null())
        .or_else(|| input.get("cloud_provider_token_id"))
        .map(as_php_string)
        .unwrap_or_default()
}

/// `CloudProviderToken::whereTeamId()->whereUuid()->where('provider', ...)` with `authorize('view')`,
/// returning the row and the decrypted API token.
async fn provider_token(
    state: &AppState,
    ctx: &AuthCtx,
    input: &Input,
    provider: &str,
    label: &'static str,
) -> Result<(Record, String), Response> {
    let not_found = || {
        json_status(
            StatusCode::NOT_FOUND,
            json!({"message": format!("{label} cloud provider token not found.")}),
        )
    };
    let row = coolify_db::cloud::find_token(&state.db, ctx.team_id, &token_uuid(input))
        .await
        .map_err(|e| ApiError::from(e).into_response())?
        .filter(|r| r.get("provider").and_then(Value::as_str) == Some(provider))
        .ok_or_else(not_found)?;
    authorize_update(ctx).map_err(IntoResponse::into_response)?;
    let enc = state
        .encrypter
        .as_ref()
        .ok_or_else(|| ApiError::Internal.into_response())?;
    let plain = row
        .get("token")
        .and_then(Value::as_str)
        .and_then(|c| enc.decrypt_string(c).ok())
        .ok_or_else(|| ApiError::Internal.into_response())?;
    Ok((row, plain))
}

/// The input and token of a list endpoint.
async fn list_token(
    state: &AppState,
    ctx: &AuthCtx,
    headers: &HeaderMap,
    uri: &Uri,
    body: &[u8],
    provider: &str,
    label: &'static str,
) -> Result<String, Response> {
    let input = lenient_json(headers, uri, body);
    let mut errors = Map::new();
    token_rule_errors(&input, &mut errors);
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors).into_response());
    }
    provider_token(state, ctx, &input, provider, label)
        .await
        .map(|(_, t)| t)
}

fn rate_limit(retry: Option<i64>) -> Response {
    let mut res = json_status(
        StatusCode::TOO_MANY_REQUESTS,
        json!({"message": "Rate limit exceeded. Please try again later."}),
    );
    if let Some(r) = retry
        && let Ok(v) = HeaderValue::from_str(&r.to_string())
    {
        res.headers_mut().insert("Retry-After", v);
    }
    res
}

fn failed(message: String) -> Response {
    json_status(
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({"message": message}),
    )
}

macro_rules! list_route {
    ($name:ident, $provider:literal, $label:literal, $call:expr, $resource:literal) => {
        pub async fn $name(
            State(state): State<AppState>,
            Extension(ctx): Extension<AuthCtx>,
            headers: HeaderMap,
            uri: Uri,
            body: Bytes,
        ) -> ApiResult {
            let token =
                match list_token(&state, &ctx, &headers, &uri, &body, $provider, $label).await {
                    Ok(t) => t,
                    Err(r) => return Ok(r),
                };
            let provider = Provider::from_name($provider).ok_or(ApiError::Internal)?;
            let client = Client::new(provider, &token);
            let call = $call;
            Ok(list_answer(
                $provider,
                call(client).await,
                &format!("Failed to fetch {} {}.", $label, $resource),
            ))
        }
    };
}

/// The answer of a list endpoint. Only `HetznerController` turns a `CloudProviderApiException`
/// into a 502; the Vultr and DigitalOcean list endpoints answer 500 for every failure.
fn list_answer(provider: &str, r: Result<Value, CloudError>, generic: &str) -> Response {
    match r {
        Ok(v) => ok(v).into_response(),
        Err(CloudError::Provider(p)) if provider == "hetzner" => {
            json_status(StatusCode::BAD_GATEWAY, p.json_body(generic))
        }
        Err(e) => {
            // `report($e)`.
            tracing::error!(error = %e, "cloud provider request failed");
            failed(generic.to_string())
        }
    }
}

fn arr(v: Result<Vec<Value>, CloudError>) -> Result<Value, CloudError> {
    v.map(Value::Array)
}

list_route!(
    hetzner_locations,
    "hetzner",
    "Hetzner",
    |c: Client| async move { arr(c.hetzner_locations().await) },
    "locations"
);

/// `HetznerController::serverTypes`: `?location=` keeps the types that can be ordered there.
pub async fn hetzner_server_types(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = lenient_json(&headers, &uri, &body);
    let mut errors = Map::new();
    token_rule_errors(&input, &mut errors);
    // `'location' => 'nullable|string'`.
    if input
        .get("location")
        .is_some_and(|v| !v.is_null() && !v.is_string())
    {
        errors.insert(
            "location".into(),
            json!(["The location field must be a string."]),
        );
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    let token = match provider_token(&state, &ctx, &input, "hetzner", "Hetzner").await {
        Ok((_, t)) => t,
        Err(r) => return Ok(r),
    };
    // `$request->query('location')`: only the query string counts here.
    // (After `TrimStrings` and `ConvertEmptyStringsToNull`.)
    let location = crate::input::query_param(&uri, "location")
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty());
    let client = Client::new(Provider::Hetzner, &token);
    Ok(list_answer(
        "hetzner",
        arr(client
            .hetzner_server_types(location.as_deref(), false)
            .await),
        "Failed to fetch Hetzner server types.",
    ))
}
list_route!(
    hetzner_ssh_keys,
    "hetzner",
    "Hetzner",
    |c: Client| async move { arr(c.hetzner_ssh_keys().await) },
    "SSH keys"
);
list_route!(
    hetzner_firewalls,
    "hetzner",
    "Hetzner",
    |c: Client| async move { arr(c.hetzner_firewalls().await) },
    "firewalls"
);
list_route!(
    hetzner_networks,
    "hetzner",
    "Hetzner",
    |c: Client| async move { arr(c.hetzner_networks().await) },
    "networks"
);
list_route!(
    hetzner_images,
    "hetzner",
    "Hetzner",
    |c: Client| async move {
        c.hetzner_images().await.map(|images| {
            Value::Array(
                images
                    .into_iter()
                    .filter(|i| {
                        i.get("type")
                            .is_none_or(|t| t.is_null() || t.as_str() == Some("system"))
                            && i.get("deprecated") != Some(&Value::Bool(true))
                    })
                    .collect(),
            )
        })
    },
    "images"
);
list_route!(
    vultr_regions,
    "vultr",
    "Vultr",
    |c: Client| async move { arr(c.vultr_regions().await) },
    "regions"
);
list_route!(
    vultr_plans,
    "vultr",
    "Vultr",
    |c: Client| async move { arr(c.vultr_plans().await) },
    "plans"
);
list_route!(
    vultr_os,
    "vultr",
    "Vultr",
    |c: Client| async move { arr(c.vultr_os().await) },
    "operating systems"
);
list_route!(
    vultr_ssh_keys,
    "vultr",
    "Vultr",
    |c: Client| async move { arr(c.vultr_ssh_keys().await) },
    "SSH keys"
);
list_route!(
    digitalocean_regions,
    "digitalocean",
    "DigitalOcean",
    |c: Client| async move { arr(c.do_regions().await) },
    "regions"
);
list_route!(
    digitalocean_sizes,
    "digitalocean",
    "DigitalOcean",
    |c: Client| async move { arr(c.do_sizes().await) },
    "sizes"
);
list_route!(
    digitalocean_images,
    "digitalocean",
    "DigitalOcean",
    |c: Client| async move { arr(c.do_images().await) },
    "images"
);
list_route!(
    digitalocean_ssh_keys,
    "digitalocean",
    "DigitalOcean",
    |c: Client| async move { arr(c.do_ssh_keys().await) },
    "SSH keys"
);

macro_rules! hostinger_route {
    ($name:ident, $method:ident, $resource:literal) => {
        pub async fn $name(
            State(state): State<AppState>,
            Extension(ctx): Extension<AuthCtx>,
            headers: HeaderMap,
            uri: Uri,
            body: Bytes,
        ) -> ApiResult {
            let token = match list_token(&state, &ctx, &headers, &uri, &body, "hostinger", "Hostinger").await {
                Ok(t) => t,
                Err(r) => return Ok(r),
            };
            let client = Client::new(Provider::Hostinger, &token);
            Ok(match client.$method().await {
                Ok(v) => ok(v).into_response(),
                Err(CloudError::RateLimit(r)) => rate_limit(r),
                Err(CloudError::Api(m, code @ (401 | 403))) => {
                    let _ = code;
                    let detail = m.strip_prefix("Hostinger API error: ");
                    match detail {
                        Some(d) => json_status(
                            StatusCode::FORBIDDEN,
                            json!({"message": format!("Hostinger denied access to {}: {d}", $resource)}),
                        ),
                        None => failed(format!("Failed to fetch Hostinger {}.", $resource)),
                    }
                }
                Err(_) => failed(format!("Failed to fetch Hostinger {}.", $resource)),
            })
        }
    };
}

hostinger_route!(
    hostinger_data_centers,
    hostinger_data_centers,
    "data centers"
);
hostinger_route!(hostinger_catalog, hostinger_catalog, "catalog");
hostinger_route!(hostinger_templates, hostinger_templates, "templates");
hostinger_route!(hostinger_ssh_keys, hostinger_public_keys, "SSH keys");
hostinger_route!(
    hostinger_post_install_scripts,
    hostinger_post_install_scripts,
    "post-install scripts"
);

// ── Creating servers ────────────────────────────────────────────────────

/// `ValidHostname`.
pub(crate) fn hostname_rule(v: &Value) -> Option<String> {
    let text = v.as_str()?;
    if text.is_empty() || text == "0" {
        return None;
    }
    let host = coolify_remote::text::php_trim(text);
    if host.len() > 253 {
        return Some("The :attribute must not exceed 253 characters.".to_string());
    }
    const INVALID: &str = "The :attribute contains invalid characters. Only letters (a-z, A-Z), numbers (0-9), hyphens (-), and dots (.) are allowed.";
    if host.chars().any(|c| {
        (c as u32) < 0x20 || c as u32 == 0x7f || ";|&$`(){}<>\"'\\!*?[]~^:#@%=+, ".contains(c)
    }) {
        return Some(INVALID.to_string());
    }
    if host.starts_with('.') || host.ends_with('.') || host.contains("..") {
        return Some("The :attribute cannot start or end with a dot.".to_string());
    }
    for label in host.split('.') {
        if label.is_empty() {
            return Some("The :attribute cannot contain consecutive dots.".to_string());
        }
        if label.len() > 63 {
            return Some("The :attribute labels must be 63 characters or less.".to_string());
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Some("The :attribute labels cannot start or end with a hyphen.".to_string());
        }
        if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Some(
                "The :attribute may only contain lowercase letters, numbers, hyphens and dots."
                    .to_string(),
            );
        }
    }
    None
}

pub(crate) fn integer_items(input: &Input, field: &str, errors: &mut Map<String, Value>) {
    if let Some(Value::Array(items)) = input.get(field) {
        for (i, v) in items.iter().enumerate() {
            let ok = v.is_i64() || v.as_str().is_some_and(|s| s.parse::<i64>().is_ok());
            if !ok {
                errors.insert(
                    format!("{field}.{i}"),
                    json!([format!(
                        "The {} field must be an integer.",
                        format!("{field}.{i}").replace('_', " ")
                    )]),
                );
            }
        }
    }
}

pub(crate) fn string_items(input: &Input, field: &str, errors: &mut Map<String, Value>) {
    if let Some(Value::Array(items)) = input.get(field) {
        for (i, v) in items.iter().enumerate() {
            if !v.is_string() {
                errors.insert(
                    format!("{field}.{i}"),
                    json!([format!(
                        "The {} field must be a string.",
                        format!("{field}.{i}").replace('_', " ")
                    )]),
                );
            }
        }
    }
}

/// `customApiValidator` with the token rules added, then the extra fields.
fn validate_create(
    input: &Input,
    fields: &[Field],
    allowed: &[&str],
    extra: impl FnOnce(&mut Map<String, Value>),
) -> Result<(), ApiError> {
    let mut errors = Map::new();
    token_rule_errors(input, &mut errors);
    extra(&mut errors);
    let rest = validate(input, fields, CUSTOM_API_MESSAGES, allowed);
    match rest {
        Ok(()) if errors.is_empty() => Ok(()),
        Ok(()) => Err(ApiError::Validation(errors)),
        Err(ApiError::Validation(mut more)) => {
            errors.append(&mut more);
            Err(ApiError::Validation(errors))
        }
        Err(e) => Err(e),
    }
}

fn name_field() -> Field {
    Field {
        name: "name",
        rules: vec![
            Rule::Nullable,
            Rule::String,
            Rule::Max(253),
            Rule::CustomOwned(hostname_rule),
        ],
    }
}

/// `ValidCloudInitYaml`.
fn cloud_init_rule(v: &Value) -> Option<String> {
    let text = v.as_str()?;
    if text.is_empty() || text == "0" {
        return None;
    }
    let script = coolify_remote::text::php_trim(text);
    if script.starts_with("#!") {
        return None;
    }
    if let Some(rest) = script.strip_prefix("#cloud-config") {
        return coolify_compat::yaml_parse::parse(rest.trim_start(), 128)
            .err()
            .map(|e| {
                format!(
                    "The :attribute must be valid YAML format. Error: {}",
                    e.message()
                )
            });
    }
    coolify_compat::yaml_parse::parse(script, 128).err().map(|e| {
        format!(
            "The :attribute must be either a valid bash script (starting with #!) or valid cloud-config YAML. YAML parse error: {}",
            e.message()
        )
    })
}

pub(crate) fn cloud_init_field() -> Field {
    Field {
        name: "cloud_init_script",
        rules: vec![
            Rule::Nullable,
            Rule::String,
            Rule::CustomOwned(cloud_init_rule),
        ],
    }
}

fn bool_field(name: &'static str) -> Field {
    Field {
        name,
        rules: vec![Rule::Nullable, Rule::Boolean],
    }
}

fn required_string(name: &'static str) -> Field {
    Field {
        name,
        rules: vec![Rule::Required, Rule::String],
    }
}

fn array_field(name: &'static str) -> Field {
    Field {
        name,
        rules: vec![Rule::Nullable, Rule::Array],
    }
}

fn not_empty(input: &Input, k: &str) -> bool {
    input.get(k).is_some_and(php_truthy)
}

/// `generate_random_name` when no name is given, then `strtolower(trim())`.
fn server_name(input: &Input) -> String {
    let name = match input.get("name").filter(|v| php_truthy(v)) {
        Some(v) => as_php_string(v),
        None => crate::routes::private_keys::random_name(),
    };
    coolify_remote::text::php_trim(&name).to_lowercase()
}

fn bool_or(input: &Input, k: &str, default: bool) -> bool {
    match input.get(k) {
        None | Some(Value::Null) => default,
        Some(_) => input_bool(input, k),
    }
}

async fn private_key(
    state: &AppState,
    ctx: &AuthCtx,
    input: &Input,
) -> Result<(KeyRow, String, String), Response> {
    let not_found = || {
        json_status(
            StatusCode::NOT_FOUND,
            json!({"message": "Private key not found."}),
        )
    };
    let key = private_keys::find(
        &state.db,
        ctx.team_id,
        input.str("private_key_uuid").unwrap_or_default(),
    )
    .await
    .map_err(|e| ApiError::from(e).into_response())?
    .ok_or_else(not_found)?;
    let (public, fingerprint) = key_material(state, &key).map_err(IntoResponse::into_response)?;
    Ok((key, public, fingerprint))
}

/// `$privateKey->getPublicKey()` and `PrivateKey::generateMd5Fingerprint`.
pub(crate) fn key_material(state: &AppState, key: &KeyRow) -> Result<(String, String), ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let pem = enc
        .decrypt_string(&key.private_key_encrypted)
        .map_err(|_| ApiError::Internal)?;
    let public = coolify_compat::sshkey::inspect(&pem)
        .map_or_else(|| "Error loading private key".to_string(), |i| i.public_key);
    let fingerprint = md5_fingerprint(&public);
    Ok((public, fingerprint))
}

/// `PrivateKey::generateMd5Fingerprint`.
pub(crate) fn md5_fingerprint(public_key: &str) -> String {
    use md5::{Digest, Md5};
    let blob = public_key
        .split_whitespace()
        .nth(1)
        .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
        .unwrap_or_default();
    Md5::digest(&blob)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

pub(crate) struct Created {
    pub(crate) id: i64,
    pub(crate) uuid: String,
}

/// `Team::createServerWithinLimit` + the proxy attribute of a provider server.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn create_row(
    state: &AppState,
    ctx: &AuthCtx,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: &HeaderMap,
    name: &str,
    ip: &str,
    key: &KeyRow,
    columns: &[(&'static str, C)],
) -> Result<Created, ApiError> {
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let uuid = new_public_id();
    let peer = peer
        .map(|c| (c.0).0)
        .unwrap_or_else(|| SocketAddr::from((IpAddr::from([127, 0, 0, 1]), 0)));
    let token =
        enc.encrypt_string(&enc.encrypt_serialized(&json!({"server_uuid": uuid}).to_string()));
    let stored = strip_tags(Some(name));
    let new = NewServer {
        sentinel_custom_url: sentinel_url(state, ip, peer, headers).await?,
        sentinel_token_encrypted: token,
        shared_vars: [
            ("COOLIFY_SERVER_UUID", enc.encrypt_string(&uuid)),
            ("COOLIFY_SERVER_NAME", enc.encrypt_string(&stored)),
        ],
        uuid: uuid.clone(),
        name: stored,
        description: None,
        ip: ip.to_string(),
        port: 22,
        user: "root".to_string(),
        private_key_id: key.id,
        team_id: ctx.team_id,
        proxy: json!({"redirect_enabled": true, "status": "exited", "type": "TRAEFIK"}).to_string(),
        docker_uuid: new_public_id(),
        server_role: "both",
        always_write_role: false,
    };
    let id = servers::create(&state.db, &new, state.now()).await?;
    servers::update_columns(&state.db, id, columns, state.now()).await?;
    Ok(Created { id, uuid })
}

fn current_ip(row: Option<coolify_db::servers::ServerRow>) -> Value {
    row.and_then(|r| r.record.get("ip").cloned())
        .unwrap_or(Value::Null)
}

/// The `RateLimitException`, `CloudProviderApiException` and `\Throwable` answers of a create.
fn provider_error(e: CloudError, generic: &str) -> Response {
    match e {
        CloudError::RateLimit(r) => rate_limit(r),
        CloudError::Provider(p) => json_status(StatusCode::BAD_GATEWAY, p.json_body(generic)),
        CloudError::Api(m, _) => {
            tracing::error!(error = %m, "cloud provider request failed");
            failed(generic.to_string())
        }
    }
}

const INSTANT_VALIDATE_REASON: &str = "Missing required permissions: deploy. Instant validation installs Docker and starts the proxy on the new server, so it needs a token with the deploy permission.";

/// `instant_validate`: as given, else true for a token with the deploy permission; a true value
/// needs that permission.
fn instant_validate(input: &Input, ctx: &AuthCtx) -> Result<bool, ApiError> {
    let wanted = if input.filled("instant_validate") {
        input_bool(input, "instant_validate")
    } else {
        deploy_forbidden(ctx, INSTANT_VALIDATE_REASON).is_ok()
    };
    if wanted {
        deploy_forbidden(ctx, INSTANT_VALIDATE_REASON)?;
    }
    Ok(wanted)
}

/// `ValidateNewCloudServerJob::dispatch($server)`.
fn spawn_new_server_validation(state: &AppState, c: &Created) {
    if let Some(jobs) = &state.jobs {
        jobs.spawn_validate_new_cloud_server(c.id);
    }
}

pub async fn hetzner_create_server(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    authorize_update(&ctx)?;
    let input = validated_json(&headers, &uri, &body)?;
    const ALLOWED: &[&str] = &[
        "cloud_provider_token_uuid",
        "cloud_provider_token_id",
        "location",
        "server_type",
        "image",
        "name",
        "private_key_uuid",
        "enable_ipv4",
        "enable_ipv6",
        "enable_backups",
        "hetzner_ssh_key_ids",
        "hetzner_firewall_ids",
        "hetzner_network_ids",
        "cloud_init_script",
        "instant_validate",
    ];
    validate_create(
        &input,
        &[
            required_string("location"),
            required_string("server_type"),
            Field {
                name: "image",
                rules: vec![Rule::Required, Rule::Integer],
            },
            name_field(),
            required_string("private_key_uuid"),
            bool_field("enable_ipv4"),
            bool_field("enable_ipv6"),
            bool_field("enable_backups"),
            array_field("hetzner_ssh_key_ids"),
            array_field("hetzner_firewall_ids"),
            array_field("hetzner_network_ids"),
            cloud_init_field(),
            bool_field("instant_validate"),
        ],
        ALLOWED,
        |errors| {
            for f in [
                "hetzner_ssh_key_ids",
                "hetzner_firewall_ids",
                "hetzner_network_ids",
            ] {
                integer_items(&input, f, errors);
            }
        },
    )?;
    if not_empty(&input, "cloud_init_script") || not_empty(&input, "hetzner_ssh_key_ids") {
        deploy_forbidden(&ctx, DEPLOY_REASON)?;
    }
    let validate_now = instant_validate(&input, &ctx)?;
    let name = server_name(&input);
    let v4 = bool_or(&input, "enable_ipv4", true);
    let v6 = bool_or(&input, "enable_ipv6", true);
    if !v4 && !v6 {
        return Err(validation_errors(&[
            ("enable_ipv4", "Enable at least one public IP protocol."),
            ("enable_ipv6", "Enable at least one public IP protocol."),
        ]));
    }
    let (token_row, token) = match provider_token(&state, &ctx, &input, "hetzner", "Hetzner").await
    {
        Ok(t) => t,
        Err(r) => return Ok(r),
    };
    let (key, public, fingerprint) = match private_key(&state, &ctx, &input).await {
        Ok(k) => k,
        Err(r) => return Ok(r),
    };
    const GENERIC: &str = "Failed to create Hetzner server.";
    let client = Client::new(Provider::Hetzner, &token);
    let keys = match client.hetzner_ssh_keys().await {
        Ok(k) => k,
        Err(e) => return Ok(provider_error(e, GENERIC)),
    };
    let existing = keys
        .iter()
        .find(|k| k.get("fingerprint").and_then(Value::as_str) == Some(fingerprint.as_str()))
        .and_then(|k| k.get("id").cloned());
    let ssh_key = match existing {
        Some(id) => id,
        None => match client.hetzner_upload_ssh_key(&key.name, &public).await {
            Ok(k) => k.get("id").cloned().unwrap_or(Value::Null),
            Err(e) => return Ok(provider_error(e, GENERIC)),
        },
    };
    let unique = |values: Vec<Value>| {
        let mut out: Vec<Value> = Vec::new();
        for v in values {
            if !out.contains(&v) {
                out.push(v);
            }
        }
        out
    };
    let list = |k: &str| {
        input
            .get(k)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut all_keys = vec![ssh_key];
    all_keys.extend(list("hetzner_ssh_key_ids"));
    let mut params = Map::new();
    params.insert("name".into(), json!(name));
    params.insert(
        "server_type".into(),
        input.get("server_type").cloned().unwrap_or(Value::Null),
    );
    params.insert(
        "image".into(),
        input.get("image").cloned().unwrap_or(Value::Null),
    );
    params.insert(
        "location".into(),
        input.get("location").cloned().unwrap_or(Value::Null),
    );
    params.insert("start_after_create".into(), json!(true));
    params.insert("ssh_keys".into(), Value::Array(unique(all_keys)));
    params.insert(
        "public_net".into(),
        json!({
            "enable_ipv4": input.get("enable_ipv4").filter(|v| !v.is_null()).cloned().unwrap_or(json!(true)),
            "enable_ipv6": input.get("enable_ipv6").filter(|v| !v.is_null()).cloned().unwrap_or(json!(true)),
        }),
    );
    let firewalls = unique(list("hetzner_firewall_ids"));
    if !firewalls.is_empty() {
        params.insert(
            "firewalls".into(),
            Value::Array(
                firewalls
                    .into_iter()
                    .map(|f| json!({"firewall": f}))
                    .collect(),
            ),
        );
    }
    let networks = unique(list("hetzner_network_ids"));
    if !networks.is_empty() {
        params.insert("networks".into(), Value::Array(networks));
    }
    if not_empty(&input, "cloud_init_script") {
        params.insert(
            "user_data".into(),
            input
                .get("cloud_init_script")
                .cloned()
                .unwrap_or(Value::Null),
        );
    }
    let created = match client.hetzner_create_server(&Value::Object(params)).await {
        Ok(s) => s,
        Err(e) => return Ok(provider_error(e, GENERIC)),
    };
    let ip = if v4
        && created
            .pointer("/public_net/ipv4/ip")
            .is_some_and(|v| !v.is_null())
    {
        created
            .pointer("/public_net/ipv4/ip")
            .and_then(Value::as_str)
            .map(str::to_string)
    } else if v6 {
        cloud::hetzner_server_ipv6(
            created
                .pointer("/public_net/ipv6/ip")
                .and_then(Value::as_str),
        )
    } else {
        None
    };
    let server_id = created.get("id").cloned().unwrap_or(Value::Null);
    // The `catch` blocks delete the server at Hetzner when it could not be tracked.
    let delete_untracked = || async {
        if let Some(id) = server_id.as_i64()
            && let Err(e) = client.hetzner_delete_server(id).await
        {
            tracing::error!(error = %e, "deleting the untracked Hetzner server failed");
        }
    };
    let Some(ip) = ip.filter(|i| !i.is_empty()) else {
        tracing::error!("No public IP address available. Enable at least one of IPv4 or IPv6.");
        delete_untracked().await;
        return Ok(failed(GENERIC.to_string()));
    };
    let c = match create_row(
        &state,
        &ctx,
        peer,
        &headers,
        &name,
        &ip,
        &key,
        &[
            (
                "cloud_provider_token_id",
                C::Int(
                    token_row
                        .get("id")
                        .and_then(Value::as_i64)
                        .unwrap_or_default(),
                ),
            ),
            (
                "hetzner_server_id",
                C::Int(server_id.as_i64().unwrap_or_default()),
            ),
            (
                "hetzner_server_status",
                C::Text(
                    created
                        .get("status")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                ),
            ),
        ],
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            delete_untracked().await;
            return Err(e);
        }
    };
    if bool_or(&input, "enable_backups", false)
        && let Err(e) = client
            .hetzner_enable_backup(server_id.as_i64().unwrap_or_default())
            .await
    {
        tracing::warn!(error = %e, "enabling Hetzner backups failed");
    }
    if validate_now {
        spawn_new_server_validation(&state, &c);
    }
    Ok(json_status(
        StatusCode::CREATED,
        json!({"uuid": c.uuid, "hetzner_server_id": server_id, "ip": ip}),
    ))
}

pub async fn vultr_create_server(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let input = validated_json(&headers, &uri, &body)?;
    const ALLOWED: &[&str] = &[
        "cloud_provider_token_uuid",
        "cloud_provider_token_id",
        "region",
        "plan",
        "os_id",
        "name",
        "private_key_uuid",
        "enable_ipv6",
        "disable_public_ipv4",
        "vultr_ssh_key_ids",
        "cloud_init_script",
        "instant_validate",
    ];
    validate_create(
        &input,
        &[
            required_string("region"),
            required_string("plan"),
            Field {
                name: "os_id",
                rules: vec![Rule::Required, Rule::Integer],
            },
            name_field(),
            required_string("private_key_uuid"),
            bool_field("enable_ipv6"),
            bool_field("disable_public_ipv4"),
            array_field("vultr_ssh_key_ids"),
            cloud_init_field(),
            bool_field("instant_validate"),
        ],
        ALLOWED,
        |errors| string_items(&input, "vultr_ssh_key_ids", errors),
    )?;
    if not_empty(&input, "cloud_init_script") || not_empty(&input, "vultr_ssh_key_ids") {
        deploy_forbidden(&ctx, DEPLOY_REASON)?;
    }
    let validate_now = instant_validate(&input, &ctx)?;
    let name = server_name(&input);
    let v6_value = input
        .get("enable_ipv6")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(json!(true));
    let no_v4_value = input
        .get("disable_public_ipv4")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(json!(false));
    let v6 = php_truthy(&v6_value);
    let no_v4 = php_truthy(&no_v4_value);
    if no_v4 && !v6 {
        return Err(validation_errors(&[(
            "enable_ipv6",
            "Enable IPv6 when disabling public IPv4.",
        )]));
    }
    let (token_row, token) = match provider_token(&state, &ctx, &input, "vultr", "Vultr").await {
        Ok(t) => t,
        Err(r) => return Ok(r),
    };
    let (key, public, _) = match private_key(&state, &ctx, &input).await {
        Ok(k) => k,
        Err(r) => return Ok(r),
    };
    const GENERIC: &str = "Failed to create Vultr server.";
    let client = Client::new(Provider::Vultr, &token);
    let normalize = |k: &str| k.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
    let keys = match client.vultr_ssh_keys().await {
        Ok(k) => k,
        Err(e) => return Ok(provider_error(e, GENERIC)),
    };
    let wanted = normalize(&public);
    let existing = keys
        .iter()
        .find(|k| normalize(k.get("ssh_key").and_then(Value::as_str).unwrap_or_default()) == wanted)
        .and_then(|k| k.get("id").cloned());
    let ssh_key = match existing {
        Some(id) => id,
        None => match client.vultr_upload_ssh_key(&key.name, &public).await {
            Ok(k) => k.get("id").cloned().unwrap_or(Value::Null),
            Err(e) => return Ok(provider_error(e, GENERIC)),
        },
    };
    let mut ssh_keys = vec![ssh_key];
    for k in input
        .get("vultr_ssh_key_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if !ssh_keys.contains(k) {
            ssh_keys.push(k.clone());
        }
    }
    let mut params = Map::new();
    params.insert(
        "region".into(),
        input.get("region").cloned().unwrap_or(Value::Null),
    );
    params.insert(
        "plan".into(),
        input.get("plan").cloned().unwrap_or(Value::Null),
    );
    params.insert(
        "os_id".into(),
        input.get("os_id").cloned().unwrap_or(Value::Null),
    );
    params.insert("label".into(), json!(name));
    params.insert("hostname".into(), json!(name));
    params.insert("sshkey_id".into(), Value::Array(ssh_keys));
    params.insert("enable_ipv6".into(), v6_value);
    params.insert("disable_public_ipv4".into(), no_v4_value);
    if not_empty(&input, "cloud_init_script") {
        params.insert(
            "user_data".into(),
            input
                .get("cloud_init_script")
                .cloned()
                .unwrap_or(Value::Null),
        );
    }
    let instance = match client.vultr_create_instance(&Value::Object(params)).await {
        Ok(i) => i,
        Err(e) => return Ok(provider_error(e, GENERIC)),
    };
    let instance_id = instance.get("id").map(as_php_string).unwrap_or_default();
    let ip = Client::vultr_public_ip(&instance, no_v4, v6).unwrap_or_else(|| "1.2.3.4".to_string());
    let status = instance
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string);
    let created = create_row(
        &state,
        &ctx,
        peer,
        &headers,
        &name,
        &ip,
        &key,
        &[
            (
                "cloud_provider_token_id",
                C::Int(
                    token_row
                        .get("id")
                        .and_then(Value::as_i64)
                        .unwrap_or_default(),
                ),
            ),
            ("vultr_instance_id", C::Text(Some(instance_id.clone()))),
            ("vultr_instance_status", C::Text(status.clone())),
        ],
    )
    .await;
    let c = match created {
        Ok(c) => c,
        Err(e) => {
            // `deleteUntrackedInstance`.
            let _ = client.vultr_delete_instance(&instance_id).await;
            return Err(e);
        }
    };
    if let Ok(i) = client.vultr_wait_for_ip(instance, no_v4, v6).await
        && let Some(assigned) = Client::vultr_public_ip(&i, no_v4, v6).filter(|a| *a != ip)
    {
        let st = i
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(status);
        servers::update_columns(
            &state.db,
            c.id,
            &[
                ("ip", C::Text(Some(assigned))),
                ("vultr_instance_status", C::Text(st)),
            ],
            state.now(),
        )
        .await?;
    }
    if validate_now {
        spawn_new_server_validation(&state, &c);
    }
    let fresh = servers::find_by_id(&state.db, c.id).await?;
    Ok(json_status(
        StatusCode::CREATED,
        json!({"uuid": c.uuid, "vultr_instance_id": instance_id, "ip": current_ip(fresh)}),
    ))
}

pub async fn digitalocean_create_server(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    authorize_update(&ctx)?;
    let input = validated_json(&headers, &uri, &body)?;
    const ALLOWED: &[&str] = &[
        "cloud_provider_token_uuid",
        "cloud_provider_token_id",
        "region",
        "size",
        "image",
        "name",
        "private_key_uuid",
        "enable_ipv6",
        "monitoring",
        "digitalocean_ssh_key_ids",
        "cloud_init_script",
        "instant_validate",
    ];
    validate_create(
        &input,
        &[
            required_string("region"),
            required_string("size"),
            Field {
                name: "image",
                rules: vec![Rule::Required],
            },
            name_field(),
            required_string("private_key_uuid"),
            bool_field("enable_ipv6"),
            bool_field("monitoring"),
            array_field("digitalocean_ssh_key_ids"),
            cloud_init_field(),
            bool_field("instant_validate"),
        ],
        ALLOWED,
        |errors| integer_items(&input, "digitalocean_ssh_key_ids", errors),
    )?;
    if not_empty(&input, "cloud_init_script") || not_empty(&input, "digitalocean_ssh_key_ids") {
        deploy_forbidden(&ctx, DEPLOY_REASON)?;
    }
    let name = server_name(&input);
    let v6 = bool_or(&input, "enable_ipv6", true);
    let monitoring = bool_or(&input, "monitoring", true);
    let validate_now = instant_validate(&input, &ctx)?;
    let (token_row, token) =
        match provider_token(&state, &ctx, &input, "digitalocean", "DigitalOcean").await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };
    let (key, public, fingerprint) = match private_key(&state, &ctx, &input).await {
        Ok(k) => k,
        Err(r) => return Ok(r),
    };
    const GENERIC: &str = "Failed to create DigitalOcean server.";
    let client = Client::new(Provider::DigitalOcean, &token);
    let keys = match client.do_ssh_keys().await {
        Ok(k) => k,
        Err(e) => return Ok(provider_error(e, GENERIC)),
    };
    let existing = keys
        .iter()
        .find(|k| k.get("fingerprint").and_then(Value::as_str) == Some(fingerprint.as_str()))
        .and_then(|k| k.get("id").cloned());
    let ssh_key = match existing {
        Some(id) => id,
        None => match client.do_upload_ssh_key(&key.name, &public).await {
            Ok(k) => k.get("id").cloned().unwrap_or(Value::Null),
            Err(e) => return Ok(provider_error(e, GENERIC)),
        },
    };
    let mut ssh_keys = vec![ssh_key];
    for k in input
        .get("digitalocean_ssh_key_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if !ssh_keys.contains(k) {
            ssh_keys.push(k.clone());
        }
    }
    let mut params = Map::new();
    params.insert("name".into(), json!(name));
    params.insert(
        "region".into(),
        input.get("region").cloned().unwrap_or(Value::Null),
    );
    params.insert(
        "size".into(),
        input.get("size").cloned().unwrap_or(Value::Null),
    );
    params.insert(
        "image".into(),
        input.get("image").cloned().unwrap_or(Value::Null),
    );
    params.insert("ssh_keys".into(), Value::Array(ssh_keys));
    params.insert("ipv6".into(), json!(v6));
    params.insert("monitoring".into(), json!(monitoring));
    if not_empty(&input, "cloud_init_script") {
        params.insert(
            "user_data".into(),
            input
                .get("cloud_init_script")
                .cloned()
                .unwrap_or(Value::Null),
        );
    }
    let droplet = match client.do_create_droplet(&Value::Object(params)).await {
        Ok(d) => d,
        Err(e) => return Ok(provider_error(e, GENERIC)),
    };
    let droplet_id = droplet
        .get("id")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let status = droplet
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string);
    let created = create_row(
        &state,
        &ctx,
        peer,
        &headers,
        &name,
        "1.2.3.4",
        &key,
        &[
            (
                "cloud_provider_token_id",
                C::Int(
                    token_row
                        .get("id")
                        .and_then(Value::as_i64)
                        .unwrap_or_default(),
                ),
            ),
            ("digitalocean_droplet_id", C::Int(droplet_id)),
            ("digitalocean_droplet_status", C::Text(status.clone())),
        ],
    )
    .await;
    let c = match created {
        Ok(c) => c,
        Err(e) => {
            let _ = client.do_delete_droplet(droplet_id).await;
            return Err(e);
        }
    };
    if let Ok(d) = client.do_wait_for_ip(droplet, v6).await
        && let Some(ip) = Client::do_public_ip(&d, true, v6)
    {
        let st = d
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(status);
        servers::update_columns(
            &state.db,
            c.id,
            &[
                ("ip", C::Text(Some(ip))),
                ("digitalocean_droplet_status", C::Text(st)),
            ],
            state.now(),
        )
        .await?;
    }
    if validate_now {
        spawn_new_server_validation(&state, &c);
    }
    let fresh = servers::find_by_id(&state.db, c.id).await?;
    Ok(json_status(
        StatusCode::CREATED,
        json!({"uuid": c.uuid, "digitalocean_droplet_id": droplet_id, "ip": current_ip(fresh)}),
    ))
}

pub async fn hostinger_create_server(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    authorize_update(&ctx)?;
    let input = validated_json(&headers, &uri, &body)?;
    const ALLOWED: &[&str] = &[
        "cloud_provider_token_uuid",
        "cloud_provider_token_id",
        "item_id",
        "data_center_id",
        "template_id",
        "name",
        "private_key_uuid",
        "enable_backups",
        "public_key_ids",
        "post_install_script_id",
        "instant_validate",
    ];
    validate_create(
        &input,
        &[
            required_string("item_id"),
            Field {
                name: "data_center_id",
                rules: vec![Rule::Required, Rule::Integer],
            },
            Field {
                name: "template_id",
                rules: vec![Rule::Required, Rule::Integer],
            },
            name_field(),
            required_string("private_key_uuid"),
            bool_field("enable_backups"),
            array_field("public_key_ids"),
            Field {
                name: "post_install_script_id",
                rules: vec![Rule::Nullable, Rule::Integer],
            },
            bool_field("instant_validate"),
        ],
        ALLOWED,
        |errors| integer_items(&input, "public_key_ids", errors),
    )?;
    let public_keys: Vec<i64> = input
        .get("public_key_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .collect();
    if input.filled("post_install_script_id") || !public_keys.is_empty() {
        deploy_forbidden(&ctx, DEPLOY_REASON_HOSTINGER)?;
    }
    let validate_now = instant_validate(&input, &ctx)?;
    let (token_row, token) =
        match provider_token(&state, &ctx, &input, "hostinger", "Hostinger").await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };
    let (key, public, _) = match private_key(&state, &ctx, &input).await {
        Ok(k) => k,
        Err(r) => return Ok(r),
    };
    let client = Client::new(Provider::Hostinger, &token);
    let name = server_name(&input);
    let int = |k: &str| input.get(k).map_or(0, crate::routes::servers::int_value);
    let mut setup = Map::new();
    setup.insert("data_center_id".into(), json!(int("data_center_id")));
    setup.insert("template_id".into(), json!(int("template_id")));
    setup.insert("hostname".into(), json!(name));
    setup.insert(
        "enable_backups".into(),
        json!(input_bool(&input, "enable_backups")),
    );
    setup.insert(
        "public_key".into(),
        json!({"name": key.name, "key": public}),
    );
    if input.filled("post_install_script_id") {
        setup.insert(
            "post_install_script_id".into(),
            json!(int("post_install_script_id")),
        );
    }
    let vm = match client
        .hostinger_purchase(&json!({
            "item_id": input.get("item_id").cloned().unwrap_or(Value::Null),
            "setup": Value::Object(setup),
        }))
        .await
    {
        Ok(v) => v,
        Err(CloudError::RateLimit(r)) => return Ok(rate_limit(r)),
        Err(CloudError::Api(m, 202)) => {
            return Ok(json_status(StatusCode::ACCEPTED, json!({"message": m})));
        }
        Err(_) => return Ok(failed("Failed to create Hostinger server.".to_string())),
    };
    let vm_id = vm.get("id").cloned().unwrap_or(Value::Null);
    let vm_int = vm_id.as_i64().unwrap_or_default();
    let status = vm.get("state").and_then(Value::as_str).map(str::to_string);
    let created = create_row(
        &state,
        &ctx,
        peer,
        &headers,
        &name,
        "1.2.3.4",
        &key,
        &[
            (
                "cloud_provider_token_id",
                C::Int(
                    token_row
                        .get("id")
                        .and_then(Value::as_i64)
                        .unwrap_or_default(),
                ),
            ),
            ("hostinger_virtual_machine_id", C::Int(vm_int)),
            ("hostinger_virtual_machine_status", C::Text(status.clone())),
        ],
    )
    .await;
    let c = match created {
        Ok(c) => c,
        Err(_) => {
            return Ok(json_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({
                    "message": "The Hostinger VPS was purchased but could not be saved in Coolify. Manage it in hPanel.",
                    "hostinger_virtual_machine_id": vm_int,
                }),
            ));
        }
    };
    if !public_keys.is_empty()
        && let Err(e) = client.hostinger_attach_keys(vm_int, &public_keys).await
    {
        tracing::warn!(error = %e, "attaching Hostinger public keys failed");
    }
    if let Ok(v) = client.hostinger_wait_for_ip(vm).await
        && let Some(ip) = Client::hostinger_public_ip(&v)
    {
        let st = v
            .get("state")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(status);
        servers::update_columns(
            &state.db,
            c.id,
            &[
                ("ip", C::Text(Some(ip))),
                ("hostinger_virtual_machine_status", C::Text(st)),
            ],
            state.now(),
        )
        .await?;
    }
    let fresh = servers::find_by_id(&state.db, c.id).await?;
    let ip = current_ip(fresh);
    let placeholder = ip.as_str() == Some("1.2.3.4");
    if validate_now {
        spawn_new_server_validation(&state, &c);
    }
    Ok(json_status(
        StatusCode::CREATED,
        json!({
            "uuid": c.uuid,
            "hostinger_virtual_machine_id": vm_id,
            "ip": ip,
            "provisioning": placeholder,
        }),
    ))
}
