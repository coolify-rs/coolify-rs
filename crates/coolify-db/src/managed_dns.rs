//! `dns_provider_zones`, `managed_dns_records` and `managed_dns_record_references`
//! (`DnsProviderZone`, `ManagedDnsRecord`, `ManagedDnsRecordReference`).

use chrono::NaiveDateTime;
use coolify_domain::new_public_id;

use crate::{Db, DbResult};

/// A zone with its Cloudflare token.
#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct Zone {
    pub id: i64,
    pub integration_token_id: i64,
    pub provider_zone_id: String,
    pub name: String,
    pub account_id: Option<String>,
    pub token_name: String,
    pub token_encrypted: String,
    pub team_id: i64,
    /// JSON text of the token's `metadata`.
    pub metadata: Option<String>,
}

const ZONE_SELECT: &str = "SELECT z.id, z.integration_token_id, z.provider_zone_id, z.name, z.account_id, t.name, t.token, t.team_id, t.metadata::text \
     FROM dns_provider_zones z JOIN integration_tokens t ON t.id = z.integration_token_id";

type ZoneRow = (
    i64,
    i64,
    String,
    String,
    Option<String>,
    String,
    String,
    i64,
    Option<String>,
);

fn zone_of(r: ZoneRow) -> Zone {
    Zone {
        id: r.0,
        integration_token_id: r.1,
        provider_zone_id: r.2,
        name: r.3,
        account_id: r.4,
        token_name: r.5,
        token_encrypted: r.6,
        team_id: r.7,
        metadata: r.8,
    }
}

/// `zonesForTeam`.
pub async fn zones_for_team(db: &Db, team_id: i64) -> DbResult<Vec<Zone>> {
    let rows: Vec<ZoneRow> = sqlx::query_as(&format!(
        "{ZONE_SELECT} WHERE t.team_id = $1 AND t.provider = 'cloudflare' ORDER BY z.id"
    ))
    .bind(team_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().map(zone_of).collect())
}

/// `$token->dnsZones`.
pub async fn zones_of_token(db: &Db, token_id: i64) -> DbResult<Vec<Zone>> {
    let rows: Vec<ZoneRow> = sqlx::query_as(&format!(
        "{ZONE_SELECT} WHERE z.integration_token_id = $1 ORDER BY z.name"
    ))
    .bind(token_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().map(zone_of).collect())
}

/// `findTeamZone` without a hostname.
pub async fn zone_in_team(db: &Db, team_id: i64, zone_id: i64) -> DbResult<Option<Zone>> {
    let row: Option<ZoneRow> =
        sqlx::query_as(&format!("{ZONE_SELECT} WHERE z.id = $1 AND t.team_id = $2"))
            .bind(zone_id)
            .bind(team_id)
            .fetch_optional(db.pool())
            .await?;
    Ok(row.map(zone_of))
}

/// One zone of `syncZones`.
#[derive(Debug, Clone)]
pub struct SyncedZone {
    pub provider_zone_id: String,
    pub name: String,
    pub account_id: Option<String>,
    pub account_name: Option<String>,
}

/// The transaction of `syncZones`.
pub async fn store_synced_zones(
    db: &Db,
    token_id: i64,
    zones: &[SyncedZone],
    synced_at: &str,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    let mut tx = db.pool().begin().await?;
    let mut ids = Vec::new();
    for z in zones {
        ids.push(z.provider_zone_id.clone());
        let updated = sqlx::query(
            "UPDATE dns_provider_zones SET name = $3, account_id = $4, account_name = $5, updated_at = $6 \
             WHERE integration_token_id = $1 AND provider_zone_id = $2",
        )
        .bind(token_id)
        .bind(&z.provider_zone_id)
        .bind(&z.name)
        .bind(&z.account_id)
        .bind(&z.account_name)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            sqlx::query(
                "INSERT INTO dns_provider_zones (uuid, integration_token_id, provider_zone_id, name, account_id, account_name, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $7)",
            )
            .bind(new_public_id())
            .bind(token_id)
            .bind(&z.provider_zone_id)
            .bind(&z.name)
            .bind(&z.account_id)
            .bind(&z.account_name)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
    }
    sqlx::query(
        "DELETE FROM dns_provider_zones z WHERE z.integration_token_id = $1 AND NOT (z.provider_zone_id = ANY($2)) \
         AND NOT EXISTS (SELECT 1 FROM managed_dns_records r WHERE r.dns_provider_zone_id = z.id)",
    )
    .bind(token_id)
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE integration_tokens SET metadata = (COALESCE(metadata::jsonb, '{}'::jsonb) || jsonb_build_object('zones_synced_at', $2::text))::json, updated_at = $3 WHERE id = $1",
    )
    .bind(token_id)
    .bind(synced_at)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// A `managed_dns_records` row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Record {
    pub id: i64,
    pub uuid: String,
    pub team_id: i64,
    pub integration_token_id: i64,
    pub dns_provider_zone_id: i64,
    pub provider_record_id: String,
    #[sqlx(rename = "type")]
    pub kind: String,
    pub name: String,
    pub content: String,
    pub owned: bool,
}

const RECORD_COLUMNS: &str = "id, uuid, team_id, integration_token_id, dns_provider_zone_id, provider_record_id, type, name, content, owned";

/// What a new row holds.
#[derive(Debug)]
pub struct NewRecord<'a> {
    pub uuid: &'a str,
    pub team_id: i64,
    pub integration_token_id: i64,
    pub dns_provider_zone_id: i64,
    pub provider_record_id: &'a str,
    pub kind: &'a str,
    pub name: &'a str,
    pub content: &'a str,
    pub owned: bool,
}

pub async fn create_record(db: &Db, n: &NewRecord<'_>, now: NaiveDateTime) -> DbResult<Record> {
    db.ensure_writable()?;
    Ok(sqlx::query_as(&format!(
        "INSERT INTO managed_dns_records (uuid, team_id, integration_token_id, dns_provider_zone_id, provider_record_id, type, name, content, owned, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10) RETURNING {RECORD_COLUMNS}"
    ))
    .bind(n.uuid)
    .bind(n.team_id)
    .bind(n.integration_token_id)
    .bind(n.dns_provider_zone_id)
    .bind(n.provider_record_id)
    .bind(n.kind)
    .bind(n.name)
    .bind(n.content)
    .bind(n.owned)
    .bind(now)
    .fetch_one(db.pool())
    .await?)
}

pub async fn record_by_provider_id(
    db: &Db,
    zone_id: i64,
    provider_record_id: &str,
) -> DbResult<Option<Record>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {RECORD_COLUMNS} FROM managed_dns_records WHERE dns_provider_zone_id = $1 AND provider_record_id = $2"
    ))
    .bind(zone_id)
    .bind(provider_record_id)
    .fetch_optional(db.pool())
    .await?)
}

pub async fn record_by_id(db: &Db, id: i64) -> DbResult<Option<Record>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {RECORD_COLUMNS} FROM managed_dns_records WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(db.pool())
    .await?)
}

/// `$record->fill([...])->save()` for an existing row.
pub async fn update_tracked_record(
    db: &Db,
    id: i64,
    kind: &str,
    name: &str,
    content: &str,
    owned: bool,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE managed_dns_records SET type = $2, name = $3, content = $4, owned = $5, updated_at = $6 WHERE id = $1",
    )
    .bind(id)
    .bind(kind)
    .bind(name)
    .bind(content)
    .bind(owned)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn delete_record(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM managed_dns_records WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `addReference`: `firstOrCreate`.
pub async fn add_reference(
    db: &Db,
    record_id: i64,
    resource_type: &str,
    resource_id: i64,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "INSERT INTO managed_dns_record_references (managed_dns_record_id, resource_type, resource_id, created_at, updated_at) \
         SELECT $1, $2, $3, $4, $4 WHERE NOT EXISTS (SELECT 1 FROM managed_dns_record_references \
         WHERE managed_dns_record_id = $1 AND resource_type = $2 AND resource_id = $3)",
    )
    .bind(record_id)
    .bind(resource_type)
    .bind(resource_id)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `$record->references()`: `(id, resource_type, resource_id)`.
pub async fn references(db: &Db, record_id: i64) -> DbResult<Vec<(i64, String, i64)>> {
    Ok(sqlx::query_as(
        "SELECT id, resource_type, resource_id FROM managed_dns_record_references WHERE managed_dns_record_id = $1 ORDER BY id",
    )
    .bind(record_id)
    .fetch_all(db.pool())
    .await?)
}

pub async fn delete_reference(db: &Db, id: i64) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query("DELETE FROM managed_dns_record_references WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// `deleteReferences`.
pub async fn delete_references(db: &Db, record_id: i64, keys: &[(String, i64)]) -> DbResult<()> {
    db.ensure_writable()?;
    for (kind, id) in keys {
        sqlx::query(
            "DELETE FROM managed_dns_record_references WHERE managed_dns_record_id = $1 AND resource_type = $2 AND resource_id = $3",
        )
        .bind(record_id)
        .bind(kind)
        .bind(id)
        .execute(db.pool())
        .await?;
    }
    Ok(())
}

/// Records of a team and hostname that a resource references.
pub async fn records_referenced(
    db: &Db,
    team_id: Option<i64>,
    name: Option<&str>,
    except_name: Option<&str>,
    keys: &[(String, i64)],
) -> DbResult<Vec<Record>> {
    let mut out: Vec<Record> = Vec::new();
    for (kind, id) in keys {
        let rows: Vec<Record> = sqlx::query_as(&format!(
            "SELECT {RECORD_COLUMNS} FROM managed_dns_records r WHERE ($1::bigint IS NULL OR team_id = $1) \
             AND ($2::text IS NULL OR name = $2) AND ($3::text IS NULL OR name <> $3) \
             AND EXISTS (SELECT 1 FROM managed_dns_record_references x WHERE x.managed_dns_record_id = r.id AND x.resource_type = $4 AND x.resource_id = $5) ORDER BY id"
        ))
        .bind(team_id)
        .bind(name)
        .bind(except_name)
        .bind(kind)
        .bind(id)
        .fetch_all(db.pool())
        .await?;
        for r in rows {
            if !out.iter().any(|x| x.id == r.id) {
                out.push(r);
            }
        }
    }
    Ok(out)
}

/// `ManagedDnsRecord::query()->where('team_id', $teamId)->whereIn('name', $hostnames)`.
pub async fn managed_names(db: &Db, team_id: i64, names: &[String]) -> DbResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT name FROM managed_dns_records WHERE team_id = $1 AND name = ANY($2)",
    )
    .bind(team_id)
    .bind(names)
    .fetch_all(db.pool())
    .await?)
}

/// A live resource whose `fqdn` or `docker_compose_domains` may hold a hostname.
#[derive(Debug, Clone)]
pub struct HostnameUser {
    pub morph: String,
    pub id: i64,
    pub fqdn: Option<String>,
    pub docker_compose_domains: Option<String>,
    pub team_id: Option<i64>,
}

type UserRow = (i64, Option<String>, Option<String>, Option<i64>);

/// The queries of `resourcesUsingHostname`, before its exact hostname check.
pub async fn hostname_candidates(db: &Db, hostname: &str) -> DbResult<Vec<HostnameUser>> {
    let pattern = format!("%{hostname}%");
    let mut out = Vec::new();
    let apps: Vec<UserRow> = sqlx::query_as(
        "SELECT a.id, a.fqdn, a.docker_compose_domains, p.team_id FROM applications a \
         LEFT JOIN environments e ON e.id = a.environment_id LEFT JOIN projects p ON p.id = e.project_id \
         WHERE a.deleted_at IS NULL AND (LOWER(a.fqdn) LIKE $1 OR LOWER(a.docker_compose_domains) LIKE $1)",
    )
    .bind(&pattern)
    .fetch_all(db.pool())
    .await?;
    out.extend(apps.into_iter().map(|(id, f, d, t)| HostnameUser {
        morph: "App\\Models\\Application".into(),
        id,
        fqdn: f,
        docker_compose_domains: d,
        team_id: t,
    }));
    let previews: Vec<UserRow> = sqlx::query_as(
        "SELECT x.id, x.fqdn, x.docker_compose_domains, p.team_id FROM application_previews x \
         LEFT JOIN applications a ON a.id = x.application_id LEFT JOIN environments e ON e.id = a.environment_id \
         LEFT JOIN projects p ON p.id = e.project_id \
         WHERE x.deleted_at IS NULL AND (LOWER(x.fqdn) LIKE $1 OR LOWER(x.docker_compose_domains) LIKE $1)",
    )
    .bind(&pattern)
    .fetch_all(db.pool())
    .await?;
    out.extend(previews.into_iter().map(|(id, f, d, t)| HostnameUser {
        morph: "App\\Models\\ApplicationPreview".into(),
        id,
        fqdn: f,
        docker_compose_domains: d,
        team_id: t,
    }));
    let services: Vec<(i64, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT x.id, x.fqdn, p.team_id FROM service_applications x \
         LEFT JOIN services s ON s.id = x.service_id LEFT JOIN environments e ON e.id = s.environment_id \
         LEFT JOIN projects p ON p.id = e.project_id \
         WHERE x.deleted_at IS NULL AND LOWER(x.fqdn) LIKE $1",
    )
    .bind(&pattern)
    .fetch_all(db.pool())
    .await?;
    out.extend(services.into_iter().map(|(id, f, t)| HostnameUser {
        morph: "App\\Models\\ServiceApplication".into(),
        id,
        fqdn: f,
        docker_compose_domains: None,
        team_id: t,
    }));
    Ok(out)
}

/// `$reference->resource !== null`: the morph target exists and is not soft-deleted.
pub async fn reference_target_exists(db: &Db, resource_type: &str, id: i64) -> DbResult<bool> {
    let table = match resource_type {
        "App\\Models\\Application" => "applications",
        "App\\Models\\ApplicationPreview" => "application_previews",
        "App\\Models\\ServiceApplication" => "service_applications",
        "App\\Models\\Server" => "servers",
        _ => return Ok(false),
    };
    let row: Option<Option<String>> = sqlx::query_scalar(&format!(
        "SELECT to_jsonb(t)->>'deleted_at' FROM {table} t WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    Ok(matches!(row, Some(None)))
}

/// `$resource->fqdn` and `docker_compose_domains` of a resource, for `hostnamesOf`.
pub async fn resource_domains(
    db: &Db,
    resource_type: &str,
    id: i64,
) -> DbResult<Option<(Option<String>, Option<String>)>> {
    let (table, compose) = match resource_type {
        "App\\Models\\Application" => ("applications", true),
        "App\\Models\\ApplicationPreview" => ("application_previews", true),
        "App\\Models\\ServiceApplication" => ("service_applications", false),
        _ => return Ok(None),
    };
    let column = if compose {
        "docker_compose_domains"
    } else {
        "NULL::text"
    };
    Ok(
        sqlx::query_as(&format!("SELECT fqdn, {column} FROM {table} WHERE id = $1"))
            .bind(id)
            .fetch_optional(db.pool())
            .await?,
    )
}

/// The ids of an application's previews, also the soft-deleted ones (`resourceKeys`).
pub async fn preview_ids(db: &Db, application_id: i64) -> DbResult<Vec<i64>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM application_previews WHERE application_id = $1")
            .bind(application_id)
            .fetch_all(db.pool())
            .await?,
    )
}

/// `server_settings` columns of a tunnel the API set up.
pub async fn tunnel_settings(
    db: &Db,
    server_id: i64,
) -> DbResult<Option<(Option<String>, Option<String>, Option<i64>)>> {
    Ok(sqlx::query_as(
        "SELECT cloudflare_tunnel_id, cloudflare_tunnel_account_id, cloudflare_tunnel_integration_token_id FROM server_settings WHERE server_id = $1",
    )
    .bind(server_id)
    .fetch_optional(db.pool())
    .await?)
}

/// Saves or forgets the tunnel of a server.
pub async fn set_tunnel_settings(
    db: &Db,
    server_id: i64,
    tunnel_id: Option<&str>,
    account_id: Option<&str>,
    token_id: Option<i64>,
    now: NaiveDateTime,
) -> DbResult<()> {
    db.ensure_writable()?;
    sqlx::query(
        "UPDATE server_settings SET cloudflare_tunnel_id = $2, cloudflare_tunnel_account_id = $3, cloudflare_tunnel_integration_token_id = $4, updated_at = $5 WHERE server_id = $1",
    )
    .bind(server_id)
    .bind(tunnel_id)
    .bind(account_id)
    .bind(token_id)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// `IntegrationToken::query()->where('team_id', $teamId)->whereKey($id)`: `(name, ciphertext)`.
pub async fn token_in_team(db: &Db, team_id: i64, id: i64) -> DbResult<Option<(String, String)>> {
    Ok(
        sqlx::query_as("SELECT name, token FROM integration_tokens WHERE team_id = $1 AND id = $2")
            .bind(team_id)
            .bind(id)
            .fetch_optional(db.pool())
            .await?,
    )
}
