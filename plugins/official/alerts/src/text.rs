//! Small text helpers: reading payload fields, shortening text, and keys services accept.

use serde_json::Value;

/// The string at `path` in `v`, or `""`. Numbers are written out; anything else is empty.
pub fn field<'a>(v: &'a Value, path: &[&str]) -> std::borrow::Cow<'a, str> {
    let mut cur = v;
    for key in path {
        match cur.get(key) {
            Some(next) => cur = next,
            None => return "".into(),
        }
    }
    match cur {
        Value::String(s) => s.as_str().into(),
        Value::Number(n) => n.to_string().into(),
        _ => "".into(),
    }
}

/// `text` as one line: control characters (line breaks included) become spaces, at most `max` characters.
pub fn line(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    head(flat.trim(), max)
}

/// The first `max` characters of `text`, with `…` when something was cut.
pub fn head(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The last `max` characters of `text`, with `…` in front when something was cut. Command output ends with
/// the error, so this is the part worth sending. Carriage returns and other control characters except line
/// breaks and tabs are dropped.
pub fn tail(text: &str, max: usize) -> String {
    let clean: String = text
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    let clean = clean.trim();
    let n = clean.chars().count();
    if n <= max {
        return clean.to_owned();
    }
    let mut out = String::from("…");
    out.extend(clean.chars().skip(n - max.saturating_sub(1)));
    out
}

/// A key made of `a-z`, `0-9`, `.`, `_` and `-` only, at most 200 characters: PagerDuty dedup keys and
/// Opsgenie aliases (which travel in a URL path) take it as is.
pub fn key(parts: &[&str]) -> String {
    let mut out = String::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push('-');
        }
        for c in part.chars() {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                out.push(c);
            } else if !out.ends_with('-') {
                out.push('-');
            }
        }
    }
    let out = out.trim_matches('-');
    out.chars().take(200).collect()
}

/// A short, stable hash (FNV-1a, 64 bits, hex) for keys built from many parts.
pub fn hash(parts: &[&str]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for b in part.bytes().chain(std::iter::once(0)) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// `text` escaped for HTML text and attribute values.
pub fn html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fields_shortening_and_keys() {
        let v = json!({"a": {"b": "x", "n": 3}, "s": null});
        assert_eq!(field(&v, &["a", "b"]), "x");
        assert_eq!(field(&v, &["a", "n"]), "3");
        assert_eq!(field(&v, &["s"]), "");
        assert_eq!(field(&v, &["missing", "b"]), "");

        assert_eq!(line(" a\nb\r\tc ", 10), "a b  c");
        assert_eq!(head("abcdef", 4), "abc…");
        assert_eq!(head("abc", 4), "abc");
        assert_eq!(tail("one\r\ntwo\nthree", 100), "one\ntwo\nthree");
        assert_eq!(tail("0123456789", 5), "…6789");

        assert_eq!(
            key(&["coolify-rs", "Server", "ab C/d"]),
            "coolify-rs-server-ab-c-d"
        );
        assert_eq!(key(&["x", "", "y"]), "x--y");
        assert_eq!(key(&[&"a".repeat(300)]).len(), 200);
        assert_eq!(hash(&["a", "b"]), hash(&["a", "b"]));
        assert_ne!(hash(&["ab", ""]), hash(&["a", "b"]));
        assert_eq!(
            html(r#"<b a="1">&'"#),
            "&lt;b a=&quot;1&quot;&gt;&amp;&#39;"
        );
    }
}
