//! Events to alerts. Payloads are the data of Coolify's notifications (docs/PLUGINS.md, "Events"); anything
//! missing reads as empty, so a malformed payload gives a vaguer alert, never a crash.

use serde_json::Value;

use crate::text::{field, hash, key, line, tail};

/// What a team chooses to send, per category (one setting each).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Deployments,
    Backups,
    Tasks,
    Servers,
    Containers,
    Certificates,
}

impl Category {
    /// The setting that says what to send for this category.
    pub fn setting(self) -> &'static str {
        match self {
            Self::Deployments => "send_deployments",
            Self::Backups => "send_backups",
            Self::Tasks => "send_tasks",
            Self::Servers => "send_servers",
            Self::Containers => "send_containers",
            Self::Certificates => "send_certificates",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Deployments => "deployments",
            Self::Backups => "backups",
            Self::Tasks => "scheduled-tasks",
            Self::Servers => "servers",
            Self::Containers => "containers",
            Self::Certificates => "certificates",
        }
    }

    pub const ALL: [Category; 6] = [
        Self::Deployments,
        Self::Backups,
        Self::Tasks,
        Self::Servers,
        Self::Containers,
        Self::Certificates,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Critical,
    Error,
    Warning,
    Info,
    /// Good news: a success or a recovery.
    Success,
}

impl Severity {
    /// PagerDuty's `payload.severity`.
    pub fn pagerduty(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info | Self::Success => "info",
        }
    }

    /// Opsgenie's `priority`.
    pub fn opsgenie(self) -> &'static str {
        match self {
            Self::Critical => "P1",
            Self::Error => "P2",
            Self::Warning => "P3",
            Self::Info | Self::Success => "P5",
        }
    }

    /// ntfy tags, shown as emoji.
    pub fn ntfy_tag(self) -> &'static str {
        match self {
            Self::Critical => "rotating_light",
            Self::Error => "x",
            Self::Warning => "warning",
            Self::Info => "information_source",
            Self::Success => "white_check_mark",
        }
    }

    pub fn is_problem(self) -> bool {
        matches!(self, Self::Critical | Self::Error | Self::Warning)
    }
}

/// What an alert does to incidents in PagerDuty and Opsgenie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Incident {
    /// Opens (or adds to) the incident `key`; a later [`Incident::Resolve`] of the same key closes it.
    Open,
    /// Opens an incident nothing in Coolify resolves later (disk usage, expiring certificates...): it is
    /// closed by hand.
    OpenOnly,
    /// Resolves the incident `key` if this plugin opened it.
    Resolve,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    pub category: Category,
    pub severity: Severity,
    pub incident: Incident,
    /// The dedup key and alias, `coolify-rs-...`: the same resource always gets the same key.
    pub key: String,
    /// One line, at most 120 characters.
    pub title: String,
    /// A few lines; command output is cut to its last 400 characters.
    pub body: String,
    pub link: Option<String>,
    /// The server or resource the alert is about, for PagerDuty's `source` and Opsgenie's `entity`.
    pub source: String,
    /// Extra fields for PagerDuty's `custom_details` and Opsgenie's `details`.
    pub details: Vec<(String, String)>,
}

/// How much command output an alert carries: its end, where the error is.
pub const OUTPUT: usize = 400;

/// Every kind the plugin subscribes to; the manifest lists the same.
pub const KINDS: &[&str] = &[
    "deployment.succeeded",
    "deployment.failed",
    "backup.succeeded",
    "backup.succeeded_with_s3_warning",
    "backup.failed",
    "backup.missing",
    "volume_backup.succeeded",
    "volume_backup.failed",
    "volume_backup.missing",
    "volume_backup.recovery_failed",
    "scheduled_task.succeeded",
    "scheduled_task.failed",
    "server.unreachable",
    "server.reachable",
    "server.high_disk_usage",
    "server.docker_cleanup_failed",
    "container.restart_limit_reached",
    "ssl.certificates_expiring",
];

struct Builder {
    category: Category,
    severity: Severity,
    incident: Incident,
    key: String,
    title: String,
    body: String,
    link: Option<String>,
    source: String,
    details: Vec<(String, String)>,
}

impl Builder {
    fn new(category: Category, severity: Severity, incident: Incident, key: String) -> Self {
        Self {
            category,
            severity,
            incident,
            key,
            title: String::new(),
            body: String::new(),
            link: None,
            source: "coolify-rs".into(),
            details: Vec::new(),
        }
    }
    fn title(mut self, t: String) -> Self {
        self.title = line(&t, 120);
        self
    }
    fn body(mut self, b: String) -> Self {
        self.body = b;
        self
    }
    fn link(mut self, l: Option<String>) -> Self {
        self.link = l.filter(|l| l.starts_with("https://") || l.starts_with("http://"));
        self
    }
    fn source(mut self, s: &str) -> Self {
        if !s.is_empty() {
            self.source = line(s, 100);
        }
        self
    }
    fn detail(mut self, k: &str, v: &str) -> Self {
        if !v.is_empty() {
            self.details.push((k.to_owned(), line(v, 200)));
        }
        self
    }
    fn done(self) -> Alert {
        Alert {
            category: self.category,
            severity: self.severity,
            incident: self.incident,
            key: self.key,
            title: self.title,
            body: self.body,
            link: self.link,
            source: self.source,
            details: self.details,
        }
    }
}

/// A page of Coolify, when the team gave its URL.
fn page(base: Option<&str>, path: String) -> Option<String> {
    base.map(|b| format!("{}{path}", b.trim_end_matches('/')))
}

/// `name` or a placeholder.
fn or<'a>(s: &'a str, fallback: &'a str) -> &'a str {
    if s.trim().is_empty() { fallback } else { s }
}

/// `{project}/{environment}`, or what is known of it.
fn place(p: &Value) -> String {
    let project = field(p, &["project_name"]);
    let env = field(p, &["environment_name"]);
    match (project.is_empty(), env.is_empty()) {
        (false, false) => format!("{project}/{env}"),
        (false, true) => project.into_owned(),
        _ => String::new(),
    }
}

/// The first of a comma separated list of domains, as Coolify links it.
fn first_domain(fqdn: &str) -> Option<String> {
    fqdn.split(',')
        .map(str::trim)
        .find(|d| !d.is_empty())
        .map(str::to_owned)
}

/// The alert for an event, or `None` for kinds this plugin does not handle. `coolify_url` is the team's
/// "Coolify URL" setting, used to link to Coolify's own pages.
pub fn alert(kind: &str, p: &Value, coolify_url: Option<&str>) -> Option<Alert> {
    use Category::*;
    use Incident::*;
    use Severity::*;
    let f = |path: &[&str]| field(p, path).into_owned();
    Some(match kind {
        "deployment.failed" | "deployment.succeeded" => {
            let ok = kind == "deployment.succeeded";
            let app = f(&["application_name"]);
            let app_uuid = f(&["application_uuid"]);
            let pr = f(&["preview", "pull_request_id"]);
            let mut k = vec!["coolify-rs", "deployment", app_uuid.as_str()];
            if !pr.is_empty() && pr != "0" {
                k.extend(["pr", pr.as_str()]);
            }
            let what = if pr.is_empty() || pr == "0" {
                or(&app, "an application").to_owned()
            } else {
                format!("{} (pull request #{pr})", or(&app, "an application"))
            };
            let at = place(p);
            let at = if at.is_empty() {
                String::new()
            } else {
                format!(" in {at}")
            };
            let mut body = if ok {
                format!("{what}{at} was deployed.")
            } else {
                format!("The deployment of {what}{at} failed.")
            };
            let commit = f(&["commit", "sha"]);
            if !commit.is_empty() {
                let short: String = commit.chars().take(7).collect();
                let message = line(&f(&["commit", "message"]), 80);
                body.push_str(&format!("\nCommit {short}"));
                if !message.is_empty() {
                    body.push_str(&format!(": {message}"));
                }
            }
            let fqdn = match f(&["preview", "fqdn"]) {
                d if !d.is_empty() => d,
                _ => f(&["fqdn"]),
            };
            let link = page(
                coolify_url,
                format!(
                    "/project/{}/environment/{}/application/{}/deployment/{}",
                    f(&["project_uuid"]),
                    f(&["environment_uuid"]),
                    app_uuid,
                    f(&["deployment_uuid"])
                ),
            )
            .or_else(|| first_domain(&fqdn));
            Builder::new(
                Deployments,
                if ok { Success } else { Error },
                if ok { Resolve } else { Open },
                key(&k),
            )
            .title(if ok {
                format!("Deployed: {}", or(&app, "application"))
            } else {
                format!("Deployment failed: {}", or(&app, "application"))
            })
            .body(body)
            .link(link)
            .source(&app)
            .detail("project", &f(&["project_name"]))
            .detail("environment", &f(&["environment_name"]))
            .detail("application_uuid", &app_uuid)
            .detail("deployment_uuid", &f(&["deployment_uuid"]))
            .done()
        }
        "backup.failed" | "backup.succeeded" | "backup.succeeded_with_s3_warning" => {
            // `backup.succeeded` carries the database at the top level, the others under `backup`.
            let b = if kind == "backup.succeeded" {
                p
            } else {
                p.get("backup").unwrap_or(&Value::Null)
            };
            let name = field(b, &["name"]).into_owned();
            let uuid = field(b, &["uuid"]).into_owned();
            let dbname = field(b, &["database_name"]).into_owned();
            let what = if dbname.is_empty() || dbname == name {
                or(&name, "a database").to_owned()
            } else {
                format!("{dbname} on {}", or(&name, "a database"))
            };
            let link = page(
                coolify_url,
                format!(
                    "/project/{}/environment/{}/database/{uuid}",
                    field(b, &["project_uuid"]),
                    field(b, &["environment_uuid"])
                ),
            );
            let k = key(&["coolify-rs", "backup", &uuid, &dbname]);
            let (severity, incident, title, body) = match kind {
                "backup.succeeded" => (
                    Success,
                    Resolve,
                    format!("Backup succeeded: {}", or(&name, "database")),
                    format!("The backup of {what} succeeded."),
                ),
                "backup.failed" => (
                    Error,
                    Open,
                    format!("Backup failed: {}", or(&name, "database")),
                    format!(
                        "The backup of {what} failed.\n\n{}",
                        tail(&f(&["output"]), OUTPUT)
                    ),
                ),
                _ => (
                    Warning,
                    Open,
                    format!("Backup not uploaded to S3: {}", or(&name, "database")),
                    format!(
                        "The backup of {what} succeeded, but uploading it to S3 failed.\n\n{}",
                        tail(&f(&["s3_error"]), OUTPUT)
                    ),
                ),
            };
            Builder::new(Backups, severity, incident, k)
                .title(title)
                .body(body.trim_end().to_owned())
                .link(link)
                .source(&name)
                .detail("database", &dbname)
                .detail("frequency", &field(b, &["frequency"]))
                .done()
        }
        "backup.missing" => {
            let name = f(&["database_name"]);
            let days = f(&["days"]);
            let last = f(&["last_execution_at"]);
            let mut body = format!("No backup of {} has run", or(&name, "a database"));
            if !days.is_empty() {
                body.push_str(&format!(" in the last {days} days"));
            }
            body.push('.');
            body.push_str(&match last.is_empty() {
                true => " It never ran.".to_owned(),
                false => format!(" The last one ran at {last}."),
            });
            Builder::new(
                Backups,
                Error,
                OpenOnly,
                key(&["coolify-rs", "backup-schedule", &f(&["backup_uuid"])]),
            )
            .title(format!("Backup missing: {}", or(&name, "database")))
            .body(body)
            .source(&name)
            .detail("backup_uuid", &f(&["backup_uuid"]))
            .done()
        }
        "volume_backup.succeeded"
        | "volume_backup.failed"
        | "volume_backup.missing"
        | "volume_backup.recovery_failed" => {
            let b = p.get("backup").filter(|b| b.is_object());
            let empty = Value::Null;
            let bv = b.unwrap_or(&empty);
            let target = or(&field(bv, &["target"]), "storage").to_owned();
            let resource = or(&field(bv, &["resource_name"]), "Unknown resource").to_owned();
            let url = field(bv, &["url"]).into_owned();
            // The payload names no schedule: the resource (its page's uuid, else its name) and the target
            // are what stays the same from one run to the next.
            let resource_id = url
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or(&resource)
                .to_owned();
            let k = match b {
                Some(_) => key(&["coolify-rs", "volume-backup", &resource_id, &target]),
                None => key(&["coolify-rs", "volume-recovery", &f(&["execution_uuid"])]),
            };
            let what = format!("{target} ({resource})");
            let (severity, incident, title, body) = match kind {
                "volume_backup.succeeded" => (
                    Success,
                    Resolve,
                    format!("Storage backup succeeded: {target}"),
                    format!("The backup of {what} succeeded."),
                ),
                "volume_backup.failed" => (
                    Error,
                    Open,
                    format!("Storage backup failed: {target}"),
                    format!(
                        "The backup of {what} failed.\n\n{}",
                        tail(&f(&["output"]), OUTPUT)
                    ),
                ),
                "volume_backup.missing" => {
                    let days = f(&["days"]);
                    let mut body = format!("No backup of {what} has run");
                    if !days.is_empty() {
                        body.push_str(&format!(" in the last {days} days"));
                    }
                    body.push('.');
                    (
                        Error,
                        Open,
                        format!("Storage backup missing: {target}"),
                        body,
                    )
                }
                _ => {
                    let reason = match f(&["recovery_error"]).as_str() {
                        "s3_auth" => "S3 credentials were rejected",
                        "s3_bucket" => "S3 bucket is missing",
                        "remote_command" => "A command on the server failed",
                        _ => "Unknown error",
                    };
                    let stopped = p
                        .get("containers_stopped")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let mut body = format!("Recovery after the backup of {what} failed: {reason}.");
                    if stopped {
                        body.push_str(" The containers stopped for the backup are still stopped.");
                    }
                    (
                        Error,
                        Open,
                        format!("Storage backup recovery failed: {target}"),
                        body,
                    )
                }
            };
            Builder::new(Backups, severity, incident, k)
                .title(title)
                .body(body.trim_end().to_owned())
                .link(Some(url))
                .source(&resource)
                .detail("frequency", &field(bv, &["frequency"]))
                .done()
        }
        "scheduled_task.failed" | "scheduled_task.succeeded" => {
            let ok = kind == "scheduled_task.succeeded";
            let name = f(&["name"]);
            let uuid = f(&["uuid"]);
            let link = p.get("resource").filter(|r| r.is_object()).and_then(|r| {
                let segment = if r.get("is_application").and_then(Value::as_bool) == Some(true) {
                    "application"
                } else {
                    "service"
                };
                page(
                    coolify_url,
                    format!(
                        "/project/{}/environment/{}/{segment}/{}/tasks/{uuid}",
                        field(r, &["project_uuid"]),
                        field(r, &["environment_uuid"]),
                        field(r, &["uuid"])
                    ),
                )
            });
            let body = if ok {
                format!("The scheduled task {} succeeded.", or(&name, "(unnamed)"))
            } else {
                format!(
                    "The scheduled task {} failed.\n\n{}",
                    or(&name, "(unnamed)"),
                    tail(&f(&["output"]), OUTPUT)
                )
            };
            Builder::new(
                Tasks,
                if ok { Success } else { Error },
                if ok { Resolve } else { Open },
                key(&["coolify-rs", "task", &uuid]),
            )
            .title(if ok {
                format!("Scheduled task succeeded: {}", or(&name, "task"))
            } else {
                format!("Scheduled task failed: {}", or(&name, "task"))
            })
            .body(body.trim_end().to_owned())
            .link(link)
            .source(&name)
            .detail("task_uuid", &uuid)
            .done()
        }
        "server.unreachable"
        | "server.reachable"
        | "server.high_disk_usage"
        | "server.docker_cleanup_failed" => {
            let name = f(&["server", "name"]);
            let uuid = f(&["server", "uuid"]);
            let server = or(&name, "a server").to_owned();
            let link = page(coolify_url, format!("/server/{uuid}"));
            let b = match kind {
                "server.unreachable" => Builder::new(
                    Servers,
                    Critical,
                    Open,
                    key(&["coolify-rs", "server", &uuid]),
                )
                .title(format!("Server unreachable: {server}"))
                .body(format!(
                    "coolify-rs cannot reach the server {server}. Its resources may be down."
                )),
                "server.reachable" => Builder::new(
                    Servers,
                    Success,
                    Resolve,
                    key(&["coolify-rs", "server", &uuid]),
                )
                .title(format!("Server reachable again: {server}"))
                .body(format!("The server {server} is reachable again.")),
                "server.high_disk_usage" => {
                    let usage = f(&["disk_usage"]);
                    let threshold = f(&["threshold"]);
                    Builder::new(
                        Servers,
                        Warning,
                        OpenOnly,
                        key(&["coolify-rs", "disk", &uuid]),
                    )
                    .title(format!("High disk usage on {server}: {usage}%"))
                    .body(format!(
                        "Disk usage on {server} is {usage}%, above the threshold of {threshold}%."
                    ))
                }
                _ => Builder::new(
                    Servers,
                    Warning,
                    OpenOnly,
                    key(&["coolify-rs", "docker-cleanup", &uuid]),
                )
                .title(format!("Docker cleanup failed on {server}"))
                .body(
                    format!(
                        "Docker cleanup on {server} failed.\n\n{}",
                        tail(&f(&["message"]), OUTPUT)
                    )
                    .trim_end()
                    .to_owned(),
                ),
            };
            b.link(link)
                .source(&name)
                .detail("server_uuid", &uuid)
                .done()
        }
        "container.restart_limit_reached" => {
            let name = f(&["resource_name"]);
            let segment = match f(&["kind"]).as_str() {
                "service" => "service",
                "database" => "database",
                _ => "application",
            };
            let link = page(
                coolify_url,
                format!(
                    "/project/{}/environment/{}/{segment}/{}",
                    f(&["project_uuid"]),
                    f(&["environment_uuid"]),
                    f(&["url_uuid"])
                ),
            )
            .or_else(|| first_domain(&f(&["fqdn"])));
            let at = place(p);
            let at = if at.is_empty() {
                String::new()
            } else {
                format!(" in {at}")
            };
            Builder::new(
                Containers,
                Error,
                OpenOnly,
                key(&["coolify-rs", "restart-limit", &f(&["resource_uuid"])]),
            )
            .title(format!("Restart limit reached: {}", or(&name, "container")))
            .body(format!(
                "{}{at} restarted {} times and reached its restart limit of {}.",
                or(&name, "A container"),
                f(&["restart_count"]),
                f(&["max_restart_count"])
            ))
            .link(link)
            .source(&name)
            .detail("resource_uuid", &f(&["resource_uuid"]))
            .done()
        }
        "ssl.certificates_expiring" => {
            let resources: Vec<&Value> = p
                .get("resources")
                .and_then(Value::as_array)
                .map(|a| a.iter().collect())
                .unwrap_or_default();
            let mut names: Vec<String> = resources
                .iter()
                .map(|r| field(r, &["name"]).into_owned())
                .filter(|n| !n.is_empty())
                .collect();
            names.dedup();
            let mut uuids: Vec<String> = resources
                .iter()
                .map(|r| field(r, &["uuid"]).into_owned())
                .collect();
            uuids.sort();
            let uuid_refs: Vec<&str> = uuids.iter().map(String::as_str).collect();
            let link = resources.first().and_then(|r| {
                page(
                    coolify_url,
                    format!(
                        "/project/{}/environment/{}/database/{}",
                        field(r, &["project_uuid"]),
                        field(r, &["environment_uuid"]),
                        field(r, &["uuid"])
                    ),
                )
            });
            let list = if names.is_empty() {
                "some databases".to_owned()
            } else {
                line(&names.join(", "), 600)
            };
            Builder::new(
                Certificates,
                Warning,
                OpenOnly,
                key(&["coolify-rs", "ssl", &hash(&uuid_refs)]),
            )
            .title(if names.is_empty() {
                "SSL certificates expiring".to_owned()
            } else {
                format!("SSL certificates expiring: {}", line(&names.join(", "), 80))
            })
            .body(format!(
                "The SSL certificates of {list} expire soon. Regenerate them in Coolify."
            ))
            .link(link)
            .done()
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BASE: Option<&str> = Some("https://coolify.example.com/");

    fn deployment() -> Value {
        json!({
            "application_name": "api", "application_uuid": "app1", "deployment_uuid": "dep1",
            "project_name": "Shop", "project_uuid": "p1", "environment_name": "production",
            "environment_uuid": "e1", "fqdn": "https://api.example.com,https://www.example.com",
            "preview": null, "commit": {"sha": "0123456789abcdef", "message": "Fix the cart\nand more", "url": null, "branch": "main"}
        })
    }

    #[test]
    fn deployments() {
        let failed = alert("deployment.failed", &deployment(), BASE).unwrap();
        assert_eq!(failed.category, Category::Deployments);
        assert_eq!(failed.severity, Severity::Error);
        assert_eq!(failed.incident, Incident::Open);
        assert_eq!(failed.key, "coolify-rs-deployment-app1");
        assert_eq!(failed.title, "Deployment failed: api");
        assert_eq!(
            failed.body,
            "The deployment of api in Shop/production failed.\nCommit 0123456: Fix the cart and more"
        );
        assert_eq!(
            failed.link.as_deref(),
            Some(
                "https://coolify.example.com/project/p1/environment/e1/application/app1/deployment/dep1"
            )
        );
        assert_eq!(failed.source, "api");

        // Without a Coolify URL, the application's own first domain.
        let ok = alert("deployment.succeeded", &deployment(), None).unwrap();
        assert_eq!(ok.incident, Incident::Resolve);
        assert_eq!(ok.key, failed.key);
        assert_eq!(ok.title, "Deployed: api");
        assert_eq!(ok.link.as_deref(), Some("https://api.example.com"));

        // A preview has its own key.
        let mut preview = deployment();
        preview["preview"] = json!({"pull_request_id": 42, "fqdn": "https://pr-42.example.com"});
        let pr = alert("deployment.failed", &preview, None).unwrap();
        assert_eq!(pr.key, "coolify-rs-deployment-app1-pr-42");
        assert!(
            pr.body
                .starts_with("The deployment of api (pull request #42) in Shop/production failed.")
        );
        assert_eq!(pr.link.as_deref(), Some("https://pr-42.example.com"));
    }

    #[test]
    fn backups_keep_only_the_end_of_the_output() {
        let output = format!("{}\nERROR: disk full", "x".repeat(5000));
        let p = json!({
            "backup": {"name": "pg", "uuid": "db1", "project_uuid": "p1", "environment_uuid": "e1",
                       "database_name": "shop", "frequency": "0 * * * *"},
            "output": output,
        });
        let a = alert("backup.failed", &p, BASE).unwrap();
        assert_eq!(a.key, "coolify-rs-backup-db1-shop");
        assert_eq!(a.title, "Backup failed: pg");
        assert!(
            a.body.starts_with("The backup of shop on pg failed.\n\n…"),
            "{}",
            a.body
        );
        assert!(a.body.ends_with("ERROR: disk full"));
        assert!(a.body.chars().count() < 460, "{}", a.body.len());
        assert_eq!(
            a.link.as_deref(),
            Some("https://coolify.example.com/project/p1/environment/e1/database/db1")
        );

        // A success carries the database at the top level and resolves the same key.
        let ok = alert("backup.succeeded", &p["backup"], BASE).unwrap();
        assert_eq!(
            (ok.key.as_str(), ok.incident),
            (a.key.as_str(), Incident::Resolve)
        );

        let s3 = json!({"backup": p["backup"], "s3_error": "AccessDenied", "s3_uuid": "s3"});
        let w = alert("backup.succeeded_with_s3_warning", &s3, None).unwrap();
        assert_eq!(
            (w.severity, w.incident),
            (Severity::Warning, Incident::Open)
        );
        assert!(
            w.body
                .ends_with("uploading it to S3 failed.\n\nAccessDenied"),
            "{}",
            w.body
        );

        let missing = json!({"database_name": "pg", "backup_uuid": "b1", "days": 2, "last_execution_at": null});
        let m = alert("backup.missing", &missing, None).unwrap();
        assert_eq!(m.key, "coolify-rs-backup-schedule-b1");
        assert_eq!(m.incident, Incident::OpenOnly);
        assert_eq!(
            m.body,
            "No backup of pg has run in the last 2 days. It never ran."
        );
    }

    #[test]
    fn volume_backups_share_a_key_per_resource_and_target() {
        let backup = json!({"resource_name": "files", "target": "Volume data", "frequency": "daily",
                            "url": "https://coolify.example.com/project/p/environment/e/application/app9"});
        let failed = alert(
            "volume_backup.failed",
            &json!({"backup": backup, "output": "boom"}),
            None,
        )
        .unwrap();
        assert_eq!(failed.key, "coolify-rs-volume-backup-app9-volume-data");
        assert_eq!(
            failed.body,
            "The backup of Volume data (files) failed.\n\nboom"
        );
        assert_eq!(failed.link.as_deref(), backup["url"].as_str());
        let missing = alert(
            "volume_backup.missing",
            &json!({"backup": backup, "backup_uuid": "s1", "days": 3, "last_execution_at": null}),
            None,
        )
        .unwrap();
        assert_eq!(missing.key, failed.key);
        let ok = alert(
            "volume_backup.succeeded",
            &json!({"backup": backup, "warning": null}),
            None,
        )
        .unwrap();
        assert_eq!(
            (ok.key.as_str(), ok.incident),
            (failed.key.as_str(), Incident::Resolve)
        );
        let recovery = alert(
            "volume_backup.recovery_failed",
            &json!({"backup": null, "execution_uuid": "x1", "recovery_error": "s3_auth", "containers_stopped": true}),
            None,
        )
        .unwrap();
        assert_eq!(recovery.key, "coolify-rs-volume-recovery-x1");
        assert_eq!(
            recovery.body,
            "Recovery after the backup of storage (Unknown resource) failed: S3 credentials were rejected. The containers stopped for the backup are still stopped."
        );
    }

    #[test]
    fn tasks_servers_containers_and_certificates() {
        let task = json!({"name": "cleanup", "uuid": "t1", "output": "exit 1",
                          "resource": {"is_application": false, "uuid": "s1", "project_uuid": "p", "environment_uuid": "e"}});
        let t = alert("scheduled_task.failed", &task, BASE).unwrap();
        assert_eq!(t.key, "coolify-rs-task-t1");
        assert_eq!(
            t.link.as_deref(),
            Some("https://coolify.example.com/project/p/environment/e/service/s1/tasks/t1")
        );
        assert_eq!(t.body, "The scheduled task cleanup failed.\n\nexit 1");

        let server = json!({"server": {"id": 3, "name": "web-1", "uuid": "srv1"}});
        let down = alert("server.unreachable", &server, BASE).unwrap();
        assert_eq!(
            (down.severity, down.incident),
            (Severity::Critical, Incident::Open)
        );
        assert_eq!(down.key, "coolify-rs-server-srv1");
        assert_eq!(down.source, "web-1");
        assert_eq!(
            down.link.as_deref(),
            Some("https://coolify.example.com/server/srv1")
        );
        let up = alert("server.reachable", &server, None).unwrap();
        assert_eq!(
            (up.key.as_str(), up.incident),
            (down.key.as_str(), Incident::Resolve)
        );
        assert_eq!(up.link, None);

        let mut disk = server.clone();
        disk["disk_usage"] = json!(91);
        disk["threshold"] = json!(80);
        let d = alert("server.high_disk_usage", &disk, None).unwrap();
        assert_eq!(d.title, "High disk usage on web-1: 91%");
        assert_eq!(d.incident, Incident::OpenOnly);

        let restart = json!({"kind": "database", "resource_name": "redis", "resource_uuid": "r1", "url_uuid": "r1",
                             "project_name": "P", "project_uuid": "p", "environment_name": "E", "environment_uuid": "e",
                             "fqdn": null, "restart_count": 10, "max_restart_count": 10});
        let r = alert("container.restart_limit_reached", &restart, BASE).unwrap();
        assert_eq!(
            r.body,
            "redis in P/E restarted 10 times and reached its restart limit of 10."
        );
        assert_eq!(
            r.link.as_deref(),
            Some("https://coolify.example.com/project/p/environment/e/database/r1")
        );

        let ssl = json!({"resources": [{"name": "pg", "uuid": "b", "project_uuid": "p", "environment_uuid": "e"},
                                       {"name": "mysql", "uuid": "a", "project_uuid": "p", "environment_uuid": "e"}]});
        let s = alert("ssl.certificates_expiring", &ssl, None).unwrap();
        assert_eq!(s.title, "SSL certificates expiring: pg, mysql");
        assert!(s.key.starts_with("coolify-rs-ssl-"));
        assert_eq!(s.category, Category::Certificates);
    }

    #[test]
    fn malformed_payloads_still_give_an_alert_and_unknown_kinds_none() {
        for kind in KINDS {
            for payload in [
                json!({}),
                json!(null),
                json!({"backup": 3, "server": "x", "resources": {}}),
            ] {
                let a = alert(kind, &payload, None).unwrap_or_else(|| panic!("{kind}"));
                assert!(!a.title.is_empty() && !a.key.is_empty(), "{kind}: {a:?}");
            }
        }
        assert_eq!(alert("general", &json!({}), None), None);
    }
}
