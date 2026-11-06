//! Small PHP helpers the notification classes and views use.

use serde_json::Value;

/// `e()`: `htmlspecialchars($v, ENT_QUOTES | ENT_SUBSTITUTE, 'UTF-8', true)`.
pub fn e(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#039;"),
            c => out.push(c),
        }
    }
    out
}

/// A PHP value in a boolean context: `null`, `""` and `"0"` are false.
pub fn truthy(s: Option<&str>) -> bool {
    s.is_some_and(|s| !s.is_empty() && s != "0")
}

/// `ucfirst()`: only an ASCII first byte changes.
pub fn ucfirst(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {
            let mut out = c.to_ascii_uppercase().to_string();
            out.push_str(chars.as_str());
            out
        }
        _ => s.to_owned(),
    }
}

/// `trim()` with PHP's default character list.
pub fn php_trim(s: &str) -> &str {
    s.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B'])
}

/// A JSON value interpolated into a PHP string.
pub fn php_string(v: &Value) -> String {
    match v {
        Value::Null | Value::Bool(false) => String::new(),
        Value::Bool(true) => "1".to_owned(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Array(_) => "Array".to_owned(),
        Value::Object(_) => "Array".to_owned(),
    }
}

/// `$v ?? false` in a boolean context.
pub fn value_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::Bool(true)) => true,
        Some(Value::String(s)) => truthy(Some(s)),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `filled()`.
pub fn filled(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !php_trim(s).is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(_) => true,
    }
}

/// East Asian wide characters count double in `mb_strwidth`.
fn char_width(c: char) -> usize {
    match c as u32 {
        0x1100..=0x115F
        | 0x11A3..=0x11A7
        | 0x11FA..=0x11FF
        | 0x2329..=0x232A
        | 0x2E80..=0x2E99
        | 0x2E9B..=0x2EF3
        | 0x2F00..=0x2FD5
        | 0x2FF0..=0x2FFB
        | 0x3000..=0x303E
        | 0x3041..=0x3096
        | 0x3099..=0x30FF
        | 0x3105..=0x312D
        | 0x3131..=0x318E
        | 0x3190..=0x31BA
        | 0x31C0..=0x31E3
        | 0x31F0..=0x321E
        | 0x3220..=0x3247
        | 0x3250..=0x32FE
        | 0x3300..=0x4DBF
        | 0x4E00..=0xA48C
        | 0xA490..=0xA4C6
        | 0xA960..=0xA97C
        | 0xAC00..=0xD7A3
        | 0xD7B0..=0xD7C6
        | 0xD7CB..=0xD7FB
        | 0xF900..=0xFAFF
        | 0xFE10..=0xFE19
        | 0xFE30..=0xFE52
        | 0xFE54..=0xFE66
        | 0xFE68..=0xFE6B
        | 0xFF01..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1B000..=0x1B001
        | 0x1F200..=0x1F202
        | 0x1F210..=0x1F23A
        | 0x1F240..=0x1F248
        | 0x1F250..=0x1F251
        | 0x20000..=0x2FFFD
        | 0x30000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// `Str::limit($value, $limit)`.
pub fn str_limit(s: &str, limit: usize) -> String {
    str_limit_end(s, limit, "...")
}

/// `Str::limit($value, $limit, $end)`.
pub fn str_limit_end(s: &str, limit: usize, end: &str) -> String {
    if s.chars().map(char_width).sum::<usize>() <= limit {
        return s.to_owned();
    }
    let mut width = 0;
    let mut cut = 0;
    for (i, c) in s.char_indices() {
        width += char_width(c);
        if width > limit {
            cut = i;
            break;
        }
    }
    format!(
        "{}{end}",
        s[..cut].trim_end_matches([' ', '\t', '\n', '\r', '\0', '\x0B'])
    )
}

/// `Symfony\Component\HttpFoundation\Response::$statusTexts`, 4xx part.
pub fn client_status_text(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        407 => "Proxy Authentication Required",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Content Too Large",
        414 => "URI Too Long",
        415 => "Unsupported Media Type",
        416 => "Range Not Satisfiable",
        417 => "Expectation Failed",
        418 => "I'm a teapot",
        421 => "Misdirected Request",
        422 => "Unprocessable Content",
        423 => "Locked",
        424 => "Failed Dependency",
        425 => "Too Early",
        426 => "Upgrade Required",
        428 => "Precondition Required",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        451 => "Unavailable For Legal Reasons",
        _ => "Client Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_like_blade() {
        assert_eq!(e(r#"a&b<c>"d'"#), "a&amp;b&lt;c&gt;&quot;d&#039;");
    }

    #[test]
    fn limit_counts_wide_characters_double() {
        assert_eq!(str_limit("abc", 3), "abc");
        assert_eq!(str_limit("abc d", 4), "abc...");
        assert_eq!(str_limit("日本語", 4), "日本...");
    }

    #[test]
    fn ucfirst_ascii_only() {
        assert_eq!(ucfirst("ubuntu"), "Ubuntu");
        assert_eq!(ucfirst("éa"), "éa");
    }
}
