//! `ServerTransferMigrator` and `MigrateServerToInstanceJob`: prechecks, then export, import on the
//! target, and complete here.

use std::time::Duration;

use axum::http::HeaderMap;
use coolify_jobs::server_transfer::{locked_metadata, mark_transferred, write_metadata};
use serde_json::{Map, Value, json};

use super::exporter::Exporter;
use super::{dg, is_dev, is_local_development_target, now_iso, text};
use crate::state::AppState;

/// `IMPORT_TIMEOUT_SECONDS`.
const IMPORT_TIMEOUT: Duration = Duration::from_secs(300);
/// `STATUS_PROBE_ATTEMPTS`.
const STATUS_PROBE_ATTEMPTS: u32 = 20;
/// `STATUS_PROBE_INTERVAL_SECONDS`.
const STATUS_PROBE_INTERVAL: Duration = Duration::from_secs(15);
/// `STATUS_REQUEST_TIMEOUT_SECONDS`.
const STATUS_TIMEOUT: Duration = Duration::from_secs(15);
/// `CHECK_TIMEOUT_SECONDS`.
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
/// `STALE_MIGRATION_SECONDS`.
const STALE_MIGRATION_SECONDS: i64 = 1800;
/// `MAX_LOG_ENTRIES`.
const MAX_LOG_ENTRIES: usize = 100;

fn db_error(e: sqlx::Error) -> String {
    tracing::warn!(error = %e, "server transfer query failed");
    "A database error occurred.".to_owned()
}

fn log_entry(message: &str, level: &str) -> Value {
    json!({"at": now_iso(), "level": level, "message": message})
}

/// `check()`.
fn check_row(label: &str, ok: Option<bool>, message: Option<String>) -> Value {
    json!({"label": label, "ok": ok, "message": message, "at": now_iso()})
}

/// `remove_iip`: strips internal instance addresses from a message. No-op here.
fn remove_iip(text: &str) -> String {
    text.to_owned()
}

/// `validateInstanceUrl`.
pub async fn validate_instance_url(state: &AppState, url: &str) -> Result<String, InstanceUrlError> {
    let url = url.trim().trim_end_matches('/').to_owned();
    if url.is_empty() || !crate::laravel_url::is_url(&url) {
        return Err(InstanceUrlError::Message(
            "A valid target instance URL is required (e.g. https://coolify.example.com).".into(),
        ));
    }
    if is_local_development_target(&url) {
        return Ok(url);
    }
    if !url.to_ascii_lowercase().starts_with("https://") {
        return Err(InstanceUrlError::Message(
            "The target instance URL must use https, because the transfer sends credentials.".into(),
        ));
    }
    match crate::safe_url::check(state, "target_url", &url, &HeaderMap::new()).await {
        Ok(None) => Ok(url),
        Ok(Some(m)) => Err(InstanceUrlError::Validation(m)),
        Err(_) => Err(InstanceUrlError::Message("The target URL could not be checked.".into())),
    }
}

#[derive(Debug)]
pub enum InstanceUrlError {
    Message(String),
    Validation(String),
}

impl InstanceUrlError {
    pub fn text(&self) -> String {
        match self {
            Self::Message(m) | Self::Validation(m) => m.clone(),
        }
    }
}

/// `isLinkableUrl`.
pub fn is_linkable_url(url: Option<&Value>) -> bool {
    let Some(u) = url.and_then(Value::as_str) else {
        return false;
    };
    if !crate::laravel_url::is_url(u) {
        return false;
    }
    let lower = u.to_ascii_lowercase();
    lower.starts_with("https://") || (lower.starts_with("http://") && is_dev())
}

/// `normalizeToken`.
fn normalize_token(token: &str) -> Result<String, String> {
    let mut t = token.trim();
    if t.len() >= 7 && t[..7].eq_ignore_ascii_case("bearer ") {
        t = t[7..].trim();
    }
    if t.is_empty() {
        return Err("A target instance API token is required (root or write + deploy).".into());
    }
    Ok(t.to_owned())
}

/// `targetRequest`.
fn client(url: &str, timeout: Duration) -> Result<reqwest::Client, String> {
    let mut b = reqwest::Client::builder().timeout(timeout);
    // A local target answers directly; a public one is checked by SafeWebhookUrl and not followed.
    if is_local_development_target(url) {
        b = b.redirect(reqwest::redirect::Policy::none());
    } else {
        b = b.redirect(reqwest::redirect::Policy::none());
    }
    b.build().map_err(|e| e.to_string())
}

/// `writeMigration` on the locked row.
async fn write_migration(state: &AppState, server_id: i64, change: impl FnOnce(&mut Map<String, Value>)) -> Result<(), String> {
    let mut tx = state.db.pool().begin().await.map_err(db_error)?;
    let mut metadata = locked_metadata(&mut tx, server_id).await?;
    let mut migration = metadata
        .get("migration")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    change(&mut migration);
    metadata.insert("migration".into(), Value::Object(migration));
    write_metadata(&mut tx, server_id, &metadata).await?;
    tx.commit().await.map_err(db_error)
}

fn append_log(migration: &mut Map<String, Value>, entry: Value) {
    let mut log = migration
        .get("log")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    log.push(entry);
    if log.len() > MAX_LOG_ENTRIES {
        log.drain(..log.len() - MAX_LOG_ENTRIES);
    }
    migration.insert("log".into(), json!(log));
}

/// `log`.
async fn log(state: &AppState, server_id: i64, message: &str, level: &str, fields: Value) {
    let entry = log_entry(message, level);
    let _ = write_migration(state, server_id, move |m| {
        if let Value::Object(f) = fields {
            m.extend(f);
        }
        append_log(m, entry);
    })
    .await;
}

/// `recordMigration`.
pub async fn record_migration(state: &AppState, server_id: i64, status: &str, details: Value) {
    let error = dg(&details, "error").map_or_else(|| "unknown error".to_owned(), text);
    let entry = match status {
        "succeeded" => log_entry("Transfer finished. The target instance manages the server now.", "success"),
        "unknown" => log_entry(&format!("Transfer result unknown: {error}"), "warning"),
        _ => log_entry(&format!("Transfer failed: {error}"), "error"),
    };
    let status = status.to_owned();
    let _ = write_migration(state, server_id, move |m| {
        if let Value::Object(f) = details {
            m.extend(f);
        }
        m.insert("status".into(), json!(status));
        m.insert("finished_at".into(), json!(now_iso()));
        append_log(m, entry);
    })
    .await;
}

/// `hasRunningMigration`.
fn has_running(metadata: &Value) -> bool {
    dg(metadata, "migration.status").and_then(Value::as_str) == Some("running")
        && dg(metadata, "migration.started_at")
            .and_then(Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .is_some_and(|t| (chrono::Utc::now() - t.to_utc()).num_seconds() < STALE_MIGRATION_SECONDS)
}

async fn metadata_of(state: &AppState, server_id: i64) -> Result<Value, String> {
    let row: Option<(Option<Value>,)> = sqlx::query_as("SELECT server_metadata FROM servers WHERE id = $1")
        .bind(server_id)
        .fetch_optional(state.db.pool())
        .await
        .map_err(db_error)?;
    Ok(row.and_then(|r| r.0).unwrap_or(Value::Null))
}

/// `migrationChecks`.
async fn migration_checks(state: &AppState, server_id: i64, metadata: &Value) -> Result<Vec<Value>, String> {
    let pool = state.db.pool();
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM application_deployment_queues WHERE server_id = $1 AND status IN ('queued', 'in_progress')",
    )
    .bind(server_id)
    .fetch_one(pool)
    .await
    .map_err(db_error)?;
    let transferred = dg(metadata, "transfer.status").and_then(Value::as_str) == Some("transferred");
    let mut checks = vec![
        check_row("the server is not the Coolify host", Some(server_id != 0), Some("The Coolify host (localhost) cannot be transferred between instances.".into())),
        check_row("this instance manages the server", Some(!transferred), Some("This server was already transferred to another instance.".into())),
        check_row(
            "no deployment is queued or running on the server",
            Some(active == 0),
            Some(format!("{active} deployment(s) on this server are queued or running. Wait until they finish or cancel them, then transfer the server.")),
        ),
    ];
    // `runningJobs`: the schedule of each run must be on this server.
    let on_server = "((x.destination_type = 'App\\Models\\StandaloneDocker' AND x.destination_id IN (SELECT id FROM standalone_dockers WHERE server_id = $1)) OR (x.destination_type = 'App\\Models\\SwarmDocker' AND x.destination_id IN (SELECT id FROM swarm_dockers WHERE server_id = $1)))";
    let db_backup: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM scheduled_database_backup_executions e JOIN scheduled_database_backups b ON b.id = e.scheduled_database_backup_id \
         WHERE e.status = 'running' AND e.created_at > now() - interval '1 day' AND EXISTS ( \
           SELECT 1 FROM service_databases sd JOIN services s ON s.id = sd.service_id WHERE b.database_type = 'App\\Models\\ServiceDatabase' AND sd.id = b.database_id AND s.server_id = $1)",
    )
    .bind(server_id)
    .fetch_one(pool)
    .await
    .map_err(db_error)?;
    let mut db_total = db_backup;
    for (table, morph) in [
        ("standalone_postgresqls", "StandalonePostgresql"), ("standalone_redis", "StandaloneRedis"), ("standalone_mongodbs", "StandaloneMongodb"),
        ("standalone_mysqls", "StandaloneMysql"), ("standalone_mariadbs", "StandaloneMariadb"), ("standalone_keydbs", "StandaloneKeydb"),
        ("standalone_dragonflies", "StandaloneDragonfly"), ("standalone_clickhouses", "StandaloneClickhouse"), ("standalone_sqlites", "StandaloneSqlite"),
    ] {
        let n: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM scheduled_database_backup_executions e JOIN scheduled_database_backups b ON b.id = e.scheduled_database_backup_id \
             JOIN {table} x ON b.database_type = 'App\\Models\\{morph}' AND x.id = b.database_id \
             WHERE e.status = 'running' AND e.created_at > now() - interval '1 day' AND {on_server}"
        ))
        .bind(server_id)
        .fetch_one(pool)
        .await
        .map_err(db_error)?;
        db_total += n;
    }
    let tasks: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM scheduled_task_executions e JOIN scheduled_tasks t ON t.id = e.scheduled_task_id \
         WHERE e.status = 'running' AND e.created_at > now() - interval '1 day' AND ( \
           t.service_id IN (SELECT id FROM services WHERE server_id = $1) OR t.application_id IN (SELECT x.id FROM applications x WHERE {on_server}))"
    ))
    .bind(server_id)
    .fetch_one(pool)
    .await
    .map_err(db_error)?;
    let volume: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM scheduled_volume_backup_executions e JOIN scheduled_volume_backups b ON b.id = e.scheduled_volume_backup_id \
         JOIN local_persistent_volumes v ON b.backupable_type = 'App\\Models\\LocalPersistentVolume' AND v.id = b.backupable_id \
         WHERE e.status = 'running' AND e.created_at > now() - interval '1 day' AND ( \
           (v.resource_type = 'App\\Models\\Application' AND v.resource_id IN (SELECT x.id FROM applications x WHERE {on_server})) \
           OR (v.resource_type = 'App\\Models\\ServiceApplication' AND v.resource_id IN (SELECT sa.id FROM service_applications sa JOIN services s ON s.id = sa.service_id WHERE s.server_id = $1)) \
           OR (v.resource_type = 'App\\Models\\ServiceDatabase' AND v.resource_id IN (SELECT sd.id FROM service_databases sd JOIN services s ON s.id = sd.service_id WHERE s.server_id = $1)))"
    ))
    .bind(server_id)
    .fetch_one(pool)
    .await
    .map_err(db_error)?;
    for (label, count) in [("database backup", db_total), ("volume backup", volume), ("scheduled task", tasks)] {
        checks.push(check_row(
            &format!("no {label} is running on the server"),
            Some(count == 0),
            Some(format!("{count} {label}(s) on this server are running. Wait until they finish, then transfer the server.")),
        ));
    }
    Ok(checks)
}

/// `systemSources`: uuid, name and applications per kind.
async fn system_sources(state: &AppState, server_id: i64) -> Result<Vec<(String, String, String, Vec<String>)>, String> {
    let mut out: Vec<(String, String, String, Vec<String>)> = Vec::new();
    for (kind, table, morph) in [("github", "github_apps", "App\\Models\\GithubApp"), ("gitlab", "gitlab_apps", "App\\Models\\GitlabApp")] {
        let rows: Vec<(String, String, String)> = sqlx::query_as(&format!(
            "SELECT g.uuid, g.name, a.name FROM applications a JOIN {table} g ON g.id = a.source_id \
             WHERE a.source_type = $2 AND g.is_system_wide = true AND a.deleted_at IS NULL AND \
             ((a.destination_type = 'App\\Models\\StandaloneDocker' AND a.destination_id IN (SELECT id FROM standalone_dockers WHERE server_id = $1)) \
              OR (a.destination_type = 'App\\Models\\SwarmDocker' AND a.destination_id IN (SELECT id FROM swarm_dockers WHERE server_id = $1)))"
        ))
        .bind(server_id)
        .bind(morph)
        .fetch_all(state.db.pool())
        .await
        .map_err(db_error)?;
        for (uuid, name, app) in rows {
            if let Some(e) = out.iter_mut().find(|e| e.0 == kind && e.1 == uuid) {
                e.3.push(app);
            } else {
                out.push((kind.to_owned(), uuid, name, vec![app]));
            }
        }
    }
    Ok(out)
}

/// `sharedProjectNotes`.
async fn shared_project_notes(state: &AppState, server_id: i64) -> Result<Vec<String>, String> {
    let dest = "((x.destination_type = 'App\\Models\\StandaloneDocker' AND x.destination_id IN (SELECT id FROM standalone_dockers WHERE server_id = $1)) OR (x.destination_type = 'App\\Models\\SwarmDocker' AND x.destination_id IN (SELECT id FROM swarm_dockers WHERE server_id = $1)))";
    let tables = ["standalone_postgresqls", "standalone_redis", "standalone_mongodbs", "standalone_mysqls", "standalone_mariadbs", "standalone_keydbs", "standalone_dragonflies", "standalone_clickhouses", "standalone_sqlites", "applications"];
    let mut on_server = vec!["SELECT environment_id FROM services WHERE server_id = $1 AND deleted_at IS NULL".to_owned()];
    let mut other = vec!["SELECT environment_id FROM services x WHERE x.deleted_at IS NULL AND x.server_id IS DISTINCT FROM $1".to_owned()];
    for t in tables {
        on_server.push(format!("SELECT x.environment_id FROM {t} x WHERE x.deleted_at IS NULL AND {dest}"));
        other.push(format!("SELECT x.environment_id FROM {t} x WHERE x.deleted_at IS NULL AND NOT {dest}"));
    }
    let sql = format!(
        "WITH here AS (SELECT DISTINCT e.project_id FROM environments e WHERE e.id IN ({})), \
         others AS (SELECT e.project_id FROM ({}) r JOIN environments e ON e.id = r.environment_id) \
         SELECT p.name, count(*) FROM others o JOIN projects p ON p.id = o.project_id WHERE o.project_id IN (SELECT project_id FROM here) GROUP BY p.name ORDER BY p.name",
        on_server.join(" UNION ALL "),
        other.join(" UNION ALL "),
    );
    let rows: Vec<(String, i64)> = sqlx::query_as(&sql)
        .bind(server_id)
        .fetch_all(state.db.pool())
        .await
        .map_err(db_error)?;
    Ok(rows
        .into_iter()
        .map(|(name, n)| {
            format!(
                "NOTE: project '{name}' also has {n} {} on other servers. They stay on this instance; only the resources of this server are transferred.",
                if n == 1 { "resource" } else { "resources" }
            )
        })
        .collect())
}

/// `targetChecks`, and the system sources the target lacks.
async fn target_checks(
    target: &str,
    token: &str,
    server: (&str, &str, i64),
    sources: &[(String, String, String, Vec<String>)],
    version: &str,
) -> (Vec<Value>, Vec<String>) {
    let url = format!("{target}/api/v1/servers/import/check");
    let reach = format!("the target instance at {} can be reached", remove_iip(target));
    let not_checked = |label: &str| check_row("the checks on the target instance", None, Some(format!("They did not run, because \"{label}\" failed.")));
    let mut query: Vec<(String, String)> = vec![
        ("server_uuid".into(), server.0.to_owned()),
        ("ip".into(), server.1.to_owned()),
        ("port".into(), server.2.to_string()),
        ("schema_version".into(), super::bundle::SCHEMA_VERSION.to_string()),
    ];
    for (kind, uuid, _, _) in sources {
        query.push((format!("system_{kind}_apps[]"), uuid.clone()));
    }
    let response = match client(&url, CHECK_TIMEOUT) {
        Ok(c) => c.get(&url).bearer_auth(token).header("Accept", "application/json").query(&query).send().await,
        Err(e) => return (vec![check_row(&reach, Some(false), Some(e)), not_checked(&reach)], Vec::new()),
    };
    let response = match response {
        Ok(r) => r,
        Err(e) => {
            return (
                vec![
                    check_row(&reach, Some(false), Some(format!("This instance cannot reach the target instance: {}", remove_iip(&e.to_string())))),
                    not_checked(&reach),
                ],
                Vec::new(),
            );
        }
    };
    let mut checks = vec![check_row(&reach, Some(true), None)];
    let status = response.status().as_u16();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    let message = dg(&body, "message").filter(|m| crate::serialize::php_truthy(m)).map_or_else(|| "no details".to_owned(), text);
    let gates: [(u16, &str, String); 3] = [
        (404, "the target instance supports server transfer", format!("The target instance does not support server transfer. Upgrade it to Coolify {version} or newer.")),
        (401, "the target instance accepts the API token", "The target instance does not accept the API token. Create a new token on the target instance and paste it here.".to_owned()),
        (403, "the API token can create servers on the target instance", format!("Use a token with the root permission, or the write and deploy permissions, from an admin or owner. Target response: {message}")),
    ];
    for (fail, label, fail_message) in gates {
        if status == fail {
            checks.push(check_row(label, Some(false), Some(fail_message)));
            checks.push(not_checked(label));
            return (checks, Vec::new());
        }
        checks.push(check_row(label, Some(true), None));
    }
    let mut missing = Vec::new();
    for kind in ["github", "gitlab"] {
        if let Some(Value::Array(a)) = dg(&body, &format!("missing_system_sources.{kind}")) {
            missing.extend(a.iter().filter_map(Value::as_str).map(|s| format!("{kind}:{s}")));
        }
    }
    let remote = dg(&body, "checks").and_then(Value::as_array);
    let Some(remote) = remote.filter(|_| status == 200 || status == 422) else {
        checks.push(check_row("the target instance answered the checks", Some(false), Some(format!("The target instance answered with HTTP {status}: {message}"))));
        return (checks, missing);
    };
    for r in remote {
        checks.push(check_row(
            &format!("on the target: {}", dg(r, "label").map_or_else(|| "unknown check".to_owned(), text)),
            Some(crate::serialize::php_truthy(dg(r, "ok").unwrap_or(&Value::Null))),
            dg(r, "message").and_then(Value::as_str).map(remove_iip),
        ));
    }
    (checks, missing)
}

/// `start`: the prechecks now, so the user sees problems at once, then the job.
pub async fn start(state: &AppState, server_id: i64, uuid: &str, ip: &str, port: i64, target_url: &str, target_token: &str) -> Result<(), String> {
    let target = validate_instance_url(state, target_url).await.map_err(|e| e.text())?;
    let token = normalize_token(target_token)?;
    let metadata = metadata_of(state, server_id).await?;
    if has_running(&metadata) {
        return Err("A transfer of this server is already running.".into());
    }
    let started_at = now_iso();
    let mut log_lines = vec![log_entry(&format!("Running checks before the transfer to {target}."), "info")];
    let sources = system_sources(state, server_id).await?;
    let mut checks = migration_checks(state, server_id, &metadata).await?;
    checks.push(ssh_check(state, server_id).await);
    let (remote, missing) = target_checks(&target, &token, (uuid, ip, port), &sources, &state.version).await;
    checks.extend(remote);
    for c in &checks {
        let label = text(&c["label"]);
        let msg = text(&c["message"]);
        log_lines.push(match c["ok"].as_bool() {
            Some(true) => json!({"at": c["at"], "level": "success", "message": format!("OK: {label}.")}),
            Some(false) => json!({"at": c["at"], "level": "error", "message": format!("FAILED: {label}. {msg}")}),
            None => json!({"at": c["at"], "level": "warning", "message": format!("NOT CHECKED: {label}. {msg}")}),
        });
    }
    for note in shared_project_notes(state, server_id).await? {
        log_lines.push(log_entry(&note, "info"));
    }
    for (kind, source_uuid, name, apps) in &sources {
        if missing.contains(&format!("{kind}:{source_uuid}")) {
            let label = if kind == "github" { "GitHub App" } else { "GitLab App" };
            log_lines.push(log_entry(
                &format!("NOTE: the system-wide {label} '{name}' of this instance is not transferred, and the target has no matching one. These applications need a new git source on the target: {}. The finish list on the target shows them.", apps.join(", ")),
                "info",
            ));
        }
    }
    let failed: Vec<&Value> = checks.iter().filter(|c| c["ok"] == json!(false)).collect();
    if !failed.is_empty() {
        let messages = failed.iter().map(|c| text(&c["message"])).collect::<Vec<_>>().join(" ");
        let error = format!("{} of {} checks failed: {messages}", failed.len(), checks.len());
        log_lines.push(log_entry(&format!("The transfer did not start, because {} check(s) failed. Fix them, then transfer again.", failed.len()), "error"));
        let (t, e, s) = (target.clone(), error.clone(), started_at.clone());
        write_migration(state, server_id, move |m| {
            *m = serde_json::from_value(json!({
                "status": "failed", "phase": "checks", "target_url": t, "started_at": s,
                "finished_at": now_iso(), "error": e, "log": log_lines,
            }))
            .unwrap_or_default();
        })
        .await?;
        return Err(error);
    }
    log_lines.push(log_entry("All checks passed. The transfer is queued.", "info"));
    let current = metadata_of(state, server_id).await?;
    if has_running(&current) {
        return Err("A transfer of this server is already running.".into());
    }
    let t = target.clone();
    write_migration(state, server_id, move |m| {
        *m = serde_json::from_value(json!({
            "status": "running", "target_url": t, "started_at": started_at, "log": log_lines,
        }))
        .unwrap_or_default();
    })
    .await?;

    // `MigrateServerToInstanceJob`.
    let state = state.clone();
    let team_id = state_team(&state, server_id).await?;
    tokio::spawn(async move {
        match migrate(&state, server_id, team_id, &target, &token).await {
            Ok(result) => record_migration(&state, server_id, "succeeded", result).await,
            Err(e) => record_migration(&state, server_id, "failed", json!({"error": e})).await,
        }
    });
    Ok(())
}

async fn state_team(state: &AppState, server_id: i64) -> Result<i64, String> {
    sqlx::query_scalar("SELECT team_id FROM servers WHERE id = $1")
        .bind(server_id)
        .fetch_one(state.db.pool())
        .await
        .map_err(db_error)
}

/// `sshCheck`.
async fn ssh_check(state: &AppState, server_id: i64) -> Value {
    const LABEL: &str = "this instance can connect to the server over SSH";
    let result = match state.jobs.as_ref() {
        Some(jobs) => match coolify_jobs::server_ops::Srv::load(jobs, server_id).await {
            Ok(s) => coolify_jobs::server_ops::install::validate_connection(&s).await,
            Err(e) => Err(e),
        },
        None => Err("Remote execution is not configured.".to_owned()),
    };
    let (ok, error) = match result {
        Ok((true, _)) => (true, String::new()),
        Ok((false, e)) | Err(e) => (false, e),
    };
    let error = if error.is_empty() { "unknown error".to_owned() } else { error };
    check_row(
        LABEL,
        Some(ok),
        (!ok).then(|| format!("This instance cannot connect to the server over SSH: {}. Fix the connection, then transfer again.", error.trim_end_matches(['.', ' ']))),
    )
}

/// `migrate`.
async fn migrate(state: &AppState, server_id: i64, team_id: i64, target: &str, token: &str) -> Result<Value, String> {
    let metadata = metadata_of(state, server_id).await?;
    for c in migration_checks(state, server_id, &metadata).await? {
        if c["ok"] != json!(true) {
            return Err(text(&c["message"]));
        }
    }
    let enc = state.encrypter.as_ref().ok_or("APP_KEY is not configured.")?;
    log(state, server_id, "Exporting the server configuration and its resources.", "info", json!({})).await;
    let bundle = Exporter::new(state, enc, team_id).export(server_id).await?;
    let export_id = text(&bundle["export_id"]);
    let mut warnings: Vec<Value> = bundle["warnings"].as_array().cloned().unwrap_or_default();
    let mut checklist: Vec<String> = Vec::new();
    for s in bundle["post_transfer_checklist"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        let step = s.replace("{target_url}", target);
        if !checklist.contains(&step) {
            checklist.push(step);
        }
    }
    log(
        state,
        server_id,
        &format!("Export {export_id} created. Sending it to {target}. The target imports the server and takes over management."),
        "info",
        json!({"export_id": export_id}),
    )
    .await;

    let import_body = post_import(state, server_id, target, token, &bundle).await?;

    let target_checklist: Vec<String> = import_body["checklist"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
    if !target_checklist.is_empty() {
        checklist = target_checklist;
    }
    for w in import_body["warnings"].as_array().into_iter().flatten() {
        if !warnings.contains(w) {
            warnings.push(w.clone());
        }
    }
    if import_body["claimed"] != json!(true) {
        let w = import_body["warnings"].as_array().map(|a| a.iter().map(text).collect::<Vec<_>>().join(" ")).unwrap_or_default();
        return Err(format!(
            "Server was imported on {target}, but the target could not take management of it. This instance still manages the server. Delete the imported server on the target, fix the problem, then transfer again. Target warnings: {}",
            if w.is_empty() { "none".to_owned() } else { w }
        ));
    }
    log(state, server_id, "The target imported the server and manages it now. Disabling management on this instance.", "success", json!({})).await;

    let jobs = state.jobs.as_ref().ok_or("Remote execution is not configured.")?;
    mark_transferred(jobs, server_id, Some(&export_id), Some(target), false)
        .await
        .map_err(|e| format!(
            "Server was imported on {target}, but this instance could not mark it as transferred: {e}. Call POST /api/v1/servers/{{uuid}}/transfer/complete on this instance so automations stay disabled here. Do not re-import on the target."
        ))?;

    let server_url = dg(&import_body, "server_url").filter(|u| is_linkable_url(Some(u))).cloned().unwrap_or(Value::Null);
    Ok(json!({
        "export_id": export_id,
        "warnings": warnings,
        "checklist": checklist,
        "target_server_url": server_url,
    }))
}

/// `postImportToTarget`.
async fn post_import(state: &AppState, server_id: i64, target: &str, token: &str, bundle: &Value) -> Result<Value, String> {
    let url = format!("{target}/api/v1/servers/import");
    let response = client(&url, IMPORT_TIMEOUT)?
        .post(&url)
        .bearer_auth(token)
        .header("Accept", "application/json")
        .json(&json!({"bundle": bundle, "dry_run": false, "preserve_uuids": true, "adopt_mode": true}))
        .send()
        .await;
    let response = match response {
        Ok(r) => r,
        Err(e) => {
            if let Some(r) = result_from_status(state, server_id, target, token, bundle, "the request could not reach the target or timed out").await {
                return Ok(r);
            }
            return Err(format!("Could not reach target instance at {}: {}", remove_iip(&url), remove_iip(&e.to_string())));
        }
    };
    let status = response.status();
    let raw = response.text().await.unwrap_or_default();
    let body: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({"raw": remove_iip(&raw)}));
    if status.as_u16() == 404 {
        return Err(format!("The target instance does not support server transfer. Upgrade it to Coolify {} or newer.", state.version));
    }
    if status.is_server_error()
        && let Some(r) = result_from_status(state, server_id, target, token, bundle, &format!("the target answered with HTTP {}", status.as_u16())).await
    {
        return Ok(r);
    }
    if !status.is_success() {
        let message = dg(&body, "message").or_else(|| dg(&body, "errors")).map_or_else(
            || raw.clone(),
            |m| if m.is_string() { text(m) } else { m.to_string() },
        );
        return Err(format!("Target import failed (HTTP {}): {}", status.as_u16(), remove_iip(&message)));
    }
    Ok(body)
}

/// `resultFromTargetStatus`.
async fn result_from_status(state: &AppState, server_id: i64, target: &str, token: &str, bundle: &Value, reason: &str) -> Option<Value> {
    let uuid = dg(bundle, "server.uuid").map(text).unwrap_or_default();
    let export_id = text(&bundle["export_id"]);
    log(state, server_id, &format!("The import request did not finish ({reason}). The target can still finish the import, so this instance asks the target for the result."), "warning", json!({})).await;
    for attempt in 1..=STATUS_PROBE_ATTEMPTS {
        tokio::time::sleep(STATUS_PROBE_INTERVAL).await;
        log(state, server_id, &format!("Asking the target for the import result (attempt {attempt} of {STATUS_PROBE_ATTEMPTS})."), "info", json!({})).await;
        let status = target_status(target, token, &uuid, &export_id).await;
        if status == "rejected" {
            return None;
        }
        if status == "claimed" || status == "imported" {
            return Some(json!({
                "server_uuid": uuid,
                "export_id": export_id,
                "claimed": status == "claimed",
                "warnings": ["The import request to the target timed out. The target status confirms the import."],
            }));
        }
    }
    None
}

/// `targetTransferStatus`.
async fn target_status(target: &str, token: &str, uuid: &str, export_id: &str) -> &'static str {
    let url = format!("{target}/api/v1/servers/{uuid}/transfer");
    let Ok(c) = client(&url, STATUS_TIMEOUT) else {
        return "unknown";
    };
    let Ok(r) = c.get(&url).bearer_auth(token).header("Accept", "application/json").send().await else {
        return "unknown";
    };
    let code = r.status();
    if code.as_u16() == 401 || code.as_u16() == 403 {
        return "rejected";
    }
    if code.as_u16() == 404 {
        return "absent";
    }
    if !code.is_success() {
        return "unknown";
    }
    let body: Value = r.json().await.unwrap_or(Value::Null);
    if dg(&body, "transfer.export_id").and_then(Value::as_str) != Some(export_id) {
        return "absent";
    }
    match dg(&body, "transfer.status").and_then(Value::as_str) {
        Some("claimed") => "claimed",
        Some("imported") => "imported",
        _ => "unknown",
    }
}

/// `requestTransferBack`, with the token creation done by the caller.
pub async fn request_transfer_back(state: &AppState, server_id: i64, uuid: &str, manager_url: &str, manager_token: &str, this_instance_url: &str, import_token: &str) -> Result<(), String> {
    let manager = validate_instance_url(state, manager_url).await.map_err(|e| e.text())?;
    let token = normalize_token(manager_token)?;
    let this = this_instance_url.trim().trim_end_matches('/').to_owned();
    if !crate::laravel_url::is_url(&this) {
        return Err("Enter the URL at which the other instance reaches this instance.".into());
    }
    let url = format!("{manager}/api/v1/servers/{uuid}/migrate");
    let response = client(&url, CHECK_TIMEOUT * 4)?
        .post(&url)
        .bearer_auth(&token)
        .header("Accept", "application/json")
        .json(&json!({"target_url": this, "target_token": import_token}))
        .send()
        .await
        .map_err(|e| format!("This instance cannot reach {}: {}", remove_iip(&manager), remove_iip(&e.to_string())))?;
    let status = response.status().as_u16();
    if status != 202 {
        let body: Value = response.json().await.unwrap_or(Value::Null);
        let message = dg(&body, "message").filter(|m| crate::serialize::php_truthy(m)).map_or_else(|| "no details".to_owned(), text);
        return Err(match status {
            401 => "The other instance does not accept the API token.".to_owned(),
            403 => format!("The API token of the other instance needs root, or read:sensitive and write, from an admin or owner. {message}"),
            404 => format!("The other instance does not know this server, or does not support transfers. {message}"),
            n => format!("The other instance did not start the transfer back (HTTP {n}): {message}"),
        });
    }
    let mut tx = state.db.pool().begin().await.map_err(db_error)?;
    let mut metadata = locked_metadata(&mut tx, server_id).await?;
    if let Some(t) = metadata.get_mut("transfer").and_then(Value::as_object_mut) {
        t.insert("transfer_back".into(), json!({"requested_at": now_iso(), "from": manager}));
    }
    write_metadata(&mut tx, server_id, &metadata).await?;
    tx.commit().await.map_err(db_error)
}
