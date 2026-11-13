//! The notification classes of `app/Notifications`: what each one sends on each channel.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::Ctx;
use super::php::{php_string, php_trim, truthy, ucfirst, value_truthy};
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
    /// `$blockedTitle`: the gate headline ("Deploy blocked: this image has 2 security problems
    /// that attackers are using") when the image scan gate stopped the deployment, for email.
    #[serde(default)]
    pub blocked_title: Option<String>,
    /// `$blockedChatTitle`: the same headline without counts, for chat and webhook channels.
    #[serde(default)]
    pub blocked_chat_title: Option<String>,
}

/// `NotificationText::plain`: control and format characters become spaces.
fn text_plain(value: &str) -> String {
    let control = regex::Regex::new(r"[\p{Cc}\p{Cf}]+").unwrap_or_else(|_| unreachable!());
    php_trim(&control.replace_all(value, " ")).to_owned()
}

/// `NotificationText::slack`: only `&`, `<` and `>` have a meaning that escaping can stop.
fn text_slack(value: &str) -> String {
    text_plain(value)
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `NotificationText::html`: `htmlspecialchars($value, ENT_QUOTES | ENT_HTML5, 'UTF-8')`.
fn text_html(value: &str) -> String {
    text_plain(value)
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// `DeploymentFailed::blockedTitle($headline)`.
fn blocked_title(headline: &str) -> String {
    let cut: String = headline.chars().take(300).collect();
    let plain = text_plain(&cut);
    let headline = plain.trim_end_matches('.');
    if headline.is_empty() {
        "Deploy blocked by the image scan".to_owned()
    } else if headline.starts_with("Deploy blocked") {
        headline.to_owned()
    } else {
        format!("Deploy blocked: {headline}")
    }
}

/// The constructor of `DeploymentFailed` over `scan_gate_result` of the deployment: the titles
/// for email and for chat when the image scan gate blocked the deployment, else `(None, None)`.
pub fn blocked_titles(gate: Option<&Value>) -> (Option<String>, Option<String>) {
    let Some(gate) = gate.filter(|g| g.is_object() || g.is_array()) else {
        return (None, None);
    };
    if !value_truthy(gate.get("blocked")) {
        return (None, None);
    }
    let headline = gate
        .get("headline")
        .filter(|v| !v.is_null())
        .map(php_string)
        .unwrap_or_default();
    let title = blocked_title(&headline);
    // A result stored before the plain headline existed may count problems: chat gets the generic
    // title then.
    let chat = match gate.get("headline_plain").filter(|v| !v.is_null()) {
        Some(plain) => blocked_title(&php_string(plain)),
        None if headline.chars().any(|c| c.is_ascii_digit()) => blocked_title(""),
        None => title.clone(),
    };
    (Some(title), Some(chat))
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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
    /// The `ScheduledDatabaseBackup` (`throttleSubject()`): a failure is sent at most once a day
    /// until a run succeeds again. 0 when unknown: never throttled.
    #[serde(default)]
    pub backup_id: i64,
    /// `ServiceDatabase::link()` is the page of its service: the UUID of the service.
    #[serde(default)]
    pub service_uuid: Option<String>,
    /// `StandalonePostgresql::isInstanceDatabase()`: the database of Coolify itself (id 0).
    #[serde(default)]
    pub is_instance_database: bool,
}

impl DatabaseBackup {
    /// `$this->database->link() ?? base_url()` (v4.7.5): the backup settings for the instance
    /// database, the page of the service for a service database, else the page of the database;
    /// the instance when the database has no project.
    pub fn url(&self, ctx: &Ctx) -> String {
        if self.is_instance_database {
            return format!("{}/settings/backup", ctx.base_url);
        }
        let (segment, uuid) = match &self.service_uuid {
            Some(service) => ("service", service.as_str()),
            None => ("database", self.uuid.as_str()),
        };
        match (&self.project_uuid, &self.environment_uuid) {
            (Some(project), Some(environment)) => format!(
                "{}/project/{project}/environment/{environment}/{segment}/{uuid}",
                ctx.base_url
            ),
            _ => ctx.base_url.clone(),
        }
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
    /// `$task->id`: the throttle subject of `TaskFailed`. `0` when unknown (never throttled).
    #[serde(default)]
    pub id: i64,
    /// `TaskFailed::$disabled`: this failure disabled the task. Always sent.
    #[serde(default)]
    pub disabled: bool,
    /// `$task->disabled_reason`.
    #[serde(default)]
    pub disabled_reason: Option<String>,
    /// `$task->consecutive_failures`.
    #[serde(default)]
    pub consecutive_failures: i64,
    /// `$task->failing_since->toDateTimeString()` (UTC).
    #[serde(default)]
    pub failing_since: Option<String>,
}

/// `ScheduledTask::AUTO_DISABLE_AFTER_FAILURES`: a task is disabled automatically only when it
/// failed this many times in a row...
pub const TASK_AUTO_DISABLE_AFTER_FAILURES: i64 = 10;
/// `ScheduledTask::AUTO_DISABLE_AFTER_HOURS`: ...and its first failure is this old, so a short
/// outage does not disable a task that runs often.
pub const TASK_AUTO_DISABLE_AFTER_HOURS: i64 = 24;
/// `NotificationThrottle::FAILURE_REMINDER_INTERVAL_MINUTES`: while a scheduled job keeps failing,
/// its failure notification is sent again at most this often.
pub const FAILURE_REMINDER_INTERVAL_MINUTES: i64 = 24 * 60;
/// `NotificationThrottle::FAILURE_REMINDER_TEXT`.
pub const FAILURE_REMINDER_TEXT: &str =
    "While it keeps failing, you get at most one notification each day.";

/// `failureEvent` of `ScheduledJobRecovered`: the notification setting of the failure.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailureEvent {
    BackupFailure,
    DockerCleanupFailure,
    ScheduledTaskFailure,
}

impl FailureEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            FailureEvent::BackupFailure => "backup_failure",
            FailureEvent::DockerCleanupFailure => "docker_cleanup_failure",
            FailureEvent::ScheduledTaskFailure => "scheduled_task_failure",
        }
    }
}

impl Task {
    /// `ScheduledTask::url()`: the page of the task; `None` without an application or service.
    pub fn url(&self, ctx: &Ctx) -> Option<String> {
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

    /// `TaskFailed::title()`.
    fn failed_title(&self) -> String {
        if self.disabled {
            format!(
                "Scheduled task ({}) was disabled because it keeps failing.",
                self.name
            )
        } else {
            format!("Scheduled task ({}) failed.", self.name)
        }
    }

    /// `TaskFailed::streakSummary()`: tells how long the task fails, and what happens next.
    fn streak_summary(&self) -> String {
        if self.disabled {
            return format!(
                "{} Fix the task, then enable it again.",
                self.disabled_reason
                    .as_deref()
                    .unwrap_or("Disabled automatically.")
            );
        }
        let summary = match self.failing_since.as_deref().filter(|s| truthy(Some(s))) {
            Some(since) if self.consecutive_failures > 1 => format!(
                "It failed {} times in a row since {since} UTC. ",
                self.consecutive_failures
            ),
            _ => String::new(),
        };
        format!(
            "{summary}While it keeps failing, you get at most one notification each day. After {TASK_AUTO_DISABLE_AFTER_FAILURES} failures in a row over more than {TASK_AUTO_DISABLE_AFTER_HOURS} hours, Coolify disables the task."
        )
    }

    /// The short title of a failed task on the chat channels.
    fn failed_label(&self) -> &'static str {
        if self.disabled {
            "Scheduled task disabled"
        } else {
            "Scheduled task failed"
        }
    }

    fn webhook(&self, ctx: &Ctx, success: bool) -> Value {
        let mut d = Map::new();
        d.insert("success".into(), json!(success));
        d.insert(
            "message".into(),
            json!(if success {
                "Scheduled task succeeded"
            } else {
                self.failed_label()
            }),
        );
        d.insert(
            "event".into(),
            json!(if success {
                "task_success"
            } else if self.disabled {
                "task_disabled"
            } else {
                "task_failed"
            }),
        );
        d.insert("task_name".into(), json!(self.name));
        d.insert("task_uuid".into(), json!(self.uuid));
        d.insert("output".into(), json!(self.output));
        if !success {
            d.insert(
                "consecutive_failures".into(),
                json!(self.consecutive_failures),
            );
        }
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
    /// The `ScheduledVolumeBackup` (`throttleSubject()`). 0 when unknown: never throttled.
    #[serde(default)]
    pub backup_id: i64,
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
    /// A resource (application, preview, service part, or database) stopped without a stop from
    /// Coolify.
    StatusChanged {
        /// `$resource_name`: the name, or `<application> PR #<id>` for a preview.
        name: String,
        /// `$resource->uuid`.
        uuid: String,
        project_name: Option<String>,
        project_uuid: Option<String>,
        environment_name: Option<String>,
        environment_uuid: Option<String>,
        fqdn: Option<String>,
        /// The resource type of the link.
        #[serde(default = "default_status_kind")]
        kind: RestartKind,
        /// The uuid in the link, when it is not the resource's own: the application of a
        /// preview, the service of a part.
        #[serde(default)]
        url_uuid: Option<String>,
        /// `throttleSubject()`: morph class and id. A resource that stops again soon after it
        /// came back sends no second notification.
        #[serde(default)]
        throttle: Option<(String, i64)>,
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
    /// `Server\HostKeyChanged`: sent with the channels of the "server unreachable" event.
    HostKeyChanged {
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
    /// `ApiTokenFailingNotification`: a token whose requests keep failing is throttled.
    ApiTokenFailing {
        token_name: String,
        failed_requests: i64,
        /// The method and path of the last failed request.
        last_request: String,
    },
    /// `ScheduledJobRecovered`: sent once on the first success after the team was notified that
    /// a scheduled job failed. It goes to the channels of the failure notification.
    ScheduledJobRecovered {
        failure_event: FailureEvent,
        title: String,
        /// The `event` value of the webhook payload.
        webhook_event: String,
        #[serde(default)]
        url: Option<String>,
        /// Extra fields of the webhook payload (an object).
        #[serde(default)]
        webhook_data: Value,
    },
    SslExpiration {
        resources: Vec<SslResource>,
    },
    /// `ImageScan\ExploitedProblemFound`: new relevant findings with an exploit signal on a
    /// resource (event `image_vulnerabilities`).
    ImageExploitedProblem(crate::image_scan::notify::ExploitedProblemFound),
    /// `ImageScan\VulnerabilityDigest`: the daily security digest of a team (event
    /// `image_vulnerabilities`).
    ImageVulnerabilityDigest(crate::image_scan::notify::VulnerabilityDigest),
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

fn default_status_kind() -> RestartKind {
    RestartKind::Application
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
            Unreachable { .. } | HostKeyChanged { .. } => "server_unreachable",
            ServerPatchCheck { .. } => "server_patch",
            TraefikVersionOutdated { .. } => "traefik_outdated",
            ApiTokenExpiring { .. } => "api_token_expiring",
            ApiTokenFailing { .. } => "api_token_failing",
            ScheduledJobRecovered { failure_event, .. } => failure_event.as_str(),
            SslExpiration { .. } => "ssl_certificate_renewal",
            ImageExploitedProblem(_) | ImageVulnerabilityDigest(_) => {
                crate::image_scan::notify::EVENT
            }
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
            ImageExploitedProblem(_) | ImageVulnerabilityDigest(_) => {
                crate::image_scan::notify::TELEGRAM_THREAD_COLUMN
            }
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
            // A resource that stops again soon after it came back sends no second notification.
            Notification::StatusChanged {
                throttle: Some((t, id)),
                ..
            } => Some(Throttle {
                notifiable_type: t.clone(),
                notifiable_id: *id,
                notification: STATUS_CHANGED_CLASS,
                minutes: 15,
            }),
            // Sent on the first failure of a streak, then at most once a day until the task
            // succeeds again. With `disabled` it is always sent.
            Notification::TaskFailed(t) if !t.disabled && t.id != 0 => Some(Throttle {
                notifiable_type: SCHEDULED_TASK_MODEL.into(),
                notifiable_id: t.id,
                notification: TASK_FAILED_CLASS,
                minutes: FAILURE_REMINDER_INTERVAL_MINUTES,
            }),
            // Sent on the first failure, then at most once a day until a cleanup on the server
            // succeeds again.
            Notification::DockerCleanupFailed { server, .. } => Some(Throttle {
                notifiable_type: "App\\Models\\Server".into(),
                notifiable_id: server.id,
                notification: DOCKER_CLEANUP_FAILED_CLASS,
                minutes: FAILURE_REMINDER_INTERVAL_MINUTES,
            }),
            // Backups: the first failure (or warning) of a streak, then at most once a day until a
            // run succeeds again (`notifyIfRecovered()` releases them).
            Notification::BackupFailed { backup, .. } if backup.backup_id != 0 => Some(Throttle {
                notifiable_type: SCHEDULED_DATABASE_BACKUP_MODEL.into(),
                notifiable_id: backup.backup_id,
                notification: DATABASE_BACKUP_FAILED_CLASS,
                minutes: FAILURE_REMINDER_INTERVAL_MINUTES,
            }),
            Notification::BackupSuccessWithS3Warning { backup, .. } if backup.backup_id != 0 => {
                Some(Throttle {
                    notifiable_type: SCHEDULED_DATABASE_BACKUP_MODEL.into(),
                    notifiable_id: backup.backup_id,
                    notification: DATABASE_BACKUP_S3_WARNING_CLASS,
                    minutes: FAILURE_REMINDER_INTERVAL_MINUTES,
                })
            }
            Notification::VolumeBackupFailed { backup, .. } if backup.backup_id != 0 => {
                Some(Throttle {
                    notifiable_type: SCHEDULED_VOLUME_BACKUP_MODEL.into(),
                    notifiable_id: backup.backup_id,
                    notification: VOLUME_BACKUP_FAILED_CLASS,
                    minutes: FAILURE_REMINDER_INTERVAL_MINUTES,
                })
            }
            // Only with a warning: `throttleSubject()` is null without one.
            Notification::VolumeBackupSuccess { backup, warning }
                if backup.backup_id != 0 && truthy(warning.as_deref()) =>
            {
                Some(Throttle {
                    notifiable_type: SCHEDULED_VOLUME_BACKUP_MODEL.into(),
                    notifiable_id: backup.backup_id,
                    notification: VOLUME_BACKUP_SUCCESS_CLASS,
                    minutes: FAILURE_REMINDER_INTERVAL_MINUTES,
                })
            }
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
                let (subject, fqdn) = if let Some(blocked) = &d.blocked_title {
                    (
                        format!("Coolify: {blocked} ({}).", d.application_name),
                        first,
                    )
                } else if pr == 0 {
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
                        d.blocked_title.as_deref(),
                    ),
                )
            }
            StatusChanged { name, .. } => mail(
                format!("Coolify: {name} has stopped"),
                views::application_status_changes(app, name, &self.status_url(ctx)),
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
                    Some(FAILURE_REMINDER_TEXT),
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
                    "Coolify: Backup succeeded locally but the storage upload failed for {}",
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
                format!("Coolify: [ACTION REQUIRED] {}", t.failed_title()),
                views::scheduled_task_failed(
                    app,
                    &t.failed_title(),
                    &t.streak_summary(),
                    &t.output,
                    t.url(ctx).as_deref(),
                ),
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
                views::docker_cleanup_failed(app, &server.name, message, FAILURE_REMINDER_TEXT),
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
            HostKeyChanged { server } => mail(
                format!(
                    "Coolify: The SSH host key of your server ({}) changed.",
                    server.name
                ),
                views::server_host_key_changed(
                    app,
                    &host_key_changed_text(server),
                    &host_key_changed_url(ctx, server),
                ),
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
                    Some(FAILURE_REMINDER_TEXT),
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
                format!(
                    "Coolify: API token '{token_name}' expires on {}",
                    expires_at.as_deref().unwrap_or_default()
                ),
                views::api_token_expiring(
                    app,
                    token_name,
                    expires_at.as_deref().unwrap_or_default(),
                    &format!("{}/security/api-tokens", ctx.base_url),
                ),
            ),
            ApiTokenFailing {
                token_name,
                failed_requests,
                last_request,
            } => mail(
                format!("Coolify: API token '{token_name}' sends many failed requests"),
                views::api_token_failing(
                    app,
                    &api_token_failing_summary(token_name, *failed_requests),
                    API_TOKEN_FAILING_ACTION,
                    last_request,
                    &format!("{}/security/api-tokens", ctx.base_url),
                ),
            ),
            ScheduledJobRecovered { title, url, .. } => mail(
                format!("Coolify: {title}"),
                views::scheduled_job_recovered(app, title, url.as_deref()),
            ),
            SslExpiration { resources } => mail(
                "Coolify: [Action Required] SSL Certificates Renewed - Manual Redeployment Needed",
                views::ssl_certificate_renewed(app, resources, &ssl_urls(ctx, resources)),
            ),
            ImageExploitedProblem(n) => mail(
                n.mail_subject(),
                views::image_vulnerabilities(app, &n.mail_slot()),
            ),
            ImageVulnerabilityDigest(n) => mail(
                n.mail_subject(),
                views::image_vulnerabilities(app, &n.mail_slot()),
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
                let title = d
                    .blocked_chat_title
                    .as_deref()
                    .unwrap_or(":cross_mark: Deployment failed");
                let mut m = D::new(title, description, D::ERROR).critical();
                m.field("Project", &d.project_name, true);
                m.field("Environment", &d.environment_name, true);
                m.field("Name", &d.application_name, true);
                m.field("Deployment Logs", format!("[Link]({url})"), false);
                if d.preview.is_some() && truthy(f.as_deref()) {
                    m.field("Domain", f.unwrap_or_default(), true);
                }
                m
            }
            StatusChanged { name, .. } => D::new(
                ":cross_mark: Resource stopped",
                format!(
                    "{name} has stopped.\n\n[Open Resource in Coolify]({})",
                    self.status_url(ctx)
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
                    ":warning: Database backup succeeded locally, storage upload failed",
                    format!(
                        "Database backup for {} (db:{}) was created successfully on local storage, but failed to upload to the backup storage.",
                        b.name,
                        b.db()
                    ),
                    D::WARNING,
                );
                m.field("Frequency", &b.frequency, true);
                m.field("Storage Error", s3_error, false);
                if let Some(u) = s3_url(ctx, s3_uuid.as_deref()) {
                    m.field("Storage", format!("[Check Configuration]({u})"), false);
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
                    format!(":cross_mark: {}", t.failed_label()),
                    format!("{}\n\n{}", t.failed_title(), t.streak_summary()),
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
            HostKeyChanged { server } => {
                let mut m = D::new(
                    ":warning: SSH host key changed",
                    host_key_changed_text(server),
                    D::ERROR,
                );
                m.field(
                    "Check the new key",
                    format!("[Link]({})", host_key_changed_url(ctx, server)),
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
            ApiTokenFailing {
                token_name,
                failed_requests,
                last_request,
            } => {
                let mut m = D::new(
                    "🔑 API token sends many failed requests",
                    format!(
                        "{}\n\n**Action Required:** {API_TOKEN_FAILING_ACTION}",
                        api_token_failing_summary(token_name, *failed_requests)
                    ),
                    D::WARNING,
                );
                m.field("Last failed request", last_request, false);
                m.field(
                    "Manage tokens",
                    format!("[Open API Tokens]({}/security/api-tokens)", ctx.base_url),
                    false,
                );
                m
            }
            ScheduledJobRecovered { title, url, .. } => {
                let mut m = D::new(
                    format!(":white_check_mark: {title}"),
                    RECOVERED_TEXT,
                    D::SUCCESS,
                );
                if let Some(u) = url.as_deref().filter(|u| truthy(Some(u))) {
                    m.field("Link", format!("[Open in Coolify]({u})"), false);
                }
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
            ImageExploitedProblem(n) => n.to_discord(),
            ImageVulnerabilityDigest(n) => n.to_discord(),
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
                let message = if let Some(blocked) = &d.blocked_chat_title {
                    format!("Coolify: {blocked} ({}): ", d.application_name)
                } else if let Some(p) = &d.preview {
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
            StatusChanged { name, .. } => TelegramMessage {
                message: format!("Coolify: {name} has stopped."),
                buttons: vec![button("Open Resource in Coolify", &self.status_url(ctx))],
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
                    "Coolify: Database backup for {} (db:{}) with frequency of {} succeeded locally but failed to upload to the backup storage.\n\nStorage Error:\n{s3_error}",
                    b.name,
                    b.db(),
                    b.frequency
                );
                if let Some(u) = s3_url(ctx, s3_uuid.as_deref()) {
                    message.push_str(&format!("\n\nCheck Storage Configuration: {u}"));
                }
                telegram(message)
            }
            General { message, .. } => telegram(message),
            TaskSuccess(t) => telegram(format!("Coolify: Scheduled task ({}) succeeded.", t.name)),
            TaskFailed(t) => telegram(format!(
                "Coolify: {}\n\n{}\n\nOutput: {}",
                t.failed_title(),
                t.streak_summary(),
                t.output
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
            HostKeyChanged { server } => TelegramMessage {
                message: format!("Coolify: {}", host_key_changed_text(server)),
                buttons: vec![button(
                    "Check the new key",
                    &host_key_changed_url(ctx, server),
                )],
            },
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
            ApiTokenFailing {
                token_name,
                failed_requests,
                last_request,
            } => TelegramMessage {
                message: format!(
                    "Coolify: {}\n\nLast failed request: {last_request}\n\nAction Required: {API_TOKEN_FAILING_ACTION}",
                    api_token_failing_summary(token_name, *failed_requests)
                ),
                buttons: vec![button(
                    "Manage API tokens",
                    &format!("{}/security/api-tokens", ctx.base_url),
                )],
            },
            ScheduledJobRecovered { title, .. } => {
                telegram(format!("Coolify: {title}\n\n{RECOVERED_TEXT}"))
            }
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
            ImageExploitedProblem(n) => n.to_telegram(),
            ImageVulnerabilityDigest(n) => n.to_telegram(),
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
                let (title, message) = if let Some(blocked) = &d.blocked_chat_title {
                    (
                        blocked.clone(),
                        format!(
                            "The image scan blocked the deployment of {}.",
                            text_html(&d.application_name)
                        ),
                    )
                } else if let Some(p) = &d.preview {
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
            StatusChanged { name, .. } => {
                P::new("Resource stopped", "error", format!("{name} has stopped.")).with_buttons(
                    vec![button("Open Resource in Coolify", &self.status_url(ctx))],
                )
            }
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
                    "Database backup for {} (db:{}) was created successfully on local storage, but failed to upload to the backup storage.<br/><br/><b>Frequency:</b> {}.<br/><b>Storage Error:</b> {s3_error}",
                    b.name,
                    b.db(),
                    b.frequency
                );
                if let Some(u) = s3_url(ctx, s3_uuid.as_deref()) {
                    message.push_str(&format!(
                        "<br/><br/><a href=\"{u}\">Check Storage Configuration</a>"
                    ));
                }
                P::new(
                    "Database backup succeeded locally, storage upload failed",
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
                let mut message = format!("{}<br/>{}<br/>", t.failed_title(), t.streak_summary());
                if truthy(Some(&t.output)) {
                    message.push_str(&format!("<br/><b>Error Output:</b>{}", t.output));
                }
                let buttons = t
                    .url(ctx)
                    .map(|u| vec![button("Open task in Coolify", &u)])
                    .unwrap_or_default();
                P::new(t.failed_label(), "error", message).with_buttons(buttons)
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
            HostKeyChanged { server } => P::new(
                "SSH host key changed",
                "error",
                host_key_changed_text(server),
            )
            .with_buttons(vec![button(
                "Check the new key",
                &host_key_changed_url(ctx, server),
            )]),
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
            ApiTokenFailing {
                token_name,
                failed_requests,
                last_request,
            } => P::new(
                "API token sends many failed requests",
                "warning",
                format!(
                    "{}<br/><br/>Last failed request: {}<br/><br/><b>Action Required:</b> {}",
                    super::php::e(&api_token_failing_summary(token_name, *failed_requests)),
                    super::php::e(last_request),
                    super::php::e(API_TOKEN_FAILING_ACTION)
                ),
            )
            .with_buttons(vec![button(
                "Manage API tokens",
                &format!("{}/security/api-tokens", ctx.base_url),
            )]),
            ScheduledJobRecovered { title, url, .. } => {
                let buttons = url
                    .as_deref()
                    .filter(|u| truthy(Some(u)))
                    .map(|u| vec![button("Open in Coolify", u)])
                    .unwrap_or_default();
                P::new(title, "success", RECOVERED_TEXT).with_buttons(buttons)
            }
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
            ImageExploitedProblem(n) => n.to_pushover(),
            ImageVulnerabilityDigest(n) => n.to_pushover(),
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
                let title = match &d.blocked_chat_title {
                    Some(blocked) => {
                        description = format!(
                            "The image scan blocked the deployment of {}.",
                            text_slack(&d.application_name)
                        );
                        blocked.clone()
                    }
                    None => title,
                };
                description.push_str(&format!(
                    "\n\n*Project:* {}\n*Environment:* {}\n*<{url}|Deployment Logs>*",
                    d.project_name, d.environment_name
                ));
                slack(title, description, SLACK_ERROR)
            }
            StatusChanged {
                name,
                project_name,
                environment_name,
                ..
            } => slack(
                "Resource stopped",
                format!(
                    "{name} has stopped\n\n*Project:* {}\n*Environment:* {}\n*Resource URL:* {}",
                    project_name.as_deref().unwrap_or_default(),
                    environment_name.as_deref().unwrap_or_default(),
                    self.status_url(ctx)
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
                    "Database backup for {} (db:{}) was created successfully on local storage, but failed to upload to the backup storage.\n\n*Frequency:* {}\n\n*Storage Error:* {s3_error}",
                    b.name,
                    b.db(),
                    b.frequency
                );
                if let Some(u) = s3_url(ctx, s3_uuid.as_deref()) {
                    description.push_str(&format!("\n\n*Storage:* <{u}|Check Configuration>"));
                }
                slack(
                    "Database backup succeeded locally, storage upload failed",
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
                let mut description = format!("{}\n\n{}", t.failed_title(), t.streak_summary());
                if truthy(Some(&t.output)) {
                    description.push_str(&format!("\n\n*Error Output:* {}", t.output));
                }
                if let Some(u) = t.url(ctx) {
                    description.push_str(&format!("\n\n*Task URL:* {u}"));
                }
                slack(t.failed_label(), description, SLACK_ERROR)
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
            HostKeyChanged { server } => slack(
                "SSH host key changed",
                format!(
                    "{}\n\nCheck the new key: {}",
                    host_key_changed_text(server),
                    host_key_changed_url(ctx, server)
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
            ApiTokenFailing {
                token_name,
                failed_requests,
                last_request,
            } => slack(
                "🔑 API token sends many failed requests",
                format!(
                    "{}\n\nLast failed request: {last_request}\n\n*Action Required:* {API_TOKEN_FAILING_ACTION}\n\nManage tokens: {}/security/api-tokens",
                    api_token_failing_summary(token_name, *failed_requests),
                    ctx.base_url
                ),
                SLACK_WARNING,
            ),
            ScheduledJobRecovered { title, url, .. } => {
                let mut description = RECOVERED_TEXT.to_owned();
                if let Some(u) = url.as_deref().filter(|u| truthy(Some(u))) {
                    description.push_str(&format!("\n\n*Link:* {u}"));
                }
                slack(title, description, SLACK_SUCCESS)
            }
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
            ImageExploitedProblem(n) => n.to_slack(),
            ImageVulnerabilityDigest(n) => n.to_slack(),
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
                environment_name,
                fqdn,
                ..
            } => json!({
                "success": false,
                "message": "Resource stopped",
                "event": "status_changed",
                "status": "exited",
                "application_name": name,
                "application_uuid": uuid,
                "resource_name": name,
                "resource_uuid": uuid,
                "url": self.status_url(ctx),
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
            HostKeyChanged { server } => json!({
                "success": false,
                "message": "SSH host key changed",
                "event": "server_ssh_host_key_changed",
                "server_name": server.name,
                "server_uuid": server.uuid,
                "url": host_key_changed_url(ctx, server),
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
            ApiTokenFailing {
                token_name,
                failed_requests,
                last_request,
            } => json!({
                "success": false,
                "message": api_token_failing_summary(token_name, *failed_requests),
                "event": "api_token_failing",
                "token_name": token_name,
                "failed_requests": failed_requests,
                "last_request": last_request,
                "url": format!("{}/security/api-tokens", ctx.base_url),
            }),
            ScheduledJobRecovered {
                title,
                webhook_event,
                url,
                webhook_data,
                ..
            } => {
                // `array_filter([...], fn ($value) => $value !== null)`: the extra fields can
                // replace the first three, and `url` is last.
                let mut d = Map::new();
                d.insert("success".into(), json!(true));
                d.insert("message".into(), json!(title));
                d.insert("event".into(), json!(webhook_event));
                if let Some(extra) = webhook_data.as_object() {
                    for (k, v) in extra {
                        d.insert(k.clone(), v.clone());
                    }
                }
                d.insert("url".into(), json!(url));
                d.retain(|_, v| !v.is_null());
                Value::Object(d)
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
            ImageExploitedProblem(n) => n.to_webhook(),
            ImageVulnerabilityDigest(n) => n.to_webhook(),
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

const SCHEDULED_TASK_MODEL: &str = "App\\Models\\ScheduledTask";
const TASK_FAILED_CLASS: &str = "App\\Notifications\\ScheduledTask\\TaskFailed";
const DOCKER_CLEANUP_FAILED_CLASS: &str = "App\\Notifications\\Server\\DockerCleanupFailed";
const STATUS_CHANGED_CLASS: &str = "App\\Notifications\\Application\\StatusChanged";

/// The throttle subject and class of `TaskFailed`, for `NotificationThrottle::release`.
pub const TASK_FAILED_THROTTLE: (&str, &str) = (SCHEDULED_TASK_MODEL, TASK_FAILED_CLASS);
/// The throttle subject and class of `DockerCleanupFailed`.
pub const DOCKER_CLEANUP_FAILED_THROTTLE: (&str, &str) =
    ("App\\Models\\Server", DOCKER_CLEANUP_FAILED_CLASS);

const SCHEDULED_DATABASE_BACKUP_MODEL: &str = "App\\Models\\ScheduledDatabaseBackup";
const SCHEDULED_VOLUME_BACKUP_MODEL: &str = "App\\Models\\ScheduledVolumeBackup";
const DATABASE_BACKUP_FAILED_CLASS: &str = "App\\Notifications\\Database\\BackupFailed";
const DATABASE_BACKUP_S3_WARNING_CLASS: &str =
    "App\\Notifications\\Database\\BackupSuccessWithS3Warning";
const VOLUME_BACKUP_FAILED_CLASS: &str = "App\\Notifications\\VolumeBackup\\BackupFailed";
const VOLUME_BACKUP_SUCCESS_CLASS: &str = "App\\Notifications\\VolumeBackup\\BackupSuccess";

/// The throttles `DatabaseBackupJob::notifyIfRecovered()` releases: the subject model, then the
/// classes of `BackupFailed` and `BackupSuccessWithS3Warning`.
pub const DATABASE_BACKUP_THROTTLES: (&str, [&str; 2]) = (
    SCHEDULED_DATABASE_BACKUP_MODEL,
    [
        DATABASE_BACKUP_FAILED_CLASS,
        DATABASE_BACKUP_S3_WARNING_CLASS,
    ],
);
/// The throttles `VolumeBackupJob::notifyIfRecovered()` releases: `BackupFailed` and `BackupSuccess`
/// (with a warning).
pub const VOLUME_BACKUP_THROTTLES: (&str, [&str; 2]) = (
    SCHEDULED_VOLUME_BACKUP_MODEL,
    [VOLUME_BACKUP_FAILED_CLASS, VOLUME_BACKUP_SUCCESS_CLASS],
);

/// The body of `ScheduledJobRecovered` on every channel.
const RECOVERED_TEXT: &str = "The last run succeeded after failed runs.";

/// `ApiTokenFailingNotification::action()`.
const API_TOKEN_FAILING_ACTION: &str = "Find the client that uses this token and fix its requests, give the token the permissions it needs, or revoke the token.";

/// `ApiTokenFailingNotification::summary()`.
fn api_token_failing_summary(token_name: &str, failed_requests: i64) -> String {
    format!(
        "API token '{token_name}' sent {failed_requests} failed requests in one minute. Coolify now answers its requests with 429 (Too Many Requests) until the failures stop."
    )
}

impl Notification {
    /// `StatusChanged::$resource_url`: the page of the resource that stopped.
    fn status_url(&self, ctx: &Ctx) -> String {
        let Notification::StatusChanged {
            uuid,
            project_uuid,
            environment_uuid,
            kind,
            url_uuid,
            ..
        } = self
        else {
            return String::new();
        };
        let segment = match kind {
            RestartKind::Application | RestartKind::Preview => "application",
            RestartKind::Service => "service",
            RestartKind::Database => "database",
        };
        format!(
            "{}/project/{}/environment/{}/{segment}/{}",
            ctx.base_url,
            project_uuid.as_deref().unwrap_or_default(),
            environment_uuid.as_deref().unwrap_or_default(),
            url_uuid.as_deref().unwrap_or(uuid)
        )
    }
}

fn server_url(ctx: &Ctx, s: &ServerRef) -> String {
    format!("{}/server/{}", ctx.base_url, s.uuid)
}

/// `HostKeyChanged::text()`.
fn host_key_changed_text(s: &ServerRef) -> String {
    format!(
        "The SSH host key of your server '{}' does not match the key that Coolify trusts. This occurs when the server was reinstalled, or when another machine answers at its address. Compare the new key with the key on the server, then trust it or reset it.",
        s.name
    )
}

/// `HostKeyChanged::url()`.
fn host_key_changed_url(ctx: &Ctx, s: &ServerRef) -> String {
    format!(
        "{}/server/{}/security#server-host-key-section",
        ctx.base_url, s.uuid
    )
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
    // `DeploymentFailed`: the image scan gate stopped the deployment.
    if !success && let Some(blocked) = &d.blocked_chat_title {
        m["message"] = json!(blocked);
        m["blocked"] = json!(true);
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
                "Coolify: Backup Storage Connection Error".to_owned(),
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

    // `Notifications\Server\HostKeyChanged`.
    #[test]
    fn host_key_changed_messages() {
        let n = Notification::HostKeyChanged { server: server() };
        assert_eq!(n.event(), "server_unreachable");
        let text = "The SSH host key of your server 'web' does not match the key that Coolify trusts. This occurs when the server was reinstalled, or when another machine answers at its address. Compare the new key with the key on the server, then trust it or reset it.";
        let url = format!(
            "{}/server/su/security#server-host-key-section",
            ctx().base_url
        );
        let telegram = n.to_telegram(&ctx());
        assert_eq!(telegram.message, format!("Coolify: {text}"));
        assert_eq!(
            telegram.buttons,
            vec![json!({"text": "Check the new key", "url": url})]
        );
        let slack = n.to_slack(&ctx());
        assert_eq!(slack.title, "SSH host key changed");
        assert_eq!(
            slack.description,
            format!("{text}\n\nCheck the new key: {url}")
        );
        assert_eq!(
            coolify_compat::php_json::encode(&n.to_webhook(&ctx())),
            format!(
                r#"{{"success":false,"message":"SSH host key changed","event":"server_ssh_host_key_changed","server_name":"web","server_uuid":"su","url":"{}"}}"#,
                url.replace('/', "\\/")
            )
        );
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
                backup_id: 0,
                service_uuid: None,
                is_instance_database: false,
            },
            output: "x".into(),
        };
        assert_eq!(
            n.to_discord(&ctx()).to_payload(&ctx(), 0)["content"],
            json!("@here")
        );
    }

    fn database_backup(backup_id: i64) -> DatabaseBackup {
        DatabaseBackup {
            name: "db".into(),
            uuid: "db-uuid".into(),
            project_uuid: Some("p".into()),
            environment_uuid: Some("e".into()),
            database_name: Some("app".into()),
            frequency: "daily".into(),
            backup_id,
            service_uuid: None,
            is_instance_database: false,
        }
    }

    fn volume_backup(backup_id: i64) -> VolumeBackup {
        VolumeBackup {
            resource_name: Some("web".into()),
            target: "Volume data".into(),
            frequency: "daily".into(),
            url: None,
            backup_id,
        }
    }

    /// `link()`: the instance database links to the backup settings, a service database to its
    /// service, any other database to its own page.
    #[test]
    fn backup_links_follow_the_kind_of_database() {
        let mut b = database_backup(1);
        assert_eq!(
            b.url(&ctx()),
            format!(
                "{}/project/p/environment/e/database/db-uuid",
                ctx().base_url
            )
        );
        b.service_uuid = Some("svc".into());
        assert_eq!(
            b.url(&ctx()),
            format!("{}/project/p/environment/e/service/svc", ctx().base_url)
        );
        b.is_instance_database = true;
        assert_eq!(b.url(&ctx()), format!("{}/settings/backup", ctx().base_url));
        b.is_instance_database = false;
        b.project_uuid = None;
        assert_eq!(b.url(&ctx()), ctx().base_url);
    }

    /// `ThrottledNotification` of the backup notifications: failures, the S3 warning and a volume
    /// backup with a warning are sent once a day; a success is never throttled.
    #[test]
    fn backup_failures_are_throttled_per_schedule() {
        let failed = Notification::BackupFailed {
            backup: database_backup(9),
            output: "x".into(),
        };
        let t = failed.throttle().unwrap();
        assert_eq!(
            (t.notifiable_type.as_str(), t.notifiable_id, t.notification),
            (
                DATABASE_BACKUP_THROTTLES.0,
                9,
                DATABASE_BACKUP_THROTTLES.1[0]
            )
        );
        assert_eq!(t.minutes, FAILURE_REMINDER_INTERVAL_MINUTES);
        let warning = Notification::BackupSuccessWithS3Warning {
            backup: database_backup(9),
            s3_error: "e".into(),
            s3_uuid: None,
        };
        assert_eq!(
            warning.throttle().unwrap().notification,
            DATABASE_BACKUP_THROTTLES.1[1]
        );
        assert!(
            Notification::BackupSuccess(database_backup(9))
                .throttle()
                .is_none()
        );
        // Without a known schedule nothing is throttled.
        assert!(
            Notification::BackupFailed {
                backup: database_backup(0),
                output: "x".into(),
            }
            .throttle()
            .is_none()
        );

        let volume_failed = Notification::VolumeBackupFailed {
            backup: volume_backup(4),
            output: "x".into(),
        };
        let t = volume_failed.throttle().unwrap();
        assert_eq!(
            (t.notifiable_type.as_str(), t.notifiable_id, t.notification),
            (VOLUME_BACKUP_THROTTLES.0, 4, VOLUME_BACKUP_THROTTLES.1[0])
        );
        let with_warning = Notification::VolumeBackupSuccess {
            backup: volume_backup(4),
            warning: Some("slow".into()),
        };
        assert_eq!(
            with_warning.throttle().unwrap().notification,
            VOLUME_BACKUP_THROTTLES.1[1]
        );
        let clean = Notification::VolumeBackupSuccess {
            backup: volume_backup(4),
            warning: None,
        };
        assert!(clean.throttle().is_none());
    }

    /// The failed backup mails end with the reminder of the daily throttle.
    #[test]
    fn failed_backup_mails_carry_the_reminder() {
        let failed = views::backup_failed(
            "Coolify",
            "db",
            Some("app"),
            "daily",
            "boom",
            Some(FAILURE_REMINDER_TEXT),
        );
        // `layout()` renders the Markdown of the mail: the reminder is its own paragraph.
        assert!(failed.contains(&format!("<p>boom</p>\n<p>{FAILURE_REMINDER_TEXT}</p>\n")));
        let volume = views::volume_backup_result(
            "Coolify",
            "Storage backup failed.",
            "daily",
            Some("boom"),
            Some(FAILURE_REMINDER_TEXT),
            Some("https://x"),
        );
        assert!(volume.contains(&format!(
            "<p>boom</p>\n<p>{FAILURE_REMINDER_TEXT}</p>\n<p>Click <a href=\"https://x\">here</a> to open the resource.</p>"
        )));
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

    fn deployment() -> Deployment {
        Deployment {
            application_name: "shop <api>".into(),
            application_uuid: "au".into(),
            deployment_uuid: "du".into(),
            project_name: "P".into(),
            project_uuid: "pu".into(),
            environment_name: "production".into(),
            environment_uuid: "eu".into(),
            fqdn: Some("https://shop.example".into()),
            preview: None,
            commit: None,
            blocked_title: None,
            blocked_chat_title: None,
        }
    }

    #[test]
    fn the_scan_gate_gives_a_failed_deployment_its_titles() {
        assert_eq!(blocked_titles(None), (None, None));
        assert_eq!(
            blocked_titles(Some(&json!({"blocked": false}))),
            (None, None)
        );
        assert_eq!(blocked_titles(Some(&json!("blocked"))), (None, None));
        // The email title keeps the counts, the chat title is the plain headline.
        assert_eq!(
            blocked_titles(Some(&json!({
                "blocked": true,
                "headline": "this image has 2 security problems that attackers are using.",
                "headline_plain": "this image has security problems that attackers are using.",
            }))),
            (
                Some(
                    "Deploy blocked: this image has 2 security problems that attackers are using"
                        .to_owned()
                ),
                Some(
                    "Deploy blocked: this image has security problems that attackers are using"
                        .to_owned()
                ),
            )
        );
        // A result stored before the plain headline existed may count problems.
        assert_eq!(
            blocked_titles(Some(&json!({"blocked": true, "headline": "3 problems"}))),
            (
                Some("Deploy blocked: 3 problems".to_owned()),
                Some("Deploy blocked by the image scan".to_owned()),
            )
        );
        assert_eq!(
            blocked_titles(Some(
                &json!({"blocked": true, "headline": "Deploy blocked: policy"})
            )),
            (
                Some("Deploy blocked: policy".to_owned()),
                Some("Deploy blocked: policy".to_owned()),
            )
        );
        assert_eq!(
            blocked_titles(Some(&json!({"blocked": 1}))),
            (
                Some("Deploy blocked by the image scan".to_owned()),
                Some("Deploy blocked by the image scan".to_owned()),
            )
        );
        // Control characters never reach a title.
        assert_eq!(
            blocked_title("a\u{0}b\u{200b}c..."),
            "Deploy blocked: a b c"
        );
    }

    #[test]
    fn a_blocked_deployment_says_so_on_every_channel() {
        let mut d = deployment();
        d.blocked_title = Some("Deploy blocked: 2 problems".into());
        d.blocked_chat_title = Some("Deploy blocked: problems".into());
        let n = Notification::DeploymentFailed(d);
        let c = ctx();
        let mail = n.to_mail(&c, false).unwrap();
        assert_eq!(
            mail.subject,
            "Coolify: Deploy blocked: 2 problems (shop <api>)."
        );
        assert!(
            mail.html
                .contains("The image scan blocked the deployment of shop &lt;api&gt;."),
            "{}",
            mail.html
        );
        assert_eq!(n.to_discord(&c).title, "Deploy blocked: problems");
        assert_eq!(
            n.to_telegram(&c).message,
            "Coolify: Deploy blocked: problems (shop <api>): "
        );
        let pushover = n.to_pushover(&c);
        assert_eq!(pushover.title, "Deploy blocked: problems");
        assert_eq!(
            pushover.message,
            "The image scan blocked the deployment of shop &lt;api&gt;."
        );
        let slack = n.to_slack(&c);
        assert_eq!(slack.title, "Deploy blocked: problems");
        assert!(slack.description.starts_with(
            "The image scan blocked the deployment of shop &lt;api&gt;.\n\n*Project:* P"
        ));
        let webhook = n.to_webhook(&c);
        assert_eq!(webhook["message"], "Deploy blocked: problems");
        assert_eq!(webhook["blocked"], true);
        // A deployment that failed for another reason is unchanged.
        let plain = Notification::DeploymentFailed(deployment());
        assert_eq!(
            plain.to_mail(&c, false).unwrap().subject,
            "Coolify: Deployment failed of shop <api>."
        );
        assert_eq!(plain.to_discord(&c).title, ":cross_mark: Deployment failed");
        assert_eq!(plain.to_webhook(&c)["message"], "Deployment failed");
        assert!(plain.to_webhook(&c).get("blocked").is_none());
    }

    fn stopped(kind: RestartKind, url_uuid: Option<&str>) -> Notification {
        Notification::StatusChanged {
            name: "web".into(),
            uuid: "ru".into(),
            project_name: Some("P".into()),
            project_uuid: Some("pu".into()),
            environment_name: Some("production".into()),
            environment_uuid: Some("eu".into()),
            fqdn: Some("https://a.example,https://b.example".into()),
            kind,
            url_uuid: url_uuid.map(str::to_owned),
            throttle: Some(("App\\Models\\Application".into(), 7)),
        }
    }

    #[test]
    fn a_stopped_resource_links_to_its_own_page() {
        let c = ctx();
        let app = stopped(RestartKind::Application, None);
        let url = "https://c.example/project/pu/environment/eu/application/ru";
        assert_eq!(app.event(), "status_change");
        let mail = app.to_mail(&c, false).unwrap();
        assert_eq!(mail.subject, "Coolify: web has stopped");
        assert!(mail.html.contains("web has stopped."), "{}", mail.html);
        assert!(mail.html.contains(url), "{}", mail.html);
        let discord = app.to_discord(&c);
        assert_eq!(discord.title, ":cross_mark: Resource stopped");
        assert_eq!(
            discord.description,
            format!("web has stopped.\n\n[Open Resource in Coolify]({url})")
        );
        assert!(discord.is_critical);
        let telegram = app.to_telegram(&c);
        assert_eq!(telegram.message, "Coolify: web has stopped.");
        assert_eq!(
            telegram.buttons,
            vec![json!({"text": "Open Resource in Coolify", "url": url})]
        );
        let pushover = app.to_pushover(&c);
        assert_eq!(
            (pushover.title.as_str(), pushover.message.as_str()),
            ("Resource stopped", "web has stopped.")
        );
        assert_eq!(
            app.to_slack(&c).description,
            format!(
                "web has stopped\n\n*Project:* P\n*Environment:* production\n*Resource URL:* {url}"
            )
        );
        assert_eq!(
            app.to_webhook(&c),
            json!({
                "success": false,
                "message": "Resource stopped",
                "event": "status_changed",
                "status": "exited",
                "application_name": "web",
                "application_uuid": "ru",
                "resource_name": "web",
                "resource_uuid": "ru",
                "url": url,
                "project": "P",
                "environment": "production",
                "fqdn": "https://a.example",
            })
        );
        // Previews link to their application, service parts to their service.
        assert_eq!(
            stopped(RestartKind::Preview, Some("app")).to_webhook(&c)["url"],
            "https://c.example/project/pu/environment/eu/application/app"
        );
        assert_eq!(
            stopped(RestartKind::Service, Some("svc")).to_webhook(&c)["url"],
            "https://c.example/project/pu/environment/eu/service/svc"
        );
        assert_eq!(
            stopped(RestartKind::Database, None).to_webhook(&c)["url"],
            "https://c.example/project/pu/environment/eu/database/ru"
        );
    }

    #[test]
    fn a_resource_that_stops_again_soon_is_throttled() {
        let throttle = stopped(RestartKind::Application, None).throttle().unwrap();
        assert_eq!(throttle.notifiable_type, "App\\Models\\Application");
        assert_eq!(throttle.notifiable_id, 7);
        assert_eq!(
            throttle.notification,
            "App\\Notifications\\Application\\StatusChanged"
        );
        assert_eq!(throttle.minutes, 15);
        // The stored form of an older notification has no link type or throttle.
        let old: Notification = serde_json::from_value(json!({
            "type": "StatusChanged", "name": "my-app", "uuid": "u", "project_name": "P",
            "project_uuid": "pu", "environment_name": "prod", "environment_uuid": "eu",
            "fqdn": null,
        }))
        .unwrap();
        assert!(old.throttle().is_none());
        assert_eq!(
            old.to_webhook(&ctx())["url"],
            "https://c.example/project/pu/environment/eu/application/u"
        );
    }

    fn task() -> Task {
        Task {
            name: "cleanup".into(),
            uuid: "tu".into(),
            output: "boom".into(),
            resource: Some(TaskResource {
                is_application: true,
                uuid: "au".into(),
                project_uuid: Some("pu".into()),
                environment_uuid: Some("eu".into()),
            }),
            id: 5,
            disabled: false,
            disabled_reason: None,
            consecutive_failures: 1,
            failing_since: Some("2026-11-08 03:04:05".into()),
        }
    }

    const KEEPS_FAILING: &str = "While it keeps failing, you get at most one notification each day. After 10 failures in a row over more than 24 hours, Coolify disables the task.";

    #[test]
    fn a_failed_task_tells_its_streak() {
        let c = ctx();
        let first = Notification::TaskFailed(task());
        assert_eq!(
            first.to_mail(&c, false).unwrap().subject,
            "Coolify: [ACTION REQUIRED] Scheduled task (cleanup) failed."
        );
        assert_eq!(
            first.to_telegram(&c).message,
            format!("Coolify: Scheduled task (cleanup) failed.\n\n{KEEPS_FAILING}\n\nOutput: boom")
        );
        assert!(first.to_telegram(&c).buttons.is_empty());

        let mut again = task();
        again.consecutive_failures = 4;
        let again = Notification::TaskFailed(again);
        let summary =
            format!("It failed 4 times in a row since 2026-11-08 03:04:05 UTC. {KEEPS_FAILING}");
        let discord = again.to_discord(&c);
        assert_eq!(discord.title, ":cross_mark: Scheduled task failed");
        assert_eq!(
            discord.description,
            format!("Scheduled task (cleanup) failed.\n\n{summary}")
        );
        let pushover = again.to_pushover(&c);
        assert_eq!(pushover.title, "Scheduled task failed");
        assert_eq!(
            pushover.message,
            format!(
                "Scheduled task (cleanup) failed.<br/>{summary}<br/><br/><b>Error Output:</b>boom"
            )
        );
        assert_eq!(
            again.to_slack(&c).description,
            format!(
                "Scheduled task (cleanup) failed.\n\n{summary}\n\n*Error Output:* boom\n\n*Task URL:* https://c.example/project/pu/environment/eu/application/au/tasks/tu"
            )
        );
        let webhook = again.to_webhook(&c);
        assert_eq!(webhook["event"], "task_failed");
        assert_eq!(webhook["message"], "Scheduled task failed");
        assert_eq!(webhook["consecutive_failures"], 4);
        let keys: Vec<&String> = webhook.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "success",
                "message",
                "event",
                "task_name",
                "task_uuid",
                "output",
                "consecutive_failures",
                "application_uuid",
                "url"
            ]
        );
        // A successful run has no failure count.
        assert!(
            Notification::TaskSuccess(task())
                .to_webhook(&c)
                .get("consecutive_failures")
                .is_none()
        );
    }

    #[test]
    fn a_task_that_coolify_disabled_says_why() {
        let c = ctx();
        let mut t = task();
        t.disabled = true;
        t.consecutive_failures = 12;
        t.disabled_reason = Some(
            "Disabled automatically after 12 failed runs in a row since 2026-11-08 03:04:05 UTC."
                .into(),
        );
        let n = Notification::TaskFailed(t);
        let title = "Scheduled task (cleanup) was disabled because it keeps failing.";
        let summary = "Disabled automatically after 12 failed runs in a row since 2026-11-08 03:04:05 UTC. Fix the task, then enable it again.";
        let mail = n.to_mail(&c, false).unwrap();
        assert_eq!(mail.subject, format!("Coolify: [ACTION REQUIRED] {title}"));
        assert!(
            mail.html.contains("Fix the task, then enable it again."),
            "{}",
            mail.html
        );
        assert!(mail.html.contains("Last error:"), "{}", mail.html);
        assert_eq!(
            n.to_discord(&c).title,
            ":cross_mark: Scheduled task disabled"
        );
        assert_eq!(
            n.to_discord(&c).description,
            format!("{title}\n\n{summary}")
        );
        assert_eq!(n.to_pushover(&c).title, "Scheduled task disabled");
        assert_eq!(n.to_slack(&c).title, "Scheduled task disabled");
        let webhook = n.to_webhook(&c);
        assert_eq!(webhook["event"], "task_disabled");
        assert_eq!(webhook["message"], "Scheduled task disabled");
        // It is always sent.
        assert!(n.throttle().is_none());
    }

    #[test]
    fn repeated_failures_are_sent_at_most_once_a_day() {
        let throttle = Notification::TaskFailed(task()).throttle().unwrap();
        assert_eq!(throttle.notifiable_type, "App\\Models\\ScheduledTask");
        assert_eq!(throttle.notifiable_id, 5);
        assert_eq!(
            throttle.notification,
            "App\\Notifications\\ScheduledTask\\TaskFailed"
        );
        assert_eq!(throttle.minutes, 1440);
        assert_eq!(
            TASK_FAILED_THROTTLE,
            (
                "App\\Models\\ScheduledTask",
                "App\\Notifications\\ScheduledTask\\TaskFailed"
            )
        );
        // Success notifications are never throttled.
        assert!(Notification::TaskSuccess(task()).throttle().is_none());

        let cleanup = Notification::DockerCleanupFailed {
            server: server(),
            message: "no space".into(),
        };
        let throttle = cleanup.throttle().unwrap();
        assert_eq!(
            (throttle.notifiable_type.as_str(), throttle.notifiable_id),
            ("App\\Models\\Server", 1)
        );
        assert_eq!(
            throttle.notification,
            "App\\Notifications\\Server\\DockerCleanupFailed"
        );
        assert_eq!(throttle.minutes, 1440);
        let html = cleanup.to_mail(&ctx(), false).unwrap().html;
        assert!(html.contains(FAILURE_REMINDER_TEXT), "{html}");
        assert!(
            Notification::DockerCleanupSuccess {
                server: server(),
                message: "ok".into()
            }
            .throttle()
            .is_none()
        );
    }

    #[test]
    fn a_recovered_job_goes_to_the_channels_of_its_failure() {
        let c = ctx();
        let n = Notification::ScheduledJobRecovered {
            failure_event: FailureEvent::ScheduledTaskFailure,
            title: "Scheduled task (cleanup) works again.".into(),
            webhook_event: "task_recovered".into(),
            url: Some("https://c.example/t".into()),
            webhook_data: json!({"task_name": "cleanup", "task_uuid": "tu"}),
        };
        assert_eq!(n.event(), "scheduled_task_failure");
        assert!(n.telegram_thread_column().is_none());
        assert!(n.throttle().is_none());
        let mail = n.to_mail(&c, false).unwrap();
        assert_eq!(
            mail.subject,
            "Coolify: Scheduled task (cleanup) works again."
        );
        assert!(
            mail.html
                .contains("The last run succeeded after failed runs.")
        );
        assert!(mail.html.contains("https://c.example/t"));
        let discord = n.to_discord(&c);
        assert_eq!(
            discord.title,
            ":white_check_mark: Scheduled task (cleanup) works again."
        );
        assert_eq!(
            discord.description,
            "The last run succeeded after failed runs."
        );
        assert_eq!(
            discord.fields,
            vec![(
                "Link".to_owned(),
                "[Open in Coolify](https://c.example/t)".to_owned(),
                false
            )]
        );
        assert_eq!(
            n.to_telegram(&c).message,
            "Coolify: Scheduled task (cleanup) works again.\n\nThe last run succeeded after failed runs."
        );
        let pushover = n.to_pushover(&c);
        assert_eq!(pushover.level, "success");
        assert_eq!(
            pushover.buttons,
            vec![json!({"text": "Open in Coolify", "url": "https://c.example/t"})]
        );
        assert_eq!(
            n.to_slack(&c).description,
            "The last run succeeded after failed runs.\n\n*Link:* https://c.example/t"
        );
        assert_eq!(
            coolify_compat::php_json::encode(&n.to_webhook(&c)),
            r#"{"success":true,"message":"Scheduled task (cleanup) works again.","event":"task_recovered","task_name":"cleanup","task_uuid":"tu","url":"https:\/\/c.example\/t"}"#
        );

        // Without a link: no button, no field, and no `url` in the webhook.
        let n = Notification::ScheduledJobRecovered {
            failure_event: FailureEvent::BackupFailure,
            title: "Backup works again.".into(),
            webhook_event: "backup_recovered".into(),
            url: None,
            webhook_data: Value::Null,
        };
        assert_eq!(n.event(), "backup_failure");
        assert!(n.to_discord(&c).fields.is_empty());
        assert!(n.to_pushover(&c).buttons.is_empty());
        assert_eq!(
            n.to_webhook(&c),
            json!({"success": true, "message": "Backup works again.", "event": "backup_recovered"})
        );
        assert_eq!(
            FailureEvent::DockerCleanupFailure.as_str(),
            "docker_cleanup_failure"
        );
    }

    #[test]
    fn a_failing_api_token_is_reported_with_its_last_request() {
        let c = ctx();
        let n = Notification::ApiTokenFailing {
            token_name: "ci".into(),
            failed_requests: 30,
            last_request: "GET /api/v1/servers → 401".into(),
        };
        let summary = "API token 'ci' sent 30 failed requests in one minute. Coolify now answers its requests with 429 (Too Many Requests) until the failures stop.";
        let action = "Find the client that uses this token and fix its requests, give the token the permissions it needs, or revoke the token.";
        assert_eq!(n.event(), "api_token_failing");
        let mail = n.to_mail(&c, false).unwrap();
        assert_eq!(
            mail.subject,
            "Coolify: API token 'ci' sends many failed requests"
        );
        assert!(
            mail.html
                .contains("Last failed request: GET /api/v1/servers → 401")
        );
        assert!(mail.html.contains("https://c.example/security/api-tokens"));
        let discord = n.to_discord(&c);
        assert_eq!(discord.title, "🔑 API token sends many failed requests");
        assert_eq!(
            discord.description,
            format!("{summary}\n\n**Action Required:** {action}")
        );
        assert_eq!(discord.color, 0xffa743);
        assert_eq!(
            discord.fields,
            vec![
                (
                    "Last failed request".to_owned(),
                    "GET /api/v1/servers → 401".to_owned(),
                    false
                ),
                (
                    "Manage tokens".to_owned(),
                    "[Open API Tokens](https://c.example/security/api-tokens)".to_owned(),
                    false
                ),
            ]
        );
        assert_eq!(
            n.to_telegram(&c).message,
            format!(
                "Coolify: {summary}\n\nLast failed request: GET /api/v1/servers → 401\n\nAction Required: {action}"
            )
        );
        let pushover = n.to_pushover(&c);
        assert_eq!(pushover.level, "warning");
        assert_eq!(
            pushover.message,
            format!(
                "API token &#039;ci&#039; sent 30 failed requests in one minute. Coolify now answers its requests with 429 (Too Many Requests) until the failures stop.<br/><br/>Last failed request: GET /api/v1/servers → 401<br/><br/><b>Action Required:</b> {action}"
            )
        );
        assert_eq!(
            n.to_slack(&c).description,
            format!(
                "{summary}\n\nLast failed request: GET /api/v1/servers → 401\n\n*Action Required:* {action}\n\nManage tokens: https://c.example/security/api-tokens"
            )
        );
        assert_eq!(
            n.to_webhook(&c),
            json!({
                "success": false,
                "message": summary,
                "event": "api_token_failing",
                "token_name": "ci",
                "failed_requests": 30,
                "last_request": "GET /api/v1/servers → 401",
                "url": "https://c.example/security/api-tokens",
            })
        );
    }

    #[test]
    fn notification_text_is_escaped_for_each_channel() {
        assert_eq!(text_plain("  a\tb\u{7}c  "), "a b c");
        assert_eq!(text_slack("<a & b>"), "&lt;a &amp; b&gt;");
        assert_eq!(
            text_html("it's <b>\"x\"</b>"),
            "it&apos;s &lt;b&gt;&quot;x&quot;&lt;/b&gt;"
        );
    }
}
