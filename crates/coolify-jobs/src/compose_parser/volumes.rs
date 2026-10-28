//! Volumes of Compose services (bootstrap/helpers/parsers.php and shared.php): local bind
//! sources, long-syntax binds, external and renamed named volumes, and the comment-preserving
//! removal of one-time volume fields from `docker_compose_raw`.

use std::sync::LazyLock;

use coolify_compat::yaml::{Key, PhpValue};

use super::php::{self, Arr};
use super::validate;

/// `base_configuration_dir()`.
pub const BASE_CONFIGURATION_DIR: &str = "/data/coolify";

/// `addPreviewDeploymentSuffix($name, $pull_request_id)`.
pub fn add_preview_deployment_suffix(name: &str, pull_request_id: i64) -> String {
    if pull_request_id == 0 {
        name.to_owned()
    } else {
        format!("{name}-pr-{pull_request_id}")
    }
}

/// `sourceIsLocal($source)`.
pub fn source_is_local(source: &str) -> bool {
    ["./", "/", "~", "..", "~/", "../"]
        .iter()
        .any(|p| source.starts_with(p))
}

/// `legacyReplaceLocalSource($source, $replacedWith)`.
pub fn legacy_replace_local_source(source: &str, replaced_with: &str) -> String {
    let mut s = source.to_owned();
    if s.starts_with('.') {
        s = php::replace_first(&s, ".", replaced_with);
    }
    if s.starts_with('~') {
        s = php::replace_first(&s, "~", replaced_with);
    }
    if s.starts_with("..") {
        s = php::replace_first(&s, "..", replaced_with);
    }
    if s.ends_with('/') && s != "/" {
        s = php::replace_last(&s, "/", "");
    }
    s
}

/// `replaceLocalSource($source, $replacedWith)`.
pub fn replace_local_source(source: &str, replaced_with: &str) -> Result<String, String> {
    if !source.split('/').any(|seg| seg == "..") {
        return Ok(legacy_replace_local_source(source, replaced_with));
    }
    let path = if let Some(rest) = source.strip_prefix('~') {
        format!("{replaced_with}{rest}")
    } else if !source.starts_with('/') {
        format!("{replaced_with}/{source}")
    } else {
        source.to_owned()
    };
    let mut segments: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            if segments.pop().is_none() {
                return Err(format!(
                    "Volume source {source} resolves to a path above /. Remove some ../ segments."
                ));
            }
            continue;
        }
        segments.push(seg);
    }
    let resolved = format!("/{}", segments.join("/"));
    validate::validate_shell_safe_path(&resolved, "volume source")?;
    Ok(resolved)
}

/// `resolveComposeBindSource($source, $directory, $existingFsPath)`.
pub fn resolve_compose_bind_source(
    source: &str,
    directory: &str,
    existing_fs_path: Option<&str>,
) -> Result<String, String> {
    let legacy = legacy_replace_local_source(source, directory);
    if let Some(existing) = existing_fs_path {
        let pattern = format!(r"^{}(-pr-\d+)?\n?\z", regex::escape(&legacy));
        if regex::Regex::new(&pattern).is_ok_and(|r| r.is_match(existing)) {
            return Ok(legacy);
        }
    }
    replace_local_source(source, directory)
}

static TARGET_MODE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^(.+):(ro|rw)\n?\z").unwrap_or_else(|_| unreachable!()));

/// `composeLongBindVolume($volume, $source)`.
pub fn compose_long_bind_volume(volume: &[(Key, PhpValue)], source: &str) -> Arr {
    let mut v: Arr = volume
        .iter()
        .filter(|(k, _)| {
            let k = k.as_string();
            [
                "type",
                "source",
                "target",
                "read_only",
                "consistency",
                "bind",
            ]
            .contains(&k.as_str())
                || k.starts_with("x-")
        })
        .cloned()
        .collect();
    let target = php::get_key(&v, &Key::Str("target".into()))
        .map(php::to_string)
        .unwrap_or_default();
    if let Some(c) = TARGET_MODE.captures(&target) {
        let (t, m) = (c[1].to_owned(), c[2].to_owned());
        php::put_str(&mut v, "target", PhpValue::Str(t));
        let current = php::get_key(&v, &Key::Str("read_only".into()));
        if current.is_none_or(|x| *x == PhpValue::Null) {
            php::put_str(&mut v, "read_only", PhpValue::Bool(m == "ro"));
        }
    }
    php::put_str(&mut v, "source", PhpValue::Str(source.to_owned()));
    let mut bind = match php::get_key(&v, &Key::Str("bind".into())) {
        Some(PhpValue::Array(b)) => b.clone(),
        _ => Vec::new(),
    };
    if php::get_key(&bind, &Key::Str("create_host_path".into()))
        .is_none_or(|x| *x == PhpValue::Null)
    {
        php::put_str(&mut bind, "create_host_path", PhpValue::Bool(true));
    }
    php::put_str(&mut v, "bind", PhpValue::Array(bind));
    v
}

/// `composeBindKeepsMissingHostPath($volume)`.
pub fn compose_bind_keeps_missing_host_path(volume: &PhpValue) -> bool {
    let PhpValue::Array(items) = volume else {
        return false;
    };
    php::data_get(volume, "bind.create_host_path") == Some(&PhpValue::Bool(false))
        && !items.iter().any(|(k, _)| *k == Key::Str("content".into()))
}

/// `composeExternalVolumeDeclaration($topLevelVolumes, $source)`.
pub fn compose_external_volume_declaration(
    top_level_volumes: &[(Key, PhpValue)],
    source: &str,
) -> Result<Option<PhpValue>, String> {
    let declaration = php::get_key(top_level_volumes, &Key::from_php_string(source));
    if !validate::is_compose_external_volume(declaration) {
        return Ok(None);
    }
    let declaration = declaration.cloned().unwrap_or(PhpValue::Null);
    validate::validate_compose_external_volume(source, &declaration)?;
    Ok(Some(declaration))
}

/// `composeExternalVolumeDockerName($key, $declaration)`.
pub fn compose_external_volume_docker_name(key: &str, declaration: &PhpValue) -> String {
    match php::data_get(declaration, "name")
        .filter(|v| **v != PhpValue::Null)
        .or_else(|| php::data_get(declaration, "external.name").filter(|v| **v != PhpValue::Null))
    {
        Some(v) => php::to_string(v),
        None => key.to_owned(),
    }
}

/// `composeLegacyExternalVolumeWarning($source, $declaration, $legacyName)`.
pub fn compose_legacy_external_volume_warning(
    source: &str,
    declaration: &PhpValue,
    legacy_name: &str,
) -> String {
    let docker = compose_external_volume_docker_name(source, declaration);
    let external = if docker == source {
        "external".to_owned()
    } else {
        format!("external (Docker volume '{docker}')")
    };
    format!(
        "Volume '{source}' is declared as {external}, but Coolify still uses '{legacy_name}' because this resource used it before. To use the external volume, copy your data into it and delete the storage entry '{legacy_name}', then redeploy."
    )
}

/// `composeRenamedVolumeDeclaration($declaration, $name)`.
pub fn compose_renamed_volume_declaration(declaration: Option<&PhpValue>, name: &str) -> Arr {
    let mut renamed: Arr = match declaration {
        Some(PhpValue::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    php::forget(&mut renamed, "external");
    php::forget(&mut renamed, "name");
    php::put_str(&mut renamed, "name", PhpValue::Str(name.to_owned()));
    renamed
}

/// `composeRenamedVolumeDeclarationFor($declaration, $name, $volume, $isPreview)`.
pub fn compose_renamed_volume_declaration_for(
    declaration: Option<&PhpValue>,
    name: &str,
    ignores_driver_options: bool,
    is_preview: bool,
) -> Arr {
    if ignores_driver_options {
        return vec![(Key::Str("name".into()), PhpValue::Str(name.to_owned()))];
    }
    let mut renamed = compose_renamed_volume_declaration(declaration, name);
    let whole = PhpValue::Array(renamed.clone());
    let device_filled = php::data_get(&whole, "driver_opts.device").is_some_and(php::filled);
    let tmpfs = php::data_get(&whole, "driver_opts.type") == Some(&PhpValue::Str("tmpfs".into()));
    if is_preview && device_filled && !tmpfs {
        php::forget(&mut renamed, "driver_opts");
    }
    renamed
}

static RAW_SIMPLE_ENV: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^\$\{[a-zA-Z_][a-zA-Z0-9_]*\}\n?\z").unwrap_or_else(|_| unreachable!())
});
static RAW_ENV_WITH_PATH: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^\$\{[a-zA-Z_][a-zA-Z0-9_]*\}(?:/[\w.\-]+)*/?\n?\z")
        .unwrap_or_else(|_| unreachable!())
});
static RAW_ENV_DEFAULT: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?s)^\$\{([a-zA-Z_][a-zA-Z0-9_]*):-(.*)\}\n?\z")
        .unwrap_or_else(|_| unreachable!())
});

/// `rawComposeBindMkdirCommand($source)`.
pub fn raw_compose_bind_mkdir_command(source: &str) -> Result<Option<String>, String> {
    if source.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err("Invalid volume source: contains a control character.".into());
    }
    let source = source.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B']);
    if source.is_empty() {
        return Err("Invalid volume source: path is empty.".into());
    }
    if RAW_SIMPLE_ENV.is_match(source) || RAW_ENV_WITH_PATH.is_match(source) {
        return Ok(None);
    }
    if let Some(c) = RAW_ENV_DEFAULT.captures(source) {
        validate::validate_shell_safe_path(&c[2], "volume source")?;
        return Ok(None);
    }
    validate::validate_shell_safe_path(source, "volume source")?;
    Ok(Some(format!(
        "mkdir -p -- {} > /dev/null 2>&1 || true",
        php::escapeshellarg(source)
    )))
}

static KEY_LINE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"^([A-Za-z0-9_.-]+|"[^"]+"|'[^']+')[ \t\n\x0B\x0C\r]*:(.*)$"#)
        .unwrap_or_else(|_| unreachable!())
});
static BLOCK_HEADER: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[|>][+-]?(?:\s+#.*)?$").unwrap_or_else(|_| unreachable!())
});

/// `removeComposeVolumeFieldsPreservingComments($source, $cleanedYaml, $fields)`.
pub fn remove_compose_volume_fields_preserving_comments(
    source: &str,
    cleaned: &PhpValue,
    fields: &[&str],
) -> String {
    let mut context: Vec<(usize, String)> = Vec::new();
    let mut remove_indent: Option<usize> = None;
    let mut block_indent: Option<usize> = None;
    let mut result = String::new();
    for line in source.split_inclusive('\n') {
        let text = line.trim_end_matches(['\r', '\n']);
        let indent = text.bytes().take_while(|b| *b == b' ').count();
        let blank = text
            .trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B'])
            .is_empty();
        if let Some(ri) = remove_indent {
            if blank || indent > ri {
                continue;
            }
            remove_indent = None;
        }
        if let Some(bi) = block_indent {
            if blank || indent > bi {
                result.push_str(line);
                continue;
            }
            block_indent = None;
        }
        if blank
            || text
                .trim_start_matches([' ', '\t', '\n', '\r', '\0', '\x0B'])
                .starts_with('#')
        {
            result.push_str(line);
            continue;
        }
        while context.last().is_some_and(|(i, _)| *i >= indent) {
            context.pop();
        }
        let mut body = text[indent..].to_owned();
        let is_list_item =
            body.starts_with('-') && body[1..].starts_with(|c: char| c.is_whitespace());
        if is_list_item {
            context.push((indent, "[]".into()));
            body = body[1..].trim_start().to_owned();
        }
        if let Some(c) = KEY_LINE.captures(&body) {
            let key = c[1].trim_matches(['"', '\'']).to_owned();
            let path: Vec<&str> = context.iter().map(|(_, k)| k.as_str()).collect();
            if !is_list_item
                && path.len() == 4
                && path[0] == "services"
                && path[2] == "volumes"
                && path[3] == "[]"
                && fields.contains(&key.as_str())
            {
                remove_indent = Some(indent);
                continue;
            }
            let value = c[2].trim();
            if value.is_empty() || value.starts_with('#') {
                context.push((indent, key));
            } else if BLOCK_HEADER.is_match(value) {
                block_indent = Some(indent);
            }
        }
        result.push_str(line);
    }
    if let Ok(parsed) = validate::parse_docker_compose_yaml(&result)
        && &parsed == cleaned
    {
        return result;
    }
    coolify_compat::yaml::dump_with_indent(cleaned, 10, 2)
}
