//! `composeProjectMountsCommand()`, `composeContainersWithRemovedVolumes()` and
//! `composeServiceVolumeTargets()` (bootstrap/helpers/docker.php, Coolify v4.7): the containers
//! that `docker compose up` would give a volume that the user deleted in Coolify.

use coolify_compat::yaml::{Key, PhpValue};
use coolify_remote::shell::quote;
use serde_json::Value;

use super::php;

/// `composeProjectMountsCommand($projectName)`: lists the containers of a Compose project, one
/// JSON object per line, for `compose_containers_with_removed_volumes()`.
pub fn compose_project_mounts_command(project_name: &str) -> String {
    let format = r#"{"name":{{json .Name}},"service":{{json (index .Config.Labels "com.docker.compose.service")}},"imageVolumes":{{json .Config.Volumes}},"mounts":{{json .Mounts}}}"#;
    format!(
        "docker ps -aq --filter {} | xargs -r docker inspect --format {} 2>/dev/null || true",
        quote(&format!("label=com.docker.compose.project={project_name}")),
        quote(format)
    )
}

/// `rtrim($path, '/') ?: '/'`.
fn normalize(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "0" {
        "/".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `composeServiceVolumeTargets($volumes)`: the container paths of the volumes of a Compose
/// service, or `None` when a path is not literal (for example `${DATA_PATH}`).
pub fn compose_service_volume_targets(volumes: Option<&PhpValue>) -> Option<Vec<String>> {
    let Some(PhpValue::Array(items)) = volumes else {
        return Some(Vec::new());
    };
    let mut targets = Vec::new();
    for (_, volume) in items {
        let target: Option<String> = match volume {
            PhpValue::Array(_) => match php::get(volume, "target") {
                Some(PhpValue::Str(s)) => Some(s.clone()),
                _ => None,
            },
            other => {
                // source:target:mode, where the source can contain colons, for example ${DATA:-/data}.
                let text = php::to_string(other);
                let mut parts: Vec<&str> = text.split(':').collect();
                if parts.len() > 1
                    && parts.last().is_some_and(|mode| {
                        !mode.is_empty()
                            && mode.chars().all(|c| c.is_ascii_alphabetic() || c == ',')
                    })
                {
                    parts.pop();
                }
                parts.last().map(|p| (*p).to_owned())
            }
        };
        let target = target.filter(|t| t.starts_with('/'))?;
        targets.push(normalize(&target));
    }
    Some(targets)
}

/// `composeContainersWithRemovedVolumes($projectMounts, $composeServices, $uuid, $storageNames)`.
///
/// `docker compose up` gives a recreated container the mount of the old container at each path
/// that the image declares with VOLUME. This also keeps a volume that the user deleted in Coolify,
/// so its old data comes back (#6875). Returns the containers to remove before `up`, as
/// `(container name, volume that the user deleted)` in the order of `project_mounts`.
///
/// Only a volume that Coolify made (its name starts with `{uuid}_`) and that has no storage row
/// anymore counts. A volume that is only removed from the Compose file still has its row, and an
/// external volume or a bind mount is not Coolify's, so Compose keeps using them as before.
pub fn compose_containers_with_removed_volumes(
    project_mounts: &str,
    compose_services: &[(Key, PhpValue)],
    uuid: &str,
    storage_names: &[String],
) -> Vec<(String, String)> {
    let mut containers: Vec<(String, String)> = Vec::new();
    let prefix = format!("{uuid}_");
    for line in project_mounts.trim().lines() {
        let Ok(Value::Object(container)) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let Some(service) = container.get("service").and_then(Value::as_str) else {
            continue;
        };
        let Some(config @ PhpValue::Array(_)) = compose_services
            .iter()
            .find(|(k, _)| k.as_string() == service)
            .map(|(_, v)| v)
        else {
            continue;
        };
        let Some(targets) = compose_service_volume_targets(php::get(config, "volumes")) else {
            continue;
        };
        let image_volumes: Vec<String> = container
            .get("imageVolumes")
            .and_then(Value::as_object)
            .map(|v| v.keys().map(|k| normalize(k)).collect())
            .unwrap_or_default();
        let name = container
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_owned();
        for mount in container
            .get("mounts")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let volume = mount
                .get("Name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let path = normalize(
                mount
                    .get("Destination")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
            if mount.get("Type").and_then(Value::as_str) != Some("volume")
                || !volume.starts_with(&prefix)
                || storage_names.iter().any(|n| n == volume)
                || !image_volumes.contains(&path)
                || targets.contains(&path)
            {
                continue;
            }
            match containers.iter_mut().find(|(n, _)| *n == name) {
                Some(slot) => slot.1 = volume.to_owned(),
                None => containers.push((name.clone(), volume.to_owned())),
            }
        }
    }
    containers
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn services(yaml: &str) -> Vec<(Key, PhpValue)> {
        let parsed = super::super::validate::parse_docker_compose_yaml(yaml).unwrap();
        match php::get(&parsed, "services") {
            Some(PhpValue::Array(items)) => items.clone(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn mounts_command_filters_by_the_compose_project_label() {
        assert_eq!(
            compose_project_mounts_command("abc"),
            r#"docker ps -aq --filter 'label=com.docker.compose.project=abc' | xargs -r docker inspect --format '{"name":{{json .Name}},"service":{{json (index .Config.Labels "com.docker.compose.service")}},"imageVolumes":{{json .Config.Volumes}},"mounts":{{json .Mounts}}}' 2>/dev/null || true"#
        );
    }

    #[test]
    fn volume_targets_are_read_from_short_and_long_syntax() {
        let s = services(
            "services:\n  db:\n    volumes:\n      - data:/var/lib/data/\n      - ${DATA:-/data}:/backup:ro\n      - type: volume\n        source: x\n        target: /long\n  odd:\n    volumes:\n      - data:${TARGET}\n  none:\n    image: x\n",
        );
        let volumes = |name: &str| {
            let config = &s.iter().find(|(k, _)| k.as_string() == name).unwrap().1;
            compose_service_volume_targets(php::get(config, "volumes"))
        };
        assert_eq!(
            volumes("db"),
            Some(vec![
                "/var/lib/data".to_owned(),
                "/backup".to_owned(),
                "/long".to_owned()
            ])
        );
        // A path that is not literal: the service is left alone.
        assert_eq!(volumes("odd"), None);
        assert_eq!(volumes("none"), Some(Vec::new()));
    }

    #[test]
    fn a_container_that_keeps_a_deleted_coolify_volume_is_removed() {
        let s = services(
            "services:\n  db:\n    image: postgres\n    volumes:\n      - kept:/kept\n  web:\n    image: nginx\n",
        );
        let mounts = [
            // The image declares /var/lib/postgresql/data, the compose file no longer mounts it,
            // and the storage row of abc_pgdata is gone.
            r#"{"name":"/db-abc","service":"db","imageVolumes":{"/var/lib/postgresql/data":{}},"mounts":[{"Type":"volume","Name":"abc_pgdata","Destination":"/var/lib/postgresql/data"},{"Type":"volume","Name":"abc_kept","Destination":"/kept"}]}"#,
            // Its storage row still exists.
            r#"{"name":"/web-abc","service":"web","imageVolumes":{"/usr/share/nginx/html/":{}},"mounts":[{"Type":"volume","Name":"abc_html","Destination":"/usr/share/nginx/html"}]}"#,
            // Not a service of the compose file.
            r#"{"name":"/old-abc","service":"old","imageVolumes":{"/data":{}},"mounts":[{"Type":"volume","Name":"abc_old","Destination":"/data"}]}"#,
            "not json",
        ]
        .join("\n");
        let storage = vec!["abc_html".to_owned(), "abc_kept".to_owned()];
        assert_eq!(
            compose_containers_with_removed_volumes(&mounts, &s, "abc", &storage),
            vec![("db-abc".to_owned(), "abc_pgdata".to_owned())]
        );
        // An external volume, a bind mount and an anonymous volume are not Coolify's.
        let foreign = r#"{"name":"/db-abc","service":"db","imageVolumes":{"/data":{},"/bind":{}},"mounts":[{"Type":"volume","Name":"external","Destination":"/data"},{"Type":"bind","Name":"","Destination":"/bind"}]}"#;
        assert!(compose_containers_with_removed_volumes(foreign, &s, "abc", &[]).is_empty());
        // The compose file mounts the path again: Compose uses the new volume.
        let remounted = r#"{"name":"/db-abc","service":"db","imageVolumes":{"/kept":{}},"mounts":[{"Type":"volume","Name":"abc_gone","Destination":"/kept"}]}"#;
        assert!(compose_containers_with_removed_volumes(remounted, &s, "abc", &[]).is_empty());
        assert!(compose_containers_with_removed_volumes("", &s, "abc", &[]).is_empty());
    }
}
