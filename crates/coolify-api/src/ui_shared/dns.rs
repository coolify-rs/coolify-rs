//! The DNS parts of `Project\Application\Domains` and `Project\Service\Domains`:
//! `checkAllDns`, `checkDomainDns`, `CheckDomainDnsJob`, `Concerns\InteractsWithDnsProviders` and
//! `Concerns\InteractsWithCloudflareDomainConnect`. Domain Connect exists on Coolify Cloud only, so the
//! menu has no Cloudflare entry here, as on a self-hosted upstream.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use coolify_db::managed_dns as store;
use coolify_domain::new_public_id;
use coolify_jobs::integrations::cloudflare::{self as cf, Deletion, Dns, DnsError, DnsResource};
use coolify_jobs::integrations::dns_check;
use serde_json::{Map, Value, json};
use sqlx::Row as _;

use super::{Class, Res, find};
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::state::AppState;
use crate::ui_notifications::{toast_error, unauthorized};

type P = Path<(String, String)>;

pub fn routes() -> Router<AppState> {
    let b = "/ui/resources/{kind}/{uuid}/dns";
    Router::new()
        .route(b, get(show))
        .route(&format!("{b}/check"), post(check_dns))
        .route(&format!("{b}/create"), post(create_record))
        .route(&format!("{b}/replace"), post(replace_record))
}

/// A row of the Domains table.
#[derive(Debug, Clone)]
struct DomainRow {
    url: String,
    service: Option<String>,
    table: &'static str,
    id: i64,
    morph: &'static str,
    uuid: String,
    name: String,
    key: String,
    status: Option<String>,
}

fn split(v: Option<&str>) -> Vec<String> {
    v.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The domain rows of the application or the service.
async fn domain_rows(state: &AppState, res: &Res) -> Result<Vec<DomainRow>, ApiError> {
    let mut out = Vec::new();
    match res.class {
        Class::Application => {
            let statuses = res
                .raw
                .get("domain_dns_statuses")
                .cloned()
                .unwrap_or(Value::Null);
            let statuses = match statuses {
                Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::Null),
                v => v,
            };
            let mut push = |url: String, service: Option<String>| {
                let key = service
                    .as_ref()
                    .map_or_else(|| url.clone(), |s| format!("{s}|{url}"));
                out.push(DomainRow {
                    status: statuses
                        .get(&key)
                        .and_then(|e| e.get("status"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    url,
                    service,
                    table: "applications",
                    id: res.id,
                    morph: "App\\Models\\Application",
                    uuid: res.uuid.clone(),
                    name: res.name.clone(),
                    key,
                });
            };
            if res.str("build_pack") == Some("dockercompose") {
                let domains = res
                    .str("docker_compose_domains")
                    .and_then(|d| serde_json::from_str::<Value>(d).ok())
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default();
                for (service, entry) in domains {
                    for u in split(entry.get("domain").and_then(Value::as_str)) {
                        push(u, Some(service.clone()));
                    }
                }
            } else {
                for u in split(res.str("fqdn")) {
                    push(u, None);
                }
            }
        }
        Class::Service => {
            let rows = sqlx::query(
                "SELECT id, uuid, name, fqdn, domain_dns_statuses::text AS statuses FROM service_applications WHERE service_id = $1 AND deleted_at IS NULL ORDER BY id",
            )
            .bind(res.id)
            .fetch_all(state.db.pool())
            .await
            .map_err(|_| ApiError::Internal)?;
            for r in rows {
                let fqdn: Option<String> = r.try_get("fqdn").unwrap_or_default();
                let statuses: Option<String> = r.try_get("statuses").unwrap_or_default();
                let statuses: Value = statuses
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(Value::Null);
                for url in split(fqdn.as_deref()) {
                    out.push(DomainRow {
                        status: statuses
                            .get(&url)
                            .and_then(|e| e.get("status"))
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        key: url.clone(),
                        url,
                        service: None,
                        table: "service_applications",
                        id: r.try_get("id").unwrap_or_default(),
                        morph: "App\\Models\\ServiceApplication",
                        uuid: r.try_get("uuid").unwrap_or_default(),
                        name: r.try_get("name").unwrap_or_default(),
                    });
                }
            }
        }
        _ => return Err(ApiError::Message(StatusCode::NOT_FOUND, "Not found.")),
    }
    Ok(out)
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next()?.rsplit('@').next()?;
    let host = authority.split(':').next()?;
    (!host.is_empty()).then(|| host.to_lowercase())
}

/// `allDomainHostnames`, sorted and unique.
fn hostnames(rows: &[DomainRow], only_needing_dns: bool) -> Vec<String> {
    let mut hosts: Vec<String> = rows.iter().filter_map(|r| host_of(&r.url)).collect();
    if only_needing_dns {
        let working: Vec<String> = rows
            .iter()
            .filter(|r| r.status.as_deref() == Some("ok"))
            .filter_map(|r| host_of(&r.url))
            .collect();
        hosts.retain(|h| !working.contains(h));
    }
    hosts.sort();
    hosts.dedup();
    hosts
}

/// `resolveServerIpAddress`: the address to publish; a hostname stays as it is.
async fn resolve_ip(configured: Option<&str>) -> Option<String> {
    let c = configured.map(str::trim).filter(|c| !c.is_empty())?;
    if c.parse::<std::net::IpAddr>().is_ok() {
        return Some(c.to_owned());
    }
    match tokio::net::lookup_host((c, 0)).await {
        Ok(mut addrs) => Some(
            addrs
                .next()
                .map_or_else(|| c.to_owned(), |a| a.ip().to_string()),
        ),
        Err(_) => Some(c.to_owned()),
    }
}

/// The server's `ip`, `id`, and the instance's public addresses and DNS settings.
struct Facts {
    configured: Option<String>,
    server_ip: Option<String>,
    has_server: bool,
    ipv4: Option<String>,
    ipv6: Option<String>,
    validation_enabled: bool,
    dns_servers: Vec<String>,
}

async fn facts(state: &AppState, res: &Res) -> Result<Facts, ApiError> {
    let settings = sqlx::query(
        "SELECT is_dns_validation_enabled, custom_dns_servers, public_ipv4, public_ipv6 FROM instance_settings WHERE id = 0",
    )
    .fetch_optional(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    let text = |k: &str| {
        settings
            .as_ref()
            .and_then(|r| r.try_get::<Option<String>, _>(k).ok())
            .flatten()
    };
    let server = match res.server_id {
        Some(id) => coolify_db::servers::find_by_id(&state.db, id).await?,
        None => None,
    };
    let is_local = server.as_ref().is_some_and(|s| s.id == 0);
    let server_ip = server
        .as_ref()
        .and_then(|s| s.record.get("ip").and_then(Value::as_str))
        .map(str::to_owned);
    let configured = if is_local {
        text("public_ipv4")
            .or_else(|| text("public_ipv6"))
            .or_else(|| server_ip.clone())
    } else {
        server_ip.clone()
    };
    let resolved = resolve_ip(configured.as_deref()).await;
    let (mut ipv4, mut ipv6) = (None, None);
    match resolved.as_deref().map(str::parse::<std::net::IpAddr>) {
        Some(Ok(std::net::IpAddr::V4(_))) => ipv4 = resolved.clone(),
        Some(Ok(std::net::IpAddr::V6(_))) => ipv6 = resolved.clone(),
        _ => {}
    }
    if is_local {
        ipv4 = ipv4.or_else(|| text("public_ipv4"));
        ipv6 = ipv6.or_else(|| text("public_ipv6"));
    }
    let validation_enabled = settings
        .as_ref()
        .and_then(|r| {
            r.try_get::<Option<bool>, _>("is_dns_validation_enabled")
                .ok()
        })
        .flatten()
        .unwrap_or(false);
    let dns_servers = text("custom_dns_servers")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    Ok(Facts {
        configured: resolved.or(configured),
        server_ip,
        has_server: server.is_some(),
        ipv4,
        ipv6,
        validation_enabled,
        dns_servers,
    })
}

/// `publicServerIpsForDnsProvider`.
fn public_ips(f: &Facts) -> Vec<String> {
    [f.ipv4.clone(), f.ipv6.clone()]
        .into_iter()
        .flatten()
        .filter(|a| cf::is_public_address(a))
        .collect()
}

/// `loadDnsProviderProposals`.
async fn proposals(
    state: &AppState,
    res: &Res,
    hosts: &[String],
    target: Option<&str>,
) -> Result<Vec<Value>, ApiError> {
    let managed = store::managed_names(&state.db, res.team_id, hosts).await?;
    let mut out = Vec::new();
    for hostname in hosts {
        let zones = cf::find_zones(&state.db, res.team_id, hostname)
            .await
            .map_err(|_| ApiError::Internal)?;
        for z in zones {
            out.push(json!({
                "hostname": hostname,
                "zone_id": z.id,
                "zone": z.name,
                "credential": z.token_name,
                "target": target,
                "managed": managed.contains(hostname),
            }));
        }
    }
    Ok(out)
}

/// `dnsRecordHints`, `dnsRecordsCopyText` and the providers modal.
async fn show(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): P,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    let rows = domain_rows(&state, &res).await?;
    let f = facts(&state, &res).await?;
    let hosts = hostnames(&rows, false);
    let needing = hostnames(&rows, true);
    let hints = dns_check::hints(&needing, f.ipv4.as_deref(), f.ipv6.as_deref());
    let list: Vec<Value> = hints
        .iter()
        .map(|h| json!({"type": h.kind, "name": h.name, "value": h.value}))
        .collect();
    let public = public_ips(&f);
    Ok(ok(json!({
        "can_update": ctx.is_team_admin(),
        "domain_connect_available": false,
        "hints": list,
        "no_public_address_message": cf::NO_PUBLIC_ADDRESS_MESSAGE,
        "server_ip": f.configured,
        "proposals": proposals(&state, &res, &hosts, public.first().map(String::as_str)).await?,
    }))
    .into_response())
}

/// `queueUrlsDns` and `CheckDomainDnsJob`.
async fn check_dns(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): P,
    Json(body): Json<Value>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let rows = domain_rows(&state, &res).await?;
    let wanted_url = body.get("url").and_then(Value::as_str);
    let wanted_service = body.get("service").and_then(Value::as_str);
    let f = facts(&state, &res).await?;
    // `$application->additional_servers->count() > 0`.
    let multi = res.class == Class::Application && {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM additional_destinations WHERE application_id = $1",
        )
        .bind(res.id)
        .fetch_one(state.db.pool())
        .await
        .unwrap_or(0);
        n > 0
    };
    let expected = f.configured.clone();
    for r in rows.iter().filter(|r| {
        wanted_url.is_none_or(|u| u == r.url)
            && (wanted_url.is_none() || wanted_service == r.service.as_deref())
    }) {
        let check_id = new_public_id();
        let checking = json!({
            "status": "checking",
            "message": "",
            "expected_ip": expected,
            "checked_at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S+00:00").to_string(),
            "check_id": check_id,
        });
        set_status(&state, r, &checking).await?;
        let ctx_check = dns_check::CheckContext {
            validation_enabled: f.validation_enabled,
            dns_servers: f.dns_servers.clone(),
            has_server: f.has_server,
            server_ip: f.server_ip.clone(),
            skip_multiple_servers: multi,
        };
        let (st, row, exp, id) = (state.clone(), r.clone(), expected.clone(), check_id);
        tokio::spawn(async move {
            let out = dns_check::check(
                &row.url,
                &ctx_check,
                exp.as_deref(),
                crate::ip::is_cloudflare_ip,
            )
            .await;
            let stored: Option<String> = sqlx::query_scalar(&format!(
                "SELECT domain_dns_statuses::jsonb -> $2 ->> 'check_id' FROM {} WHERE id = $1",
                row.table
            ))
            .bind(row.id)
            .bind(&row.key)
            .fetch_optional(st.db.pool())
            .await
            .ok()
            .flatten();
            if stored.as_deref() == Some(id.as_str()) {
                let _ = set_status(&st, &row, &out.stored()).await;
            }
        });
    }
    Ok(ok(json!({})).into_response())
}

/// `$resource->domain_dns_statuses[$key] = $status`. The column is plain JSON text upstream.
async fn set_status(state: &AppState, row: &DomainRow, status: &Value) -> Result<(), ApiError> {
    sqlx::query(&format!(
        "UPDATE {} SET domain_dns_statuses = (COALESCE(domain_dns_statuses::jsonb, '{{}}'::jsonb) || jsonb_build_object($2::text, $3::jsonb))::json, updated_at = $4 WHERE id = $1",
        row.table
    ))
    .bind(row.id)
    .bind(&row.key)
    .bind(status.to_string())
    .bind(state.now())
    .execute(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    Ok(())
}

fn dns_resource(r: &DomainRow) -> DnsResource {
    DnsResource {
        morph: r.morph.to_owned(),
        id: r.id,
        uuid: Some(r.uuid.clone()),
        name: Some(r.name.clone()),
        type_name: if r.table == "applications" {
            "application".into()
        } else {
            "service_application".into()
        },
    }
}

/// `dnsResourceForHostname` and `otherDnsResourcesForHostname`.
fn resources_for(rows: &[DomainRow], hostname: &str) -> (Option<DnsResource>, Vec<DnsResource>) {
    let mut found: Vec<DnsResource> = Vec::new();
    for r in rows
        .iter()
        .filter(|r| host_of(&r.url).as_deref() == Some(hostname))
    {
        if !found.iter().any(|x| x.id == r.id) {
            found.push(dns_resource(r));
        }
    }
    let first = found.first().cloned();
    (
        first,
        if rows
            .first()
            .is_some_and(|r| r.table == "service_applications")
        {
            found
        } else {
            Vec::new()
        },
    )
}

fn busy(hostname: &str) -> String {
    format!("Another DNS change for {hostname} is in progress. Try again in a few seconds.")
}

/// `createManagedDnsRecord`.
async fn create_record(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): P,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let hostname = body
        .get("hostname")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let zone_id = body.get("zone_id").and_then(Value::as_i64).unwrap_or(-1);
    let rows = domain_rows(&state, &res).await?;
    let f = facts(&state, &res).await?;
    let public = public_ips(&f);
    if public.is_empty() {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            cf::NO_PUBLIC_ADDRESS_MESSAGE,
        ));
    }
    let zones = cf::find_zones(&state.db, res.team_id, &hostname)
        .await
        .unwrap_or_default();
    let zone = zones.into_iter().find(|z| z.id == zone_id);
    let content = public.first().cloned();
    let (resource, others) = resources_for(&rows, &hostname);
    let (Some(zone), Some(content), Some(resource)) = (zone, content, resource) else {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "No connected DNS provider or public server IP is available for this domain.",
        ));
    };
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let dns = Dns {
        db: &state.db,
        enc,
        now: state.now(),
        source: "ui",
    };
    match dns
        .create_record(&zone, &hostname, &content, Some(&resource), &others, None)
        .await
    {
        Ok(_) => {
            // `markDnsManaged`
            let status = json!({
                "status": "ok",
                "message": format!("DNS record created through {}.", zone.token_name),
                "expected_ip": f.configured,
                "checked_at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S+00:00").to_string(),
                "check_id": Value::Null,
            });
            for r in rows
                .iter()
                .filter(|r| host_of(&r.url).as_deref() == Some(hostname.as_str()))
            {
                set_status(&state, r, &status).await?;
            }
            Ok(
                ok(json!({"message": format!("DNS record created for {hostname}.")}))
                    .into_response(),
            )
        }
        Err(DnsError::Conflict {
            record_id,
            current,
            proposed,
        }) => Ok(ok(json!({
            "conflict": {"record_id": record_id, "current": current, "proposed": proposed},
        }))
        .into_response()),
        Err(DnsError::Busy) => Ok(toast_error(StatusCode::CONFLICT, busy(&hostname))),
        Err(DnsError::Other(m)) => Ok(toast_error(StatusCode::UNPROCESSABLE_ENTITY, m)),
    }
}

/// `replaceManagedDnsRecord`.
async fn replace_record(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): P,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let res = find(&state, &ctx, &kind, &uuid).await?;
    if !ctx.is_team_admin() {
        return Ok(unauthorized());
    }
    let text = |k: &str| {
        body.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let hostname = text("hostname").to_lowercase();
    let zone_id = body.get("zone_id").and_then(Value::as_i64).unwrap_or(-1);
    let rows = domain_rows(&state, &res).await?;
    let f = facts(&state, &res).await?;
    let public = public_ips(&f);
    if public.is_empty() {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            cf::NO_PUBLIC_ADDRESS_MESSAGE,
        ));
    }
    let zones = cf::find_zones(&state.db, res.team_id, &hostname)
        .await
        .unwrap_or_default();
    let zone = zones.into_iter().find(|z| z.id == zone_id);
    let proposed = text("proposed");
    let content = public
        .iter()
        .find(|a| cf::same_address(Some(a), Some(&proposed)))
        .cloned();
    let (resource, others) = resources_for(&rows, &hostname);
    let (Some(zone), Some(content), Some(resource)) = (zone, content, resource) else {
        return Ok(toast_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The DNS conflict is no longer available. Check the record again.",
        ));
    };
    let enc = state.encrypter.as_ref().ok_or(ApiError::Internal)?;
    let dns = Dns {
        db: &state.db,
        enc,
        now: state.now(),
        source: "ui",
    };
    let current = text("current");
    match dns
        .replace_record(
            &zone,
            &text("record_id"),
            &hostname,
            &content,
            Some(&resource),
            Some(&current),
            &others,
            false,
        )
        .await
    {
        Ok(_) => Ok(
            ok(json!({"message": format!("DNS record replaced for {hostname}.")})).into_response(),
        ),
        Err(DnsError::Busy) => Ok(toast_error(StatusCode::CONFLICT, busy(&hostname))),
        Err(e) => Ok(toast_error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string())),
    }
}

/// Who a domain change belongs to, for releasing managed records.
#[derive(Debug, Clone, Copy)]
pub struct Owner<'a> {
    pub morph: &'a str,
    pub id: i64,
    pub uuid: &'a str,
    pub name: &'a str,
    pub type_name: &'a str,
    pub team_id: i64,
}

impl Owner<'_> {
    fn resource(&self) -> DnsResource {
        DnsResource {
            morph: self.morph.to_owned(),
            id: self.id,
            uuid: Some(self.uuid.to_owned()),
            name: Some(self.name.to_owned()),
            type_name: self.type_name.to_owned(),
        }
    }
}

/// `managedDnsHostnamesOf`.
pub async fn hostnames_before(state: &AppState, owner: &Owner<'_>) -> Vec<String> {
    cf::hostnames_of_resource(&state.db, owner.morph, owner.id)
        .await
        .unwrap_or_default()
}

/// `releaseManagedDnsForEditedDomains`: the warning for the user.
pub async fn release_after_edit(
    state: &AppState,
    owner: &Owner<'_>,
    previous: &[String],
) -> Option<String> {
    let enc = state.encrypter.as_ref()?;
    let dns = Dns {
        db: &state.db,
        enc,
        now: state.now(),
        source: "ui",
    };
    let result = dns
        .release_removed_hostnames(&owner.resource(), previous, owner.team_id)
        .await
        .ok()?;
    release_warning(result, "updated")
}

/// `releaseManagedDnsForUrl`: the warning for the user.
pub async fn release_after_remove(
    state: &AppState,
    owner: &Owner<'_>,
    url: &str,
    delete_record: bool,
) -> Option<String> {
    let enc = state.encrypter.as_ref()?;
    let hostname = host_of(url)?;
    let dns = Dns {
        db: &state.db,
        enc,
        now: state.now(),
        source: "ui",
    };
    let result = dns
        .release_hostname(&owner.resource(), &hostname, owner.team_id, delete_record)
        .await
        .ok()?;
    release_warning(result, "removed")
}

/// `queueReleaseOfRemovedHostnames` after a resource's domains were saved by the API, General or
/// a preview. `previous` comes from [`previous_hostnames`], read before the save.
pub fn queue_release_of_removed_hostnames(
    state: &AppState,
    morph: &str,
    id: i64,
    previous: Vec<String>,
    team_id: i64,
) {
    let Some(enc) = state.encrypter.as_ref() else {
        return;
    };
    cf::queue_release_of_removed_hostnames(
        &state.db,
        enc,
        DnsResource {
            morph: morph.to_owned(),
            id,
            uuid: None,
            name: None,
            type_name: String::new(),
        },
        previous,
        team_id,
    );
}

/// `$dnsCleanup->hostnamesOf($resource)` before a save.
pub async fn previous_hostnames(state: &AppState, morph: &str, id: i64) -> Vec<String> {
    cf::hostnames_of_resource(&state.db, morph, id)
        .await
        .unwrap_or_default()
}

/// `ReleaseManagedDnsRecordsJob` after a resource is deleted; a failed provider is retried three times.
pub fn spawn_release_resource(state: &AppState, resource_type: &'static str, id: i64) {
    let state = state.clone();
    tokio::spawn(async move {
        let Some(enc) = state.encrypter.clone() else {
            return;
        };
        for attempt in 0..3u64 {
            let dns = Dns {
                db: &state.db,
                enc: &enc,
                now: state.now(),
                source: "system",
            };
            if dns.release_resource(resource_type, id).await {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_secs(60 * (attempt + 1))).await;
        }
    });
}

/// `reportManagedDnsRelease`: the warning of a release.
pub fn release_warning(result: Option<Deletion>, change: &str) -> Option<String> {
    match result {
        Some(Deletion::ChangedExternally) => Some(format!(
            "The domain was {change}, but its DNS record changed externally and was left untouched."
        )),
        Some(Deletion::Failed) => Some(format!(
            "The domain was {change}, but its DNS record could not be deleted because Cloudflare did not respond. It was left untouched."
        )),
        Some(Deletion::Busy) => Some(format!(
            "The domain was {change}, but another DNS change for the same hostname was in progress. Its DNS record was left untouched."
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_lowercase_without_port() {
        assert_eq!(
            host_of("https://A.Example.com:8080/x").as_deref(),
            Some("a.example.com")
        );
        assert_eq!(host_of("b.test").as_deref(), Some("b.test"));
    }

    #[test]
    fn warnings() {
        assert!(release_warning(None, "removed").is_none());
        assert!(release_warning(Some(Deletion::Deleted), "removed").is_none());
        assert!(
            release_warning(Some(Deletion::Busy), "updated")
                .unwrap()
                .contains("updated")
        );
    }
}
