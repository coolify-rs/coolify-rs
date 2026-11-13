//! Validation shared by the Compose parsers (bootstrap/helpers/parsers.php and shared.php):
//! `validateShellSafePath()`, `validateDockerComposeForInjection()`, `parseDockerVolumeString()`
//! and the volume and network name rules. Errors carry upstream's exception messages.

use std::sync::LazyLock;

use coolify_compat::yaml::{Key, PhpValue};
use regex::Regex;

use super::php::{self, data_get, get};

/// `Application::MAX_DOCKER_COMPOSE_COLLECTION_ALIASES`.
pub const MAX_COLLECTION_ALIASES: usize = 256;

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap_or_else(|e| panic!("invalid regex {p}: {e}"))
}

/// `ValidationPatterns::VOLUME_NAME_PATTERN` (`$` also matches before a final newline).
static VOLUME_NAME: LazyLock<Regex> = LazyLock::new(|| re(r"^[a-zA-Z0-9][a-zA-Z0-9._-]*\n?\z"));

/// `ValidationPatterns::isValidDockerNetwork()`.
pub fn is_valid_docker_network(name: &str) -> bool {
    VOLUME_NAME.is_match(name)
}

/// `preg_match(ValidationPatterns::VOLUME_NAME_PATTERN, $name) === 1`.
pub fn is_valid_volume_name(name: &str) -> bool {
    VOLUME_NAME.is_match(name)
}

/// `parseDockerComposeYaml()`: strips a UTF-8 BOM and parses with the higher alias limit.
pub fn parse_docker_compose_yaml(
    compose: &str,
) -> Result<PhpValue, coolify_compat::yaml_parse::ParseError> {
    let compose = compose.strip_prefix('\u{feff}').unwrap_or(compose);
    coolify_compat::yaml_parse::parse(compose, MAX_COLLECTION_ALIASES)
}

/// `validateShellSafePath($input, $context)`.
pub fn validate_shell_safe_path(input: &str, context: &str) -> Result<(), String> {
    const DANGEROUS: [(&str, &str); 11] = [
        ("`", "backtick (command substitution)"),
        ("$(", "command substitution"),
        (
            "${",
            "variable substitution with potential command injection",
        ),
        ("|", "pipe operator"),
        ("&", "background/AND operator"),
        (";", "command separator"),
        ("\n", "newline (command separator)"),
        ("\r", "carriage return"),
        ("\t", "tab (token separator)"),
        (">", "output redirection"),
        ("<", "input redirection"),
    ];
    for (c, description) in DANGEROUS {
        if input.contains(c) {
            return Err(format!(
                "Invalid {context}: contains forbidden character '{c}' ({description}). Shell metacharacters are not allowed for security reasons."
            ));
        }
    }
    Ok(())
}

/// The result of `parseDockerVolumeString()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeString {
    pub source: Option<String>,
    pub target: Option<String>,
    pub mode: Option<String>,
}

const VALID_MODES: [&str; 13] = [
    "ro",
    "rw",
    "z",
    "Z",
    "rslave",
    "rprivate",
    "rshared",
    "slave",
    "private",
    "shared",
    "cached",
    "delegated",
    "consistent",
];

static ENV_WITH_DEFAULT_PREFIX: LazyLock<Regex> = LazyLock::new(|| re(r"^\$\{[^}]+:-[^}]*\}"));
static WINDOWS_DRIVE: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Za-z]:"));
static SIMPLE_ENV_VAR: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\$\{[a-zA-Z_][a-zA-Z0-9_]*\}\n?\z"));
static ENV_VAR_WITH_PATH: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\$\{[a-zA-Z_][a-zA-Z0-9_]*\}[/\w.\-]*\n?\z"));

/// `strrpos`-based split of `rest` into target and an optional valid mode.
fn split_mode(rest: &str) -> (String, Option<String>) {
    match rest.rfind(':') {
        Some(last) => {
            let possible = &rest[last + 1..];
            if VALID_MODES.contains(&possible) {
                (rest[..last].to_owned(), Some(possible.to_owned()))
            } else {
                (rest.to_owned(), None)
            }
        }
        None => (rest.to_owned(), None),
    }
}

/// `parseDockerVolumeString($volumeString)`.
#[allow(clippy::too_many_lines)]
pub fn parse_docker_volume_string(volume: &str) -> Result<VolumeString, String> {
    let volume = volume.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B']);
    let mut source: Option<String>;
    let mut target: Option<String> = None;
    let mut mode: Option<String> = None;

    let env_end = ENV_WITH_DEFAULT_PREFIX.find(volume).map(|m| m.end());
    let colon_count = match env_end {
        Some(end) => volume[end..].matches(':').count(),
        None => volume.matches(':').count(),
    };

    if colon_count == 0 {
        source = Some(volume.to_owned());
        target = Some(volume.to_owned());
    } else if colon_count == 1 {
        if let Some(end) = env_end {
            source = Some(volume[..end].to_owned());
            let remaining = &volume[end..];
            target = Some(remaining.strip_prefix(':').unwrap_or(remaining).to_owned());
        } else {
            let mut parts = volume.split(':');
            source = parts.next().map(str::to_owned);
            target = parts.next().map(str::to_owned);
        }
    } else if colon_count == 2 {
        if let Some(end) = env_end {
            source = Some(volume[..end].to_owned());
            let remaining = &volume[end..];
            if let Some(rest) = remaining.strip_prefix(':') {
                let (t, m) = split_mode(rest);
                target = Some(t);
                mode = m;
            }
        } else if WINDOWS_DRIVE.is_match(volume) {
            match volume[2..].find(':').map(|i| i + 2) {
                Some(second) => {
                    source = Some(volume[..second].to_owned());
                    target = Some(volume[second + 1..].to_owned());
                }
                None => {
                    source = Some(volume.to_owned());
                    target = Some(volume.to_owned());
                }
            }
        } else {
            let last = volume.rfind(':').unwrap_or(0);
            let possible = &volume[last + 1..];
            if VALID_MODES.contains(&possible) {
                mode = Some(possible.to_owned());
                let without = &volume[..last];
                match without.find(':') {
                    Some(c) => {
                        source = Some(without[..c].to_owned());
                        target = Some(without[c + 1..].to_owned());
                    }
                    None => {
                        source = Some(without.to_owned());
                        target = Some(without.to_owned());
                    }
                }
            } else {
                let first = volume.find(':').unwrap_or(0);
                source = Some(volume[..first].to_owned());
                target = Some(volume[first + 1..].to_owned());
            }
        }
    } else if WINDOWS_DRIVE.is_match(volume) {
        match volume[2..].find(':').map(|i| i + 2) {
            Some(second) => {
                source = Some(volume[..second].to_owned());
                let (t, m) = split_mode(&volume[second + 1..]);
                target = Some(t);
                mode = m;
            }
            None => {
                source = Some(volume.to_owned());
                target = Some(volume.to_owned());
            }
        }
    } else {
        let first = volume.find(':').unwrap_or(0);
        source = Some(volume[..first].to_owned());
        let (t, m) = split_mode(&volume[first + 1..]);
        target = Some(t);
        mode = m;
    }

    if let Some(s) = source.as_deref()
        && !php::empty_str(s)
    {
        source = Some(resolve_compose_volume_source_default(s));
    }

    if let Some(s) = source.as_deref()
        && !SIMPLE_ENV_VAR.is_match(s)
        && !ENV_VAR_WITH_PATH.is_match(s)
    {
        validate_shell_safe_path(s, "volume source").map_err(|e| {
            format!("Invalid Docker volume definition: {e} Please use safe path names without shell metacharacters.")
        })?;
    }
    if let Some(t) = target.as_deref() {
        validate_shell_safe_path(t, "volume target").map_err(|e| {
            format!("Invalid Docker volume definition: {e} Please use safe path names without shell metacharacters.")
        })?;
    }
    Ok(VolumeString {
        source,
        target,
        mode,
    })
}

static WHOLE_BRACED: LazyLock<Regex> = LazyLock::new(|| re(r"^\$\{([^}]+)\}\n?\z"));

/// `resolveComposeVolumeSourceDefault($source)`.
pub fn resolve_compose_volume_source_default(source: &str) -> String {
    let Some(c) = WHOLE_BRACED.captures(source) else {
        return source.to_owned();
    };
    let inner = &c[1];
    let Some((var, default)) = inner.split_once(":-") else {
        return source.to_owned();
    };
    if default.is_empty() {
        format!("${{{var}}}")
    } else {
        default.to_owned()
    }
}

static CONTROL: LazyLock<Regex> = LazyLock::new(|| re(r"[\x00-\x1F\x7F]"));
static ARRAY_ENV_WITH_PATH: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\$\{[A-Za-z_][A-Za-z0-9_]*\}[/\w.\-]*\n?\z"));
static ARRAY_ENV_DEFAULT: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\$\{[A-Za-z_][A-Za-z0-9_]*:-([^}]*)\}\n?\z"));

/// `validateComposeArrayVolumeSource($source)`.
pub fn validate_compose_array_volume_source(source: &str) -> Result<(), String> {
    let inner = || -> Result<(), String> {
        if CONTROL.is_match(source) {
            return Err("Invalid volume source: contains a control character.".into());
        }
        if ARRAY_ENV_WITH_PATH.is_match(source) {
            return Ok(());
        }
        if let Some(c) = ARRAY_ENV_DEFAULT.captures(source) {
            return validate_shell_safe_path(&c[1], "volume source");
        }
        validate_shell_safe_path(source, "volume source")
    };
    inner().map_err(|e| {
        format!("Invalid Docker volume definition (array syntax): {e} Please use safe path names without shell metacharacters.")
    })
}

/// `validateComposeContentVolumeSource($volume)`.
pub fn validate_compose_content_volume_source(volume: &PhpValue) -> Result<(), String> {
    let PhpValue::Array(items) = volume else {
        return Ok(());
    };
    let has_content = items.iter().any(|(k, _)| *k == Key::Str("content".into()));
    let is_bind = get(volume, "type") == Some(&PhpValue::Str("bind".into()));
    if !has_content || !is_bind {
        return Ok(());
    }
    match get(volume, "source") {
        Some(PhpValue::Str(s)) if !s.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B']).is_empty() => {
            validate_compose_array_volume_source(s)
        }
        _ => Err("Invalid Docker volume definition (array syntax): A bind volume with content needs a source path.".into()),
    }
}

/// `validateComposeServiceName($serviceName)`: Coolify names each container
/// "<service name>-<suffix>". Docker allows only container names that match
/// `[a-zA-Z0-9][a-zA-Z0-9_.-]+`, and Docker Compose allows only service names that match
/// `[a-zA-Z0-9._-]+`. Compose accepts a service name that starts with ".", "-", or "_", but
/// Docker then refuses to create its container.
pub fn validate_compose_service_name(service_name: &str) -> Result<(), String> {
    let mut chars = service_name.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if valid {
        return Ok(());
    }
    Err(format!(
        "Service name \"{service_name}\" must start with a letter or a digit and contain only letters, digits, \".\", \"-\", and \"_\". Docker uses the same rule for container names, and Coolify uses the service name at the start of the container name."
    ))
}

/// `validateComposeServiceScale($serviceName, $service)`.
pub fn validate_compose_service_scale(
    service_name: &str,
    service: &PhpValue,
) -> Result<(), String> {
    for key in ["scale", "deploy.replicas"] {
        let Some(value) = data_get(service, key) else {
            continue;
        };
        let as_int = match value {
            PhpValue::Int(i) => Some(*i),
            PhpValue::Float(f) => Some(*f as i64),
            PhpValue::Str(s) if coolify_compat::yaml::php_is_numeric(s) => {
                s.trim().parse::<f64>().ok().map(|f| f as i64)
            }
            _ => None,
        };
        if as_int.is_some_and(|n| n > 1) {
            let shown = php::to_string(value);
            return Err(format!(
                "Service \"{service_name}\" sets {key} to {shown}. Coolify sets a container_name for each service, and Docker Compose cannot run more than one container with the same container_name. Remove {key}, or define each instance as a separate service."
            ));
        }
    }
    Ok(())
}

const NETWORK_VARIABLE: &str = r"\$\{([A-Za-z_][A-Za-z0-9_]*)(?::?([-?+])([^}]*))?\}";
static WHOLE_NETWORK_VARIABLE: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"\A{NETWORK_VARIABLE}\z")));
static NETWORK_VARIABLES: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"{NETWORK_VARIABLE}|\$[A-Za-z_][A-Za-z0-9_]*")));

/// `composeNetworkNameVariable($name)`: the variable and its `-` default.
pub fn compose_network_name_variable(name: &str) -> Option<(String, Option<String>)> {
    let c = WHOLE_NETWORK_VARIABLE.captures(name)?;
    let default = (c.get(2).map(|m| m.as_str()) == Some("-"))
        .then(|| c.get(3).map_or(String::new(), |m| m.as_str().to_owned()));
    Some((c[1].to_owned(), default))
}

static SAFE_ERROR_OPERAND: LazyLock<Regex> = LazyLock::new(|| re(r"\A[A-Za-z0-9 _.,:!-]*\z"));
static SAFE_OPERAND: LazyLock<Regex> = LazyLock::new(|| re(r"\A[A-Za-z0-9_.-]*\z"));

/// `isSafeComposeNetworkVariableOperand()`.
fn is_safe_operand(operator: &str, operand: &str) -> bool {
    if operator == "?" {
        SAFE_ERROR_OPERAND.is_match(operand)
    } else {
        SAFE_OPERAND.is_match(operand)
    }
}

/// `validateComposeNetworkNameWithVariables()`.
pub fn validate_compose_network_name_with_variables(
    name: &str,
    context: &str,
    service_name: Option<&str>,
) -> Result<(), String> {
    let valid = match compose_network_name_variable(name) {
        Some((_, Some(default))) => is_valid_docker_network(&default),
        _ => {
            let mut safe = true;
            let without = NETWORK_VARIABLES.replace_all(name, |c: &regex::Captures<'_>| {
                let op = c.get(2).map_or("", |m| m.as_str());
                let operand = c.get(3).map_or("", |m| m.as_str());
                if !is_safe_operand(op, operand) {
                    safe = false;
                }
                "x"
            });
            safe && is_valid_docker_network(&without)
        }
    };
    if valid {
        Ok(())
    } else {
        Err(invalid_network_name(name, context, service_name, true))
    }
}

/// `validateComposeNetworkName()`.
pub fn validate_compose_network_name(
    name: &str,
    context: &str,
    service_name: Option<&str>,
) -> Result<(), String> {
    if name.is_empty() || !is_valid_docker_network(name) {
        return Err(invalid_network_name(name, context, service_name, false));
    }
    Ok(())
}

/// `invalidComposeNetworkNameException()`.
fn invalid_network_name(
    name: &str,
    context: &str,
    service_name: Option<&str>,
    allows_variables: bool,
) -> String {
    let location = service_name.map_or(String::new(), |s| format!(" in service {s}"));
    format!(
        "Invalid Docker Compose {context} \"{name}\"{location}. Network names must start with an alphanumeric character and contain only alphanumeric characters, dots, hyphens, and underscores{}",
        if allows_variables {
            ", and can use variables such as ${NETWORK:-default}."
        } else {
            "."
        }
    )
}

/// `filter_var($value, FILTER_VALIDATE_BOOLEAN)`.
fn filter_bool(v: &PhpValue) -> bool {
    match v {
        PhpValue::Bool(b) => *b,
        PhpValue::Int(i) => *i == 1,
        PhpValue::Float(f) => *f == 1.0,
        PhpValue::Str(s) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        ),
        _ => false,
    }
}

/// `isComposeExternalVolume($declaration)`.
pub fn is_compose_external_volume(declaration: Option<&PhpValue>) -> bool {
    let Some(d @ PhpValue::Array(_)) = declaration else {
        return false;
    };
    match get(d, "external") {
        None => false,
        Some(PhpValue::Array(_)) => true,
        Some(other) => filter_bool(other),
    }
}

static VOLUME_NAME_WITH_VARIABLES: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^(?:[A-Za-z0-9._-]|\$[A-Za-z_][A-Za-z0-9_]*|\$\{[A-Za-z_][A-Za-z0-9_]*(?::?-[A-Za-z0-9._-]*)?\})+\n?\z",
    )
});

/// `isComposeVolumeNameWithVariables($name)`.
pub fn is_compose_volume_name_with_variables(name: &str) -> bool {
    name.contains('$') && VOLUME_NAME_WITH_VARIABLES.is_match(name)
}

/// `validateComposeExternalVolume($key, $declaration)`.
pub fn validate_compose_external_volume(key: &str, declaration: &PhpValue) -> Result<(), String> {
    if !is_valid_volume_name(key) {
        return Err("Invalid external Docker Compose volume. Volume names must start with an alphanumeric character and contain only alphanumeric characters, dots, hyphens, and underscores.".into());
    }
    let mut names: Vec<&PhpValue> = Vec::new();
    if let Some(n) = get(declaration, "name") {
        names.push(n);
    }
    if let Some(ext @ PhpValue::Array(_)) = get(declaration, "external")
        && let Some(n) = get(ext, "name")
    {
        names.push(n);
    }
    for n in names {
        let ok = matches!(n, PhpValue::Str(s) if is_valid_volume_name(s) || is_compose_volume_name_with_variables(s));
        if !ok {
            return Err(format!(
                "Invalid external Docker Compose volume {key}. The name must be a Docker volume name (alphanumeric characters, dots, hyphens, and underscores) and can contain variables such as ${{NAME}} or ${{NAME:-default}}."
            ));
        }
    }
    Ok(())
}

/// `validateDockerComposeForInjection($composeYaml)`.
pub fn validate_docker_compose_for_injection(compose: &str) -> Result<(), String> {
    let parsed = parse_docker_compose_yaml(compose)
        .map_err(|e| format!("Invalid YAML format: {}", e.message()))?;
    let services = match get(&parsed, "services") {
        Some(s @ PhpValue::Array(_)) => s,
        _ => return Err("Docker Compose file must contain a \"services\" section".into()),
    };
    let PhpValue::Array(service_items) = services else {
        return Ok(());
    };
    for (name_key, config) in service_items {
        let service_name = name_key.as_string();
        validate_shell_safe_path(&service_name, "service name").map_err(|e| {
            format!("Invalid Docker Compose service name: {e} Service names must not contain shell metacharacters.")
        })?;
        validate_compose_service_name(&service_name)?;
        if let Some(PhpValue::Array(volumes)) = get(config, "volumes") {
            for (_, volume) in volumes {
                match volume {
                    PhpValue::Str(s) => {
                        parse_docker_volume_string(s)?;
                    }
                    PhpValue::Array(_) => {
                        if let Some(PhpValue::Str(source)) = get(volume, "source") {
                            validate_compose_array_volume_source(source)?;
                        }
                        if let Some(PhpValue::Str(target)) = get(volume, "target") {
                            validate_shell_safe_path(target, "volume target").map_err(|e| {
                                format!("Invalid Docker volume definition (array syntax): {e} Please use safe path names without shell metacharacters.")
                            })?;
                        }
                        validate_compose_content_volume_source(volume)?;
                    }
                    _ => {}
                }
            }
        }
        if let Some(PhpValue::Array(networks)) = get(config, "networks") {
            for (key, details) in networks {
                match (key, details) {
                    (Key::Int(_), PhpValue::Str(_) | PhpValue::Int(_)) => {
                        validate_compose_network_name_with_variables(
                            &php::to_string(details),
                            "service network",
                            Some(&service_name),
                        )?;
                    }
                    _ => validate_compose_network_name(
                        &key.as_string(),
                        "service network",
                        Some(&service_name),
                    )?,
                }
            }
        }
    }
    if let Some(PhpValue::Array(networks)) = get(&parsed, "networks") {
        for (name, config) in networks {
            validate_compose_network_name(&name.as_string(), "network name", None)?;
            if let Some(PhpValue::Str(n)) = get(config, "name") {
                validate_compose_network_name_with_variables(n, "network name field", None)?;
            }
        }
    }
    if let Some(PhpValue::Array(volumes)) = get(&parsed, "volumes") {
        for (name, config) in volumes {
            if is_compose_external_volume(Some(config)) {
                validate_compose_external_volume(&name.as_string(), config)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `validateComposeServiceName()`: Docker refuses a container name that starts with ".",
    /// "-" or "_", and Coolify puts the service name at the start of the container name.
    #[test]
    fn service_names_must_work_as_the_start_of_a_container_name() {
        for name in ["web", "Web1", "my.app", "my-app_2", "0db", "api.v1"] {
            assert!(validate_compose_service_name(name).is_ok(), "{name}");
        }
        for name in ["", ".hidden", "-web", "_web", "we b", "web/app", "w\u{e9}b"] {
            assert!(validate_compose_service_name(name).is_err(), "{name}");
        }
        assert_eq!(
            validate_compose_service_name("_web").unwrap_err(),
            "Service name \"_web\" must start with a letter or a digit and contain only letters, digits, \".\", \"-\", and \"_\". Docker uses the same rule for container names, and Coolify uses the service name at the start of the container name."
        );
    }

    /// The injection check of a pasted compose file reports such a name too; a dotted name is fine.
    #[test]
    fn the_injection_check_validates_service_names() {
        let error = validate_docker_compose_for_injection("services:\n  _web:\n    image: nginx\n")
            .err()
            .unwrap_or_default();
        assert!(
            error.starts_with("Service name \"_web\" must start with"),
            "{error}"
        );
        assert!(
            validate_docker_compose_for_injection("services:\n  web.api:\n    image: nginx\n")
                .is_ok()
        );
    }

    #[test]
    fn volume_strings() {
        let v = parse_docker_volume_string("./data:/app/data:ro").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(v.source.as_deref(), Some("./data"));
        assert_eq!(v.target.as_deref(), Some("/app/data"));
        assert_eq!(v.mode.as_deref(), Some("ro"));
        let v = parse_docker_volume_string("${DATA:-/srv/x}:/x").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(v.source.as_deref(), Some("/srv/x"));
        assert!(parse_docker_volume_string("$(rm):/x").is_err());
        let v = parse_docker_volume_string("myvolume").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(v.target.as_deref(), Some("myvolume"));
    }

    #[test]
    fn network_names() {
        assert!(validate_compose_network_name_with_variables("${NET:-default}", "x", None).is_ok());
        assert!(
            validate_compose_network_name_with_variables("${NET:-bad name}", "x", None).is_err()
        );
        assert_eq!(
            compose_network_name_variable("${NET:-abc}"),
            Some(("NET".into(), Some("abc".into())))
        );
        assert_eq!(
            compose_network_name_variable("${NET:?err}"),
            Some(("NET".into(), None))
        );
    }
}
