//! Hand port of `resources/views/emails/image-vulnerabilities.blade.php`, the email of both image
//! security notifications.
//!
//! [`image_vulnerabilities`] builds what Blade's output buffer holds inside `<x-emails.layout>`
//! (the slot): `{{ }}` escaped with `e()`, a directive line gone. The layout then trims the slot,
//! parses it as Markdown and wraps it in the header and footer (`notifications::views::layout`).
//! The values are escaped for Markdown by the notifications (`NotificationText::mailMarkdown()`).

use crate::notifications::php::e;

/// One resource of the email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailResource {
    pub name: String,
    pub url: Option<String>,
    pub summary: String,
    /// The rows of the why-list (`NotificationText::mailWhy()`).
    pub why: Vec<String>,
}

/// One ignore that ends soon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailIgnore {
    pub subject: String,
    /// `None`: all resources.
    pub resource: Option<String>,
    pub expires_at: String,
}

/// One server whose scans failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailServer {
    pub name: String,
    pub url: String,
}

/// A PHP string in a boolean context: `null`, `""` and `"0"` are false.
fn truthy(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// The slot of `emails.image-vulnerabilities`.
pub fn image_vulnerabilities(
    intro: &str,
    resources: &[MailResource],
    ignores: &[MailIgnore],
    servers: &[MailServer],
    url: &str,
) -> String {
    let mut out = format!("\n{}\n", e(intro));
    if !resources.is_empty() {
        out.push_str("\n## Resources\n\n");
        for resource in resources {
            match resource.url.as_deref().filter(|u| truthy(Some(u))) {
                Some(link) => {
                    out.push_str(&format!("### [{}]({})\n", e(&resource.name), e(link)));
                }
                None => out.push_str(&format!("### {}\n", e(&resource.name))),
            }
            out.push('\n');
            if !resource.summary.is_empty() {
                out.push_str(&format!("{}\n\n", e(&resource.summary)));
            }
            for why in &resource.why {
                out.push_str(&format!("- {}\n", e(why)));
            }
            out.push('\n');
        }
    }
    if !ignores.is_empty() {
        out.push_str("## Ignores that end within 7 days\n\n");
        for ignore in ignores {
            let on = match ignore.resource.as_deref().filter(|r| truthy(Some(r))) {
                Some(resource) => format!(" on {resource}"),
                None => " on all resources".to_owned(),
            };
            out.push_str(&format!(
                "- {}{}, ends on {}\n",
                e(&ignore.subject),
                e(&on),
                e(&ignore.expires_at)
            ));
        }
        out.push('\n');
    }
    if !servers.is_empty() {
        out.push_str("## Servers whose scans failed for 2 days\n\n");
        for server in servers {
            out.push_str(&format!("- [{}]({})\n", e(&server.name), e(&server.url)));
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "---\n\nSee all results on the [Security page]({}).\n",
        e(url)
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_every_section() {
        let slot = image_vulnerabilities(
            "Changes in the security status of your resources since the last digest.",
            &[
                MailResource {
                    name: "api".to_owned(),
                    url: Some(
                        "https://c.test/project/p/environment/e/application/a/vulnerabilities"
                            .to_owned(),
                    ),
                    summary: "Security update\\. Update to 1\\.28\\.".to_owned(),
                    why: vec![
                        "A bug (CVE\\-2026\\-0001, High).".to_owned(),
                        "Another <b>bug</b>".to_owned(),
                    ],
                },
                MailResource {
                    name: "web".to_owned(),
                    url: None,
                    summary: String::new(),
                    why: Vec::new(),
                },
            ],
            &[
                MailIgnore {
                    subject: "CVE\\-2026\\-0300".to_owned(),
                    resource: None,
                    expires_at: "2026-10-30".to_owned(),
                },
                MailIgnore {
                    subject: "Package openssl".to_owned(),
                    resource: Some("api & web".to_owned()),
                    expires_at: "2026-10-31".to_owned(),
                },
            ],
            &[MailServer {
                name: "server".to_owned(),
                url: "https://c.test/server/s".to_owned(),
            }],
            "https://c.test/vulnerabilities",
        );
        assert_eq!(
            slot,
            "\nChanges in the security status of your resources since the last digest.\n\
             \n## Resources\n\n\
             ### [api](https://c.test/project/p/environment/e/application/a/vulnerabilities)\n\n\
             Security update\\. Update to 1\\.28\\.\n\n\
             - A bug (CVE\\-2026\\-0001, High).\n\
             - Another &lt;b&gt;bug&lt;/b&gt;\n\n\
             ### web\n\n\n\
             ## Ignores that end within 7 days\n\n\
             - CVE\\-2026\\-0300 on all resources, ends on 2026-10-30\n\
             - Package openssl on api &amp; web, ends on 2026-10-31\n\n\
             ## Servers whose scans failed for 2 days\n\n\
             - [server](https://c.test/server/s)\n\n\
             ---\n\nSee all results on the [Security page](https://c.test/vulnerabilities).\n"
        );
    }

    #[test]
    fn leaves_out_empty_sections() {
        assert_eq!(
            image_vulnerabilities("Intro.", &[], &[], &[], "https://c.test/vulnerabilities"),
            "\nIntro.\n---\n\nSee all results on the [Security page](https://c.test/vulnerabilities).\n"
        );
    }
}
