//! PHP and Laravel semantics the parsers rely on, over [`PhpValue`]: array access (`data_get`,
//! `Arr::exists`), string conversion and truthiness, `Str::*` / `Stringable` helpers, `json_encode`
//! / `json_decode`, `parse_url`, Spatie `Url`, `version_compare`, `Str::random` and
//! `Str::password`.

use coolify_compat::yaml::{Key, PhpValue};
use rand::Rng;

/// An ordered PHP array.
pub type Arr = Vec<(Key, PhpValue)>;

/// `$array[$key]` for a string key (integer-like keys are integer keys in PHP).
pub fn get<'a>(v: &'a PhpValue, key: &str) -> Option<&'a PhpValue> {
    let PhpValue::Array(items) = v else {
        return None;
    };
    let k = Key::from_php_string(key);
    items.iter().find(|(ik, _)| *ik == k).map(|(_, v)| v)
}

/// `$array[$key]` for any key.
pub fn get_key<'a>(items: &'a [(Key, PhpValue)], key: &Key) -> Option<&'a PhpValue> {
    items.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// `data_get($target, 'a.b.c')`: `None` when a segment is missing (the default applies), the
/// value (possibly null) otherwise.
pub fn data_get<'a>(v: &'a PhpValue, path: &str) -> Option<&'a PhpValue> {
    let mut cur = v;
    for seg in path.split('.') {
        cur = get(cur, seg)?;
    }
    Some(cur)
}

/// `data_get($target, $path) ?? $default` as a string (`data_get_str()`).
pub fn data_get_str(v: &PhpValue, path: &str, default: &str) -> String {
    match data_get(v, path) {
        None | Some(PhpValue::Null) => default.to_owned(),
        Some(x) => to_string(x),
    }
}

/// `$array[$key] = $value` (replaces in place or appends).
pub fn put(items: &mut Arr, key: Key, value: PhpValue) {
    match items.iter_mut().find(|(k, _)| *k == key) {
        Some(slot) => slot.1 = value,
        None => items.push((key, value)),
    }
}

/// `$array[$key] = $value` for a string key.
pub fn put_str(items: &mut Arr, key: &str, value: PhpValue) {
    put(items, Key::from_php_string(key), value);
}

/// `$array[] = $value`.
pub fn push(items: &mut Arr, value: PhpValue) {
    let next = items
        .iter()
        .filter_map(|(k, _)| match k {
            Key::Int(i) => Some(*i + 1),
            Key::Str(_) => None,
        })
        .max()
        .unwrap_or(0)
        .max(0);
    items.push((Key::Int(next), value));
}

/// `unset($array[$key])`.
pub fn forget(items: &mut Arr, key: &str) {
    let k = Key::from_php_string(key);
    items.retain(|(ik, _)| *ik != k);
}

/// `collect($value)->all()`: null is empty, a scalar is a one-item list.
pub fn collect(v: Option<&PhpValue>) -> Arr {
    match v {
        None | Some(PhpValue::Null) => Vec::new(),
        Some(PhpValue::Array(items)) => items.clone(),
        Some(other) => vec![(Key::Int(0), other.clone())],
    }
}

/// `array_merge($a, $b)` / `Collection::merge()`: integer keys are renumbered and appended,
/// string keys are replaced in place or appended.
pub fn merge(a: &[(Key, PhpValue)], b: &[(Key, PhpValue)]) -> Arr {
    let mut out: Arr = Vec::new();
    for (k, v) in a.iter().chain(b.iter()) {
        match k {
            Key::Int(_) => push(&mut out, v.clone()),
            Key::Str(_) => put(&mut out, k.clone(), v.clone()),
        }
    }
    out
}

/// `array_values()`.
pub fn values(items: &[(Key, PhpValue)]) -> Arr {
    items
        .iter()
        .enumerate()
        .map(|(i, (_, v))| (Key::Int(i as i64), v.clone()))
        .collect()
}

/// `Inline::isHash()` negated: the keys are exactly 0..n.
pub fn is_list(items: &[(Key, PhpValue)]) -> bool {
    items
        .iter()
        .enumerate()
        .all(|(i, (k, _))| *k == Key::Int(i as i64))
}

/// `isAssociativeArray()`: not empty and the keys are not 0..n.
pub fn is_associative(items: &[(Key, PhpValue)]) -> bool {
    !items.is_empty() && !is_list(items)
}

/// PHP `(string) $value` (arrays become `Array`, as PHP does with a warning).
pub fn to_string(v: &PhpValue) -> String {
    match v {
        PhpValue::Null | PhpValue::Bool(false) => String::new(),
        PhpValue::Bool(true) => "1".into(),
        PhpValue::Int(i) => i.to_string(),
        PhpValue::Float(f) => float_to_string(*f),
        PhpValue::Str(s) => s.clone(),
        PhpValue::Array(_) => "Array".into(),
    }
}

/// PHP `(string) $float` (`precision` 14, `%.14G`).
pub fn float_to_string(f: f64) -> String {
    if f.is_nan() {
        return "NAN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "INF".into() } else { "-INF".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    let sci = format!("{f:.13e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if !(-4..14).contains(&exp) {
        let mut m = mantissa.trim_end_matches('0').to_owned();
        if m.ends_with('.') {
            m.push('0');
        }
        return format!("{m}E{}{}", if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    let decimals = usize::try_from(13 - exp).unwrap_or(0);
    let fixed = format!("{f:.decimals$}");
    if fixed.contains('.') {
        fixed.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        fixed
    }
}

/// PHP truthiness.
pub fn truthy(v: &PhpValue) -> bool {
    match v {
        PhpValue::Null => false,
        PhpValue::Bool(b) => *b,
        PhpValue::Int(i) => *i != 0,
        PhpValue::Float(f) => *f != 0.0,
        PhpValue::Str(s) => !s.is_empty() && s != "0",
        PhpValue::Array(a) => !a.is_empty(),
    }
}

/// `filled()` / `! blank()`.
pub fn filled(v: &PhpValue) -> bool {
    match v {
        PhpValue::Null => false,
        PhpValue::Str(s) => !s.trim().is_empty(),
        PhpValue::Array(a) => !a.is_empty(),
        _ => true,
    }
}

/// `filled()` for an optional string.
pub fn filled_str(v: Option<&str>) -> bool {
    v.is_some_and(|s| !s.trim().is_empty())
}

/// `empty($string)`.
pub fn empty_str(s: &str) -> bool {
    s.is_empty() || s == "0"
}

/// PHP 8 loose comparison `$a == $b` for the scalar shapes the parsers compare.
pub fn loose_eq(a: &PhpValue, b: &PhpValue) -> bool {
    use PhpValue as V;
    match (a, b) {
        (V::Null, V::Null) => true,
        (V::Null, V::Str(s)) | (V::Str(s), V::Null) => s.is_empty(),
        (V::Null, other) | (other, V::Null) => !truthy(other),
        (V::Bool(x), other) | (other, V::Bool(x)) => *x == truthy(other),
        (V::Str(x), V::Str(y)) => {
            if coolify_compat::yaml::php_is_numeric(x) && coolify_compat::yaml::php_is_numeric(y) {
                x.trim().parse::<f64>().ok() == y.trim().parse::<f64>().ok()
            } else {
                x == y
            }
        }
        (V::Int(i), V::Str(s)) | (V::Str(s), V::Int(i)) => {
            if coolify_compat::yaml::php_is_numeric(s) {
                s.trim().parse::<f64>().ok() == Some(*i as f64)
            } else {
                i.to_string() == *s
            }
        }
        (V::Int(x), V::Int(y)) => x == y,
        (V::Float(x), V::Float(y)) => x == y,
        (V::Int(i), V::Float(f)) | (V::Float(f), V::Int(i)) => *i as f64 == *f,
        (V::Float(f), V::Str(s)) | (V::Str(s), V::Float(f)) => {
            coolify_compat::yaml::php_is_numeric(s) && s.trim().parse::<f64>().ok() == Some(*f)
        }
        (V::Array(x), V::Array(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| get_key(y, k).is_some_and(|w| loose_eq(v, w)))
        }
        _ => false,
    }
}

/// The value of a key as PHP prints it in `"$key"`.
pub fn key_string(k: &Key) -> String {
    k.as_string()
}

/// `is_numeric()`.
pub fn is_numeric_value(v: &PhpValue) -> bool {
    match v {
        PhpValue::Int(_) | PhpValue::Float(_) => true,
        PhpValue::Str(s) => coolify_compat::yaml::php_is_numeric(s),
        _ => false,
    }
}

// --- Str / Stringable --------------------------------------------------------------------------

/// `Str::after()`.
pub fn after(s: &str, search: &str) -> String {
    if search.is_empty() {
        return s.to_owned();
    }
    s.split_once(search)
        .map_or_else(|| s.to_owned(), |(_, a)| a.to_owned())
}

/// `Str::before()`.
pub fn before(s: &str, search: &str) -> String {
    if search.is_empty() {
        return s.to_owned();
    }
    s.split_once(search)
        .map_or_else(|| s.to_owned(), |(b, _)| b.to_owned())
}

/// `Str::beforeLast()`.
pub fn before_last(s: &str, search: &str) -> String {
    if search.is_empty() {
        return s.to_owned();
    }
    s.rfind(search)
        .map_or_else(|| s.to_owned(), |p| s[..p].to_owned())
}

/// `Str::afterLast()`.
pub fn after_last(s: &str, search: &str) -> String {
    if search.is_empty() {
        return s.to_owned();
    }
    s.rfind(search)
        .map_or_else(|| s.to_owned(), |p| s[p + search.len()..].to_owned())
}

/// `Str::replaceFirst()`.
pub fn replace_first(s: &str, search: &str, replace: &str) -> String {
    if search.is_empty() {
        return s.to_owned();
    }
    s.find(search).map_or_else(
        || s.to_owned(),
        |p| format!("{}{replace}{}", &s[..p], &s[p + search.len()..]),
    )
}

/// `Str::replaceLast()`.
pub fn replace_last(s: &str, search: &str, replace: &str) -> String {
    if search.is_empty() {
        return s.to_owned();
    }
    s.rfind(search).map_or_else(
        || s.to_owned(),
        |p| format!("{}{replace}{}", &s[..p], &s[p + search.len()..]),
    )
}

/// `Str::upper()` (`mb_strtoupper`).
pub fn upper(s: &str) -> String {
    s.to_uppercase()
}

/// `Str::slug($title)` (ASCII transliteration is not ported: other letters are kept, lowercased).
pub fn slug(title: &str) -> String {
    crate::databases::slug(title)
}

/// `escapeshellarg()`.
pub fn escapeshellarg(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `e()` (`htmlspecialchars` with `ENT_QUOTES`, double encoding on).
pub fn html_escape(s: &str) -> String {
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

/// `Str::random($length)`.
pub fn str_random(length: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    (0..length)
        .map(|_| char::from(ALPHABET[rng.gen_range(0..ALPHABET.len())]))
        .collect()
}

/// `Str::password($length, symbols: $symbols)`: one character of each set, the rest from all of
/// them, shuffled.
pub fn str_password(length: usize, symbols: bool) -> String {
    use rand::seq::SliceRandom;
    let letters: Vec<char> = ('a'..='z').chain('A'..='Z').collect();
    let numbers: Vec<char> = ('0'..='9').collect();
    let symbol_set: Vec<char> = "~!#$%^&*()-_.,<>?/\\{}[]|:;".chars().collect();
    let mut sets: Vec<&Vec<char>> = vec![&letters, &numbers];
    if symbols {
        sets.push(&symbol_set);
    }
    let mut rng = rand::thread_rng();
    let mut out: Vec<char> = sets.iter().map(|s| s[rng.gen_range(0..s.len())]).collect();
    let all: Vec<char> = sets.iter().flat_map(|s| s.iter().copied()).collect();
    for _ in 0..length.saturating_sub(out.len()) {
        out.push(all[rng.gen_range(0..all.len())]);
    }
    out.shuffle(&mut rng);
    out.into_iter().collect()
}

/// `bin2hex(random_bytes($n))`.
pub fn random_hex(bytes: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..bytes)
        .map(|_| format!("{:02x}", rng.r#gen::<u8>()))
        .collect()
}

/// `base64_encode(random_bytes($n))`.
pub fn random_base64(bytes: usize) -> String {
    use base64::Engine as _;
    let mut rng = rand::thread_rng();
    let data: Vec<u8> = (0..bytes).map(|_| rng.r#gen::<u8>()).collect();
    base64::engine::general_purpose::STANDARD.encode(data)
}

// --- JSON ------------------------------------------------------------------------------------

/// `json_encode($value)` with PHP's default flags (`/` escaped, non-ASCII as `\uXXXX`).
pub fn json_encode(v: &PhpValue) -> String {
    let mut out = String::new();
    encode_into(v, &mut out);
    out
}

fn encode_into(v: &PhpValue, out: &mut String) {
    match v {
        PhpValue::Null => out.push_str("null"),
        PhpValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        PhpValue::Int(i) => out.push_str(&i.to_string()),
        PhpValue::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                out.push_str(&format!("{f:.1}"));
            } else {
                out.push_str(&format!("{f}"));
            }
        }
        PhpValue::Str(s) => encode_string(s, out),
        PhpValue::Array(items) => {
            if is_list(items) {
                out.push('[');
                for (i, (_, v)) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    encode_into(v, out);
                }
                out.push(']');
            } else {
                out.push('{');
                for (i, (k, v)) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    encode_string(&k.as_string(), out);
                    out.push(':');
                    encode_into(v, out);
                }
                out.push('}');
            }
        }
    }
}

fn encode_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '/' => out.push_str("\\/"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if (c as u32) < 0x80 => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

/// `json_decode($json, true)`: `None` for invalid JSON (PHP returns null).
pub fn json_decode(s: &str) -> Option<PhpValue> {
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    Some(from_json(&v))
}

fn from_json(v: &serde_json::Value) -> PhpValue {
    use serde_json::Value as J;
    match v {
        J::Null => PhpValue::Null,
        J::Bool(b) => PhpValue::Bool(*b),
        J::Number(n) => n
            .as_i64()
            .map_or_else(|| PhpValue::Float(n.as_f64().unwrap_or(0.0)), PhpValue::Int),
        J::String(s) => PhpValue::Str(s.clone()),
        J::Array(a) => PhpValue::list(a.iter().map(from_json)),
        J::Object(o) => {
            let mut items: Arr = Vec::new();
            for (k, v) in o {
                put(&mut items, Key::from_php_string(k), from_json(v));
            }
            PhpValue::Array(items)
        }
    }
}

/// `json_decode($s ?: '[]', true) ?: []` as an array.
pub fn json_array(s: Option<&str>) -> Arr {
    let text = s.filter(|t| !t.is_empty() && *t != "0").unwrap_or("[]");
    match json_decode(text) {
        Some(PhpValue::Array(items)) => items,
        _ => Vec::new(),
    }
}

// --- parse_url / Spatie Url -------------------------------------------------------------------

/// `parse_url($url)`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedUrl {
    pub scheme: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub pass: Option<String>,
    pub path: Option<String>,
    pub query: Option<String>,
    pub fragment: Option<String>,
}

fn control_to_underscore(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_control() { '_' } else { c })
        .collect()
}

/// `parse_url()` (`php_url_parse_ex2`): `None` where PHP returns false.
#[allow(clippy::too_many_lines)]
pub fn parse_url(url: &str) -> Option<ParsedUrl> {
    let b = url.as_bytes();
    let ue = b.len();
    let mut ret = ParsedUrl::default();
    let mut s = 0usize;
    let colon = b.iter().position(|c| *c == b':');
    let first_qf = b
        .iter()
        .position(|c| *c == b'?' || *c == b'#')
        .unwrap_or(ue);

    enum Next {
        Port(usize),
        Host,
        Path,
    }
    let next = 'scheme: {
        if let Some(e) = colon
            && e != 0
        {
            for &c in &b[..e] {
                if !(c.is_ascii_alphanumeric() || c == b'+' || c == b'.' || c == b'-') {
                    if e + 1 < ue && e < first_qf {
                        break 'scheme Next::Port(e);
                    } else if s + 1 < ue && b[s] == b'/' && b[s + 1] == b'/' {
                        s += 2;
                        break 'scheme Next::Host;
                    }
                    break 'scheme Next::Path;
                }
            }
            if e + 1 == ue {
                ret.scheme = Some(control_to_underscore(&url[..e]));
                return Some(ret);
            }
            if b[e + 1] != b'/' {
                let mut p = e + 1;
                while p < ue && b[p].is_ascii_digit() {
                    p += 1;
                }
                if (p == ue || b[p] == b'/') && (p - e) < 7 {
                    break 'scheme Next::Port(e);
                }
                ret.scheme = Some(control_to_underscore(&url[..e]));
                s = e + 1;
                break 'scheme Next::Path;
            }
            ret.scheme = Some(control_to_underscore(&url[..e]));
            if e + 2 < ue && b[e + 2] == b'/' {
                s = e + 3;
                if ret
                    .scheme
                    .as_deref()
                    .is_some_and(|sc| sc.eq_ignore_ascii_case("file"))
                    && e + 3 < ue
                    && b[e + 3] == b'/'
                {
                    if e + 5 < ue && b[e + 5] == b':' {
                        s = e + 4;
                    }
                    break 'scheme Next::Path;
                }
                break 'scheme Next::Host;
            }
            s = e + 1;
            break 'scheme Next::Path;
        } else if let Some(e) = colon {
            break 'scheme Next::Port(e);
        } else if s + 1 < ue && b[s] == b'/' && b[s + 1] == b'/' {
            s += 2;
            break 'scheme Next::Host;
        }
        Next::Path
    };

    let next = match next {
        Next::Port(e) => {
            let p = e + 1;
            let mut pp = p;
            while pp < ue && pp - p < 6 && b[pp].is_ascii_digit() {
                pp += 1;
            }
            if pp - p > 0 && pp - p < 6 && (pp == ue || b[pp] == b'/') {
                let port: u32 = url[p..pp].parse().ok()?;
                if port > 65535 {
                    return None;
                }
                ret.port = Some(port as u16);
                if s + 1 < ue && b[s] == b'/' && b[s + 1] == b'/' {
                    s += 2;
                }
                Next::Host
            } else if p == pp && pp == ue {
                return None;
            } else if s + 1 < ue && b[s] == b'/' && b[s + 1] == b'/' {
                s += 2;
                Next::Host
            } else {
                Next::Path
            }
        }
        other => other,
    };

    if matches!(next, Next::Host) {
        let mut e = s + b[s..]
            .iter()
            .position(|c| matches!(c, b'/' | b'?' | b'#'))
            .unwrap_or(ue - s);
        if e > ue {
            e = ue;
        }
        if let Some(at) = b[s..e].iter().rposition(|c| *c == b'@').map(|i| s + i) {
            if let Some(pp) = b[s..at].iter().position(|c| *c == b':').map(|i| s + i) {
                ret.user = Some(url[s..pp].to_owned());
                ret.pass = Some(url[pp + 1..at].to_owned());
            } else {
                ret.user = Some(url[s..at].to_owned());
            }
            s = at + 1;
        }
        let mut p = if s < ue && b[s] == b'[' && e > 0 && b[e - 1] == b']' {
            None
        } else {
            b[s..e].iter().rposition(|c| *c == b':').map(|i| s + i)
        };
        if let Some(pc) = p {
            if ret.port.is_none() {
                let ps = pc + 1;
                if e - ps > 5 {
                    return None;
                } else if e > ps {
                    let digits: String = url[ps..e]
                        .chars()
                        .take_while(char::is_ascii_digit)
                        .collect();
                    if digits.is_empty() {
                        return None;
                    }
                    let port: u32 = digits.parse().ok()?;
                    if port > 65535 {
                        return None;
                    }
                    if port != 0 {
                        ret.port = Some(port as u16);
                    }
                }
            }
            p = Some(pc);
        }
        let host_end = p.unwrap_or(e);
        if host_end <= s {
            return None;
        }
        ret.host = Some(control_to_underscore(&url[s..host_end]));
        if e == ue {
            return Some(ret);
        }
        s = e;
    }

    // just_path
    let mut e = ue;
    if let Some(p) = b[s..e].iter().position(|c| *c == b'#').map(|i| s + i) {
        ret.fragment = Some(if p + 1 < e {
            control_to_underscore(&url[p + 1..e])
        } else {
            String::new()
        });
        e = p;
    }
    if let Some(p) = b[s..e].iter().position(|c| *c == b'?').map(|i| s + i) {
        ret.query = Some(if p + 1 < e {
            control_to_underscore(&url[p + 1..e])
        } else {
            String::new()
        });
        e = p;
    }
    if s < e || s == ue {
        ret.path = Some(control_to_underscore(&url[s..e]));
    }
    Some(ret)
}

/// Spatie `Url` as the parsers use it: `fromString()` (with the default scheme validation),
/// the getters and `__toString()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpatieUrl {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    pub password: Option<String>,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

impl SpatieUrl {
    /// `Url::fromString($url)`: `Err` with Spatie's message when `parse_url` fails or the scheme
    /// is not http, https, mailto or tel.
    pub fn from_string(url: &str) -> Result<SpatieUrl, String> {
        let Some(p) = parse_url(url) else {
            return Err(format!("The string `{url}` is no valid url."));
        };
        let scheme = p.scheme.unwrap_or_default().to_lowercase();
        if !["", "http", "https", "mailto", "tel"].contains(&scheme.as_str()) {
            return Err(format!(
                "The scheme `{scheme}` isn't valid. It should be either `http`, `https`, `mailto`, `tel`."
            ));
        }
        Ok(SpatieUrl {
            scheme,
            host: p.host.unwrap_or_default(),
            port: p.port,
            user: p.user.unwrap_or_default(),
            password: p.pass,
            path: p.path.unwrap_or_else(|| "/".into()),
            query: p.query.unwrap_or_default(),
            fragment: p.fragment.unwrap_or_default(),
        })
    }

    fn user_info(&self) -> String {
        let mut s = self.user.clone();
        if let Some(p) = &self.password {
            s.push(':');
            s.push_str(p);
        }
        s
    }

    /// `getAuthority()`.
    pub fn authority(&self) -> String {
        let mut a = self.host.clone();
        let ui = self.user_info();
        if !ui.is_empty() && ui != "0" {
            a = format!("{ui}@{a}");
        }
        if let Some(p) = self.port {
            a.push_str(&format!(":{p}"));
        }
        a
    }
}

impl std::fmt::Display for SpatieUrl {
    /// `__toString()`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut url = String::new();
        let mail = ["mailto", "tel"].contains(&self.scheme.as_str());
        if !self.scheme.is_empty() && !mail {
            url.push_str(&self.scheme);
            url.push_str("://");
        }
        if mail && !self.path.is_empty() {
            url.push_str(&self.scheme);
            url.push(':');
        }
        let authority = self.authority();
        if self.scheme.is_empty() && !authority.is_empty() {
            url.push_str("//");
        }
        url.push_str(&authority);
        if self.path != "/" {
            if mail {
                url.push_str(self.path.trim_start_matches('/'));
            } else {
                url.push_str(&self.path);
            }
        }
        if !self.query.is_empty() {
            url.push('?');
            url.push_str(&self.query);
        }
        if !self.fragment.is_empty() {
            url.push('#');
            url.push_str(&self.fragment);
        }
        f.write_str(&url)
    }
}

/// `version_compare($a, $b)` as -1, 0 or 1.
pub fn version_compare(a: &str, b: &str) -> i32 {
    fn canon(v: &str) -> Vec<String> {
        let mut out = String::new();
        let mut prev: Option<char> = None;
        for c in v.chars() {
            let c = if matches!(c, '-' | '_' | '+') { '.' } else { c };
            if let Some(p) = prev
                && c != '.'
                && p != '.'
                && (p.is_ascii_digit() != c.is_ascii_digit())
            {
                out.push('.');
            }
            out.push(c);
            prev = Some(c);
        }
        out.split('.')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }
    fn order(s: &str) -> i32 {
        if s.starts_with("dev") {
            0
        } else if s.starts_with("alpha") || s.starts_with('a') {
            1
        } else if s.starts_with("beta") || s.starts_with('b') {
            2
        } else if s.starts_with("RC") || s.starts_with("rc") {
            3
        } else if s == "#" {
            4
        } else if s.starts_with("pl") || s.starts_with('p') {
            5
        } else {
            -6
        }
    }
    fn cmp_part(x: &str, y: &str) -> i32 {
        let xd = x.chars().all(|c| c.is_ascii_digit());
        let yd = y.chars().all(|c| c.is_ascii_digit());
        match (xd, yd) {
            (true, true) => {
                let (a, b) = (x.parse::<u64>().unwrap_or(0), y.parse::<u64>().unwrap_or(0));
                a.cmp(&b) as i32
            }
            (false, false) => order(x).cmp(&order(y)) as i32,
            (true, false) => order("#").cmp(&order(y)) as i32,
            (false, true) => order(x).cmp(&order("#")) as i32,
        }
    }
    let (x, y) = (canon(a), canon(b));
    let n = x.len().max(y.len());
    for i in 0..n {
        match (x.get(i), y.get(i)) {
            (Some(p), Some(q)) => {
                let c = cmp_part(p, q);
                if c != 0 {
                    return c;
                }
            }
            (Some(p), None) => {
                return if p.chars().all(|c| c.is_ascii_digit()) {
                    1
                } else {
                    cmp_part(p, "#")
                };
            }
            (None, Some(q)) => {
                return if q.chars().all(|c| c.is_ascii_digit()) {
                    -1
                } else {
                    cmp_part("#", q)
                };
            }
            (None, None) => break,
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_url_matches_php() {
        let p = parse_url("http://web-x.10.0.0.1.sslip.io:3000/blog").unwrap_or_default();
        assert_eq!(p.scheme.as_deref(), Some("http"));
        assert_eq!(p.host.as_deref(), Some("web-x.10.0.0.1.sslip.io"));
        assert_eq!(p.port, Some(3000));
        assert_eq!(p.path.as_deref(), Some("/blog"));
        let p = parse_url("uuid.10.0.0.1.sslip.io:3000").unwrap_or_default();
        assert_eq!(p.host.as_deref(), Some("uuid.10.0.0.1.sslip.io"));
        assert_eq!(p.port, Some(3000));
        let p = parse_url("uuid.10.0.0.1.sslip.io").unwrap_or_default();
        assert_eq!(p.path.as_deref(), Some("uuid.10.0.0.1.sslip.io"));
        assert_eq!(p.host, None);
        assert!(parse_url("http://").is_none());
        assert_eq!(
            SpatieUrl::from_string("https://a.example.com:8080/x").map(|u| u.to_string()),
            Ok("https://a.example.com:8080/x".into())
        );
    }

    #[test]
    fn versions_compare_like_php() {
        assert_eq!(
            version_compare("4.5.1-dev.b5823c777", "4.0.0-beta.420.7"),
            1
        );
        assert_eq!(version_compare("4.0.0-beta.420.6", "4.0.0-beta.420.7"), -1);
        assert_eq!(version_compare("4.0.0", "4.0.0-beta.1"), 1);
        assert_eq!(version_compare("4.5.3", "4.0.0-beta.420.7"), 1);
    }

    /// Release builds report `COOLIFY_VERSION=4.5.3-rs.N`. Expected values are PHP 8's
    /// `version_compare` output: `rs` is not a special form, so it sorts below `dev`.
    #[test]
    fn release_versions_compare_like_php() {
        assert_eq!(version_compare("4.5.3-rs.1", "4.0.0-beta.420.7"), 1);
        assert_eq!(version_compare("4.5.3-rs.2", "4.5.3-rs.1"), 1);
        assert_eq!(version_compare("4.5.3-rs.10", "4.5.3-rs.9"), 1);
        assert_eq!(version_compare("4.5.4-rs.1", "4.5.3-rs.9"), 1);
        assert_eq!(version_compare("4.5.3-rs.1", "4.5.2"), 1);
        assert_eq!(version_compare("4.5.3-rs.1", "4.5.3-rs.1"), 0);
        assert_eq!(version_compare("4.5.3-rs.1", "4.5.3"), -1);
        assert_eq!(version_compare("4.5.3", "4.5.3-rs.1"), 1);
    }

    #[test]
    fn floats_print_like_php() {
        assert_eq!(float_to_string(1.5), "1.5");
        assert_eq!(float_to_string(0.1 + 0.2), "0.3");
        assert_eq!(float_to_string(2.0), "2");
        assert_eq!(float_to_string(1e20), "1.0E+20");
    }
}
