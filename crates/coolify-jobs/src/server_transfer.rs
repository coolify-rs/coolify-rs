//! `ServerTransferClaimer`: take a server over, mark it transferred, and read the owner marker on
//! the host. The bundle code lives in `coolify-api`; this part also runs from
//! `ServerConnectionCheckJob`.

use chrono::Utc;
use coolify_domain::new_public_id;
use serde_json::{Map, Value, json};
use sqlx::{PgConnection, Row};

use crate::Jobs;
use crate::notifications::spawn_team_general;
use crate::server_ops::{Srv, shell_arg};

/// `ServerTransferClaimer::OWNER_MARKER_DIR`.
pub const OWNER_MARKER_DIR: &str = "/data/coolify/transfer";
/// `ServerTransferClaimer::OWNER_MARKER_PATH`.
pub const OWNER_MARKER_PATH: &str = "/data/coolify/transfer/owner.json";

/// `ServerTransferBundle::SCHEMA_VERSION`.
pub const SCHEMA_VERSION: i64 = 1;

/// `now()->toIso8601String()`.
pub fn now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

fn db_error(e: sqlx::Error) -> String {
    tracing::warn!(error = %e, "server transfer query failed");
    "A database error occurred.".to_owned()
}

/// The row, locked: `Server::query()->lockForUpdate()->findOrFail($id)->server_metadata`.
pub async fn locked_metadata(conn: &mut PgConnection, id: i64) -> Result<Map<String, Value>, String> {
    let meta: Option<Value> =
        sqlx::query_scalar("SELECT server_metadata FROM servers WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(db_error)?
            .ok_or_else(|| "No query results for model [App\\Models\\Server].".to_owned())?;
    Ok(match meta {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    })
}

/// `$server->server_metadata = $metadata; $server->save();`
pub async fn write_metadata(
    conn: &mut PgConnection,
    id: i64,
    metadata: &Map<String, Value>,
) -> Result<(), String> {
    sqlx::query("UPDATE servers SET server_metadata = $2::text::json, updated_at = $3 WHERE id = $1")
        .bind(id)
        .bind(Value::Object(metadata.clone()).to_string())
        .bind(Utc::now().naive_utc())
        .execute(&mut *conn)
        .await
        .map_err(db_error)?;
    Ok(())
}

/// `ServerSetting::generateSentinelToken()`: `encrypted` cast over `encrypt(json)`.
fn sentinel_token(jobs: &Jobs, server_uuid: &str) -> Result<String, String> {
    let enc = jobs
        .encrypter()
        .ok_or_else(|| "APP_KEY is not configured.".to_owned())?;
    Ok(enc.encrypt_string(&enc.encrypt_serialized(&json!({"server_uuid": server_uuid}).to_string())))
}

/// `ServerTransferClaimer::claim`.
pub async fn claim(jobs: &Jobs, server_id: i64, instance_url: &str) -> Result<Value, String> {
    if server_id == 0 {
        return Err("Cannot claim the Coolify host itself.".into());
    }
    let instance_url = instance_url.trim_end_matches('/');
    if instance_url.is_empty() {
        return Err(
            "Instance URL (FQDN or APP_URL) must be set before claiming a server.".to_owned(),
        );
    }
    let srv = Srv::load(jobs, server_id).await?;
    let server_uuid = srv.row.uuid.clone();
    let team_id = srv.info.team_id;
    let mut tx = srv.db().pool().begin().await.map_err(db_error)?;
    let mut metadata = locked_metadata(&mut tx, server_id).await?;
    let export_id = metadata
        .get("transfer")
        .and_then(|t| t.get("export_id"))
        .cloned()
        .unwrap_or(Value::Null);
    let claim = json!({
        "claim_id": new_public_id(),
        "instance_url": instance_url,
        "server_uuid": server_uuid,
        "team_id": team_id,
        "claimed_at": now_iso(),
        "export_id": export_id,
        "schema_version": SCHEMA_VERSION,
    });

    // A new token: the instance that managed the server before keeps a copy of the old one.
    sqlx::query(
        "UPDATE server_settings SET sentinel_custom_url = $2, sentinel_token = $3, is_sentinel_enabled = true, \
         force_disabled = false, updated_at = $4 WHERE server_id = $1",
    )
    .bind(server_id)
    .bind(instance_url)
    .bind(sentinel_token(jobs, &server_uuid)?)
    .bind(Utc::now().naive_utc())
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;

    let transfer = metadata
        .entry("transfer")
        .or_insert_with(|| Value::Object(Map::new()));
    if !transfer.is_object() {
        *transfer = Value::Object(Map::new());
    }
    let t = transfer.as_object_mut().expect("object");
    t.insert("status".into(), json!("claimed"));
    t.insert("claimed_at".into(), claim["claimed_at"].clone());
    t.insert("claim".into(), claim.clone());
    t.remove("owner_marker_conflict");
    t.remove("claimed_by_marker");
    write_metadata(&mut tx, server_id, &metadata).await?;

    write_owner_marker(&srv, &claim).await?;
    tx.commit().await.map_err(db_error)?;

    Ok(json!({
        "server_uuid": server_uuid,
        "claim": claim,
        "message": "This Coolify instance now manages the server.",
    }))
}

/// `writeOwnerMarker`.
async fn write_owner_marker(srv: &Srv, claim: &Value) -> Result<(), String> {
    let marker = serde_json::to_string(&json!({
        "claim_id": claim["claim_id"],
        "instance_url": claim["instance_url"],
        "server_uuid": claim["server_uuid"],
        "claimed_at": claim["claimed_at"],
    }))
    .map_err(|e| e.to_string())?;
    srv.run(
        &[format!("mkdir -p {OWNER_MARKER_DIR}")],
        "server.transfer.claim",
    )
    .await?;
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(marker);
    srv.run(
        &[format!(
            "echo '{encoded}' | base64 -d | tee {} > /dev/null",
            shell_arg(OWNER_MARKER_PATH)
        )],
        "server.transfer.claim",
    )
    .await?;
    Ok(())
}

/// The morph keys of `dnsResources`: the server, its primary applications with their previews,
/// and the applications of its services (`resourceKeys`).
async fn dns_reference_ids(conn: &mut PgConnection, server_id: i64) -> Result<Vec<i64>, String> {
    sqlx::query_scalar::<_, i64>(
        "SELECT DISTINCT r.managed_dns_record_id FROM managed_dns_record_references r WHERE \
           (r.resource_type = 'App\\Models\\Server' AND r.resource_id = $1) \
        OR (r.resource_type = 'App\\Models\\Application' AND r.resource_id IN (SELECT id FROM applications a WHERE \
             (a.destination_type = 'App\\Models\\StandaloneDocker' AND a.destination_id IN (SELECT id FROM standalone_dockers WHERE server_id = $1)) \
          OR (a.destination_type = 'App\\Models\\SwarmDocker' AND a.destination_id IN (SELECT id FROM swarm_dockers WHERE server_id = $1)))) \
        OR (r.resource_type = 'App\\Models\\ApplicationPreview' AND r.resource_id IN (SELECT p.id FROM application_previews p JOIN applications a ON a.id = p.application_id WHERE \
             (a.destination_type = 'App\\Models\\StandaloneDocker' AND a.destination_id IN (SELECT id FROM standalone_dockers WHERE server_id = $1)) \
          OR (a.destination_type = 'App\\Models\\SwarmDocker' AND a.destination_id IN (SELECT id FROM swarm_dockers WHERE server_id = $1)))) \
        OR (r.resource_type = 'App\\Models\\ServiceApplication' AND r.resource_id IN (SELECT sa.id FROM service_applications sa JOIN services s ON s.id = sa.service_id WHERE s.server_id = $1))",
    )
    .bind(server_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(db_error)
}

const REFERENCE_MATCH: &str = "((resource_type = 'App\\Models\\Server' AND resource_id = $2) \
    OR (resource_type = 'App\\Models\\Application' AND resource_id IN (SELECT id FROM applications a WHERE \
         (a.destination_type = 'App\\Models\\StandaloneDocker' AND a.destination_id IN (SELECT id FROM standalone_dockers WHERE server_id = $2)) \
      OR (a.destination_type = 'App\\Models\\SwarmDocker' AND a.destination_id IN (SELECT id FROM swarm_dockers WHERE server_id = $2)))) \
    OR (resource_type = 'App\\Models\\ApplicationPreview' AND resource_id IN (SELECT p.id FROM application_previews p JOIN applications a ON a.id = p.application_id WHERE \
         (a.destination_type = 'App\\Models\\StandaloneDocker' AND a.destination_id IN (SELECT id FROM standalone_dockers WHERE server_id = $2)) \
      OR (a.destination_type = 'App\\Models\\SwarmDocker' AND a.destination_id IN (SELECT id FROM swarm_dockers WHERE server_id = $2)))) \
    OR (resource_type = 'App\\Models\\ServiceApplication' AND resource_id IN (SELECT sa.id FROM service_applications sa JOIN services s ON s.id = sa.service_id WHERE s.server_id = $2)))";

/// `ManagedDnsRecordCleanup::parkResources`.
async fn park_dns(conn: &mut PgConnection, server_id: i64) -> Result<Vec<i64>, String> {
    let mut parked = Vec::new();
    for record_id in dns_reference_ids(conn, server_id).await? {
        let shared: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT 1 FROM managed_dns_record_references WHERE managed_dns_record_id = $1 AND NOT {REFERENCE_MATCH})"
        ))
        .bind(record_id)
        .bind(server_id)
        .fetch_one(&mut *conn)
        .await
        .map_err(db_error)?;
        if shared {
            sqlx::query(&format!(
                "DELETE FROM managed_dns_record_references WHERE managed_dns_record_id = $1 AND {REFERENCE_MATCH}"
            ))
            .bind(record_id)
            .bind(server_id)
            .execute(&mut *conn)
            .await
            .map_err(db_error)?;
            continue;
        }
        let changed = sqlx::query(
            "UPDATE managed_dns_records SET owned = false, updated_at = $2 WHERE id = $1 AND owned = true",
        )
        .bind(record_id)
        .bind(Utc::now().naive_utc())
        .execute(&mut *conn)
        .await
        .map_err(db_error)?;
        if changed.rows_affected() > 0 {
            parked.push(record_id);
        }
    }
    Ok(parked)
}

/// `ManagedDnsRecordCleanup::forgetResources`: the references go, and a record without references
/// is removed here without a provider call.
pub async fn forget_dns(conn: &mut PgConnection, server_id: i64) -> Result<(), String> {
    for record_id in dns_reference_ids(conn, server_id).await? {
        sqlx::query(&format!(
            "DELETE FROM managed_dns_record_references WHERE managed_dns_record_id = $1 AND {REFERENCE_MATCH}"
        ))
        .bind(record_id)
        .bind(server_id)
        .execute(&mut *conn)
        .await
        .map_err(db_error)?;
        sqlx::query(
            "DELETE FROM managed_dns_records WHERE id = $1 AND NOT EXISTS (SELECT 1 FROM managed_dns_record_references WHERE managed_dns_record_id = $1)",
        )
        .bind(record_id)
        .execute(&mut *conn)
        .await
        .map_err(db_error)?;
    }
    Ok(())
}

/// `ServerTransferClaimer::markTransferred`.
pub async fn mark_transferred(
    jobs: &Jobs,
    server_id: i64,
    export_id: Option<&str>,
    target_instance_url: Option<&str>,
    from_owner_marker: bool,
) -> Result<Value, String> {
    if server_id == 0 {
        return Err("Cannot transfer the Coolify host itself.".into());
    }
    let srv = Srv::load(jobs, server_id).await?;
    let uuid = srv.row.uuid.clone();
    let now = Utc::now().naive_utc();
    let mut tx = srv.db().pool().begin().await.map_err(db_error)?;
    let mut metadata = locked_metadata(&mut tx, server_id).await?;
    sqlx::query(
        "UPDATE server_settings SET force_disabled = true, is_sentinel_enabled = false, updated_at = $2 WHERE server_id = $1",
    )
    .bind(server_id)
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;

    let previous_export = metadata
        .get("transfer")
        .and_then(|t| t.get("export_id"))
        .cloned()
        .unwrap_or(Value::Null);
    let transfer = metadata
        .entry("transfer")
        .or_insert_with(|| Value::Object(Map::new()));
    if !transfer.is_object() {
        *transfer = Value::Object(Map::new());
    }
    let t = transfer.as_object_mut().expect("object");
    t.insert("status".into(), json!("transferred"));
    t.insert(
        "export_id".into(),
        export_id.map(|s| json!(s)).unwrap_or(previous_export),
    );
    t.insert("target_instance_url".into(), json!(target_instance_url));
    t.insert("claimed_by_marker".into(), json!(from_owner_marker));
    t.insert("transferred_at".into(), json!(now_iso()));
    t.remove("finish");
    t.remove("taken_back_at");
    t.remove("owner_marker_conflict");

    // `releaseGithubRunners`.
    sqlx::query("UPDATE github_runner_configs SET is_enabled = false, updated_at = $2 WHERE server_id = $1")
        .bind(server_id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    sqlx::query(
        "UPDATE github_runner_executions SET status = 'cancelled', error_message = $2, \
         completed_at = COALESCE(completed_at, $3), updated_at = $3 \
         WHERE server_id = $1 AND status IN ('queued', 'provisioning', 'idle', 'running')",
    )
    .bind(server_id)
    .bind("The server was transferred to another Coolify instance. This instance stopped managing its runners.")
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;

    let parked = park_dns(&mut tx, server_id).await?;
    metadata
        .get_mut("transfer")
        .and_then(Value::as_object_mut)
        .expect("object")
        .insert("parked_dns_record_ids".into(), json!(parked));
    write_metadata(&mut tx, server_id, &metadata).await?;
    tx.commit().await.map_err(db_error)?;

    Ok(json!({
        "server_uuid": uuid,
        "message": "Server marked as transferred. Automations disabled on this instance.",
    }))
}

/// `isNewerThanOwnClaim`.
fn newer_than(marker: Option<&Value>, own: Option<&Value>) -> bool {
    let parse = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
    };
    matches!((parse(marker), parse(own)), (Some(m), Some(o)) if m > o)
}

/// `recordOwnerMarkerConflict`.
async fn record_conflict(
    srv: &Srv,
    server_id: i64,
    marker_claim_id: Option<&str>,
    instance_url: Option<&Value>,
) -> Result<(), String> {
    let mut tx = srv.db().pool().begin().await.map_err(db_error)?;
    let mut metadata = locked_metadata(&mut tx, server_id).await?;
    let transfer = metadata
        .entry("transfer")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(t) = transfer.as_object_mut() {
        t.insert(
            "owner_marker_conflict".into(),
            match marker_claim_id {
                None => Value::Null,
                Some(id) => json!({
                    "claim_id": id,
                    "instance_url": instance_url
                        .and_then(Value::as_str)
                        .map(|u| u.chars().take(255).collect::<String>()),
                    "seen_at": now_iso(),
                }),
            },
        );
    }
    write_metadata(&mut tx, server_id, &metadata).await?;
    tx.commit().await.map_err(db_error)
}

/// `ServerTransferClaimer::releaseIfClaimedElsewhere`. True when this instance released the server.
pub async fn release_if_claimed_elsewhere(
    jobs: &Jobs,
    srv: &Srv,
    marker_json: Option<&str>,
) -> Result<bool, String> {
    let server_id = srv.id();
    let metadata = srv.row.record.get("server_metadata").cloned();
    let transfer = metadata.as_ref().and_then(|m| m.get("transfer"));
    if server_id == 0
        || transfer.and_then(|t| t.get("status")).and_then(Value::as_str) != Some("claimed")
    {
        return Ok(false);
    }
    let Ok(marker) = serde_json::from_str::<Value>(marker_json.unwrap_or_default().trim()) else {
        return Ok(false);
    };
    let Some(marker_claim_id) = marker
        .get("claim_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return Ok(false);
    };
    let transfer = transfer.expect("checked above");
    let own_claim = transfer.get("claim");
    if Some(marker_claim_id)
        == own_claim
            .and_then(|c| c.get("claim_id"))
            .and_then(Value::as_str)
    {
        if transfer
            .get("owner_marker_conflict")
            .is_some_and(|c| !c.is_null())
        {
            record_conflict(srv, server_id, None, None).await?;
        }
        return Ok(false);
    }
    // Any http(s) URL: the URL is not trusted, so it is never prefilled for a transfer back.
    let owner = marker
        .get("instance_url")
        .and_then(Value::as_str)
        .filter(|u| {
            let l = u.to_ascii_lowercase();
            (l.starts_with("http://") || l.starts_with("https://")) && u.len() > 8 && !u.contains(' ')
        })
        .map(|u| u.trim().trim_end_matches('/').to_owned());
    let newer = newer_than(
        marker.get("claimed_at"),
        own_claim.and_then(|c| c.get("claimed_at")),
    );
    let Some(owner) = owner.filter(|_| newer) else {
        let seen = transfer
            .get("owner_marker_conflict")
            .and_then(|c| c.get("claim_id"))
            .and_then(Value::as_str);
        if seen != Some(marker_claim_id) {
            tracing::warn!(
                server = srv.row.uuid,
                "The owner marker on the server names another claim, but it is not trusted; this instance keeps managing the server"
            );
            record_conflict(
                srv,
                server_id,
                Some(marker_claim_id),
                marker.get("instance_url"),
            )
            .await?;
        }
        return Ok(false);
    };

    mark_transferred(jobs, server_id, None, Some(&owner), true).await?;
    tracing::warn!(
        server = srv.row.uuid,
        owner = %owner,
        "Server is claimed by another Coolify instance; management stopped here"
    );
    let row = sqlx::query("SELECT name, team_id FROM servers WHERE id = $1")
        .bind(server_id)
        .fetch_one(srv.db().pool())
        .await
        .map_err(db_error)?;
    let name: String = row.try_get("name").unwrap_or_default();
    let team_id: i64 = row.try_get("team_id").unwrap_or_default();
    spawn_team_general(
        srv.db(),
        jobs.encrypter(),
        team_id,
        format!("Server '{name}' is now managed by {owner}. This instance stopped managing it."),
        false,
    );
    Ok(true)
}
