//! Traefik's `acme.json`: `GetTraefikCertificates`, `TraefikAcmeService`, `DeleteTraefikCertificate`,
//! `SaveTraefikAcmeFile`, `ListTraefikAcmeBackups`, `RestoreTraefikAcmeBackup` and `DeleteTraefikAcmeBackup`.

use base64::Engine as _;
use coolify_compat::php_json;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{Srv, certificate, shell_arg};

/// `GetTraefikCertificates::MAX_FILE_SIZE_BYTES`.
const MAX_FILE_SIZE_BYTES: usize = 10 * 1024 * 1024;
/// `ListTraefikAcmeBackups::KEEP`.
pub const KEEP_BACKUPS: usize = 10;
const SHELL_NAME_PATTERN: &str = "^acme\\.json\\.backup-[0-9]{8}T[0-9]{6}Z-[0-9a-f]{8}$";

fn is_traefik(srv: &Srv) -> bool {
    srv.proxy.get("type").and_then(Value::as_str) == Some("TRAEFIK")
}

/// `ListTraefikAcmeBackups::directory`.
fn directory(srv: &Srv) -> String {
    srv.proxy_path().trim_end_matches('/').to_string()
}

/// `bin2hex(random_bytes($n))`.
fn random_hex(bytes: usize) -> String {
    (0..bytes)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect()
}

/// `GetTraefikCertificates::contents`.
pub async fn contents(srv: &Srv) -> Result<Option<Vec<u8>>, String> {
    let path = shell_arg(&format!("{}/acme.json", directory(srv)));
    let script = format!(
        "if [ ! -f {path} ]; then exit 0; elif [ \"$(wc -c < {path})\" -gt {MAX_FILE_SIZE_BYTES} ]; then echo '__COOLIFY_ACME_FILE_TOO_LARGE__'; else head -c {} {path} | base64 | tr -d '\\n'; fi",
        MAX_FILE_SIZE_BYTES + 1
    );
    let out = srv
        .run_quiet(&[format!("sh -c {}", shell_arg(&script))], "proxy.acme")
        .await
        .unwrap_or_default();
    let out = out.trim();
    if out.is_empty() {
        return Ok(None);
    }
    if out == "__COOLIFY_ACME_FILE_TOO_LARGE__" {
        return Err("The Traefik ACME file exceeds the 10 MiB size limit.".to_string());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(out)
        .map_err(|_| "The Traefik ACME file could not be read safely.".to_string())?;
    if bytes.len() > MAX_FILE_SIZE_BYTES {
        return Err("The Traefik ACME file could not be read safely.".to_string());
    }
    Ok(Some(bytes))
}

/// `TraefikAcmeService::decode`.
fn decode(contents: &[u8]) -> Result<Value, String> {
    let invalid = || "The Traefik ACME file contains invalid JSON.".to_string();
    let data: Value = serde_json::from_slice(contents).map_err(|_| invalid())?;
    if !data.is_object() && !data.is_array() {
        return Err(invalid());
    }
    Ok(data)
}

/// `json_encode($certificate, JSON_UNESCAPED_SLASHES)`.
fn encode_unescaped_slashes(value: &Value) -> String {
    php_json::encode(value).replace("\\/", "/")
}

/// `TraefikAcmeService::certificateId`.
fn certificate_id(resolver: &str, certificate: &Value) -> String {
    let mut hash = Sha256::new();
    hash.update(resolver.as_bytes());
    hash.update([0u8]);
    hash.update(encode_unescaped_slashes(&php_assoc(certificate)).as_bytes());
    format!("{:x}", hash.finalize())
}

/// `json_decode($contents, true)`: PHP keeps an empty object as an empty array.
fn php_assoc(value: &Value) -> Value {
    match value {
        Value::Object(map) if map.is_empty() => Value::Array(vec![]),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), php_assoc(v))).collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(php_assoc).collect()),
        other => other.clone(),
    }
}

/// `TraefikAcmeService::certificates`.
pub fn certificates(contents: &[u8]) -> Result<Vec<Value>, String> {
    let data = decode(contents)?;
    let mut out = Vec::new();
    let Some(resolvers) = data.as_object() else {
        return Ok(out);
    };
    for (resolver, resolver_data) in resolvers {
        let Some(certs) = resolver_data.get("Certificates").and_then(Value::as_array) else {
            continue;
        };
        for c in certs {
            if !c.is_object() {
                continue;
            }
            let Some(main) = c
                .pointer("/domain/main")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
            else {
                continue;
            };
            let sans: Vec<&str> = c
                .pointer("/domain/sans")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .filter(|d| !d.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            out.push(json!({
                "id": certificate_id(resolver, c),
                "resolver": resolver,
                "main_domain": main,
                "sans": sans,
                "store": c.get("Store").and_then(Value::as_str),
                "expires_at": c.get("certificate").and_then(Value::as_str).and_then(certificate::acme_expiration),
            }));
        }
    }
    Ok(out)
}

/// `GetTraefikCertificates::handle`.
pub async fn list_certificates(srv: &Srv) -> Result<Vec<Value>, String> {
    if !is_traefik(srv) {
        return Ok(Vec::new());
    }
    let list = match contents(srv).await? {
        Some(bytes) => certificates(&bytes)?,
        None => Vec::new(),
    };
    if list.is_empty() {
        return Ok(list);
    }
    // v4.7.5: `in_use` is false when no route of the proxy and no resource on the server uses a
    // domain of the certificate, and null when Coolify could not check it.
    let known = routing_configuration(srv)
        .await
        .map(|text| hostnames(&text));
    Ok(list
        .into_iter()
        .map(|mut certificate| {
            let in_use = known
                .as_ref()
                .map(|names| json!(is_in_use(&certificate, names)))
                .unwrap_or(Value::Null);
            if let Some(map) = certificate.as_object_mut() {
                map.insert("in_use".into(), in_use);
            }
            certificate
        })
        .collect())
}

/// `GetTraefikCertificates::routingConfiguration()`: text that names the domains the proxy can
/// serve: the Traefik labels of all containers and Swarm services, the proxy and dynamic
/// configuration files, and the domains of the resources on the server. Stopped resources have
/// no containers, so their domains come from the database. `None` when the server did not answer.
async fn routing_configuration(srv: &Srv) -> Option<String> {
    let proxy_path = shell_arg(directory(srv).trim_end_matches('/'));
    let script = [
        "docker ps -aq --filter label=traefik.enable=true | xargs -r docker inspect --format '{{json .Config.Labels}}' 2>/dev/null".to_owned(),
        "docker service ls -q 2>/dev/null | xargs -r docker service inspect --format '{{json .Spec.Labels}}' 2>/dev/null".to_owned(),
        format!("cat {proxy_path}/docker-compose.yml {proxy_path}/dynamic/*.yaml {proxy_path}/dynamic/*.yml 2>/dev/null"),
        "true".to_owned(),
    ]
    .join("; ");
    let output = srv
        .run(
            &[format!("sh -c {}", shell_arg(&script))],
            "proxy.certificates",
        )
        .await
        .ok()?
        .unwrap_or_default();
    let on_server = "((a.destination_type = 'App\\Models\\StandaloneDocker' AND a.destination_id IN \
         (SELECT id FROM standalone_dockers WHERE server_id = $1)) \
         OR (a.destination_type = 'App\\Models\\SwarmDocker' AND a.destination_id IN \
         (SELECT id FROM swarm_dockers WHERE server_id = $1)) \
         OR a.id IN (SELECT application_id FROM additional_destinations WHERE server_id = $1))";
    let domains: Vec<Option<String>> = sqlx::query_scalar(&format!(
        "SELECT d FROM ( \
           SELECT a.fqdn::text AS d FROM applications a WHERE a.deleted_at IS NULL AND {on_server} \
           UNION ALL SELECT a.docker_compose_domains::text FROM applications a WHERE a.deleted_at IS NULL AND {on_server} \
           UNION ALL SELECT p.fqdn::text FROM application_previews p JOIN applications a ON a.id = p.application_id \
             WHERE a.deleted_at IS NULL AND {on_server} \
           UNION ALL SELECT p.docker_compose_domains::text FROM application_previews p JOIN applications a ON a.id = p.application_id \
             WHERE a.deleted_at IS NULL AND {on_server} \
           UNION ALL SELECT sa.fqdn::text FROM service_applications sa JOIN services s ON s.id = sa.service_id \
             WHERE s.server_id = $1 AND s.deleted_at IS NULL \
           UNION ALL SELECT i.fqdn::text FROM instance_settings i WHERE i.id = 0 AND $2 \
         ) x"
    ))
    .bind(srv.id())
    .bind(srv.is_localhost())
    .fetch_all(srv.db().pool())
    .await
    .ok()?;
    let domains: Vec<String> = domains
        .into_iter()
        .flatten()
        .filter(|d| !d.is_empty())
        .collect();
    Some(format!("{output}\n{}", domains.join("\n")))
}

/// `TraefikAcmeService::hostnames()`: every host name in the text, such as the hosts of Traefik
/// rules, labels, and resource URLs.
pub fn hostnames(text: &str) -> std::collections::HashSet<String> {
    static HOST: std::sync::LazyLock<fancy_regex::Regex> = std::sync::LazyLock::new(|| {
        fancy_regex::Regex::new(
            r"(?<![a-z0-9-])(?:\*\.)?(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}(?![a-z0-9-])",
        )
        .unwrap_or_else(|e| unreachable!("{e}"))
    });
    let lower = text.to_lowercase();
    HOST.find_iter(&lower)
        .filter_map(Result::ok)
        .map(|m| m.as_str().to_owned())
        .collect()
}

/// `TraefikAcmeService::isInUse()`: true when a host in use matches the main domain or an
/// alternative name of the certificate.
pub fn is_in_use(certificate: &Value, hostnames: &std::collections::HashSet<String>) -> bool {
    let main = certificate.get("main_domain").and_then(Value::as_str);
    let sans = certificate
        .get("sans")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str);
    for domain in main.into_iter().chain(sans) {
        let domain = domain.to_lowercase();
        if hostnames.contains(&domain) {
            return true;
        }
        // A wildcard certificate covers exactly one label below its domain.
        if let Some(suffix) = domain.strip_prefix('*') {
            let covered = hostnames.iter().any(|hostname| {
                hostname
                    .strip_suffix(suffix)
                    .is_some_and(|label| !label.is_empty() && !label.contains('.'))
            });
            if covered {
                return true;
            }
        }
    }
    false
}

/// `TraefikAcmeService::deleteCertificates()`: removes the certificates with the given
/// identifiers from every resolver.
fn delete_many_from_contents(contents: &[u8], ids: &[String]) -> Result<String, String> {
    let not_found = || "The selected certificate could not be found.".to_string();
    let mut data = php_assoc(&decode(contents)?);
    let mut pending: Vec<&String> = ids.iter().collect();
    if let Some(resolvers) = data.as_object_mut() {
        for (resolver, resolver_data) in resolvers.iter_mut() {
            let Some(certs) = resolver_data
                .get_mut("Certificates")
                .and_then(Value::as_array_mut)
            else {
                continue;
            };
            certs.retain(|certificate| {
                if !certificate.is_object() {
                    return true;
                }
                let id = certificate_id(resolver, certificate);
                match pending.iter().position(|p| **p == id) {
                    Some(position) => {
                        pending.remove(position);
                        false
                    }
                    None => true,
                }
            });
        }
    }
    if ids.is_empty() || !pending.is_empty() {
        return Err(not_found());
    }
    Ok(php_json::encode_pretty(&data) + "\n")
}

/// `DeleteTraefikCertificate::handle($server, $certificateIds)` (v4.7.5): deletes the
/// certificates in one write, so acme.json gets one backup.
pub async fn delete_certificates(srv: &mut Srv, ids: &[String]) -> Result<Vec<Value>, String> {
    if !is_traefik(srv) {
        return Err("TLS certificates can only be managed for Traefik proxies.".to_string());
    }
    let Some(bytes) = contents(srv).await? else {
        return Err("The Traefik ACME file could not be found.".to_string());
    };
    let deleted: Vec<Value> = certificates(&bytes)?
        .into_iter()
        .filter(|c| {
            c.get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| ids.iter().any(|wanted| wanted == id))
        })
        .collect();
    let updated = delete_many_from_contents(&bytes, ids)?;
    save_file(srv, &updated).await?;
    require_restart(srv).await?;
    Ok(deleted)
}

/// `Certificates::deleteUnusedTraefikCertificates()`: deletes the certificates that no route and
/// no resource on the server uses, so Traefik stops renewing them. Empty when there are none.
pub async fn delete_unused_certificates(srv: &mut Srv) -> Result<Vec<Value>, String> {
    let unused: Vec<String> = list_certificates(srv)
        .await?
        .iter()
        .filter(|c| c.get("in_use") == Some(&Value::Bool(false)))
        .filter_map(|c| c.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    if unused.is_empty() {
        return Ok(Vec::new());
    }
    delete_certificates(srv, &unused).await
}

/// `TraefikAcmeService::deleteCertificate`.
fn delete_from_contents(contents: &[u8], resolver: &str, id: &str) -> Result<String, String> {
    let not_found = || "The selected certificate could not be found.".to_string();
    let mut data = php_assoc(&decode(contents)?);
    let certs = data
        .get_mut(resolver)
        .and_then(|r| r.get_mut("Certificates"))
        .and_then(Value::as_array_mut)
        .ok_or_else(not_found)?;
    let position = certs
        .iter()
        .position(|c| c.is_object() && certificate_id(resolver, c) == id)
        .ok_or_else(not_found)?;
    certs.remove(position);
    Ok(php_json::encode_pretty(&data) + "\n")
}

/// `ListTraefikAcmeBackups::backupCommands`: `(script, temporary path)`.
fn backup_commands(srv: &Srv) -> (String, String) {
    let dir = directory(srv);
    let suffix = random_hex(4);
    let acme = shell_arg(&format!("{dir}/acme.json"));
    let temporary_path = format!("{dir}/.acme.json.backup-{suffix}.tmp");
    let backup = shell_arg(&format!(
        "{dir}/acme.json.backup-{}-{suffix}",
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
    ));
    let temporary = shell_arg(&temporary_path);
    (
        format!(
            "if [ -f {acme} ]; then cp -- {acme} {temporary}; chmod 600 {temporary}; mv -- {temporary} {backup}; fi"
        ),
        temporary_path,
    )
}

/// `ListTraefikAcmeBackups::pruneCommands`.
fn prune_commands(srv: &Srv) -> String {
    let dir = shell_arg(&directory(srv));
    format!(
        "ls -1 -- {dir} | grep -E {} | sort -r | tail -n +{} | while IFS= read -r name; do rm -f -- {dir}/\"$name\"; done",
        shell_arg(SHELL_NAME_PATTERN),
        KEEP_BACKUPS + 1
    )
}

/// `SaveTraefikAcmeFile::handle`. Upstream uploads with scp; here the content goes through the script in pieces.
async fn save_file(srv: &Srv, contents: &str) -> Result<(), String> {
    let path = format!("{}/acme.json", directory(srv));
    let suffix = random_hex(8);
    let temporary_path = format!("{path}.coolify-{suffix}");
    let upload_path = format!("/tmp/coolify-acme-{suffix}.json");
    let encoded = base64::engine::general_purpose::STANDARD.encode(contents);
    let mut upload = vec![format!("umask 077; : > {}", shell_arg(&upload_path))];
    for piece in encoded.as_bytes().chunks(65_536) {
        upload.push(format!(
            "echo '{}' | base64 -d >> {}",
            String::from_utf8_lossy(piece),
            shell_arg(&upload_path)
        ));
    }
    srv.run(&upload, "proxy.acme").await?;
    let (backup, backup_temporary) = backup_commands(srv);
    let trap = format!(
        "rm -f -- {} {} {}",
        shell_arg(&upload_path),
        shell_arg(&temporary_path),
        shell_arg(&backup_temporary)
    );
    let script = [
        "set -e".to_string(),
        format!("trap {} EXIT", shell_arg(&trap)),
        "umask 077".to_string(),
        format!(
            "cat -- {} > {}",
            shell_arg(&upload_path),
            shell_arg(&temporary_path)
        ),
        format!("chmod 600 {}", shell_arg(&temporary_path)),
        backup,
        format!("mv -- {} {}", shell_arg(&temporary_path), shell_arg(&path)),
        prune_commands(srv),
    ]
    .join("; ");
    srv.run(&[format!("sh -c {}", shell_arg(&script))], "proxy.acme")
        .await
        .map(|_| ())
}

/// `$server->proxy->certificates_restart_required = true; $server->save()`.
async fn require_restart(srv: &mut Srv) -> Result<(), String> {
    srv.proxy_set("certificates_restart_required", json!(true));
    srv.save_proxy().await
}

/// `DeleteTraefikCertificate::handle`.
pub async fn delete_certificate(srv: &mut Srv, id: &str) -> Result<Value, String> {
    if !is_traefik(srv) {
        return Err("TLS certificates can only be managed for Traefik proxies.".to_string());
    }
    let Some(bytes) = contents(srv).await? else {
        return Err("The Traefik ACME file could not be found.".to_string());
    };
    let certificate = certificates(&bytes)?
        .into_iter()
        .find(|c| c.get("id").and_then(Value::as_str) == Some(id))
        .ok_or_else(|| "The selected certificate could not be found.".to_string())?;
    let resolver = certificate
        .get("resolver")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let updated = delete_from_contents(&bytes, resolver, id)?;
    save_file(srv, &updated).await?;
    require_restart(srv).await?;
    Ok(certificate)
}

/// `ListTraefikAcmeBackups::NAME_PATTERN`: the timestamp of a valid name.
fn backup_timestamp(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("acme.json.backup-")?;
    let (stamp, suffix) = rest.split_once('-')?;
    let b = stamp.as_bytes();
    let valid = b.len() == 16
        && b[8] == b'T'
        && b[15] == b'Z'
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[9..15].iter().all(u8::is_ascii_digit)
        && suffix.len() == 8
        && suffix
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c));
    valid.then_some(stamp)
}

/// `ListTraefikAcmeBackups::handle`.
pub async fn list_backups(srv: &Srv) -> Result<Vec<Value>, String> {
    if !is_traefik(srv) {
        return Ok(Vec::new());
    }
    let pattern = format!("{}/acme.json.backup-*", shell_arg(&directory(srv)));
    let script = format!(
        "for file in {pattern}; do if [ -f \"$file\" ]; then printf '%s %s\\n' \"${{file##*/}}\" \"$(wc -c < \"$file\")\"; fi; done"
    );
    let output = srv
        .run(&[format!("sh -c {}", shell_arg(&script))], "proxy.acme")
        .await?
        .unwrap_or_default();
    let mut list: Vec<(String, Value)> = output
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let name = parts.next()?;
            let size: i64 = parts.next()?.parse().ok()?;
            let stamp = backup_timestamp(name)?;
            let created = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%SZ").ok()?;
            Some((
                name.to_string(),
                json!({"name": name, "created_at": created.format("%Y-%m-%d %H:%M:%S").to_string(), "size": size}),
            ))
        })
        .collect();
    list.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(list.into_iter().map(|(_, v)| v).collect())
}

/// `ListTraefikAcmeBackups::resolve`.
async fn resolve_backup(srv: &Srv, name: &str) -> Result<(), String> {
    if !is_traefik(srv) {
        return Err("TLS certificates can only be managed for Traefik proxies.".to_string());
    }
    let found = backup_timestamp(name).is_some()
        && list_backups(srv)
            .await?
            .iter()
            .any(|b| b.get("name").and_then(Value::as_str) == Some(name));
    if found {
        Ok(())
    } else {
        Err("The selected acme.json backup could not be found.".to_string())
    }
}

/// `RestoreTraefikAcmeBackup::handle`.
pub async fn restore_backup(srv: &mut Srv, name: &str) -> Result<(), String> {
    resolve_backup(srv, name).await?;
    let dir = directory(srv);
    let (backup, backup_temporary) = backup_commands(srv);
    let restore_path = format!("{dir}/.acme.json.restore-{}.tmp", random_hex(8));
    let source = shell_arg(&format!("{dir}/{name}"));
    let temporary = shell_arg(&restore_path);
    let trap = format!(
        "rm -f -- {} {}",
        shell_arg(&restore_path),
        shell_arg(&backup_temporary)
    );
    let script = [
        "set -e".to_string(),
        format!("trap {} EXIT", shell_arg(&trap)),
        "umask 077".to_string(),
        format!("[ -f {source} ]"),
        format!("cp -- {source} {temporary}"),
        format!("chmod 600 {temporary}"),
        backup,
        format!(
            "mv -- {temporary} {}",
            shell_arg(&format!("{dir}/acme.json"))
        ),
        prune_commands(srv),
    ]
    .join("; ");
    srv.run(&[format!("sh -c {}", shell_arg(&script))], "proxy.acme")
        .await?;
    require_restart(srv).await
}

/// `DeleteTraefikAcmeBackup::handle`.
pub async fn delete_backup(srv: &Srv, name: &str) -> Result<(), String> {
    resolve_backup(srv, name).await?;
    let path = shell_arg(&format!("{}/{name}", directory(srv)));
    srv.run(
        &[format!("sh -c {}", shell_arg(&format!("rm -f -- {path}")))],
        "proxy.acme",
    )
    .await
    .map(|_| ())
}

#[cfg(test)]
mod v475_tests {
    use super::*;

    // `TraefikAcmeService::hostnames()` and `isInUse()` (upstream tests/Unit/TraefikAcmeServiceTest).
    #[test]
    fn finds_hosts_of_rules_labels_and_urls() {
        let names = hostnames(
            "traefik.http.routers.a.rule=Host(`App.Example.com`) && PathPrefix(`/`)\nhttps://api.example.org:8443/path,http://x.internal\nHostRegexp(`*.wild.example.net`)\nnot-a-host localhost 10.0.0.1",
        );
        for expected in [
            "app.example.com",
            "api.example.org",
            "x.internal",
            "*.wild.example.net",
        ] {
            assert!(names.contains(expected), "{expected}: {names:?}");
        }
        assert!(!names.contains("localhost"));
    }

    #[test]
    fn a_certificate_is_in_use_when_a_host_matches_one_of_its_names() {
        let names = hostnames("Host(`app.example.com`) Host(`deep.sub.example.org`)");
        let certificate = |main: &str, sans: &[&str]| json!({"main_domain": main, "sans": sans});
        assert!(is_in_use(&certificate("app.example.com", &[]), &names));
        assert!(is_in_use(
            &certificate("other.example.com", &["APP.example.com"]),
            &names
        ));
        assert!(!is_in_use(
            &certificate("gone.example.com", &["old.example.com"]),
            &names
        ));
        // A wildcard certificate covers exactly one label below its domain.
        assert!(is_in_use(&certificate("*.example.com", &[]), &names));
        assert!(!is_in_use(&certificate("*.example.org", &[]), &names));
        assert!(is_in_use(&certificate("*.sub.example.org", &[]), &names));
    }

    // `TraefikAcmeService::deleteCertificates()`.
    #[test]
    fn deletes_several_certificates_in_one_write() {
        let acme = br#"{"letsencrypt":{"Account":{},"Certificates":[{"domain":{"main":"a.example.com"},"certificate":"QQ==","key":"Sw=="},{"domain":{"main":"b.example.com"},"certificate":"Qg==","key":"Sw=="}]},"other":{"Certificates":[{"domain":{"main":"c.example.com"},"certificate":"Qw==","key":"Sw=="}]}}"#;
        let list = certificates(acme).unwrap_or_default();
        let id = |domain: &str| {
            list.iter()
                .find(|c| c["main_domain"] == domain)
                .and_then(|c| c["id"].as_str())
                .unwrap_or_default()
                .to_owned()
        };
        let updated = delete_many_from_contents(acme, &[id("a.example.com"), id("c.example.com")])
            .unwrap_or_default();
        let left = certificates(updated.as_bytes()).unwrap_or_default();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0]["main_domain"], "b.example.com");
        assert!(updated.ends_with('\n'));
        // An unknown identifier, or none: nothing is written.
        let not_found = Err("The selected certificate could not be found.".to_string());
        assert_eq!(
            delete_many_from_contents(acme, &["nope".to_owned()]),
            not_found
        );
        assert_eq!(
            delete_many_from_contents(acme, &[id("a.example.com"), "nope".to_owned()]),
            not_found
        );
        assert_eq!(delete_many_from_contents(acme, &[]), not_found);
    }
}
