//! `Symfony\Component\Yaml\Yaml::dump($value, $inline)` with default flags and
//! 4-space indentation (symfony/yaml 7, as used by upstream). Upstream writes
//! the Docker Compose files of resources with it, so the files coolify-rs
//! writes must come out byte-identical.

use std::sync::LazyLock;

use regex::Regex;

/// A PHP value as `Yaml::dump` sees it. Arrays keep insertion order; an array
/// whose keys are exactly 0..n is a list (`Inline::isHash` is false).
#[derive(Debug, Clone, PartialEq)]
pub enum PhpValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<(Key, PhpValue)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Int(i64),
    Str(String),
}

impl Key {
    /// The key PHP stores for a string key: a canonical decimal integer (`"8080"`, `"-1"`, not
    /// `"08"`, `"+1"` or `"-0"`) becomes an integer key.
    pub fn from_php_string(s: &str) -> Key {
        let digits = s.strip_prefix('-').unwrap_or(s);
        let canonical = !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit())
            && (digits == "0" || !digits.starts_with('0'))
            && s != "-0";
        match s.parse::<i64>() {
            Ok(i) if canonical => Key::Int(i),
            _ => Key::Str(s.to_owned()),
        }
    }

    /// The key as PHP prints it.
    pub fn as_string(&self) -> String {
        match self {
            Key::Int(i) => i.to_string(),
            Key::Str(s) => s.clone(),
        }
    }
}

impl From<&str> for PhpValue {
    fn from(s: &str) -> Self {
        PhpValue::Str(s.to_owned())
    }
}
impl From<String> for PhpValue {
    fn from(s: String) -> Self {
        PhpValue::Str(s)
    }
}
impl From<i64> for PhpValue {
    fn from(i: i64) -> Self {
        PhpValue::Int(i)
    }
}
impl From<bool> for PhpValue {
    fn from(b: bool) -> Self {
        PhpValue::Bool(b)
    }
}

impl PhpValue {
    /// A list (`[a, b]` in PHP).
    pub fn list<I: IntoIterator<Item = PhpValue>>(items: I) -> Self {
        PhpValue::Array(
            items
                .into_iter()
                .enumerate()
                .map(|(i, v)| (Key::Int(i as i64), v))
                .collect(),
        )
    }
    /// An associative array with string keys, in order.
    pub fn map<K: Into<String>, I: IntoIterator<Item = (K, PhpValue)>>(items: I) -> Self {
        PhpValue::Array(
            items
                .into_iter()
                .map(|(k, v)| (Key::Str(k.into()), v))
                .collect(),
        )
    }
    pub fn strings<I: IntoIterator<Item = S>, S: Into<String>>(items: I) -> Self {
        Self::list(items.into_iter().map(|s| PhpValue::Str(s.into())))
    }
    fn is_empty_array(&self) -> bool {
        matches!(self, PhpValue::Array(a) if a.is_empty())
    }
    /// `$array[$key] = $value`: replaces in place or appends.
    pub fn set(&mut self, key: &str, value: PhpValue) {
        if let PhpValue::Array(items) = self {
            match items
                .iter_mut()
                .find(|(k, _)| matches!(k, Key::Str(s) if s == key))
            {
                Some(slot) => slot.1 = value,
                None => items.push((Key::Str(key.to_owned()), value)),
            }
        }
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut PhpValue> {
        match self {
            PhpValue::Array(items) => items
                .iter_mut()
                .find(|(k, _)| matches!(k, Key::Str(s) if s == key))
                .map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn remove(&mut self, key: &str) {
        if let PhpValue::Array(items) = self {
            items.retain(|(k, _)| !matches!(k, Key::Str(s) if s == key));
        }
    }
    /// `$list[] = $value`.
    pub fn push(&mut self, value: PhpValue) {
        if let PhpValue::Array(items) = self {
            let next = items
                .iter()
                .filter_map(|(k, _)| {
                    if let Key::Int(i) = k {
                        Some(*i + 1)
                    } else {
                        None
                    }
                })
                .max()
                .unwrap_or(0);
            items.push((Key::Int(next), value));
        }
    }
}

fn is_hash(items: &[(Key, PhpValue)]) -> bool {
    items
        .iter()
        .enumerate()
        .any(|(i, (k, _))| *k != Key::Int(i as i64))
}

/// `Yaml::dump($value, $inline)` (indentation 4).
pub fn dump(value: &PhpValue, inline: i32) -> String {
    do_dump(value, inline, 0, 0, 4)
}

/// `Yaml::dump($value, $inline, $indentation)`.
pub fn dump_with_indent(value: &PhpValue, inline: i32, indentation: usize) -> String {
    do_dump(value, inline, 0, 0, indentation.max(1))
}

fn do_dump(input: &PhpValue, inline: i32, indent: usize, nesting: usize, step: usize) -> String {
    let prefix = " ".repeat(indent);
    let items = match input {
        PhpValue::Array(items) if inline > 0 && !items.is_empty() => items,
        _ => return format!("{prefix}{}", inline_dump(input, nesting == 0)),
    };
    let as_map = is_hash(items);
    let mut out = String::new();
    for (key, value) in items {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        let will_inline =
            inline - 1 <= 0 || !matches!(value, PhpValue::Array(_)) || value.is_empty_array();
        let head = if as_map {
            format!("{}:", inline_key(key))
        } else {
            "-".to_owned()
        };
        let child = do_dump(
            value,
            inline - 1,
            if will_inline { 0 } else { indent + step },
            nesting + 1,
            step,
        );
        out.push_str(&prefix);
        out.push_str(&head);
        out.push_str(if will_inline { " " } else { "\n" });
        out.push_str(&child);
        if will_inline {
            out.push('\n');
        }
    }
    out
}

fn inline_key(k: &Key) -> String {
    match k {
        Key::Int(i) => i.to_string(),
        Key::Str(s) => inline_dump(&PhpValue::Str(s.clone()), false),
    }
}

/// `Inline::dump`.
fn inline_dump(v: &PhpValue, root: bool) -> String {
    let _ = root;
    match v {
        PhpValue::Null => "null".into(),
        PhpValue::Bool(true) => "true".into(),
        PhpValue::Bool(false) => "false".into(),
        PhpValue::Int(i) => i.to_string(),
        PhpValue::Float(f) => dump_float(*f),
        PhpValue::Str(s) => dump_string(s),
        PhpValue::Array(items) => {
            if !items.is_empty() && !is_hash(items) {
                let parts: Vec<String> = items.iter().map(|(_, v)| inline_dump(v, false)).collect();
                format!("[{}]", parts.join(", "))
            } else if items.is_empty() {
                "{}".into()
            } else {
                let parts: Vec<String> = items
                    .iter()
                    .map(|(k, v)| format!("{}: {}", inline_key(k), inline_dump(v, false)))
                    .collect();
                format!("{{ {} }}", parts.join(", "))
            }
        }
    }
}

fn dump_float(f: f64) -> String {
    if f.is_nan() {
        return ".NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 {
            ".Inf".into()
        } else {
            "-.Inf".into()
        };
    }
    let repr = php_float_string(f);
    if f.floor() == f && !repr.contains('E') {
        format!("{repr}.0")
    } else {
        repr
    }
}

/// PHP `(string) $float` (precision 14).
fn php_float_string(f: f64) -> String {
    let a = f.abs();
    if a != 0.0 && !(1e-4..1e15).contains(&a) {
        // `1.0E+20`: shortest mantissa, at least one decimal, signed exponent.
        let s = format!("{:.13e}", f);
        let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
        let mut m = mantissa.trim_end_matches('0').to_owned();
        if m.ends_with('.') {
            m.push('0');
        }
        let exp: i32 = exp.parse().unwrap_or(0);
        return format!("{m}E{}{}", if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    let s = format!("{:.14}", f);
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".into()
    } else {
        trimmed.to_owned()
    }
}

/// PHP `is_numeric` for strings (PHP 8: leading and trailing whitespace allowed).
pub fn php_is_numeric(s: &str) -> bool {
    static NUM: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[ \t\n\r\x0B\x0C]*[+-]?(\d+(\.\d*)?|\.\d+)([eE][+-]?\d+)?[ \t\n\r\x0B\x0C]*$")
            .unwrap_or_else(|_| unreachable!())
    });
    NUM.is_match(s)
}

fn requires_double_quoting(s: &str) -> bool {
    s.chars().any(|c| {
        (c as u32) < 0x20
            || c == '\x7f'
            || c == '\u{85}'
            || c == '\u{a0}'
            || c == '\u{2028}'
            || c == '\u{2029}'
    })
}

fn requires_single_quoting(s: &str) -> bool {
    if [
        "null", "~", "true", "false", "y", "n", "yes", "no", "on", "off",
    ]
    .contains(&s.to_lowercase().as_str())
    {
        return true;
    }
    static ANY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"[\s'":{}\[\],&*#?]"#).unwrap_or_else(|_| unreachable!()));
    static START: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\A[\-?|<>=!%@`\p{Zs}]").unwrap_or_else(|_| unreachable!()));
    ANY.is_match(s) || START.is_match(s)
}

fn is_binary_string(s: &str) -> bool {
    s.bytes()
        .any(|b| !(b == 0 || (0x07..=0x0d).contains(&b) || b == 0x1b || b >= 0x20))
}

fn escape_double(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\0' => out.push_str("\\0"),
            '\x07' => out.push_str("\\a"),
            '\x08' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\x0b' => out.push_str("\\v"),
            '\x0c' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '\x1b' => out.push_str("\\e"),
            '\u{85}' => out.push_str("\\N"),
            '\u{a0}' => out.push_str("\\_"),
            '\u{2028}' => out.push_str("\\L"),
            '\u{2029}' => out.push_str("\\P"),
            c if (c as u32) < 0x20 || c == '\x7f' => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn escape_single(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn dump_string(s: &str) -> String {
    static DIGITS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[0-9]+[_0-9]*$").unwrap_or_else(|_| unreachable!()));
    static OCTAL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[+-]?0o[0-7_]+$").unwrap_or_else(|_| unreachable!()));
    static HEX: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^0x[0-9a-fA-F_]+$").unwrap_or_else(|_| unreachable!()));
    static TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^[0-9]{4}-[0-9]{1,2}-[0-9]{1,2}(?:(?:[Tt]|[ \t]+)[0-9]{1,2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]*)?(?:[ \t]*(?:Z|[-+][0-9]{1,2}(?::[0-9]{2})?))?)?$",
        )
        .unwrap_or_else(|_| unreachable!())
    });
    if php_is_numeric(s) && !s.contains(['\x0c', '\n', '\r', '\t', '\x0b']) {
        return format!("'{s}'");
    }
    if s.is_empty() {
        return "''".into();
    }
    if is_binary_string(s) {
        use base64::Engine as _;
        return format!(
            "!!binary {}",
            base64::engine::general_purpose::STANDARD.encode(s)
        );
    }
    if requires_double_quoting(s) {
        return escape_double(s);
    }
    if requires_single_quoting(s) {
        let single = escape_single(s);
        if !s.contains('\'') {
            return single;
        }
        let double = escape_double(s);
        return if double.len() < single.len() {
            double
        } else {
            single
        };
    }
    if DIGITS.is_match(s) || OCTAL.is_match(s) || HEX.is_match(s) || TIMESTAMP.is_match(s) {
        return escape_single(s);
    }
    s.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_follow_symfony() {
        for (input, expected) in [
            ("plain", "plain"),
            ("postgres:16-alpine", "'postgres:16-alpine'"),
            ("-U", "'-U'"),
            ("SELECT 1", "'SELECT 1'"),
            ("", "''"),
            ("123", "'123'"),
            ("1.5", "'1.5'"),
            ("yes", "'yes'"),
            ("it's here", "\"it's here\""),
            ("a\nb", "\"a\\nb\""),
            ("2024-01-01", "'2024-01-01'"),
            ("0x1F", "'0x1F'"),
            ("1_000", "'1_000'"),
            ("POSTGRES_USER=postgres", "POSTGRES_USER=postgres"),
            ("coolify.managed=true", "coolify.managed=true"),
            ("unless-stopped", "unless-stopped"),
            ("15s", "15s"),
            ("%h", "'%h'"),
            ("a#b", "'a#b'"),
        ] {
            assert_eq!(dump_string(input), expected, "{input:?}");
        }
        assert_eq!(dump_float(0.0), "0.0");
        assert_eq!(dump_float(1.5), "1.5");
        assert_eq!(dump_float(2.0), "2.0");
    }

    #[test]
    fn block_structure() {
        let doc = PhpValue::map([
            (
                "services",
                PhpValue::map([(
                    "abc",
                    PhpValue::map([
                        ("image", PhpValue::from("postgres:16-alpine")),
                        ("environment", PhpValue::strings(["A=1", "B=2"])),
                        (
                            "healthcheck",
                            PhpValue::map([
                                ("test", PhpValue::strings(["CMD", "psql"])),
                                ("retries", PhpValue::Int(5)),
                            ]),
                        ),
                        ("cpus", PhpValue::Float(0.0)),
                        ("volumes", PhpValue::list([])),
                        (
                            "mount",
                            PhpValue::list([PhpValue::map([
                                ("type", PhpValue::from("bind")),
                                ("read_only", PhpValue::Bool(true)),
                            ])]),
                        ),
                    ]),
                )]),
            ),
            (
                "networks",
                PhpValue::map([(
                    "coolify",
                    PhpValue::map([("external", PhpValue::Bool(true))]),
                )]),
            ),
        ]);
        assert_eq!(
            dump(&doc, 10),
            "services:\n    abc:\n        image: 'postgres:16-alpine'\n        environment:\n            - A=1\n            - B=2\n        healthcheck:\n            test:\n                - CMD\n                - psql\n            retries: 5\n        cpus: 0.0\n        volumes: {  }\n        mount:\n            -\n                type: bind\n                read_only: true\nnetworks:\n    coolify:\n        external: true\n"
                .replace("{  }", "{}")
        );
    }
}
