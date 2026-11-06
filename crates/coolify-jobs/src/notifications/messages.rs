//! The notification classes of `app/Notifications`: what each one sends on each channel.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::Ctx;
use super::php::{php_string, truthy, ucfirst};
use super::views;

/// `App\Notifications\Dto\DiscordMessage`.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscordMessage {
    pub title: String,
    pub description: String,
    pub color: i64,
    pub is_critical: bool,
    pub fields: Vec<(String, String, bool)>,
}

impl DiscordMessage {
    const SUCCESS: i64 = 0xa1ffa5;
    const WARNING: i64 = 0xffa743;
    const ERROR: i64 = 0xff705f;
    const INFO: i64 = 0x4f545c;

    fn new(title: impl Into<String>, description: impl Into<String>, color: i64) -> Self {
        Self {
            title: title.into(),
            description: description.into(),
            color,
            is_critical: false,
            fields: Vec::new(),
        }
    }

    fn critical(mut self) -> Self {
        self.is_critical = true;
        self
    }

    fn field(&mut self, name: impl Into<String>, value: impl Into<String>, inline: bool) {
        self.fields.push((name.into(), value.into(), inline));
    }

    /// `toPayload()`. The `Time` field holds `<t:{timestamp}:R>`.
    pub fn to_payload(&self, ctx: &Ctx, timestamp: i64) -> Value {
        let footer = if ctx.cloud {
            "Coolify Cloud".to_owned()
        } else {
            format!("Coolify v{}", ctx.version)
        };
        let mut fields: Vec<Value> = self
            .fields
            .iter()
            .map(|(n, v, i)| json!({"name": n, "value": v, "inline": i}))
            .collect();
        fields.push(json!({"name": "Time", "value": format!("<t:{timestamp}:R>"), "inline": true}));
        let mut payload = json!({
            "embeds": [{
                "title": self.title,
                "description": self.description,
                "color": self.color,
                "fields": fields,
                "footer": {"text": footer},
            }],
        });
        if self.is_critical {
            payload["content"] = json!("@here");
        }
        payload
    }
}

/// `App\Notifications\Dto\SlackMessage`.
#[derive(Debug, Clone, PartialEq)]
pub struct SlackMessage {
    pub title: String,
    pub description: String,
    pub color: &'static str,
}

const SLACK_INFO: &str = "#0099ff";
const SLACK_ERROR: &str = "#ff0000";
const SLACK_SUCCESS: &str = "#00ff00";
const SLACK_WARNING: &str = "#ffa500";

fn slack(
    title: impl Into<String>,
    description: impl Into<String>,
    color: &'static str,
) -> SlackMessage {
    SlackMessage {
        title: title.into(),
        description: description.into(),
        color,
    }
}

/// `App\Notifications\Dto\PushoverMessage`. Buttons are kept as upstream builds them: an
/// entry without `url` gives an empty link (`HighDiskUsage` passes a map of strings).
#[derive(Debug, Clone, PartialEq)]
pub struct PushoverMessage {
    pub title: String,
    pub message: String,
    pub buttons: Vec<Value>,
    pub level: &'static str,
}

/// `data_get($button, 'url')` and `data_get($button, 'text', 'Click here')`, the replacement of
/// `http://localhost` included.
pub(super) fn button_parts(button: &Value, app_url: &str) -> (String, Option<String>) {
    let text = match button.get("text") {
        Some(Value::Null) | None => "Click here".to_owned(),
        Some(v) => php_string(v),
    };
    let url = button
        .get("url")
        .filter(|v| !v.is_null())
        .map(php_string)
        .map(|u| {
            if truthy(Some(&u)) && u.contains("http://localhost") {
                u.replace("http://localhost", app_url)
            } else {
                u
            }
        });
    (text, url)
}

impl PushoverMessage {
    fn new(title: impl Into<String>, level: &'static str, message: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            buttons: Vec::new(),
            level,
        }
    }

    fn with_buttons(mut self, buttons: Vec<Value>) -> Self {
        self.buttons = buttons;
        self
    }

    fn icon(&self) -> &'static str {
        match self.level {
            "error" => "❌",
            "success" => "✅ ",
            "warning" => "⚠️",
            _ => "ℹ️",
        }
    }

    /// `toPayload($token, $user)`.
    pub fn to_payload(&self, ctx: &Ctx, token: &str, user: &str) -> Value {
        let mut message = self.message.clone();
        for b in &self.buttons {
            let (text, url) = button_parts(b, &ctx.app_url);
            message.push_str(&format!(
                "&nbsp;<a href='{}'>{text}</a>",
                url.unwrap_or_default()
            ));
        }
        json!({
            "token": token,
            "user": user,
            "title": format!("{} {}", self.icon(), self.title),
            "message": message,
            "html": 1,
        })
    }
}

/// `toTelegram()`.
#[derive(Debug, Clone, PartialEq)]
pub struct TelegramMessage {
    pub message: String,
    pub buttons: Vec<Value>,
}

fn telegram(message: impl Into<String>) -> TelegramMessage {
    TelegramMessage {
        message: message.into(),
        buttons: Vec::new(),
    }
}

fn button(text: &str, url: &str) -> Value {
    json!({"text": text, "url": url})
}

/// `toMail()`: the subject and `render()`.
#[derive(Debug, Clone, PartialEq)]
pub struct MailMessage {
    pub subject: String,
    pub html: String,
}

fn mail(subject: impl Into<String>, html: String) -> Option<MailMessage> {
    Some(MailMessage {
        subject: subject.into(),
        html,
    })
}

/// `IncludesDeploymentCommitDetails`: the stored plain strings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CommitDetails {
    pub sha: Option<String>,
    pub message: Option<String>,
    pub url: Option<String>,
    pub branch: Option<String>,
}

impl CommitDetails {
    /// `resolveDeploymentCommitDetails($application, $deployment, $preview)`. `commit_url` turns a
    /// commit into `$application->gitCommitLink($commit)`.
    pub fn resolve(
        commit: Option<&str>,
        commit_message: Option<&str>,
        git_branch: Option<&str>,
        is_preview: bool,
        commit_url: impl Fn(&str) -> Option<String>,
    ) -> Self {
        let mut d = Self::default();
        let commit = super::php::php_trim(commit.unwrap_or_default());
        if (7..=64).contains(&commit.len()) && commit.chars().all(|c| c.is_ascii_hexdigit()) {
            d.sha = Some(commit.to_owned());
            d.url = commit_url(commit);
        }
        let message = super::php::php_trim(commit_message.unwrap_or_default());
        if !message.is_empty() {
            d.message = Some(super::php::str_limit(message, 1000));
        }
        if !is_preview && let Some(b) = git_branch.filter(|b| !super::php::php_trim(b).is_empty()) {
            d.branch = Some(b.to_owned());
        }
        d
    }
}

/// `Application::gitCommitLink($link)`.
pub fn git_commit_link(
    source_html_url: Option<&str>,
    git_repository: Option<&str>,
    git_branch: Option<&str>,
    commit: &str,
) -> Option<String> {
    if let (Some(html), Some(repo), Some(_)) = (source_html_url, git_repository, git_branch) {
        let path = if html.contains("bitbucket") {
            "commits"
        } else {
            "commit"
        };
        return Some(format!("{html}/{repo}/{path}/{commit}"));
    }
    let repo = git_repository?;
    let repo = crate::github::ssh_git_url_to_https(repo).unwrap_or_else(|| repo.to_owned());
    let (scheme, rest) = repo.split_once("://")?;
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    // `Url::fromString(...)->withUserInfo('')`.
    let trimmed = rest.strip_suffix(".git").unwrap_or(rest);
    let (authority, tail) = match trimmed.find(['/', '?', '#']) {
        Some(i) => trimmed.split_at(i),
        None => (trimmed, ""),
    };
    let host = authority.rsplit('@').next()?;
    if host.is_empty() {
        return None;
    }
    let path = tail.split(['?', '#']).next().unwrap_or_default();
    let path = if path.ends_with('/') {
        path.to_owned()
    } else {
        format!("{path}/")
    };
    let segment = if repo.contains("bitbucket") {
        "commits"
    } else {
        "commit"
    };
    Some(format!("{scheme}://{host}{path}{segment}/{commit}"))
}

/// A pull request preview of a deployment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Preview {
    pub pull_request_id: i64,
    pub fqdn: Option<String>,
}

/// `DeploymentSuccess` and `DeploymentFailed`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Deployment {
    pub application_name: String,
    pub application_uuid: String,
    pub deployment_uuid: String,
    pub project_name: String,
    pub project_uuid: String,
    pub environment_name: String,
    pub environment_uuid: String,
    pub fqdn: Option<String>,
    #[serde(default)]
    pub preview: Option<Preview>,
    #[serde(default)]
    pub commit: Option<CommitDetails>,
}

/// `str($fqdn)->explode(',')`: only the first domain is used.
fn first_fqdn(fqdn: Option<&str>) -> Option<String> {
    let f = fqdn?;
    Some(f.split(',').next().unwrap_or(f).to_owned())
}

impl Deployment {
    fn url(&self, ctx: &Ctx) -> String {
        format!(
            "{}/project/{}/environment/{}/application/{}/deployment/{}",
            ctx.base_url,
            self.project_uuid,
            self.environment_uuid,
            self.application_uuid,
            self.deployment_uuid
        )
    }
}

/// The resource of `RestartLimitReached`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RestartKind {
    /// `Application`.
    Application,
    /// `ApplicationPreview`.
    Preview,
    /// `ServiceApplication` or `ServiceDatabase`.
    Service,
    /// A standalone database.
    Database,
}

/// `RestartLimitReached`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RestartLimit {
    pub kind: RestartKind,
    pub resource_name: String,
    /// `$resource->uuid`.
    pub resource_uuid: String,
    /// The uuid in the link: the application of a preview, the service of a sub-resource.
    pub url_uuid: String,
    pub project_name: String,
    pub project_uuid: String,
    pub environment_name: String,
    pub environment_uuid: String,
    pub fqdn: Option<String>,
    pub restart_count: i64,
    pub max_restart_count: i64,
}

/// A database whose backup ran.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DatabaseBackup {
    pub name: String,
    pub uuid: String,
    pub project_uuid: Option<String>,
    pub environment_uuid: Option<String>,
    /// The `$database_name` argument.
    pub database_name: Option<String>,
    pub frequency: String,
}

impl DatabaseBackup {
    fn url(&self, ctx: &Ctx) -> String {
        format!(
            "{}/project/{}/environment/{}/database/{}",
            ctx.base_url,
            self.project_uuid.as_deref().unwrap_or_default(),
            self.environment_uuid.as_deref().unwrap_or_default(),
            self.uuid
        )
    }
    fn db(&self) -> &str {
        self.database_name.as_deref().unwrap_or_default()
    }
}

/// The application or service of a scheduled task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskResource {
    pub is_application: bool,
    pub uuid: String,
    pub project_uuid: Option<String>,
    pub environment_uuid: Option<String>,
}

/// `TaskSuccess` and `TaskFailed`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Task {
    pub name: String,
    pub uuid: String,
    pub output: String,
    pub resource: Option<TaskResource>,
}

impl Task {
    fn url(&self, ctx: &Ctx) -> Option<String> {
        self.resource.as_ref().map(|r| {
            format!(
                "{}/project/{}/environment/{}/{}/{}/tasks/{}",
                ctx.base_url,
                r.project_uuid.as_deref().unwrap_or_default(),
                r.environment_uuid.as_deref().unwrap_or_default(),
                if r.is_application {
                    "application"
                } else {
                    "service"
                },
                r.uuid,
                self.uuid
            )
        })
    }

    fn webhook(&self, ctx: &Ctx, success: bool) -> Value {
        let mut d = Map::new();
        d.insert("success".into(), json!(success));
        d.insert(
            "message".into(),
            json!(if success {
                "Scheduled task succeeded"
            } else {
                "Scheduled task failed"
            }),
        );
        d.insert(
            "event".into(),
            json!(if success {
                "task_success"
            } else {
                "task_failed"
            }),
        );
        d.insert("task_name".into(), json!(self.name));
        d.insert("task_uuid".into(), json!(self.uuid));
        d.insert("output".into(), json!(self.output));
        if let Some(r) = &self.resource {
            let key = if r.is_application {
                "application_uuid"
            } else {
                "service_uuid"
            };
            d.insert(key.into(), json!(r.uuid));
        }
        if let Some(u) = self.url(ctx) {
            d.insert("url".into(), json!(u));
        }
        Value::Object(d)
    }
}

/// A server named in a notification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerRef {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    pub uuid: String,
}

/// `$server->outdatedInfo` of `TraefikVersionOutdated`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TraefikServer {
    pub name: String,
    pub uuid: String,
    #[serde(default)]
    pub current: Option<String>,
    #[serde(default)]
    pub latest: Option<String>,
    #[serde(default)]
    pub update_type: Option<String>,
    #[serde(default)]
    pub upgrade_target: Option<String>,
    #[serde(default)]
    pub newer_branch_target: Option<String>,
    #[serde(default)]
    pub newer_branch_latest: Option<String>,
}

fn with_v(v: &str) -> String {
    if v.starts_with('v') {
        v.to_owned()
    } else {
        format!("v{v}")
    }
}

impl TraefikServer {
    fn kind(&self) -> &str {
        self.update_type.as_deref().unwrap_or("patch_update")
    }
    fn current(&self) -> String {
        with_v(self.current.as_deref().unwrap_or("unknown"))
    }
    fn latest(&self) -> String {
        with_v(self.latest.as_deref().unwrap_or("unknown"))
    }
    /// `getUpgradeTarget`.
    fn target(&self) -> String {
        if self.kind() == "minor_upgrade"
            && let Some(t) = &self.upgrade_target
        {
            return with_v(t);
        }
        self.latest()
    }
    fn is_patch(&self) -> bool {
        self.kind() == "patch_update"
    }
    fn newer_latest(&self) -> String {
        with_v(self.newer_branch_latest.as_deref().unwrap_or_default())
    }
}

fn traefik_has_upgrades(servers: &[TraefikServer]) -> bool {
    servers
        .iter()
        .any(|s| s.kind() == "minor_upgrade" || s.newer_branch_target.is_some())
}

/// A storage backup target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VolumeBackup {
    /// `$resource?->name`.
    pub resource_name: Option<String>,
    /// `$backup->targetType().' '.$backup->targetName()`.
    pub target: String,
    pub frequency: String,
    /// `$backup->resourceUrl()`.
    pub url: Option<String>,
}

impl VolumeBackup {
    fn resource(&self) -> &str {
        self.resource_name.as_deref().unwrap_or("Unknown resource")
    }
    fn open_button(&self) -> Vec<Value> {
        match self.url.as_deref().filter(|u| truthy(Some(u))) {
            Some(u) => vec![button("Open resource in Coolify", u)],
            None => Vec::new(),
        }
    }
    fn slack_link(&self) -> String {
        match self.url.as_deref().filter(|u| truthy(Some(u))) {
            Some(u) => format!("\n\n<{u}|Open resource in Coolify>"),
            None => String::new(),
        }
    }
    fn discord_link(&self, m: &mut DiscordMessage) {
        if let Some(u) = self.url.as_deref().filter(|u| truthy(Some(u))) {
            m.field("Resource", format!("[Link]({u})"), false);
        }
    }
}

/// A database of `SslExpirationNotification`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SslResource {
    pub name: String,
    pub uuid: String,
    pub project_uuid: Option<String>,
    pub environment_uuid: Option<String>,
}

/// Every notification, built from plain data. See the variants' fields; URLs under
/// `base_url()` are derived when delivering, as upstream's constructors do.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum Notification {
    DeploymentSuccess(Deployment),
    DeploymentFailed(Deployment),
    StatusChanged {
        name: String,
        uuid: String,
        project_name: String,
        project_uuid: String,
        environment_name: String,
        environment_uuid: String,
        fqdn: Option<String>,
    },
    RestartLimitReached(RestartLimit),
    ContainerRestarted {
        name: String,
        server: ServerRef,
        url: Option<String>,
        /// `$restartedResource`: morph class and id. At most one per hour when set.
        #[serde(default)]
        throttle: Option<(String, i64)>,
    },
    BackupSuccess(DatabaseBackup),
    BackupFailed {
        backup: DatabaseBackup,
        output: String,
    },
    BackupMissing {
        database_name: String,
        backup_uuid: String,
        /// `missing_backup_notification_days`.
        days: Value,
        last_execution_at: Option<String>,
    },
    BackupSuccessWithS3Warning {
        backup: DatabaseBackup,
        s3_error: String,
        s3_uuid: Option<String>,
    },
    General {
        message: String,
        success: bool,
    },
    TaskSuccess(Task),
    TaskFailed(Task),
    DockerCleanupSuccess {
        server: ServerRef,
        message: String,
    },
    DockerCleanupFailed {
        server: ServerRef,
        message: String,
    },
    ForceDisabled {
        server: ServerRef,
    },
    ForceEnabled {
        server: ServerRef,
    },
    HetznerDeletionFailed {
        hetzner_server_id: i64,
        error: String,
    },
    HighDiskUsage {
        server: ServerRef,
        disk_usage: i64,
        threshold: i64,
        /// `settings.server_disk_usage_notification_interval_hours`.
        #[serde(default = "default_interval")]
        interval_hours: i64,
    },
    Reachable {
        server: ServerRef,
    },
    Unreachable {
        server: ServerRef,
    },
    ServerPatchCheck {
        server: ServerRef,
        /// `$patchData`, as `CheckUpdates` returns it.
        patch_data: Value,
    },
    TraefikVersionOutdated {
        servers: Vec<TraefikServer>,
    },
    VolumeBackupSuccess {
        backup: VolumeBackup,
        warning: Option<String>,
    },
    VolumeBackupFailed {
        backup: VolumeBackup,
        output: String,
    },
    VolumeBackupMissing {
        backup: VolumeBackup,
        backup_uuid: String,
        days: Value,
        last_execution_at: Option<String>,
    },
    VolumeBackupRecoveryFailed {
        backup: Option<VolumeBackup>,
        execution_uuid: String,
        /// `recovery_error`.
        recovery_error: Option<String>,
        containers_stopped: bool,
    },
    ApiTokenExpiring {
        token_name: String,
        expires_at: Option<String>,
    },
    SslExpiration {
        resources: Vec<SslResource>,
    },
    Test {
        emails: Option<String>,
        channel: Option<String>,
        #[serde(default)]
        ping: bool,
    },
}

fn default_interval() -> i64 {
    24
}

/// The throttle `Team::notify` claims for a `ThrottledNotification`.
#[derive(Debug, Clone, PartialEq)]
pub struct Throttle {
    pub notifiable_type: String,
    pub notifiable_id: i64,
    pub notification: &'static str,
    pub minutes: i64,
}

fn patch_str<'a>(p: &'a Value, key: &str, default: &'a str) -> String {
    match p.get(key) {
        Some(v) if !v.is_null() => php_string(v),
        _ => default.to_owned(),
    }
}

fn patch_updates(p: &Value) -> Vec<Value> {
    match p.get("updates") {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::Object(o)) => o.values().cloned().collect(),
        _ => Vec::new(),
    }
}

fn patch_error(p: &Value) -> Option<String> {
    p.get("error").filter(|v| !v.is_null()).map(php_string)
}

fn patch_total(p: &Value) -> Value {
    match p.get("total_updates") {
        Some(v) if !v.is_null() => v.clone(),
        _ => json!(0),
    }
}

fn update_line(u: &Value, code: bool) -> String {
    let f = |k: &str| u.get(k).map(php_string).unwrap_or_default();
    if code {
        format!(
            "• `{}`: {} → {}\n",
            f("package"),
            f("current_version"),
            f("new_version")
        )
    } else {
        format!(
            "• {}: {} → {}\n",
            f("package"),
            f("current_version"),
            f("new_version")
        )
    }
}

fn critical(updates: &[Value]) -> Vec<&Value> {
    updates
        .iter()
        .filter(|u| super::php::filled(u.get("risk")))
        .collect()
}

fn ssl_urls(ctx: &Ctx, resources: &[SslResource]) -> Vec<(String, String)> {
    let mut urls: Vec<(String, String)> = Vec::new();
    for r in resources {
        let url = format!(
            "{}/project/{}/environment/{}/database/{}",
            ctx.base_url,
            r.project_uuid.as_deref().unwrap_or_default(),
            r.environment_uuid.as_deref().unwrap_or_default(),
            r.uuid
        );
        match urls.iter_mut().find(|(n, _)| n == &r.name) {
            Some(e) => e.1 = url,
            None => urls.push((r.name.clone(), url)),
        }
    }
    urls
}

fn ssl_names(resources: &[SslResource]) -> String {
    resources
        .iter()
        .map(|r| r.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn recovery_reason(recovery_error: Option<&str>) -> &'static str {
    match recovery_error {
        Some("s3_auth") => "S3 credentials were rejected",
        Some("s3_bucket") => "S3 bucket is missing",
        Some("remote_command") => "A command on the server failed",
        _ => "Unknown error",
    }
}

fn recovery_parts(backup: Option<&VolumeBackup>) -> (String, String, Option<String>) {
    match backup {
        Some(b) => (b.resource().to_owned(), b.target.clone(), b.url.clone()),
        None => ("Unknown resource".into(), "Unknown storage".into(), None),
    }
}

fn recovery_summary(
    backup: Option<&VolumeBackup>,
    recovery_error: Option<&str>,
    stopped: bool,
) -> String {
    let (name, target, _) = recovery_parts(backup);
    let summary = format!(
        "Recovery after the storage backup of {target} ({name}) failed: {}.",
        recovery_reason(recovery_error)
    );
    if stopped {
        format!(
            "{summary} The containers stopped for the backup are still stopped, and new backups of this storage are skipped until you retry the recovery."
        )
    } else {
        format!(
            "{summary} The partial S3 upload was not removed. Retry the recovery after fixing the S3 storage."
        )
    }
}

fn volume_success_summary(b: &VolumeBackup, warning: Option<&str>) -> String {
    let s = format!(
        "Storage backup of {} ({}) was successful.",
        b.target,
        b.resource()
    );
    if truthy(warning) {
        format!("{s} Warning: {}", warning.unwrap_or_default())
    } else {
        s
    }
}

fn volume_failed_summary(b: &VolumeBackup) -> String {
    format!(
        "Storage backup of {} ({}) has FAILED.",
        b.target,
        b.resource()
    )
}

fn volume_missing_summary(b: &VolumeBackup, days: &Value) -> String {
    format!(
        "The enabled storage backup schedule for {} ({}) has produced no executions in the last {} day(s).",
        b.target,
        b.resource(),
        php_string(days)
    )
}

fn s3_url(ctx: &Ctx, uuid: Option<&str>) -> Option<String> {
    uuid.map(|u| format!("{}/storages/{u}", ctx.base_url))
}

impl Notification {
    /// `via()`: the event name of `getEnabledChannels`.
    pub fn event(&self) -> &'static str {
        use Notification::*;
        match self {
            DeploymentSuccess(_) => "deployment_success",
            DeploymentFailed(_) => "deployment_failure",
            StatusChanged { .. } | ContainerRestarted { .. } => "status_change",
            RestartLimitReached(_) => "restart_limit_reached",
            BackupSuccess(_) => "backup_success",
            BackupFailed { .. }
            | BackupMissing { .. }
            | BackupSuccessWithS3Warning { .. }
            | VolumeBackupFailed { .. }
            | VolumeBackupMissing { .. }
            | VolumeBackupRecoveryFailed { .. } => "backup_failure",
            VolumeBackupSuccess { warning, .. } => {
                if truthy(warning.as_deref()) {
                    "backup_failure"
                } else {
                    "backup_success"
                }
            }
            General { .. } => "general",
            TaskSuccess(_) => "scheduled_task_success",
            TaskFailed(_) => "scheduled_task_failure",
            DockerCleanupSuccess { .. } => "docker_cleanup_success",
            DockerCleanupFailed { .. } => "docker_cleanup_failure",
            ForceDisabled { .. } => "server_force_disabled",
            ForceEnabled { .. } => "server_force_enabled",
            HetznerDeletionFailed { .. } => "hetzner_deletion_failure",
            HighDiskUsage { .. } => "server_disk_usage",
            Reachable { .. } => "server_reachable",
            Unreachable { .. } => "server_unreachable",
            ServerPatchCheck { .. } => "server_patch",
            TraefikVersionOutdated { .. } => "traefik_outdated",
            ApiTokenExpiring { .. } => "api_token_expiring",
            SslExpiration { .. } => "ssl_certificate_renewal",
            Test { .. } => "test",
        }
    }

    /// `isTestNotification`.
    pub fn is_test(&self) -> bool {
        matches!(self, Notification::Test { .. })
    }

    /// `TelegramChannel`: the column with the thread id of this notification.
    pub fn telegram_thread_column(&self) -> Option<&'static str> {
        use Notification::*;
        Some(match self {
            DeploymentSuccess(_) => "telegram_notifications_deployment_success_thread_id",
            DeploymentFailed(_) => "telegram_notifications_deployment_failure_thread_id",
            StatusChanged { .. } | ContainerRestarted { .. } => {
                "telegram_notifications_status_change_thread_id"
            }
            RestartLimitReached(_) => "telegram_notifications_restart_limit_reached_thread_id",
            BackupSuccess(_) => "telegram_notifications_backup_success_thread_id",
            VolumeBackupSuccess { warning, .. } => {
                if truthy(warning.as_deref()) {
                    "telegram_notifications_backup_failure_thread_id"
                } else {
                    "telegram_notifications_backup_success_thread_id"
                }
            }
            BackupFailed { .. }
            | BackupMissing { .. }
            | BackupSuccessWithS3Warning { .. }
            | VolumeBackupFailed { .. }
            | VolumeBackupMissing { .. }
            | VolumeBackupRecoveryFailed { .. } => {
                "telegram_notifications_backup_failure_thread_id"
            }
            TaskSuccess(_) => "telegram_notifications_scheduled_task_success_thread_id",
            TaskFailed(_) => "telegram_notifications_scheduled_task_failure_thread_id",
            DockerCleanupSuccess { .. } => {
                "telegram_notifications_docker_cleanup_success_thread_id"
            }
            DockerCleanupFailed { .. } => "telegram_notifications_docker_cleanup_failure_thread_id",
            HighDiskUsage { .. } => "telegram_notifications_server_disk_usage_thread_id",
            Unreachable { .. } => "telegram_notifications_server_unreachable_thread_id",
            Reachable { .. } => "telegram_notifications_server_reachable_thread_id",
            ServerPatchCheck { .. } => "telegram_notifications_server_patch_thread_id",
            TraefikVersionOutdated { .. } => "telegram_notifications_traefik_outdated_thread_id",
            _ => return None,
        })
    }

    /// `ThrottledNotification`.
    pub fn throttle(&self) -> Option<Throttle> {
        match self {
            Notification::ContainerRestarted {
                throttle: Some((t, id)),
                ..
            } => Some(Throttle {
                notifiable_type: t.clone(),
                notifiable_id: *id,
                notification: "App\\Notifications\\Container\\ContainerRestarted",
                minutes: 60,
            }),
            Notification::HighDiskUsage {
                server,
                interval_hours,
                ..
            } => Some(Throttle {
                notifiable_type: "App\\Models\\Server".into(),
                notifiable_id: server.id,
                notification: "App\\Notifications\\Server\\HighDiskUsage",
                minutes: (*interval_hours).max(1) * 60,
            }),
            _ => None,
        }
    }

    /// `isEmailRateLimited` of `Reachable` and `Unreachable`.
    pub fn rate_limit_key(&self) -> Option<String> {
        match self {
            Notification::Reachable { server } => Some(format!("server-reachable:{}", server.id)),
            Notification::Unreachable { server } => {
                Some(format!("server-unreachable:{}", server.id))
            }
            _ => None,
        }
    }

    /// `toMail($notifiable)`; `None` for `GeneralNotification`, which has no mail.
    /// `commit_details` is `emailNotificationSettings.deployment_commit_details_email_notifications`.
    pub fn to_mail(&self, ctx: &Ctx, commit_details: bool) -> Option<MailMessage> {
        use Notification::*;
        let app = ctx.app_name.as_str();
        match self {
            DeploymentSuccess(d) => {
                let pr = d.preview.as_ref().map_or(0, |p| p.pull_request_id);
                let first = first_fqdn(d.fqdn.as_deref());
                let (subject, fqdn) = if pr == 0 {
                    (
                        format!("Coolify: New version is deployed of {}", d.application_name),
                        first,
                    )
                } else {
                    (
                        format!(
                            "Coolify: Pull request #{pr} of {} deployed successfully",
                            d.application_name
                        ),
                        d.preview.as_ref().and_then(|p| p.fqdn.clone()),
                    )
                };
                let commit = d
                    .commit
                    .as_ref()
                    .filter(|_| commit_details)
                    .and_then(CommitDetails::for_mail);
                mail(
                    subject,
                    views::deployment_success(
                        app,
                        &d.application_name,
                        fqdn.as_deref(),
                        pr,
                        &d.url(ctx),
                        commit.as_ref(),
                    ),
                )
            }
            DeploymentFailed(d) => {
                let pr = d.preview.as_ref().map_or(0, |p| p.pull_request_id);
                let first = first_fqdn(d.fqdn.as_deref());
                let (subject, fqdn) = if pr == 0 {
                    (
                        format!("Coolify: Deployment failed of {}.", d.application_name),
                        first,
                    )
                } else {
                    (
                        format!(
                            "Coolify: Deployment failed of pull request #{pr} of {}.",
                            d.application_name
                        ),
                        d.preview.as_ref().and_then(|p| p.fqdn.clone()),
                    )
                };
                let commit = d
                    .commit
                    .as_ref()
                    .filter(|_| commit_details)
                    .and_then(CommitDetails::for_mail);
                mail(
                    subject,
                    views::deployment_failed(
                        app,
                        &d.application_name,
                        fqdn.as_deref(),
                        pr,
                        &d.url(ctx),
                        commit.as_ref(),
                    ),
                )
            }
            StatusChanged { name, .. } => mail(
                format!("Coolify: {name} has been stopped"),
                views::application_status_changes(app, name),
            ),
            RestartLimitReached(r) => mail(
                format!(
                    "Coolify: {} stopped - restart limit reached ({}/{})",
                    r.resource_name, r.restart_count, r.max_restart_count
                ),
                views::restart_limit_reached(
                    app,
                    &r.resource_name,
                    r.restart_count,
                    r.max_restart_count,
                    &r.url(ctx),
                ),
            ),
            ContainerRestarted { name, server, .. } => mail(
                format!(
                    "Coolify: A resource ({name}) has been restarted automatically on {}",
                    server.name
                ),
                views::container_restarted(app, name, &server.name),
            ),
            BackupSuccess(b) => mail(
                format!("Coolify: Backup successfully done for {}", b.name),
                views::backup_success(app, &b.name, b.database_name.as_deref(), &b.frequency),
            ),
            BackupFailed { backup: b, output } => mail(
                format!(
                    "Coolify: [ACTION REQUIRED] Database Backup FAILED for {}",
                    b.name
                ),
                views::backup_failed(
                    app,
                    &b.name,
                    b.database_name.as_deref(),
                    &b.frequency,
                    output,
                ),
            ),
            BackupMissing {
                database_name,
                days,
                last_execution_at,
                ..
            } => mail(
                format!("Coolify: [ACTION REQUIRED] No recent backup for {database_name}"),
                views::backup_missing(
                    database_name,
                    &php_string(days),
                    last_execution_at.as_deref(),
                ),
            ),
            BackupSuccessWithS3Warning {
                backup: b,
                s3_error,
                s3_uuid,
            } => mail(
                format!(
                    "Coolify: Backup succeeded locally but S3 upload failed for {}",
                    b.name
                ),
                views::backup_success_with_s3_warning(
                    app,
                    &b.name,
                    b.database_name.as_deref(),
                    &b.frequency,
                    s3_error,
                    s3_url(ctx, s3_uuid.as_deref()).as_deref(),
                ),
            ),
            General { .. } => None,
            TaskSuccess(t) => mail(
                format!("Coolify: Scheduled task ({}) succeeded.", t.name),
                views::scheduled_task_success(app, &t.name, &t.output, t.url(ctx).as_deref()),
            ),
            TaskFailed(t) => mail(
                format!(
                    "Coolify: [ACTION REQUIRED] Scheduled task ({}) failed.",
                    t.name
                ),
                views::scheduled_task_failed(app, &t.name, &t.output, t.url(ctx).as_deref()),
            ),
            DockerCleanupSuccess { server, message } => mail(
                format!("Coolify: Docker cleanup job succeeded on {}", server.name),
                views::docker_cleanup_success(app, &server.name, message),
            ),
            DockerCleanupFailed { server, message } => mail(
                format!(
                    "Coolify: [ACTION REQUIRED] Docker cleanup job failed on {}",
                    server.name
                ),
                views::docker_cleanup_failed(app, &server.name, message),
            ),
            ForceDisabled { server } => mail(
                format!(
                    "Coolify: Server ({}) disabled because it is not paid!",
                    server.name
                ),
                views::server_force_disabled(app, &server.name),
            ),
            ForceEnabled { server } => mail(
                format!("Coolify: Server ({}) enabled again!", server.name),
                views::server_force_enabled(app, &server.name),
            ),
            HetznerDeletionFailed {
                hetzner_server_id,
                error,
            } => mail(
                format!(
                    "Coolify: [ACTION REQUIRED] Failed to delete Hetzner server #{hetzner_server_id}"
                ),
                views::hetzner_deletion_failed(app, *hetzner_server_id, error),
            ),
            HighDiskUsage {
                server,
                disk_usage,
                threshold,
                ..
            } => mail(
                format!(
                    "Coolify: Server ({}) high disk usage detected!",
                    server.name
                ),
                views::high_disk_usage(app, &server.name, *disk_usage, *threshold),
            ),
            Reachable { server } => mail(
                format!("Coolify: Server ({}) revived.", server.name),
                views::server_revived(app, &server.name),
            ),
            Unreachable { server } => mail(
                format!("Coolify: Your server ({}) is unreachable.", server.name),
                views::server_lost_connection(app, &server.name),
            ),
            ServerPatchCheck { server, patch_data } => {
                let url = patch_url(ctx, server);
                let os = patch_str(patch_data, "osId", "unknown");
                let manager = patch_str(patch_data, "package_manager", "unknown");
                if let Some(error) = patch_error(patch_data) {
                    return mail(
                        format!(
                            "Coolify: [ERROR] Failed to check patches on {}",
                            server.name
                        ),
                        views::server_patches_error(app, &server.name, &os, &manager, &error, &url),
                    );
                }
                let total = patch_total(patch_data);
                mail(
                    format!(
                        "Coolify: [ACTION REQUIRED] {} server patches available on {}",
                        php_string(&total),
                        server.name
                    ),
                    views::server_patches(
                        app,
                        &server.name,
                        &total,
                        &patch_updates(patch_data),
                        patch_data.get("security_updates"),
                        patch_data.get("reboot_required"),
                        &os,
                        &manager,
                        &url,
                    ),
                )
            }
            TraefikVersionOutdated { servers } => {
                let with_urls: Vec<(TraefikServer, String)> = servers
                    .iter()
                    .map(|s| {
                        (
                            s.clone(),
                            format!("{}/server/{}/proxy", ctx.base_url, s.uuid),
                        )
                    })
                    .collect();
                mail(
                    format!(
                        "Coolify: Traefik proxy outdated on {} server(s)",
                        servers.len()
                    ),
                    views::traefik_version_outdated(app, servers.len(), &with_urls),
                )
            }
            VolumeBackupSuccess { backup: b, warning } => mail(
                if truthy(warning.as_deref()) {
                    format!(
                        "Coolify: Storage backup succeeded with a warning for {}",
                        b.resource()
                    )
                } else {
                    format!(
                        "Coolify: Storage backup successfully done for {}",
                        b.resource()
                    )
                },
                views::volume_backup_result(
                    app,
                    &volume_success_summary(b, warning.as_deref()),
                    &b.frequency,
                    None,
                    b.url.as_deref(),
                ),
            ),
            VolumeBackupFailed { backup: b, output } => mail(
                format!(
                    "Coolify: [ACTION REQUIRED] Storage backup FAILED for {}",
                    b.resource()
                ),
                views::volume_backup_result(
                    app,
                    &volume_failed_summary(b),
                    &b.frequency,
                    Some(output),
                    b.url.as_deref(),
                ),
            ),
            VolumeBackupMissing {
                backup: b,
                days,
                last_execution_at,
                ..
            } => mail(
                format!(
                    "Coolify: [ACTION REQUIRED] No recent storage backup for {}",
                    b.resource()
                ),
                views::volume_backup_missing(
                    app,
                    &volume_missing_summary(b, days),
                    last_execution_at.as_deref(),
                    b.url.as_deref(),
                ),
            ),
            VolumeBackupRecoveryFailed {
                backup,
                recovery_error,
                containers_stopped,
                ..
            } => {
                let (name, _, url) = recovery_parts(backup.as_ref());
                mail(
                    format!("Coolify: [ACTION REQUIRED] Storage backup recovery FAILED for {name}"),
                    views::volume_backup_recovery_failed(
                        app,
                        &recovery_summary(
                            backup.as_ref(),
                            recovery_error.as_deref(),
                            *containers_stopped,
                        ),
                        url.as_deref(),
                    ),
                )
            }
            ApiTokenExpiring {
                token_name,
                expires_at,
            } => mail(
                format!("Coolify: API token '{token_name}' expires in 24 hours"),
                views::api_token_expiring(
                    app,
                    token_name,
                    expires_at.as_deref().unwrap_or_default(),
                    &format!("{}/security/api-tokens", ctx.base_url),
                ),
            ),
            SslExpiration { resources } => mail(
                "Coolify: [Action Required] SSL Certificates Renewed - Manual Redeployment Needed",
                views::ssl_certificate_renewed(app, resources, &ssl_urls(ctx, resources)),
            ),
            Test { .. } => mail("Coolify: Test Email", views::test_email(app)),
        }
    }

    /// `toDiscord()`.
    pub fn to_discord(&self, ctx: &Ctx) -> DiscordMessage {
        use DiscordMessage as D;
        use Notification::*;
        match self {
            DeploymentSuccess(d) => {
                let url = d.url(ctx);
                if let Some(p) = &d.preview {
                    let mut m = D::new(
                        ":white_check_mark: Preview deployment successful",
                        format!("Pull request: {}", p.pull_request_id),
                        D::SUCCESS,
                    );
                    if truthy(p.fqdn.as_deref()) {
                        m.field(
                            "Application",
                            format!("[Link]({})", p.fqdn.as_deref().unwrap_or_default()),
                            false,
                        );
                    }
                    m.field("Project", &d.project_name, true);
                    m.field("Environment", &d.environment_name, true);
                    m.field("Name", &d.application_name, true);
                    m.field("Deployment logs", format!("[Link]({url})"), false);
                    m
                } else {
                    let f = first_fqdn(d.fqdn.as_deref());
                    let description = if truthy(f.as_deref()) {
                        format!("[Open application]({})", f.unwrap_or_default())
                    } else {
                        String::new()
                    };
                    let mut m = D::new(
                        ":white_check_mark: New version successfully deployed",
                        description,
                        D::SUCCESS,
                    );
                    m.field("Project", &d.project_name, true);
                    m.field("Environment", &d.environment_name, true);
                    m.field("Name", &d.application_name, true);
                    m.field("Deployment logs", format!("[Link]({url})"), false);
                    m
                }
            }
            DeploymentFailed(d) => {
                let url = d.url(ctx);
                let f = first_fqdn(d.fqdn.as_deref());
                let description = match &d.preview {
                    Some(p) => format!("Pull request: {}", p.pull_request_id),
                    None if truthy(f.as_deref()) => {
                        format!("[Open application]({})", f.as_deref().unwrap_or_default())
                    }
                    None => String::new(),
                };
                let mut m = D::new(":cross_mark: Deployment failed", description, D::ERROR).critical();
                m.field("Project", &d.project_name, true);
                m.field("Environment", &d.environment_name, true);
                m.field("Name", &d.application_name, true);
                m.field("Deployment Logs", format!("[Link]({url})"), false);
                if d.preview.is_some() && truthy(f.as_deref()) {
                    m.field("Domain", f.unwrap_or_default(), true);
                }
                m
            }
            StatusChanged {
                uuid,
                project_uuid,
                environment_uuid,
                ..
            } => D::new(
                ":cross_mark: Application stopped",
                format!(
                    "[Open Application in Coolify]({})",
                    status_url(ctx, project_uuid, environment_uuid, uuid)
                ),
                D::ERROR,
            )
            .critical(),
            RestartLimitReached(r) => D::new(
                ":warning: Restart limit reached",
                format!(
                    "{} has been stopped after {} restarts (limit: {}).\n\n[Open Resource in Coolify]({})",
                    r.resource_name,
                    r.restart_count,
                    r.max_restart_count,
                    r.url(ctx)
                ),
                D::ERROR,
            )
            .critical(),
            ContainerRestarted {
                name, server, url, ..
            } => {
                let mut m = D::new(
                    ":warning: Resource restarted",
                    format!(
                        "{name} has been restarted automatically on {}.",
                        server.name
                    ),
                    D::INFO,
                );
                if truthy(url.as_deref()) {
                    m.field(
                        "Resource",
                        format!("[Link]({})", url.as_deref().unwrap_or_default()),
                        false,
                    );
                }
                m
            }
            BackupSuccess(b) => {
                let mut m = D::new(
                    ":white_check_mark: Database backup successful",
                    format!(
                        "Database backup for {} (db:{}) was successful.",
                        b.name,
                        b.db()
                    ),
                    D::SUCCESS,
                );
                m.field("Frequency", &b.frequency, true);
                m
            }
            BackupFailed { backup: b, output } => {
                let mut m = D::new(
                    ":cross_mark: Database backup failed",
                    format!("Database backup for {} (db:{}) has FAILED.", b.name, b.db()),
                    D::ERROR,
                )
                .critical();
                m.field("Frequency", &b.frequency, true);
                m.field("Output", output, false);
                m
            }
            BackupMissing {
                database_name,
                days,
                ..
            } => D::new(
                ":warning: Scheduled database backup missing",
                backup_missing_description(database_name, days),
                D::ERROR,
            )
            .critical(),
            BackupSuccessWithS3Warning {
                backup: b,
                s3_error,
                s3_uuid,
            } => {
                let mut m = D::new(
                    ":warning: Database backup succeeded locally, S3 upload failed",
                    format!(
                        "Database backup for {} (db:{}) was created successfully on local storage, but failed to upload to S3.",
                        b.name,
                        b.db()
                    ),
                    D::WARNING,
                );
                m.field("Frequency", &b.frequency, true);
                m.field("S3 Error", s3_error, false);
                if let Some(u) = s3_url(ctx, s3_uuid.as_deref()) {
                    m.field("S3 Storage", format!("[Check Configuration]({u})"), false);
                }
                m
            }
            General { message, success } => D::new(
                if *success {
                    "Coolify: General Notification"
                } else {
                    "Coolify: Action required"
                },
                message,
                if *success { D::INFO } else { D::ERROR },
            ),
            TaskSuccess(t) => {
                let mut m = D::new(
                    ":white_check_mark: Scheduled task succeeded",
                    format!("Scheduled task ({}) succeeded.", t.name),
                    D::SUCCESS,
                );
                if let Some(u) = t.url(ctx) {
                    m.field("Scheduled task", format!("[Link]({u})"), false);
                }
                m
            }
            TaskFailed(t) => {
                let mut m = D::new(
                    ":cross_mark: Scheduled task failed",
                    format!("Scheduled task ({}) failed.", t.name),
                    D::ERROR,
                );
                if let Some(u) = t.url(ctx) {
                    m.field("Scheduled task", format!("[Link]({u})"), false);
                }
                m
            }
            DockerCleanupSuccess { server, message } => D::new(
                format!(
                    ":white_check_mark: Coolify: Docker cleanup job succeeded on {}",
                    server.name
                ),
                message,
                D::SUCCESS,
            ),
            DockerCleanupFailed { server, message } => D::new(
                format!(
                    ":cross_mark: Coolify: [ACTION REQUIRED] Docker cleanup job failed on {}",
                    server.name
                ),
                message,
                D::ERROR,
            ),
            ForceDisabled { server } => {
                let mut m = D::new(
                    ":cross_mark: Server disabled",
                    format!(
                        "Server ({}) disabled because it is not paid!",
                        server.name
                    ),
                    D::ERROR,
                );
                m.field(
                    "Please update your subscription to enable the server again!",
                    "[Link](https://app.coolify.io/subscription)",
                    false,
                );
                m
            }
            ForceEnabled { server } => D::new(
                ":white_check_mark: Server enabled",
                format!("Server '{}' enabled again!", server.name),
                D::SUCCESS,
            ),
            HetznerDeletionFailed {
                hetzner_server_id: id,
                error,
            } => D::new(
                ":cross_mark: Coolify: [ACTION REQUIRED] Failed to delete Hetzner server",
                format!(
                    "Failed to delete Hetzner server #{id} from Hetzner Cloud.\n\n**Error:** {error}\n\nThe server has been removed from Coolify, but may still exist in your Hetzner Cloud account. Please check your Hetzner Cloud console and manually delete the server if needed."
                ),
                D::ERROR,
            ),
            HighDiskUsage {
                server,
                disk_usage,
                threshold,
                ..
            } => {
                let mut m = D::new(
                    ":cross_mark: High disk usage detected",
                    format!("Server '{}' high disk usage detected!", server.name),
                    D::ERROR,
                )
                .critical();
                m.field("Disk usage", format!("{disk_usage}%"), true);
                m.field("Threshold", format!("{threshold}%"), true);
                m.field(
                    "What to do?",
                    "[Link](https://coolify.io/docs/knowledge-base/server/automated-cleanup)",
                    true,
                );
                m.field(
                    "Change Settings",
                    format!(
                        "[Threshold]({0}/server/{1}#advanced) | [Notification]({0}/notifications/discord)",
                        ctx.base_url, server.uuid
                    ),
                    false,
                );
                m
            }
            Reachable { server } => D::new(
                format!(":white_check_mark: Server '{}' revived", server.name),
                "All automations & integrations are turned on again!",
                D::SUCCESS,
            ),
            Unreachable { server } => {
                let mut m = D::new(
                    ":cross_mark: Server unreachable",
                    format!("Your server '{}' is unreachable.", server.name),
                    D::ERROR,
                );
                m.field(
                    "IMPORTANT",
                    "We automatically try to revive your server and turn on all automations & integrations.",
                    false,
                );
                m
            }
            ServerPatchCheck { server, patch_data } => {
                let url = patch_url(ctx, server);
                let os = ucfirst(&patch_str(patch_data, "osId", "unknown"));
                let manager = patch_str(patch_data, "package_manager", "unknown");
                if let Some(error) = patch_error(patch_data) {
                    let description = format!(
                        "**Failed to check for updates** on server {}\n\n**Error Details:**\n• OS: {os}\n• Package Manager: {manager}\n• Error: {error}\n\n[Manage Server]({url})",
                        server.name
                    );
                    return D::new(
                        format!(
                            ":x: Coolify: [ERROR] Failed to check patches on {}",
                            server.name
                        ),
                        description,
                        D::ERROR,
                    );
                }
                let total = php_string(&patch_total(patch_data));
                let updates = patch_updates(patch_data);
                let mut description = format!(
                    "**{total} package updates** available for server {}\n\n**Summary:**\n• OS: {os}\n• Package Manager: {manager}\n• Total Updates: {total}\n\n",
                    server.name
                );
                if !updates.is_empty() {
                    description.push_str("**Sample Updates:**\n");
                    for u in updates.iter().take(5) {
                        description.push_str(&update_line(u, false));
                    }
                    if updates.len() > 5 {
                        description
                            .push_str(&format!("• ... and {} more packages\n", updates.len() - 5));
                    }
                    let crit = critical(&updates);
                    if !crit.is_empty() {
                        description.push_str(&format!(
                            "\n **Critical packages detected** ({} packages may require restarts)",
                            crit.len()
                        ));
                    }
                    description.push_str(&format!("\n [Manage Server Patches]({url})"));
                }
                D::new(
                    format!(
                        ":warning: Coolify: [ACTION REQUIRED] Server patches available on {}",
                        server.name
                    ),
                    description,
                    D::ERROR,
                )
            }
            TraefikVersionOutdated { servers } => {
                let mut description = format!(
                    "**{} server(s)** running outdated Traefik proxy. Update recommended for security and features.\n\n**Affected servers:**\n",
                    servers.len()
                );
                for s in servers {
                    description.push_str(&traefik_lines(s, false, "  ↳ Also available", "latest patch"));
                }
                description.push_str(
                    "\n⚠️ It is recommended to test before switching the production version.",
                );
                if traefik_has_upgrades(servers) {
                    description.push_str("\n\n📖 **For minor version upgrades**: Read the Traefik changelog before upgrading to understand breaking changes and new features.");
                }
                D::new(
                    ":warning: Coolify: Traefik proxy outdated",
                    description,
                    D::WARNING,
                )
            }
            VolumeBackupSuccess { backup: b, warning } => {
                let warned = truthy(warning.as_deref());
                let mut m = D::new(
                    if warned {
                        ":warning: Storage backup succeeded with a warning"
                    } else {
                        ":white_check_mark: Storage backup successful"
                    },
                    volume_success_summary(b, warning.as_deref()),
                    if warned { D::WARNING } else { D::SUCCESS },
                );
                m.field("Frequency", &b.frequency, true);
                b.discord_link(&mut m);
                m
            }
            VolumeBackupFailed { backup: b, output } => {
                let mut m = D::new(
                    ":cross_mark: Storage backup failed",
                    volume_failed_summary(b),
                    D::ERROR,
                )
                .critical();
                m.field("Frequency", &b.frequency, true);
                m.field("Output", output, false);
                b.discord_link(&mut m);
                m
            }
            VolumeBackupMissing { backup: b, days, .. } => {
                let mut m = D::new(
                    ":warning: Scheduled storage backup missing",
                    volume_missing_summary(b, days),
                    D::ERROR,
                )
                .critical();
                b.discord_link(&mut m);
                m
            }
            VolumeBackupRecoveryFailed {
                backup,
                recovery_error,
                containers_stopped,
                ..
            } => {
                let mut m = D::new(
                    ":cross_mark: Storage backup recovery failed",
                    recovery_summary(
                        backup.as_ref(),
                        recovery_error.as_deref(),
                        *containers_stopped,
                    ),
                    D::ERROR,
                );
                m.is_critical = *containers_stopped;
                if let Some(b) = backup {
                    b.discord_link(&mut m);
                }
                m
            }
            ApiTokenExpiring {
                token_name,
                expires_at,
            } => {
                let mut m = D::new(
                    "🔑 API token expiring soon",
                    format!(
                        "API token **{token_name}** expires on {}.\n\n**Action Required:** Rotate this token before it expires to avoid API outages.",
                        expires_at.as_deref().unwrap_or_default()
                    ),
                    D::WARNING,
                );
                m.field(
                    "Manage tokens",
                    format!("[Open API Tokens]({}/security/api-tokens)", ctx.base_url),
                    false,
                );
                m
            }
            SslExpiration { resources } => {
                let mut m = D::new(
                    "🔒 SSL Certificates Renewed",
                    format!(
                        "SSL certificates have been renewed for: {}.\n\n**Action Required:** These resources need to be redeployed manually.",
                        ssl_names(resources)
                    ),
                    D::WARNING,
                );
                for (name, url) in ssl_urls(ctx, resources) {
                    m.field(name, format!("[View Resource]({url})"), false);
                }
                m
            }
            Test { ping, .. } => {
                let mut m = D::new(
                    ":white_check_mark: Test Success",
                    "This is a test Discord notification from Coolify. :cross_mark: :warning: :information_source:",
                    D::SUCCESS,
                );
                m.is_critical = *ping;
                m.field("Dashboard", format!("[Link]({})", ctx.base_url), true);
                m
            }
        }
    }

    /// `toTelegram()`.
    pub fn to_telegram(&self, ctx: &Ctx) -> TelegramMessage {
        use Notification::*;
        match self {
            DeploymentSuccess(d) => {
                let url = d.url(ctx);
                let mut buttons = Vec::new();
                let message = if let Some(p) = &d.preview {
                    if truthy(p.fqdn.as_deref()) {
                        buttons.push(button(
                            "Open Application",
                            p.fqdn.as_deref().unwrap_or_default(),
                        ));
                    }
                    format!(
                        "Coolify: New PR{} version successfully deployed of {}",
                        p.pull_request_id, d.application_name
                    )
                } else {
                    let f = first_fqdn(d.fqdn.as_deref());
                    if truthy(f.as_deref()) {
                        buttons.push(button("Open Application", f.as_deref().unwrap_or_default()));
                    }
                    format!(
                        "✅ New version successfully deployed of {}",
                        d.application_name
                    )
                };
                buttons.push(button("Deployment logs", &url));
                TelegramMessage { message, buttons }
            }
            DeploymentFailed(d) => {
                let url = d.url(ctx);
                let message = if let Some(p) = &d.preview {
                    format!(
                        "Coolify: Pull request #{} of {} ({}) deployment failed: ",
                        p.pull_request_id,
                        d.application_name,
                        p.fqdn.as_deref().unwrap_or_default()
                    )
                } else {
                    format!(
                        "Coolify: Deployment failed of {} ({}): ",
                        d.application_name,
                        first_fqdn(d.fqdn.as_deref()).unwrap_or_default()
                    )
                };
                TelegramMessage {
                    message,
                    buttons: vec![button("Deployment logs", &url)],
                }
            }
            StatusChanged {
                name,
                uuid,
                project_uuid,
                environment_uuid,
                ..
            } => TelegramMessage {
                message: format!("Coolify: {name} has been stopped."),
                buttons: vec![button(
                    "Open Application in Coolify",
                    &status_url(ctx, project_uuid, environment_uuid, uuid),
                )],
            },
            RestartLimitReached(r) => TelegramMessage {
                message: format!(
                    "Coolify: {} has been stopped after {} restarts (limit: {}).",
                    r.resource_name, r.restart_count, r.max_restart_count
                ),
                buttons: vec![button("Open Resource in Coolify", &r.url(ctx))],
            },
            ContainerRestarted {
                name, server, url, ..
            } => TelegramMessage {
                message: format!(
                    "Coolify: A resource ({name}) has been restarted automatically on {}",
                    server.name
                ),
                // Upstream nests the button one level too deep, so the job sees no text or url.
                buttons: match url.as_deref().filter(|u| truthy(Some(u))) {
                    Some(u) => vec![json!([button("Check Proxy in Coolify", u)])],
                    None => Vec::new(),
                },
            },
            BackupSuccess(b) => telegram(format!(
                "Coolify: Database backup for {} (db:{}) with frequency of {} was successful.",
                b.name,
                b.db(),
                b.frequency
            )),
            BackupFailed { backup: b, output } => telegram(format!(
                "Coolify: Database backup for {} (db:{}) with frequency of {} was FAILED.\n\nReason:\n{output}",
                b.name,
                b.db(),
                b.frequency
            )),
            BackupMissing {
                database_name,
                days,
                ..
            } => telegram(format!(
                "Coolify: {}",
                backup_missing_description(database_name, days)
            )),
            BackupSuccessWithS3Warning {
                backup: b,
                s3_error,
                s3_uuid,
            } => {
                let mut message = format!(
                    "Coolify: Database backup for {} (db:{}) with frequency of {} succeeded locally but failed to upload to S3.\n\nS3 Error:\n{s3_error}",
                    b.name,
                    b.db(),
                    b.frequency
                );
                if let Some(u) = s3_url(ctx, s3_uuid.as_deref()) {
                    message.push_str(&format!("\n\nCheck S3 Configuration: {u}"));
                }
                telegram(message)
            }
            General { message, .. } => telegram(message),
            TaskSuccess(t) => telegram(format!("Coolify: Scheduled task ({}) succeeded.", t.name)),
            TaskFailed(t) => telegram(format!(
                "Coolify: Scheduled task ({}) failed with output: {}",
                t.name, t.output
            )),
            DockerCleanupSuccess { server, message } => telegram(format!(
                "Coolify: Docker cleanup job succeeded on {}!\n\n{message}",
                server.name
            )),
            DockerCleanupFailed { server, message } => telegram(format!(
                "Coolify: [ACTION REQUIRED] Docker cleanup job failed on {}!\n\n{message}",
                server.name
            )),
            ForceDisabled { server } => telegram(format!(
                "Coolify: Server ({}) disabled because it is not paid!\n All automations and integrations are stopped.\nPlease update your subscription to enable the server again [here](https://app.coolify.io/subscription).",
                server.name
            )),
            ForceEnabled { server } => {
                telegram(format!("Coolify: Server ({}) enabled again!", server.name))
            }
            HetznerDeletionFailed {
                hetzner_server_id: id,
                error,
            } => telegram(format!(
                "Coolify: [ACTION REQUIRED] Failed to delete Hetzner server #{id} from Hetzner Cloud.\n\nError: {error}\n\nThe server has been removed from Coolify, but may still exist in your Hetzner Cloud account. Please check your Hetzner Cloud console and manually delete the server if needed."
            )),
            HighDiskUsage {
                server,
                disk_usage,
                threshold,
                ..
            } => telegram(format!(
                "Coolify: Server '{}' high disk usage detected!\nDisk usage: {disk_usage}%. Threshold: {threshold}%.\nPlease cleanup your disk to prevent data-loss.\nHere are some tips: https://coolify.io/docs/knowledge-base/server/automated-cleanup.",
                server.name
            )),
            Reachable { server } => telegram(format!(
                "Coolify: Server '{}' revived. All automations & integrations are turned on again!",
                server.name
            )),
            Unreachable { server } => telegram(format!(
                "Coolify: Your server '{}' is unreachable. All automations & integrations are turned off! Please check your server! IMPORTANT: We automatically try to revive your server and turn on all automations & integrations.",
                server.name
            )),
            ServerPatchCheck { server, patch_data } => {
                let url = patch_url(ctx, server);
                let os = ucfirst(&patch_str(patch_data, "osId", "unknown"));
                let manager = patch_str(patch_data, "package_manager", "unknown");
                if let Some(error) = patch_error(patch_data) {
                    return TelegramMessage {
                        message: format!(
                            "❌ Coolify: [ERROR] Failed to check patches on {}!\n\n📊 Error Details:\n• OS: {os}\n• Package Manager: {manager}\n• Error: {error}\n\n",
                            server.name
                        ),
                        buttons: vec![button("Manage Server", &url)],
                    };
                }
                let total = php_string(&patch_total(patch_data));
                let updates = patch_updates(patch_data);
                let mut message = format!(
                    "🔧 Coolify: [ACTION REQUIRED] {total} server patches available on {}!\n\n📊 Summary:\n• OS: {os}\n• Package Manager: {manager}\n• Total Updates: {total}\n\n",
                    server.name
                );
                if !updates.is_empty() {
                    message.push_str("📦 Sample Updates:\n");
                    for u in updates.iter().take(5) {
                        message.push_str(&update_line(u, false));
                    }
                    if updates.len() > 5 {
                        message
                            .push_str(&format!("• ... and {} more packages\n", updates.len() - 5));
                    }
                    let crit = critical(&updates);
                    if !crit.is_empty() {
                        message.push_str(&format!(
                            "\n⚠️ Critical packages detected: {} packages may require restarts\n",
                            crit.len()
                        ));
                        for p in crit.iter().take(3) {
                            message.push_str(&update_line(p, false));
                        }
                        if crit.len() > 3 {
                            message.push_str(&format!(
                                "• ... and {} more critical packages\n",
                                crit.len() - 3
                            ));
                        }
                    }
                }
                TelegramMessage {
                    message,
                    buttons: vec![button("Manage Server Patches", &url)],
                }
            }
            TraefikVersionOutdated { servers } => {
                let mut message = format!(
                    "⚠️ Coolify: Traefik proxy outdated on {} server(s)!\n\nUpdate recommended for security and features.\n📊 Affected servers:\n",
                    servers.len()
                );
                for s in servers {
                    message.push_str(&traefik_lines(
                        s,
                        false,
                        "  ↳ Also available",
                        "latest patch",
                    ));
                }
                message.push_str(
                    "\n⚠️ It is recommended to test before switching the production version.",
                );
                if traefik_has_upgrades(servers) {
                    message.push_str("\n\n📖 For minor version upgrades: Read the Traefik changelog before upgrading to understand breaking changes and new features.");
                }
                telegram(message)
            }
            VolumeBackupSuccess { backup: b, warning } => TelegramMessage {
                message: format!(
                    "Coolify: {}\n\nFrequency: {}",
                    volume_success_summary(b, warning.as_deref()),
                    b.frequency
                ),
                buttons: b.open_button(),
            },
            VolumeBackupFailed { backup: b, output } => TelegramMessage {
                message: format!(
                    "Coolify: {}\n\nFrequency: {}\n\nReason:\n{output}",
                    volume_failed_summary(b),
                    b.frequency
                ),
                buttons: b.open_button(),
            },
            VolumeBackupMissing {
                backup: b, days, ..
            } => TelegramMessage {
                message: format!("Coolify: {}", volume_missing_summary(b, days)),
                buttons: b.open_button(),
            },
            VolumeBackupRecoveryFailed {
                backup,
                recovery_error,
                containers_stopped,
                ..
            } => TelegramMessage {
                message: format!(
                    "Coolify: [ACTION REQUIRED] {}",
                    recovery_summary(
                        backup.as_ref(),
                        recovery_error.as_deref(),
                        *containers_stopped
                    )
                ),
                buttons: backup
                    .as_ref()
                    .map(VolumeBackup::open_button)
                    .unwrap_or_default(),
            },
            ApiTokenExpiring {
                token_name,
                expires_at,
            } => TelegramMessage {
                message: format!(
                    "Coolify: API token '{token_name}' expires on {}.\n\nAction Required: Rotate this token before it expires to avoid API outages.",
                    expires_at.as_deref().unwrap_or_default()
                ),
                buttons: vec![button(
                    "Manage API tokens",
                    &format!("{}/security/api-tokens", ctx.base_url),
                )],
            },
            SslExpiration { resources } => TelegramMessage {
                message: format!(
                    "Coolify: SSL certificates have been renewed for: {}.\n\nAction Required: These resources need to be redeployed manually for the new SSL certificates to take effect.",
                    ssl_names(resources)
                ),
                buttons: ssl_urls(ctx, resources)
                    .iter()
                    .map(|(n, u)| button(&format!("View {n}"), u))
                    .collect(),
            },
            Test { .. } => TelegramMessage {
                message: "Coolify: This is a test Telegram notification from Coolify.".into(),
                buttons: vec![button(
                    "Go to your dashboard",
                    if ctx.dev {
                        "https://staging-but-dev.coolify.io"
                    } else {
                        &ctx.base_url
                    },
                )],
            },
        }
    }

    /// `toPushover()`.
    pub fn to_pushover(&self, ctx: &Ctx) -> PushoverMessage {
        use Notification::*;
        use PushoverMessage as P;
        match self {
            DeploymentSuccess(d) => {
                let url = d.url(ctx);
                let mut buttons = Vec::new();
                let (title, message) = if let Some(p) = &d.preview {
                    if truthy(p.fqdn.as_deref()) {
                        buttons.push(button(
                            "Open Application",
                            p.fqdn.as_deref().unwrap_or_default(),
                        ));
                    }
                    (
                        format!("Pull request #{} successfully deployed", p.pull_request_id),
                        format!(
                            "New PR{} version successfully deployed of {}",
                            p.pull_request_id, d.application_name
                        ),
                    )
                } else {
                    let f = first_fqdn(d.fqdn.as_deref());
                    if truthy(f.as_deref()) {
                        buttons.push(button("Open Application", f.as_deref().unwrap_or_default()));
                    }
                    (
                        "New version successfully deployed".to_owned(),
                        format!("New version successfully deployed of {}", d.application_name),
                    )
                };
                buttons.push(button("Deployment logs", &url));
                P::new(title, "success", message).with_buttons(buttons)
            }
            DeploymentFailed(d) => {
                let url = d.url(ctx);
                let (title, message) = if let Some(p) = &d.preview {
                    (
                        format!("Pull request #{} deployment failed", p.pull_request_id),
                        format!("Pull request deployment failed for {}", d.application_name),
                    )
                } else {
                    (
                        "Deployment failed".to_owned(),
                        format!("Deployment failed for {}", d.application_name),
                    )
                };
                P::new(title, "error", message)
                    .with_buttons(vec![button("Deployment logs", &url)])
            }
            StatusChanged {
                name,
                uuid,
                project_uuid,
                environment_uuid,
                ..
            } => P::new(
                "Application stopped",
                "error",
                format!("{name} has been stopped."),
            )
            .with_buttons(vec![button(
                "Open Application in Coolify",
                &status_url(ctx, project_uuid, environment_uuid, uuid),
            )]),
            RestartLimitReached(r) => P::new(
                "Restart limit reached",
                "error",
                format!(
                    "{} has been stopped after {} restarts (limit: {}).",
                    r.resource_name, r.restart_count, r.max_restart_count
                ),
            )
            .with_buttons(vec![button("Open Resource in Coolify", &r.url(ctx))]),
            ContainerRestarted {
                name, server, url, ..
            } => {
                let buttons = match url.as_deref().filter(|u| truthy(Some(u))) {
                    Some(u) => vec![button("Check Proxy in Coolify", u)],
                    None => Vec::new(),
                };
                P::new(
                    "Resource restarted",
                    "warning",
                    format!(
                        "A resource ({name}) has been restarted automatically on {}",
                        server.name
                    ),
                )
                .with_buttons(buttons)
            }
            BackupSuccess(b) => P::new(
                "Database backup successful",
                "success",
                format!(
                    "Database backup for {} (db:{}) was successful.<br/><br/><b>Frequency:</b> {}.",
                    b.name,
                    b.db(),
                    b.frequency
                ),
            ),
            BackupFailed { backup: b, output } => P::new(
                "Database backup failed",
                "error",
                format!(
                    "Database backup for {} (db:{}) was FAILED<br/><br/><b>Frequency:</b> {} .<br/><b>Reason:</b> {output}",
                    b.name,
                    b.db(),
                    b.frequency
                ),
            ),
            BackupMissing {
                database_name,
                days,
                ..
            } => P::new(
                "Scheduled database backup missing",
                "error",
                backup_missing_description(database_name, days),
            ),
            BackupSuccessWithS3Warning {
                backup: b,
                s3_error,
                s3_uuid,
            } => {
                let mut message = format!(
                    "Database backup for {} (db:{}) was created successfully on local storage, but failed to upload to S3.<br/><br/><b>Frequency:</b> {}.<br/><b>S3 Error:</b> {s3_error}",
                    b.name,
                    b.db(),
                    b.frequency
                );
                if let Some(u) = s3_url(ctx, s3_uuid.as_deref()) {
                    message.push_str(&format!(
                        "<br/><br/><a href=\"{u}\">Check S3 Configuration</a>"
                    ));
                }
                P::new(
                    "Database backup succeeded locally, S3 upload failed",
                    "warning",
                    message,
                )
            }
            General { message, success } => {
                if *success {
                    P::new("General Notification", "info", message)
                } else {
                    P::new("Action required", "error", message)
                }
            }
            TaskSuccess(t) => {
                let buttons = t
                    .url(ctx)
                    .map(|u| vec![button("Open task in Coolify", &u)])
                    .unwrap_or_default();
                P::new(
                    "Scheduled task succeeded",
                    "success",
                    format!("Coolify: Scheduled task ({}) succeeded.", t.name),
                )
                .with_buttons(buttons)
            }
            TaskFailed(t) => {
                let mut message = format!("Scheduled task ({}) failed<br/>", t.name);
                if truthy(Some(&t.output)) {
                    message.push_str(&format!("<br/><b>Error Output:</b>{}", t.output));
                }
                let buttons = t
                    .url(ctx)
                    .map(|u| vec![button("Open task in Coolify", &u)])
                    .unwrap_or_default();
                P::new("Scheduled task failed", "error", message).with_buttons(buttons)
            }
            DockerCleanupSuccess { server, message } => P::new(
                "Docker cleanup job succeeded",
                "success",
                format!(
                    "Docker cleanup job succeeded on {}!\n\n{message}",
                    server.name
                ),
            ),
            DockerCleanupFailed { server, message } => P::new(
                "Docker cleanup job failed",
                "error",
                format!(
                    "[ACTION REQUIRED] Docker cleanup job failed on {}!\n\n{message}",
                    server.name
                ),
            ),
            ForceDisabled { server } => P::new(
                "Server disabled",
                "error",
                format!(
                    "Server ({}) disabled because it is not paid!\n All automations and integrations are stopped.<br/>Please update your subscription to enable the server again [here](https://app.coolify.io/subscription).",
                    server.name
                ),
            ),
            ForceEnabled { server } => P::new(
                "Server enabled",
                "success",
                format!("Server ({}) enabled again!", server.name),
            ),
            HetznerDeletionFailed {
                hetzner_server_id: id,
                error,
            } => P::new(
                "Hetzner Server Deletion Failed",
                "error",
                format!(
                    "[ACTION REQUIRED] Failed to delete Hetzner server #{id}.\n\nError: {error}\n\nThe server has been removed from Coolify, but may still exist in your Hetzner Cloud account. Please check and manually delete if needed."
                ),
            ),
            HighDiskUsage {
                server,
                disk_usage,
                threshold,
                ..
            } => P::new(
                "High disk usage detected",
                "warning",
                format!(
                    "Server '{}' high disk usage detected!<br/><br/><b>Disk usage:</b> {disk_usage}%.<br/><b>Threshold:</b> {threshold}%.<br/>Please cleanup your disk to prevent data-loss.",
                    server.name
                ),
            )
            // Upstream passes `['Change settings' => $url, ...]`: two entries without `url`.
            .with_buttons(vec![
                json!(format!("{}/server/{}#advanced", ctx.base_url, server.uuid)),
                json!("https://coolify.io/docs/knowledge-base/server/automated-cleanup"),
            ]),
            Reachable { server } => P::new(
                "Server revived",
                "success",
                format!(
                    "Server '{}' revived. All automations & integrations are turned on again!",
                    server.name
                ),
            ),
            Unreachable { server } => P::new(
                "Server unreachable",
                "error",
                format!(
                    "Your server '{}' is unreachable.<br/>All automations & integrations are turned off!<br/><br/><b>IMPORTANT:</b> We automatically try to revive your server and turn on all automations & integrations.",
                    server.name
                ),
            ),
            ServerPatchCheck { server, patch_data } => {
                let url = patch_url(ctx, server);
                let os = ucfirst(&patch_str(patch_data, "osId", "unknown"));
                let manager = patch_str(patch_data, "package_manager", "unknown");
                if let Some(error) = patch_error(patch_data) {
                    return P::new(
                        "Server patch check failed",
                        "error",
                        format!(
                            "[ERROR] Failed to check patches on {}!\n\nError Details:\n• OS: {os}\n• Package Manager: {manager}\n• Error: {error}\n\n",
                            server.name
                        ),
                    )
                    .with_buttons(vec![button("Manage Server", &url)]);
                }
                let total = php_string(&patch_total(patch_data));
                let updates = patch_updates(patch_data);
                let mut message = format!(
                    "[ACTION REQUIRED] {total} server patches available on {}!\n\nSummary:\n• OS: {os}\n• Package Manager: {manager}\n• Total Updates: {total}\n\n",
                    server.name
                );
                if !updates.is_empty() {
                    message.push_str("Sample Updates:\n");
                    for u in updates.iter().take(3) {
                        message.push_str(&update_line(u, false));
                    }
                    if updates.len() > 3 {
                        message
                            .push_str(&format!("• ... and {} more packages\n", updates.len() - 3));
                    }
                    let crit = critical(&updates);
                    if !crit.is_empty() {
                        message.push_str(&format!(
                            "\nCritical packages detected: {} may require restarts",
                            crit.len()
                        ));
                    }
                }
                P::new("Server patches available", "error", message)
                    .with_buttons(vec![button("Manage Server Patches", &url)])
            }
            TraefikVersionOutdated { servers } => {
                let mut message = format!(
                    "Traefik proxy outdated on {} server(s)!\nAffected servers:\n",
                    servers.len()
                );
                for s in servers {
                    message.push_str(&traefik_lines(s, false, "  Also", "latest"));
                }
                message.push_str(
                    "\nIt is recommended to test before switching the production version.",
                );
                if traefik_has_upgrades(servers) {
                    message.push_str(
                        "\n\nFor minor version upgrades: Read the Traefik changelog before upgrading.",
                    );
                }
                P::new("Traefik proxy outdated", "warning", message)
            }
            VolumeBackupSuccess { backup: b, warning } => {
                let warned = truthy(warning.as_deref());
                P::new(
                    if warned {
                        "Storage backup succeeded with a warning"
                    } else {
                        "Storage backup successful"
                    },
                    if warned { "warning" } else { "success" },
                    format!(
                        "{}<br/><br/><b>Frequency:</b> {}.",
                        volume_success_summary(b, warning.as_deref()),
                        b.frequency
                    ),
                )
                .with_buttons(b.open_button())
            }
            VolumeBackupFailed { backup: b, output } => P::new(
                "Storage backup failed",
                "error",
                format!(
                    "{}<br/><br/><b>Frequency:</b> {}.<br/><b>Reason:</b> {output}",
                    volume_failed_summary(b),
                    b.frequency
                ),
            )
            .with_buttons(b.open_button()),
            VolumeBackupMissing { backup: b, days, .. } => P::new(
                "Scheduled storage backup missing",
                "error",
                volume_missing_summary(b, days),
            )
            .with_buttons(b.open_button()),
            VolumeBackupRecoveryFailed {
                backup,
                recovery_error,
                containers_stopped,
                ..
            } => P::new(
                "Storage backup recovery failed",
                "error",
                format!(
                    "[ACTION REQUIRED] {}",
                    recovery_summary(
                        backup.as_ref(),
                        recovery_error.as_deref(),
                        *containers_stopped
                    )
                ),
            )
            .with_buttons(backup.as_ref().map(VolumeBackup::open_button).unwrap_or_default()),
            ApiTokenExpiring {
                token_name,
                expires_at,
            } => P::new(
                "API token expiring soon",
                "warning",
                format!(
                    "API token <b>{token_name}</b> expires on {}.<br/><br/><b>Action Required:</b> Rotate this token before it expires to avoid API outages.",
                    expires_at.as_deref().unwrap_or_default()
                ),
            )
            .with_buttons(vec![button(
                "Manage API tokens",
                &format!("{}/security/api-tokens", ctx.base_url),
            )]),
            SslExpiration { resources } => P::new(
                "SSL Certificates Renewed",
                "warning",
                format!(
                    "SSL certificates have been renewed for: {}<br/><br/><b>Action Required:</b> These resources need to be redeployed manually for the new SSL certificates to take effect.",
                    ssl_names(resources)
                ),
            )
            .with_buttons(
                ssl_urls(ctx, resources)
                    .iter()
                    .map(|(n, u)| button(&format!("View {n}"), u))
                    .collect(),
            ),
            Test { .. } => P::new(
                "Test Pushover Notification",
                "info",
                "This is a test Pushover notification from Coolify.",
            )
            .with_buttons(vec![button("Go to your dashboard", &ctx.base_url)]),
        }
    }

    /// `toSlack()`.
    pub fn to_slack(&self, ctx: &Ctx) -> SlackMessage {
        use Notification::*;
        match self {
            DeploymentSuccess(d) => {
                let url = d.url(ctx);
                let (title, mut description) = if let Some(p) = &d.preview {
                    let mut de = format!(
                        "New version successfully deployed for {}",
                        d.application_name
                    );
                    if truthy(p.fqdn.as_deref()) {
                        de.push_str(&format!(
                            "\nPreview URL: {}",
                            p.fqdn.as_deref().unwrap_or_default()
                        ));
                    }
                    (
                        format!("Pull request #{} successfully deployed", p.pull_request_id),
                        de,
                    )
                } else {
                    let mut de = format!(
                        "New version successfully deployed for {}",
                        d.application_name
                    );
                    let f = first_fqdn(d.fqdn.as_deref());
                    if truthy(f.as_deref()) {
                        de.push_str(&format!(
                            "\nApplication URL: {}",
                            f.as_deref().unwrap_or_default()
                        ));
                    }
                    ("New version successfully deployed".to_owned(), de)
                };
                description.push_str(&format!(
                    "\n\n*Project:* {}\n*Environment:* {}\n*<{url}|Deployment Logs>*",
                    d.project_name, d.environment_name
                ));
                slack(title, description, SLACK_SUCCESS)
            }
            DeploymentFailed(d) => {
                let url = d.url(ctx);
                let (title, mut description) = if let Some(p) = &d.preview {
                    let mut de =
                        format!("Pull request deployment failed for {}", d.application_name);
                    if truthy(p.fqdn.as_deref()) {
                        de.push_str(&format!(
                            "\nPreview URL: {}",
                            p.fqdn.as_deref().unwrap_or_default()
                        ));
                    }
                    (
                        format!("Pull request #{} deployment failed", p.pull_request_id),
                        de,
                    )
                } else {
                    let mut de = format!("Deployment failed for {}", d.application_name);
                    let f = first_fqdn(d.fqdn.as_deref());
                    if truthy(f.as_deref()) {
                        de.push_str(&format!(
                            "\nApplication URL: {}",
                            f.as_deref().unwrap_or_default()
                        ));
                    }
                    ("Deployment failed".to_owned(), de)
                };
                description.push_str(&format!(
                    "\n\n*Project:* {}\n*Environment:* {}\n*<{url}|Deployment Logs>*",
                    d.project_name, d.environment_name
                ));
                slack(title, description, SLACK_ERROR)
            }
            StatusChanged {
                name,
                uuid,
                project_name,
                project_uuid,
                environment_name,
                environment_uuid,
                ..
            } => slack(
                "Application stopped",
                format!(
                    "{name} has been stopped\n\n*Project:* {project_name}\n*Environment:* {environment_name}\n*Application URL:* {}",
                    status_url(ctx, project_uuid, environment_uuid, uuid)
                ),
                SLACK_ERROR,
            ),
            RestartLimitReached(r) => slack(
                "Restart limit reached",
                format!(
                    "{} has been stopped after {} restarts (limit: {})\n\n*Project:* {}\n*Environment:* {}\n*Resource URL:* {}",
                    r.resource_name,
                    r.restart_count,
                    r.max_restart_count,
                    r.project_name,
                    r.environment_name,
                    r.url(ctx)
                ),
                SLACK_ERROR,
            ),
            ContainerRestarted {
                name, server, url, ..
            } => {
                let mut description = format!(
                    "A resource ({name}) has been restarted automatically on {}",
                    server.name
                );
                if truthy(url.as_deref()) {
                    description.push_str(&format!(
                        "\n*Resource URL:* {}",
                        url.as_deref().unwrap_or_default()
                    ));
                }
                slack("Resource restarted", description, SLACK_WARNING)
            }
            BackupSuccess(b) => slack(
                "Database backup successful",
                format!(
                    "Database backup for {} (db:{}) was successful.\n\n*Frequency:* {}",
                    b.name,
                    b.db(),
                    b.frequency
                ),
                SLACK_SUCCESS,
            ),
            BackupFailed { backup: b, output } => slack(
                "Database backup failed",
                format!(
                    "Database backup for {} (db:{}) has FAILED.\n\n*Frequency:* {}\n\n*Error Output:* {output}",
                    b.name,
                    b.db(),
                    b.frequency
                ),
                SLACK_ERROR,
            ),
            BackupMissing {
                database_name,
                days,
                ..
            } => slack(
                "Scheduled database backup missing",
                backup_missing_description(database_name, days),
                SLACK_ERROR,
            ),
            BackupSuccessWithS3Warning {
                backup: b,
                s3_error,
                s3_uuid,
            } => {
                let mut description = format!(
                    "Database backup for {} (db:{}) was created successfully on local storage, but failed to upload to S3.\n\n*Frequency:* {}\n\n*S3 Error:* {s3_error}",
                    b.name,
                    b.db(),
                    b.frequency
                );
                if let Some(u) = s3_url(ctx, s3_uuid.as_deref()) {
                    description.push_str(&format!("\n\n*S3 Storage:* <{u}|Check Configuration>"));
                }
                slack(
                    "Database backup succeeded locally, S3 upload failed",
                    description,
                    SLACK_WARNING,
                )
            }
            General { message, success } => {
                if *success {
                    slack("Coolify: General Notification", message, SLACK_INFO)
                } else {
                    slack("Coolify: Action required", message, SLACK_ERROR)
                }
            }
            TaskSuccess(t) => {
                let mut description = format!("Scheduled task ({}) succeeded.", t.name);
                if let Some(u) = t.url(ctx) {
                    description.push_str(&format!("\n\n*Task URL:* {u}"));
                }
                slack("Scheduled task succeeded", description, SLACK_SUCCESS)
            }
            TaskFailed(t) => {
                let mut description = format!("Scheduled task ({}) failed.", t.name);
                if truthy(Some(&t.output)) {
                    description.push_str(&format!("\n\n*Error Output:* {}", t.output));
                }
                if let Some(u) = t.url(ctx) {
                    description.push_str(&format!("\n\n*Task URL:* {u}"));
                }
                slack("Scheduled task failed", description, SLACK_ERROR)
            }
            DockerCleanupSuccess { server, message } => slack(
                "Coolify: Docker cleanup job succeeded",
                format!(
                    "Docker cleanup job succeeded on '{}'!\n\n{message}",
                    server.name
                ),
                SLACK_SUCCESS,
            ),
            DockerCleanupFailed { server, message } => slack(
                "Coolify: [ACTION REQUIRED] Docker cleanup job failed",
                format!(
                    "Docker cleanup job failed on '{}'!\n\n{message}",
                    server.name
                ),
                SLACK_ERROR,
            ),
            ForceDisabled { server } => slack(
                "Server disabled",
                format!(
                    "Server ({}) disabled because it is not paid!\nAll automations and integrations are stopped.\n\nPlease update your subscription to enable the server again: https://app.coolify.io/subscription",
                    server.name
                ),
                SLACK_ERROR,
            ),
            ForceEnabled { server } => slack(
                "Server enabled",
                format!("Server '{}' enabled again!", server.name),
                SLACK_SUCCESS,
            ),
            HetznerDeletionFailed {
                hetzner_server_id: id,
                error,
            } => slack(
                "Coolify: [ACTION REQUIRED] Hetzner Server Deletion Failed",
                format!(
                    "Failed to delete Hetzner server #{id} from Hetzner Cloud.\n\nError: {error}\n\nThe server has been removed from Coolify, but may still exist in your Hetzner Cloud account. Please check your Hetzner Cloud console and manually delete the server if needed."
                ),
                SLACK_ERROR,
            ),
            HighDiskUsage {
                server,
                disk_usage,
                threshold,
                ..
            } => slack(
                "High disk usage detected",
                format!(
                    "Server '{0}' high disk usage detected!\nDisk usage: {disk_usage}%\nThreshold: {threshold}%\n\nPlease cleanup your disk to prevent data-loss.\nTips for cleanup: https://coolify.io/docs/knowledge-base/server/automated-cleanup\nChange settings:\n- Threshold: {1}/server/{2}#advanced\n- Notifications: {1}/notifications/slack",
                    server.name, ctx.base_url, server.uuid
                ),
                SLACK_ERROR,
            ),
            Reachable { server } => slack(
                "Server revived",
                format!(
                    "Server '{}' revived.\nAll automations & integrations are turned on again!",
                    server.name
                ),
                SLACK_SUCCESS,
            ),
            Unreachable { server } => slack(
                "Server unreachable",
                format!(
                    "Your server '{}' is unreachable.\nAll automations & integrations are turned off!\n\n*IMPORTANT:* We automatically try to revive your server and turn on all automations & integrations.",
                    server.name
                ),
                SLACK_ERROR,
            ),
            ServerPatchCheck { server, patch_data } => {
                let url = patch_url(ctx, server);
                let os = ucfirst(&patch_str(patch_data, "osId", "unknown"));
                let manager = patch_str(patch_data, "package_manager", "unknown");
                if let Some(error) = patch_error(patch_data) {
                    return slack(
                        "Coolify: [ERROR] Server patch check failed",
                        format!(
                            "Failed to check patches on '{}'!\n\n*Error Details:*\n• OS: {os}\n• Package Manager: {manager}\n• Error: `{error}`\n\n\n:link: <{url}|Manage Server>",
                            server.name
                        ),
                        SLACK_ERROR,
                    );
                }
                let total = php_string(&patch_total(patch_data));
                let updates = patch_updates(patch_data);
                let mut description = format!(
                    "{total} server patches available on '{}'!\n\n*Summary:*\n• OS: {os}\n• Package Manager: {manager}\n• Total Updates: {total}\n\n",
                    server.name
                );
                if !updates.is_empty() {
                    description.push_str("*Sample Updates:*\n");
                    for u in updates.iter().take(5) {
                        description.push_str(&update_line(u, true));
                    }
                    if updates.len() > 5 {
                        description
                            .push_str(&format!("• ... and {} more packages\n", updates.len() - 5));
                    }
                    let crit = critical(&updates);
                    if !crit.is_empty() {
                        description.push_str(&format!(
                            "\n:warning: *Critical packages detected:* {} packages may require restarts\n",
                            crit.len()
                        ));
                        for p in crit.iter().take(3) {
                            description.push_str(&update_line(p, true));
                        }
                        if crit.len() > 3 {
                            description.push_str(&format!(
                                "• ... and {} more critical packages\n",
                                crit.len() - 3
                            ));
                        }
                    }
                }
                description.push_str(&format!("\n:link: <{url}|Manage Server Patches>"));
                slack(
                    "Coolify: [ACTION REQUIRED] Server patches available",
                    description,
                    SLACK_ERROR,
                )
            }
            TraefikVersionOutdated { servers } => {
                let mut description = format!(
                    "Traefik proxy outdated on {} server(s)!\n*Affected servers:*\n",
                    servers.len()
                );
                for s in servers {
                    description.push_str(&traefik_lines(
                        s,
                        true,
                        "  ↳ Also available",
                        "latest patch",
                    ));
                }
                description.push_str(
                    "\n:warning: It is recommended to test before switching the production version.",
                );
                if traefik_has_upgrades(servers) {
                    description.push_str("\n\n:book: For minor version upgrades: Read the Traefik changelog before upgrading to understand breaking changes and new features.");
                }
                slack(
                    "Coolify: Traefik proxy outdated",
                    description,
                    SLACK_WARNING,
                )
            }
            VolumeBackupSuccess { backup: b, warning } => {
                let warned = truthy(warning.as_deref());
                slack(
                    if warned {
                        "Storage backup succeeded with a warning"
                    } else {
                        "Storage backup successful"
                    },
                    format!(
                        "{}\n\n*Frequency:* {}{}",
                        volume_success_summary(b, warning.as_deref()),
                        b.frequency,
                        b.slack_link()
                    ),
                    if warned { SLACK_WARNING } else { SLACK_SUCCESS },
                )
            }
            VolumeBackupFailed { backup: b, output } => slack(
                "Storage backup failed",
                format!(
                    "{}\n\n*Frequency:* {}\n\n*Error Output:* {output}{}",
                    volume_failed_summary(b),
                    b.frequency,
                    b.slack_link()
                ),
                SLACK_ERROR,
            ),
            VolumeBackupMissing {
                backup: b, days, ..
            } => slack(
                "Scheduled storage backup missing",
                format!("{}{}", volume_missing_summary(b, days), b.slack_link()),
                SLACK_ERROR,
            ),
            VolumeBackupRecoveryFailed {
                backup,
                recovery_error,
                containers_stopped,
                ..
            } => slack(
                "Coolify: [ACTION REQUIRED] Storage backup recovery failed",
                format!(
                    "{}{}",
                    recovery_summary(
                        backup.as_ref(),
                        recovery_error.as_deref(),
                        *containers_stopped
                    ),
                    backup
                        .as_ref()
                        .map(VolumeBackup::slack_link)
                        .unwrap_or_default()
                ),
                SLACK_ERROR,
            ),
            ApiTokenExpiring {
                token_name,
                expires_at,
            } => slack(
                "🔑 API token expiring soon",
                format!(
                    "API token *{token_name}* expires on {}.\n\n*Action Required:* Rotate this token before it expires to avoid API outages.\n\nManage tokens: {}/security/api-tokens",
                    expires_at.as_deref().unwrap_or_default(),
                    ctx.base_url
                ),
                SLACK_WARNING,
            ),
            SslExpiration { resources } => {
                let mut description = format!(
                    "SSL certificates have been renewed for: {}\n\n**Action Required:** These resources need to be redeployed manually for the new SSL certificates to take effect.",
                    ssl_names(resources)
                );
                let urls = ssl_urls(ctx, resources);
                if !urls.is_empty() {
                    description.push_str("\n\n**Resource URLs:**\n");
                    for (name, url) in urls {
                        description.push_str(&format!("• {name}: {url}\n"));
                    }
                }
                slack("🔒 SSL Certificates Renewed", description, SLACK_WARNING)
            }
            Test { .. } => slack(
                "Test Slack Notification",
                "This is a test Slack notification from Coolify.",
                SLACK_INFO,
            ),
        }
    }

    /// `toWebhook()`.
    pub fn to_webhook(&self, ctx: &Ctx) -> Value {
        use Notification::*;
        match self {
            DeploymentSuccess(d) => deployment_webhook(ctx, d, true),
            DeploymentFailed(d) => deployment_webhook(ctx, d, false),
            StatusChanged {
                name,
                uuid,
                project_name,
                project_uuid,
                environment_name,
                environment_uuid,
                fqdn,
            } => json!({
                "success": false,
                "message": "Application stopped",
                "event": "status_changed",
                "application_name": name,
                "application_uuid": uuid,
                "url": status_url(ctx, project_uuid, environment_uuid, uuid),
                "project": project_name,
                "environment": environment_name,
                "fqdn": first_fqdn(fqdn.as_deref()),
            }),
            RestartLimitReached(r) => json!({
                "success": false,
                "message": "Restart limit reached",
                "event": "restart_limit_reached",
                "application_name": r.resource_name,
                "application_uuid": r.resource_uuid,
                "resource_name": r.resource_name,
                "resource_uuid": r.resource_uuid,
                "restart_count": r.restart_count,
                "max_restart_count": r.max_restart_count,
                "url": r.url(ctx),
                // `data_get($this->resource, 'environment.project.name')`: previews and service
                // parts have no `environment` relation.
                "project": match r.kind {
                    RestartKind::Application | RestartKind::Database => Some(r.project_name.clone()),
                    _ => None,
                },
                "environment": r.environment_name,
                "fqdn": first_fqdn(r.fqdn.as_deref()),
            }),
            ContainerRestarted {
                name, server, url, ..
            } => {
                let mut d = json!({
                    "success": true,
                    "message": "Resource restarted automatically",
                    "event": "container_restarted",
                    "container_name": name,
                    "server_name": server.name,
                    "server_uuid": server.uuid,
                });
                if truthy(url.as_deref()) {
                    d["url"] = json!(url);
                }
                d
            }
            BackupSuccess(b) => json!({
                "success": true,
                "message": "Database backup successful",
                "event": "backup_success",
                "database_name": b.name,
                "database_uuid": b.uuid,
                "database_type": b.database_name,
                "frequency": b.frequency,
                "url": b.url(ctx),
            }),
            BackupFailed { backup: b, output } => json!({
                "success": false,
                "message": "Database backup failed",
                "event": "backup_failed",
                "database_name": b.name,
                "database_uuid": b.uuid,
                "database_type": b.database_name,
                "frequency": b.frequency,
                "error_output": output,
                "url": b.url(ctx),
            }),
            BackupMissing {
                database_name,
                backup_uuid,
                days,
                last_execution_at,
            } => json!({
                "database_name": database_name,
                "days": days,
                "last_execution_at": last_execution_at,
                "success": false,
                "message": "Scheduled database backup missing",
                "event": "backup_missing",
                "backup_uuid": backup_uuid,
            }),
            BackupSuccessWithS3Warning {
                backup: b,
                s3_error,
                s3_uuid,
            } => {
                let mut d = json!({
                    "success": true,
                    "message": "Database backup succeeded locally, S3 upload failed",
                    "event": "backup_success_with_s3_warning",
                    "database_name": b.name,
                    "database_uuid": b.uuid,
                    "database_type": b.database_name,
                    "frequency": b.frequency,
                    "s3_error": s3_error,
                    "url": b.url(ctx),
                });
                if let Some(u) = s3_url(ctx, s3_uuid.as_deref()) {
                    d["s3_storage_url"] = json!(u);
                }
                d
            }
            General { message, success } => json!({
                "success": success,
                "message": message,
                "event": "general",
                "url": ctx.base_url,
            }),
            TaskSuccess(t) => t.webhook(ctx, true),
            TaskFailed(t) => t.webhook(ctx, false),
            DockerCleanupSuccess { server, message } => json!({
                "success": true,
                "message": "Docker cleanup job succeeded",
                "event": "docker_cleanup_success",
                "server_name": server.name,
                "server_uuid": server.uuid,
                "cleanup_message": message,
                "url": server_url(ctx, server),
            }),
            DockerCleanupFailed { server, message } => json!({
                "success": false,
                "message": "Docker cleanup job failed",
                "event": "docker_cleanup_failed",
                "server_name": server.name,
                "server_uuid": server.uuid,
                "error_message": message,
                "url": server_url(ctx, server),
            }),
            ForceDisabled { server } => json!({
                "success": false,
                "message": format!("Server ({}) disabled because it is not paid! All automations and integrations are stopped.", server.name),
                "event": "server_force_disabled",
                "server_name": server.name,
                "server_uuid": server.uuid,
                "url": server_url(ctx, server),
            }),
            ForceEnabled { server } => json!({
                "success": true,
                "message": format!("Server ({}) enabled again!", server.name),
                "event": "server_force_enabled",
                "server_name": server.name,
                "server_uuid": server.uuid,
                "url": server_url(ctx, server),
            }),
            HetznerDeletionFailed {
                hetzner_server_id: id,
                error,
            } => json!({
                "success": false,
                "message": format!("[ACTION REQUIRED] Failed to delete Hetzner server #{id} from Hetzner Cloud. The server has been removed from Coolify, but may still exist in your Hetzner Cloud account."),
                "event": "hetzner_deletion_failed",
                "hetzner_server_id": id,
                "error": error,
                "url": format!("{}/servers", ctx.base_url),
            }),
            HighDiskUsage {
                server,
                disk_usage,
                threshold,
                ..
            } => json!({
                "success": false,
                "message": "High disk usage detected",
                "event": "high_disk_usage",
                "server_name": server.name,
                "server_uuid": server.uuid,
                "disk_usage": disk_usage,
                "threshold": threshold,
                "url": server_url(ctx, server),
            }),
            Reachable { server } => json!({
                "success": true,
                "message": "Server revived",
                "event": "server_reachable",
                "server_name": server.name,
                "server_uuid": server.uuid,
                "url": server_url(ctx, server),
            }),
            Unreachable { server } => json!({
                "success": false,
                "message": "Server unreachable",
                "event": "server_unreachable",
                "server_name": server.name,
                "server_uuid": server.uuid,
                "url": server_url(ctx, server),
            }),
            ServerPatchCheck { server, patch_data } => {
                let url = patch_url(ctx, server);
                let os = patch_str(patch_data, "osId", "unknown");
                let manager = patch_str(patch_data, "package_manager", "unknown");
                if let Some(error) = patch_error(patch_data) {
                    return json!({
                        "success": false,
                        "message": "Failed to check patches",
                        "event": "server_patch_check_error",
                        "server_name": server.name,
                        "server_uuid": server.uuid,
                        "os_id": os,
                        "package_manager": manager,
                        "error": error,
                        "url": url,
                    });
                }
                let updates = patch_updates(patch_data);
                let crit = critical(&updates).len();
                json!({
                    "success": false,
                    "message": "Server patches available",
                    "event": "server_patch_check",
                    "server_name": server.name,
                    "server_uuid": server.uuid,
                    "total_updates": patch_total(patch_data),
                    "os_id": os,
                    "package_manager": manager,
                    "updates": updates,
                    "critical_packages_count": crit,
                    "security_updates": patch_data.get("security_updates"),
                    "reboot_required": patch_data.get("reboot_required"),
                    "url": url,
                })
            }
            TraefikVersionOutdated { servers } => {
                let list: Vec<Value> = servers
                    .iter()
                    .map(|s| {
                        let mut d = json!({
                            "name": s.name,
                            "uuid": s.uuid,
                            "current_version": s.current.as_deref().unwrap_or("unknown"),
                            "latest_version": s.latest.as_deref().unwrap_or("unknown"),
                            "update_type": s.kind(),
                        });
                        if s.kind() == "minor_upgrade"
                            && let Some(t) = &s.upgrade_target
                        {
                            d["upgrade_target"] = json!(t);
                        }
                        if let Some(t) = &s.newer_branch_target {
                            d["newer_branch_target"] = json!(t);
                            d["newer_branch_latest"] = json!(s.newer_branch_latest);
                        }
                        d
                    })
                    .collect();
                json!({
                    "success": false,
                    "message": "Traefik proxy outdated",
                    "event": "traefik_version_outdated",
                    "affected_servers_count": servers.len(),
                    "servers": list,
                })
            }
            VolumeBackupSuccess { backup: b, warning } => json!({
                "success": true,
                "message": if truthy(warning.as_deref()) {
                    "Storage backup succeeded with a warning"
                } else {
                    "Storage backup successful"
                },
                "event": "volume_backup_success",
                "resource_name": b.resource(),
                "storage": b.target,
                "frequency": b.frequency,
                "warning": warning,
                "url": b.url,
            }),
            VolumeBackupFailed { backup: b, output } => json!({
                "success": false,
                "message": "Storage backup failed",
                "event": "volume_backup_failed",
                "resource_name": b.resource(),
                "storage": b.target,
                "frequency": b.frequency,
                "error_output": output,
                "url": b.url,
            }),
            VolumeBackupMissing {
                backup: b,
                backup_uuid,
                days,
                last_execution_at,
            } => json!({
                "success": false,
                "message": "Scheduled storage backup missing",
                "event": "volume_backup_missing",
                "resource_name": b.resource(),
                "storage": b.target,
                "backup_uuid": backup_uuid,
                "days": days,
                "last_execution_at": last_execution_at,
                "url": b.url,
            }),
            VolumeBackupRecoveryFailed {
                backup,
                execution_uuid,
                recovery_error,
                containers_stopped,
            } => {
                let (name, target, url) = recovery_parts(backup.as_ref());
                json!({
                    "success": false,
                    "message": "Storage backup recovery failed",
                    "event": "volume_backup_recovery_failed",
                    "resource_name": name,
                    "storage": target,
                    "execution_uuid": execution_uuid,
                    "reason": recovery_error,
                    "containers_stopped": containers_stopped,
                    "url": url,
                })
            }
            ApiTokenExpiring {
                token_name,
                expires_at,
            } => {
                let expires = expires_at.as_deref().unwrap_or_default();
                json!({
                    "success": false,
                    "message": format!("API token '{token_name}' expires on {expires}. Rotate this token before it expires to avoid API outages."),
                    "event": "api_token_expiring",
                    "token_name": token_name,
                    "expires_at": expires,
                    "url": format!("{}/security/api-tokens", ctx.base_url),
                })
            }
            SslExpiration { resources } => {
                let names = ssl_names(resources);
                let urls = ssl_urls(ctx, resources);
                let list: Vec<&str> = resources.iter().map(|r| r.name.as_str()).collect();
                let urls_value = if urls.is_empty() {
                    json!([])
                } else {
                    Value::Object(urls.into_iter().map(|(n, u)| (n, json!(u))).collect())
                };
                json!({
                    "success": false,
                    "message": format!("SSL certificates have been renewed for: {names}. These resources need to be redeployed manually for the new SSL certificates to take effect."),
                    "event": "ssl_certificate_renewal",
                    "resources": list,
                    "urls": urls_value,
                    "url": ctx.base_url,
                })
            }
            Test { .. } => json!({
                "success": true,
                "message": "This is a test webhook notification from Coolify.",
                "event": "test",
                "url": ctx.base_url,
            }),
        }
    }
}

impl RestartLimit {
    /// `resolveResourceUrl`.
    fn url(&self, ctx: &Ctx) -> String {
        let segment = match self.kind {
            RestartKind::Application | RestartKind::Preview => "application",
            RestartKind::Service => "service",
            RestartKind::Database => "database",
        };
        format!(
            "{}/project/{}/environment/{}/{segment}/{}",
            ctx.base_url, self.project_uuid, self.environment_uuid, self.url_uuid
        )
    }
}

fn status_url(ctx: &Ctx, project: &str, environment: &str, uuid: &str) -> String {
    format!(
        "{}/project/{project}/environment/{environment}/application/{uuid}",
        ctx.base_url
    )
}

fn server_url(ctx: &Ctx, s: &ServerRef) -> String {
    format!("{}/server/{}", ctx.base_url, s.uuid)
}

fn patch_url(ctx: &Ctx, s: &ServerRef) -> String {
    format!("{}/server/{}/security/patches", ctx.base_url, s.uuid)
}

fn backup_missing_description(name: &str, days: &Value) -> String {
    format!(
        "The enabled backup schedule for {name} has produced no executions in the last {} day(s).",
        php_string(days)
    )
}

/// The per-server lines shared by Discord, Telegram, Pushover and Slack.
fn traefik_lines(s: &TraefikServer, code: bool, also: &str, latest_word: &str) -> String {
    let name = if code {
        format!("`{}`", s.name)
    } else {
        s.name.clone()
    };
    let (current, latest, target) = (s.current(), s.latest(), s.target());
    if s.is_patch() {
        let mut out = format!("• {name}: {current} → {target} (patch update available)\n");
        if let Some(t) = &s.newer_branch_target {
            out.push_str(&format!(
                "{also}: {t} ({latest_word}: {}) - new minor version\n",
                s.newer_latest()
            ));
        }
        out
    } else {
        format!(
            "• {name}: {current} (latest patch: {latest}) → {target} (new minor version available)\n"
        )
    }
}

fn deployment_webhook(ctx: &Ctx, d: &Deployment, success: bool) -> Value {
    let mut m = json!({
        "success": success,
        "message": if success { "New version successfully deployed" } else { "Deployment failed" },
        "event": if success { "deployment_success" } else { "deployment_failed" },
        "application_name": d.application_name,
        "application_uuid": d.application_uuid,
        "deployment_uuid": d.deployment_uuid,
        "deployment_url": d.url(ctx),
        "project": d.project_name,
        "environment": d.environment_name,
    });
    if let Some(p) = &d.preview {
        m["pull_request_id"] = json!(p.pull_request_id);
        m["preview_fqdn"] = json!(p.fqdn);
    }
    let f = first_fqdn(d.fqdn.as_deref());
    if truthy(f.as_deref()) {
        m["fqdn"] = json!(f);
    }
    m
}

/// `new Test(channel: ...)`.
pub fn test_notification(channel: &str, emails: Option<String>) -> Notification {
    Notification::Test {
        emails,
        channel: Some(channel.to_owned()),
        ping: false,
    }
}

/// `TracksDeliveryFailures::notifyOtherChannels` and `send_internal_notification`.
pub fn general(message: impl Into<String>, success: bool) -> Notification {
    Notification::General {
        message: message.into(),
        success,
    }
}

/// Transactional mail through the instance settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum Mail {
    /// `TransactionalEmails\ResetPassword`.
    ResetPassword { token: String, email: String },
    /// `Livewire\Team\InviteLink::viaEmail`.
    TeamInvitation {
        team: String,
        invitation_link: String,
    },
    /// `TransactionalEmails\InvitationLink`.
    InvitationLink {
        team: String,
        invitation_link: String,
    },
    /// `TransactionalEmails\EmailChangeVerification`. The mail goes to `new_email`.
    EmailChangeVerification {
        new_email: String,
        verification_code: String,
    },
    /// `User::sendVerificationEmail`.
    EmailVerification { url: String },
    /// `TransactionalEmails\Test`.
    Test,
    /// `Livewire\Help::submit`: `emails.help`.
    Help {
        subject: String,
        description: String,
    },
    /// `TracksDeliveryFailures::sendChannelDisabledEmail`.
    ChannelDisabled {
        channel: String,
        failures: i64,
        reason: String,
        url: String,
    },
    /// `S3Storage::testConnection`.
    S3ConnectionError {
        name: String,
        reason: String,
        url: String,
    },
}

/// `config('auth.passwords.users.expire')`.
const PASSWORD_EXPIRE_MINUTES: i64 = 10;
/// `config('constants.email_change.verification_code_expiry_minutes')`.
const EMAIL_CHANGE_EXPIRY_MINUTES: i64 = 10;

/// A route path parameter: `rawurlencode` and `RouteUrlGenerator::$dontEncode`.
fn route_path_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/@:;,=+!*|?&#%".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A route query value: `http_build_query`.
fn query_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
            out.push(b as char);
        } else if b == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl Mail {
    /// `toMail()` and `render()`.
    pub fn render(&self, ctx: &Ctx) -> MailMessage {
        let app = ctx.app_name.as_str();
        let (subject, html) = match self {
            Mail::ResetPassword { token, email } => {
                let url = format!(
                    "{}/reset-password/{}?email={}",
                    ctx.base_url.trim_end_matches('/'),
                    route_path_encode(token),
                    query_encode(email)
                );
                (
                    "Coolify: Reset Password".to_owned(),
                    views::reset_password(app, &url, PASSWORD_EXPIRE_MINUTES),
                )
            }
            Mail::EmailVerification { url } => (
                "Coolify: Verify your email.".to_owned(),
                views::email_verification(app, url),
            ),
            Mail::TeamInvitation {
                team,
                invitation_link,
            } => (
                format!("You have been invited to {team} on {app}."),
                views::invitation_link(app, team, invitation_link),
            ),
            Mail::InvitationLink {
                team,
                invitation_link,
            } => (
                format!("Coolify: Invitation for {team}"),
                views::invitation_link(app, team, invitation_link),
            ),
            Mail::EmailChangeVerification {
                new_email,
                verification_code,
            } => (
                "Coolify: Verify Your New Email Address".to_owned(),
                views::email_change_verification(
                    app,
                    new_email,
                    verification_code,
                    EMAIL_CHANGE_EXPIRY_MINUTES,
                ),
            ),
            Mail::Test => ("Coolify: Test Email".to_owned(), views::test_email(app)),
            Mail::Help {
                subject,
                description,
            } => (
                format!("[HELP]: {subject}"),
                format!(
                    "{}\n\n<hr />\n\n",
                    description
                        .replace('&', "&amp;")
                        .replace('<', "&lt;")
                        .replace('>', "&gt;")
                        .replace('"', "&quot;")
                        .replace('\'', "&#039;")
                ),
            ),
            Mail::ChannelDisabled {
                channel,
                failures,
                reason,
                url,
            } => (
                format!("Coolify: {channel} notifications were disabled"),
                views::notification_channel_disabled(app, channel, *failures, reason, url),
            ),
            Mail::S3ConnectionError { name, reason, url } => (
                "Coolify: S3 Storage Connection Error".to_owned(),
                views::s3_connection_error(app, name, reason, url),
            ),
        };
        MailMessage { subject, html }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx {
        Ctx {
            base_url: "https://c.example".into(),
            app_url: "https://app.example".into(),
            app_name: "Coolify".into(),
            version: "4.5.3".into(),
            cloud: false,
            dev: false,
        }
    }

    fn server() -> ServerRef {
        ServerRef {
            id: 1,
            name: "web".into(),
            uuid: "su".into(),
        }
    }

    #[test]
    fn discord_payload_order_and_ping() {
        let n = Notification::Unreachable { server: server() };
        let p = n.to_discord(&ctx()).to_payload(&ctx(), 5);
        assert_eq!(
            coolify_compat::php_json::encode(&p),
            r#"{"embeds":[{"title":":cross_mark: Server unreachable","description":"Your server 'web' is unreachable.","color":16740447,"fields":[{"name":"IMPORTANT","value":"We automatically try to revive your server and turn on all automations & integrations.","inline":false},{"name":"Time","value":"<t:5:R>","inline":true}],"footer":{"text":"Coolify v4.5.3"}}]}"#
        );
        let n = Notification::BackupFailed {
            backup: DatabaseBackup {
                name: "db".into(),
                uuid: "u".into(),
                project_uuid: None,
                environment_uuid: None,
                database_name: None,
                frequency: "0 0 * * *".into(),
            },
            output: "x".into(),
        };
        assert_eq!(
            n.to_discord(&ctx()).to_payload(&ctx(), 0)["content"],
            json!("@here")
        );
    }

    #[test]
    fn high_disk_pushover_buttons_have_no_url() {
        let n = Notification::HighDiskUsage {
            server: server(),
            disk_usage: 90,
            threshold: 80,
            interval_hours: 24,
        };
        let p = n.to_pushover(&ctx()).to_payload(&ctx(), "T", "U");
        assert!(
            p["message"]
                .as_str()
                .unwrap()
                .ends_with("&nbsp;<a href=''>Click here</a>&nbsp;<a href=''>Click here</a>")
        );
        assert_eq!(p["title"], json!("⚠️ High disk usage detected"));
        assert_eq!(p["html"], json!(1));
    }

    #[test]
    fn container_restarted_telegram_button_is_nested() {
        let n = Notification::ContainerRestarted {
            name: "coolify-proxy".into(),
            server: server(),
            url: Some("https://x".into()),
            throttle: None,
        };
        let m = n.to_telegram(&ctx());
        let (text, url) = button_parts(&m.buttons[0], "");
        assert_eq!(text, "Click here");
        assert_eq!(url, None);
    }

    #[test]
    fn commit_details() {
        let d = CommitDetails::resolve(
            Some(" abcdef1234 "),
            Some("  fix: a_b\r\nsecond\n\nthird  "),
            Some("feat/x"),
            false,
            |c| Some(format!("https://g/{c}")),
        );
        let m = d.for_mail().unwrap();
        assert_eq!(m.short_sha.as_deref(), Some("abcdef1"));
        assert_eq!(m.branch.as_deref(), Some("feat/x"));
        assert_eq!(m.message.as_deref(), Some("fix: a\\_b\\\nsecond\n\nthird"));
        let none = CommitDetails::resolve(Some("HEAD"), None, Some("m"), true, |_| None);
        assert!(none.for_mail().is_none());
    }

    #[test]
    fn git_commit_links() {
        assert_eq!(
            git_commit_link(Some("https://github.com"), Some("a/b"), Some("main"), "abc")
                .as_deref(),
            Some("https://github.com/a/b/commit/abc")
        );
        assert_eq!(
            git_commit_link(
                Some("https://bitbucket.org"),
                Some("a/b"),
                Some("main"),
                "abc"
            )
            .as_deref(),
            Some("https://bitbucket.org/a/b/commits/abc")
        );
        assert_eq!(
            git_commit_link(None, Some("https://u:p@gitlab.com/a/b.git"), None, "abc").as_deref(),
            Some("https://gitlab.com/a/b/commit/abc")
        );
        assert_eq!(git_commit_link(None, Some("not a url"), None, "abc"), None);
    }

    #[test]
    fn reset_password_url() {
        let m = Mail::ResetPassword {
            token: "T".into(),
            email: "a+b@c.d".into(),
        }
        .render(&ctx());
        assert!(
            m.html
                .contains("https://c.example/reset-password/T?email=a%2Bb%40c.d")
        );
    }
}
