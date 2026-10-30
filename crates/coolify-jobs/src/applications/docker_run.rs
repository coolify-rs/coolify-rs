//! Custom Docker run options: `convertDockerRunToCompose()`, `unsupportedDockerRunOptions()` and
//! `seccompProfileMountFlags()` (`bootstrap/helpers/docker.php`), and the GPU settings of the
//! Compose file (`ApplicationSetting::gpuDeviceReservation()`).

use std::sync::LazyLock;

use coolify_compat::yaml::{Key, PhpValue, php_is_numeric};
use coolify_remote::shell::quote;
use regex::Regex;

/// `application_settings` as the job holds it.
type Settings = serde_json::Map<String, serde_json::Value>;

/// `DOCKER_RUN_OPTION_MAPPING`.
const OPTION_MAPPING: [(&str, &str); 20] = [
    ("--add-host", "extra_hosts"),
    ("--cap-add", "cap_add"),
    ("--cap-drop", "cap_drop"),
    ("--security-opt", "security_opt"),
    ("--sysctl", "sysctls"),
    ("--device", "devices"),
    ("--init", "init"),
    ("--ulimit", "ulimits"),
    ("--privileged", "privileged"),
    ("--ip", "ip"),
    ("--ip6", "ip6"),
    ("--shm-size", "shm_size"),
    ("--dns", "dns"),
    ("--gpus", "gpus"),
    ("--hostname", "hostname"),
    ("--entrypoint", "entrypoint"),
    ("--pids-limit", "pids_limit"),
    ("--read-only", "read_only"),
    ("--runtime", "runtime"),
    ("--tmpfs", "tmpfs"),
];

fn mapped(option: &str) -> Option<&'static str> {
    OPTION_MAPPING
        .iter()
        .find(|(k, _)| *k == option)
        .map(|(_, v)| *v)
}

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|_| unreachable!())
}

/// PHP `trim()` with its default characters.
fn php_trim(s: &str) -> &str {
    s.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B'])
}

/// PHP `empty()` for a string.
fn php_empty(s: &str) -> bool {
    s.is_empty() || s == "0"
}

/// Unescapes `\"`, `\$`, `` \` `` and `\\`.
fn unescape_double_quoted(s: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| regex(r#"\\(["$`\\])"#));
    RE.replace_all(s, "$1").into_owned()
}

/// The value of an option in the `$options` array.
enum Opt {
    List(Vec<String>),
    Flag(bool),
    Gpus(PhpValue),
}

fn append_unique(options: &mut Vec<(String, Opt)>, option: &str, value: String) {
    match options.iter_mut().find(|(k, _)| k == option) {
        Some((_, Opt::List(values))) => {
            if !values.contains(&value) {
                values.push(value);
            }
        }
        _ => options.push((option.to_owned(), Opt::List(vec![value]))),
    }
}

fn set(options: &mut Vec<(String, Opt)>, option: &str, value: Opt) {
    match options.iter_mut().find(|(k, _)| k == option) {
        Some(slot) => slot.1 = value,
        None => options.push((option.to_owned(), value)),
    }
}

/// `(int) $numericString`.
fn php_int(s: &str) -> i64 {
    let t = s.trim();
    t.parse::<i64>()
        .ok()
        .or_else(|| t.parse::<f64>().ok().map(|f| f as i64))
        .unwrap_or(0)
}

/// `--entrypoint`: the first occurrence in the whole text, with its quotes removed.
fn entrypoint_value(text: &str) -> Option<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        regex(
            r#"--entrypoint(?:=|[ \t\n\r\x0B\x0C]+)("(?:\\.|[^"])*"|'(?:\\.|[^'])*'|[^ \t\n\r\x0B\x0C]+)"#,
        )
    });
    let raw = RE.captures(text)?.get(1)?.as_str();
    let inner = || {
        if raw.len() >= 2 {
            &raw[1..raw.len() - 1]
        } else {
            ""
        }
    };
    Some(if raw.starts_with('"') && raw.ends_with('"') {
        unescape_double_quoted(inner())
    } else if raw.starts_with('\'') && raw.ends_with('\'') {
        inner().to_owned()
    } else {
        raw.to_owned()
    })
}

/// The payload of `--gpus`: `all`, a count or `"device=0,1"`.
fn gpus_payload(value: &str) -> PhpValue {
    static DEVICE: LazyLock<Regex> = LazyLock::new(|| regex(r"^device=([0-9A-Za-z\-,]+)\n?$"));
    let gpus = value.trim_matches('"');
    let mut payload = PhpValue::map([
        ("driver", PhpValue::from("nvidia")),
        ("capabilities", PhpValue::strings(["gpu"])),
    ]);
    if let Some(c) = DEVICE.captures(gpus) {
        payload.set("device_ids", PhpValue::strings(c[1].split(',')));
    } else if !gpus.is_empty() && gpus.bytes().all(|b| b.is_ascii_digit()) {
        payload.set("count", PhpValue::Int(php_int(gpus)));
    }
    payload
}

/// `convertDockerRunToCompose($custom_docker_run_options)`.
pub fn convert_docker_run_to_compose(custom: Option<&str>) -> PhpValue {
    // `(--\w+(?:-\w+)*)(?:\s+|=)?((?!-(?!\d))(?:"(?:\\.|[^"])*"|'[^']*'|[^\s"']|["'])+)?`
    static OPTION: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
        fancy_regex::Regex::new(
            r#"(--[A-Za-z0-9_]+(?:-[A-Za-z0-9_]+)*)(?:[ \t\n\r\x0B\x0C]+|=)?((?!-(?![0-9]))(?:"(?:\\.|[^"])*"|'[^']*'|[^ \t\n\r\x0B\x0C"']|["'])+)?"#,
        )
        .unwrap_or_else(|_| unreachable!())
    });
    static QUOTED: LazyLock<Regex> = LazyLock::new(|| regex(r#""((?:\\.|[^"])*)"|'([^']*)'"#));
    static TOKEN: LazyLock<Regex> = LazyLock::new(|| {
        regex(r#"(?:[^ \t\n\r\x0B\x0C'"\\]+|\\.|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*')+"#)
    });
    static SHORT: LazyLock<Regex> = LazyLock::new(|| regex(r"^-[dit]+\n?$"));

    let text = custom.unwrap_or_default();
    let mut options: Vec<(String, Opt)> = Vec::new();
    for m in OPTION.captures_iter(text).filter_map(Result::ok) {
        let whole = m.get(0).map_or("", |x| x.as_str());
        let option = m.get(1).map_or("", |x| x.as_str());
        let value = QUOTED
            .replace_all(
                m.get(2).map_or("", |x| x.as_str()),
                |q: &regex::Captures<'_>| match q.get(2) {
                    Some(single) => single.as_str().to_owned(),
                    None => unescape_double_quoted(&q[1]),
                },
            )
            .into_owned();
        match option {
            "--gpus" => set(&mut options, option, Opt::Gpus(gpus_payload(&value))),
            "--entrypoint" => {
                if let Some(v) = entrypoint_value(text)
                    && !php_empty(&v)
                    && !php_trim(&v).is_empty()
                {
                    append_unique(&mut options, option, v);
                }
            }
            "--init" | "--privileged" | "--read-only" => {
                let inline = whole[option.len()..].starts_with('=');
                set(
                    &mut options,
                    option,
                    Opt::Flag(!(inline && value.to_lowercase() == "false")),
                );
            }
            _ if php_trim(&value).is_empty() => {}
            _ => append_unique(&mut options, option, value),
        }
    }

    let mut compose = PhpValue::list([]);
    for (option, value) in options {
        let Some(key) = mapped(&option) else { continue };
        match (option.as_str(), value) {
            ("--ulimit", Opt::List(values)) => {
                let mut ulimits = PhpValue::list([]);
                for ulimit in values {
                    let Some((kind, limit)) = ulimit.split_once('=') else {
                        continue;
                    };
                    let parts: Vec<&str> = limit.split(':').collect();
                    let entry = if parts.len() == 2 {
                        PhpValue::map([
                            ("soft", PhpValue::from(parts[0])),
                            ("hard", PhpValue::from(parts[1])),
                        ])
                    } else {
                        PhpValue::from(limit)
                    };
                    put(&mut ulimits, Key::from_php_string(kind), entry);
                }
                if !matches!(&ulimits, PhpValue::Array(u) if u.is_empty()) {
                    compose.set(key, ulimits);
                }
            }
            ("--shm-size" | "--hostname" | "--runtime" | "--entrypoint", Opt::List(values)) => {
                if let Some(first) = values.first()
                    && !php_empty(php_trim(first))
                {
                    compose.set(key, PhpValue::from(first.as_str()));
                }
            }
            ("--pids-limit", Opt::List(values)) => {
                if let Some(first) = values.first()
                    && php_is_numeric(first)
                {
                    compose.set(key, PhpValue::Int(php_int(first)));
                }
            }
            ("--gpus", Opt::Gpus(payload)) => {
                compose.set(
                    "deploy",
                    PhpValue::map([(
                        "resources",
                        PhpValue::map([(
                            "reservations",
                            PhpValue::map([("devices", PhpValue::list([payload]))]),
                        )]),
                    )]),
                );
            }
            (_, Opt::List(values)) => compose.set(key, PhpValue::strings(values)),
            (_, Opt::Flag(flag)) => compose.set(key, PhpValue::Bool(flag)),
            (_, Opt::Gpus(_)) => {}
        }
    }

    for token in TOKEN.find_iter(text).map(|t| t.as_str()) {
        if token == "--interactive" {
            compose.set("stdin_open", PhpValue::Bool(true));
        } else if token == "--tty" {
            compose.set("tty", PhpValue::Bool(true));
        } else if SHORT.is_match(token) {
            if token.contains('i') {
                compose.set("stdin_open", PhpValue::Bool(true));
            }
            if token.contains('t') {
                compose.set("tty", PhpValue::Bool(true));
            }
        }
    }
    compose
}

/// `$collection->put($key, $value)`.
fn put(target: &mut PhpValue, key: Key, value: PhpValue) {
    if let PhpValue::Array(items) = target {
        match items.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => items.push((key, value)),
        }
    }
}

/// `unsupportedDockerRunOptions($custom_docker_run_options)`.
pub fn unsupported_docker_run_options(custom: Option<&str>) -> Vec<String> {
    static WORD: LazyLock<Regex> =
        LazyLock::new(|| regex(r#"(?:"(?:\\.|[^"])*"|'[^']*'|[^ \t\n\r\x0B\x0C"']|["'])+"#));
    static NEGATIVE: LazyLock<Regex> = LazyLock::new(|| regex(r"^-[0-9]"));
    static MAPPED_ELSEWHERE: LazyLock<Regex> =
        LazyLock::new(|| regex(r"^(-[dit]+|--interactive|--tty)\n?$"));
    let mut unsupported: Vec<String> = Vec::new();
    for word in WORD
        .find_iter(custom.unwrap_or_default())
        .map(|w| w.as_str())
    {
        if !word.starts_with('-') || NEGATIVE.is_match(word) || MAPPED_ELSEWHERE.is_match(word) {
            continue;
        }
        let token = word.split('=').next().unwrap_or_default();
        let option = if word.starts_with("--") {
            format!("--{}", token.trim_start_matches('-'))
        } else {
            token.to_owned()
        };
        if mapped(&option).is_none() && !unsupported.contains(&option) {
            unsupported.push(option);
        }
    }
    unsupported
}

/// `seccompProfileMountFlags($custom_docker_run_options)`.
pub fn seccomp_profile_mount_flags(custom: Option<&str>) -> String {
    static PROFILE: LazyLock<Regex> =
        LazyLock::new(|| regex(r"^seccomp[=:](/[A-Za-z0-9._/-]+)\n?$"));
    let mut compose = convert_docker_run_to_compose(custom);
    let mut flags: Vec<String> = Vec::new();
    if let Some(PhpValue::Array(opts)) = compose.get_mut("security_opt") {
        for (_, opt) in opts.iter() {
            let PhpValue::Str(opt) = opt else { continue };
            if let Some(c) = PROFILE.captures(opt)
                && !c[1].contains("..")
            {
                let flag = format!(
                    "--mount {}",
                    quote(&format!(
                        "type=bind,source={p},target={p},readonly",
                        p = &c[1]
                    ))
                );
                if !flags.contains(&flag) {
                    flags.push(flag);
                }
            }
        }
    }
    flags.join(" ")
}

/// `array_merge_recursive($dest, $src)`.
pub fn array_merge_recursive(dest: &mut PhpValue, src: PhpValue) {
    let PhpValue::Array(src) = src else { return };
    let PhpValue::Array(items) = dest else { return };
    for (key, value) in src {
        let existing = match &key {
            Key::Str(_) => items.iter().position(|(k, _)| *k == key),
            Key::Int(_) => None,
        };
        match (key, existing) {
            (Key::Str(_), Some(i)) => {
                let slot = &mut items[i].1;
                *slot = match std::mem::replace(slot, PhpValue::Null) {
                    array @ PhpValue::Array(_) => array,
                    PhpValue::Null => PhpValue::list([PhpValue::Null]),
                    scalar => PhpValue::list([scalar]),
                };
                if let PhpValue::Array(_) = value {
                    array_merge_recursive(slot, value);
                } else {
                    slot.push(value);
                }
            }
            (Key::Str(s), None) => items.push((Key::Str(s), value)),
            (Key::Int(_), _) => {
                let next = items
                    .iter()
                    .filter_map(|(k, _)| match k {
                        Key::Int(i) => Some(*i + 1),
                        Key::Str(_) => None,
                    })
                    .max()
                    .unwrap_or(0);
                items.push((Key::Int(next), value));
            }
        }
    }
}

/// A scalar `application_settings` value as PHP's `(string)` cast prints it.
fn setting_text(settings: &Settings, key: &str) -> String {
    match settings.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// `ApplicationSetting::parseGpuOptions($options)`: `None` when the value is not a flat mapping.
pub fn parse_gpu_options(options: Option<&str>) -> Option<Vec<(String, String)>> {
    let options = php_trim(options.unwrap_or_default());
    if options.is_empty() {
        return Some(Vec::new());
    }
    if let Ok(PhpValue::Array(items)) = coolify_compat::yaml_parse::parse(options, 50)
        && !items.is_empty()
        && items
            .iter()
            .enumerate()
            .any(|(i, (k, _))| *k != Key::Int(i as i64))
    {
        let mut mapping = Vec::new();
        for (key, value) in items {
            let text = match value {
                PhpValue::Bool(b) => if b { "true" } else { "false" }.to_owned(),
                PhpValue::Int(i) => i.to_string(),
                PhpValue::Float(f) => f.to_string(),
                PhpValue::Str(s) => s,
                PhpValue::Null | PhpValue::Array(_) => return None,
            };
            mapping.push((key.as_string(), text));
        }
        return Some(mapping);
    }
    static LINE: LazyLock<Regex> =
        LazyLock::new(|| regex(r"\r\n|\n|\x0B|\x0C|\r|\x{85}|\x{2028}|\x{2029}"));
    let mut mapping: Vec<(String, String)> = Vec::new();
    for pair in LINE.split(options) {
        let pair = php_trim(pair);
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=')?;
        let key = php_trim(key);
        if key.is_empty() {
            return None;
        }
        match mapping.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => php_trim(value).clone_into(&mut slot.1),
            None => mapping.push((key.to_owned(), php_trim(value).to_owned())),
        }
    }
    Some(mapping)
}

/// `ApplicationSetting::gpuDeviceReservation()`.
pub fn gpu_device_reservation(settings: &Settings) -> PhpValue {
    let mut device = PhpValue::map([("capabilities", PhpValue::strings(["gpu"]))]);
    let driver = setting_text(settings, "gpu_driver");
    if !php_trim(&driver).is_empty() {
        device.set("driver", PhpValue::from(php_trim(&driver)));
    }
    let count = setting_text(settings, "gpu_count");
    let count = php_trim(&count);
    let ids = setting_text(settings, "gpu_device_ids");
    let ids: Vec<&str> = ids
        .split(',')
        .map(php_trim)
        .filter(|i| !i.is_empty())
        .collect();
    if !count.is_empty() {
        device.set(
            "count",
            if count == "all" {
                PhpValue::from("all")
            } else {
                PhpValue::Int(php_int(count))
            },
        );
    } else if !ids.is_empty() {
        device.set("device_ids", PhpValue::strings(ids));
    }
    let options = settings
        .get("gpu_options")
        .and_then(serde_json::Value::as_str);
    if let Some(options) = parse_gpu_options(options).filter(|o| !o.is_empty()) {
        device.set(
            "options",
            PhpValue::map(options.into_iter().map(|(k, v)| (k, PhpValue::Str(v)))),
        );
    }
    device
}

#[cfg(test)]
mod tests {
    use super::*;
    use coolify_compat::yaml::dump;

    /// `compat/fixtures`-style cases generated by running upstream's helpers in the reference
    /// container: `[options, compose yaml, unsupported, seccomp flags]`.
    #[test]
    fn matches_upstream() {
        let cases: Vec<(String, String, Vec<String>, String)> =
            serde_json::from_str(include_str!("../../tests/fixtures/docker_run_options.json"))
                .unwrap();
        assert!(cases.len() > 40);
        for (options, compose, unsupported, seccomp) in cases {
            let converted = convert_docker_run_to_compose(Some(&options));
            assert_eq!(dump(&converted, 10), compose, "convert: {options}");
            assert_eq!(
                unsupported_docker_run_options(Some(&options)),
                unsupported,
                "unsupported: {options}"
            );
            assert_eq!(
                seccomp_profile_mount_flags(Some(&options)),
                seccomp,
                "seccomp: {options}"
            );
        }
    }

    #[test]
    fn merge_recursive() {
        let mut a = PhpValue::map([
            ("x", PhpValue::from("1")),
            ("deploy", PhpValue::map([("r", PhpValue::strings(["a"]))])),
        ]);
        array_merge_recursive(
            &mut a,
            PhpValue::map([
                ("x", PhpValue::from("2")),
                ("y", PhpValue::from("3")),
                ("deploy", PhpValue::map([("r", PhpValue::strings(["b"]))])),
            ]),
        );
        assert_eq!(
            a,
            PhpValue::map([
                ("x", PhpValue::strings(["1", "2"])),
                (
                    "deploy",
                    PhpValue::map([("r", PhpValue::strings(["a", "b"]))]),
                ),
                ("y", PhpValue::from("3")),
            ])
        );
    }

    #[test]
    fn gpu_options() {
        assert_eq!(parse_gpu_options(Some("")), Some(vec![]));
        assert_eq!(
            parse_gpu_options(Some("a=1\nb = x")),
            Some(vec![("a".into(), "1".into()), ("b".into(), "x".into())])
        );
        assert_eq!(parse_gpu_options(Some("nonsense")), None);
        assert_eq!(
            parse_gpu_options(Some("a: true\nb: 2")),
            Some(vec![("a".into(), "true".into()), ("b".into(), "2".into())])
        );
    }
}
