//! The build part of `Application\General` for the git build packs coolify-rs deploys
//! (Dockerfile, Static, Nixpacks, Railpack): what `syncData(toModel: true)` + `submit()` write
//! for the build fields (with the `Application` model's mutators and `saving` hook), and the
//! side effects of `updatedBuildPack()`, `setSiteType()` and `generateNginxConfiguration()`.

use coolify_db::applications::ApplicationRow;
use coolify_db::servers::ColumnValue;
use serde_json::{Map, Value};

use crate::validation::{Field, Rule};

/// `ValidationPatterns::SHELL_SAFE_COMMAND_PATTERN`.
static SHELL_SAFE_COMMAND: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r#"^(?:[ \t]+|&&|\|\||"[^"$`\\\n\r]*"|'[^'\n\r]*'|[a-zA-Z0-9._\-/=:@,+\[\]{}#%^~*?!]+)+$"#,
    )
    .unwrap_or_else(|_| unreachable!())
});
/// `ValidationPatterns::DIRECTORY_PATH_PATTERN`.
static DIRECTORY_PATH: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^/([a-zA-Z0-9._\-/~@+]*)?$").unwrap_or_else(|_| unreachable!())
});
/// `ValidationPatterns::FILE_PATH_PATTERN`.
static FILE_PATH: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^/[a-zA-Z0-9._\-/~@+]+$").unwrap_or_else(|_| unreachable!())
});
/// `ValidationPatterns::DOCKER_TARGET_PATTERN`.
static DOCKER_TARGET: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._-]*$").unwrap_or_else(|_| unreachable!())
});

/// `ValidationPatterns::CONTAINER_NAME_PATTERN`.
static CONTAINER_NAME: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._-]*$").unwrap_or_else(|_| unreachable!())
});

/// `General::rules()` for the build fields (snake_case keys).
pub(super) fn rules() -> Vec<Field> {
    let shell = || {
        vec![
            Rule::Nullable,
            Rule::String,
            Rule::Max(1000),
            Rule::Regex(&SHELL_SAFE_COMMAND),
        ]
    };
    let path = |re: &'static regex::Regex, max: usize| {
        vec![
            Rule::Nullable,
            Rule::String,
            Rule::Max(max),
            Rule::Regex(re),
        ]
    };
    vec![
        Field {
            name: "install_command",
            rules: shell(),
        },
        Field {
            name: "build_command",
            rules: shell(),
        },
        Field {
            name: "start_command",
            rules: shell(),
        },
        Field {
            name: "build_pack",
            rules: vec![Rule::Required],
        },
        Field {
            name: "static_image",
            rules: vec![Rule::Required, Rule::In(&["nginx:alpine"])],
        },
        Field {
            name: "base_directory",
            rules: vec![
                Rule::Required,
                Rule::String,
                Rule::Max(255),
                Rule::Regex(&DIRECTORY_PATH),
            ],
        },
        Field {
            name: "publish_directory",
            rules: path(&DIRECTORY_PATH, 255),
        },
        Field {
            name: "dockerfile_location",
            rules: path(&FILE_PATH, 255),
        },
        Field {
            name: "dockerfile_target_build",
            rules: path(&DOCKER_TARGET, 128),
        },
        Field {
            name: "docker_compose_location",
            rules: path(&FILE_PATH, 255),
        },
        Field {
            name: "docker_compose_custom_start_command",
            rules: shell(),
        },
        Field {
            name: "docker_compose_custom_build_command",
            rules: shell(),
        },
        Field {
            name: "custom_docker_run_options",
            rules: vec![
                Rule::Nullable,
                Rule::String,
                Rule::Max(2000),
                Rule::Regex(&SHELL_SAFE_COMMAND),
            ],
        },
        Field {
            name: "pre_deployment_command_container",
            rules: path(&CONTAINER_NAME, 255),
        },
        Field {
            name: "post_deployment_command_container",
            rules: path(&CONTAINER_NAME, 255),
        },
    ]
}

/// `defaultNginxConfiguration($type)`.
pub fn default_nginx_configuration(spa: bool) -> &'static str {
    if spa {
        "server {\n    location / {\n        root /usr/share/nginx/html;\n        index index.html;\n        try_files $uri $uri/ /index.html;\n    }\n\n    # Handle 404 errors\n    error_page 404 /404.html;\n    location = /404.html {\n        root /usr/share/nginx/html;\n        internal;\n    }\n\n    # Handle server errors (50x)\n    error_page 500 502 503 504 /50x.html;\n    location = /50x.html {\n        root /usr/share/nginx/html;\n        internal;\n    }\n}"
    } else {
        "server {\n    location / {\n        root /usr/share/nginx/html;\n        index index.html index.htm;\n        try_files $uri $uri.html $uri/index.html $uri/index.htm $uri/ =404;\n    }\n\n    # Handle 404 errors\n    error_page 404 /404.html;\n    location = /404.html {\n        root /usr/share/nginx/html;\n        internal;\n    }\n\n    # Handle server errors (50x)\n    error_page 500 502 503 504 /50x.html;\n    location = /50x.html {\n        root /usr/share/nginx/html;\n        internal;\n    }\n}"
    }
}

/// PHP truthiness of a form string.
fn truthy(v: Option<&str>) -> bool {
    v.is_some_and(|v| !v.is_empty() && v != "0")
}

fn slash(v: &str) -> String {
    format!("/{}", v.trim_start_matches('/'))
}

/// `$model->publish_directory = …` through the mutator, then `submit()`'s `rtrim('/')`.
fn publish_directory(form: Option<&str>) -> Option<String> {
    let set = |v: Option<&str>| truthy(v).then(|| slash(v.unwrap_or_default()));
    match form {
        Some(raw) if truthy(Some(raw)) && raw != "/" => set(Some(raw.trim_end_matches('/'))),
        _ => set(form),
    }
}

/// `$model->base_directory = …` through the mutator, then `submit()`'s `rtrim('/')`.
fn base_directory(form: &str) -> String {
    if truthy(Some(form)) && form != "/" {
        slash(form.trim_end_matches('/'))
    } else {
        slash(form)
    }
}

/// The `dockerfileLocation` mutator.
fn dockerfile_location(form: Option<&str>, build_pack: &str) -> Option<String> {
    match form {
        None | Some("") => (build_pack == "dockerfile").then(|| "/Dockerfile".to_owned()),
        Some("/") => Some("/".into()),
        Some(v) => Some(slash(v.strip_suffix('/').unwrap_or(v))),
    }
}

/// `Application::parseWatchPaths()` (the `watchPaths` mutator; falsy values become null).
fn watch_paths(form: Option<&str>) -> Option<String> {
    let v = form.filter(|v| truthy(Some(v)))?;
    let paths: Vec<String> = v
        .split('\n')
        .map(|p| {
            let p = p.trim();
            match p.strip_prefix('!') {
                Some(rest) => format!("!{}", rest.trim().trim_start_matches('/')),
                None => p.trim_start_matches('/').to_owned(),
            }
        })
        .filter(|p| !p.is_empty())
        .collect();
    Some(paths.join("\n").trim().to_owned())
}

/// What a save writes besides the columns.
#[derive(Debug, Default)]
pub(super) struct Effects {
    /// `application_settings` writes (`updatedBuildPack()` turns `is_static` off).
    pub settings: Vec<(&'static str, ColumnValue)>,
    /// `resetDefaultLabels(false)` after the save.
    pub reset_labels: bool,
    /// Toasts before `Application settings updated!` (`Nginx configuration generated.`).
    pub toasts: Vec<&'static str>,
    /// Switching away from Docker Compose removes the `SERVICE_FQDN_*` / `SERVICE_URL_*` variables.
    pub remove_service_variables: bool,
}

/// One column write with the `saving` hook's dirty check and trim: a changed value is
/// replaced by `str($value)->trim()`, so `null` becomes an empty string, and the mutator
/// (`then`) runs again on the trimmed value.
fn saved(
    stored: Option<&str>,
    new: Option<String>,
    then: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if new.as_deref() == stored {
        return new;
    }
    then(new.as_deref().unwrap_or_default().trim())
}

/// Adds the build columns that the body carries to `values` (`syncData(toModel: true)` and the
/// path normalisation of `submit()`), and the `updatedBuildPack()` side effects.
pub(super) fn apply(
    a: &ApplicationRow,
    m: &Map<String, Value>,
    values: &mut Vec<(&'static str, ColumnValue)>,
) -> Effects {
    let mut fx = Effects::default();
    let text = |k: &str| m.get(k).and_then(Value::as_str).map(str::to_owned);
    let stored_pack = a.str("build_pack").unwrap_or_default().to_owned();
    let pack = text("build_pack").unwrap_or_else(|| stored_pack.clone());
    let changed_pack = m.contains_key("build_pack") && pack != stored_pack;
    let mut set = |k: &'static str, v: Option<String>| values.push((k, ColumnValue::Text(v)));

    if m.contains_key("build_pack") {
        set("build_pack", Some(pack.clone()));
    }
    if let Some(v) = text("static_image") {
        set("static_image", Some(v));
    }
    for k in ["install_command", "build_command", "start_command"] {
        if m.contains_key(k) {
            set(k, saved(a.str(k), text(k), |v| Some(v.to_owned())));
        }
    }
    if let Some(v) = text("base_directory") {
        set(
            "base_directory",
            saved(a.str("base_directory"), Some(base_directory(&v)), |v| {
                Some(slash(v))
            }),
        );
    }
    if m.contains_key("publish_directory") {
        let new = publish_directory(text("publish_directory").as_deref());
        // `str(null)->trim()` is a Stringable: the mutator sees a truthy object.
        set(
            "publish_directory",
            saved(a.str("publish_directory"), new, |v| Some(slash(v))),
        );
    }
    if m.contains_key("dockerfile_location") {
        set(
            "dockerfile_location",
            dockerfile_location(text("dockerfile_location").as_deref(), &pack),
        );
    }
    if m.contains_key("dockerfile_target_build") {
        let v = text("dockerfile_target_build");
        set(
            "dockerfile_target_build",
            saved(a.str("dockerfile_target_build"), v, |v| Some(v.to_owned())),
        );
    }
    if m.contains_key("watch_paths") {
        set("watch_paths", watch_paths(text("watch_paths").as_deref()));
    }
    let mut nginx: Option<Option<String>> = m
        .contains_key("custom_nginx_configuration")
        .then(|| text("custom_nginx_configuration").filter(|v| !v.is_empty()));

    if changed_pack {
        // updatedBuildPack()
        if pack != "nixpacks" && pack != "railpack" {
            fx.settings.push(("is_static", ColumnValue::Bool(false)));
        } else {
            fx.reset_labels = true;
        }
        if pack == "dockercompose" && a.str("docker_compose_location").is_none_or(str::is_empty) {
            set(
                "docker_compose_location",
                Some("/docker-compose.yaml".into()),
            );
        }
        if pack == "static" {
            values.retain(|(k, _)| *k != "ports_exposes");
            values.push(("ports_exposes", ColumnValue::Text(Some("80".into()))));
            fx.reset_labels = true;
            nginx = Some(Some(default_nginx_configuration(false).to_owned()));
            fx.toasts.push("Nginx configuration generated.");
        }
        // The `saving` hook's cleanup when leaving a build pack.
        if stored_pack == "dockerfile" {
            values
                .retain(|(k, _)| !matches!(*k, "dockerfile_location" | "dockerfile_target_build"));
            values.push(("dockerfile", ColumnValue::Text(None)));
            values.push(("dockerfile_location", ColumnValue::Text(None)));
            values.push(("dockerfile_target_build", ColumnValue::Text(None)));
            values.push(("custom_healthcheck_found", ColumnValue::Bool(false)));
        }
        if stored_pack == "dockercompose" {
            values.push(("docker_compose_domains", ColumnValue::Text(None)));
            values.push(("docker_compose_raw", ColumnValue::Text(None)));
            fx.remove_service_variables = true;
        }
    }
    if m.get("generate_nginx").and_then(Value::as_str).is_some() {
        let spa = m.get("generate_nginx").and_then(Value::as_str) == Some("spa");
        nginx = Some(Some(default_nginx_configuration(spa).to_owned()));
        fx.toasts.push("Nginx configuration generated.");
    }
    if let Some(v) = nginx {
        use base64::Engine as _;
        // The mutator stores base64; an empty configuration is saved as null.
        values.push((
            "custom_nginx_configuration",
            ColumnValue::Text(v.map(|v| base64::engine::general_purpose::STANDARD.encode(v))),
        ));
    }
    fx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutators() {
        assert_eq!(publish_directory(Some("dist/")), Some("/dist".into()));
        assert_eq!(publish_directory(Some("")), None);
        assert_eq!(publish_directory(Some("/")), Some("/".into()));
        assert_eq!(base_directory("static/"), "/static");
        assert_eq!(base_directory("/"), "/");
        assert_eq!(
            dockerfile_location(None, "dockerfile"),
            Some("/Dockerfile".into())
        );
        assert_eq!(
            dockerfile_location(Some("Dockerfile.prod/"), "dockerfile"),
            Some("/Dockerfile.prod".into())
        );
        assert_eq!(
            watch_paths(Some(" /src/**\n! /docs\n\n")),
            Some("src/**\n!docs".into())
        );
        assert_eq!(saved(None, None, |v| Some(slash(v))), None);
        assert_eq!(
            saved(Some("/dist"), None, |v| Some(slash(v))),
            Some("/".into())
        );
        assert_eq!(
            saved(None, Some(String::new()), |v| Some(v.to_owned())),
            Some(String::new())
        );
    }
}
