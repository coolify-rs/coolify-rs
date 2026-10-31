//! `ConfigureCloudflared`, `UpdateCloudflared` and `CloudflareTunnelChangedNotification`.

use std::time::Duration;

use base64::Engine as _;
use coolify_compat::yaml::{self, PhpValue};
use coolify_db::servers::{self, ColumnValue};

use super::Srv;
use super::proxy::{queue_activity, run_activity};
use crate::Jobs;

/// `ConfigureCloudflared::DIRECTORY`.
const DIRECTORY: &str = "/data/coolify/cloudflared";
const TUNNEL_NOT_CONFIGURED: &str = "The cloudflared configuration was not found on the server. Configure the tunnel again with the automated setup.";

/// `ConfigureCloudflared::handle`: the Compose file.
pub fn compose(token: &str) -> String {
    let health = PhpValue::map([
        (
            "test",
            PhpValue::strings([
                "CMD",
                "cloudflared",
                "tunnel",
                "--metrics",
                "127.0.0.1:60123",
                "ready",
            ]),
        ),
        ("interval", PhpValue::Str("5s".to_string())),
        ("timeout", PhpValue::Str("30s".to_string())),
        ("retries", PhpValue::Int(5)),
        ("start_period", PhpValue::Str("120s".to_string())),
    ]);
    let service = PhpValue::map([
        (
            "container_name",
            PhpValue::Str("coolify-cloudflared".to_string()),
        ),
        (
            "image",
            PhpValue::Str("cloudflare/cloudflared:latest".to_string()),
        ),
        ("restart", PhpValue::Str("unless-stopped".to_string())),
        ("network_mode", PhpValue::Str("host".to_string())),
        ("command", PhpValue::Str("tunnel run".to_string())),
        (
            "environment",
            PhpValue::strings([
                format!("TUNNEL_TOKEN={token}"),
                "TUNNEL_METRICS=127.0.0.1:60123".to_string(),
            ]),
        ),
        ("healthcheck", health),
    ]);
    let config = PhpValue::map([(
        "services",
        PhpValue::map([("coolify-cloudflared", service)]),
    )]);
    yaml::dump_with_indent(&config, 12, 2)
}

/// `ConfigureCloudflared::handle`: the commands.
pub fn configure_commands(token: &str) -> Vec<String> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(compose(token));
    vec![
        format!("mkdir -p {DIRECTORY}"),
        format!("cd {DIRECTORY}"),
        format!("echo '{encoded}' | base64 -d | tee docker-compose.yml > /dev/null"),
        "echo Pulling latest Cloudflare Tunnel image.".to_string(),
        "docker compose pull".to_string(),
        "echo Stopping existing Cloudflare Tunnel container.".to_string(),
        "docker rm -f coolify-cloudflared || true".to_string(),
        "echo Starting new Cloudflare Tunnel container.".to_string(),
        "docker compose up --wait --wait-timeout 180 --remove-orphans || docker logs coolify-cloudflared"
            .to_string(),
    ]
}

/// `CloudflareTunnelChangedNotification::handle`.
async fn tunnel_changed(jobs: &Jobs, server_id: i64, ssh_domain: &str) -> Result<(), String> {
    let srv = Srv::load(jobs, server_id).await?;
    if srv.info.transferred {
        return Ok(());
    }
    let mut healthy = false;
    for attempt in 1..=3 {
        let result = srv
            .run_timeout(
                &[
                    r#"docker inspect coolify-cloudflared | jq -e ".[0].State.Health.Status == \"healthy\"""#
                        .to_string(),
                ],
                "cloudflared.health",
                Duration::from_secs(10),
            )
            .await
            .unwrap_or_default();
        if result.trim() == "true" {
            healthy = true;
            break;
        }
        if attempt < 3 {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
    if !healthy {
        tracing::error!(server_id, "cloudflared failed all health checks");
        return Ok(());
    }
    servers::update_settings(
        srv.db(),
        server_id,
        &[("is_cloudflare_tunnel", ColumnValue::Bool(true))],
        Jobs::now(),
    )
    .await
    .map_err(|e| e.to_string())?;
    if srv.str_column("ip") != Some(ssh_domain) && !ssh_domain.is_empty() {
        let previous = srv.str_column("ip").map(str::to_string);
        servers::update_columns(
            srv.db(),
            server_id,
            &[
                ("ip", ColumnValue::Text(Some(ssh_domain.to_string()))),
                ("ip_previous", ColumnValue::Text(previous)),
            ],
            Jobs::now(),
        )
        .await
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// `ConfigureCloudflared::run`: the id of the activity, which ends with `CloudflareTunnelChanged`.
pub async fn configure_cloudflared(
    srv: &Srv,
    token: &str,
    ssh_domain: &str,
) -> Result<i64, String> {
    let commands = configure_commands(token);
    let activity = queue_activity(srv, "cloudflared.configure", None).await?;
    let (jobs, id, domain) = (srv.jobs.clone(), srv.id(), ssh_domain.to_string());
    tokio::spawn(async move {
        let Ok(s) = Srv::load(&jobs, id).await else {
            return;
        };
        let _ = run_activity(&s, activity, &commands, "cloudflared.configure").await;
        if let Err(e) = tunnel_changed(&jobs, id, &domain).await {
            tracing::warn!(server_id = id, error = %e, "cloudflare tunnel change failed");
        }
    });
    Ok(activity)
}

/// `UpdateCloudflared::handle`.
pub async fn update_cloudflared(srv: &Srv) -> Result<i64, String> {
    let directory = srv
        .run_quiet(
            &[format!(
                "for dir in {DIRECTORY} /tmp/cloudflared; do if [ -f \"$dir/docker-compose.yml\" ]; then echo \"$dir\"; break; fi; done"
            )],
            "cloudflared.update",
        )
        .await
        .unwrap_or_default();
    let directory = directory.trim();
    if directory.is_empty() {
        return Err(TUNNEL_NOT_CONFIGURED.to_string());
    }
    let commands = vec![
        format!("cd {directory}"),
        "echo Pulling latest Cloudflare Tunnel image.".to_string(),
        "docker compose pull".to_string(),
        "echo Recreating the Cloudflare Tunnel container in the background. SSH through the tunnel reconnects in a few seconds."
            .to_string(),
        "nohup setsid sh -c 'sleep 2; docker compose up -d --force-recreate --remove-orphans' > /dev/null 2>&1 < /dev/null &"
            .to_string(),
    ];
    let activity = queue_activity(srv, "cloudflared.update", None).await?;
    let (jobs, id) = (srv.jobs.clone(), srv.id());
    tokio::spawn(async move {
        let Ok(s) = Srv::load(&jobs, id).await else {
            return;
        };
        let _ = run_activity(&s, activity, &commands, "cloudflared.update").await;
    });
    Ok(activity)
}

/// `remote_process(['docker rm -f coolify-cloudflared'], ...)`: not waited for.
pub async fn remove_cloudflared(srv: &Srv) -> Result<(), String> {
    let commands = vec!["docker rm -f coolify-cloudflared".to_string()];
    let activity = queue_activity(srv, "cloudflared.remove", None).await?;
    let (jobs, id) = (srv.jobs.clone(), srv.id());
    tokio::spawn(async move {
        let Ok(s) = Srv::load(&jobs, id).await else {
            return;
        };
        let _ = run_activity(&s, activity, &commands, "cloudflared.remove").await;
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// From the reference: `Yaml::dump($config, 12, 2)` of `ConfigureCloudflared`, with the token `abc`.
    #[test]
    fn compose_matches_upstream() {
        assert_eq!(compose("abc"), include_str!("testdata/cloudflared.yml"));
    }
}
