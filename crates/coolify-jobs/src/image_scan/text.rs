//! `App\Services\ImageScan\ScannerText`: cleaning of text that comes out of the scanners, and
//! the short title of a finding.

use std::sync::LazyLock;

use coolify_remote::text::php_trim;
use regex::Regex;
use serde_json::Value;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|_| unreachable!())
}

fn fancy(pattern: &str) -> fancy_regex::Regex {
    fancy_regex::Regex::new(pattern).unwrap_or_else(|_| unreachable!())
}

static CONTROL: LazyLock<Regex> = LazyLock::new(|| re(r"[\p{Cc}\p{Cf}]"));
static SPACES: LazyLock<Regex> = LazyLock::new(|| re(r"\s+"));
static CHANGELOG_START: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:\[[^\]]*\]\s*[-:]|-)\s+"));
static CHANGELOG_TOKENS: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(?:(?:\[[^\]]*\]\s*[-:]|-)\s+)+"));
static ORABUG: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\s*\[Orabug:[^\]]*\]"));
static TRAILING_CVES: LazyLock<fancy_regex::Regex> =
    LazyLock::new(|| fancy(r"(?i)(?:\s*\[CVE-\d{4}-\d+\])+(?=\s*[.…]?\s*$)"));
static FIRST_ENTRY: LazyLock<Regex> = LazyLock::new(|| re(r"^(.+?)\s+-\s+\S"));
static TITLE_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^(?:(?:issue\s+summary|summary|description|vulnerability)\s*:\s*)+"));
static MARKDOWN_LINK: LazyLock<Regex> = LazyLock::new(|| re(r"!?\[([^\]]*)\]\([^)\s]*\)"));
static URL_IN_PARENTHESES: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\(\s*(?:https?|ftp)://[^\s()]*\s*\)"));
static URL: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(?:https?|ftp)://\S+|\bwww\.\S+"));
static BOLD_OR_STRIKE: LazyLock<fancy_regex::Regex> =
    LazyLock::new(|| fancy(r"(?<!\w)(\*\*|~~)(?=\S)(.+?)(?<=\S)\1(?!\w)"));
static UNDERSCORE_BOLD: LazyLock<fancy_regex::Regex> =
    LazyLock::new(|| fancy(r"(?<!\w)__(?=\S)([^_]*?\s[^_]*?)(?<=\S)__(?!\w)"));
static CODE: LazyLock<fancy_regex::Regex> = LazyLock::new(|| fancy(r"`(?=\S)([^`]*?)(?<=\S)`"));
static HEADING_OR_QUOTE: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:#{1,6}|>)\s+"));

/// `mb_substr($text, 0, $max)`.
fn mb_prefix(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// PHP's `(string)` of a float that came out of `json_decode`.
fn php_float_string(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.0}")
    } else {
        f.to_string()
    }
}

/// `ScannerText::clean($value, $max)`: strips control and format characters (including ANSI
/// escape starts and bidi overrides) and cuts to `$max` characters. Numbers are written as text;
/// any other type is the empty string.
pub fn clean(value: Option<&Value>, max: usize) -> String {
    match value {
        Some(Value::String(s)) => clean_str(s, max),
        Some(Value::Number(n)) => {
            let text = match (n.as_i64(), n.as_u64(), n.as_f64()) {
                (Some(i), _, _) => i.to_string(),
                (_, Some(u), _) => u.to_string(),
                (_, _, Some(f)) => php_float_string(f),
                _ => String::new(),
            };
            clean_str(&text, max)
        }
        _ => String::new(),
    }
}

/// [`clean`] of a string.
pub fn clean_str(value: &str, max: usize) -> String {
    mb_prefix(&CONTROL.replace_all(value, ""), max)
}

/// PHP truthiness of a string (`?:`): empty and `"0"` are false.
fn truthy(text: &str) -> bool {
    !text.is_empty() && text != "0"
}

/// `ScannerText::title()`: a short single-line title for a finding: the first sentence of the
/// description, else of the related description, without Markdown, URLs, or invisible
/// characters. A description that is a package changelog (Oracle Linux and similar) yields to
/// the related description.
pub fn title(
    description: Option<&str>,
    related_description: Option<&str>,
    package: &str,
    version: &str,
    severity: &str,
) -> String {
    let own = if looks_like_changelog(description) {
        String::new()
    } else {
        sentence(description)
    };
    let text = if truthy(&own) {
        own
    } else {
        let related = sentence(related_description);
        if truthy(&related) {
            related
        } else {
            sentence(description)
        }
    };
    if text.split_whitespace().count() < 4 {
        return fallback_title(package, version, severity);
    }
    text
}

/// `ScannerText::looksLikeChangelog()`: a description that is a package changelog entry rather
/// than a text about the vulnerability.
pub fn looks_like_changelog(description: Option<&str>) -> bool {
    let text = description
        .unwrap_or_default()
        .trim_start_matches([' ', '\t', '\n', '\r', '\0', '\x0B']);
    text.starts_with('[')
        || text.starts_with("- ")
        || text.contains("Orabug")
        || text.contains("Resolves:")
}

/// `ScannerText::tidyTitle()`: cleans a title, also one stored before these rules: leading
/// changelog tokens ("[1.0.9-10.0.1] - ", "[3.0.7]: ", "- "), "[Orabug: …]" groups, and trailing
/// "[CVE-…]" references. A changelog keeps its first entry only. Then the label of
/// [`strip_title_prefix`].
pub fn tidy_title(text: &str) -> String {
    let text = php_trim(text);
    let changelog = CHANGELOG_START.is_match(text);
    let text = CHANGELOG_TOKENS.replace(text, "");
    let text = ORABUG.replace_all(&text, "");
    let text = TRAILING_CVES.replace_all(&text, "");
    let collapsed = SPACES.replace_all(&text, " ");
    let mut text = php_trim(&collapsed).to_owned();
    if changelog && let Some(first) = FIRST_ENTRY.captures(&text).and_then(|c| c.get(1)) {
        text = first.as_str().to_owned();
    }
    strip_title_prefix(&text)
}

/// `ScannerText::storedTitle()`: a stored title for display: the fallback title while the stored
/// one is a package changelog entry (until a rescan parses it again), else [`tidy_title`].
pub fn stored_title(title: &str, package: &str, version: &str, severity: &str) -> String {
    let title = php_trim(title);
    if title.is_empty() || looks_like_changelog(Some(title)) {
        return fallback_title(package, version, severity);
    }
    let tidy = tidy_title(title);
    if tidy.is_empty() {
        fallback_title(package, version, severity)
    } else {
        tidy
    }
}

/// `ScannerText::stripTitlePrefix()`: removes the label some advisories start with ("Issue
/// summary:", "Summary:", "Description:", "Vulnerability:"), also from titles stored before the
/// parser removed it.
pub fn strip_title_prefix(text: &str) -> String {
    let stripped = TITLE_PREFIX.replace(text, "");
    if stripped == text {
        return text.to_owned();
    }
    let mut chars = stripped.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `ScannerText::fallbackTitle()`.
pub fn fallback_title(package: &str, version: &str, severity: &str) -> String {
    let joined = format!("{package} {version}");
    let cleaned = clean_str(php_trim(&joined), 80);
    let collapsed = SPACES.replace_all(&cleaned, " ");
    let prefix = php_trim(&collapsed);
    let cleaned_label = clean_str(severity, 16);
    let label = match php_trim(&cleaned_label) {
        "" => "unknown",
        label => label,
    };
    if prefix.is_empty() {
        format!("{label} vulnerability")
    } else {
        format!("{prefix}: {label} vulnerability")
    }
}

/// The end of the first sentence: a `.`, `!` or `?` that does not end "e.g" or "i.e" and is
/// followed by whitespace and an uppercase letter
/// (`/^(.*?(?i:(?<!\be\.g)(?<!\bi\.e))[.!?])\s+\p{Lu}/su`). Returns the byte length to keep.
fn first_sentence_end(text: &str) -> Option<usize> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    for (i, &(offset, c)) in chars.iter().enumerate() {
        if !matches!(c, '.' | '!' | '?') {
            continue;
        }
        // "e.g" or "i.e" right before, starting at a word boundary.
        let abbreviation = i >= 3 && {
            let a = chars[i - 3].1.to_lowercase().to_string();
            let b = chars[i - 2].1;
            let c3 = chars[i - 1].1.to_lowercase().to_string();
            b == '.'
                && ((a == "e" && c3 == "g") || (a == "i" && c3 == "e"))
                && (i == 3 || !is_word(chars[i - 4].1))
        };
        if abbreviation {
            continue;
        }
        let mut next = i + 1;
        while next < chars.len() && chars[next].1.is_whitespace() {
            next += 1;
        }
        if next > i + 1 && next < chars.len() && chars[next].1.is_uppercase() {
            return Some(offset + c.len_utf8());
        }
    }
    None
}

/// `ScannerText::sentence()`.
fn sentence(text: Option<&str>) -> String {
    let Some(text) = text else {
        return String::new();
    };
    let text = SPACES.replace_all(text, " ");
    let text = clean_str(&text, 2000);
    let text = MARKDOWN_LINK.replace_all(&text, "$1");
    let text = URL_IN_PARENTHESES.replace_all(&text, "");
    // A URL goes away; the punctuation that ends it stays.
    let text = URL.replace_all(&text, |c: &regex::Captures<'_>| {
        let url = c.get(0).map_or("", |m| m.as_str());
        url[url.trim_end_matches(['.', ',', ';', ':', ')']).len()..].to_owned()
    });
    let text = BOLD_OR_STRIKE.replace_all(&text, "$2");
    let text = UNDERSCORE_BOLD.replace_all(&text, "$1");
    let text = CODE.replace_all(&text, "$1");
    let text = HEADING_OR_QUOTE.replace(&text, "");
    let collapsed = SPACES.replace_all(&text, " ");
    let mut text = tidy_title(php_trim(&collapsed));
    if let Some(end) = first_sentence_end(&text) {
        text.truncate(end);
    }
    if text.chars().count() <= 120 {
        return text;
    }
    let mut cut: String = mb_prefix(&text, 119);
    if let Some(space) = cut.rfind(' ')
        && space > 0
    {
        cut.truncate(space);
    }
    format!(
        "{}…",
        cut.trim_end_matches([' ', '\t', ',', ';', ':', '.', '-'])
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn clean_strips_invisible_characters_and_cuts() {
        assert_eq!(
            clean(Some(&json!("a\u{1b}[31mb\u{202e}c\n")), 10),
            "a[31mbc"
        );
        assert_eq!(clean(Some(&json!("abcdef")), 3), "abc");
        assert_eq!(clean(Some(&json!("äöü")), 2), "äö");
        assert_eq!(clean(Some(&json!(12)), 10), "12");
        assert_eq!(clean(Some(&json!(1.5)), 10), "1.5");
        assert_eq!(clean(Some(&json!(2.0)), 10), "2");
        assert_eq!(clean(Some(&json!(true)), 10), "");
        assert_eq!(clean(Some(&json!(["a"])), 10), "");
        assert_eq!(clean(None, 10), "");
    }

    #[test]
    fn title_is_the_first_sentence() {
        assert_eq!(
            title(
                Some("A flaw was found in the parser. An attacker can crash it."),
                None,
                "libfoo",
                "1.0",
                "high"
            ),
            "A flaw was found in the parser."
        );
        // "e.g." does not end a sentence.
        assert_eq!(
            title(
                Some("Some inputs, e.g. Large ones, crash the parser. More text."),
                None,
                "libfoo",
                "1.0",
                "high"
            ),
            "Some inputs, e.g. Large ones, crash the parser."
        );
    }

    #[test]
    fn title_falls_back_for_short_or_missing_text() {
        assert_eq!(
            title(None, None, "libfoo", "1.0", "high"),
            "libfoo 1.0: high vulnerability"
        );
        assert_eq!(
            title(Some("Too short."), None, "libfoo", "1.0", "medium"),
            "libfoo 1.0: medium vulnerability"
        );
        assert_eq!(title(None, None, "", "", ""), "unknown vulnerability");
    }

    #[test]
    fn title_removes_markdown_and_urls() {
        assert_eq!(
            title(
                Some(
                    "## The **parser** in `libfoo` reads [past the end](https://example.com/a) of a buffer (https://example.com/b). See https://example.com/c."
                ),
                None,
                "libfoo",
                "1.0",
                "high"
            ),
            "The parser in libfoo reads past the end of a buffer ."
        );
        assert_eq!(
            title(
                Some("the __very long__ name of www.example.com, overflows"),
                None,
                "libfoo",
                "1.0",
                "high"
            ),
            "the very long name of , overflows"
        );
    }

    #[test]
    fn changelog_descriptions_yield_to_the_related_description() {
        let changelog = "[1.0.9-10.0.1] - Fix overflow in the parser [Orabug: 123] - Other entry";
        assert!(looks_like_changelog(Some(changelog)));
        assert_eq!(
            title(
                Some(changelog),
                Some("The parser of libfoo overflows a buffer."),
                "libfoo",
                "1.0",
                "high"
            ),
            "The parser of libfoo overflows a buffer."
        );
        // Without a related description the changelog keeps its first entry.
        assert_eq!(
            title(Some(changelog), None, "libfoo", "1.0", "high"),
            "Fix overflow in the parser"
        );
    }

    #[test]
    fn tidy_title_removes_changelog_tokens_and_references() {
        assert_eq!(
            tidy_title("[3.0.7]: Fix a crash in the resolver [CVE-2024-1234]"),
            "Fix a crash in the resolver"
        );
        assert_eq!(
            tidy_title("- Fix a crash [CVE-2024-1234] [CVE-2024-99999]."),
            "Fix a crash."
        );
        assert_eq!(
            tidy_title("Issue summary: a crash in the resolver"),
            "A crash in the resolver"
        );
        assert_eq!(tidy_title("  Plain   title  "), "Plain title");
    }

    #[test]
    fn stored_title_falls_back_for_changelogs() {
        assert_eq!(
            stored_title("", "libfoo", "1.0", "low"),
            "libfoo 1.0: low vulnerability"
        );
        assert_eq!(
            stored_title("[1.0] - Resolves: rhbz#1", "libfoo", "1.0", "low"),
            "libfoo 1.0: low vulnerability"
        );
        assert_eq!(
            stored_title("Summary: buffer overflow", "libfoo", "1.0", "low"),
            "Buffer overflow"
        );
        assert_eq!(
            stored_title("Buffer overflow", "libfoo", "1.0", "low"),
            "Buffer overflow"
        );
    }

    #[test]
    fn long_sentences_are_cut_at_a_word() {
        let long = format!("{} end", "word ".repeat(40));
        let cut = title(Some(&long), None, "p", "1", "low");
        assert!(cut.ends_with('…'));
        assert!(cut.chars().count() <= 120);
        assert!(cut.starts_with("word word"));
        assert!(!cut.contains(" …"));
    }
}
