//! `ConfigureTrafficAnalytics` with `applyTrafficAnalyticsToProxyConfiguration`.

use coolify_compat::yaml::{self, PhpValue};
use coolify_compat::yaml_parse;
use coolify_db::servers::ColumnValue;
use serde_json::{Value, json};

use super::{Srv, proxy_config, sentinel};
use crate::Jobs;

const USER_COMMANDS: &str = "traffic_analytics_user_accesslog_commands";
const STOPPED_PROXY_STATUSES: [&str; 4] = ["exited", "stopped", "stopping", "dead"];

/// `isTraefikAccessLogCommand`.
fn is_access_log_flag(command: &str) -> bool {
    let flag = command.split('=').next().unwrap_or_default().to_lowercase();
    flag == "--accesslog" || flag.starts_with("--accesslog.")
}

/// `isCaddyTrafficVolume`.
fn is_caddy_traffic_volume(volume: &PhpValue) -> bool {
    match volume {
        PhpValue::Str(v) => {
            let parts: Vec<&str> = v.split(':').collect();
            parts.len() >= 2 && parts[1].trim_end_matches('/') == "/traffic"
        }
        PhpValue::Array(items) => items.iter().any(|(k, v)| {
            k.as_string() == "target"
                && matches!(v, PhpValue::Str(t) if t.trim_end_matches('/') == "/traffic")
        }),
        _ => false,
    }
}

/// `applyTraefikAccessLogCommands` on the list of a stored configuration.
async fn apply_traefik_commands(
    srv: &mut Srv,
    commands: Vec<PhpValue>,
    enabled: bool,
) -> Result<Vec<PhpValue>, String> {
    let managed = proxy_config::traefik_access_log_commands();
    let is_managed = |c: &PhpValue| matches!(c, PhpValue::Str(s) if managed.contains(s));
    let stored = srv
        .proxy
        .get(USER_COMMANDS)
        .and_then(Value::as_array)
        .cloned();
    let mut user: Vec<Value> = stored.clone().unwrap_or_default();
    let has_all = managed.iter().all(|m| {
        commands
            .iter()
            .any(|c| matches!(c, PhpValue::Str(s) if s == m))
    });
    let mut commands = commands;
    if stored.is_some() || has_all {
        commands.retain(|c| !is_managed(c));
    }
    if enabled {
        for c in &commands {
            if let PhpValue::Str(s) = c
                && is_access_log_flag(s)
                && !user.iter().any(|u| u.as_str() == Some(s))
            {
                user.push(json!(s));
            }
        }
        commands.retain(|c| !matches!(c, PhpValue::Str(s) if is_access_log_flag(s)));
        commands.extend(managed.iter().map(|m| PhpValue::Str(m.clone())));
        if stored.as_ref() != Some(&user) {
            srv.proxy_set(USER_COMMANDS, Value::Array(user));
            srv.save_proxy().await?;
        }
    } else if stored.is_some() {
        for c in user.iter().filter_map(Value::as_str) {
            if !commands
                .iter()
                .any(|x| matches!(x, PhpValue::Str(s) if s == c))
            {
                commands.push(PhpValue::Str(c.to_string()));
            }
        }
        srv.proxy.shift_remove(USER_COMMANDS);
        srv.save_proxy().await?;
    }
    Ok(commands)
}

fn list_values(value: Option<&PhpValue>, error: &str) -> Result<Vec<PhpValue>, String> {
    match value {
        None => Ok(Vec::new()),
        Some(PhpValue::Array(items)) => Ok(items.iter().map(|(_, v)| v.clone()).collect()),
        Some(_) => Err(error.to_string()),
    }
}

fn service_mut<'a>(config: &'a mut PhpValue, name: &str) -> Option<&'a mut PhpValue> {
    config.get_mut("services")?.get_mut(name)
}

/// `applyTrafficAnalyticsToProxyConfiguration`.
pub async fn apply_to_configuration(
    srv: &mut Srv,
    configuration: &str,
    enabled: bool,
) -> Result<String, String> {
    let mut config = yaml_parse::parse(configuration, 128).map_err(|e| e.message())?;
    if !matches!(config, PhpValue::Array(_)) {
        return Err("Proxy configuration must be a YAML mapping.".to_string());
    }
    match srv.proxy_type().map(str::to_string).as_deref() {
        Some("TRAEFIK") => {
            let current = match service_mut(&mut config, "traefik") {
                Some(PhpValue::Array(items)) => items
                    .iter()
                    .find(|(k, _)| k.as_string() == "command")
                    .map(|(_, v)| v.clone()),
                _ => None,
            };
            let commands = list_values(current.as_ref(), "Traefik commands must be a YAML list.")?;
            let updated = apply_traefik_commands(srv, commands, enabled).await?;
            if let Some(traefik) = service_mut(&mut config, "traefik") {
                traefik.set("command", PhpValue::list(updated));
            }
            if let Some(services) = config.get_mut("services") {
                services.remove("traefik-logrotate");
                if enabled && !srv.info.is_swarm() {
                    services.set(
                        "traefik-logrotate",
                        proxy_config::traefik_logrotate_service(&srv.proxy_path()),
                    );
                }
            }
        }
        Some("CADDY") => {
            let volume = format!("{}:/traffic", sentinel::traffic_log_directory(srv));
            let current = match service_mut(&mut config, "caddy") {
                Some(PhpValue::Array(items)) => items
                    .iter()
                    .find(|(k, _)| k.as_string() == "volumes")
                    .map(|(_, v)| v.clone()),
                _ => None,
            };
            let mut volumes = list_values(current.as_ref(), "Caddy volumes must be a YAML list.")?;
            volumes.retain(|v| !is_caddy_traffic_volume(v));
            if enabled {
                volumes.push(PhpValue::Str(volume));
            }
            if let Some(caddy) = service_mut(&mut config, "caddy") {
                caddy.set("volumes", PhpValue::list(volumes));
            }
        }
        _ => {}
    }
    Ok(yaml::dump_with_indent(&config, 12, 2))
}

/// `Server::trafficAnalyticsUnsupportedReason`.
pub fn unsupported_reason(srv: &Srv) -> Option<&'static str> {
    if srv.info.is_swarm() || srv.info.is_build_server() {
        return Some("Traffic analytics is not supported on Swarm/Build servers.");
    }
    if !matches!(srv.proxy_type(), Some("TRAEFIK" | "CADDY")) {
        return Some("Traffic analytics needs the Traefik or Caddy proxy.");
    }
    None
}

/// `ConfigureTrafficAnalytics::handle`: whether a proxy restart was queued.
///
/// Not ported: regenerating the read-only labels of applications on Caddy
/// (`regenerateReadonlyApplicationLabels`); deployments generate them when labels are not stored.
pub async fn configure(jobs: &Jobs, server_id: i64, enable: bool) -> Result<bool, String> {
    let mut srv = Srv::load(jobs, server_id).await?;
    if enable && let Some(reason) = unsupported_reason(&srv) {
        return Err(reason.to_string());
    }
    let sentinel_was_enabled = srv.info.setting_bool("is_sentinel_enabled") == Some(true);
    let has_proxy = matches!(srv.proxy_type(), Some("TRAEFIK" | "CADDY"));
    if has_proxy {
        let previous = srv.proxy.clone();
        let saved = async {
            let configuration = proxy_config::get_proxy_configuration(&mut srv, false).await?;
            proxy_config::ports_from_configuration(&configuration).map_err(|e| e.0)?;
            let updated = apply_to_configuration(&mut srv, &configuration, enable).await?;
            proxy_config::save_proxy_configuration(&mut srv, &updated)
                .await
                .map_err(|e| match e {
                    proxy_config::SaveError::Invalid(i) => i.0,
                    proxy_config::SaveError::Other(o) => o,
                })
        }
        .await;
        if let Err(e) = saved {
            srv.proxy = previous;
            let _ = srv.save_proxy().await;
            return Err(e);
        }
    }
    srv.settings_update(&[("is_traffic_analytics_enabled", ColumnValue::Bool(enable))])
        .await?;
    let force_stop = srv
        .proxy
        .get("force_stop")
        .is_some_and(|v| v.as_bool() == Some(true));
    let status = srv
        .proxy
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let restart = has_proxy && !force_stop && !STOPPED_PROXY_STATUSES.contains(&status);
    if restart {
        jobs.spawn_restart_proxy(server_id);
    }
    if enable || sentinel_was_enabled {
        sentinel::start_sentinel(jobs, server_id, true, None).await?;
    }
    Ok(restart)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caddy_traffic_volume() {
        assert!(is_caddy_traffic_volume(&PhpValue::Str(
            "/x:/traffic".into()
        )));
        assert!(is_caddy_traffic_volume(&PhpValue::Str(
            "/x:/traffic/:ro".into()
        )));
        assert!(!is_caddy_traffic_volume(&PhpValue::Str("/x:/data".into())));
    }

    #[test]
    fn access_log_flags() {
        assert!(is_access_log_flag("--accesslog=true"));
        assert!(is_access_log_flag("--accesslog.format=json"));
        assert!(!is_access_log_flag("--accesslogx"));
    }
}
