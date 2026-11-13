//! `App\Notifications\ImageScan\VulnerabilityDigest`: the daily security digest of a team (spec
//! 15.8): resources that became update_available or check_needed, or got a new fixing version,
//! with their status, the primary action, and a link; ignores that end soon; failing servers.
//! Chat and webhook channels get no vulnerability IDs and no counts; the email adds the why-list
//! of each resource (at most [`MAIL_ID_LIMIT`] rows in total).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::mail::{self, MailIgnore, MailResource, MailServer};
use super::text::{self, WhyEntry};
use crate::notifications::{DiscordMessage, PushoverMessage, SlackMessage, TelegramMessage};

/// `VulnerabilityDigest::TITLE`.
pub const TITLE: &str = "Daily security digest";

/// `VulnerabilityDigest::BUTTON`.
pub const BUTTON: &str = "Open security status";

/// `VulnerabilityDigest::MAIL_ID_LIMIT`.
pub const MAIL_ID_LIMIT: usize = 10;

/// Text limits: Discord embed description, Telegram message, Slack section text, Pushover
/// message.
pub const DISCORD_LIMIT: usize = 4096;
pub const TELEGRAM_LIMIT: usize = 4096;
pub const SLACK_LIMIT: usize = 3000;
pub const PUSHOVER_LIMIT: usize = 1024;

const NAME_LIMIT: usize = 100;
const SENTENCE_LIMIT: usize = 500;

/// `DiscordMessage::errorColor()` and `DiscordMessage::warningColor()`.
pub const DISCORD_ERROR: i64 = 0xff705f;
pub const DISCORD_WARNING: i64 = 0xffa743;

/// `SlackMessage::errorColor()` and `SlackMessage::warningColor()`.
pub const SLACK_ERROR: &str = "#ff0000";
pub const SLACK_WARNING: &str = "#ffa500";

/// One resource of the digest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DigestResource {
    pub name: String,
    pub uuid: String,
    pub url: Option<String>,
    /// `SecurityStatusType` value.
    pub status: String,
    /// `SecuritySentences::badge($status)`.
    pub badge: String,
    /// The sentence of the primary action.
    pub action: Option<String>,
    pub why: Vec<WhyEntry>,
}

/// One ignore that ends within 7 days.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestIgnore {
    /// The name of the resource; `None`: all resources of the team.
    pub resource: Option<String>,
    /// The vulnerability ID.
    pub id: Option<String>,
    pub package: Option<String>,
    /// `Y-m-d`.
    pub expires_at: String,
}

/// One server whose rescans failed 2 days in a row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestServer {
    pub name: String,
    pub url: String,
}

/// The notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VulnerabilityDigest {
    pub resources: Vec<DigestResource>,
    pub ignores: Vec<DigestIgnore>,
    pub servers: Vec<DigestServer>,
    /// `rtrim(base_url(), '/').'/vulnerabilities'`.
    pub team_url: String,
}

impl VulnerabilityDigest {
    /// `new VulnerabilityDigest($digest)`. `base_url`: `base_url()`.
    pub fn new(
        resources: Vec<DigestResource>,
        ignores: Vec<DigestIgnore>,
        servers: Vec<DigestServer>,
        base_url: &str,
    ) -> Self {
        Self {
            resources,
            ignores,
            servers,
            team_url: format!("{}/vulnerabilities", base_url.trim_end_matches('/')),
        }
    }

    /// `toMail()`: the subject.
    pub fn mail_subject(&self) -> String {
        format!("Coolify: {TITLE}")
    }

    /// `toMail()`: what the view `emails.image-vulnerabilities` renders into the email layout.
    pub fn mail_slot(&self) -> String {
        let mut budget = MAIL_ID_LIMIT;
        let resources: Vec<MailResource> = self
            .resources
            .iter()
            .map(|resource| {
                let why: Vec<String> = resource
                    .why
                    .iter()
                    .take(budget)
                    .map(text::mail_why)
                    .collect();
                budget -= why.len();
                MailResource {
                    name: text::mail_markdown(&resource.name),
                    url: resource.url.clone(),
                    summary: text::mail_markdown(&resource_sentence(resource)),
                    why,
                }
            })
            .collect();

        let ignores: Vec<MailIgnore> = self
            .ignores
            .iter()
            .map(|ignore| {
                let id = match ignore.id.as_deref() {
                    Some(id) if budget > 0 => {
                        budget -= 1;
                        Some(id)
                    }
                    Some(_) => Some("A vulnerability"),
                    None => None,
                };
                let package = ignore.package.as_deref();
                let subject = match (id, package) {
                    (Some(id), Some(package)) => format!("{id} in {package}"),
                    (Some(id), None) => id.to_owned(),
                    (None, package) => format!("Package {}", package.unwrap_or_default()),
                };
                MailIgnore {
                    subject: text::mail_markdown(&subject),
                    resource: ignore.resource.as_deref().map(text::mail_markdown),
                    expires_at: ignore.expires_at.clone(),
                }
            })
            .collect();

        let servers: Vec<MailServer> = self
            .servers
            .iter()
            .map(|server| MailServer {
                name: text::mail_markdown(&server.name),
                url: server.url.clone(),
            })
            .collect();

        mail::image_vulnerabilities(
            "Changes in the security status of your resources since the last digest.",
            &resources,
            &ignores,
            &servers,
            &self.team_url,
        )
    }

    /// `toDiscord()`.
    pub fn to_discord(&self) -> DiscordMessage {
        let suffix = format!("\n\n[{BUTTON}]({})", self.team_url);
        let lines = self.chat_lines(
            |resource| {
                let name = match &resource.url {
                    Some(url) => format!("[{}]({url})", text::discord(&resource_name(resource))),
                    None => text::discord(&resource_name(resource)),
                };
                format!("• {name}: {}", text::discord(&resource_sentence(resource)))
            },
            text::discord,
            DISCORD_LIMIT.saturating_sub(text::mb_strlen(&suffix)),
        );
        DiscordMessage {
            title: TITLE.to_owned(),
            description: format!("{}{suffix}", lines.join("\n")),
            color: DISCORD_WARNING,
            is_critical: false,
            fields: Vec::new(),
        }
    }

    /// `toSlack()`.
    pub fn to_slack(&self) -> SlackMessage {
        let suffix = format!("\n\n<{}|{BUTTON}>", self.team_url);
        let lines = self.chat_lines(
            |resource| {
                let name = match &resource.url {
                    Some(url) => format!("<{url}|{}>", text::slack(&resource_name(resource))),
                    None => text::slack(&resource_name(resource)),
                };
                format!("• {name}: {}", text::slack(&resource_sentence(resource)))
            },
            text::slack,
            SLACK_LIMIT.saturating_sub(text::mb_strlen(&suffix)),
        );
        SlackMessage {
            title: TITLE.to_owned(),
            description: format!("{}{suffix}", lines.join("\n")),
            color: SLACK_WARNING,
        }
    }

    /// `toTelegram()`.
    pub fn to_telegram(&self) -> TelegramMessage {
        let prefix = format!("Coolify: {TITLE}\n\n");
        let lines = self.chat_lines(
            |resource| {
                let link = match &resource.url {
                    Some(url) => format!("\n  {url}"),
                    None => String::new(),
                };
                format!(
                    "• {}: {}{link}",
                    text::plain(&resource_name(resource)),
                    text::plain(&resource_sentence(resource))
                )
            },
            text::plain,
            TELEGRAM_LIMIT.saturating_sub(text::mb_strlen(&prefix)),
        );
        TelegramMessage {
            message: format!("{prefix}{}", lines.join("\n")),
            buttons: vec![json!({"text": BUTTON, "url": self.team_url})],
        }
    }

    /// `toPushover()`.
    pub fn to_pushover(&self) -> PushoverMessage {
        // PushoverMessage appends each button to the message as a link.
        let button_length =
            text::mb_strlen(&format!("&nbsp;<a href='{}'>{BUTTON}</a>", self.team_url));
        let lines = self.chat_lines(
            |resource| {
                let name = match &resource.url {
                    Some(url) => format!(
                        "<a href='{}'>{}</a>",
                        text::html(url),
                        text::html(&resource_name(resource))
                    ),
                    None => text::html(&resource_name(resource)),
                };
                format!("• {name}: {}", text::html(&resource_sentence(resource)))
            },
            text::html,
            PUSHOVER_LIMIT.saturating_sub(button_length),
        );
        PushoverMessage {
            title: TITLE.to_owned(),
            message: lines.join("\n"),
            buttons: vec![json!({"text": BUTTON, "url": self.team_url})],
            level: "warning",
        }
    }

    /// `toWebhook()`.
    pub fn to_webhook(&self) -> Value {
        let resources: Vec<Value> = self
            .resources
            .iter()
            .map(|resource| {
                json!({
                    "name": resource.name,
                    "uuid": resource.uuid,
                    "url": resource.url,
                    "status": resource.status,
                    "action": resource.action,
                })
            })
            .collect();
        let servers: Vec<Value> = self
            .servers
            .iter()
            .map(|server| json!({"name": server.name, "url": server.url}))
            .collect();
        json!({
            "success": false,
            "message": TITLE,
            "event": "image_vulnerabilities_digest",
            "url": self.team_url,
            "resources": resources,
            "ignores_ending": !self.ignores.is_empty(),
            "servers_failing": servers,
        })
    }

    /// `chatLines()`: the lines of a chat message that fit in `budget` characters: resources,
    /// then ignores and failing servers, without IDs or counts, then "And N more." for the rest.
    fn chat_lines(
        &self,
        resource_line: impl Fn(&DigestResource) -> String,
        escape: impl Fn(&str) -> String,
        budget: usize,
    ) -> Vec<String> {
        let mut lines: Vec<String> = self.resources.iter().map(resource_line).collect();
        match self.ignores.len() {
            0 => {}
            1 => lines.push("An ignore ends within 7 days.".to_owned()),
            _ => lines.push("Some ignores end within 7 days.".to_owned()),
        }
        for server in &self.servers {
            lines.push(escape(&format!(
                "Image scans failed for 2 days on {}.",
                text::limit(&server.name, NAME_LIMIT)
            )));
        }
        text::fit_lines(&lines, budget)
    }
}

/// `name($resource)`.
fn resource_name(resource: &DigestResource) -> String {
    text::limit(&resource.name, NAME_LIMIT)
}

/// `resourceSentence($resource)`: "Security update. Update to 1.28." / "Check needed. Check
/// again: the last check was 3 days ago."
fn resource_sentence(resource: &DigestResource) -> String {
    let sentence = format!(
        "{}. {}",
        resource.badge,
        resource.action.as_deref().unwrap_or_default()
    );
    text::limit(
        sentence.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B']),
        SENTENCE_LIMIT,
    )
}

#[cfg(test)]
mod tests {
    use coolify_compat::php_json;

    use super::*;

    const BASE: &str = "https://coolify.example.com";

    fn resource(name: &str, url: Option<&str>) -> DigestResource {
        DigestResource {
            name: name.to_owned(),
            uuid: format!("uuid-{name}"),
            url: url.map(str::to_owned),
            status: "update_available".to_owned(),
            badge: "Security update".to_owned(),
            action: Some("Update to 1.28.".to_owned()),
            why: Vec::new(),
        }
    }

    /// A long resource line for the channel limits (`notifyLongResource()`).
    fn long_resource(index: usize) -> DigestResource {
        DigestResource {
            name: format!("{}{index}", "n".repeat(95)),
            uuid: format!("uuid-{index}"),
            url: Some(format!(
                "{BASE}/project/p/environment/e/application/{index}/vulnerabilities"
            )),
            status: "update_available".to_owned(),
            badge: "Security update".to_owned(),
            action: Some(format!(
                "Change the base image to ghcr.io/{}:1.0 in your Dockerfile, then redeploy.",
                "a".repeat(150)
            )),
            why: Vec::new(),
        }
    }

    fn why(id: &str, title: &str) -> WhyEntry {
        WhyEntry {
            title: title.to_owned(),
            display_id: id.to_owned(),
            link: None,
            score: Some(9.8),
            severity: "critical".to_owned(),
            signal: None,
        }
    }

    fn ignore(id: Option<&str>, package: Option<&str>, resource: Option<&str>) -> DigestIgnore {
        DigestIgnore {
            resource: resource.map(str::to_owned),
            id: id.map(str::to_owned),
            package: package.map(str::to_owned),
            expires_at: "2026-10-30".to_owned(),
        }
    }

    fn server() -> DigestServer {
        DigestServer {
            name: "server".to_owned(),
            url: format!("{BASE}/server/s"),
        }
    }

    fn pushover_text(message: &PushoverMessage) -> String {
        let mut out = message.message.clone();
        for button in &message.buttons {
            out.push_str(&format!(
                "&nbsp;<a href='{}'>{}</a>",
                button["url"].as_str().unwrap_or_default(),
                button["text"].as_str().unwrap_or_default()
            ));
        }
        out
    }

    #[test]
    fn builds_each_channel() {
        let digest = VulnerabilityDigest::new(
            vec![
                resource("api", Some("https://coolify.example.com/r/api")),
                resource("web", None),
            ],
            vec![ignore(Some("CVE-2026-0300"), None, None)],
            vec![server()],
            "https://coolify.example.com/",
        );
        assert_eq!(
            digest.team_url,
            "https://coolify.example.com/vulnerabilities"
        );
        assert_eq!(digest.mail_subject(), "Coolify: Daily security digest");

        let discord = digest.to_discord();
        assert_eq!(discord.title, "Daily security digest");
        assert_eq!(
            discord.description,
            "• [api](https://coolify.example.com/r/api): Security update. Update to 1.28.\n\
             • web: Security update. Update to 1.28.\n\
             An ignore ends within 7 days.\n\
             Image scans failed for 2 days on server.\n\n\
             [Open security status](https://coolify.example.com/vulnerabilities)"
        );
        assert_eq!(discord.color, 0xffa743);
        assert!(!discord.is_critical);

        let slack = digest.to_slack();
        assert_eq!(
            slack.description,
            "• <https://coolify.example.com/r/api|api>: Security update. Update to 1.28.\n\
             • web: Security update. Update to 1.28.\n\
             An ignore ends within 7 days.\n\
             Image scans failed for 2 days on server.\n\n\
             <https://coolify.example.com/vulnerabilities|Open security status>"
        );
        assert_eq!(slack.color, "#ffa500");

        let telegram = digest.to_telegram();
        assert_eq!(
            telegram.message,
            "Coolify: Daily security digest\n\n\
             • api: Security update. Update to 1.28.\n  https://coolify.example.com/r/api\n\
             • web: Security update. Update to 1.28.\n\
             An ignore ends within 7 days.\n\
             Image scans failed for 2 days on server."
        );
        assert_eq!(
            telegram.buttons,
            vec![
                json!({"text": "Open security status", "url": "https://coolify.example.com/vulnerabilities"})
            ]
        );

        let pushover = digest.to_pushover();
        assert_eq!(
            pushover.message,
            "• <a href='https://coolify.example.com/r/api'>api</a>: Security update. Update to 1.28.\n\
             • web: Security update. Update to 1.28.\n\
             An ignore ends within 7 days.\n\
             Image scans failed for 2 days on server."
        );
        assert_eq!(pushover.level, "warning");
        assert_eq!(pushover.buttons, telegram.buttons);

        assert_eq!(
            php_json::encode(&digest.to_webhook()),
            php_json::encode(&json!({
                "success": false,
                "message": "Daily security digest",
                "event": "image_vulnerabilities_digest",
                "url": "https://coolify.example.com/vulnerabilities",
                "resources": [
                    {"name": "api", "uuid": "uuid-api", "url": "https://coolify.example.com/r/api", "status": "update_available", "action": "Update to 1.28."},
                    {"name": "web", "uuid": "uuid-web", "url": null, "status": "update_available", "action": "Update to 1.28."},
                ],
                "ignores_ending": true,
                "servers_failing": [{"name": "server", "url": "https://coolify.example.com/server/s"}],
            }))
        );
    }

    #[test]
    fn names_ignores_without_ids_in_chat_channels() {
        let mut digest = VulnerabilityDigest::new(
            Vec::new(),
            vec![
                ignore(Some("CVE-2026-0300"), None, None),
                ignore(Some("GHSA-aaaa-bbbb-cccc"), Some("openssl"), Some("api")),
            ],
            Vec::new(),
            BASE,
        );
        for text in [
            digest.to_discord().description,
            digest.to_slack().description,
            digest.to_telegram().message,
            digest.to_pushover().message,
            php_json::encode(&digest.to_webhook()),
        ] {
            assert!(!text.contains("CVE-") && !text.contains("GHSA-"), "{text}");
        }
        assert!(
            digest
                .to_telegram()
                .message
                .ends_with("\n\nSome ignores end within 7 days.")
        );
        digest.ignores.clear();
        assert_eq!(digest.to_webhook()["ignores_ending"], false);
        assert_eq!(
            digest.to_telegram().message,
            "Coolify: Daily security digest\n\n"
        );
    }

    #[test]
    fn a_status_without_an_action_is_only_the_badge() {
        let mut check = resource("api", None);
        check.badge = "Check needed".to_owned();
        check.action = None;
        assert_eq!(resource_sentence(&check), "Check needed.");
        check.action = Some("Check again: the last check was 3 days ago.".to_owned());
        assert_eq!(
            resource_sentence(&check),
            "Check needed. Check again: the last check was 3 days ago."
        );
        check.action = Some("x".repeat(600));
        assert_eq!(text::mb_strlen(&resource_sentence(&check)), 500);
    }

    #[test]
    fn the_email_lists_the_why_rows_and_the_ignores() {
        let mut api = resource("api", Some("https://coolify.example.com/r/api"));
        api.why = vec![why("CVE-2026-0001", "The bug lets attackers read memory")];
        let digest = VulnerabilityDigest::new(
            vec![api],
            vec![
                ignore(Some("CVE-2026-0300"), None, None),
                ignore(Some("CVE-2026-0301"), Some("openssl"), Some("api")),
                ignore(None, Some("zlib"), None),
            ],
            vec![server()],
            BASE,
        );
        assert_eq!(
            digest.mail_slot(),
            "\nChanges in the security status of your resources since the last digest.\n\
             \n## Resources\n\n\
             ### [api](https://coolify.example.com/r/api)\n\n\
             Security update\\. Update to 1\\.28\\.\n\n\
             - The bug lets attackers read memory ([CVE\\-2026\\-0001](https://nvd.nist.gov/vuln/detail/CVE-2026-0001), 9\\.8 Critical).\n\n\
             ## Ignores that end within 7 days\n\n\
             - CVE\\-2026\\-0300 on all resources, ends on 2026-10-30\n\
             - CVE\\-2026\\-0301 in openssl on api, ends on 2026-10-30\n\
             - Package zlib on all resources, ends on 2026-10-30\n\n\
             ## Servers whose scans failed for 2 days\n\n\
             - [server](https://coolify.example.com/server/s)\n\n\
             ---\n\nSee all results on the [Security page](https://coolify.example.com/vulnerabilities).\n"
        );
    }

    #[test]
    fn the_email_lists_at_most_ten_ids() {
        // "lists at most 10 problems in the digest email"
        let resources: Vec<DigestResource> = (1..=5)
            .map(|index| {
                let mut r = resource(&format!("app-{index}"), None);
                r.why = (1..=3)
                    .map(|n| why(&format!("CVE-2026-{index:02}{n:02}"), "A bug"))
                    .collect();
                r
            })
            .collect();
        let digest = VulnerabilityDigest::new(
            resources,
            vec![ignore(Some("CVE-2026-0300"), Some("openssl"), None)],
            Vec::new(),
            BASE,
        );
        let slot = digest.mail_slot();
        let ids = regex::Regex::new(r"\[CVE\\-2026\\-\d{4}\]").unwrap();
        assert_eq!(ids.find_iter(&slot).count(), 10);
        // The fourth resource gets the last row, the fifth none.
        assert!(slot.contains("CVE\\-2026\\-0401"));
        assert!(!slot.contains("CVE\\-2026\\-0402"));
        assert!(slot.contains("### app\\-5\n\nSecurity update\\. Update to 1\\.28\\.\n\n\n"));
        // An ignore beyond the limit is named without its ID.
        assert!(
            slot.contains("- A vulnerability in openssl on all resources, ends on 2026-10-30\n")
        );
        assert!(!slot.contains("0300"));
    }

    #[test]
    fn escapes_names_and_titles_per_channel() {
        // "escapes resource names per channel" and "escapes the titles of the why-list in the
        // email and links the IDs"
        let name = "*bold* & [link](https://evil.example)";
        let url = "https://coolify.example.com/r/api";
        let mut evil = resource(name, Some(url));
        evil.why = vec![why(
            "CVE-2026-0002",
            "*bold* <b>x</b> [link](https://evil.example)",
        )];
        let digest = VulnerabilityDigest::new(vec![evil], Vec::new(), Vec::new(), BASE);

        assert!(digest.to_discord().description.contains(
            "[\\*bold\\* & \\[link\\]\\(https://evil.example\\)](https://coolify.example.com/"
        ));
        assert!(
            digest
                .to_slack()
                .description
                .contains("|*bold* &amp; [link](https://evil.example)>")
        );
        assert!(
            digest
                .to_pushover()
                .message
                .contains(">*bold* &amp; [link](https://evil.example)</a>")
        );
        let slot = digest.mail_slot();
        assert!(slot.contains(
            "### [\\*bold\\* &amp; \\[link\\]\\(https://evil\\.example\\)](https://coolify.example.com/r/api)\n"
        ));
        assert!(slot.contains(
            "- \\*bold\\* &lt;b&gt;x&lt;/b&gt; \\[link\\]\\(https://evil\\.example\\) \
             ([CVE\\-2026\\-0002](https://nvd.nist.gov/vuln/detail/CVE-2026-0002), 9\\.8 Critical).\n"
        ));
    }

    #[test]
    fn fits_every_chat_channel_in_its_length_limit() {
        // "fits every chat channel in its length limit"
        let digest = VulnerabilityDigest::new(
            (1..=60).map(long_resource).collect(),
            vec![ignore(Some("CVE-2026-0300"), None, None)],
            vec![server()],
            BASE,
        );
        let discord = digest.to_discord().description;
        let telegram = digest.to_telegram().message;
        let slack = digest.to_slack().description;
        let pushover = pushover_text(&digest.to_pushover());

        let length = text::mb_strlen;
        assert!(
            length(&discord) <= 4096 && length(&discord) > 3000,
            "{}",
            length(&discord)
        );
        assert!(
            length(&telegram) <= 4096 && length(&telegram) > 3000,
            "{}",
            length(&telegram)
        );
        assert!(
            length(&slack) <= 3000 && length(&slack) > 2000,
            "{}",
            length(&slack)
        );
        assert!(length(&pushover) <= 1024, "{}", length(&pushover));
        let more = regex::Regex::new(r"And \d+ more\.").unwrap();
        for text in [&discord, &telegram, &slack, &pushover] {
            assert!(more.is_match(text), "{text}");
        }
        assert_eq!(
            digest.to_webhook()["resources"].as_array().map(Vec::len),
            Some(60)
        );
    }
}
