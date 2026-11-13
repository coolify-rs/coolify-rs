//! The SSH hosts of an application's repository, for "SSH host keys" on the Source page:
//! `Application::gitSshHost()`, `gitSshHostsToScan()` and `fetchGitSshHostKeys()`. The known_hosts
//! helpers are in [`crate::applications::git_ssh`]; the build servers come from
//! `Jobs::build_server_candidates()` (`Application::buildServerCandidates()`).

use std::sync::LazyLock;

use coolify_db::applications::ApplicationRow;
use coolify_db::servers;
use coolify_remote::RunOptions;
use coolify_remote::shell::quote;
use regex::Regex;
use serde_json::Value;

use super::git::{self, DeploymentType, Source};
use crate::Jobs;
use crate::applications::git_ssh;
use crate::server::ServerInfo;

/// A host and port that git connects to over SSH.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSshHost {
    pub host: String,
    pub port: i64,
}

/// The host names `gitSshHost()` and `gitSshHostsToScan()` accept.
static HOST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[A-Za-z0-9._:][A-Za-z0-9._:-]*\z").unwrap_or_else(|_| unreachable!())
});

static BRACKETED_HOST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A\[(.+)\]:([0-9]+)\z").unwrap_or_else(|_| unreachable!()));

/// `Application::gitSshHost()` over `customRepository()`'s repository and port: `None` when the
/// repository does not use SSH. `source_port` is the custom port of the Git source when the
/// application deploys through it.
fn ssh_host_of(repository: &str, port: &str, source_port: Option<i64>) -> Option<GitSshHost> {
    let host = if let Some((scheme, _)) = repository.split_once("://") {
        if scheme != "ssh" {
            return None;
        }
        git::url_host(repository)
    } else {
        git::parse_scp(repository).map(|(_, host, _, _)| host)
    };
    let host = host.unwrap_or_default();
    let host = host.trim_matches(|c| c == '[' || c == ']');
    let port = match source_port {
        Some(port) => port,
        None => port.trim().parse::<i64>().ok()?,
    };
    HOST.is_match(host).then(|| GitSshHost {
        host: host.to_owned(),
        port,
    })
}

/// `Application::gitSshHostsToScan()`: the repository host and the hosts of the saved keys (for
/// example submodule hosts), without the hosts of `gitHostsWithPublishedKeys()`.
fn hosts_to_scan(
    repository_host: Option<GitSshHost>,
    known_hosts: Option<&str>,
) -> Vec<GitSshHost> {
    let mut hosts: Vec<GitSshHost> = repository_host.into_iter().collect();
    for line in git_ssh::git_known_hosts_lines(known_hosts) {
        let entry = line
            .split(' ')
            .next()
            .unwrap_or_default()
            .split(',')
            .next()
            .unwrap_or_default();
        hosts.push(match BRACKETED_HOST.captures(entry) {
            Some(c) => GitSshHost {
                host: c[1].to_owned(),
                port: c[2].parse().unwrap_or(22),
            },
            None => GitSshHost {
                host: entry.to_owned(),
                port: 22,
            },
        });
    }
    let published = git_ssh::git_hosts_with_published_keys();
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for host in hosts {
        let lower = host.host.to_lowercase();
        if !HOST.is_match(&host.host) || published.contains(&lower) {
            continue;
        }
        let key = format!("{lower}:{}", host.port);
        if !seen.contains(&key) {
            seen.push(key);
            out.push(host);
        }
    }
    out
}

/// One `ssh-keyscan` run in the helper image, as `fetchGitSshHostKeys()` builds it. `network` is
/// empty or `--network 'name' `.
fn scan_command(network: &str, image: &str, host: &GitSshHost) -> String {
    format!(
        "docker run --rm {network}--entrypoint ssh-keyscan {image} -T 10 -p {} -- {} 2>/dev/null || true",
        host.port,
        quote(&host.host)
    )
}

impl Jobs {
    /// `$this->source` of an application.
    async fn git_source_of(&self, a: &ApplicationRow) -> Option<Source> {
        let (kind, id) = (a.str("source_type")?, a.int("source_id")?);
        let table = match kind {
            git::GITHUB_APP => "github_apps",
            git::GITLAB_APP => "gitlab_apps",
            git::GITEA_APP => "gitea_apps",
            _ => return None,
        };
        let row = coolify_db::git_sources::source_row(&self.0.db, table, id)
            .await
            .ok()
            .flatten()?;
        Some(Source {
            row,
            morph: kind.to_owned(),
        })
    }

    /// `Application::gitSshHost()`: the host and port that git connects to over SSH, or `None`
    /// when the repository does not use SSH.
    pub async fn git_ssh_host(&self, a: &ApplicationRow) -> Option<GitSshHost> {
        let source = self.git_source_of(a).await;
        let kind = git::deployment_type(a, source.as_ref());
        let (repository, port) = git::convert_git_url(
            a.str("git_repository").unwrap_or_default(),
            kind,
            source.as_ref(),
        );
        let source_port = if kind == DeploymentType::Source {
            source
                .as_ref()
                .and_then(|s| s.row.get("custom_port"))
                .and_then(Value::as_i64)
        } else {
            None
        };
        ssh_host_of(&repository, &port, source_port)
    }

    /// `Application::gitSshHostsToScan()`: the SSH hosts whose keys "Fetch current host key"
    /// reads.
    pub async fn git_ssh_hosts_to_scan(&self, a: &ApplicationRow) -> Vec<GitSshHost> {
        hosts_to_scan(self.git_ssh_host(a).await, a.str("git_ssh_known_hosts"))
    }

    /// `Application::fetchGitSshHostKeys()`: reads the SSH host keys that the hosts of
    /// `gitSshHostsToScan()` send now, from where the deployment helper container runs: a build
    /// server when the application builds on one, else the server of the application on the
    /// Docker network of the destination. The first server that reads keys is used. Answers with
    /// known_hosts lines.
    pub async fn fetch_git_ssh_host_keys(&self, a: &ApplicationRow) -> Vec<String> {
        let hosts = self.git_ssh_hosts_to_scan(a).await;
        if hosts.is_empty() {
            return Vec::new();
        }
        let db = &self.0.db;
        // `coolifyHelperImage().':'.getHelperVersion()`, as the deployment job computes it.
        let settings = coolify_db::instance::deployment_settings(db)
            .await
            .unwrap_or_default();
        let registry = settings
            .docker_registry_url
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| "docker.io".into());
        let version = match settings.helper_version.filter(|v| !v.is_empty()) {
            Some(v) if super::version_gt(&v, super::HELPER_VERSION) => v,
            _ => super::HELPER_VERSION.to_owned(),
        };
        let image = quote(&format!("{registry}/coollabsio/coolify-helper:{version}"));
        let Some(server_id) = a.server_id() else {
            return Vec::new();
        };
        let Ok(Some(row)) = servers::find_by_id(db, server_id).await else {
            return Vec::new();
        };
        let server = ServerInfo::from_row(&row);
        let mut targets: Vec<(i64, String)> = self
            .build_server_candidates(a, &server)
            .await
            .into_iter()
            .map(|id| (id, String::new()))
            .collect();
        targets.push((server.id, format!("--network {} ", quote(a.network()))));
        for (id, network) in targets {
            let info = if id == server.id {
                server.clone()
            } else {
                match servers::find_by_id(db, id).await {
                    Ok(Some(row)) => ServerInfo::from_row(&row),
                    _ => continue,
                }
            };
            let Ok(target) = info.target(self).await else {
                continue;
            };
            let commands: Vec<String> = hosts
                .iter()
                .map(|host| scan_command(&network, &image, host))
                .collect();
            let commands: Vec<&str> = commands.iter().map(String::as_str).collect();
            let output = self
                .remote()
                .run_quiet(
                    &target,
                    &commands,
                    RunOptions::op("application.git_host_keys.scan"),
                )
                .await
                .unwrap_or_default();
            let lines = git_ssh::git_known_hosts_lines(Some(&output));
            if !lines.is_empty() {
                return lines;
            }
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(host: &str, port: i64) -> GitSshHost {
        GitSshHost {
            host: host.to_owned(),
            port,
        }
    }

    #[test]
    fn ssh_hosts_of_repositories() {
        // scp-style and ssh:// URLs use SSH.
        assert_eq!(
            ssh_host_of("git@git.example.com:acme/web.git", "22", None),
            Some(host("git.example.com", 22))
        );
        assert_eq!(
            ssh_host_of("ssh://git@git.example.com:2222/acme/web.git", "2222", None),
            Some(host("git.example.com", 2222))
        );
        // The brackets of an IPv6 host are removed.
        assert_eq!(
            ssh_host_of("ssh://git@[2001:db8::1]:2222/acme/web.git", "2222", None),
            Some(host("2001:db8::1", 2222))
        );
        // HTTP(S) and git:// do not use SSH, nor does owner/name of a Git source.
        assert_eq!(
            ssh_host_of("https://git.example.com/acme/web", "22", None),
            None
        );
        assert_eq!(
            ssh_host_of("git://git.example.com/acme/web", "22", None),
            None
        );
        assert_eq!(ssh_host_of("acme/web", "22", Some(22)), None);
        // A source with a custom port wins over the port of the URL.
        assert_eq!(
            ssh_host_of("git@git.example.com:acme/web.git", "22", Some(2022)),
            Some(host("git.example.com", 2022))
        );
        // A port that is no number.
        assert_eq!(
            ssh_host_of("git@git.example.com:acme/web.git", "x", None),
            None
        );
    }

    #[test]
    fn scan_commands_run_ssh_keyscan_in_the_helper_image() {
        assert_eq!(
            scan_command("", "'helper:1'", &host("git.example.com", 22)),
            "docker run --rm --entrypoint ssh-keyscan 'helper:1' -T 10 -p 22 -- 'git.example.com' 2>/dev/null || true"
        );
        assert_eq!(
            scan_command(
                "--network 'coolify' ",
                "'helper:1'",
                &host("2001:db8::1", 2222)
            ),
            "docker run --rm --network 'coolify' --entrypoint ssh-keyscan 'helper:1' -T 10 -p 2222 -- '2001:db8::1' 2>/dev/null || true"
        );
    }

    #[test]
    fn hosts_to_scan_are_the_repository_host_and_the_saved_hosts() {
        let key =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";
        let saved = format!(
            "git.example.com {key}\n[sub.example.com]:2222,alias.example.com {key}\ngithub.com {key}\nnot a line"
        );
        assert_eq!(
            hosts_to_scan(Some(host("Git.Example.com", 22)), Some(&saved)),
            vec![host("Git.Example.com", 22), host("sub.example.com", 2222)]
        );
        // The same host on another port is scanned too.
        assert_eq!(
            hosts_to_scan(Some(host("git.example.com", 2022)), Some(&saved)),
            vec![
                host("git.example.com", 2022),
                host("git.example.com", 22),
                host("sub.example.com", 2222)
            ]
        );
        // Hosts whose keys are published are never scanned.
        assert_eq!(hosts_to_scan(Some(host("github.com", 22)), None), vec![]);
        assert_eq!(hosts_to_scan(None, None), vec![]);
    }
}
