//! Hand ports of `resources/views/emails/*.blade.php` and `components/emails/*`.
//!
//! Each function builds what Blade's output buffer holds: `{{ }}` escaped with `e()`, a
//! directive line gone. `layout` then does what `<x-emails.layout>` does: trim the slot,
//! `Markdown::parse` it, and wrap it in the header and footer.

use serde_json::Value;

use super::markdown;
use super::messages::{CommitDetails, SslResource, TraefikServer};
use super::php::{e, filled, php_string, truthy, ucfirst, value_truthy};

/// `components/emails/layout`, `header` and `footer`.
fn layout(app_name: &str, slot: &str) -> String {
    let slot = super::php::php_trim(slot);
    format!(
        "Hello,\n{}\n{}\n\nThank you,<br>\n{}\n\n{}\n",
        markdown::parse(slot),
        markdown::parse("---"),
        e(app_name),
        markdown::parse("[Contact Support](https://coolify.io/docs/contact)"),
    )
}

/// `components/emails/deployment-commit`.
fn deployment_commit(commit: Option<&MailCommit>) -> String {
    let Some(c) = commit else {
        return String::new();
    };
    let mut line = None;
    if let Some(short) = c.short_sha.as_deref().filter(|s| truthy(Some(s))) {
        let short = e(short);
        let mut l = match c.url.as_deref().filter(|u| truthy(Some(u))) {
            Some(url) => format!("**Commit:** [{short}]({})", e(url)),
            None => format!("**Commit:** {short}"),
        };
        if let Some(branch) = c.branch.as_deref().filter(|b| truthy(Some(b))) {
            l.push_str(" on ");
            l.push_str(&e(branch));
        }
        line = Some(l);
    }
    let message = c
        .message
        .as_deref()
        .filter(|m| truthy(Some(m)))
        .map(|m| format!("**Commit message:**\n\n{}", e(m)));
    let parts: Vec<String> = [line, message]
        .into_iter()
        .flatten()
        .filter(|p| truthy(Some(p)))
        .collect();
    format!("{}\n\n", parts.join("\n\n"))
}

/// `IncludesDeploymentCommitDetails::deploymentCommitDetailsForMail`.
#[derive(Debug, Clone)]
pub struct MailCommit {
    pub short_sha: Option<String>,
    pub url: Option<String>,
    pub branch: Option<String>,
    pub message: Option<String>,
}

/// `escapeMailMarkdown`.
fn escape_mail_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\`*_{}[]()#+-.!|~=".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `formatCommitMessageForMail`.
fn format_commit_message(message: &str) -> String {
    const R: &str = r"(?:\r\n|[\n\x0b\x0c\r\x{85}\x{2028}\x{2029}])";
    let para = regex::Regex::new(&format!(r"{R}\s*{R}")).unwrap_or_else(|_| unreachable!());
    let line = regex::Regex::new(R).unwrap_or_else(|_| unreachable!());
    let message = message.replace("\r\n", "\n");
    para.split(&message)
        .map(|p| {
            line.split(p)
                .map(|l| escape_mail_markdown(super::php::php_trim(l)))
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join("\\\n")
        })
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

impl CommitDetails {
    /// `deploymentCommitDetailsForMail`, after its check of the team's setting.
    pub fn for_mail(&self) -> Option<MailCommit> {
        if self.sha.is_none() && self.message.is_none() {
            return None;
        }
        Some(MailCommit {
            short_sha: self.sha.as_ref().map(|s| s.chars().take(7).collect()),
            url: self.url.clone(),
            branch: self
                .branch
                .as_deref()
                .filter(|b| truthy(Some(b)))
                .map(escape_mail_markdown),
            message: self
                .message
                .as_deref()
                .filter(|m| truthy(Some(m)))
                .map(format_commit_message),
        })
    }
}

pub fn deployment_success(
    app: &str,
    name: &str,
    fqdn: Option<&str>,
    pr: i64,
    url: &str,
    commit: Option<&MailCommit>,
) -> String {
    let (n, f) = (e(name), e(fqdn.unwrap_or_default()));
    let head = if pr == 0 {
        format!("A new version of {n} is available at [{f}]({f}) .\n")
    } else {
        format!("Pull request #{pr} of {n} deployed successfully\n[{f}]({f}).\n")
    };
    layout(
        app,
        &format!(
            "{head}\n{}[View Deployment Logs]({})\n",
            deployment_commit(commit),
            e(url)
        ),
    )
}

pub fn deployment_failed(
    app: &str,
    name: &str,
    fqdn: Option<&str>,
    pr: i64,
    url: &str,
    commit: Option<&MailCommit>,
) -> String {
    let (n, f) = (e(name), e(fqdn.unwrap_or_default()));
    let head = if pr == 0 {
        format!("Failed to deploy a new version of {n} at [{f}]({f}) .\n")
    } else {
        format!("Failed to deploy a pull request #{pr} of {n} at\n[{f}]({f}).\n")
    };
    layout(
        app,
        &format!(
            "{head}\n{}[View Deployment Logs]({})\n",
            deployment_commit(commit),
            e(url)
        ),
    )
}

/// `application-status-changes`: upstream's view reads `$application_url`, which
/// `StatusChanged::toMail` does not pass, so the link is always empty. Nothing dispatches
/// `StatusChanged` upstream (only `php artisan emails`).
pub fn application_status_changes(app: &str, name: &str) -> String {
    layout(
        app,
        &format!(
            "{} has been stopped.\n\nIf it was your intention to stop this application, you can ignore this email.\n\nIf not, [check what is going on]().\n",
            e(name)
        ),
    )
}

pub fn restart_limit_reached(
    app: &str,
    name: &str,
    restart_count: i64,
    max: i64,
    url: &str,
) -> String {
    layout(
        app,
        &format!(
            "{} has been automatically stopped after {restart_count} restarts (limit: {max}).\n\nThe application appears to be in a restart loop. Please investigate the issue and redeploy when ready.\n\n[Check what is going on]({}).\n",
            e(name),
            e(url)
        ),
    )
}

pub fn container_restarted(app: &str, container: &str, server: &str) -> String {
    let mut slot = format!(
        "A resource ({}) has been restarted automatically on {}, because it was stopped unexpectedly.\n\n",
        e(container),
        e(server)
    );
    if container == "coolify-proxy" {
        slot.push_str("Coolify Proxy should run on your server as you have FQDNs set up in one of your resources.\n\nIf you don't want to use Coolify Proxy, please remove FQDN from your resources or set Proxy type to Custom(None).\n");
    }
    layout(app, &slot)
}

/// The `Database backup for ... (db:...)` start of the three backup views.
fn backup_head(name: &str, database: Option<&str>, frequency: &str) -> String {
    let db = if truthy(database) {
        format!("(db:{})", e(database.unwrap_or_default()))
    } else {
        String::new()
    };
    format!(
        "Database backup for {} {db} with frequency of {}",
        e(name),
        e(frequency)
    )
}

pub fn backup_success(app: &str, name: &str, database: Option<&str>, frequency: &str) -> String {
    layout(
        app,
        &format!(
            "{} was successful.\n",
            backup_head(name, database, frequency)
        ),
    )
}

pub fn backup_failed(
    app: &str,
    name: &str,
    database: Option<&str>,
    frequency: &str,
    output: &str,
) -> String {
    layout(
        app,
        &format!(
            "{} was FAILED.\n\n### Reason\n\n{}\n",
            backup_head(name, database, frequency),
            e(output)
        ),
    )
}

pub fn backup_success_with_s3_warning(
    app: &str,
    name: &str,
    database: Option<&str>,
    frequency: &str,
    s3_error: &str,
    s3_url: Option<&str>,
) -> String {
    let mut slot = format!(
        "{} succeeded locally but failed to upload to S3.\n\nS3 Error: {}\n\n",
        backup_head(name, database, frequency),
        e(s3_error)
    );
    if truthy(s3_url) {
        slot.push_str(&format!(
            "Check S3 Configuration: {}\n",
            e(s3_url.unwrap_or_default())
        ));
    }
    layout(app, &slot)
}

/// `backup-missing` has no layout: its text is the mail body as it is.
pub fn backup_missing(database: &str, days: &str, last: Option<&str>) -> String {
    let tail = if truthy(last) {
        format!(
            "The last execution was at {}.\n",
            e(last.unwrap_or_default())
        )
    } else {
        "This schedule has never produced an execution.\n".to_owned()
    };
    format!(
        "The enabled backup schedule for {} has produced no executions in the last {} day(s).\n\n{tail}",
        e(database),
        e(days)
    )
}

pub fn docker_cleanup_success(app: &str, name: &str, text: &str) -> String {
    layout(
        app,
        &format!(
            "Docker Cleanup on {} succeeded with the following message:\n\n\n<pre>\n{}\n</pre>\n\n",
            e(name),
            e(text)
        ),
    )
}

pub fn docker_cleanup_failed(app: &str, name: &str, text: &str) -> String {
    layout(
        app,
        &format!(
            "Docker Cleanup on {} FAILED with the following error:\n\n<pre>\n{}\n</pre>\n\n",
            e(name),
            e(text)
        ),
    )
}

pub fn email_change_verification(app: &str, new_email: &str, code: &str, minutes: i64) -> String {
    layout(
        app,
        &format!(
            "You have requested to change your email address to: {}\n\nPlease use the following verification code to confirm this change:\n\nVerification Code: {}\n\nThis code is valid for {minutes} minutes.\n\nIf you did not request this change, please ignore this email and your email address will remain unchanged.\n",
            e(new_email),
            e(code)
        ),
    )
}

pub fn hetzner_deletion_failed(app: &str, id: i64, error: &str) -> String {
    layout(
        app,
        &format!(
            "Failed to delete Hetzner server #{id} from Hetzner Cloud.\n\nError:\n<pre>\n{}\n</pre>\n\nThe server has been removed from Coolify, but may still exist in your Hetzner Cloud account.\n\nPlease check your Hetzner Cloud console and manually delete the server if needed to avoid ongoing charges.\n\n",
            e(error)
        ),
    )
}

pub fn high_disk_usage(app: &str, name: &str, disk: i64, threshold: i64) -> String {
    layout(
        app,
        &format!(
            "Your server ({}) has high disk usage ({disk}% used). Threshold is {threshold}%.\n\nPlease cleanup your disk to prevent data-loss. Here are some [tips](https://coolify.io/docs/knowledge-base/server/automated-cleanup).\n\n(You can change the threshold in the Server Settings menu.)\n",
            e(name)
        ),
    )
}

pub fn invitation_link(app: &str, team: &str, link: &str) -> String {
    layout(
        app,
        &format!(
            "You have been invited to \"{}\" on \"{}\".\n\nPlease [click here]({}) to accept the invitation.\n\nIf you have any questions, please contact the team owner.<br><br>\n\nIf it was not you who requested this invitation, please ignore this email.\n",
            e(team),
            e(app),
            e(link)
        ),
    )
}

/// `emails.s3-connection-error`.
pub fn s3_connection_error(app: &str, name: &str, reason: &str, url: &str) -> String {
    layout(
        app,
        &format!(
            "\nConnection could not be established with one of your S3 Storage ({}). Please fix it [here]({}).\n\n{}\n",
            e(name),
            e(url),
            e(reason),
        ),
    )
}

pub fn notification_channel_disabled(
    app: &str,
    channel: &str,
    failures: i64,
    reason: &str,
    url: &str,
) -> String {
    let c = e(channel);
    layout(
        app,
        &format!(
            "\nCoolify could not send {c} notifications for your team {failures} times in a row, so we disabled {c} notifications.\n\nReason: {}\n\nPlease fix the settings and enable {c} notifications again [here]({}).\n",
            e(reason),
            e(url)
        ),
    )
}

pub fn reset_password(app: &str, url: &str, count: i64) -> String {
    layout(
        app,
        &format!(
            "A password reset has been requested for this email address.\n\nClick [here]({}) to reset your password.\n\nThis link will expire in {count} minutes.\n",
            e(url)
        ),
    )
}

/// `emails/email-verification.blade.php`.
pub fn email_verification(app: &str, url: &str) -> String {
    layout(app, &format!("Verify your email [here]({}).\n", e(url)))
}

pub fn scheduled_task_success(app: &str, name: &str, output: &str, url: Option<&str>) -> String {
    layout(
        app,
        &format!(
            "Scheduled task ({}) completed successfully with the following output:\n\n<pre>\n{}\n</pre>\n\nClick [here]({}) to view the task.\n",
            e(name),
            e(output),
            e(url.unwrap_or_default())
        ),
    )
}

pub fn scheduled_task_failed(app: &str, name: &str, output: &str, url: Option<&str>) -> String {
    layout(
        app,
        &format!(
            "Scheduled task ({}) was FAILED with the following error:\n\n<pre>\n{}\n</pre>\n\nClick [here]({}) to view the task.\n",
            e(name),
            e(output),
            e(url.unwrap_or_default())
        ),
    )
}

pub fn server_force_disabled(app: &str, name: &str) -> String {
    layout(
        app,
        &format!(
            "    Your server ({}) disabled because it is not paid! All automations and integrations are stopped.\n\n    Please update your subscription to enable the server again [here](https://app.coolify.io/subscription).\n",
            e(name)
        ),
    )
}

pub fn server_force_enabled(app: &str, name: &str) -> String {
    layout(
        app,
        &format!("Your server ({}) is enabled again!\n", e(name)),
    )
}

pub fn server_lost_connection(app: &str, name: &str) -> String {
    layout(
        app,
        &format!(
            "Coolify cannot connect to your server ({}). Please check your server and make sure it is running.\n\nAll automations & integrations are turned off!\n\nIMPORTANT: We automatically try to revive your server and turn on all automations & integrations.\n\nIf you have any questions, please contact us.\n",
            e(name)
        ),
    )
}

pub fn server_revived(app: &str, name: &str) -> String {
    layout(
        app,
        &format!(
            "Your server ({}) was offline for a while, but it is back online now. All automations & integrations are turned on again.\n",
            e(name)
        ),
    )
}

pub fn server_patches_error(
    app: &str,
    name: &str,
    os: &str,
    manager: &str,
    error: &str,
    url: &str,
) -> String {
    layout(
        app,
        &format!(
            "Failed to check for package updates on your server {}.\n\n## Error Details\n\n- Operating System: {}\n- Package Manager: {}\n- Error: {}\n\n---\n\nYou can manage your server and view more details in your [Coolify Dashboard]({}).\n",
            e(name),
            e(&ucfirst(os)),
            e(manager),
            e(error),
            e(url)
        ),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn server_patches(
    app: &str,
    name: &str,
    total: &Value,
    updates: &[Value],
    security_updates: Option<&Value>,
    reboot_required: Option<&Value>,
    os: &str,
    manager: &str,
    url: &str,
) -> String {
    let total_text = e(&php_string(total));
    let mut s = format!(
        "{total_text} package updates are available for your server {}.\n\n## Summary\n\n- Operating System: {}\n- Package Manager: {}\n- Total Updates: {total_text}\n",
        e(name),
        e(&ucfirst(os)),
        e(manager),
    );
    if let Some(v) = security_updates.filter(|v| !v.is_null()) {
        s.push_str(&format!("- Security Updates: {}\n", e(&php_string(v))));
    }
    if value_truthy(reboot_required) {
        s.push_str("- A reboot is required to apply installed updates.\n");
    }
    s.push_str("\n## Available Updates\n\n");
    let positive = total.as_f64().is_some_and(|n| n > 0.0);
    if positive {
        let field = |u: &Value, key: &str| e(&u.get(key).map(php_string).unwrap_or_default());
        for u in updates {
            let security = if value_truthy(u.get("security")) {
                " [security]"
            } else {
                ""
            };
            let architecture = match u.get("architecture") {
                Some(v) if !v.is_null() => php_string(v),
                _ => "unknown".to_owned(),
            };
            let repository = match u.get("repository") {
                Some(v) if !v.is_null() => php_string(v),
                _ => "Unknown".to_owned(),
            };
            s.push_str(&format!(
                "\nPackage: {}{} ({}), from version {} to {} at repository {}\n",
                field(u, "package"),
                e(security),
                e(&architecture),
                field(u, "current_version"),
                field(u, "new_version"),
                e(&repository),
            ));
        }
        s.push_str("\n## Security Considerations\n\nSome of these updates may include important security patches. We recommend reviewing and applying these updates promptly.\n\n### Critical packages that may require container/server/service restarts:\n\n");
        let critical: Vec<&Value> = updates.iter().filter(|u| filled(u.get("risk"))).collect();
        if critical.is_empty() {
            s.push_str("No critical packages requiring container restarts detected.\n");
        } else {
            for p in critical {
                s.push_str(&format!(
                    "- {}: {} → {}\n",
                    field(p, "package"),
                    field(p, "current_version"),
                    field(p, "new_version"),
                ));
            }
        }
        s.push_str("\n## Next Steps\n\n1. Review the available updates\n2. Plan maintenance window if critical packages are involved\n3. Apply updates through the Coolify dashboard\n4. Monitor services after updates are applied\n");
    } else {
        s.push_str("Your server is up to date! No packages require updating at this time.\n");
    }
    s.push_str(&format!(
        "\n---\n\nYou can manage server patches in your [Coolify Dashboard]({}).\n",
        e(url)
    ));
    layout(app, &s)
}

pub fn ssl_certificate_renewed(
    app: &str,
    resources: &[SslResource],
    urls: &[(String, String)],
) -> String {
    let mut s = String::from(
        "<h2>SSL Certificates Renewed</h2>\n\n<p>SSL certificates have been renewed for the following resources:</p>\n\n<ul>\n",
    );
    for r in resources {
        s.push_str(&format!("    <li>{}</li>\n", e(&r.name)));
    }
    s.push_str("</ul>\n\n<div style=\"margin: 20px 0; padding: 15px; background-color: #fff3cd; border: 1px solid #ffeeba; border-radius: 4px;\">\n    <strong>⚠️ Action Required:</strong> These resources need to be redeployed manually for the new SSL certificates to take effect. Please do this in the next few days to ensure your database connections remain accessible.\n</div>\n\n<p>The old SSL certificates will remain valid for approximately 14 more days, as we renew certificates 14 days before their expiration.</p>\n\n");
    if !urls.is_empty() {
        s.push_str("<div style=\"margin-top: 20px;\">\n    <p>You can redeploy these resources here:</p>\n    <ul>\n");
        for (name, url) in urls {
            s.push_str(&format!(
                "            <li><a href=\"{}\">{}</a></li>\n",
                e(url),
                e(name)
            ));
        }
        s.push_str("        </ul>\n</div>\n");
    }
    // The blade file has a space and a new line after `</x-emails.layout>`.
    format!("{} \n", layout(app, &s))
}

pub fn test_email(app: &str) -> String {
    layout(
        app,
        "If you are seeing this, it means that your Email settings are correct.\n",
    )
}

fn with_v(version: &str) -> String {
    if version.starts_with('v') {
        version.to_owned()
    } else {
        format!("v{version}")
    }
}

pub fn traefik_version_outdated(
    app: &str,
    count: usize,
    servers: &[(TraefikServer, String)],
) -> String {
    let mut s = format!(
        "{count} server(s) are running outdated Traefik proxy. Update recommended for security and features.\n\n## Affected Servers\n\n"
    );
    let mut has_upgrades = false;
    for (server, url) in servers {
        let current = with_v(server.current.as_deref().unwrap_or("unknown"));
        let latest = with_v(server.latest.as_deref().unwrap_or("unknown"));
        let is_patch = server.update_type.as_deref().unwrap_or("patch_update") == "patch_update";
        let newer = server.newer_branch_target.as_deref();
        if !is_patch || newer.is_some() {
            has_upgrades = true;
        }
        let target = match server.upgrade_target.as_deref() {
            Some(t) if !is_patch && truthy(Some(t)) => with_v(t),
            _ => latest.clone(),
        };
        let head = format!("- [**{}**]({}): {}", e(&server.name), e(url), e(&current));
        if is_patch && newer.is_some() {
            let newer_latest = with_v(server.newer_branch_latest.as_deref().unwrap_or("unknown"));
            s.push_str(&format!(
                "{head} → {} (patch update available) | Also available: {} (latest patch: {}) - new minor version\n",
                e(&target),
                e(newer.unwrap_or_default()),
                e(&newer_latest),
            ));
        } else if is_patch {
            s.push_str(&format!(
                "{head} → {} (patch update available)\n",
                e(&target)
            ));
        } else {
            s.push_str(&format!(
                "{head} (latest patch: {}) → {} (new minor version available)\n",
                e(&latest),
                e(&target),
            ));
        }
    }
    s.push_str("\n## Recommendation\n\nIt is recommended to test the new Traefik version before switching it in production environments. You can update your proxy configuration by clicking on any server name above.\n\n");
    if has_upgrades {
        s.push_str("**Important for minor version upgrades:** Before upgrading to a new minor version, please read the [Traefik changelog](https://github.com/traefik/traefik/releases) to understand breaking changes and new features.\n");
    }
    s.push_str("\n## Next Steps\n\n1. Review the [Traefik release notes](https://github.com/traefik/traefik/releases) for changes\n2. Test the new version in a non-production environment\n3. Update your proxy configuration when ready ([how to update Traefik](https://coolify.io/docs/core/networking/proxy/traefik/update))\n4. Monitor services after the update\n\n---\n\nClick on any server name above to manage its proxy settings.\n");
    layout(app, &s)
}

pub fn volume_backup_missing(
    app: &str,
    summary: &str,
    last: Option<&str>,
    url: Option<&str>,
) -> String {
    let mut s = format!("{}\n\n", e(summary));
    if truthy(last) {
        s.push_str(&format!(
            "The last execution was at {}.\n",
            e(last.unwrap_or_default())
        ));
    } else {
        s.push_str("This schedule has never produced an execution.\n");
    }
    s.push('\n');
    if truthy(url) {
        s.push_str(&format!(
            "Click [here]({}) to open the resource.\n",
            e(url.unwrap_or_default())
        ));
    }
    layout(app, &s)
}

pub fn volume_backup_recovery_failed(app: &str, summary: &str, url: Option<&str>) -> String {
    let mut s = format!("{}\n\n", e(summary));
    if truthy(url) {
        s.push_str(&format!(
            "Click [here]({}) to open the resource.\n",
            e(url.unwrap_or_default())
        ));
    }
    layout(app, &s)
}

pub fn volume_backup_result(
    app: &str,
    summary: &str,
    frequency: &str,
    output: Option<&str>,
    url: Option<&str>,
) -> String {
    let mut s = format!("{}\n\nFrequency: {}\n\n", e(summary), e(frequency));
    if let Some(o) = output {
        s.push_str(&format!("### Reason\n\n{}\n", e(o)));
    }
    s.push('\n');
    if truthy(url) {
        s.push_str(&format!(
            "Click [here]({}) to open the resource.\n",
            e(url.unwrap_or_default())
        ));
    }
    layout(app, &s)
}

pub fn api_token_expiring(app: &str, token: &str, expires: &str, url: &str) -> String {
    layout(
        app,
        &format!(
            "Your Coolify API token ({}) expires on {}.\n\nRotate this token before it expires. API calls using this token will start failing once the expiration time is reached.\n\nManage your API tokens [here]({}).\n",
            e(token),
            e(expires),
            e(url)
        ),
    )
}
