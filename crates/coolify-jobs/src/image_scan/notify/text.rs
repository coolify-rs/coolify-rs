//! `App\Notifications\ImageScan\NotificationText`: escapes values for each notification channel.
//! Names, image refs, and tags come from users and registries.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::super::resource_scans;

static CONTROL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\p{Cc}\p{Cf}]+").unwrap_or_else(|_| unreachable!()));

/// `SecurityRules::EXPLOITED` and `SecurityRules::LIKELY` as they are stored in a why-entry.
const SIGNAL_EXPLOITED: &str = "exploited";
const SIGNAL_LIKELY: &str = "likely";

/// One row of a why-list (`SecurityStatus::whyFor()`), as the notifications keep it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhyEntry {
    pub title: String,
    pub display_id: String,
    pub link: Option<String>,
    pub score: Option<f64>,
    pub severity: String,
    /// `exploited`, `likely`, or none.
    pub signal: Option<String>,
}

/// `mb_strlen($value)`.
pub fn mb_strlen(value: &str) -> usize {
    value.chars().count()
}

/// `trim()` with PHP's default character list.
fn php_trim(value: &str) -> &str {
    value.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B'])
}

/// `NotificationText::plain($value)`: control and format characters become one space; trimmed.
pub fn plain(value: &str) -> String {
    php_trim(&CONTROL.replace_all(value, " ")).to_owned()
}

fn escape_with_backslash(value: &str, special: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if special.contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `NotificationText::discord($value)`: Discord markdown.
pub fn discord(value: &str) -> String {
    escape_with_backslash(&plain(value), "\\*_~`|>[]()#")
}

/// `NotificationText::slack($value)`: Slack mrkdwn: only &, <, and > have a meaning that escaping
/// can stop.
pub fn slack(value: &str) -> String {
    plain(value)
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `NotificationText::html($value)`: Pushover with `html=1`
/// (`htmlspecialchars($value, ENT_QUOTES | ENT_HTML5, 'UTF-8')`).
pub fn html(value: &str) -> String {
    let value = plain(value);
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// `NotificationText::mailMarkdown($value)`: Markdown of the email body; Blade escapes the HTML.
pub fn mail_markdown(value: &str) -> String {
    escape_with_backslash(&plain(value), "\\`*_{}[]()#+-.!|~=")
}

/// `ucfirst()`: only an ASCII first byte changes.
pub(super) fn ucfirst(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {
            let mut out = c.to_ascii_uppercase().to_string();
            out.push_str(chars.as_str());
            out
        }
        _ => value.to_owned(),
    }
}

/// `number_format($score, 1)` of a CVSS score: one decimal, halves rounded away from zero.
fn score_text(score: f64) -> String {
    format!("{:.1}", (score * 10.0).round() / 10.0)
}

/// `NotificationText::mailWhy($entry)`: one row of a why-list in the email, as escaped Markdown:
/// "Title ([CVE-2025-1094](https://nvd…), 8.1 High). Used by attackers." The ID links only to the
/// advisory Coolify builds for it.
pub fn mail_why(entry: &WhyEntry) -> String {
    let id = entry.display_id.as_str();
    let reference = match resource_scans::advisory_url(id) {
        Some(link) => format!("[{}]({link})", mail_markdown(id)),
        None => mail_markdown(id),
    };
    let severity = ucfirst(&plain(&entry.severity).to_ascii_lowercase());
    let score = match entry.score {
        Some(score) => format!("{} {severity}", score_text(score)),
        None => severity,
    };
    let label = match entry.signal.as_deref() {
        Some(SIGNAL_EXPLOITED) => " Used by attackers.",
        Some(SIGNAL_LIKELY) => " Likely to be exploited.",
        _ => "",
    };
    format!(
        "{} ({reference}, {}).{label}",
        mail_markdown(&limit(&entry.title, 200)),
        mail_markdown(&score)
    )
}

/// `NotificationText::limit($value, $max)`: cuts a raw value before it is escaped, so one long
/// name cannot fill a message.
pub fn limit(value: &str, max: usize) -> String {
    let value = plain(value);
    if mb_strlen(&value) <= max {
        return value;
    }
    let cut: String = value.chars().take(max.saturating_sub(1)).collect();
    format!(
        "{}…",
        cut.trim_end_matches([' ', '\t', '\n', '\r', '\0', '\x0B'])
    )
}

/// `NotificationText::fitLines($lines, $budget)`: the lines that fit in `budget` characters,
/// joined with new lines, then "And N more." for the rest.
pub fn fit_lines(lines: &[String], budget: usize) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0;
    let count = lines.len();
    for (index, line) in lines.iter().enumerate() {
        let remaining = count - index - 1;
        let reserve = if remaining > 0 {
            mb_strlen(&format!("\nAnd {remaining} more."))
        } else {
            0
        };
        let length = mb_strlen(line) + usize::from(!kept.is_empty());
        if used + length + reserve > budget {
            break;
        }
        kept.push(line.clone());
        used += length;
    }
    let more = count - kept.len();
    if more > 0 {
        kept.push(format!("And {more} more."));
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn why(title: &str, id: &str, score: Option<f64>, signal: Option<&str>) -> WhyEntry {
        WhyEntry {
            title: title.to_owned(),
            display_id: id.to_owned(),
            link: resource_scans::advisory_url(id),
            score,
            severity: "HIGH".to_owned(),
            signal: signal.map(str::to_owned),
        }
    }

    #[test]
    fn plain_replaces_control_and_format_characters() {
        assert_eq!(plain("  api\r\n\tweb  "), "api web");
        // ANSI escape starts, zero-width spaces and bidi overrides.
        assert_eq!(plain("a\u{1b}[31mb"), "a [31mb");
        assert_eq!(plain("a\u{200b}\u{202e}b"), "a b");
        assert_eq!(plain("\u{0}\u{7f}"), "");
        assert_eq!(plain("näme ✓"), "näme ✓");
    }

    #[test]
    fn escapes_per_channel() {
        let name = "*bold* & [link](https://evil.example)";
        assert_eq!(
            discord(name),
            "\\*bold\\* & \\[link\\]\\(https://evil.example\\)"
        );
        assert_eq!(
            discord("a\\b_c~d`e|f>g#h"),
            "a\\\\b\\_c\\~d\\`e\\|f\\>g\\#h"
        );
        assert_eq!(slack(name), "*bold* &amp; [link](https://evil.example)");
        assert_eq!(slack("<@U1> &amp;"), "&lt;@U1&gt; &amp;amp;");
        assert_eq!(html(name), "*bold* &amp; [link](https://evil.example)");
        assert_eq!(
            html("<b>'x' \"y\"</b>"),
            "&lt;b&gt;&apos;x&apos; &quot;y&quot;&lt;/b&gt;"
        );
        assert_eq!(
            mail_markdown(name),
            "\\*bold\\* & \\[link\\]\\(https://evil\\.example\\)"
        );
        assert_eq!(
            mail_markdown("a-b+c.d!e=f{g}h|i~j"),
            "a\\-b\\+c\\.d\\!e\\=f\\{g\\}h\\|i\\~j"
        );
    }

    #[test]
    fn limit_cuts_before_escaping() {
        assert_eq!(limit("short", 10), "short");
        assert_eq!(limit("abcdefghij", 10), "abcdefghij");
        assert_eq!(limit("abcdefghijk", 10), "abcdefghi…");
        // The cut never ends with a space.
        assert_eq!(limit("abcd efghijk", 6), "abcd…");
        assert_eq!(mb_strlen(&limit(&"ä".repeat(300), 100)), 100);
        assert_eq!(limit("  a\nb  ", 10), "a b");
    }

    #[test]
    fn fit_lines_keeps_what_fits_and_counts_the_rest() {
        let lines: Vec<String> = ["aaaa", "bbbb", "cccc"].map(str::to_owned).to_vec();
        assert_eq!(fit_lines(&lines, 100), lines);
        // "aaaa" + "\nAnd 2 more." is 16 characters.
        assert_eq!(fit_lines(&lines, 16), vec!["aaaa", "And 2 more."]);
        assert_eq!(fit_lines(&lines, 15), vec!["And 3 more."]);
        // The room for "And N more." is kept while lines are left, so 14 characters are not
        // enough for the three lines (14 characters).
        assert_eq!(fit_lines(&lines, 14), vec!["And 3 more."]);
        assert_eq!(fit_lines(&lines, 21), lines);
        // "aaaa\nbbbb" + "\nAnd 1 more." is 21 characters.
        let long: Vec<String> = vec!["aaaa".to_owned(), "bbbb".to_owned(), "c".repeat(20)];
        assert_eq!(fit_lines(&long, 21), vec!["aaaa", "bbbb", "And 1 more."]);
        assert_eq!(fit_lines(&lines[..1], 4), vec!["aaaa"]);
        assert!(fit_lines(&[], 10).is_empty());
    }

    #[test]
    fn mail_why_links_the_id_and_escapes_the_title() {
        let entry = why(
            "*bold* <b>x</b> [link](https://evil.example)",
            "CVE-2026-0001",
            Some(8.05),
            Some("exploited"),
        );
        assert_eq!(
            mail_why(&entry),
            "\\*bold\\* <b>x</b> \\[link\\]\\(https://evil\\.example\\) \
             ([CVE\\-2026\\-0001](https://nvd.nist.gov/vuln/detail/CVE-2026-0001), 8\\.1 High). Used by attackers."
        );
        let likely = why("A bug", "GHSA-aaaa-bbbb-cccc", None, Some("likely"));
        assert_eq!(
            mail_why(&likely),
            "A bug ([GHSA\\-aaaa\\-bbbb\\-cccc](https://github.com/advisories/GHSA-aaaa-bbbb-cccc), High). Likely to be exploited."
        );
        // An ID that Coolify builds no advisory link for is text only.
        let other = why("A bug", "ALAS-2026-1", Some(7.0), None);
        assert_eq!(mail_why(&other), "A bug (ALAS\\-2026\\-1, 7\\.0 High).");
        assert_eq!(score_text(8.25), "8.3");
        assert_eq!(score_text(10.0), "10.0");
    }

    #[test]
    fn mail_why_cuts_a_long_title() {
        let entry = why(&"t".repeat(300), "CVE-2026-0001", None, None);
        let row = mail_why(&entry);
        assert!(row.starts_with(&format!("{}… (", "t".repeat(199))));
    }
}
