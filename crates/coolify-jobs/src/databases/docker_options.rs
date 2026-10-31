//! `convertDockerRunToCompose` and `generateCustomDockerRunOptionsForDatabases`
//! (bootstrap/helpers/docker.php).

use std::sync::LazyLock;

use coolify_compat::yaml::{Key, PhpValue};
use fancy_regex::Regex as FancyRegex;
use regex::Regex;

static OPTION: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(
        r#"(--\w+(?:-\w+)*)(?:\s+|=)?((?!-(?!\d))(?:"(?:\\.|[^"])*"|'[^']*'|[^\s"']|["'])+)?"#,
    )
    .unwrap_or_else(|_| unreachable!())
});
static QUOTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""((?:\\.|[^"])*)"|'([^']*)'"#).unwrap_or_else(|_| unreachable!())
});
static SHELL_ESCAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\\(["$`\\])"#).unwrap_or_else(|_| unreachable!()));
static ENTRYPOINT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"--entrypoint(?:=|\s+)("(?:\\.|[^"])*"|'(?:\\.|[^'])*'|[^\s]+)"#)
        .unwrap_or_else(|_| unreachable!())
});
static DEVICE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^device=([0-9A-Za-z\-,]+)$").unwrap_or_else(|_| unreachable!()));
static TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:[^\s'"\\]+|\\.|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*')+"#)
        .unwrap_or_else(|_| unreachable!())
});
static SHORT_FLAGS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^-[dit]+$").unwrap_or_else(|_| unreachable!()));

/// `DOCKER_RUN_OPTION_MAPPING`.
fn mapping(option: &str) -> Option<&'static str> {
    Some(match option {
        "--add-host" => "extra_hosts",
        "--cap-add" => "cap_add",
        "--cap-drop" => "cap_drop",
        "--security-opt" => "security_opt",
        "--sysctl" => "sysctls",
        "--device" => "devices",
        "--init" => "init",
        "--ulimit" => "ulimits",
        "--privileged" => "privileged",
        "--ip" => "ip",
        "--ip6" => "ip6",
        "--shm-size" => "shm_size",
        "--dns" => "dns",
        "--gpus" => "gpus",
        "--hostname" => "hostname",
        "--entrypoint" => "entrypoint",
        "--pids-limit" => "pids_limit",
        "--read-only" => "read_only",
        "--runtime" => "runtime",
        "--tmpfs" => "tmpfs",
        _ => return None,
    })
}

#[derive(Debug, Clone)]
enum Parsed {
    Flag(bool),
    Values(Vec<String>),
    Gpu(PhpValue),
}

fn unquote(value: &str) -> String {
    QUOTED
        .replace_all(value, |c: &regex::Captures<'_>| match c.get(2) {
            Some(single) => single.as_str().to_owned(),
            None => SHELL_ESCAPE.replace_all(&c[1], "$1").into_owned(),
        })
        .into_owned()
}

/// `convertDockerRunToCompose`: the Compose keys, in the order upstream's collection gets them.
pub fn convert_docker_run_to_compose(options: Option<&str>) -> PhpValue {
    let text = options.unwrap_or_default();
    let mut parsed: Vec<(String, Parsed)> = Vec::new();
    let set = |parsed: &mut Vec<(String, Parsed)>, k: &str, v: Parsed| {
        if let Some(slot) = parsed.iter_mut().find(|(o, _)| o == k) {
            slot.1 = v;
        } else {
            parsed.push((k.to_owned(), v));
        }
    };
    for caps in OPTION.captures_iter(text).flatten() {
        let option = caps.get(1).map_or("", |m| m.as_str()).to_owned();
        let whole = caps.get(0).map_or("", |m| m.as_str());
        let value = unquote(caps.get(2).map_or("", |m| m.as_str()));
        if option == "--gpus" {
            let gpus = value.trim_matches('"');
            let mut payload = PhpValue::map([
                ("driver", PhpValue::from("nvidia")),
                ("capabilities", PhpValue::strings(["gpu"])),
            ]);
            if let Some(ids) = DEVICE.captures(gpus) {
                payload.set("device_ids", PhpValue::strings(ids[1].split(',')));
            } else if !gpus.is_empty() && gpus.bytes().all(|b| b.is_ascii_digit()) {
                payload.set("count", PhpValue::Int(gpus.parse().unwrap_or(0)));
            }
            set(&mut parsed, &option, Parsed::Gpu(payload));
            continue;
        }
        if option == "--entrypoint" {
            if let Some(m) = ENTRYPOINT.captures(text) {
                let raw = &m[1];
                let value = if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
                    SHELL_ESCAPE
                        .replace_all(&raw[1..raw.len() - 1], "$1")
                        .into_owned()
                } else if raw.starts_with('\'') && raw.ends_with('\'') && raw.len() >= 2 {
                    raw[1..raw.len() - 1].to_owned()
                } else {
                    raw.to_owned()
                };
                if !value.trim().is_empty() {
                    add_value(&mut parsed, &option, value);
                }
            }
            continue;
        }
        if ["--init", "--privileged", "--read-only"].contains(&option.as_str()) {
            let inline = whole[option.len()..].starts_with('=');
            set(
                &mut parsed,
                &option,
                Parsed::Flag(!(inline && value.eq_ignore_ascii_case("false"))),
            );
            continue;
        }
        if value.trim().is_empty() {
            continue;
        }
        add_value(&mut parsed, &option, value);
    }

    let mut compose = PhpValue::Array(Vec::new());
    for (option, value) in &parsed {
        let Some(key) = mapping(option) else { continue };
        match (option.as_str(), value) {
            ("--ulimit", Parsed::Values(values)) => {
                let mut ulimits = PhpValue::Array(Vec::new());
                for ulimit in values {
                    let Some((kind, limit)) = ulimit.split_once('=') else {
                        continue;
                    };
                    let parts: Vec<&str> = limit.split(':').collect();
                    if parts.len() == 2 {
                        ulimits.set(
                            kind,
                            PhpValue::map([
                                ("soft", PhpValue::from(parts[0])),
                                ("hard", PhpValue::from(parts[1])),
                            ]),
                        );
                    } else {
                        ulimits.set(kind, PhpValue::from(limit));
                    }
                }
                if !matches!(&ulimits, PhpValue::Array(u) if u.is_empty()) {
                    compose.set(key, ulimits);
                }
            }
            (
                "--shm-size" | "--hostname" | "--runtime" | "--entrypoint",
                Parsed::Values(values),
            ) => {
                if let Some(first) = values.first().filter(|f| php_not_empty(f.trim())) {
                    compose.set(key, PhpValue::from(first.as_str()));
                }
            }
            ("--pids-limit", Parsed::Values(values)) => {
                if let Some(first) = values
                    .first()
                    .filter(|f| coolify_compat::yaml::php_is_numeric(f))
                {
                    compose.set(
                        key,
                        PhpValue::Int(first.trim().parse::<f64>().unwrap_or(0.0) as i64),
                    );
                }
            }
            ("--gpus", Parsed::Gpu(payload)) => {
                compose.set(
                    "deploy",
                    PhpValue::map([(
                        "resources",
                        PhpValue::map([(
                            "reservations",
                            PhpValue::map([("devices", PhpValue::list([payload.clone()]))]),
                        )]),
                    )]),
                );
            }
            (_, Parsed::Flag(b)) => compose.set(key, PhpValue::Bool(*b)),
            (_, Parsed::Values(values)) => {
                compose.set(key, php_unique_list(values));
            }
            _ => {}
        }
    }
    for token in TOKEN.find_iter(text).map(|m| m.as_str()) {
        if token == "--interactive" {
            compose.set("stdin_open", PhpValue::Bool(true));
        } else if token == "--tty" {
            compose.set("tty", PhpValue::Bool(true));
        } else if SHORT_FLAGS.is_match(token) {
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

/// PHP `! empty($v)` for a string.
fn php_not_empty(v: &str) -> bool {
    !v.is_empty() && v != "0"
}

fn add_value(parsed: &mut Vec<(String, Parsed)>, option: &str, value: String) {
    if let Some((_, Parsed::Values(values))) = parsed.iter_mut().find(|(o, _)| o == option) {
        if !values.contains(&value) {
            values.push(value);
        }
    } else {
        parsed.push((option.to_owned(), Parsed::Values(vec![value])));
    }
}

fn php_unique_list(values: &[String]) -> PhpValue {
    PhpValue::strings(values.iter().cloned())
}

/// PHP `array_merge_recursive`: a string key in both sides merges into a list or a nested merge;
/// integer keys append.
fn merge_recursive(base: PhpValue, other: PhpValue) -> PhpValue {
    let as_items = |v: PhpValue| match v {
        PhpValue::Array(items) => items,
        scalar => vec![(Key::Int(0), scalar)],
    };
    let mut out = as_items(base);
    for (k, v) in as_items(other) {
        match k {
            Key::Int(_) => {
                let next = out
                    .iter()
                    .filter_map(|(k, _)| match k {
                        Key::Int(i) => Some(*i + 1),
                        Key::Str(_) => None,
                    })
                    .max()
                    .unwrap_or(0);
                out.push((Key::Int(next), v));
            }
            Key::Str(name) => {
                if let Some(slot) = out
                    .iter_mut()
                    .find(|(k, _)| matches!(k, Key::Str(s) if *s == name))
                {
                    let existing = std::mem::replace(&mut slot.1, PhpValue::Null);
                    slot.1 = merge_recursive(existing, v);
                } else {
                    out.push((Key::Str(name), v));
                }
            }
        }
    }
    PhpValue::Array(out)
}

/// `generateCustomDockerRunOptionsForDatabases` on the service of a database.
pub fn apply_to_service(service: &mut PhpValue, options: Option<&str>, network: &str) {
    let mut run = convert_docker_run_to_compose(options);
    let first = |v: Option<&mut PhpValue>| match v {
        Some(PhpValue::Array(items)) => items.first().and_then(|(_, f)| match f {
            PhpValue::Str(s) => Some(s.clone()),
            _ => None,
        }),
        _ => None,
    };
    let ipv4 = first(run.get_mut("ip"));
    let ipv6 = first(run.get_mut("ip6"));
    run.remove("ip");
    run.remove("ip6");
    if ipv4.is_some() || ipv6.is_some() {
        service.remove("networks");
        let mut block = PhpValue::Array(Vec::new());
        if let Some(ip) = ipv4 {
            block.set("ipv4_address", PhpValue::from(ip));
        }
        if let Some(ip) = ipv6 {
            block.set("ipv6_address", PhpValue::from(ip));
        }
        service.set("networks", PhpValue::map([(network, block)]));
    }
    // A custom ulimit replaces the default of the same type.
    if let Some(PhpValue::Array(custom)) = run.get_mut("ulimits") {
        let types: Vec<String> = custom.iter().map(|(k, _)| k.as_string()).collect();
        if let Some(existing) = service.get_mut("ulimits") {
            for t in types {
                existing.remove(&t);
            }
        }
    }
    let current = std::mem::replace(service, PhpValue::Null);
    *service = merge_recursive(current, run);
}

#[cfg(test)]
mod tests {
    use super::*;
    use coolify_compat::yaml;

    fn dump(options: &str) -> String {
        yaml::dump(&convert_docker_run_to_compose(Some(options)), 10)
    }

    #[test]
    fn maps_common_options() {
        let out =
            dump("--cap-add SYS_ADMIN --cap-add=NET_ADMIN --shm-size 1g --init --pids-limit 50");
        assert!(
            out.contains("cap_add:\n    - SYS_ADMIN\n    - NET_ADMIN\n"),
            "{out}"
        );
        assert!(out.contains("shm_size: 1g"), "{out}");
        assert!(out.contains("init: true"), "{out}");
        assert!(out.contains("pids_limit: 50"), "{out}");
    }

    #[test]
    fn quoted_values_and_short_flags() {
        let out = dump(r#"--sysctl "net.core.somaxconn=1024" -it"#);
        assert!(out.contains("net.core.somaxconn=1024"), "{out}");
        assert!(out.contains("stdin_open: true"), "{out}");
        assert!(out.contains("tty: true"), "{out}");
    }

    #[test]
    fn ulimits_with_soft_and_hard() {
        let mut service = PhpValue::map([("image", PhpValue::from("x"))]);
        apply_to_service(&mut service, Some("--ulimit nofile=1024:2048"), "coolify");
        let out = yaml::dump(&service, 10);
        assert!(
            out.contains("nofile: { soft: '1024', hard: '2048' }") || out.contains("soft: '1024'"),
            "{out}"
        );
    }

    #[test]
    fn static_ip_replaces_networks() {
        let mut service = PhpValue::map([("networks", PhpValue::strings(["coolify"]))]);
        apply_to_service(&mut service, Some("--ip 10.0.0.5"), "coolify");
        let out = yaml::dump(&service, 10);
        assert!(out.contains("ipv4_address: 10.0.0.5"), "{out}");
    }
}
