//! Pull request previews as far as the webhooks reach them: `ApplicationPreview` rows and
//! `generate_preview_fqdn()`, `CleanupPreviewDeployment` and `ApplicationPullRequestUpdateJob`.
//!
//! Deploying a preview is not part of this module.

use chrono::Utc;
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::records::Record;
use coolify_db::servers;
use coolify_remote::RunOptions;
use coolify_remote::shell::quote;
use serde_json::{Value, json};

use crate::Jobs;
use crate::server::ServerInfo;

/// `ProcessStatus` of `ApplicationPullRequestUpdateJob`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullRequestStatus {
    Queued,
    InProgress,
    Finished,
    Error,
    Killed,
    Cancelled,
    Closed,
}

/// `getPreviewLinks()`.
fn preview_links(build_pack: Option<&str>, preview: &Record) -> String {
    if build_pack == Some("dockercompose") {
        let domains: serde_json::Map<String, Value> = preview
            .get("docker_compose_domains")
            .and_then(Value::as_str)
            .and_then(|t| serde_json::from_str(t).ok())
            .unwrap_or_default();
        let mut links = Vec::new();
        for (service, config) in &domains {
            let first = config
                .get("domain")
                .and_then(Value::as_str)
                .and_then(|d| d.split(',').next())
                .map(str::trim)
                .unwrap_or_default();
            if !first.is_empty() {
                links.push(format!("[Open {service}]({first})"));
            }
        }
        return if links.is_empty() {
            String::new()
        } else {
            format!("{} | ", links.join(" | "))
        };
    }
    preview
        .get("fqdn")
        .and_then(Value::as_str)
        .filter(|f| !f.is_empty())
        .map(|f| format!("[Open Preview]({f}) | "))
        .unwrap_or_default()
}

/// The status line of `ApplicationPullRequestUpdateJob::handle()`.
fn pull_request_status_line(
    status: PullRequestStatus,
    name: &str,
    build_pack: Option<&str>,
    preview: &Record,
) -> String {
    match status {
        PullRequestStatus::Queued => {
            format!("The preview deployment for **{name}** is queued. ⏳\n\n")
        }
        PullRequestStatus::InProgress => {
            format!("The preview deployment for **{name}** is in progress. 🟡\n\n")
        }
        PullRequestStatus::Finished => format!(
            "The preview deployment for **{name}** is ready. 🟢\n\n{}",
            preview_links(build_pack, preview)
        ),
        PullRequestStatus::Error => format!("The preview deployment for **{name}** failed. 🔴\n\n"),
        PullRequestStatus::Killed => {
            format!("The preview deployment for **{name}** was killed. ⚫\n\n")
        }
        PullRequestStatus::Cancelled => {
            format!("The preview deployment for **{name}** was cancelled. 🚫\n\n")
        }
        PullRequestStatus::Closed => String::new(),
    }
}

/// `generatedPreviewDomain($sourceDomain)`: the URL and the port override.
fn generated_preview_domain(
    a: &ApplicationRow,
    source_domain: &str,
    pull_request_id: i64,
    random: &str,
) -> (String, Option<i64>) {
    let parts = super::domains::UrlParts::parse(source_domain);
    let (scheme, host, path, port) = parts.map_or_else(
        || ("http".to_owned(), String::new(), String::new(), None),
        |p| {
            let path = p.path.clone().filter(|v| v != "/").unwrap_or_default();
            (p.scheme.clone(), p.host.clone(), path, p.port)
        },
    );
    let template = a.str("preview_url_template").unwrap_or_default();
    let mut fqdn = template
        .replace("{{random}}", random)
        .replace("{{domain}}", &host)
        .replace("{{pr_id}}", &pull_request_id.to_string());
    let mut schema = scheme;
    let lower = fqdn.to_lowercase();
    for s in ["http", "https"] {
        let prefix = format!("{s}://");
        if lower.starts_with(&prefix) {
            schema = s.to_owned();
            fqdn = fqdn[prefix.len()..].to_owned();
            break;
        }
    }
    let overrides = a
        .raw
        .get("domain_port_overrides")
        .and_then(|v| match v {
            Value::String(s) => serde_json::from_str::<Value>(s).ok(),
            other => Some(other.clone()),
        })
        .unwrap_or(Value::Null);
    let canonical = format!("{schema}://{host}{path}");
    let port = port.and_then(|p| p.parse::<i64>().ok()).or_else(|| {
        overrides
            .get(&canonical)
            .or_else(|| overrides.get(source_domain))
            .and_then(Value::as_i64)
    });
    (format!("{schema}://{fqdn}{path}"), port)
}

impl Jobs {
    /// `ApplicationPreview::where(...)->first()`.
    pub async fn find_preview(
        &self,
        application_id: i64,
        pull_request_id: i64,
    ) -> Result<Option<Record>, String> {
        let row: Option<String> = sqlx::query_scalar(
            "SELECT row_to_json(p)::text FROM application_previews p WHERE application_id = $1 AND pull_request_id = $2 AND deleted_at IS NULL LIMIT 1",
        )
        .bind(application_id)
        .bind(pull_request_id as i32)
        .fetch_optional(self.0.db.pool())
        .await
        .map_err(|e| e.to_string())?;
        row.map(|t| coolify_db::records::parse_row(&t).map_err(|e| e.to_string()))
            .transpose()
    }

    /// `ApplicationPreview::create([...])` with `generate_preview_fqdn()`, or the compose domains
    /// copied from the application (`generate_preview_fqdn_compose()` needs the preview parser).
    pub async fn create_preview(
        &self,
        a: &ApplicationRow,
        pull_request_id: i64,
        html_url: &str,
        git_type: &str,
    ) -> Result<(), String> {
        let now = Utc::now().naive_utc();
        let uuid = coolify_domain::new_public_id();
        let compose = a.str("build_pack") == Some("dockercompose");
        let (mut fqdn, mut overrides) = (None, None);
        if !compose && let Some(domain) = a.str("fqdn").filter(|d| !d.is_empty()) {
            let first = domain.split(',').next().unwrap_or(domain);
            let random = coolify_domain::new_public_id();
            let (url, port) = generated_preview_domain(a, first, pull_request_id, &random);
            overrides = port.map(|p| json!({url.clone(): p}).to_string());
            fqdn = Some(url);
        }
        sqlx::query(
            "INSERT INTO application_previews (uuid, git_type, application_id, pull_request_id, pull_request_html_url, docker_compose_domains, fqdn, domain_port_overrides, max_restart_count, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8::json, 0, $9, $9)",
        )
        .bind(&uuid)
        .bind(git_type)
        .bind(a.id)
        .bind(pull_request_id as i32)
        .bind(html_url)
        .bind(if compose { a.str("docker_compose_domains") } else { None })
        .bind(&fqdn)
        .bind(&overrides)
        .bind(now)
        .execute(self.0.db.pool())
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// `CleanupPreviewDeployment::run($application, $pullRequestId, $preview)`.
    pub async fn cleanup_preview(
        &self,
        a: &ApplicationRow,
        pull_request_id: i64,
        preview: &Record,
    ) {
        let db = &self.0.db;
        let Some(server_id) = a.server_id() else {
            return;
        };
        let Ok(Some(row)) = servers::find_by_id(db, server_id).await else {
            return;
        };
        let info = ServerInfo::from_row(&row);
        if !info.is_functional() {
            return;
        }
        let Ok(target) = info.target(self).await else {
            return;
        };
        let active: Vec<(i64, String)> = sqlx::query_as(
            "SELECT id, deployment_uuid FROM application_deployment_queues WHERE application_id = $1 AND pull_request_id = $2 AND status IN ('queued', 'in_progress')",
        )
        .bind(a.id.to_string())
        .bind(pull_request_id as i32)
        .fetch_all(db.pool())
        .await
        .unwrap_or_default();
        let now = Utc::now().naive_utc();
        for (id, uuid) in &active {
            let _ = apps::transition_status(db, *id, "cancelled-by-user", now).await;
            let entry = apps::LogEntry {
                command: None,
                output: "Deployment cancelled: Pull request closed.".into(),
                kind: "stderr",
                timestamp: now.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string(),
                hidden: false,
                batch: 1,
            };
            let _ = apps::append_log(db, *id, &[entry], now).await;
            let name = quote(uuid);
            let _ = self
                .remote()
                .run(
                    &target,
                    &[format!("docker rm -f {name}").as_str()],
                    RunOptions::op("application.preview.cleanup"),
                )
                .await;
        }
        let list = format!(
            "docker ps -a --filter label=coolify.applicationId={} --filter label=coolify.pullRequestId={pull_request_id} --format '{{{{.Names}}}}'",
            a.id
        );
        let names = self
            .remote()
            .run(
                &target,
                &[list.as_str()],
                RunOptions::op("application.preview.cleanup"),
            )
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        for name in names.lines().map(str::trim).filter(|n| !n.is_empty()) {
            let command = format!("docker rm -f {}", quote(name));
            let _ = self
                .remote()
                .run(
                    &target,
                    &[command.as_str()],
                    RunOptions::op("application.preview.cleanup"),
                )
                .await;
        }
        // DeleteResourceJob($preview): the preview ends up soft deleted, then force deleted.
        if let Some(id) = preview.get("id").and_then(Value::as_i64) {
            let _ = sqlx::query("DELETE FROM application_previews WHERE id = $1")
                .bind(id)
                .execute(db.pool())
                .await;
        }
    }

    /// `ApplicationPullRequestUpdateJob::dispatch(...)`.
    pub fn spawn_pull_request_update(
        &self,
        a: ApplicationRow,
        preview: Record,
        status: PullRequestStatus,
        deployment_uuid: String,
    ) {
        let jobs = self.clone();
        tokio::spawn(async move {
            jobs.update_pull_request(&a, &preview, status, Some(&deployment_uuid))
                .await;
        });
    }

    /// `ApplicationPullRequestUpdateJob`: creates, updates or deletes the comment on the pull
    /// request of a private GitHub App source. Errors stay inside like upstream's `catch`.
    pub async fn update_pull_request(
        &self,
        a: &ApplicationRow,
        preview: &Record,
        status: PullRequestStatus,
        deployment_uuid: Option<&str>,
    ) {
        if let Err(e) = self
            .pull_request_comment(a, preview, status, deployment_uuid)
            .await
        {
            tracing::debug!(application = %a.uuid, error = %e, "pull request update failed");
        }
    }

    async fn pull_request_comment(
        &self,
        a: &ApplicationRow,
        preview: &Record,
        status: PullRequestStatus,
        deployment_uuid: Option<&str>,
    ) -> Result<(), String> {
        const GITHUB: &str = r"App\Models\GithubApp";
        let db = &self.0.db;
        if a.str("source_type") != Some(GITHUB) {
            return Ok(());
        }
        let source = coolify_db::git_sources::source_row(
            db,
            "github_apps",
            a.int("source_id").unwrap_or(-1),
        )
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no source")?;
        if source.get("is_public").and_then(Value::as_bool) == Some(true) {
            return Ok(());
        }
        let enc = self.0.encrypter.as_ref().ok_or("no encrypter")?;
        let repository = a.str("git_repository").unwrap_or_default();
        let comment = preview
            .get("pull_request_issue_comment_id")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty());
        let pr = preview
            .get("pull_request_id")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if status == PullRequestStatus::Closed {
            let endpoint = format!(
                "/repos/{repository}/issues/comments/{}",
                comment.unwrap_or_default()
            );
            crate::github::api(db, enc, &source, reqwest::Method::DELETE, &endpoint, None).await?;
            return Ok(());
        }
        let name = a.str("name").unwrap_or_default();
        let mut body = pull_request_status_line(status, name, a.str("build_pack"), preview);
        let base = crate::notifications::Ctx::load(db).await.base_url;
        let project = a
            .project
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let env = a
            .environment
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let app_url = format!(
            "{base}/project/{project}/environment/{env}/application/{}",
            a.uuid
        );
        body.push_str(&format!(
            "[Open Build Logs]({app_url}/deployment/{}) | [Open Application Logs]({app_url}/logs)\n\n\n",
            deployment_uuid.unwrap_or_default()
        ));
        body.push_str(&format!(
            "Last updated at: {} CET",
            Utc::now().format("%Y-%m-%d %H:%M:%S")
        ));
        let data = json!({"body": body});
        if let Some(id) = comment {
            let endpoint = format!("/repos/{repository}/issues/comments/{id}");
            let (_, answer) = crate::github::api(
                db,
                enc,
                &source,
                reqwest::Method::PATCH,
                &endpoint,
                Some(&data),
            )
            .await?;
            if answer.get("message").and_then(Value::as_str) != Some("Not Found") {
                return Ok(());
            }
        }
        let endpoint = format!("/repos/{repository}/issues/{pr}/comments");
        let (_, answer) = crate::github::api(
            db,
            enc,
            &source,
            reqwest::Method::POST,
            &endpoint,
            Some(&data),
        )
        .await?;
        if let (Some(id), Some(preview_id)) = (
            answer.get("id").and_then(Value::as_i64),
            preview.get("id").and_then(Value::as_i64),
        ) {
            sqlx::query("UPDATE application_previews SET pull_request_issue_comment_id = $1, updated_at = $2 WHERE id = $3")
                .bind(id.to_string())
                .bind(Utc::now().naive_utc())
                .bind(preview_id)
                .execute(db.pool())
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
