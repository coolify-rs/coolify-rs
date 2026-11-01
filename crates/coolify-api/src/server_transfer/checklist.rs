//! `TransferFinishChecklist`: the steps that finish a transfer on the instance that manages the
//! server now. Saved in `server_metadata.transfer.finish`.

use chrono::{DateTime, FixedOffset};
use coolify_jobs::server_transfer::{locked_metadata, write_metadata};
use serde_json::{Map, Value, json};

use super::{blank, dg, instance_url, now_iso, text};
use crate::state::AppState;

/// `OPEN_STATUSES`.
pub const OPEN: &[&str] = &["pending", "action", "unchecked"];
/// `RECHECK_AFTER_SECONDS`.
const RECHECK_AFTER_SECONDS: i64 = 30;

fn db_error(e: sqlx::Error) -> String {
    tracing::warn!(error = %e, "transfer checklist query failed");
    "A database error occurred.".to_owned()
}

/// `item()`.
#[allow(clippy::too_many_arguments)]
fn item(
    kind: &str,
    subject: &str,
    label: String,
    hint: Option<&str>,
    details: Value,
    link: Option<String>,
    link_label: Option<&str>,
) -> Value {
    json!({
        "key": format!("{kind}:{subject}"),
        "type": kind,
        "subject": subject,
        "label": label,
        "status": "pending",
        "hint": hint,
        "message": null,
        "details": details,
        "error": null,
        "link": link,
        "link_label": link_label,
        "checked_at": null,
    })
}

async fn query_rows(state: &AppState, sql: &str, id: i64) -> Result<Vec<Value>, String> {
    let wrapped = format!("SELECT row_to_json(t)::text FROM ({sql}) t");
    let raw: Vec<String> = sqlx::query_scalar(&wrapped)
        .bind(id)
        .fetch_all(state.db.pool())
        .await
        .map_err(db_error)?;
    Ok(raw
        .iter()
        .filter_map(|r| serde_json::from_str(r).ok())
        .collect())
}

const APPLICATIONS: &str = "SELECT a.id, a.uuid, a.name, a.fqdn, a.build_pack, a.git_repository, a.source_type, a.source_id, \
    e.uuid AS environment_uuid, p.uuid AS project_uuid, \
    COALESCE(gh.is_public, gl.is_public, gt.is_public) AS source_public, COALESCE(gh.is_system_wide, gl.is_system_wide, gt.is_system_wide) AS source_system, \
    COALESCE(gh.uuid, gl.uuid, gt.uuid) AS source_uuid, COALESCE(gh.name, gl.name, gt.name) AS source_name \
    FROM applications a LEFT JOIN environments e ON e.id = a.environment_id LEFT JOIN projects p ON p.id = e.project_id \
    LEFT JOIN github_apps gh ON a.source_type = 'App\\Models\\GithubApp' AND gh.id = a.source_id \
    LEFT JOIN gitlab_apps gl ON a.source_type = 'App\\Models\\GitlabApp' AND gl.id = a.source_id \
    LEFT JOIN gitea_apps gt ON a.source_type = 'App\\Models\\GiteaApp' AND gt.id = a.source_id \
    WHERE a.deleted_at IS NULL AND ((a.destination_type = 'App\\Models\\StandaloneDocker' AND a.destination_id IN (SELECT id FROM standalone_dockers WHERE server_id = $1)) \
    OR (a.destination_type = 'App\\Models\\SwarmDocker' AND a.destination_id IN (SELECT id FROM swarm_dockers WHERE server_id = $1)))";

/// `parse_url($url, PHP_URL_HOST)`, lowercased.
fn host_of(url: &str) -> Option<String> {
    let (_, rest) = url.trim().split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let host = if authority.starts_with('[') {
        authority.split(']').next()?.to_owned() + "]"
    } else {
        authority.split(':').next()?.to_owned()
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// `buildItems`.
async fn build_items(
    state: &AppState,
    server_id: i64,
    uuid: &str,
    metadata: &Value,
) -> Result<Vec<Value>, String> {
    let url = instance_url(&state.db).await;
    let webhook = format!("{url}/webhooks/source/github/events");
    let applications = query_rows(state, APPLICATIONS, server_id).await?;
    let service_apps = query_rows(state, "SELECT sa.id, sa.fqdn FROM service_applications sa JOIN services s ON s.id = sa.service_id WHERE s.server_id = $1", server_id).await?;
    let runner = query_rows(state, "SELECT c.uuid, g.uuid AS app_uuid, g.name AS app_name, g.is_system_wide, g.is_public FROM github_runner_configs c LEFT JOIN github_apps g ON g.id = c.github_app_id WHERE c.server_id = $1 LIMIT 1", server_id).await?;
    let runner = runner.first();

    let mut items = vec![
        item(
            "server_connection",
            "server",
            "Connection to the server".into(),
            None,
            json!([]),
            None,
            None,
        ),
        item(
            "sentinel",
            "server",
            "Sentinel sends its data to this instance".into(),
            None,
            json!([]),
            None,
            None,
        ),
        // The SSH key moved with the server, so the previous instance still has a copy of it.
        item(
            "previous_ssh_key",
            "server",
            "Remove the SSH key of the previous instance from the server".into(),
            Some(
                "The previous instance keeps a copy of the SSH key. Add a new key of this instance to the server and select it here, then remove the old public key from ~/.ssh/authorized_keys on the server.",
            ),
            json!([]),
            Some(format!("/server/{uuid}/private-key")),
            Some("Open private key"),
        ),
    ];

    let mut seen = Vec::new();
    let mut github_apps: Vec<(String, String)> = applications
        .iter()
        .filter(|a| {
            a["source_type"].as_str() == Some("App\\Models\\GithubApp")
                && a["source_system"] == json!(false)
                && a["source_public"] == json!(false)
        })
        .map(|a| (text(&a["source_uuid"]), text(&a["source_name"])))
        .collect();
    if let Some(r) = runner
        && r["is_system_wide"] == json!(false)
        && r["is_public"] == json!(false)
    {
        github_apps.push((text(&r["app_uuid"]), text(&r["app_name"])));
    }
    for (app_uuid, name) in github_apps {
        if seen.contains(&app_uuid) {
            continue;
        }
        seen.push(app_uuid.clone());
        items.push(item(
            "github_app_webhook",
            &app_uuid,
            format!("Webhook URL of GitHub App {name}"),
            Some("GitHub must send webhooks to this instance."),
            json!([{"label": "Webhook URL", "value": webhook}]),
            None,
            None,
        ));
        items.push(item(
            "github_app_urls",
            &app_uuid,
            format!("Setup and Callback URL of GitHub App {name}"),
            Some("Set both in the GitHub App settings. GitHub has no API for them."),
            json!([
                {"label": "Setup URL", "value": format!("{url}/webhooks/source/github/install")},
                {"label": "Callback URL", "value": format!("{url}/login/github/app")},
            ]),
            None,
            None,
        ));
    }
    let mut seen = Vec::new();
    for a in applications.iter().filter(|a| {
        a["source_type"].as_str() == Some("App\\Models\\GitlabApp")
            && a["source_system"] == json!(false)
    }) {
        let source = text(&a["source_uuid"]);
        if seen.contains(&source) {
            continue;
        }
        seen.push(source.clone());
        items.push(item(
            "gitlab_webhook",
            &source,
            format!("GitLab webhooks of GitLab App {}", text(&a["source_name"])),
            Some("Set the webhook URL in each GitLab project that uses it, then push once."),
            json!([{"label": "Webhook URL", "value": format!("{url}/webhooks/source/gitlab/events")}]),
            None,
            None,
        ));
    }

    let git_sources: Vec<String> = dg(metadata, "transfer.git_source_applications")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(text).collect())
        .unwrap_or_default();
    for a in &applications {
        let u = text(&a["uuid"]);
        if !git_sources.contains(&u)
            && a["build_pack"].as_str() != Some("dockerimage")
            && !blank(a.get("git_repository"))
            && (a["source_uuid"].is_null() || a["source_public"] == json!(true))
        {
            let details: Vec<Value> = [("github", "GitHub"), ("gitlab", "GitLab"), ("bitbucket", "Bitbucket"), ("gitea", "Gitea")]
                .iter()
                .map(|(p, l)| json!({"label": l, "value": format!("{url}/webhooks/source/{p}/events/manual")}))
                .collect();
            items.push(item(
                "manual_webhook",
                &u,
                format!("Manual webhook of {}", text(&a["name"])),
                Some("If the repository has a webhook to Coolify, change its URL, then push once."),
                json!(details),
                None,
                None,
            ));
        }
    }

    // DNS records of the resources.
    let records = query_rows(state, "SELECT DISTINCT r.uuid, r.name FROM managed_dns_records r JOIN managed_dns_record_references x ON x.managed_dns_record_id = r.id WHERE \
        (x.resource_type = 'App\\Models\\Application' AND x.resource_id IN (SELECT a.id FROM applications a WHERE (a.destination_type = 'App\\Models\\StandaloneDocker' AND a.destination_id IN (SELECT id FROM standalone_dockers WHERE server_id = $1)) OR (a.destination_type = 'App\\Models\\SwarmDocker' AND a.destination_id IN (SELECT id FROM swarm_dockers WHERE server_id = $1)))) \
        OR (x.resource_type = 'App\\Models\\ServiceApplication' AND x.resource_id IN (SELECT sa.id FROM service_applications sa JOIN services s ON s.id = sa.service_id WHERE s.server_id = $1)) ORDER BY r.name", server_id).await?;
    for r in records {
        items.push(item(
            "dns_record",
            &text(&r["uuid"]),
            format!("DNS record {}", text(&r["name"])),
            None,
            json!([]),
            None,
            None,
        ));
    }

    let mut hosts = Vec::new();
    let fqdns = applications
        .iter()
        .chain(service_apps.iter())
        .map(|a| text(&a["fqdn"]));
    for fqdn in fqdns {
        for part in fqdn.split(',') {
            if let Some(h) = host_of(part)
                && !hosts.contains(&h)
            {
                hosts.push(h);
            }
        }
    }
    for host in hosts {
        items.push(item(
            "domain",
            &host,
            format!("Domain {host} points to the server"),
            None,
            json!([]),
            None,
            None,
        ));
    }

    // `s3Storages`: the storages of the backups of this server's databases.
    let mut database_ids = String::from(
        "(b.database_type = 'App\\Models\\ServiceDatabase' AND b.database_id IN (SELECT sd.id FROM service_databases sd JOIN services sv ON sv.id = sd.service_id WHERE sv.server_id = $1))",
    );
    for (table, model) in [
        ("standalone_postgresqls", "StandalonePostgresql"),
        ("standalone_redis", "StandaloneRedis"),
        ("standalone_mongodbs", "StandaloneMongodb"),
        ("standalone_mysqls", "StandaloneMysql"),
        ("standalone_mariadbs", "StandaloneMariadb"),
        ("standalone_keydbs", "StandaloneKeydb"),
        ("standalone_dragonflies", "StandaloneDragonfly"),
        ("standalone_clickhouses", "StandaloneClickhouse"),
        ("standalone_sqlites", "StandaloneSqlite"),
    ] {
        database_ids.push_str(&format!(" OR (b.database_type = 'App\\Models\\{model}' AND b.database_id IN (SELECT x.id FROM {table} x WHERE (x.destination_type = 'App\\Models\\StandaloneDocker' AND x.destination_id IN (SELECT id FROM standalone_dockers WHERE server_id = $1)) OR (x.destination_type = 'App\\Models\\SwarmDocker' AND x.destination_id IN (SELECT id FROM swarm_dockers WHERE server_id = $1))))"));
    }
    let s3 = query_rows(state, &format!("SELECT s.uuid, s.name FROM s3_storages s WHERE s.team_id = (SELECT team_id FROM servers WHERE id = $1) AND s.id IN (SELECT b.s3_storage_id FROM scheduled_database_backups b WHERE {database_ids} UNION SELECT j.s3_storage_id FROM scheduled_database_backup_s3_storage j JOIN scheduled_database_backups b ON b.id = j.scheduled_database_backup_id WHERE {database_ids})"), server_id).await?;
    for st in s3 {
        items.push(item(
            "s3_storage",
            &text(&st["uuid"]),
            format!("S3 storage {} works from this instance", text(&st["name"])),
            None,
            json!([]),
            None,
            None,
        ));
    }

    if let Some(r) = runner {
        items.push(item(
            "github_runners",
            &text(&r["uuid"]),
            "GitHub runners on this server".into(),
            Some("Disabled after the transfer. Enable them when the GitHub App sends its webhooks here."),
            json!([]),
            Some(format!("/server/{uuid}/github-runners")),
            Some("Open runners"),
        ));
    }

    for a in applications
        .iter()
        .filter(|a| git_sources.contains(&text(&a["uuid"])))
    {
        let link = (!a["project_uuid"].is_null()).then(|| {
            format!(
                "/project/{}/environment/{}/application/{}/source",
                text(&a["project_uuid"]),
                text(&a["environment_uuid"]),
                text(&a["uuid"])
            )
        });
        items.push(item(
            "git_source",
            &text(&a["uuid"]),
            format!("Git source of {}", text(&a["name"])),
            Some("System-wide git sources are not transferred. Select a source of this instance."),
            json!([]),
            link,
            Some("Open source"),
        ));
    }
    Ok(items)
}

/// `save`: writes only `transfer.finish`, on the current row.
async fn save(state: &AppState, server_id: i64, finish: Value) -> Result<(), String> {
    let mut tx = state.db.pool().begin().await.map_err(db_error)?;
    let mut metadata = locked_metadata(&mut tx, server_id).await?;
    let transfer = metadata
        .entry("transfer")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(t) = transfer.as_object_mut() {
        t.insert("finish".into(), finish);
    }
    write_metadata(&mut tx, server_id, &metadata).await?;
    tx.commit().await.map_err(db_error)
}

async fn metadata_of(state: &AppState, server_id: i64) -> Result<Value, String> {
    let row: (Option<Value>,) = sqlx::query_as("SELECT server_metadata FROM servers WHERE id = $1")
        .bind(server_id)
        .fetch_one(state.db.pool())
        .await
        .map_err(db_error)?;
    Ok(row.0.unwrap_or(Value::Null))
}

/// `start`.
pub async fn start(state: &AppState, server_id: i64, uuid: &str) -> Result<usize, String> {
    let metadata = metadata_of(state, server_id).await?;
    let items = build_items(state, server_id, uuid, &metadata).await?;
    let count = items.len();
    save(
        state,
        server_id,
        json!({"started_at": now_iso(), "finished_at": null, "finished_by": null, "skipped": [], "items": items}),
    )
    .await?;
    Ok(count)
}

/// `startFinishChecklist`: a failure never fails the import.
pub async fn start_after_import(state: &AppState, server_id: i64, team_id: i64, name: &str) {
    let uuid: Result<String, _> = sqlx::query_scalar("SELECT uuid FROM servers WHERE id = $1")
        .bind(server_id)
        .fetch_one(state.db.pool())
        .await;
    let Ok(uuid) = uuid else { return };
    match start(state, server_id, &uuid).await {
        Ok(count) => coolify_jobs::notifications::spawn_team_general(
            &state.db,
            state.encrypter.as_ref(),
            team_id,
            format!(
                "Server '{name}' was transferred to this instance. Open the server to finish the transfer: {count} steps to check."
            ),
            true,
        ),
        Err(e) => {
            tracing::warn!(server = %uuid, error = %e, "Could not start the transfer finish checklist")
        }
    }
}

/// `of`, `isOpen`, and the view data of `TransferFinish::render`.
pub async fn view(state: &AppState, server_id: i64) -> Result<Value, String> {
    let metadata = metadata_of(state, server_id).await?;
    let finish = dg(&metadata, "transfer.finish");
    let transferred =
        dg(&metadata, "transfer.status").and_then(Value::as_str) == Some("transferred");
    let Some(finish) = finish.filter(|f| f["finished_at"].is_null() && !transferred) else {
        return Ok(Value::Null);
    };
    let mut items = finish["items"].as_array().cloned().unwrap_or_default();
    for it in &mut items {
        let kind = text(&it["type"]);
        let (link, label) = link_of(it);
        let o = it.as_object_mut().expect("item");
        o.insert("group".into(), json!(group_of(&kind)));
        o.insert(
            "can_check".into(),
            json!(!matches!(
                kind.as_str(),
                "github_app_urls" | "previous_ssh_key"
            )),
        );
        o.insert("fix_label".into(), json!(fix_label(&kind)));
        o.insert("resolved_link".into(), json!(link));
        o.insert("resolved_link_label".into(), json!(label));
    }
    Ok(json!({
        "last_checked_at": finish["last_checked_at"],
        "items": items,
    }))
}

fn group_of(kind: &str) -> &'static str {
    match kind {
        "github_app_webhook" | "github_app_urls" | "gitlab_webhook" | "manual_webhook"
        | "git_source" | "github_runners" => "git",
        "dns_record" | "domain" => "dns",
        _ => "server",
    }
}

fn fix_label(kind: &str) -> Option<&'static str> {
    match kind {
        "github_app_webhook" => Some("Set webhook URL"),
        "sentinel" => Some("Restart Sentinel"),
        "dns_record" => Some("Take ownership"),
        _ => None,
    }
}

/// `linkOf`: saved links are paths of this instance.
fn link_of(item: &Value) -> (Option<String>, Option<String>) {
    let link = item["link"].as_str();
    match link {
        Some(l) if l.starts_with('/') && !l.starts_with("//") && !l.starts_with("/\\") => (
            Some(l.to_owned()),
            Some(item["link_label"].as_str().unwrap_or("Open").to_owned()),
        ),
        _ => (None, None),
    }
}

fn parse(s: Option<&str>) -> Option<DateTime<FixedOffset>> {
    s.and_then(|s| DateTime::parse_from_rfc3339(s).ok())
}

/// `runCheck`: status, message, error. GitHub, Cloudflare and S3 calls are not ported, so those
/// steps stay `action` with the reason in the error.
async fn run_check(
    state: &AppState,
    server_id: i64,
    item: &Value,
    started_at: Option<&str>,
) -> (String, String, Option<String>) {
    let kind = text(&item["type"]);
    let subject = text(&item["subject"]);
    let done = |s: &str, m: String, e: Option<String>| (s.to_owned(), m, e);
    match kind.as_str() {
        "server_connection" => {
            let Some(jobs) = state.jobs.as_ref() else {
                return done(
                    "action",
                    "This instance cannot connect to the server over SSH.".into(),
                    Some("Remote execution is not configured.".into()),
                );
            };
            let srv = coolify_jobs::server_ops::Srv::load(jobs, server_id).await;
            let result = match srv {
                Ok(s) => coolify_jobs::server_ops::install::validate_connection(&s).await,
                Err(e) => Err(e),
            };
            match result {
                Ok((true, _)) => done(
                    "ok",
                    "This instance connects to the server over SSH.".into(),
                    None,
                ),
                Ok((false, e)) => done(
                    "action",
                    "This instance cannot connect to the server over SSH.".into(),
                    Some(e),
                ),
                Err(e) => done(
                    "action",
                    "This instance cannot connect to the server over SSH.".into(),
                    Some(e),
                ),
            }
        }
        "sentinel" => {
            let updated: Option<(Option<chrono::NaiveDateTime>,)> =
                sqlx::query_as("SELECT sentinel_updated_at FROM servers WHERE id = $1")
                    .bind(server_id)
                    .fetch_optional(state.db.pool())
                    .await
                    .unwrap_or(None);
            let started = parse(started_at).map(|d| d.naive_utc());
            match (updated.and_then(|u| u.0), started) {
                (Some(u), Some(s)) if u > s => done(
                    "ok",
                    format!("Sentinel sent data at {}.", u.format("%H:%M")),
                    None,
                ),
                _ => done(
                    "action",
                    "No data from Sentinel since the transfer.".into(),
                    None,
                ),
            }
        }
        "github_app_webhook" | "gitlab_webhook" | "manual_webhook" => {
            let receipt = match kind.as_str() {
                "github_app_webhook" => "github_app",
                "gitlab_webhook" => "gitlab_app",
                _ => "application",
            };
            let received = webhook_received_at(receipt, &subject);
            match (received, parse(started_at)) {
                (Some(at), Some(started)) if at > started => done(
                    "ok",
                    format!("A webhook arrived at {}.", at.format("%H:%M")),
                    None,
                ),
                _ => done(
                    "action",
                    "No webhook arrived since the transfer.".into(),
                    None,
                ),
            }
        }
        "dns_record" => {
            let owned: Option<(bool,)> =
                sqlx::query_as("SELECT owned FROM managed_dns_records WHERE uuid = $1")
                    .bind(&subject)
                    .fetch_optional(state.db.pool())
                    .await
                    .unwrap_or(None);
            match owned {
                None => done("unchecked", "This instance does not track this record anymore.".into(), None),
                Some((true,)) => {
                    if reclaim_record(state, &subject).await {
                        done("ok", "This instance manages the record at Cloudflare.".into(), None)
                    } else {
                        done("action", "Cloudflare does not confirm the record.".into(), Some("The record changed at Cloudflare, or Cloudflare cannot be reached with the token.".into()))
                    }
                }
                Some((false,)) => done("action", "This instance does not own the record yet.".into(), Some("Until it owns the record, this instance does not change or delete it.".into())),
            }
        }
        "domain" => check_domain(state, server_id, &subject).await,
        "s3_storage" => done(
            "action",
            "This instance cannot use the S3 storage.".into(),
            None,
        ),
        "git_source" => {
            let rows = query_rows(state, "SELECT a.source_type, COALESCE(gh.name, gl.name) AS name, COALESCE(gh.is_system_wide, gl.is_system_wide) AS system_wide FROM applications a LEFT JOIN github_apps gh ON a.source_type = 'App\\Models\\GithubApp' AND gh.id = a.source_id LEFT JOIN gitlab_apps gl ON a.source_type = 'App\\Models\\GitlabApp' AND gl.id = a.source_id WHERE a.uuid = (SELECT uuid FROM applications WHERE id = $1)", item_app_id(state, &subject).await).await.unwrap_or_default();
            match rows.first().filter(|r| !r["name"].is_null()) {
                None => done("action", "No git source on this instance, so deployments from git fail.".into(), None),
                Some(r) => done(
                    "ok",
                    format!("Uses {}.", text(&r["name"])),
                    (r["system_wide"] == json!(true)).then(|| "For push-to-deploy, use a manual webhook if the system-wide App is registered on another instance.".to_owned()),
                ),
            }
        }
        "github_runners" => {
            let enabled: Option<(bool,)> =
                sqlx::query_as("SELECT is_enabled FROM github_runner_configs WHERE server_id = $1")
                    .bind(server_id)
                    .fetch_optional(state.db.pool())
                    .await
                    .unwrap_or(None);
            if enabled.is_some_and(|e| e.0) {
                done("ok", "GitHub runners are enabled.".into(), None)
            } else {
                done("action", "Disabled after the transfer. Enable them when the GitHub App sends its webhooks here.".into(), None)
            }
        }
        _ => done(
            "action",
            "Coolify could not do this step.".into(),
            Some("Coolify cannot check this step. Do it by hand, then mark it as done.".into()),
        ),
    }
}

async fn item_app_id(state: &AppState, uuid: &str) -> i64 {
    sqlx::query_scalar("SELECT id FROM applications WHERE uuid = $1")
        .bind(uuid)
        .fetch_optional(state.db.pool())
        .await
        .ok()
        .flatten()
        .unwrap_or(-1)
}

/// `checkDomain`.
async fn check_domain(
    state: &AppState,
    server_id: i64,
    host: &str,
) -> (String, String, Option<String>) {
    let ip: Option<(String,)> = sqlx::query_as("SELECT ip FROM servers WHERE id = $1")
        .bind(server_id)
        .fetch_optional(state.db.pool())
        .await
        .unwrap_or(None);
    let ip = ip.map(|r| r.0).unwrap_or_default();
    let resolve = |h: String| async move {
        tokio::net::lookup_host(format!("{h}:80"))
            .await
            .map(|a| a.map(|x| x.ip().to_string()).collect::<Vec<_>>())
            .unwrap_or_default()
    };
    let server_ips = if ip.parse::<std::net::IpAddr>().is_ok() {
        vec![ip.clone()]
    } else {
        resolve(ip.clone()).await
    };
    let ips = resolve(host.to_owned()).await;
    if ips.is_empty() {
        return ("action".into(), "The domain does not resolve.".into(), None);
    }
    if ips.iter().any(|i| server_ips.contains(i)) {
        return ("ok".into(), "The domain points to the server.".into(), None);
    }
    let shown = if server_ips.is_empty() {
        ip
    } else {
        server_ips.join(", ")
    };
    (
        "action".into(),
        "The domain points to another address.".into(),
        Some(format!(
            "Resolves to {}; the server is {shown}. If a proxy or load balancer is in front, mark it as done.",
            ips.join(", ")
        )),
    )
}

/// `update`.
async fn update_item(
    state: &AppState,
    server_id: i64,
    key: &str,
    status: &str,
    message: &str,
    error: Option<String>,
) -> Result<Value, String> {
    let mut tx = state.db.pool().begin().await.map_err(db_error)?;
    let mut metadata = locked_metadata(&mut tx, server_id).await?;
    let finish = metadata
        .get_mut("transfer")
        .and_then(|t| t.get_mut("finish"))
        .ok_or("This server has no transfer to finish.")?;
    let items = finish["items"]
        .as_array_mut()
        .ok_or("This step is not in the list.")?;
    let found = items
        .iter_mut()
        .find(|i| i["key"].as_str() == Some(key))
        .ok_or("This step is not in the list.")?;
    let o = found
        .as_object_mut()
        .ok_or("This step is not in the list.")?;
    o.insert("status".into(), json!(status));
    o.insert("message".into(), json!(message));
    o.insert("error".into(), json!(error));
    o.insert("checked_at".into(), json!(now_iso()));
    let out = found.clone();
    write_metadata(&mut tx, server_id, &metadata).await?;
    tx.commit().await.map_err(db_error)?;
    Ok(out)
}

/// `check`.
pub async fn check(state: &AppState, server_id: i64, key: &str) -> Result<Value, String> {
    let metadata = metadata_of(state, server_id).await?;
    let item = dg(&metadata, "transfer.finish.items")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().find(|i| i["key"].as_str() == Some(key)))
        .cloned()
        .ok_or("This step is not in the list.")?;
    let started = dg(&metadata, "transfer.finish.started_at").and_then(Value::as_str);
    let (status, message, error) = run_check(state, server_id, &item, started).await;
    update_item(state, server_id, key, &status, &message, error).await
}

/// `checkAll`.
pub async fn check_all(state: &AppState, server_id: i64) -> Result<(), String> {
    let metadata = metadata_of(state, server_id).await?;
    let open: Vec<String> = dg(&metadata, "transfer.finish.items")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|i| OPEN.contains(&i["status"].as_str().unwrap_or("")))
                .filter(|i| {
                    !matches!(
                        i["type"].as_str(),
                        Some("github_app_urls" | "previous_ssh_key")
                    )
                })
                .map(|i| text(&i["key"]))
                .collect()
        })
        .unwrap_or_default();
    for key in open {
        check(state, server_id, &key).await?;
    }
    let metadata = metadata_of(state, server_id).await?;
    let mut finish = dg(&metadata, "transfer.finish")
        .cloned()
        .unwrap_or(json!({}));
    finish["last_checked_at"] = json!(now_iso());
    save(state, server_id, finish).await
}

/// `checkWhenStale`.
pub async fn check_when_stale(state: &AppState, server_id: i64) -> Result<(), String> {
    let metadata = metadata_of(state, server_id).await?;
    let last = dg(&metadata, "transfer.finish.last_checked_at").and_then(Value::as_str);
    if let Some(t) = parse(last)
        && (chrono::Utc::now() - t.to_utc()).num_seconds() < RECHECK_AFTER_SECONDS
    {
        return Ok(());
    }
    let state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = check_all(&state, server_id).await {
            tracing::warn!(error = %e, "CheckTransferFinishStepsJob failed");
        }
    });
    Ok(())
}

/// `fix`: Sentinel restarts in the background. The GitHub call is not ported.
pub async fn fix(state: &AppState, server_id: i64, key: &str) -> Result<Value, String> {
    let kind = key.split(':').next().unwrap_or("");
    if fix_label(kind).is_none() {
        return update_item(
            state,
            server_id,
            key,
            "action",
            "Coolify could not do this step.",
            Some("Coolify cannot fix this step. Do it by hand, then mark it as done.".into()),
        )
        .await;
    }
    if kind == "sentinel" {
        if let Some(jobs) = state.jobs.as_ref() {
            let (jobs, id) = (jobs.clone(), server_id);
            tokio::spawn(async move {
                if let Err(e) =
                    coolify_jobs::server_ops::sentinel::check_and_start_sentinel(&jobs, id).await
                {
                    tracing::warn!(error = %e, "Restart Sentinel failed");
                }
            });
        }
        return update_item(
            state,
            server_id,
            key,
            "pending",
            "Sentinel restarts. The step turns OK when Sentinel sends data.",
            None,
        )
        .await;
    }
    if kind == "dns_record" {
        let subject = key.split_once(':').map_or("", |(_, s)| s);
        if reclaim_record(state, subject).await {
            let _ = sqlx::query("UPDATE managed_dns_records SET owned = true WHERE uuid = $1")
                .bind(subject)
                .execute(state.db.pool())
                .await;
            return check(state, server_id, key).await;
        }
    }
    update_item(state, server_id, key, "action", "Coolify could not do this step.", Some(if kind == "dns_record" { "Coolify could not take ownership: the record changed at Cloudflare, or Cloudflare cannot be reached.".to_owned() } else { "GitHub cannot be reached from this build.".to_owned() })).await
}

fn webhook_receipts_path() -> std::path::PathBuf {
    coolify_jobs::kernel::data_dir().join("transfer-finish-webhooks.json")
}

static WEBHOOK_RECEIPTS: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// `now()->addDays(60)` of `recordWebhook`.
const WEBHOOK_RECEIPT_DAYS: i64 = 60;

/// `TransferFinishChecklist::recordWebhook`: upstream's cache entry, here a small file in the data
/// directory, so it survives a restart without a cache service.
pub fn record_webhook(kind: &str, uuid: &str) {
    if uuid.is_empty() {
        return;
    }
    let Ok(_guard) = WEBHOOK_RECEIPTS.lock() else {
        return;
    };
    let path = webhook_receipts_path();
    let mut map: serde_json::Map<String, Value> = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let now = chrono::Utc::now();
    map.retain(|_, v| {
        v.as_str()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .is_some_and(|t| (now - t.to_utc()).num_days() < WEBHOOK_RECEIPT_DAYS)
    });
    map.insert(format!("{kind}:{uuid}"), json!(now_iso()));
    if let Ok(text) = serde_json::to_vec(&map) {
        let _ = std::fs::write(&path, text);
    }
}

fn webhook_received_at(kind: &str, uuid: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    let _guard = WEBHOOK_RECEIPTS.lock().ok()?;
    let map: serde_json::Map<String, Value> =
        serde_json::from_slice(&std::fs::read(webhook_receipts_path()).ok()?).ok()?;
    map.get(&format!("{kind}:{uuid}"))
        .and_then(Value::as_str)
        .and_then(|t| parse(Some(t)))
}

/// `CloudflareDnsProvider::reclaimRecord` of the record with this uuid.
pub(crate) async fn reclaim_record(state: &AppState, uuid: &str) -> bool {
    let Some(enc) = state.encrypter.as_ref() else {
        return false;
    };
    let id: Option<i64> = sqlx::query_scalar("SELECT id FROM managed_dns_records WHERE uuid = $1")
        .bind(uuid)
        .fetch_optional(state.db.pool())
        .await
        .unwrap_or(None);
    let Some(id) = id else {
        return false;
    };
    let Ok(Some(record)) = coolify_db::managed_dns::record_by_id(&state.db, id).await else {
        return false;
    };
    let dns = coolify_jobs::integrations::cloudflare::Dns {
        db: &state.db,
        enc,
        now: state.now(),
        source: "system",
    };
    dns.reclaim_record(&record).await
}

/// `markDone`.
pub async fn mark_done(
    state: &AppState,
    server_id: i64,
    key: &str,
    user: &str,
) -> Result<Value, String> {
    update_item(
        state,
        server_id,
        key,
        "done",
        &format!("Marked as done by {user}."),
        None,
    )
    .await
}

/// `finish`.
pub async fn finish(state: &AppState, server_id: i64, user_id: i64) -> Result<usize, String> {
    let metadata = metadata_of(state, server_id).await?;
    let mut finish = dg(&metadata, "transfer.finish")
        .cloned()
        .ok_or("This server has no transfer to finish.")?;
    let skipped: Vec<String> = finish["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|i| OPEN.contains(&i["status"].as_str().unwrap_or("")))
                .map(|i| text(&i["key"]))
                .collect()
        })
        .unwrap_or_default();
    let count = skipped.len();
    finish["finished_at"] = json!(now_iso());
    finish["finished_by"] = json!(user_id);
    finish["skipped"] = json!(skipped);
    save(state, server_id, finish).await?;
    Ok(count)
}
