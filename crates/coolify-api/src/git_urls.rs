//! Git repository URLs and branches as the public-repository create endpoint sees them:
//! the `ValidGitRepositoryUrl` and `ValidGitBranch` rules (`app/Rules/`),
//! `parseScpStyleGitUrl()` / `sshGitUrlToHttps()` and `generate_application_name()`
//! (`bootstrap/helpers/shared.php`), plus the PHP built-ins they rely on
//! (`rawurldecode`, `filter_var(FILTER_VALIDATE_URL / FILTER_VALIDATE_IP)`, `Str::kebab`).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::LazyLock;

use coolify_jobs::applications::domains::UrlParts;
use regex::Regex;
use serde_json::Value;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|_| unreachable!())
}

fn bytes_re(pattern: &str) -> regex::bytes::Regex {
    regex::bytes::Regex::new(pattern).unwrap_or_else(|_| unreachable!())
}

static IPV6_HOST: LazyLock<Regex> =
    LazyLock::new(|| re(r"^((?:https?|git)://|(?:ssh://)?[a-zA-Z0-9._-]+@)\[([0-9A-Fa-f:.]+)\]"));
static SCP_FORM: LazyLock<Regex> =
    LazyLock::new(|| re(r"^[a-zA-Z0-9._-]+@(\[[0-9A-Fa-f:.]+\]|[a-zA-Z0-9.-]+):"));
static SCP_PARTS: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^(?<user>[A-Za-z0-9._-]+)@(?<host>\[[0-9A-Fa-f:.]+\]|[^:\[\]\n]+):(?:(?<port>\d+)/)?(?<path>[^\n]+)$",
    )
});
static SCP_HOST: LazyLock<Regex> = LazyLock::new(|| re(r"^(\[[0-9A-Fa-f:.]+\]|[a-zA-Z0-9.-]+)$"));
static SCP_PATH: LazyLock<Regex> = LazyLock::new(|| re(r"^[a-zA-Z0-9\-_/.~%]+$"));
static SSH_URL: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^ssh://[a-zA-Z0-9._-]+@(\[[0-9A-Fa-f:.]+\]|[a-zA-Z0-9.-]+)(:[0-9]{1,5})?/[a-zA-Z0-9\-_/.~%]+$",
    )
});
static HTTP_PATH: LazyLock<Regex> = LazyLock::new(|| re(r"^[a-zA-Z0-9\-_/.@~%]+$"));
static GIT_URL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^git://(\[[0-9A-Fa-f:.]+\]|[a-zA-Z0-9.\-]+)(:[0-9]+)?[:/][a-zA-Z0-9\-_/.~%]+$")
});
static CONTROL: LazyLock<regex::bytes::Regex> = LazyLock::new(|| bytes_re(r"[\x00-\x1F\x7F]"));
/// The `$dangerousPatterns` of `ValidGitRepositoryUrl`, byte-wise like `preg_match` without `/u`.
static DANGEROUS_PATTERNS: LazyLock<Vec<regex::bytes::Regex>> = LazyLock::new(|| {
    [
        r"\$\(.*\)",
        r"\$\{.*\}",
        r";;",
        r"&&",
        r"\|\|",
        r">>",
        r"<<",
        r"\\n",
        r"\.\.[/\\]",
    ]
    .iter()
    .map(|p| bytes_re(&format!("(?-u){p}")))
    .collect()
});
static BRANCH_CHARS: LazyLock<Regex> = LazyLock::new(|| re(r"^[a-zA-Z0-9\-_/.]+$"));
static NAME_STRIP: LazyLock<Regex> = LazyLock::new(|| re(r"[^\p{L}\p{M}\p{N}\s\-_.@/&()#,:+]+"));

/// PHP `empty()` of a JSON value.
fn php_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty() || s == "0",
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

/// PHP's string conversion of a scalar the rules receive.
fn php_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "1".into(),
        Value::Bool(false) | Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Whether a rule object would throw a `TypeError` on this value (`rawurldecode()` /
/// `trim()` of a non-empty array): Laravel answers 500.
pub fn rule_throws(v: Option<&Value>) -> bool {
    v.is_some_and(|v| (v.is_array() || v.is_object()) && !php_empty(v))
}

/// `rawurldecode()`: `%XX` becomes the byte, everything else stays.
fn rawurldecode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && b[i + 1].is_ascii_hexdigit()
            && b[i + 2].is_ascii_hexdigit()
        {
            let hex = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
            out.push(hex(b[i + 1]) * 16 + hex(b[i + 2]));
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// `filter_var($ip, FILTER_VALIDATE_IP)`.
fn parse_ip(s: &str) -> Option<IpAddr> {
    s.parse::<Ipv4Addr>()
        .map(IpAddr::V4)
        .ok()
        .or_else(|| s.parse::<Ipv6Addr>().ok().map(IpAddr::V6))
}

/// `filter_var($ip, FILTER_VALIDATE_IP, FILTER_FLAG_NO_RES_RANGE) === false` for a valid IP
/// (PHP 8.4: 0/8, 127/8, 169.254/16, 240/4; ::, ::1, ::ffff:0:0/96, fe80::/10).
fn is_reserved_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 0 || o[0] >= 240 || o[0] == 127 || (o[0] == 169 && o[1] == 254)
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            let zero6 = s[..5].iter().all(|x| *x == 0);
            (zero6 && s[5] == 0 && s[6] == 0 && (s[7] == 0 || s[7] == 1))
                || (zero6 && s[5] == 0xffff)
                || (0xfe80..=0xfebf).contains(&s[0])
        }
    }
}

/// `_php_filter_validate_domain($host, FILTER_FLAG_HOSTNAME)`.
fn valid_php_hostname(host: &str) -> bool {
    let mut b = host.as_bytes();
    if b.last() == Some(&b'.') {
        b = &b[..b.len() - 1];
    }
    if b.len() > 253 || b.is_empty() {
        return false;
    }
    if !b[0].is_ascii_alphanumeric() {
        return false;
    }
    let mut label = 1usize;
    for i in 0..b.len() {
        let c = b[i];
        if c == b'.' {
            let next = b.get(i + 1).copied();
            if next == Some(b'.')
                || i == 0
                || !b[i - 1].is_ascii_alphanumeric()
                || !next.is_some_and(|n| n.is_ascii_alphanumeric())
            {
                return false;
            }
            label = 1;
        } else {
            let last = i + 1 == b.len();
            if label > 63 || ((c != b'-' || last) && !c.is_ascii_alphanumeric()) {
                return false;
            }
            label += 1;
        }
    }
    true
}

/// `is_userinfo_valid()` of `FILTER_VALIDATE_URL` (note `isdigit` for the first hex digit).
fn valid_userinfo(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:".contains(&c) {
            i += 1;
        } else if c == b'%'
            && i + 3 <= b.len()
            && b[i + 1].is_ascii_digit()
            && b[i + 2].is_ascii_hexdigit()
        {
            i += 3;
        } else {
            return false;
        }
    }
    true
}

/// PHP `parse_url()` of an `http(s)://` URL (`false` for a port above 65535).
fn php_parse_url(url: &str) -> Option<UrlParts> {
    let p = UrlParts::parse(url)?;
    if p.port_number().is_some_and(|n| n > 65535) {
        return None;
    }
    Some(p)
}

/// `filter_var($url, FILTER_VALIDATE_URL)` for `http(s)://` URLs.
fn filter_validate_url(url: &str) -> bool {
    // FILTER_SANITIZE_URL must not remove anything.
    let allowed =
        |c: char| c.is_ascii_alphanumeric() || "$-_.+!*'(),{}|\\^~[]`<>#%\";/?:@&=".contains(c);
    if !url.chars().all(allowed) {
        return false;
    }
    let Some(p) = php_parse_url(url) else {
        return false;
    };
    if p.host.is_empty() {
        return false;
    }
    let host_ok = match p.host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(inner) => inner.parse::<Ipv6Addr>().is_ok(),
        None => valid_php_hostname(&p.host),
    };
    host_ok
        && p.user.as_deref().is_none_or(valid_userinfo)
        && p.pass.as_deref().is_none_or(valid_userinfo)
}

/// `parseScpStyleGitUrl()`: `(user, host, port, path)` of `user@host:[port/]path`.
pub fn parse_scp_style_git_url(url: &str) -> Option<(String, String, Option<String>, String)> {
    let c = SCP_PARTS.captures(url)?;
    let host = c["host"].trim().to_owned();
    let path = c["path"].trim_start_matches('/').to_owned();
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some((
        c["user"].to_owned(),
        host,
        c.name("port").map(|m| m.as_str().to_owned()),
        path,
    ))
}

/// `sshGitUrlToHttps()`: `ssh://` and scp-style URLs as `https://{host}/{path}`.
pub fn ssh_git_url_to_https(url: &str) -> Option<String> {
    if url.starts_with("ssh://") {
        let p = UrlParts::parse(url)?;
        let path = p.path.unwrap_or_default();
        let path = path.trim_start_matches('/');
        return (!p.host.trim().is_empty() && !path.is_empty())
            .then(|| format!("https://{}/{path}", p.host));
    }
    let (_, host, _, path) = parse_scp_style_git_url(url)?;
    Some(format!("https://{host}/{path}"))
}

/// The `ValidGitRepositoryUrl` rule (SSH and IPs allowed).
pub fn valid_git_repository_url(v: &Value) -> Option<String> {
    if php_empty(v) {
        return None;
    }
    let value = php_string(v);
    let without_ipv6 = IPV6_HOST.replace(&value, |c: &regex::Captures<'_>| {
        if c[2].parse::<Ipv6Addr>().is_ok() {
            format!("{}ipv6-host", &c[1])
        } else {
            c[0].to_owned()
        }
    });
    let decoded = rawurldecode(&without_ipv6);
    let invalid_chars = || Some("The :attribute contains invalid characters.".to_owned());
    if CONTROL.is_match(&decoded) {
        return invalid_chars();
    }
    // '\n', '\r' and '\0' are the two-character strings of PHP single quotes.
    let dangerous: &[&[u8]] = &[
        b";", b"|", b"&", b"$", b"`", b"(", b")", b"{", b"}", b"[", b"]", b"<", b">", b"\\n",
        b"\\r", b"\\0", b"\"", b"'", b"\\", b"!", b"?", b"*", b"^", b"%", b"=", b"+", b"#",
    ];
    if dangerous
        .iter()
        .any(|d| decoded.windows(d.len()).any(|w| w == *d))
    {
        return invalid_chars();
    }
    if DANGEROUS_PATTERNS.iter().any(|p| p.is_match(&decoded)) {
        return Some("The :attribute contains invalid patterns.".into());
    }
    let ssh_invalid = || Some("The :attribute is not a valid SSH repository URL.".to_owned());
    if SCP_FORM.is_match(&value) {
        let ok = parse_scp_style_git_url(&value)
            .is_some_and(|(_, host, _, path)| SCP_HOST.is_match(&host) && SCP_PATH.is_match(&path));
        return if ok { None } else { ssh_invalid() };
    }
    if value.starts_with("ssh://") {
        return if SSH_URL.is_match(&value) {
            None
        } else {
            ssh_invalid()
        };
    }
    if value.starts_with("http://") || value.starts_with("https://") {
        if !filter_validate_url(&value) {
            return Some("The :attribute is not a valid URL.".into());
        }
        let p = php_parse_url(&value)?;
        let host = p.host.to_lowercase();
        let host = host.trim_matches(|c| c == '[' || c == ']');
        let reserved = parse_ip(host).is_some_and(is_reserved_ip);
        if ["localhost", "127.0.0.1", "0.0.0.0", "::1"].contains(&host)
            || reserved
            || host.ends_with(".local")
        {
            return Some("The :attribute cannot point to internal hosts.".into());
        }
        let userinfo = format!(
            "{}{}{}",
            p.host,
            p.user.as_deref().unwrap_or_default(),
            p.pass.as_deref().unwrap_or_default()
        );
        if userinfo.contains('%') {
            return invalid_chars();
        }
        let filled = |s: &Option<String>| s.as_deref().is_some_and(|s| !s.is_empty() && s != "0");
        if filled(&p.query) || filled(&p.fragment) {
            return Some("The :attribute should not contain query parameters or fragments.".into());
        }
        let path = p.path.unwrap_or_default();
        if !path.is_empty() && path != "0" && !HTTP_PATH.is_match(&path) {
            return Some("The :attribute path contains invalid characters.".into());
        }
        return None;
    }
    if value.starts_with("git://") {
        return if GIT_URL.is_match(&value) {
            None
        } else {
            Some("The :attribute is not a valid git:// URL.".into())
        };
    }
    Some("The :attribute must start with https://, http://, git://, ssh://, or be an SSH URL (user@host:path).".into())
}

/// The `ValidGitBranch` rule.
pub fn valid_git_branch(v: &Value) -> Option<String> {
    if php_empty(v) {
        return None;
    }
    let raw = php_string(v);
    let branch = raw.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B']);
    let dangerous = [
        ";", "|", "&", "$", "`", "(", ")", "{", "}", "<", ">", "\\n", "\\r", "\\0", "\"", "'",
        "\\", "!", "*", "?", "[", "]", "~", "^", ":", " ", "#",
    ];
    if dangerous.iter().any(|d| branch.contains(d)) {
        return Some("The :attribute contains invalid characters.".into());
    }
    if branch.contains("..") || branch.contains("//") || branch.contains("@{") {
        return Some("The :attribute contains invalid patterns.".into());
    }
    if branch.starts_with('/')
        || branch.ends_with('/')
        || branch.starts_with('.')
        || branch.ends_with('.')
    {
        return Some("The :attribute cannot start or end with / or .".into());
    }
    if !BRANCH_CHARS.is_match(branch) {
        return Some("The :attribute contains invalid characters. Only letters, numbers, hyphens, underscores, forward slashes, and dots are allowed.".into());
    }
    if branch == "HEAD" {
        return Some("The :attribute cannot be HEAD.".into());
    }
    if branch.ends_with(".lock") {
        return Some("The :attribute cannot end with .lock.".into());
    }
    None
}

/// `Str::kebab()` (`Str::snake($value, '-')`).
pub fn kebab(value: &str) -> String {
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_lowercase()) {
        return value.to_owned();
    }
    // ucwords(): upper-case the first character of each word (ASCII only).
    let mut words = String::with_capacity(value.len());
    let mut at_start = true;
    for c in value.chars() {
        words.push(if at_start { c.to_ascii_uppercase() } else { c });
        at_start = matches!(c, ' ' | '\t' | '\r' | '\n' | '\x0C' | '\x0B');
    }
    let compact: Vec<char> = words.chars().filter(|c| !c.is_whitespace()).collect();
    // preg_replace('/(.)(?=[A-Z])/u', '$1-', ...)
    let mut out = String::with_capacity(compact.len() * 2);
    for (i, c) in compact.iter().enumerate() {
        out.push(*c);
        if *c != '\n' && compact.get(i + 1).is_some_and(char::is_ascii_uppercase) {
            out.push('-');
        }
    }
    out.to_lowercase()
}

/// `generate_application_name($git_repository, $git_branch)`.
pub fn generate_application_name(git_repository: &str, git_branch: &str) -> String {
    let cuid = coolify_domain::new_public_id();
    let repo_name = git_repository.rsplit('/').next().unwrap_or(git_repository);
    let name = kebab(&format!("{repo_name}:{git_branch}-{cuid}"));
    let name = NAME_STRIP.replace_all(&name, "").into_owned();
    if name.chars().count() < 3 {
        return crate::routes::private_keys::random_name();
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn repo(s: &str) -> Option<String> {
        valid_git_repository_url(&json!(s))
    }

    #[test]
    fn repository_rule_messages() {
        assert_eq!(repo("https://github.com/coollabsio/coolify-examples"), None);
        assert_eq!(repo("https://github.com/o/r.git"), None);
        assert_eq!(repo("git@github.com:o/r.git"), None);
        assert_eq!(repo("ssh://git@gitlab.com:2222/o/r.git"), None);
        assert_eq!(repo("git://example.com/o/r.git"), None);
        assert_eq!(repo("https://[2001:db8::1]/o/r"), None);
        assert_eq!(repo("0"), None);
        let m = |s: &str| repo(s).unwrap_or_default();
        assert_eq!(
            m("https://github.com/o/r;ls"),
            "The :attribute contains invalid characters."
        );
        assert_eq!(
            m("https://github.com/o/r%0A"),
            "The :attribute contains invalid characters."
        );
        assert_eq!(
            m("https://github.com/o/r%"),
            "The :attribute contains invalid characters."
        );
        assert_eq!(
            m("https://github.com/o/../r"),
            "The :attribute contains invalid patterns."
        );
        assert_eq!(
            m("git@github.com:o/r,x"),
            "The :attribute is not a valid SSH repository URL."
        );
        assert_eq!(
            m("ssh://github.com/o/r"),
            "The :attribute is not a valid SSH repository URL."
        );
        assert_eq!(
            m("https://git_hub.com/o/r"),
            "The :attribute is not a valid URL."
        );
        assert_eq!(m("https://a.com/o r"), "The :attribute is not a valid URL.");
        assert_eq!(
            m("https://localhost/o/r"),
            "The :attribute cannot point to internal hosts."
        );
        assert_eq!(
            m("http://[::1]/o/r"),
            "The :attribute cannot point to internal hosts."
        );
        assert_eq!(
            m("https://169.254.1.1/o/r"),
            "The :attribute cannot point to internal hosts."
        );
        assert_eq!(
            m("https://box.local/o/r"),
            "The :attribute cannot point to internal hosts."
        );
        assert_eq!(
            m("https://us%65r@github.com/o/r"),
            "The :attribute contains invalid characters."
        );
        assert_eq!(
            m("https://github.com/o/r,x"),
            "The :attribute path contains invalid characters."
        );
        assert_eq!(
            m("git://github.com/o/r,x"),
            "The :attribute is not a valid git:// URL."
        );
        assert_eq!(
            m("github.com/o/r"),
            "The :attribute must start with https://, http://, git://, ssh://, or be an SSH URL (user@host:path)."
        );
    }

    #[test]
    fn branch_rule_messages() {
        let m = |s: &str| valid_git_branch(&json!(s)).unwrap_or_default();
        assert_eq!(valid_git_branch(&json!("feature/x-1.2")), None);
        assert_eq!(m("a b"), "The :attribute contains invalid characters.");
        assert_eq!(m("a..b"), "The :attribute contains invalid patterns.");
        assert_eq!(m("a//b"), "The :attribute contains invalid patterns.");
        assert_eq!(m("/a"), "The :attribute cannot start or end with / or .");
        assert_eq!(
            m("maïn"),
            "The :attribute contains invalid characters. Only letters, numbers, hyphens, underscores, forward slashes, and dots are allowed."
        );
        assert_eq!(m("HEAD"), "The :attribute cannot be HEAD.");
        assert_eq!(m("x.lock"), "The :attribute cannot end with .lock.");
    }

    #[test]
    fn ssh_urls_become_https() {
        assert_eq!(
            ssh_git_url_to_https("git@github.com:o/r.git").as_deref(),
            Some("https://github.com/o/r.git")
        );
        assert_eq!(
            ssh_git_url_to_https("git@host:22/o/r").as_deref(),
            Some("https://host/o/r")
        );
        assert_eq!(
            ssh_git_url_to_https("ssh://git@gitlab.com:2222/o/r.git").as_deref(),
            Some("https://gitlab.com/o/r.git")
        );
        assert_eq!(ssh_git_url_to_https("https://github.com/o/r"), None);
    }

    #[test]
    fn kebab_matches_laravel() {
        // Captured from the reference: `r.git:-feature/-a-b-c-<cuid>`.
        assert_eq!(kebab("r.git:Feature/ABC-xyz"), "r.git:-feature/-a-b-c-xyz");
        assert_eq!(
            kebab("coolify-examples:main-abc"),
            "coolify-examples:main-abc"
        );
        assert_eq!(kebab("MyRepo:main-x"), "my-repo:main-x");
        let n = generate_application_name("https://github.com/o/coolify-examples.git", "main");
        assert!(n.starts_with("coolify-examples.git:main-") && n.len() == 26 + 24);
    }
}
