//! `DockerCleanupJob` and `CleanupDocker`.

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use coolify_db::server_ops::{self as ops, CleanupUpdate};
use coolify_domain::new_public_id;
use serde_json::{Value, json};

use super::{MARKS, Srv, helper_version, shell_arg};
use crate::Jobs;
use crate::notifications::{self, Notification, lookup};

/// `CleanupDocker::REMOTE_COMMANDS_DEADLINE`.
const REMOTE_COMMANDS_DEADLINE: u64 = 540;
/// `CleanupDocker::STOP_CLEANUP_COOLDOWN`.
const STOP_CLEANUP_COOLDOWN: u64 = 3600;
const OVERLAP_LOCK_SECONDS: u64 = 660;

/// `preg_replace('/([.\\\\+*?\[\]^$(){}|])/', '\\\\$1', $v)`.
fn ere_escape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        if ".\\+*?[]^$(){}|".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `buildContainerPruneCommand`.
fn container_prune_command() -> String {
    "docker ps -a --filter status=created --filter status=exited --filter status=dead --filter label=coolify.managed=true --format '{{.ID}} coolify.proxy={{.Label \"coolify.proxy\"}} coolify.type={{.Label \"coolify.type\"}}' | grep -v -E 'coolify\\.proxy=true |coolify\\.type=(database|application|service)$' | awk '{print $1}' | xargs -r docker rm".to_string()
}

/// `buildCoolifyImageCleanupCommand`.
fn coolify_image_cleanup_command(helper: &str) -> String {
    let escaped = ere_escape(helper);
    format!(
        "docker images --format '{{{{.Repository}}}}:{{{{.Tag}}}}' | grep -E '(^|/)coollabsio/(coolify-helper|coolify-realtime|sentinel):' | grep -v -E ':<none>$|(^|/)coollabsio/coolify-helper:{escaped}$' | xargs -r docker rmi 2>/dev/null || true"
    )
}

/// `buildImagePruneCommand`.
fn image_prune_command(repos: &[String], helper: &str) -> String {
    let escaped = ere_escape(helper);
    let infra = format!("(^|/)coollabsio/coolify-helper:{escaped}$");
    let mut grep = "grep -v '<none>'".to_string();
    if !repos.is_empty() {
        let patterns = repos
            .iter()
            .map(|r| ere_escape(r))
            .collect::<Vec<_>>()
            .join("|");
        grep.push_str(&format!(
            " | grep -v -E {}",
            shell_arg(&format!("^({patterns})[_:@].+"))
        ));
    }
    grep.push_str(&format!(" | grep -v -E {}", shell_arg(&infra)));
    format!(
        "docker image prune -f && docker images --digests --format '{{{{if eq .Tag \"<none>\"}}}}{{{{.Repository}}}}@{{{{.Digest}}}}{{{{else}}}}{{{{.Repository}}}}:{{{{.Tag}}}}{{{{end}}}}' | {grep} | xargs -r -I {{}} sh -c 'docker inspect --format \"{{{{index .Config.Labels \\\"coolify.managed\\\"}}}}\" \"{{}}\" 2>/dev/null | grep -q true || docker rmi \"{{}}\" 2>/dev/null' || true"
    )
}

/// `railpackBuilderHelperCommand` with `railpackBuilderLockedScript`.
fn railpack_prune_command(helper_image: &str) -> String {
    let command = "docker buildx inspect --bootstrap coolify-railpack >/dev/null 2>&1; docker buildx prune --builder coolify-railpack -af";
    let script = format!(
        "exec 9>/root/.docker/buildx/coolify-railpack.lock; if flock -s 9; then :; else exit $?; fi; {command}; status=$?; if flock -n -x 9; then DOCKER_CONFIG=/root/.docker docker buildx stop coolify-railpack >/dev/null 2>&1; fi; exit $status"
    );
    format!(
        "docker run --rm -v $HOME/.docker/buildx:/root/.docker/buildx -v /var/run/docker.sock:/var/run/docker.sock {helper_image} bash -c '{script}' 2>/dev/null || true"
    )
}

struct Cleanup<'a> {
    srv: &'a Srv,
    deadline: Instant,
}

impl Cleanup<'_> {
    /// `runRemoteCommand`.
    async fn run(&self, command: &str) -> Result<Option<String>, String> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining < Duration::from_secs(1) {
            return Err(format!(
                "Docker cleanup did not finish within {REMOTE_COMMANDS_DEADLINE} seconds. The remaining cleanup commands were not run."
            ));
        }
        Ok(self
            .srv
            .run_timeout(&[command.to_string()], "docker.cleanup", remaining)
            .await)
    }

    /// `cleanupApplicationImages`.
    async fn application_images(
        &self,
        apps: &[ops::CleanupApplication],
        log: &mut Vec<Value>,
    ) -> Result<(), String> {
        if apps.is_empty() {
            return Ok(());
        }
        let srv = self.srv;
        let disable = srv.info.setting_bool("disable_application_image_retention") == Some(true);
        let containers = self
            .run("docker ps -a --format '{{.Names}}#{{.Image}}' 2>/dev/null || true")
            .await?
            .unwrap_or_default();
        let container_images: HashSet<String> = containers
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.split_once('#').map_or("", |(_, i)| i).to_string())
            .collect();
        let listing = self
            .run("docker images --format '{{.Repository}}#{{.Tag}}#{{.CreatedAt}}' 2>/dev/null || true")
            .await?
            .unwrap_or_default();
        #[derive(Clone)]
        struct Image {
            repository: String,
            tag: String,
            created_at: String,
            image_ref: String,
        }
        let images: Vec<Image> = listing
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|l| {
                let mut p = l.splitn(3, '#');
                let repository = p.next().unwrap_or_default().to_string();
                let tag = p.next().unwrap_or_default().to_string();
                let created_at = p.next().unwrap_or_default().to_string();
                (!tag.is_empty() && tag != "<none>").then(|| Image {
                    image_ref: format!("{repository}:{tag}"),
                    repository,
                    tag,
                    created_at,
                })
            })
            .collect();
        let timeout = srv.setting_int("dynamic_timeout").unwrap_or(3600);
        let deploying: HashSet<String> =
            ops::deploying_applications(srv.db(), timeout, Jobs::now())
                .await
                .map_err(|e| e.to_string())?
                .into_iter()
                .collect();
        let mut to_delete: Vec<String> = Vec::new();
        for (id, uuid, registry, keep, _) in apps {
            if deploying.contains(&id.to_string()) {
                continue;
            }
            let images_to_keep = if disable { 0 } else { keep.unwrap_or(2) } as usize;
            let repo = registry.as_deref().unwrap_or(uuid);
            let mut repositories: BTreeMap<String, Vec<Image>> = BTreeMap::new();
            for i in images.iter().filter(|i| {
                i.repository.starts_with(repo) && !i.repository[repo.len()..].contains('/')
            }) {
                repositories
                    .entry(i.repository.clone())
                    .or_default()
                    .push(i.clone());
            }
            for group in repositories.values() {
                let current: Vec<&str> = group
                    .iter()
                    .filter(|i| container_images.contains(&i.image_ref))
                    .map(|i| i.tag.as_str())
                    .collect();
                let mut regular: Vec<&Image> = group
                    .iter()
                    .filter(|i| !i.tag.starts_with("pr-") && !i.tag.ends_with("-build"))
                    .filter(|i| !current.contains(&i.tag.as_str()))
                    .collect();
                regular.sort_by(|a, b| b.created_at.cmp(&a.created_at));
                let mut kept: Vec<&str> = regular
                    .iter()
                    .take(images_to_keep)
                    .map(|i| i.tag.as_str())
                    .collect();
                kept.extend(&current);
                to_delete.extend(
                    group
                        .iter()
                        .filter(|i| i.tag.starts_with("pr-"))
                        .map(|i| i.image_ref.clone()),
                );
                to_delete.extend(
                    regular
                        .iter()
                        .skip(images_to_keep)
                        .map(|i| i.image_ref.clone()),
                );
                to_delete.extend(
                    group
                        .iter()
                        .filter(|i| !i.tag.starts_with("pr-") && i.tag.ends_with("-build"))
                        .filter(|i| !kept.contains(&i.tag.strip_suffix("-build").unwrap_or(&i.tag)))
                        .map(|i| i.image_ref.clone()),
                );
            }
        }
        let mut seen = HashSet::new();
        to_delete.retain(|r| seen.insert(r.clone()));
        for chunk in to_delete.chunks(100) {
            let command = format!(
                "docker rmi {} 2>/dev/null || true",
                chunk
                    .iter()
                    .map(|r| shell_arg(r))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            let output = self.run(&command).await?;
            log.push(json!({
                "command": command,
                "output": coolify_compat::redact::redact_text(
                    output.as_deref().unwrap_or("Images removed or were in use"), &[], &[]),
            }));
        }
        Ok(())
    }
}

/// `CleanupDocker::handle`.
pub async fn cleanup_docker(
    srv: &Srv,
    volumes: bool,
    networks: bool,
) -> Result<Vec<Value>, String> {
    let c = Cleanup {
        srv,
        deadline: Instant::now() + Duration::from_secs(REMOTE_COMMANDS_DEADLINE),
    };
    let settings = ops::instance_server_settings(srv.db())
        .await
        .map_err(|e| e.to_string())?;
    let helper = helper_version(settings.helper_version.as_deref());
    let registry = settings
        .docker_registry_url
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "docker.io".to_string());
    let helper_image = format!("{registry}/coollabsio/coolify-helper:{helper}");
    let mut log: Vec<Value> = Vec::new();
    let apps = ops::applications_for_cleanup(srv.db(), srv.id())
        .await
        .map_err(|e| e.to_string())?;
    let mut repos: Vec<String> = Vec::new();
    for (_, uuid, registry, _, _) in &apps {
        let r = registry.clone().unwrap_or_else(|| uuid.clone());
        if !repos.contains(&r) {
            repos.push(r);
        }
    }
    c.application_images(&apps, &mut log).await?;
    let mut commands = vec![
        container_prune_command(),
        image_prune_command(&repos, &helper),
        "docker builder prune -af".to_string(),
        railpack_prune_command(&helper_image),
        coolify_image_cleanup_command(&helper),
    ];
    if volumes {
        commands.push("docker volume prune -af".to_string());
    }
    if networks {
        commands.push("docker network prune -f".to_string());
    }
    for command in commands {
        if let Some(output) = c.run(&command).await? {
            log.push(json!({
                "command": command,
                "output": coolify_compat::redact::redact_text(&output, &[], &[]),
            }));
        }
    }
    MARKS.put(
        format!("docker-cleanup-last-run-{}", srv.row.uuid),
        "1".into(),
        Duration::from_secs(STOP_CLEANUP_COOLDOWN),
    );
    Ok(log)
}

async fn disk_usage(srv: &Srv) -> Option<String> {
    srv.run_quiet(
        &["df / --output=pcent | tr -cd 0-9".to_string()],
        "docker.cleanup.disk",
    )
    .await
}

fn usage_int(v: Option<&str>) -> i64 {
    v.and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

/// `DockerCleanupJob::handle`.
pub async fn docker_cleanup_job(
    jobs: &Jobs,
    server_id: i64,
    manual: bool,
    volumes: bool,
    networks: bool,
) -> Result<(), String> {
    let srv = Srv::load(jobs, server_id).await?;
    let lock = format!("docker-cleanup-{}", srv.row.uuid);
    // A manual run waits for a running cleanup; other runs are dropped.
    let mut waited = 0u64;
    while !MARKS.add(lock.clone(), Duration::from_secs(OVERLAP_LOCK_SECONDS)) {
        if !manual || waited >= OVERLAP_LOCK_SECONDS + 120 {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
        waited += 60;
    }
    let result = run_job(&srv, manual, volumes, networks).await;
    MARKS.forget(&lock);
    result
}

async fn run_job(srv: &Srv, manual: bool, volumes: bool, networks: bool) -> Result<(), String> {
    let db = srv.db();
    let now = Jobs::now;
    let id = ops::create_cleanup_execution(db, &new_public_id(), srv.id(), now())
        .await
        .map_err(|e| e.to_string())?;
    async fn write(db: &coolify_db::Db, id: i64, u: CleanupUpdate<'_>) {
        let _ = ops::update_cleanup_execution(db, id, &u, Jobs::now()).await;
    }
    if !srv.info.is_functional() {
        write(
            db,
            id,
            CleanupUpdate {
                status: Some("failed"),
                message: Some("Server is not functional (unreachable, unusable, or disabled)"),
                finished_at: Some(now()),
                ..CleanupUpdate::default()
            },
        )
        .await;
        return Ok(());
    }
    // `$this->server->team?->notify(new DockerCleanupSuccess|DockerCleanupFailed(...))`.
    let notify = |message: String, success: bool| async move {
        if let Some((team_id, server)) = lookup::server(db, srv.id()).await {
            let n = if success {
                Notification::DockerCleanupSuccess { server, message }
            } else {
                Notification::DockerCleanupFailed { server, message }
            };
            notifications::notify(db, srv.jobs.0.encrypter.as_ref(), team_id, n).await;
        }
    };
    let inner = async {
        if !manual {
            let timeout = srv.setting_int("dynamic_timeout").unwrap_or(3600);
            if let Some(uuid) = ops::active_deployment(db, srv.id(), timeout, now())
                .await
                .map_err(|e| e.to_string())?
            {
                let message = format!(
                    "Docker cleanup skipped: deployment {uuid} is queued or running on this server. The next scheduled cleanup runs as usual."
                );
                write(
                    db,
                    id,
                    CleanupUpdate {
                        status: Some("success"),
                        message: Some(&message),
                        ..CleanupUpdate::default()
                    },
                )
                .await;
                return Ok::<(), String>(());
            }
        }
        let before = disk_usage(srv).await;
        let forced = srv.info.setting_bool("force_docker_cleanup") == Some(true);
        if manual || forced {
            let log = cleanup_docker(srv, volumes, networks).await?;
            let after = disk_usage(srv).await;
            let message = format!(
                "{} Docker cleanup job executed successfully. Disk usage before: {}%, Disk usage after: {}%.",
                if manual { "Manual" } else { "Forced" },
                before.as_deref().unwrap_or_default(),
                after.as_deref().unwrap_or_default()
            );
            let text = coolify_compat::php_json::encode(&Value::Array(log));
            write(
                db,
                id,
                CleanupUpdate {
                    status: Some("success"),
                    message: Some(&message),
                    cleanup_log: Some(&text),
                    ..CleanupUpdate::default()
                },
            )
            .await;
            notify(message, true).await;
            return Ok(());
        }
        if before.as_deref().is_none_or(str::is_empty) {
            let log = cleanup_docker(srv, volumes, networks).await?;
            let text = coolify_compat::php_json::encode(&Value::Array(log));
            let message =
                "Docker cleanup job executed successfully, but no disk usage could be determined.";
            write(
                db,
                id,
                CleanupUpdate {
                    status: Some("success"),
                    message: Some(message),
                    cleanup_log: Some(&text),
                    ..CleanupUpdate::default()
                },
            )
            .await;
            notify(message.to_owned(), true).await;
            return Ok(());
        }
        let threshold = srv.setting_int("docker_cleanup_threshold").unwrap_or(80);
        if usage_int(before.as_deref()) >= threshold {
            let log = cleanup_docker(srv, volumes, networks).await?;
            let after = disk_usage(srv).await;
            let saved = usage_int(before.as_deref()) - usage_int(after.as_deref());
            let b = before.as_deref().unwrap_or_default();
            let a = after.as_deref().unwrap_or_default();
            let message = if saved > 0 {
                format!(
                    "Saved {saved}% disk space. Disk usage before: {b}%, Disk usage after: {a}%."
                )
            } else {
                format!(
                    "Docker cleanup job executed successfully, but no disk space was saved. Disk usage before: {b}%, Disk usage after: {a}%."
                )
            };
            let text = coolify_compat::php_json::encode(&Value::Array(log));
            write(
                db,
                id,
                CleanupUpdate {
                    status: Some("success"),
                    message: Some(&message),
                    cleanup_log: Some(&text),
                    ..CleanupUpdate::default()
                },
            )
            .await;
            notify(message, true).await;
        } else {
            let name = srv.str_column("name").unwrap_or_default();
            let message = format!("No cleanup needed for {name}");
            write(
                db,
                id,
                CleanupUpdate {
                    status: Some("success"),
                    message: Some(&message),
                    ..CleanupUpdate::default()
                },
            )
            .await;
            notify(message, true).await;
        }
        Ok(())
    };
    let result = inner.await;
    if let Err(e) = &result {
        let message = coolify_compat::redact::redact_text(e, &[], &[]);
        write(
            db,
            id,
            CleanupUpdate {
                status: Some("failed"),
                message: Some(&message),
                ..CleanupUpdate::default()
            },
        )
        .await;
        notify(
            format!("Docker cleanup job failed with the following error: {message}"),
            false,
        )
        .await;
    }
    write(
        db,
        id,
        CleanupUpdate {
            finished_at: Some(now()),
            ..CleanupUpdate::default()
        },
    )
    .await;
    result
}

impl Jobs {
    /// `DockerCleanupJob::dispatch($server, $manual, $volumes, $networks)`.
    pub fn spawn_docker_cleanup(
        &self,
        server_id: i64,
        manual: bool,
        volumes: bool,
        networks: bool,
    ) {
        self.spawn_server_job("docker-cleanup", server_id, move |jobs, id| async move {
            docker_cleanup_job(&jobs, id, manual, volumes, networks).await
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_commands() {
        assert!(image_prune_command(&["a.b".into()], "1.0.19").contains(r"'^(a\.b)[_:@].+'"));
        assert!(coolify_image_cleanup_command("1.0.19").contains(r"coolify-helper:1\.0\.19$"));
    }
}
