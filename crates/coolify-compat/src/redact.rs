//! Port of upstream `App\Services\Security\SensitiveDataRedactor::redactText`
//! (the `remove_iip()` helper), which runs before logs and command output are
//! stored.
//!
//! PCRE semantics without the `u` flag are kept by spelling `\w` and `\s` as
//! ASCII classes. When a pattern exceeds the backtracking limit the chunk is
//! redacted line by line, and a line that still fails is suppressed, as
//! upstream does on a PCRE limit (the limits themselves differ).

use std::sync::LazyLock;

use fancy_regex::{Captures, Regex};

/// Upstream `REDACTED`.
pub const REDACTED: &str = "<REDACTED>";
pub const FAILURE_MESSAGE: &str = "[LOG SUPPRESSED: REDACTION FAILED]";

const CHUNK_BYTES: usize = 262_144;
const MIN_KNOWN_SECRET_LENGTH: usize = 8;

const SENSITIVE_KEYS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "client_secret",
    "api_key",
    "apikey",
    "access_key",
    "private_key",
    "token",
    "access_token",
    "refresh_token",
    "auth_token",
    "authorization",
    "proxy_authorization",
    "cookie",
    "set_cookie",
    "webhook_secret",
    "signing_secret",
    "credential",
    "credentials",
    "connection_string",
    "database_url",
];

/// PCRE `\s` (no `u` flag).
const S: &str = r"[\t\n\x0B\x0C\r ]";

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap_or_else(|e| unreachable!("static pattern {p}: {e}"))
}

static ANSI: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\x1B(?:[@-Z\\\-_]|\[[0-?]*[ -/]*[@-~])").unwrap_or_else(|_| unreachable!())
});
static CONTROL: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]").unwrap_or_else(|_| unreachable!())
});
static BEGIN_KEY: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").unwrap_or_else(|_| unreachable!())
});
static END_KEY: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"-----END [A-Z ]*PRIVATE KEY-----").unwrap_or_else(|_| unreachable!())
});

const EMAIL: &str = r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}";

static PATTERNS: LazyLock<Vec<(Regex, String)>> = LazyLock::new(|| {
    let r = REDACTED;
    [
        (format!(r"(?i)((?:Authorization|Proxy-Authorization){S}*:{S}*)[^\r\n]+"), format!("${{1}}{r}")),
        (r"x-access-token:.*?(?=@)".to_owned(), format!("x-access-token:{r}")),
        (r"oauth2:.*?(?=@)".to_owned(), format!("oauth2:{r}")),
        (
            r"(?i)((?:https?|postgres|mysql|mongodb|rediss?|mariadb|ftp|sftp|ssh|amqp|amqps|ldap|ldaps|s3)://[^:\t\n\x0B\x0C\r @/]+:)[^@\t\n\x0B\x0C\r ]+(@)"
                .to_owned(),
            format!("${{1}}{r}${{2}}"),
        ),
        (EMAIL.to_owned(), r.to_owned()),
        (r"(?i)Bearer[\t\n\x0B\x0C\r ]+[A-Za-z0-9\-_]+\.[A-Za-z0-9\-_]+\.[A-Za-z0-9\-_]+".to_owned(), format!("Bearer {r}")),
        (r"(?i)Bearer[\t\n\x0B\x0C\r ]+[^\t\n\x0B\x0C\r ,;]+".to_owned(), format!("Bearer {r}")),
        (r"\bgh[pousr]_[A-Za-z0-9.\-_]{36,}(?![A-Za-z0-9.\-_])".to_owned(), r.to_owned()),
        (r"\b(gl(?:pat|cbt|rt)-[A-Za-z0-9\-_]{20,})\b".to_owned(), r.to_owned()),
        (r"\b(A(?:KIA|BIA|CCA|SIA)[A-Z0-9]{16})\b".to_owned(), r.to_owned()),
        (
            format!(r#"(?i)(aws_secret_access_key|AWS_SECRET_ACCESS_KEY)[=:]{S}*['"]?([A-Za-z0-9/+=]{{40}})['"]?"#),
            format!("${{1}}={r}"),
        ),
        (
            format!(r#"(?i)(api[_-]?key|apikey|api[_-]?secret|secret[_-]?key)[=:]{S}*['"]?[A-Za-z0-9\-_]{{16,}}['"]?"#),
            format!("${{1}}={r}"),
        ),
        (r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----".to_owned(), r.to_owned()),
    ]
    .into_iter()
    .map(|(p, rep)| (re(&p), rep))
    .collect()
});

fn keys_pattern() -> String {
    let keys = SENSITIVE_KEYS
        .iter()
        .filter(|k| !matches!(**k, "authorization" | "proxy_authorization"))
        .map(|k| fancy_regex::escape(k).into_owned())
        .collect::<Vec<_>>()
        .join("|");
    format!(
        r"(?i:(?:[a-z0-9]+[_-])*(?:{keys}))|[A-Z][A-Z0-9_]*(?:SECRET|PASSWORD|TOKEN|PASSWD|API_KEY|PRIVATE_KEY)[A-Z0-9_]*"
    )
}

static QUOTED_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    let already = format!("(?!{})", fancy_regex::escape(REDACTED));
    re(&format!(
        r#"(?<![A-Za-z0-9_-])(["'])((?:{}){S}*={S}*){already}(?:(?!\1)[^\r\n])*\1"#,
        keys_pattern()
    ))
});

static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    let already = format!("(?!{})", fancy_regex::escape(REDACTED));
    let value =
        r#"(?P<value>"(?:[^"\\\r\n]|\\.)*"|'(?:[^'\\\r\n]|\\.)*'|[^\t\n\x0B\x0C\r ,;}\]]+)"#;
    let ends = r"(?P<end>(?=[\t\x0B\x0C ]*(?:$|[\r\n,;}\]])))?";
    re(&format!(
        r#"(?m)(?<![A-Za-z0-9_-])(?P<prefix>["']?(?P<key>{})["']?{S}*(?P<separator>[=:]){S}*){already}{value}{ends}"#,
        keys_pattern()
    ))
});

#[derive(Debug)]
struct Limit;

fn replace_all(re: &Regex, text: &str, rep: &str) -> Result<String, Limit> {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in re.captures_iter(text) {
        let caps = caps.map_err(|_| Limit)?;
        let Some(m) = caps.get(0) else { continue };
        out.push_str(&text[last..m.start()]);
        caps.expand(rep, &mut out);
        last = m.end();
    }
    out.push_str(&text[last..]);
    Ok(out)
}

fn replace_with(
    re: &Regex,
    text: &str,
    f: impl Fn(&Captures<'_>) -> String,
) -> Result<String, Limit> {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in re.captures_iter(text) {
        let caps = caps.map_err(|_| Limit)?;
        let Some(m) = caps.get(0) else { continue };
        out.push_str(&text[last..m.start()]);
        out.push_str(&f(&caps));
        last = m.end();
    }
    out.push_str(&text[last..]);
    Ok(out)
}

fn redact_patterns(text: &str) -> Result<String, Limit> {
    let mut text = text.to_owned();
    for (re, rep) in PATTERNS.iter() {
        // PCRE fails fast on a required literal; see `redact_assignments()`.
        let required: &[&str] = match re.as_str() {
            p if p.starts_with("x-access-token:") => &["x-access-token:"],
            p if p.starts_with("oauth2:") => &["oauth2:"],
            p if p.starts_with(r"\bgh") => &["ghp_", "gho_", "ghu_", "ghs_", "ghr_"],
            p if p.starts_with(r"\b(gl") => &["glpat-", "glcbt-", "glrt-"],
            p if p.starts_with(r"\b(A") => &["AKIA", "ABIA", "ACCA", "ASIA"],
            _ => &[],
        };
        if required.is_empty() || required.iter().any(|r| text.contains(r)) {
            text = replace_all(re, &text, rep)?;
        }
    }
    Ok(text)
}

fn redact_assignments(text: &str) -> Result<String, Limit> {
    // PCRE fails fast on a required literal: a long line without a separator cannot match. Without
    // this check fancy-regex spends its step limit on the line and it would be suppressed.
    if !text.contains(['=', ':']) {
        return Ok(text.to_owned());
    }
    let text = replace_all(
        &QUOTED_ASSIGNMENT,
        text,
        &format!("${{1}}${{2}}{REDACTED}${{1}}"),
    )?;
    replace_with(&ASSIGNMENT, &text, |m| {
        let whole = m.get(0).map_or("", |g| g.as_str());
        let prefix = m.name("prefix").map_or("", |g| g.as_str());
        let key = m.name("key").map_or("", |g| g.as_str());
        let value = m.name("value").map_or("", |g| g.as_str());
        let first = value.as_bytes().first().copied();
        let quote = match first {
            Some(q @ (b'"' | b'\'')) if value.len() > 1 && value.as_bytes().last() == Some(&q) => {
                (q as char).to_string()
            }
            _ => String::new(),
        };
        // "Failed to refresh token: Connection refused" is prose, not an assignment.
        let plain_word_key = key.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && key.chars().skip(1).all(|c| c.is_ascii_lowercase());
        let is_prose = quote.is_empty()
            && m.name("separator").map(|g| g.as_str()) == Some(":")
            && !prefix.starts_with(['"', '\''])
            && plain_word_key
            && !value.is_empty()
            && value.bytes().all(|b| b.is_ascii_alphabetic())
            && m.name("end").is_none();
        if is_prose {
            whole.to_owned()
        } else {
            format!("{prefix}{quote}{REDACTED}{quote}")
        }
    })
}

fn redact_chunk(chunk: &str) -> String {
    if let Ok(done) = redact_patterns(chunk).and_then(|t| redact_assignments(&t)) {
        return done;
    }
    let mut out = String::new();
    let mut inside_key = false;
    for line in chunk.split_inclusive('\n') {
        let nl = if line.ends_with('\n') { "\n" } else { "" };
        if !inside_key && BEGIN_KEY.is_match(line) && !END_KEY.is_match(line) {
            inside_key = true;
            out.push_str(REDACTED);
            out.push_str(nl);
            continue;
        }
        if inside_key {
            inside_key = !END_KEY.is_match(line);
            continue;
        }
        match redact_patterns(line).and_then(|t| redact_assignments(&t)) {
            Ok(t) => out.push_str(&t),
            Err(Limit) => {
                out.push_str(FAILURE_MESSAGE);
                out.push_str(nl);
            }
        }
    }
    out
}

fn line_end(text: &str, position: usize) -> usize {
    if position >= text.len() {
        return text.len();
    }
    text.as_bytes()[position..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(text.len(), |i| position + i + 1)
}

/// Splits on line boundaries; a chunk never ends inside a private key block.
fn chunks(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut offset = 0;
    while offset < text.len() {
        let mut end = line_end(text, offset + CHUNK_BYTES);
        let chunk = &text[offset..end];
        let unterminated = BEGIN_KEY
            .find_iter(chunk)
            .last()
            .is_some_and(|b| !END_KEY.is_match(&chunk[b.end()..]));
        if unterminated {
            end = END_KEY
                .find_at(text, end)
                .map_or(text.len(), |m| line_end(text, m.end()));
        }
        out.push(&text[offset..end]);
        offset = end;
    }
    out
}

fn rawurlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn json_inner(s: &str) -> String {
    let j = crate::php_json::encode(&serde_json::Value::String(s.to_owned()));
    j[1..j.len() - 1].to_owned()
}

fn replace_known_secrets(text: &str, known: &[&str], locked: &[&str]) -> Result<String, Limit> {
    let mut variants: Vec<String> = Vec::new();
    let mut short: Vec<String> = Vec::new();
    for (secrets, min) in [(known, MIN_KNOWN_SECRET_LENGTH), (locked, 1)] {
        for secret in secrets {
            if secret
                .trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B'])
                .is_empty()
                || secret.len() < min
            {
                continue;
            }
            let forms = [
                (*secret).to_owned(),
                rawurlencode(secret),
                json_inner(secret),
            ];
            if secret.len() < MIN_KNOWN_SECRET_LENGTH {
                short.extend(forms)
            } else {
                variants.extend(forms)
            }
        }
    }
    let dedup = |v: Vec<String>| {
        let mut seen = std::collections::HashSet::new();
        let mut v: Vec<String> = v
            .into_iter()
            .filter(|s| !s.is_empty() && seen.insert(s.clone()))
            .collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.len()));
        v
    };
    let mut text = text.to_owned();
    for v in dedup(variants) {
        text = text.replace(&v, REDACTED);
    }
    let short = dedup(short);
    if short.is_empty() {
        return Ok(text);
    }
    let alternatives = short
        .iter()
        .map(|s| fancy_regex::escape(s).into_owned())
        .collect::<Vec<_>>()
        .join("|");
    // PCRE fails fast on a required literal; fancy-regex would spend its limit on a long line.
    if !short.iter().any(|s| text.contains(s.as_str())) {
        return Ok(text);
    }
    let token = Regex::new(&format!(
        r"(?<![\p{{L}}\p{{M}}\p{{N}}_])(?:{alternatives})(?![\p{{L}}\p{{M}}\p{{N}}_])"
    ))
    .map_err(|_| Limit)?;
    replace_all(&token, &text, REDACTED)
}

/// `remove_iip($text)` / `redactText($text, $knownSecrets, $lockedSecrets)`.
pub fn redact_text(text: &str, known: &[&str], locked: &[&str]) -> String {
    let text = ANSI.replace_all(text, "");
    let text = CONTROL.replace_all(&text, "");
    let Ok(text) = replace_known_secrets(&text, known, locked) else {
        return FAILURE_MESSAGE.to_owned();
    };
    chunks(&text).into_iter().map(redact_chunk).collect()
}

/// The output of a stream up to its last complete line, as `DatabaseStartCommandExecutor` buffers
/// it, so a secret split across two output chunks is redacted as a whole.
pub fn take_complete_lines(pending: &mut String) -> String {
    let Some(end) = pending.rfind(['\n', '\r']) else {
        return String::new();
    };
    let rest = pending.split_off(end + 1);
    std::mem::replace(pending, rest)
}

/// `RunRemoteProcess::takeCompleteOutput()`: complete lines, with a private key block buffered until
/// its END line. With `last`, all buffered output, and an unfinished private key block is hidden.
pub fn take_complete_output(pending: &mut String, last: bool) -> String {
    static UNFINISHED_PRIVATE_KEY: std::sync::LazyLock<Option<fancy_regex::Regex>> =
        std::sync::LazyLock::new(|| {
            fancy_regex::Regex::new(
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----(?![\s\S]*-----END [A-Z ]*PRIVATE KEY-----)[\s\S]*",
            )
            .ok()
        });
    let unfinished = |text: &str| -> Result<Option<usize>, ()> {
        match UNFINISHED_PRIVATE_KEY
            .as_ref()
            .map(|re| re.find(text).map(|m| m.map(|m| m.start())))
        {
            Some(Ok(found)) => Ok(found),
            _ => Err(()),
        }
    };
    if last {
        let text = std::mem::take(pending);
        return match unfinished(&text) {
            Ok(Some(start)) => format!("{}{REDACTED}", &text[..start]),
            Ok(None) => text,
            Err(()) => REDACTED.to_owned(),
        };
    }
    let Some(end) = pending.rfind(['\n', '\r']) else {
        return String::new();
    };
    let mut take = end + 1;
    if let Ok(Some(start)) = unfinished(&pending[..take]) {
        take = start;
    }
    let rest = pending.split_off(take);
    std::mem::replace(pending, rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_lines_keep_a_split_secret_together() {
        let mut pending = "one\ntopse".to_owned();
        assert_eq!(take_complete_lines(&mut pending), "one\n");
        pending.push_str("cret\n");
        assert_eq!(take_complete_lines(&mut pending), "topsecret\n");
        assert_eq!(pending, "");
    }

    #[test]
    fn complete_output_buffers_a_private_key_block() {
        let begin = concat!("-----BEGIN RSA ", "PRIVATE KEY-----");
        let finish = concat!("-----END RSA ", "PRIVATE KEY-----");
        let mut pending = format!("first\n{begin}\nabc\n");
        assert_eq!(take_complete_output(&mut pending, false), "first\n");
        pending.push_str(&format!("{finish}\n"));
        assert_eq!(
            take_complete_output(&mut pending, false),
            format!("{begin}\nabc\n{finish}\n")
        );
        let mut open = format!("x\n{begin}\nabc");
        assert_eq!(take_complete_output(&mut open, true), format!("x\n{REDACTED}"));
    }

    fn r(t: &str) -> String {
        redact_text(t, &[], &[])
    }
    const X: &str = REDACTED;

    /// The largest output a scheduled task stores: one line of 5MB.
    #[test]
    fn a_very_long_line_is_kept() {
        let line = "a".repeat(5 * 1024 * 1024);
        assert!(redact_text(&line, &["some-long-secret"], &["pw1"]) == line);
    }

    // Cases from upstream tests/Unit/SensitiveDataRedactorTest.php.
    #[test]
    fn known_and_locked_secrets() {
        let known = [
            "coolify-secret",
            "coolify-secret-longer",
            "secret with spaces",
            "quote\"secret",
        ];
        assert_eq!(
            redact_text(
                r#"coolify-secret-longer coolify-secret secret%20with%20spaces quote\"secret"#,
                &known,
                &[]
            ),
            format!("{X} {X} {X} {X}")
        );
        assert_eq!(
            redact_text("mode=dev enabled=yes count=1", &["dev", "yes", "1"], &[]),
            "mode=dev enabled=yes count=1"
        );
        assert_eq!(
            redact_text("pin 4821 mode dev", &["dev"], &["4821"]),
            format!("pin {X} mode dev")
        );
        assert_eq!(
            redact_text("database ab_c cab 9ab", &[], &["ab"]),
            "database ab_c cab 9ab"
        );
        assert_eq!(
            redact_text(
                r#"password xy user=xy "xy" postgres://u:xy@host"#,
                &[],
                &["xy"]
            ),
            format!(r#"password {X} user={X} "{X}" postgres://u:{X}@host"#)
        );
        assert_eq!(redact_text("éxyé xy", &[], &["xy"]), format!("éxyé {X}"));
        assert_eq!(
            redact_text(
                r#"p%40s p\"s\"q long-locked-value long-locked-value-2 inside-long-locked-valueX"#,
                &[],
                &["p@s", "p\"s\"q", "long-locked-value", "long-locked-value-2"]
            ),
            format!("{X} {X} {X} {X} inside-{X}X")
        );
    }

    #[test]
    fn patterns() {
        assert_eq!(
            r("Authorization: Bearer coolify-canary-secret-scheduled\nstatus=ok"),
            format!("Authorization: {X}\nstatus=ok")
        );
        let out = r(
            "\x1b[31mhttps://user:password@example.com\x1b[0m Bearer aaa.bbb.ccc -----BEGIN PRIVATE KEY-----\nsecret-key-data\n-----END PRIVATE KEY-----",
        );
        assert!(
            out.contains(&format!("https://user:{X}@example.com")),
            "{out}"
        );
        assert!(
            out.contains(&format!("Bearer {X}"))
                && !out.contains("secret-key-data")
                && !out.contains('\x1b'),
            "{out}"
        );
        assert_eq!(r("valid\u{0}\u{7}\nnext"), "valid\nnext");
        assert_eq!(
            r("Error: deploy@example.com: Permission denied (publickey)."),
            format!("Error: {X}: Permission denied (publickey).")
        );
    }

    #[test]
    fn assignments() {
        assert_eq!(
            r(r#"password="first second" PASSWORD='third fourth' next=1"#),
            format!(r#"password="{X}" PASSWORD='{X}' next=1"#)
        );
        assert_eq!(
            r(r#"smtp_password=hunter2 "db-password": "p w" max_tokens=100"#),
            format!(r#"smtp_password={X} "db-password": "{X}" max_tokens=100"#)
        );
        assert_eq!(
            r(r#"{"token": "abc", "password": "a\"b", "name": "demo"}"#),
            format!(r#"{{"token": "{X}", "password": "{X}", "name": "demo"}}"#)
        );
        assert_eq!(r("{'secret': 'abc'}"), format!("{{'secret': '{X}'}}"));
        assert_eq!(
            r("Failed to refresh token: Connection refused"),
            "Failed to refresh token: Connection refused"
        );
        assert_eq!(r("PWD=/artifacts/abc"), "PWD=/artifacts/abc");
        for (input, expected) in [
            ("password=abc", format!("password={X}")),
            (r#""password": "abc""#, format!(r#""password": "{X}""#)),
            (r#""token": abc123,"#, format!(r#""token": {X},"#)),
            ("API_TOKEN=abc", format!("API_TOKEN={X}")),
            (
                "POSTGRES_PASSWORD: abc next",
                format!("POSTGRES_PASSWORD: {X} next"),
            ),
            (
                "smtp_password: abc next",
                format!("smtp_password: {X} next"),
            ),
            (
                "password: correcthorse\nnext",
                format!("password: {X}\nnext"),
            ),
            (
                "Password: hunter2 was set",
                format!("Password: {X} was set"),
            ),
            ("Authorization: Bearer x", format!("Authorization: {X}")),
            (
                "docker build --build-arg 'COOLIFY_BUILD_SECRETS_HASH=abc123def' --build-arg 'COOLIFY_URL'",
                format!(
                    "docker build --build-arg 'COOLIFY_BUILD_SECRETS_HASH={X}' --build-arg 'COOLIFY_URL'"
                ),
            ),
            (
                r#"export "DB_PASSWORD=first second" && next"#,
                format!(r#"export "DB_PASSWORD={X}" && next"#),
            ),
            (
                "--env 'api_token=abc def' --rm",
                format!("--env 'api_token={X}' --rm"),
            ),
            // From the reference container (remove_iip).
            (
                "Error: password=hunter2 token: abc123 Bearer xyz.abc",
                format!("Error: password={X} token: {X} Bearer {X}"),
            ),
        ] {
            assert_eq!(r(input), expected, "{input}");
        }
    }

    #[test]
    fn large_text_and_keys_across_chunks() {
        let line = format!("{}\n", "a".repeat(1000));
        let text = format!(
            "{}password=coolify-canary-tail-secret\nlast line",
            line.repeat(600)
        );
        let out = r(&text);
        assert!(out.len() > 600_000 && out.ends_with(&format!("password={X}\nlast line")));

        let key = format!(
            "-----BEGIN PRIVATE KEY-----\n{}-----END PRIVATE KEY-----",
            "secret-key-line\n".repeat(20000)
        );
        let text = format!("{}{key}\nafter", line.repeat(250));
        let out = r(&text);
        assert!(!out.contains("secret-key-line") && out.ends_with(&format!("{X}\nafter")));
    }
}
