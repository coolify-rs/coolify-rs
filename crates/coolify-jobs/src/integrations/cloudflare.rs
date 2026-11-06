//! `CloudflareTokenValidator`, `CloudflareDnsProvider`, `ManagedDnsRecordCleanup`,
//! `ManagedDnsHostnameLock`, `DnsRecordHints` and `CloudflareTunnelService`.

use crate::git_http::SendRetry as _;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use chrono::{NaiveDateTime, Utc};
use coolify_compat::encrypter::Encrypter;
use coolify_db::Db;
use coolify_db::managed_dns::{self as store, Record, SyncedZone, Zone};
use coolify_domain::new_public_id;
use reqwest::{Client, Method, Response};
use serde_json::{Map, Value, json};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use super::provider_urls::{hosted, hosted_client};
use crate::server_ops::{Srv, tunnel};

pub const NO_PUBLIC_ADDRESS_MESSAGE: &str =
    "The server has no public IP address; add the DNS record manually.";
const OWNERSHIP_COMMENT_PREFIX: &str = "managed-by: coolify";
const TUNNEL_PERMISSION: &str = "Account → Cloudflare Tunnel → Edit";
const DELETE_ATTEMPTS: usize = 5;
/// `ManagedDnsHostnameLock::WAIT_SECONDS`.
const LOCK_WAIT_SECONDS: u64 = 5;

fn api() -> String {
    format!(
        "{}/client/v4",
        hosted("cloudflare", "https://api.cloudflare.com")
    )
}

async fn parse(response: Response) -> (u16, Value) {
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

fn ok(status: u16) -> bool {
    (200..300).contains(&status)
}

/// `Http::withToken($token)->acceptJson()`.
fn request(client: &Client, method: Method, url: String, token: &str) -> reqwest::RequestBuilder {
    client
        .request(method, url)
        .bearer_auth(token)
        .header("accept", "application/json")
}

/// `CloudflareTokenValidator::validate`.
pub async fn validate_token(token: &str, capabilities: &[String]) -> bool {
    let Ok(client) = hosted_client(10) else {
        return false;
    };
    let base = api();
    let get = |url: String| request(&client, Method::GET, url, token);
    let Ok(verification) = get(format!("{base}/user/tokens/verify")).send_retry().await else {
        return false;
    };
    let (status, body) = parse(verification).await;
    if !ok(status) || body.pointer("/result/status").and_then(Value::as_str) != Some("active") {
        return false;
    }
    if !capabilities.iter().any(|c| c == "dns") {
        return true;
    }
    let Ok(zones) = get(format!("{base}/zones"))
        .query(&[("per_page", 1)])
        .send_retry()
        .await
    else {
        return false;
    };
    let (status, zones) = parse(zones).await;
    let Some(zone_id) = zones.pointer("/result/0/id").and_then(Value::as_str) else {
        return false;
    };
    if !ok(status) {
        return false;
    }
    let records = get(format!("{base}/zones/{zone_id}/dns_records"))
        .query(&[("per_page", 1)])
        .send_retry()
        .await;
    if !records.is_ok_and(|r| r.status().is_success()) {
        return false;
    }
    if capabilities.iter().any(|c| c == "tunnel") {
        let Some(account) = zones
            .pointer("/result/0/account/id")
            .and_then(Value::as_str)
        else {
            return false;
        };
        return get(format!("{base}/accounts/{account}/cfd_tunnel"))
            .query(&[("per_page", "1"), ("is_deleted", "false")])
            .send_retry()
            .await
            .is_ok_and(|r| r.status().is_success());
    }
    true
}

/// `CloudflareDnsProvider::missingPermissionMessage`.
pub fn missing_permission_message(permission: &str) -> String {
    format!("The Cloudflare token needs the {permission} permission.")
}

fn iso_now() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

/// `syncZones`: the number of zones.
pub async fn sync_zones(
    db: &Db,
    token_id: i64,
    token: &str,
    now: NaiveDateTime,
) -> Result<usize, String> {
    let client = hosted_client(10)?;
    let base = api();
    let mut zones: Vec<SyncedZone> = Vec::new();
    let mut page = 1i64;
    loop {
        let response = request(&client, Method::GET, format!("{base}/zones"), token)
            .query(&[("page", page), ("per_page", 50)])
            .send_retry()
            .await
            .map_err(|e| e.to_string())?;
        let (status, body) = parse(response).await;
        if status == 403 {
            return Err(missing_permission_message("Zone → Zone → Read"));
        }
        if !ok(status) || body.get("success").and_then(Value::as_bool) != Some(true) {
            return Err("Cloudflare zones could not be synchronized.".to_owned());
        }
        for z in body
            .get("result")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let text = |p: &str| z.pointer(p).and_then(Value::as_str).map(str::to_owned);
            zones.push(SyncedZone {
                provider_zone_id: text("/id").unwrap_or_default(),
                name: text("/name").unwrap_or_default().to_lowercase(),
                account_id: text("/account/id"),
                account_name: text("/account/name"),
            });
        }
        let total = body
            .pointer("/result_info/total_pages")
            .and_then(Value::as_i64)
            .unwrap_or(1)
            .max(1);
        page += 1;
        if page > total {
            break;
        }
    }
    store::store_synced_zones(db, token_id, &zones, &iso_now(), now)
        .await
        .map_err(|e| e.to_string())?;
    Ok(zones.len())
}

fn normalize(hostname: &str) -> String {
    hostname.trim_end_matches('.').to_lowercase()
}

/// `findZones`: the zones with the longest name that manages the hostname.
pub async fn find_zones(db: &Db, team_id: i64, hostname: &str) -> Result<Vec<Zone>, String> {
    let hostname = normalize(hostname);
    let zones = store::zones_for_team(db, team_id)
        .await
        .map_err(|e| e.to_string())?;
    let matches: Vec<Zone> = zones
        .into_iter()
        .filter(|z| hostname == z.name || hostname.ends_with(&format!(".{}", z.name)))
        .collect();
    let longest = matches.iter().map(|z| z.name.len()).max().unwrap_or(0);
    Ok(matches
        .into_iter()
        .filter(|z| z.name.len() == longest)
        .collect())
}

/// `normalizeRemoteRecord`.
#[derive(Debug, Clone)]
struct Remote {
    id: String,
    kind: String,
    name: String,
    content: String,
    comment: Option<String>,
}

fn remote_of(v: &Value, hostname: &str, kind: &str) -> Remote {
    let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    Remote {
        id: text("id").unwrap_or_default(),
        kind: text("type").unwrap_or_else(|| kind.to_owned()),
        name: text("name")
            .unwrap_or_else(|| hostname.to_owned())
            .to_lowercase(),
        content: text("content").unwrap_or_default(),
        comment: text("comment"),
    }
}

/// `recordType`.
pub fn record_type(content: &str) -> &'static str {
    match content.parse::<IpAddr>() {
        Ok(IpAddr::V6(_)) => "AAAA",
        Ok(IpAddr::V4(_)) => "A",
        Err(_) => "CNAME",
    }
}

/// `DnsRecordHints::sameAddress`.
pub fn same_address(first: Option<&str>, second: Option<&str>) -> bool {
    let (Some(a), Some(b)) = (first, second) else {
        return false;
    };
    match (a.trim().parse::<IpAddr>(), b.trim().parse::<IpAddr>()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// `DnsRecordHints::isPublicAddress`.
pub fn is_public_address(address: &str) -> bool {
    let Ok(parsed) = address.parse::<IpAddr>() else {
        return false;
    };
    let ip = match parsed {
        IpAddr::V6(v) => v.to_ipv4_mapped().map_or(parsed, IpAddr::V4),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(v) => {
            let [a, b, c, _] = v.octets();
            let private = v.is_private();
            // FILTER_FLAG_NO_RES_RANGE.
            let reserved = a == 0
                || a >= 240
                || a == 127
                || (a == 169 && b == 254)
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 0 && c == 2)
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113);
            let cgnat = a == 100 && (b & 0xC0) == 64;
            !(private || reserved || cgnat || (a & 0xF0) == 224)
        }
        IpAddr::V6(v) => {
            let s = v.segments();
            let private = (s[0] & 0xfe00) == 0xfc00;
            let reserved = v.is_loopback()
                || v.is_unspecified()
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] == 0x2001 && s[1] == 0xdb8);
            !(private || reserved || (s[0] >> 8) == 0xff)
        }
    }
}

/// A failed DNS change.
#[derive(Debug, Clone)]
pub enum DnsError {
    /// `DnsRecordConflictException`.
    Conflict {
        record_id: String,
        current: String,
        proposed: String,
    },
    /// `LockTimeoutException`.
    Busy,
    Other(String),
}

impl std::fmt::Display for DnsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DnsError::Conflict { current, .. } => {
                write!(f, "DNS record already points to {current}.")
            }
            DnsError::Busy => f.write_str("Another DNS change is in progress."),
            DnsError::Other(m) => f.write_str(m),
        }
    }
}

impl From<coolify_db::DbError> for DnsError {
    fn from(e: coolify_db::DbError) -> Self {
        DnsError::Other(e.to_string())
    }
}

/// `ManagedDnsDeletionResult`.
/// `ManagedDnsRecordCleanup::ORPHAN_GRACE_MINUTES`.
const ORPHAN_GRACE_MINUTES: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deletion {
    Deleted,
    AlreadyGone,
    NotOwned,
    ChangedExternally,
    Failed,
    Busy,
}

impl Deletion {
    /// The enum's value.
    pub fn value(self) -> &'static str {
        match self {
            Deletion::Deleted => "deleted",
            Deletion::AlreadyGone => "already_gone",
            Deletion::NotOwned => "not_owned",
            Deletion::ChangedExternally => "changed_externally",
            Deletion::Failed => "failed",
            Deletion::Busy => "busy",
        }
    }

    /// `shouldRetry()`.
    pub fn should_retry(self) -> bool {
        matches!(self, Deletion::Failed | Deletion::Busy)
    }
}

type Locks = Mutex<HashMap<String, Arc<AsyncMutex<()>>>>;
static HOSTNAME_LOCKS: LazyLock<Locks> = LazyLock::new(Locks::default);

/// `ManagedDnsHostnameLock::run`: this instance's changes for one hostname run one at a time.
async fn hostname_lock(team_id: i64, hostname: &str) -> Result<OwnedMutexGuard<()>, DnsError> {
    let key = format!("managed-dns:{team_id}:{}", normalize(hostname));
    let lock = {
        let mut map = HOSTNAME_LOCKS
            .lock()
            .map_err(|_| DnsError::Other("Lock failed.".into()))?;
        map.entry(key).or_default().clone()
    };
    tokio::time::timeout(Duration::from_secs(LOCK_WAIT_SECONDS), lock.lock_owned())
        .await
        .map_err(|_| DnsError::Busy)
}

/// `ownershipCommentFor`: `config('app.id')`.
pub fn ownership_comment(uuid: &str) -> String {
    let id = std::env::var("APP_ID")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "default".to_owned());
    let id: String = id.chars().take(40).collect();
    format!("{OWNERSHIP_COMMENT_PREFIX} {id}/{uuid}")
}

/// Who a DNS change is for: the `$resource` audit fields and references.
#[derive(Debug, Clone)]
pub struct DnsResource {
    pub morph: String,
    pub id: i64,
    pub uuid: Option<String>,
    pub name: Option<String>,
    pub type_name: String,
}

/// The services a DNS change needs.
pub struct Dns<'a> {
    pub db: &'a Db,
    pub enc: &'a Encrypter,
    pub now: NaiveDateTime,
    /// `auth()->check() ? 'ui' : 'system'`.
    pub source: &'a str,
}

impl std::fmt::Debug for Dns<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dns")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl Dns<'_> {
    fn token(&self, encrypted: &str) -> Result<String, DnsError> {
        self.enc
            .decrypt_string(encrypted)
            .map_err(|_| DnsError::Other("The MAC is invalid.".into()))
    }

    /// `auditDnsRecord`.
    async fn audit(
        &self,
        action: &str,
        zone: &Zone,
        hostname: &str,
        resource: Option<&DnsResource>,
    ) {
        let kind = resource.map_or("dns_record", |r| r.type_name.as_str());
        let mut ctx = Map::new();
        ctx.insert("team_id".into(), json!(zone.team_id));
        ctx.insert("resource".into(), json!(kind));
        ctx.insert(
            format!("{kind}_uuid"),
            json!(resource.and_then(|r| r.uuid.clone())),
        );
        ctx.insert(
            format!("{kind}_name"),
            json!(resource.and_then(|r| r.name.clone())),
        );
        ctx.insert("hostname".into(), json!(hostname));
        ctx.insert("provider".into(), json!("cloudflare"));
        ctx.insert("zone".into(), json!(zone.name));
        audit_log(
            self.db,
            zone.team_id,
            &format!("{}.dns_record.{action}", self.source),
            Value::Object(ctx),
            "info",
            self.now,
        )
        .await;
    }

    /// `auditSkipped`.
    async fn audit_skipped(&self, record: &Record, reason: &str, level: &str) {
        audit_log(
            self.db,
            record.team_id,
            &format!("{}.dns_record.delete_skipped", self.source),
            json!({
                "team_id": record.team_id,
                "hostname": record.name,
                "provider": "cloudflare",
                "reason": reason,
            }),
            level,
            self.now,
        )
        .await;
    }

    /// `findRecords`.
    async fn find_records(
        &self,
        zone: &Zone,
        hostname: &str,
        kind: Option<&str>,
    ) -> Result<Vec<Remote>, DnsError> {
        let hostname = normalize(hostname);
        let token = self.token(&zone.token_encrypted)?;
        let client = hosted_client(10).map_err(DnsError::Other)?;
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(k) = kind {
            query.push(("type", k.to_owned()));
        }
        query.push(("name", hostname.clone()));
        query.push(("per_page", "100".to_owned()));
        let response = request(
            &client,
            Method::GET,
            format!("{}/zones/{}/dns_records", api(), zone.provider_zone_id),
            &token,
        )
        .query(&query)
        .send_retry()
        .await
        .map_err(|e| DnsError::Other(e.to_string()))?;
        let (status, body) = parse(response).await;
        if status == 403 {
            return Err(DnsError::Other(missing_permission_message(
                "Zone → DNS → Edit",
            )));
        }
        if !ok(status) {
            return Err(DnsError::Other(
                "Cloudflare DNS records could not be checked.".into(),
            ));
        }
        Ok(body
            .get("result")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|r| r.is_object())
            .map(|r| remote_of(r, &hostname, kind.unwrap_or("")))
            .collect())
    }

    /// `conflictingRecords`.
    async fn conflicting(
        &self,
        zone: &Zone,
        hostname: &str,
        kind: &str,
    ) -> Result<Vec<Remote>, DnsError> {
        if kind != "CNAME" {
            return self.find_records(zone, hostname, Some(kind)).await;
        }
        let mut all = self.find_records(zone, hostname, None).await?;
        all.retain(|r| matches!(r.kind.as_str(), "A" | "AAAA" | "CNAME"));
        Ok(all)
    }

    /// `trackExistingRecord`.
    async fn track_existing(
        &self,
        zone: &Zone,
        remote: &Remote,
        kind: &str,
        name: &str,
        content: &str,
        resource: Option<&DnsResource>,
    ) -> Result<Record, DnsError> {
        let existing = store::record_by_provider_id(self.db, zone.id, &remote.id).await?;
        let record = match existing {
            Some(r) => {
                let owned =
                    r.owned && remote.comment.as_deref() == Some(&ownership_comment(&r.uuid));
                store::update_tracked_record(self.db, r.id, kind, name, content, owned, self.now)
                    .await?;
                Record {
                    kind: kind.to_owned(),
                    name: name.to_owned(),
                    content: content.to_owned(),
                    owned,
                    ..r
                }
            }
            None => {
                store::create_record(
                    self.db,
                    &store::NewRecord {
                        uuid: &new_public_id(),
                        team_id: zone.team_id,
                        integration_token_id: zone.integration_token_id,
                        dns_provider_zone_id: zone.id,
                        provider_record_id: &remote.id,
                        kind,
                        name,
                        content,
                        owned: false,
                    },
                    self.now,
                )
                .await?
            }
        };
        if let Some(res) = resource {
            store::add_reference(self.db, record.id, &res.morph, res.id, self.now).await?;
        }
        Ok(record)
    }

    /// `createRecord`.
    pub async fn create_record(
        &self,
        zone: &Zone,
        hostname: &str,
        content: &str,
        resource: Option<&DnsResource>,
        additional: &[DnsResource],
        proxied: Option<bool>,
    ) -> Result<Record, DnsError> {
        let hostname = normalize(hostname);
        let _guard = hostname_lock(zone.team_id, &hostname).await?;
        let record = self
            .create_record_locked(zone, &hostname, content, resource, proxied)
            .await?;
        for extra in additional {
            store::add_reference(self.db, record.id, &extra.morph, extra.id, self.now).await?;
        }
        Ok(record)
    }

    async fn create_record_locked(
        &self,
        zone: &Zone,
        hostname: &str,
        content: &str,
        resource: Option<&DnsResource>,
        proxied: Option<bool>,
    ) -> Result<Record, DnsError> {
        let kind = record_type(content);
        let remotes = self.conflicting(zone, hostname, kind).await?;
        if let Some(matching) = remotes
            .iter()
            .find(|r| r.kind == kind && same_address(Some(&r.content), Some(content)))
        {
            if matching.id.is_empty() {
                return Err(DnsError::Other(
                    "Cloudflare DNS records could not be checked.".into(),
                ));
            }
            let record = self
                .track_existing(zone, matching, kind, hostname, content, resource)
                .await?;
            self.audit("adopted", zone, hostname, resource).await;
            return Ok(record);
        }
        if remotes.len() > 1 {
            return Err(DnsError::Other(format!(
                "Several DNS records already exist for {hostname}. Update them in Cloudflare."
            )));
        }
        if let Some(r) = remotes.first() {
            return Err(DnsError::Conflict {
                record_id: r.id.clone(),
                current: r.content.clone(),
                proposed: content.to_owned(),
            });
        }
        let uuid = new_public_id();
        let token = self.token(&zone.token_encrypted)?;
        let metadata: Value = zone
            .metadata
            .as_deref()
            .and_then(|m| serde_json::from_str(m).ok())
            .unwrap_or(Value::Null);
        let proxied = proxied
            .unwrap_or_else(|| metadata.get("proxied_dns").and_then(Value::as_bool) == Some(true));
        let client = hosted_client(10).map_err(DnsError::Other)?;
        let response = request(
            &client,
            Method::POST,
            format!("{}/zones/{}/dns_records", api(), zone.provider_zone_id),
            &token,
        )
        .json(&json!({
            "type": kind,
            "name": hostname,
            "content": content,
            "ttl": 1,
            "proxied": proxied,
            "comment": ownership_comment(&uuid),
        }))
        .send_retry()
        .await
        .map_err(|e| DnsError::Other(e.to_string()))?;
        let (status, body) = parse(response).await;
        if status == 403 {
            return Err(DnsError::Other(missing_permission_message(
                "Zone → DNS → Edit",
            )));
        }
        let id = body.pointer("/result/id").and_then(Value::as_str);
        let Some(id) = id.filter(|_| ok(status)) else {
            return Err(DnsError::Other(
                "Cloudflare could not create the DNS record.".into(),
            ));
        };
        let record = store::create_record(
            self.db,
            &store::NewRecord {
                uuid: &uuid,
                team_id: zone.team_id,
                integration_token_id: zone.integration_token_id,
                dns_provider_zone_id: zone.id,
                provider_record_id: id,
                kind,
                name: hostname,
                content,
                owned: true,
            },
            self.now,
        )
        .await?;
        if let Some(res) = resource {
            store::add_reference(self.db, record.id, &res.morph, res.id, self.now).await?;
        }
        self.audit("created", zone, hostname, resource).await;
        Ok(record)
    }

    /// `replaceRecord`.
    #[allow(clippy::too_many_arguments)]
    pub async fn replace_record(
        &self,
        zone: &Zone,
        record_id: &str,
        hostname: &str,
        content: &str,
        resource: Option<&DnsResource>,
        expected_current: Option<&str>,
        additional: &[DnsResource],
        proxied: bool,
    ) -> Result<Record, DnsError> {
        let hostname = normalize(hostname);
        let _guard = hostname_lock(zone.team_id, &hostname).await?;
        let kind = record_type(content);
        let remotes = self.conflicting(zone, &hostname, kind).await?;
        if remotes.len() > 1 {
            return Err(DnsError::Other(format!(
                "Several DNS records already exist for {hostname}. Update them in Cloudflare."
            )));
        }
        let gone = || {
            DnsError::Other(
                "The DNS conflict is no longer available. Check the record again.".into(),
            )
        };
        let remote = remotes.first().ok_or_else(gone)?;
        if remote.id.is_empty()
            || remote.id != record_id
            || expected_current.is_some_and(|e| !same_address(Some(&remote.content), Some(e)))
            || remote.name != hostname
        {
            return Err(gone());
        }
        let mut changes = Map::new();
        changes.insert("content".into(), json!(content));
        if remote.kind != kind {
            changes.insert("type".into(), json!(kind));
        }
        if proxied {
            changes.insert("proxied".into(), json!(true));
        }
        let token = self.token(&zone.token_encrypted)?;
        let client = hosted_client(10).map_err(DnsError::Other)?;
        let response = request(
            &client,
            Method::PATCH,
            format!(
                "{}/zones/{}/dns_records/{}",
                api(),
                zone.provider_zone_id,
                remote.id
            ),
            &token,
        )
        .json(&Value::Object(changes))
        .send_retry()
        .await
        .map_err(|e| DnsError::Other(e.to_string()))?;
        let status = response.status().as_u16();
        if status == 403 {
            return Err(DnsError::Other(missing_permission_message(
                "Zone → DNS → Edit",
            )));
        }
        if !ok(status) {
            return Err(DnsError::Other(
                "Cloudflare could not replace the conflicting DNS record.".into(),
            ));
        }
        let record = self
            .track_existing(zone, remote, kind, &hostname, content, resource)
            .await?;
        self.audit("replaced", zone, &hostname, resource).await;
        for extra in additional {
            store::add_reference(self.db, record.id, &extra.morph, extra.id, self.now).await?;
        }
        Ok(record)
    }

    async fn zone_of_record(&self, record: &Record) -> Result<Option<Zone>, DnsError> {
        Ok(store::zone_in_team(self.db, record.team_id, record.dns_provider_zone_id).await?)
    }

    /// `deleteRecord`.
    pub async fn delete_record(&self, record: &Record, resource: Option<&DnsResource>) -> Deletion {
        if !record.owned {
            return Deletion::NotOwned;
        }
        let Ok(Some(zone)) = self.zone_of_record(record).await else {
            return Deletion::Failed;
        };
        let Ok(token) = self.token(&zone.token_encrypted) else {
            return Deletion::Failed;
        };
        let Ok(client) = hosted_client(10) else {
            return Deletion::Failed;
        };
        let url = format!(
            "{}/zones/{}/dns_records/{}",
            api(),
            zone.provider_zone_id,
            record.provider_record_id
        );
        let Ok(response) = request(&client, Method::GET, url.clone(), &token)
            .send_retry()
            .await
        else {
            return Deletion::Failed;
        };
        let (status, body) = parse(response).await;
        if status == 404 {
            return self
                .forget_deleted(record, &zone, Deletion::AlreadyGone, resource)
                .await;
        }
        let Some(remote) = body.get("result").filter(|r| ok(status) && r.is_object()) else {
            return Deletion::Failed;
        };
        let text = |k: &str| remote.get(k).and_then(Value::as_str);
        if text("type") != Some(&record.kind)
            || text("name").unwrap_or_default().to_lowercase() != record.name
            || !same_address(text("content"), Some(&record.content))
            || text("comment") != Some(&ownership_comment(&record.uuid))
        {
            return Deletion::ChangedExternally;
        }
        let Ok(deleted) = request(&client, Method::DELETE, url, &token)
            .send_retry()
            .await
        else {
            return Deletion::Failed;
        };
        let status = deleted.status().as_u16();
        if status == 404 {
            return self
                .forget_deleted(record, &zone, Deletion::AlreadyGone, resource)
                .await;
        }
        if !ok(status) {
            return Deletion::Failed;
        }
        self.forget_deleted(record, &zone, Deletion::Deleted, resource)
            .await
    }

    /// `forgetDeletedRecord`.
    async fn forget_deleted(
        &self,
        record: &Record,
        zone: &Zone,
        result: Deletion,
        resource: Option<&DnsResource>,
    ) -> Deletion {
        let _ = store::delete_record(self.db, record.id).await;
        let action = if result == Deletion::Deleted {
            "deleted"
        } else {
            "already_deleted"
        };
        self.audit(action, zone, &record.name, resource).await;
        result
    }

    /// `adoptRecordLocked`.
    async fn adopt_locked(&self, record: &Record, zone: &Zone, previous: &str) -> bool {
        let Ok(token) = self.token(&zone.token_encrypted) else {
            return false;
        };
        let Ok(client) = hosted_client(10) else {
            return false;
        };
        let url = format!(
            "{}/zones/{}/dns_records/{}",
            api(),
            zone.provider_zone_id,
            record.provider_record_id
        );
        let Ok(response) = request(&client, Method::GET, url.clone(), &token)
            .send_retry()
            .await
        else {
            return false;
        };
        let (status, body) = parse(response).await;
        let Some(remote) = body.get("result").filter(|r| ok(status) && r.is_object()) else {
            return false;
        };
        let text = |k: &str| remote.get(k).and_then(Value::as_str);
        if text("type") != Some(&record.kind)
            || text("name").unwrap_or_default().to_lowercase() != record.name
            || !same_address(text("content"), Some(&record.content))
            || text("comment") != Some(previous)
        {
            return false;
        }
        let patched = request(&client, Method::PATCH, url, &token)
            .json(&json!({"comment": ownership_comment(&record.uuid)}))
            .send_retry()
            .await;
        if !patched.is_ok_and(|r| r.status().is_success()) {
            return false;
        }
        self.audit("ownership_adopted", zone, &record.name, None)
            .await;
        true
    }

    /// `adoptRecord`.
    pub async fn adopt_record(&self, record: &Record, previous: &str) -> bool {
        let Ok(Some(zone)) = self.zone_of_record(record).await else {
            return false;
        };
        let Ok(_guard) = hostname_lock(record.team_id, &record.name).await else {
            return false;
        };
        self.adopt_locked(record, &zone, previous).await
    }

    /// `reclaimRecord`.
    pub async fn reclaim_record(&self, record: &Record) -> bool {
        let Ok(Some(zone)) = self.zone_of_record(record).await else {
            return false;
        };
        let Ok(_guard) = hostname_lock(record.team_id, &record.name).await else {
            return false;
        };
        let (Ok(token), Ok(client)) = (self.token(&zone.token_encrypted), hosted_client(10)) else {
            return false;
        };
        let url = format!(
            "{}/zones/{}/dns_records/{}",
            api(),
            zone.provider_zone_id,
            record.provider_record_id
        );
        let Ok(response) = request(&client, Method::GET, url, &token)
            .send_retry()
            .await
        else {
            return false;
        };
        let (_, body) = parse(response).await;
        let Some(comment) = body.pointer("/result/comment").and_then(Value::as_str) else {
            return false;
        };
        // `^managed-by: coolify \S+/{uuid}$`
        let matches = comment
            .strip_prefix(OWNERSHIP_COMMENT_PREFIX)
            .and_then(|r| r.strip_prefix(' '))
            .and_then(|r| r.strip_suffix(&format!("/{}", record.uuid)))
            .is_some_and(|id| !id.is_empty() && !id.contains(char::is_whitespace));
        if !matches {
            return false;
        }
        comment == ownership_comment(&record.uuid)
            || self.adopt_locked(record, &zone, comment).await
    }

    /// `ManagedDnsRecordCleanup::release`.
    pub async fn release(
        &self,
        record: &Record,
        releasing: &[(String, i64)],
        delete_record: bool,
        context: Option<&DnsResource>,
    ) -> Result<Option<Deletion>, String> {
        let Ok(_guard) = hostname_lock(record.team_id, &record.name).await else {
            self.audit_skipped(record, "hostname_busy", "warning").await;
            return Ok(Some(Deletion::Busy));
        };
        self.release_locked(record, releasing, delete_record, context)
            .await
            .map_err(|e| e.to_string())
    }

    async fn release_locked(
        &self,
        record: &Record,
        releasing: &[(String, i64)],
        delete_record: bool,
        context: Option<&DnsResource>,
    ) -> Result<Option<Deletion>, coolify_db::DbError> {
        let Some(record) = store::record_by_id(self.db, record.id).await? else {
            return Ok(None);
        };
        if let Some(res) = context
            && hostnames_of_resource(self.db, &res.morph, res.id)
                .await?
                .contains(&record.name)
        {
            return Ok(None);
        }
        let mut live = false;
        for (id, kind, resource_id) in store::references(self.db, record.id).await? {
            if releasing
                .iter()
                .any(|(k, i)| *k == kind && *i == resource_id)
            {
                continue;
            }
            if store::reference_target_exists(self.db, &kind, resource_id).await? {
                live = true;
            } else {
                store::delete_reference(self.db, id).await?;
            }
        }
        if live {
            store::delete_references(self.db, record.id, releasing).await?;
            return Ok(None);
        }
        let users = resources_using_hostname(self.db, &record.name, releasing).await?;
        if !users.is_empty() {
            let same_team: Vec<_> = users
                .iter()
                .filter(|u| u.team_id == Some(record.team_id))
                .collect();
            if same_team.is_empty() {
                store::delete_record(self.db, record.id).await?;
                self.audit_skipped(&record, "hostname_used_by_another_team", "warning")
                    .await;
                return Ok(None);
            }
            for u in same_team {
                store::add_reference(self.db, record.id, &u.morph, u.id, self.now).await?;
            }
            store::delete_references(self.db, record.id, releasing).await?;
            return Ok(None);
        }
        if !record.owned || !delete_record {
            store::delete_record(self.db, record.id).await?;
            let reason = if record.owned {
                "kept_by_user"
            } else {
                "not_owned"
            };
            self.audit_skipped(&record, reason, "info").await;
            return Ok(if record.owned {
                None
            } else {
                Some(Deletion::NotOwned)
            });
        }
        let result = self.delete_record(&record, context).await;
        if result == Deletion::ChangedExternally {
            store::delete_record(self.db, record.id).await?;
            self.audit_skipped(&record, "remote_record_changed", "warning")
                .await;
        } else if result == Deletion::Failed {
            self.audit_skipped(&record, "provider_unavailable", "warning")
                .await;
        }
        Ok(Some(result))
    }

    /// `releaseHostname`.
    pub async fn release_hostname(
        &self,
        resource: &DnsResource,
        hostname: &str,
        team_id: i64,
        delete_record: bool,
    ) -> Result<Option<Deletion>, String> {
        let hostname = normalize(hostname);
        if hostnames_of_resource(self.db, &resource.morph, resource.id)
            .await
            .map_err(|e| e.to_string())?
            .contains(&hostname)
        {
            return Ok(None);
        }
        let keys = vec![(resource.morph.clone(), resource.id)];
        let records =
            store::records_referenced(self.db, Some(team_id), Some(&hostname), None, &keys)
                .await
                .map_err(|e| e.to_string())?;
        let mut worst = None;
        for record in records {
            let next = self
                .release(&record, &keys, delete_record, Some(resource))
                .await?;
            worst = worse(worst, next);
        }
        Ok(worst)
    }

    /// `releaseRemovedHostnames`: the hostnames from `hostnamesOf()` before the edit.
    pub async fn release_removed_hostnames(
        &self,
        resource: &DnsResource,
        previous: &[String],
        team_id: i64,
    ) -> Result<Option<Deletion>, String> {
        let removed = removed_hostnames(self.db, resource, previous).await?;
        let mut worst = None;
        for hostname in removed {
            worst = worse(
                worst,
                self.release_hostname(resource, &hostname, team_id, true)
                    .await?,
            );
        }
        Ok(worst)
    }

    /// `releaseOrphanedRecords`: what happened to records no live resource references, by outcome.
    pub async fn release_orphaned_records(
        &self,
    ) -> Result<std::collections::BTreeMap<&'static str, i64>, String> {
        let mut counts = std::collections::BTreeMap::new();
        let cutoff = self.now - chrono::Duration::minutes(ORPHAN_GRACE_MINUTES);
        let records = store::orphan_candidates(self.db, cutoff)
            .await
            .map_err(|e| e.to_string())?;
        for record in records {
            let references = store::references(self.db, record.id)
                .await
                .map_err(|e| e.to_string())?;
            let mut live = false;
            for (_, resource_type, id) in &references {
                if store::reference_target_exists(self.db, resource_type, *id)
                    .await
                    .map_err(|e| e.to_string())?
                {
                    live = true;
                    break;
                }
            }
            if live {
                continue;
            }
            let outcome = self.release_orphaned_record(&record).await;
            *counts.entry(outcome).or_insert(0) += 1;
        }
        Ok(counts)
    }

    /// `releaseOrphanedRecord`.
    async fn release_orphaned_record(&self, record: &Record) -> &'static str {
        match self.release(record, &[], true, None).await {
            Err(e) => {
                tracing::warn!(managed_dns_record_id = record.id, hostname = %record.name, error = %e, "Managed DNS orphaned record cleanup failed.");
                Deletion::Failed.value()
            }
            Ok(Some(result)) => result.value(),
            Ok(None) => {
                if matches!(store::record_by_id(self.db, record.id).await, Ok(Some(_))) {
                    "kept"
                } else {
                    "forgotten"
                }
            }
        }
    }

    /// `releaseResource`: `false` when the release should be retried.
    pub async fn release_resource(&self, resource_type: &str, id: i64) -> bool {
        let mut keys = vec![(resource_type.to_owned(), id)];
        if resource_type == "App\\Models\\Application"
            && let Ok(previews) = store::preview_ids(self.db, id).await
        {
            keys.extend(
                previews
                    .into_iter()
                    .map(|p| ("App\\Models\\ApplicationPreview".to_owned(), p)),
            );
        }
        let Ok(records) = store::records_referenced(self.db, None, None, None, &keys).await else {
            return false;
        };
        let mut successful = true;
        for record in records {
            match self.release(&record, &keys, true, None).await {
                Ok(Some(r)) if r.should_retry() => successful = false,
                Ok(_) => {}
                Err(e) => {
                    successful = false;
                    tracing::warn!(hostname = %record.name, error = %e, "Managed DNS record cleanup failed after resource deletion.");
                }
            }
        }
        successful
    }
}

/// `removedHostnames`.
async fn removed_hostnames(
    db: &Db,
    resource: &DnsResource,
    previous: &[String],
) -> Result<Vec<String>, String> {
    let current = hostnames_of_resource(db, &resource.morph, resource.id)
        .await
        .map_err(|e| e.to_string())?;
    let mut removed: Vec<String> = Vec::new();
    for h in previous.iter().map(|h| normalize(h)) {
        if !current.contains(&h) && !removed.contains(&h) {
            removed.push(h);
        }
    }
    Ok(removed)
}

/// `queueReleaseOfRemovedHostnames`: nothing is queued when the resource references no managed
/// record for a removed hostname. `resource` is read after its domains were saved.
pub fn queue_release_of_removed_hostnames(
    db: &Db,
    enc: &Encrypter,
    resource: DnsResource,
    previous: Vec<String>,
    team_id: i64,
) {
    if previous.is_empty() {
        return;
    }
    let (db, enc) = (db.clone(), enc.clone());
    tokio::spawn(async move {
        let Ok(removed) = removed_hostnames(&db, &resource, &previous).await else {
            return;
        };
        if removed.is_empty() {
            return;
        }
        let keys = [(resource.morph.clone(), resource.id)];
        let Ok(referenced) = store::records_referenced(&db, Some(team_id), None, None, &keys).await
        else {
            return;
        };
        if !referenced.iter().any(|r| removed.contains(&r.name)) {
            return;
        }
        release_removed_hostnames_job(&db, &enc, &resource, &removed, team_id).await;
    });
}

/// `ReleaseRemovedDnsHostnamesJob`.
pub async fn release_removed_hostnames_job(
    db: &Db,
    enc: &Encrypter,
    resource: &DnsResource,
    hostnames: &[String],
    team_id: i64,
) {
    for attempt in 1..=3u64 {
        // A deleted resource releases all of its records through `ReleaseManagedDnsRecordsJob`.
        if !store::reference_target_exists(db, &resource.morph, resource.id)
            .await
            .unwrap_or(false)
        {
            return;
        }
        let dns = Dns {
            db,
            enc,
            now: chrono::Utc::now().naive_utc(),
            source: "system",
        };
        let mut retry = false;
        for hostname in hostnames {
            match dns
                .release_hostname(resource, hostname, team_id, true)
                .await
            {
                Ok(Some(r)) if r.should_retry() => retry = true,
                Ok(_) => {}
                Err(e) => {
                    retry = true;
                    tracing::warn!(%hostname, error = %e, "Managed DNS cleanup of a removed hostname failed.");
                }
            }
        }
        if !retry {
            return;
        }
        if attempt < 3 {
            tokio::time::sleep(std::time::Duration::from_secs(60 * attempt)).await;
        }
    }
    tracing::warn!(
        resource_type = %resource.morph,
        resource_id = resource.id,
        hostnames = ?hostnames,
        "Managed DNS records of removed hostnames could not be cleaned up; they were left in place."
    );
}

/// What `DnsRecordConfigurationFinished` carries.
#[derive(Debug, Clone)]
pub struct DnsConfigured {
    pub successful: bool,
    pub credential: String,
    pub message: String,
}

/// `ConfigureDnsRecordJob`: only a busy hostname lock is retried.
pub async fn configure_dns_record_job(
    dns: &Dns<'_>,
    team_id: i64,
    zone_id: i64,
    resource: Option<&DnsResource>,
    hostname: &str,
    content: &str,
) -> DnsConfigured {
    let Ok(Some(zone)) = store::zone_in_team(dns.db, team_id, zone_id).await else {
        // `failed()`, after `firstOrFail()`.
        return DnsConfigured {
            successful: false,
            credential: String::new(),
            message: "The DNS zone is no longer available.".to_owned(),
        };
    };
    let finished = |successful: bool, message: String| DnsConfigured {
        successful,
        credential: zone.token_name.clone(),
        message,
    };
    if !is_public_address(content) {
        return finished(false, NO_PUBLIC_ADDRESS_MESSAGE.to_owned());
    }
    for attempt in 1..=3u64 {
        match dns
            .create_record(&zone, hostname, content, resource, &[], None)
            .await
        {
            Ok(_) => return finished(true, format!("DNS record added for {hostname}.")),
            Err(DnsError::Busy) if attempt < 3 => {
                tokio::time::sleep(std::time::Duration::from_secs(10 * attempt)).await;
            }
            Err(DnsError::Busy) => break,
            Err(e) => return finished(false, e.to_string()),
        }
    }
    finished(
        false,
        "Another DNS change for this hostname is still in progress. Try again later.".to_owned(),
    )
}

/// `worseResult`.
pub fn worse(current: Option<Deletion>, next: Option<Deletion>) -> Option<Deletion> {
    let rank = |r: Option<Deletion>| match r {
        Some(Deletion::Failed) => 3,
        Some(Deletion::Busy) => 2,
        Some(Deletion::ChangedExternally) => 1,
        _ => 0,
    };
    if rank(next) > rank(current) {
        next
    } else {
        current
    }
}

/// `parse_url($url, PHP_URL_HOST)` of a stored URL, without a scheme too.
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next()?
    } else {
        authority.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_owned())
}

/// `hostnamesOf` from the columns.
pub fn hostnames_of(fqdn: Option<&str>, compose_domains: Option<&str>) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    let mut split = |v: Option<&str>| {
        for u in v.unwrap_or_default().split(',') {
            let u = u.trim();
            if !u.is_empty() {
                urls.push(u.to_owned());
            }
        }
    };
    split(fqdn);
    if let Some(domains) = compose_domains
        .and_then(|d| serde_json::from_str::<Value>(d).ok())
        .and_then(|v| v.as_object().cloned())
    {
        for service in domains.values() {
            split(service.get("domain").and_then(Value::as_str));
        }
    }
    let mut out: Vec<String> = Vec::new();
    for host in urls
        .iter()
        .filter_map(|u| host_of(u))
        .map(|h| normalize(&h))
    {
        if !out.contains(&host) {
            out.push(host);
        }
    }
    out
}

/// `hostnamesOf($resource->fresh())`.
pub async fn hostnames_of_resource(
    db: &Db,
    morph: &str,
    id: i64,
) -> Result<Vec<String>, coolify_db::DbError> {
    Ok(store::resource_domains(db, morph, id)
        .await?
        .map(|(f, d)| hostnames_of(f.as_deref(), d.as_deref()))
        .unwrap_or_default())
}

/// `resourcesUsingHostname`.
async fn resources_using_hostname(
    db: &Db,
    hostname: &str,
    except: &[(String, i64)],
) -> Result<Vec<store::HostnameUser>, coolify_db::DbError> {
    let hostname = normalize(hostname);
    Ok(store::hostname_candidates(db, &hostname)
        .await?
        .into_iter()
        .filter(|u| !except.iter().any(|(k, i)| *k == u.morph && *i == u.id))
        .filter(|u| {
            hostnames_of(u.fqdn.as_deref(), u.docker_compose_domains.as_deref()).contains(&hostname)
        })
        .collect())
}

/// `auditLog()` outside a request.
async fn audit_log(
    db: &Db,
    team_id: i64,
    event: &str,
    context: Value,
    level: &str,
    now: NaiveDateTime,
) {
    let actor = coolify_db::audit::Actor::default();
    if let Err(e) =
        coolify_db::audit::record(db, Some(team_id), event, &context, level, &actor, None, now)
            .await
    {
        tracing::warn!(event, error = %e, "audit log failed");
    }
}

/// A tunnel the API set up (`server_settings`).
pub fn tunnel_name(server_uuid: &str) -> String {
    format!("coolify-{server_uuid}")
}

/// `CloudflareTunnelService::request`.
async fn tunnel_request(
    client: &Client,
    token: &str,
    method: Method,
    path: &str,
    query: &[(&str, &str)],
    body: Option<&Value>,
) -> Result<Value, DnsError> {
    let mut builder = request(client, method, format!("{}{path}", api()), token).query(query);
    if let Some(b) = body {
        builder = builder.json(b);
    }
    let response = builder
        .send_retry()
        .await
        .map_err(|e| DnsError::Other(e.to_string()))?;
    let (status, value) = parse(response).await;
    if status == 403 {
        return Err(DnsError::Other(missing_permission_message(
            TUNNEL_PERMISSION,
        )));
    }
    if !ok(status) || value.get("success").and_then(Value::as_bool) == Some(false) {
        let message = value.pointer("/errors/0/message").and_then(Value::as_str);
        return Err(DnsError::Other(match message {
            Some(m) => format!("Cloudflare Tunnel request failed: {m}"),
            None => "Cloudflare Tunnel request failed.".to_owned(),
        }));
    }
    Ok(value)
}

/// `findOrCreateTunnel`: id, token and whether this call created it.
async fn find_or_create_tunnel(
    client: &Client,
    token: &str,
    account: &str,
    name: &str,
) -> Result<(String, String, bool), DnsError> {
    let list = tunnel_request(
        client,
        token,
        Method::GET,
        &format!("/accounts/{account}/cfd_tunnel"),
        &[("name", name), ("is_deleted", "false")],
        None,
    )
    .await?;
    let existing = list
        .get("result")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|t| t.get("name").and_then(Value::as_str) == Some(name))
        .and_then(|t| t.get("id").and_then(Value::as_str).map(str::to_owned));
    let (id, tunnel_token, created) = match existing {
        Some(id) => {
            let t = tunnel_request(
                client,
                token,
                Method::GET,
                &format!("/accounts/{account}/cfd_tunnel/{id}/token"),
                &[],
                None,
            )
            .await?;
            let value = t.get("result").and_then(Value::as_str).map(str::to_owned);
            (Some(id), value, false)
        }
        None => {
            let r = tunnel_request(
                client,
                token,
                Method::POST,
                &format!("/accounts/{account}/cfd_tunnel"),
                &[],
                Some(&json!({"name": name, "config_src": "cloudflare"})),
            )
            .await?;
            let text = |p: &str| r.pointer(p).and_then(Value::as_str).map(str::to_owned);
            (text("/result/id"), text("/result/token"), true)
        }
    };
    match (id, tunnel_token.filter(|t| !t.is_empty())) {
        (Some(id), Some(t)) => Ok((id, t, created)),
        _ => Err(DnsError::Other(
            "Cloudflare returned no tunnel token.".into(),
        )),
    }
}

/// `tearDown`/`setUp`'s `releaseDnsRecords`.
async fn release_dns_records(
    dns: &Dns<'_>,
    team_id: i64,
    server_id: i64,
    except: Option<&str>,
) -> Vec<String> {
    let keys = vec![("App\\Models\\Server".to_owned(), server_id)];
    let Ok(records) = store::records_referenced(dns.db, Some(team_id), None, except, &keys).await
    else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    for record in records {
        match dns.release(&record, &keys, true, None).await {
            Ok(Some(Deletion::ChangedExternally)) => warnings.push(format!(
                "The DNS record {} changed outside Coolify and was left untouched.",
                record.name
            )),
            Ok(Some(Deletion::Failed | Deletion::Busy)) | Err(_) => warnings.push(format!(
                "The DNS record {} could not be deleted. Delete it in Cloudflare.",
                record.name
            )),
            _ => {}
        }
    }
    warnings
}

/// `zoneFor`.
async fn zone_for(
    dns: &Dns<'_>,
    team_id: i64,
    token_id: i64,
    token: &str,
    token_name: &str,
    hostname: &str,
) -> Result<Zone, DnsError> {
    let find = || async {
        find_zones(dns.db, team_id, hostname)
            .await
            .map(|z| z.into_iter().find(|x| x.integration_token_id == token_id))
    };
    let mut zone = find().await.map_err(DnsError::Other)?;
    if zone.is_none() {
        sync_zones(dns.db, token_id, token, dns.now)
            .await
            .map_err(DnsError::Other)?;
        zone = find().await.map_err(DnsError::Other)?;
    }
    zone.ok_or_else(|| {
        DnsError::Other(format!(
            "No Cloudflare zone of the token {token_name} manages {hostname}."
        ))
    })
}

/// An existing DNS record the user agreed to replace.
#[derive(Debug, Clone)]
pub struct ConfirmedConflict {
    pub record_id: String,
    pub current: String,
}

/// `CloudflareTunnelService::setUp`: the activity of `ConfigureCloudflared`.
pub async fn tunnel_set_up(
    dns: &Dns<'_>,
    srv: &Srv,
    token_id: i64,
    token_name: &str,
    token: &str,
    hostname: &str,
    confirmed: Option<&ConfirmedConflict>,
) -> Result<i64, DnsError> {
    let (team_id, server_id, uuid, name, port) = srv.tunnel_facts();
    let zone = zone_for(dns, team_id, token_id, token, token_name, hostname).await?;
    let account = zone
        .account_id
        .clone()
        .filter(|a| !a.trim().is_empty())
        .ok_or_else(|| {
            DnsError::Other(format!(
                "Cloudflare returned no account for the zone {}.",
                zone.name
            ))
        })?;
    let client = hosted_client(15).map_err(DnsError::Other)?;
    let (tunnel_id, tunnel_token, created) =
        find_or_create_tunnel(&client, token, &account, &tunnel_name(&uuid)).await?;
    let resource = DnsResource {
        morph: "App\\Models\\Server".into(),
        id: server_id,
        uuid: Some(uuid.clone()),
        name: Some(name),
        type_name: "server".into(),
    };
    let target = format!("{tunnel_id}.cfargotunnel.com");
    let configured: Result<(), DnsError> = async {
        tunnel_request(
            &client,
            token,
            Method::PUT,
            &format!("/accounts/{account}/cfd_tunnel/{tunnel_id}/configurations"),
            &[],
            Some(&json!({
                "config": {"ingress": [
                    {"hostname": hostname, "service": format!("ssh://localhost:{port}")},
                    {"service": "http_status:404"},
                ]},
            })),
        )
        .await?;
        match confirmed {
            Some(c) => {
                dns.replace_record(
                    &zone,
                    &c.record_id,
                    hostname,
                    &target,
                    Some(&resource),
                    Some(&c.current),
                    &[],
                    true,
                )
                .await?;
            }
            None => {
                dns.create_record(&zone, hostname, &target, Some(&resource), &[], Some(true))
                    .await?;
            }
        }
        Ok(())
    }
    .await;
    if let Err(e) = configured {
        // `deleteUntrackedTunnel`: a re-run reuses the tunnel by name anyway.
        if created {
            let _ = request(
                &client,
                Method::DELETE,
                format!("{}/accounts/{account}/cfd_tunnel/{tunnel_id}", api()),
                token,
            )
            .send_retry()
            .await;
        }
        return Err(e);
    }
    release_dns_records(dns, team_id, server_id, Some(hostname)).await;
    store::set_tunnel_settings(
        dns.db,
        server_id,
        Some(&tunnel_id),
        Some(&account),
        Some(token_id),
        dns.now,
    )
    .await?;
    tunnel::configure_cloudflared(srv, &tunnel_token, hostname)
        .await
        .map_err(DnsError::Other)
}

/// `deleteStoppedTunnel`.
async fn delete_stopped_tunnel(
    token: &str,
    account: &str,
    tunnel_id: &str,
    name: &str,
) -> Option<String> {
    let did_not_respond =
        || format!("Cloudflare did not respond. Delete the tunnel {name} in Cloudflare.");
    let Ok(client) = hosted_client(15) else {
        return Some(did_not_respond());
    };
    let path = format!("{}/accounts/{account}/cfd_tunnel/{tunnel_id}", api());
    for attempt in 1..=DELETE_ATTEMPTS {
        if request(
            &client,
            Method::DELETE,
            format!("{path}/connections"),
            token,
        )
        .send_retry()
        .await
        .is_err()
        {
            return Some(did_not_respond());
        }
        let Ok(response) = request(&client, Method::DELETE, path.clone(), token)
            .send_retry()
            .await
        else {
            return Some(did_not_respond());
        };
        let status = response.status().as_u16();
        if ok(status) || status == 404 {
            return None;
        }
        if status == 403 {
            return Some(format!(
                "{} Delete the tunnel {name} in Cloudflare.",
                missing_permission_message(TUNNEL_PERMISSION)
            ));
        }
        if attempt < DELETE_ATTEMPTS {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
    Some(format!(
        "Cloudflare could not delete the tunnel {name}. Delete it in Cloudflare."
    ))
}

/// `CloudflareTunnelService::tearDown`: warnings for the user; the tunnel is always forgotten.
pub async fn tunnel_tear_down(dns: &Dns<'_>, srv: &Srv) -> Vec<String> {
    let (team_id, server_id, uuid, _, _) = srv.tunnel_facts();
    let Ok(Some((Some(tunnel_id), account, token_id))) =
        store::tunnel_settings(dns.db, server_id).await
    else {
        return Vec::new();
    };
    if tunnel_id.trim().is_empty() {
        return Vec::new();
    }
    if srv.info.transferred {
        return vec!["This server is managed by another Coolify instance.".to_owned()];
    }
    let name = tunnel_name(&uuid);
    let mut warnings = Vec::new();
    // The SSH session may run through this tunnel, so the stop is detached and the session ends first.
    tunnel::stop_cloudflared_detached(srv).await;
    warnings.extend(release_dns_records(dns, team_id, server_id, None).await);
    let token = match token_id {
        Some(id) => store::token_in_team(dns.db, team_id, id)
            .await
            .ok()
            .flatten(),
        None => None,
    };
    match token.and_then(|(_, c)| dns.enc.decrypt_string(&c).ok()) {
        None => warnings.push(format!(
            "The Cloudflare token of the tunnel no longer exists. Delete the tunnel {name} in Cloudflare."
        )),
        Some(token) => {
            if let Some(w) = delete_stopped_tunnel(
                &token,
                account.as_deref().unwrap_or_default(),
                &tunnel_id,
                &name,
            )
            .await
            {
                warnings.push(w);
            }
        }
    }
    let _ = store::set_tunnel_settings(dns.db, server_id, None, None, None, dns.now).await;
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_addresses() {
        for p in ["1.1.1.1", "8.8.8.8", "2606:4700::1111"] {
            assert!(is_public_address(p), "{p}");
        }
        for np in [
            "10.0.0.1",
            "192.168.1.1",
            "172.16.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "224.0.0.1",
            "fd00::1",
            "fe80::1",
            "::1",
            "ff02::1",
            "::ffff:10.0.0.1",
            "nope",
        ] {
            assert!(!is_public_address(np), "{np}");
        }
    }

    #[test]
    fn addresses_compare_in_binary() {
        assert!(same_address(Some("2001:db8::1"), Some("2001:0DB8:0:0::1")));
        assert!(same_address(Some("x.example"), Some("x.example")));
        assert!(!same_address(Some("x"), Some("y")));
        assert!(!same_address(None, Some("y")));
    }

    #[test]
    fn hostnames() {
        assert_eq!(
            hostnames_of(
                Some("https://A.example.com:8080/x, b.example.com"),
                Some(r#"{"web":{"domain":"https://c.example.com,https://a.example.com"}}"#)
            ),
            ["a.example.com", "b.example.com", "c.example.com"]
        );
        assert!(hostnames_of(None, None).is_empty());
    }

    #[test]
    fn record_types() {
        assert_eq!(record_type("1.2.3.4"), "A");
        assert_eq!(record_type("2001:db8::1"), "AAAA");
        assert_eq!(record_type("x.cfargotunnel.com"), "CNAME");
    }
}
