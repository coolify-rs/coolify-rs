//! `DnsController`, `ResourceDnsRecords` and `ServerDnsAddresses` (v4.6.0): the Cloudflare zones of
//! the team, and the managed DNS records of an application's or a service's hostnames.

use std::collections::BTreeMap;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use coolify_db::managed_dns as store;
use coolify_jobs::integrations::cloudflare::{self as cf, Deletion, DnsError, DnsResource};
use serde_json::{Map, Value, json};

use crate::auth::AuthCtx;
use crate::input::{query_param, validated_json};
use crate::integration_token_actions::{automatic_dns_enabled, proxied_dns_enabled};
use crate::response::{ApiError, ApiResult, created, ok};
use crate::state::AppState;

const APPLICATION: &str = "App\\Models\\Application";
const SERVICE_APPLICATION: &str = "App\\Models\\ServiceApplication";

/// An application or a service (`Application|Service $resource`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Application,
    Service,
}

/// The resource of the request, with what the DNS records need.
struct Resource {
    team_id: i64,
    server_id: Option<i64>,
    /// `domainModels()`: the application, or the applications of the service.
    models: Vec<DnsResource>,
}

fn message(status: StatusCode, text: impl Into<String>) -> ApiError {
    ApiError::Status(status, json!({"message": text.into()}))
}

fn validation(field: &str, text: impl Into<String>) -> ApiError {
    let mut errors = Map::new();
    errors.insert(field.to_owned(), json!([text.into()]));
    ApiError::Validation(errors)
}

/// `normalizeHostname()`.
fn normalize_hostname(hostname: &str) -> String {
    hostname.trim().trim_end_matches('.').to_lowercase()
}

/// `busyMessage()`.
fn busy(hostname: &str) -> String {
    format!("Another DNS change for {hostname} is in progress. Try again in a few seconds.")
}

async fn load(state: &AppState, kind: Kind, id: i64) -> Result<Option<Resource>, ApiError> {
    match kind {
        Kind::Application => {
            let Some(a) = coolify_db::applications::find_by_id(&state.db, id).await? else {
                return Ok(None);
            };
            Ok(Some(Resource {
                team_id: a.team_id,
                server_id: a.server_id(),
                models: vec![DnsResource {
                    morph: APPLICATION.to_owned(),
                    id: a.id,
                    uuid: Some(a.uuid.clone()),
                    name: a.str("name").map(str::to_owned),
                    type_name: "application".to_owned(),
                }],
            }))
        }
        Kind::Service => {
            let Some(s) = coolify_db::services::find_by_id(&state.db, id).await? else {
                return Ok(None);
            };
            let rows: Vec<(i64, String, Option<String>)> = sqlx::query_as(
                "SELECT id, uuid, name FROM service_applications WHERE service_id = $1 AND deleted_at IS NULL ORDER BY id",
            )
            .bind(s.id)
            .fetch_all(state.db.pool())
            .await
            .map_err(|_| ApiError::Internal)?;
            Ok(Some(Resource {
                team_id: s.team_id,
                server_id: s.server_id(),
                models: rows
                    .into_iter()
                    .map(|(id, uuid, name)| DnsResource {
                        morph: SERVICE_APPLICATION.to_owned(),
                        id,
                        uuid: Some(uuid),
                        name,
                        type_name: "service_application".to_owned(),
                    })
                    .collect(),
            }))
        }
    }
}

/// `findResource()`: the resource of the token team; `update` needs an admin.
async fn find_resource(
    state: &AppState,
    ctx: &AuthCtx,
    kind: Kind,
    uuid: &str,
    update: bool,
) -> Result<Resource, ApiError> {
    let id = match kind {
        Kind::Application => coolify_db::applications::find_in_team(&state.db, ctx.team_id, uuid)
            .await?
            .map(|a| a.id),
        Kind::Service => coolify_db::services::find_in_team(&state.db, ctx.team_id, uuid)
            .await?
            .map(|s| s.id),
    };
    let not_found = match kind {
        Kind::Application => "Application not found.",
        Kind::Service => "Service not found.",
    };
    let Some(id) = id else {
        return Err(ApiError::Message(StatusCode::NOT_FOUND, not_found));
    };
    if update && !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    load(state, kind, id)
        .await?
        .ok_or(ApiError::Message(StatusCode::NOT_FOUND, not_found))
}

/// `hostnameUsers()`: hostname => the models that use it, sorted by hostname.
async fn hostname_users(
    state: &AppState,
    r: &Resource,
) -> Result<BTreeMap<String, Vec<DnsResource>>, ApiError> {
    let mut users: BTreeMap<String, Vec<DnsResource>> = BTreeMap::new();
    for model in &r.models {
        for hostname in cf::hostnames_of_resource(&state.db, &model.morph, model.id).await? {
            users.entry(hostname).or_default().push(model.clone());
        }
    }
    Ok(users)
}

/// `FILTER_VALIDATE_IP` with `FILTER_FLAG_IPV4` / `FILTER_FLAG_IPV6`.
fn ip_version(address: &str) -> Option<u8> {
    match address.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(_)) => Some(4),
        Ok(std::net::IpAddr::V6(_)) => Some(6),
        Err(_) => None,
    }
}

/// `ServerDnsAddresses::forServer($server)->publicAddresses()`: IPv4 first.
async fn public_addresses(
    state: &AppState,
    server_id: Option<i64>,
) -> Result<Vec<String>, ApiError> {
    let Some(server_id) = server_id else {
        return Ok(Vec::new());
    };
    let Some(server) = coolify_db::servers::find_by_id(&state.db, server_id).await? else {
        return Ok(Vec::new());
    };
    let settings: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT public_ipv4, public_ipv6 FROM instance_settings WHERE id = 0")
            .fetch_optional(state.db.pool())
            .await
            .map_err(|_| ApiError::Internal)?;
    let (public_v4, public_v6) = settings.unwrap_or_default();
    let filled = |v: &Option<String>| v.clone().filter(|v| !v.is_empty());
    let ip = server
        .record
        .get("ip")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let instance = server.id == 0;
    let configured = if instance {
        filled(&public_v4).or_else(|| filled(&public_v6)).or(ip)
    } else {
        ip
    };
    // `resolveServerIpAddress()`: the resolved IP, or the configured value.
    let address = match configured
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        None => None,
        Some(c) if c.parse::<std::net::IpAddr>().is_ok() => Some(c.to_owned()),
        Some(c) => match tokio::net::lookup_host((c, 0)).await {
            Ok(mut found) => Some(
                found
                    .next()
                    .map_or_else(|| c.to_owned(), |a| a.ip().to_string()),
            ),
            Err(_) => Some(c.to_owned()),
        },
    };
    let (mut ipv4, mut ipv6) = (None, None);
    if let Some(a) = address.filter(|a| !a.trim().is_empty()) {
        match ip_version(&a) {
            Some(4) => ipv4 = Some(a),
            Some(6) => ipv6 = Some(a),
            _ => {}
        }
    }
    if instance {
        if ipv4.is_none() {
            ipv4 = public_v4.filter(|v| ip_version(v) == Some(4));
        }
        if ipv6.is_none() {
            ipv6 = public_v6.filter(|v| ip_version(v) == Some(6));
        }
    }
    Ok([ipv4, ipv6]
        .into_iter()
        .flatten()
        .filter(|a| cf::is_public_address(a))
        .collect())
}

/// `ServerDnsAddresses::recordContent($requested)`.
fn record_content(public: &[String], requested: Option<&str>) -> Option<String> {
    match requested {
        None => public.first().cloned(),
        Some(r) => public
            .iter()
            .find(|a| cf::same_address(Some(a), Some(r)))
            .cloned(),
    }
}

async fn token_uuid(state: &AppState, id: i64) -> Result<Option<String>, ApiError> {
    sqlx::query_scalar("SELECT uuid FROM integration_tokens WHERE id = $1")
        .bind(id)
        .fetch_optional(state.db.pool())
        .await
        .map_err(|_| ApiError::Internal)
}

/// `recordData()`.
async fn record_data(state: &AppState, r: &store::Record) -> Result<Value, ApiError> {
    Ok(json!({
        "uuid": r.uuid,
        "hostname": r.name,
        "type": r.kind,
        "address": r.content,
        "owned": r.owned,
        "integration_token_uuid": token_uuid(state, r.integration_token_id).await?,
    }))
}

fn zone_metadata(z: &store::Zone) -> Value {
    z.metadata
        .as_deref()
        .and_then(|m| serde_json::from_str(m).ok())
        .unwrap_or(Value::Null)
}

/// `GET /dns/zones`.
pub async fn zones(State(state): State<AppState>, Extension(ctx): Extension<AuthCtx>) -> ApiResult {
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let rows: Vec<(String, Option<String>, String, String, i64)> = sqlx::query_as(
        "SELECT z.name, z.account_name, t.uuid, t.name, \
           (SELECT count(*) FROM managed_dns_records r WHERE r.dns_provider_zone_id = z.id) \
         FROM dns_provider_zones z JOIN integration_tokens t ON t.id = z.integration_token_id \
         WHERE t.team_id = $1 AND t.provider = 'cloudflare' ORDER BY z.name",
    )
    .bind(ctx.team_id)
    .fetch_all(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    let list: Vec<Value> = rows
        .into_iter()
        .map(|(zone, account, token_uuid, token_name, count)| {
            json!({
                "zone": zone,
                "account_name": account,
                "integration_token_uuid": token_uuid,
                "integration_token_name": token_name,
                "managed_records_count": count,
            })
        })
        .collect();
    Ok(ok(Value::Array(list)).into_response())
}

/// `ResourceDnsRecords::status()`: reads only the database.
async fn status(state: &AppState, r: &Resource) -> Result<Value, ApiError> {
    let hostnames: Vec<String> = hostname_users(state, r).await?.into_keys().collect();
    let records: Vec<(String, String, String, String, bool, Option<String>)> = sqlx::query_as(
        "SELECT r.uuid, r.name, r.type, r.content, r.owned, t.uuid FROM managed_dns_records r \
         LEFT JOIN integration_tokens t ON t.id = r.integration_token_id \
         WHERE r.team_id = $1 AND r.name = ANY($2) ORDER BY r.id",
    )
    .bind(r.team_id)
    .bind(&hostnames)
    .fetch_all(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    let public = public_addresses(state, r.server_id).await?;
    let mut list = Vec::with_capacity(hostnames.len());
    for hostname in &hostnames {
        let mut providers = Vec::new();
        for z in cf::find_zones(&state.db, r.team_id, hostname)
            .await
            .map_err(|_| ApiError::Internal)?
        {
            let metadata = zone_metadata(&z);
            providers.push(json!({
                "integration_token_uuid": token_uuid(state, z.integration_token_id).await?,
                "integration_token_name": z.token_name,
                "zone": z.name,
                "automatic_dns": automatic_dns_enabled("cloudflare", &metadata),
                "proxied_dns": proxied_dns_enabled("cloudflare", &metadata),
            }));
        }
        let of_host: Vec<Value> = records
            .iter()
            .filter(|rec| rec.1 == *hostname)
            .map(|(uuid, name, kind, content, owned, token)| {
                json!({
                    "uuid": uuid,
                    "hostname": name,
                    "type": kind,
                    "address": content,
                    "owned": owned,
                    "integration_token_uuid": token,
                })
            })
            .collect();
        list.push(json!({
            "hostname": hostname,
            "providers": providers,
            "records": of_host,
        }));
    }
    Ok(json!({
        "default_address": record_content(&public, None),
        "public_addresses": public,
        "hostnames": list,
    }))
}

/// `GET /applications/{uuid}/dns`.
pub async fn application_status(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let r = find_resource(&state, &ctx, Kind::Application, &uuid, false).await?;
    Ok(ok(status(&state, &r).await?).into_response())
}

/// `GET /services/{uuid}/dns`.
pub async fn service_status(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let r = find_resource(&state, &ctx, Kind::Service, &uuid, false).await?;
    Ok(ok(status(&state, &r).await?).into_response())
}

/// `validateBody()`: the rules (`required|string|max:255` hostname, the other fields strings),
/// then the fields that are not allowed.
fn validate_body(
    headers: &HeaderMap,
    uri: &Uri,
    body: &Bytes,
    fields: &[(&str, bool)],
) -> Result<Map<String, Value>, ApiError> {
    validated_json(headers, uri, body)?;
    let body = validated_json(headers, &Uri::from_static("/"), body)?.0;
    let mut errors = Map::new();
    let mut add = |field: &str, text: String| {
        errors.insert(field.to_owned(), json!([text]));
    };
    for (field, required) in fields {
        let attribute = field.replace('_', " ");
        match body.get(*field) {
            None | Some(Value::Null) if *required => {
                add(field, "This field is required.".to_owned())
            }
            Some(Value::String(s)) if *required && s.trim().is_empty() => {
                add(field, "This field is required.".to_owned())
            }
            None | Some(Value::Null) => {}
            Some(Value::String(s)) => {
                if *field == "hostname" && s.chars().count() > 255 {
                    add(
                        field,
                        format!("The {attribute} field must not be greater than 255 characters."),
                    );
                }
            }
            Some(_) => add(field, format!("The {attribute} field must be a string.")),
        }
    }
    for k in body.keys() {
        if !fields.iter().any(|(f, _)| f == k) {
            errors.insert(k.clone(), json!(["This field is not allowed."]));
        }
    }
    if !errors.is_empty() {
        return Err(ApiError::Validation(errors));
    }
    Ok(body)
}

fn text<'a>(body: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    body.get(k).and_then(Value::as_str)
}

/// `prepare()`: the hostname, its users, the zone and the record content.
async fn prepare(
    state: &AppState,
    r: &Resource,
    hostname: &str,
    token_uuid_wanted: Option<&str>,
    content: Option<&str>,
) -> Result<(String, Vec<DnsResource>, store::Zone, String), ApiError> {
    let hostname = normalize_hostname(hostname);
    let mut users = hostname_users(state, r).await?;
    let Some(users) = users.remove(&hostname) else {
        return Err(validation(
            "hostname",
            format!("{hostname} is not a domain of this resource."),
        ));
    };
    let public = public_addresses(state, r.server_id).await?;
    if public.is_empty() {
        return Err(validation("address", cf::NO_PUBLIC_ADDRESS_MESSAGE));
    }
    let Some(content) = record_content(&public, content) else {
        return Err(validation(
            "address",
            format!(
                "The address must be a public address of the server: {}.",
                public.join(", ")
            ),
        ));
    };
    let mut zones = Vec::new();
    for z in cf::find_zones(&state.db, r.team_id, &hostname)
        .await
        .map_err(|_| ApiError::Internal)?
    {
        let uuid = token_uuid(state, z.integration_token_id)
            .await?
            .unwrap_or_default();
        if token_uuid_wanted.is_none_or(|w| w == uuid) {
            zones.push((uuid, z));
        }
    }
    if zones.is_empty() {
        return Err(validation(
            "integration_token_uuid",
            format!("No connected DNS provider of the team manages {hostname}."),
        ));
    }
    if zones.len() > 1 {
        let uuids: Vec<&str> = zones.iter().map(|(u, _)| u.as_str()).collect();
        return Err(validation(
            "integration_token_uuid",
            format!(
                "Several DNS providers manage {hostname}. Set integration_token_uuid to one of: {}.",
                uuids.join(", ")
            ),
        ));
    }
    let (_, zone) = zones.remove(0);
    Ok((hostname, users, zone, content))
}

/// `runDnsChange()`: DNS errors as responses.
fn dns_error(e: DnsError, hostname: &str) -> ApiError {
    match e {
        DnsError::Conflict {
            record_id,
            current,
            proposed,
        } => ApiError::Status(
            StatusCode::CONFLICT,
            json!({
                "message": format!("DNS record already points to {current}."),
                "conflict": {"record_id": record_id, "current": current, "proposed": proposed},
            }),
        ),
        DnsError::Busy => message(StatusCode::LOCKED, busy(&hostname.to_lowercase())),
        DnsError::Other(m) => message(StatusCode::BAD_REQUEST, m),
    }
}

fn dns_service(state: &AppState) -> Result<cf::Dns<'_>, ApiError> {
    Ok(cf::Dns {
        db: &state.db,
        enc: state.encrypter.as_ref().ok_or(ApiError::Internal)?,
        now: state.now(),
        source: "api",
    })
}

async fn create(
    state: &AppState,
    r: &Resource,
    headers: &HeaderMap,
    uri: &Uri,
    body: &Bytes,
) -> ApiResult {
    let body = validate_body(
        headers,
        uri,
        body,
        &[
            ("hostname", true),
            ("integration_token_uuid", false),
            ("address", false),
        ],
    )?;
    let raw = text(&body, "hostname").unwrap_or_default().to_owned();
    let (hostname, users, zone, content) = prepare(
        state,
        r,
        &raw,
        text(&body, "integration_token_uuid"),
        text(&body, "address"),
    )
    .await?;
    let dns = dns_service(state)?;
    let record = dns
        .create_record(&zone, &hostname, &content, users.first(), &users[1..], None)
        .await
        .map_err(|e| dns_error(e, &raw))?;
    Ok(created(json!({"record": record_data(state, &record).await?})).into_response())
}

async fn replace(
    state: &AppState,
    r: &Resource,
    headers: &HeaderMap,
    uri: &Uri,
    body: &Bytes,
) -> ApiResult {
    let body = validate_body(
        headers,
        uri,
        body,
        &[
            ("hostname", true),
            ("integration_token_uuid", false),
            ("record_id", true),
            ("current", true),
            ("address", false),
        ],
    )?;
    let raw = text(&body, "hostname").unwrap_or_default().to_owned();
    let (hostname, users, zone, content) = prepare(
        state,
        r,
        &raw,
        text(&body, "integration_token_uuid"),
        text(&body, "address"),
    )
    .await?;
    let dns = dns_service(state)?;
    let record = dns
        .replace_record(
            &zone,
            text(&body, "record_id").unwrap_or_default(),
            &hostname,
            &content,
            users.first(),
            text(&body, "current"),
            &users[1..],
            false,
        )
        .await
        .map_err(|e| dns_error(e, &raw))?;
    Ok(ok(json!({"record": record_data(state, &record).await?})).into_response())
}

/// `$request->boolean('delete_record', true)`.
fn delete_record_flag(uri: &Uri, body: &Bytes) -> bool {
    let from_body = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|b| b.get("delete_record").cloned());
    let value = from_body.or_else(|| query_param(uri, "delete_record").map(Value::String));
    match value {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::String(s)) => matches!(s.to_lowercase().as_str(), "1" | "true" | "on" | "yes"),
        Some(_) => false,
    }
}

async fn release(state: &AppState, r: &Resource, raw: &str, delete_record: bool) -> ApiResult {
    let hostname = normalize_hostname(raw);
    if hostname_users(state, r).await?.contains_key(&hostname) {
        return Err(validation(
            "hostname",
            format!(
                "The resource still uses {hostname}. Remove the domain first; its DNS record is then released."
            ),
        ));
    }
    let mut models = Vec::new();
    for model in &r.models {
        let keys = vec![(model.morph.clone(), model.id)];
        if !store::records_referenced(&state.db, Some(r.team_id), Some(&hostname), None, &keys)
            .await?
            .is_empty()
        {
            models.push(model.clone());
        }
    }
    if models.is_empty() {
        return Err(message(
            StatusCode::NOT_FOUND,
            format!("No managed DNS record of this resource for {raw}."),
        ));
    }
    let dns = dns_service(state)?;
    let mut results = Vec::new();
    for model in &models {
        if let Some(result) = dns
            .release_hostname(model, &hostname, r.team_id, delete_record)
            .await
            .map_err(|_| ApiError::Internal)?
        {
            results.push(result);
        }
    }
    let result = results
        .iter()
        .copied()
        .find(|r| r.should_retry())
        .or_else(|| {
            results
                .iter()
                .copied()
                .find(|r| *r == Deletion::ChangedExternally)
        })
        .or_else(|| results.first().copied());
    match result {
        Some(Deletion::Busy) => Err(message(StatusCode::LOCKED, busy(raw))),
        Some(Deletion::Failed) => Err(ApiError::Message(
            StatusCode::BAD_GATEWAY,
            "Cloudflare did not respond. The record was left untouched; try again.",
        )),
        other => {
            let text = match other {
                Some(Deletion::Deleted) => "The DNS record was deleted.",
                Some(Deletion::AlreadyGone) => "The DNS record no longer existed.",
                Some(Deletion::NotOwned) => {
                    "Coolify did not create the DNS record, so it was kept."
                }
                Some(Deletion::ChangedExternally) => {
                    "The DNS record changed outside Coolify, so it was kept."
                }
                _ => {
                    "The DNS record was kept: another resource uses the hostname, or delete_record was false."
                }
            };
            Ok(ok(json!({
                "result": other.map_or("kept", Deletion::value),
                "message": text,
            }))
            .into_response())
        }
    }
}

/// `POST /applications/{uuid}/dns`.
pub async fn create_application_record(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let r = find_resource(&state, &ctx, Kind::Application, &uuid, true).await?;
    create(&state, &r, &headers, &uri, &body).await
}

/// `POST /services/{uuid}/dns`.
pub async fn create_service_record(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let r = find_resource(&state, &ctx, Kind::Service, &uuid, true).await?;
    create(&state, &r, &headers, &uri, &body).await
}

/// `POST /applications/{uuid}/dns/replace`.
pub async fn replace_application_record(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let r = find_resource(&state, &ctx, Kind::Application, &uuid, true).await?;
    replace(&state, &r, &headers, &uri, &body).await
}

/// `POST /services/{uuid}/dns/replace`.
pub async fn replace_service_record(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let r = find_resource(&state, &ctx, Kind::Service, &uuid, true).await?;
    replace(&state, &r, &headers, &uri, &body).await
}

/// `DELETE /applications/{uuid}/dns/{hostname}`.
pub async fn release_application_record(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, hostname)): Path<(String, String)>,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let r = find_resource(&state, &ctx, Kind::Application, &uuid, true).await?;
    release(&state, &r, &hostname, delete_record_flag(&uri, &body)).await
}

/// `DELETE /services/{uuid}/dns/{hostname}`.
pub async fn release_service_record(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, hostname)): Path<(String, String)>,
    uri: Uri,
    body: Bytes,
) -> ApiResult {
    let r = find_resource(&state, &ctx, Kind::Service, &uuid, true).await?;
    release(&state, &r, &hostname, delete_record_flag(&uri, &body)).await
}

/// `ResourceDnsRecords::queueAutomaticRecords()`: a `ConfigureDnsRecordJob` for each hostname added
/// since `previous`, when exactly one zone of the team manages it and its token has automatic DNS
/// on. Returns the hostnames that were queued.
pub async fn queue_automatic_records(
    state: &AppState,
    kind: Kind,
    id: i64,
    previous: &[String],
) -> Vec<String> {
    let Ok(Some(r)) = load(state, kind, id).await else {
        return Vec::new();
    };
    let Ok(users) = hostname_users(state, &r).await else {
        return Vec::new();
    };
    let mut automatic: Vec<(String, store::Zone, DnsResource)> = Vec::new();
    for (hostname, users) in users {
        if previous.contains(&hostname) {
            continue;
        }
        let Ok(zones) = cf::find_zones(&state.db, r.team_id, &hostname).await else {
            continue;
        };
        if let [zone] = zones.as_slice()
            && automatic_dns_enabled("cloudflare", &zone_metadata(zone))
            && let Some(user) = users.first()
        {
            automatic.push((hostname, zone.clone(), user.clone()));
        }
    }
    if automatic.is_empty() {
        return Vec::new();
    }
    let Ok(public) = public_addresses(state, r.server_id).await else {
        return Vec::new();
    };
    let Some(content) = record_content(&public, None) else {
        return Vec::new();
    };
    let Some(enc) = state.encrypter.clone() else {
        return Vec::new();
    };
    let mut queued = Vec::new();
    for (hostname, zone, user) in automatic {
        queued.push(hostname.clone());
        let state = state.clone();
        let enc = enc.clone();
        let content = content.clone();
        let team_id = r.team_id;
        tokio::spawn(async move {
            let dns = cf::Dns {
                db: &state.db,
                enc: &enc,
                now: state.now(),
                source: "system",
            };
            cf::configure_dns_record_job(&dns, team_id, zone.id, Some(&user), &hostname, &content)
                .await;
        });
    }
    queued
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostnames_and_addresses() {
        assert_eq!(
            normalize_hostname(" App.Example.TEST. "),
            "app.example.test"
        );
        let public = vec!["203.0.113.9".to_owned(), "2001:db8::1".to_owned()];
        assert_eq!(
            record_content(&public, None).as_deref(),
            Some("203.0.113.9")
        );
        assert_eq!(
            record_content(&public, Some("2001:0db8::0001")).as_deref(),
            Some("2001:db8::1")
        );
        assert_eq!(record_content(&public, Some("198.51.100.1")), None);
        assert_eq!(record_content(&[], None), None);
    }

    #[test]
    fn delete_record_defaults_to_true() {
        let uri = |s: &str| s.parse::<Uri>().unwrap_or_default();
        assert!(delete_record_flag(&uri("/x"), &Bytes::new()));
        assert!(!delete_record_flag(
            &uri("/x?delete_record=false"),
            &Bytes::new()
        ));
        assert!(!delete_record_flag(
            &uri("/x"),
            &Bytes::from_static(br#"{"delete_record":false}"#)
        ));
        assert!(delete_record_flag(
            &uri("/x?delete_record=1"),
            &Bytes::new()
        ));
    }
}
