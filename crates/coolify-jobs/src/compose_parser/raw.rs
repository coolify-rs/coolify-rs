//! `Application::oldRawParser()` (app/Models/Application.php) for raw compose deployments
//! (`is_raw_compose_deployment_enabled`): the Coolify labels on every service and the bind source
//! directories created on the server. The compose file is used as written otherwise.

use chrono::NaiveDateTime;
use coolify_compat::yaml::{Key, PhpValue};
use coolify_remote::RunOptions;

use super::application::Run;
use super::php;
use super::validate;
use super::volumes;
use crate::Jobs;

/// The labels and mkdir commands of `oldRawParser()`: `(docker_compose_raw, commands)`.
pub fn raw_compose(raw: &str, uuid: &str) -> Result<(String, Vec<String>), String> {
    let mut yaml = validate::parse_docker_compose_yaml(raw).map_err(|e| e.message())?;
    let workdir = format!("{}/applications/{uuid}", volumes::BASE_CONFIGURATION_DIR);
    let mut commands: Vec<String> = Vec::new();
    let services = php::collect(php::get(&yaml, "services"));
    let mut out_services: Vec<(Key, PhpValue)> = Vec::new();
    for (name, service) in services {
        for (_, volume) in php::collect(php::get(&service, "volumes")) {
            let (kind, source) = match &volume {
                PhpValue::Str(s) => {
                    let source = php::before(s, ":");
                    let bind = source.starts_with("./")
                        || source.starts_with('/')
                        || source.starts_with('~');
                    (bind.then(|| "bind".to_owned()), source)
                }
                PhpValue::Array(_) => (
                    Some(php::data_get_str(&volume, "type", "")),
                    php::data_get_str(&volume, "source", ""),
                ),
                _ => (None, String::new()),
            };
            if kind.as_deref() != Some("bind") {
                continue;
            }
            if ["/var/run/docker.sock", "/tmp", "/tmp/"].contains(&source.as_str()) {
                continue;
            }
            let source = if source.starts_with('.') {
                volumes::replace_local_source(&source, &workdir)?
            } else {
                source
            };
            if let Some(cmd) = volumes::raw_compose_bind_mkdir_command(&source)? {
                commands.push(cmd);
            }
        }
        let mut labels = php::collect(php::get(&service, "labels"));
        let contains = |labels: &[(Key, PhpValue)], needle: &str| {
            labels
                .iter()
                .any(|(_, v)| php::loose_eq(v, &PhpValue::Str(needle.to_owned())))
        };
        if !contains(&labels, "coolify.managed") {
            php::push(&mut labels, PhpValue::from("coolify.managed=true"));
        }
        let app_label = format!("coolify.applicationUuid={uuid}");
        if !contains(&labels, &app_label) {
            php::push(&mut labels, PhpValue::Str(app_label));
        }
        if !contains(&labels, "coolify.type") {
            php::push(&mut labels, PhpValue::from("coolify.type=application"));
        }
        let mut service = match service {
            PhpValue::Array(items) => items,
            _ => Vec::new(),
        };
        php::put_str(&mut service, "labels", PhpValue::Array(labels));
        out_services.push((name, PhpValue::Array(service)));
    }
    match &mut yaml {
        PhpValue::Array(items) => php::put_str(items, "services", PhpValue::Array(out_services)),
        other => *other = PhpValue::map([("services", PhpValue::Array(out_services))]),
    }
    Ok((
        coolify_compat::yaml::dump_with_indent(&yaml, 10, 2),
        commands,
    ))
}

/// `$application->oldRawParser()`: returns the new `docker_compose_raw`. Upstream only sets the
/// attribute (no save); the caller decides when it is persisted.
pub async fn old_raw_parser(
    jobs: &Jobs,
    app_id: i64,
    now: NaiveDateTime,
) -> Result<String, String> {
    let run = Run::load(jobs, app_id, now).await?;
    let raw = run.docker_compose_raw().unwrap_or_default().to_owned();
    let (compose, commands) = raw_compose(&raw, &run.app().uuid)?;
    if !commands.is_empty()
        && let Some(server_id) = run.app().server_id()
        && let Ok(Some(row)) = coolify_db::servers::find_by_id(run.db(), server_id).await
        && let Ok(target) = crate::server::ServerInfo::from_row(&row).target(jobs).await
    {
        let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
        let _ = jobs
            .remote()
            .run_quiet(
                &target,
                &refs,
                RunOptions::op("application.raw_compose_mkdir"),
            )
            .await;
    }
    Ok(compose)
}
