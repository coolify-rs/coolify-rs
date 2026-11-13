//! SSH host key checking of git over SSH (bootstrap/helpers/applications.php, Coolify v4.7):
//! `gitSshSetupCommand()`, `gitSshCommand()`, the pinned keys of the large git hosts and the
//! `known_hosts` helpers of an application (`applications.git_ssh_known_hosts`).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD as B64, STANDARD_NO_PAD as B64_NO_PAD};
use regex::Regex;

/// `ApplicationDeploymentJob::GIT_HOST_KEY_CHANGED_HINT`: logged when a git command of Coolify
/// fails because of the SSH host key. The deployment page looks for this text.
pub const GIT_HOST_KEY_CHANGED_HINT: &str = "The Git host sent an SSH host key that is not the saved or published key. This can be an attack. Ask the administrator of the Git host if the key changed. Then open Source > SSH host keys, click Fetch current host key, and accept the key only if its fingerprint is the same as the fingerprint from the administrator. For GitHub and Bitbucket Cloud, Fetch current host key also reads the keys that they publish. For GitLab.com, compare the fingerprint with the keys on docs.gitlab.com.";

/// `GIT_PINNED_HOST_KEYS`: the published SSH host keys of the large git hosts, also for their SSH
/// endpoints on port 443. Git over SSH to these hosts accepts only these keys. Update this list
/// when a provider changes its host keys.
pub const GIT_PINNED_HOST_KEYS: [&str; 9] = [
    "github.com,ssh.github.com,[ssh.github.com]:443 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl",
    "github.com,ssh.github.com,[ssh.github.com]:443 ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBEmKSENjQEezOmxkZMy7opKgwFB9nkt5YRrYMjNuG5N87uRgg6CLrbo5wAdT/y6v0mKV0U2w0WZ2YB/++Tpockg=",
    "github.com,ssh.github.com,[ssh.github.com]:443 ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQCj7ndNxQowgcQnjshcLrqPEiiphnt+VTTvDP6mHBL9j1aNUkY4Ue1gvwnGLVlOhGeYrnZaMgRK6+PKCUXaDbC7qtbW8gIkhL7aGCsOr/C56SJMy/BCZfxd1nWzAOxSDPgVsmerOBYfNqltV9/hWCqBywINIR+5dIg6JTJ72pcEpEjcYgXkE2YEFXV1JHnsKgbLWNlhScqb2UmyRkQyytRLtL+38TGxkxCflmO+5Z8CSSNY7GidjMIZ7Q4zMjA2n1nGrlTDkzwDCsw+wqFPGQA179cnfGWOWRVruj16z6XyvxvjJwbz0wQZ75XK5tKSb7FNyeIEs4TT4jk+S4dhPeAUC5y+bDYirYgM4GC7uEnztnZyaVWQ7B381AK4Qdrwt51ZqExKbQpTUNn+EjqoTwvqNj4kqx5QUCI0ThS/YkOxJCXmPUWZbhjpCg56i+2aB6CmK2JGhn57K5mj0MNdBXA4/WnwH6XoPWJzK5Nyu2zB3nAZp+S5hpQs+p1vN1/wsjk=",
    "gitlab.com,[altssh.gitlab.com]:443 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAfuCHKVTjquxvt6CM6tdG4SLp1Btn/nOeHHE5UOzRdf",
    "gitlab.com,[altssh.gitlab.com]:443 ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBFSMqzJeV9rUzU4kWitGjeR4PWSa29SPqJ1fVkhtj3Hw9xjLVXVYrU9QlYWrOLXBpQ6KWjbjTDTdDkoohFzgbEY=",
    "gitlab.com,[altssh.gitlab.com]:443 ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQCsj2bNKTBSpIYDEGk9KxsGh3mySTRgMtXL583qmBpzeQ+jqCMRgBqB98u3z++J1sKlXHWfM9dyhSevkMwSbhoR8XIq/U0tCNyokEi/ueaBMCvbcTHhO7FcwzY92WK4Yt0aGROY5qX2UKSeOvuP4D6TPqKF1onrSzH9bx9XUf2lEdWT/ia1NEKjunUqu1xOB/StKDHMoX4/OKyIzuS0q/T1zOATthvasJFoPrAjkohTyaDUz2LN5JoH839hViyEG82yB+MjcFV5MU3N1l1QL3cVUCh93xSaua1N85qivl+siMkPGbO5xR/En4iEY6K2XPASUEMaieWVNTRCtJ4S8H+9",
    "bitbucket.org,[altssh.bitbucket.org]:443 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIazEu89wgQZ4bqs3d63QSMzYVa0MuJ2e2gKTKqu+UUO",
    "bitbucket.org,[altssh.bitbucket.org]:443 ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBPIQmuzMBuKdWeF4+a2sjSSpBK0iqitSQ+5BM9KhpexuGt20JpTVM7u5BDZngncgrqDMbWdxMWWOGtZ9UgbqgZE=",
    "bitbucket.org,[altssh.bitbucket.org]:443 ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQDQeJzhupRu0u0cdegZIa8e86EG2qOCsIsD1Xw0xSeiPDlCr7kq97NLmMbpKTX6Esc30NuoqEEHCuc7yWtwp8dI76EEEB1VqY9QJq6vk+aySyboD5QF61I/1WeTwu+deCbgKMGbUijeXhtfbxSxm6JwGrXrhBdofTsbKRUsrN1WoNgUa8uqN1Vx6WAJw1JHPhglEGGHea6QICwJOAr/6mrui/oB7pkaWKHj3z7d1IC4KWLtY47elvjbaTlkN04Kc/5LFEirorGYVbt15kAUlqGM65pk6ZBxtaO3+30LVlORZkxOh+LKL/BvbZ/iRNhItLqNyieoQj/uh/7Iv4uyH/cV/0b4WDSd3DptigWq84lJubb9t/DnZlrJazxyDCulTmKdOR7vs9gMTo+uoIrPSb8ScTtvw65+odKAlBj59dhnVp9zd7QUojOpXlL62Aw56U4oO+FALuevvMjiWeavKhJqlR7i5n9srYcrNV7ttmDw7kf/97P5zauIhxcjX+xHv4M=",
];

/// `GIT_PROVIDER_HOST_KEY_SOURCES`: the providers that publish their SSH host keys over HTTPS.
/// "Fetch current host key" on the Source page reads them (`fetch_git_provider_host_keys()`), so
/// deployments follow a key change without a release. GitLab publishes its keys only in its docs.
/// The first value is the host list of the provider's lines in `GIT_PINNED_HOST_KEYS`.
pub const GIT_PROVIDER_HOST_KEY_SOURCES: [(&str, &str); 2] = [
    (
        "github.com,ssh.github.com,[ssh.github.com]:443",
        "https://api.github.com/meta",
    ),
    (
        "bitbucket.org,[altssh.bitbucket.org]:443",
        "https://bitbucket.org/site/ssh",
    ),
];

/// `Cache::forever(gitProviderHostKeysCacheKey($hosts), $lines)`. In-process: after a restart the
/// pinned keys of the release apply until the keys are fetched again.
static PUBLISHED_HOST_KEYS: LazyLock<Mutex<HashMap<String, Vec<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `gitProviderHostKeysCacheKey($hosts)`.
pub fn git_provider_host_keys_cache_key(hosts: &str) -> String {
    format!("git-provider-host-keys:{hosts}")
}

/// `gitProviderHostKeySource($host)`: the host list of `GIT_PROVIDER_HOST_KEY_SOURCES` that
/// contains `host`, or `None`.
pub fn git_provider_host_key_source(host: &str) -> Option<&'static str> {
    let host = host.to_lowercase();
    GIT_PROVIDER_HOST_KEY_SOURCES
        .iter()
        .map(|(hosts, _)| *hosts)
        .find(|hosts| git_known_hosts_line_hosts(hosts).contains(&host))
}

/// The lines that `fetchGitProviderHostKeys()` builds from the provider's answer: `ssh_keys` of
/// the GitHub meta document, or the lines of the first host in Bitbucket's `known_hosts` text.
/// `Err` is upstream's message when the answer has no valid host keys.
pub fn published_host_key_lines(hosts: &str, url: &str, body: &str) -> Result<Vec<String>, String> {
    let keys: Vec<String> = if hosts == GIT_PROVIDER_HOST_KEY_SOURCES[0].0 {
        serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v.get("ssh_keys").cloned())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|k| k.as_str().map(str::to_owned))
            .collect()
    } else {
        let first_host = hosts.split(',').next().unwrap_or_default();
        split_lines(body)
            .into_iter()
            .map(|line| line.split_whitespace().collect::<Vec<_>>())
            .filter(|parts| parts.len() >= 3 && parts[0] == first_host)
            .map(|parts| format!("{} {}", parts[1], parts[2]))
            .collect()
    };
    let mut lines: Vec<String> = Vec::new();
    for key in keys {
        let line = format!("{hosts} {}", key.trim());
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    if lines.is_empty() || lines.iter().any(|l| !is_valid_git_known_hosts_line(l)) {
        return Err(format!("The answer from {url} has no valid host keys."));
    }
    Ok(lines)
}

/// `fetchGitProviderHostKeys($hosts)`: reads the host keys that a provider publishes over HTTPS
/// and saves them for `git_provider_host_keys()`. `Err` when the provider does not answer with
/// valid keys; the saved keys do not change then.
pub async fn fetch_git_provider_host_keys(hosts: &str) -> Result<Vec<String>, String> {
    let Some((_, url)) = GIT_PROVIDER_HOST_KEY_SOURCES
        .iter()
        .find(|(h, _)| *h == hosts)
    else {
        return Err(format!("No published host keys for {hosts}."));
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("coolify-rs")
        .build()
        .map_err(|e| e.to_string())?;
    let response = client.get(*url).send().await.map_err(|e| e.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "Could not read the published host keys from {url} (HTTP {}).",
            status.as_u16()
        ));
    }
    let body = response.text().await.map_err(|e| e.to_string())?;
    let lines = published_host_key_lines(hosts, url, &body)?;
    if let Ok(mut cache) = PUBLISHED_HOST_KEYS.lock() {
        cache.insert(git_provider_host_keys_cache_key(hosts), lines.clone());
    }
    Ok(lines)
}

/// `gitProviderHostKeys()` over an explicit set of published keys: `GIT_PINNED_HOST_KEYS`, with
/// the lines of a provider replaced by the keys that it published last. A replaced list also
/// drops a key that the provider withdrew.
pub fn provider_host_keys_with(published: &HashMap<String, Vec<String>>) -> Vec<String> {
    let mut lines: Vec<String> = GIT_PINNED_HOST_KEYS
        .iter()
        .map(|l| (*l).to_owned())
        .collect();
    for (hosts, _) in GIT_PROVIDER_HOST_KEY_SOURCES {
        if let Some(keys) = published
            .get(&git_provider_host_keys_cache_key(hosts))
            .filter(|k| !k.is_empty())
        {
            lines.retain(|line| line.split_once(' ').map_or(line.as_str(), |(h, _)| h) != hosts);
            lines.extend(keys.iter().cloned());
        }
    }
    lines
}

/// `gitProviderHostKeys()`.
pub fn git_provider_host_keys() -> Vec<String> {
    let published = PUBLISHED_HOST_KEYS
        .lock()
        .map(|c| c.clone())
        .unwrap_or_default();
    provider_host_keys_with(&published)
}

/// `gitSshConfigFile($uuid)`.
pub fn git_ssh_config_file(uuid: &str) -> String {
    format!("/root/.ssh/coolify_git_{uuid}.config")
}

/// `gitKnownHostsFile($uuid)`.
pub fn git_known_hosts_file(uuid: &str) -> String {
    format!("/root/.ssh/coolify_git_{uuid}.known_hosts")
}

/// `isValidGitKnownHostsLine($line)`: a known_hosts line without markers, hashed host names, or
/// comments: `host[,host] key-type key`. A host with a custom SSH port is written as `[host]:port`.
pub fn is_valid_git_known_hosts_line(line: &str) -> bool {
    static LINE: LazyLock<Regex> = LazyLock::new(|| {
        let host = r"(?:[A-Za-z0-9._:-]+|\[[A-Za-z0-9._:-]+\]:[0-9]{1,5})";
        let key_type = r"(?:ssh-ed25519|ssh-rsa|ecdsa-sha2-nistp(?:256|384|521)|sk-ssh-ed25519@openssh\.com|sk-ecdsa-sha2-nistp256@openssh\.com)";
        Regex::new(&format!(
            r"\A{host}(?:,{host})* {key_type} [A-Za-z0-9+/]+={{0,2}}\z"
        ))
        .unwrap_or_else(|_| unreachable!())
    });
    LINE.is_match(line)
}

static BRACKETED_HOST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A\[(.+)\]:([0-9]+)\z").unwrap_or_else(|_| unreachable!()));

/// `gitKnownHostsLineHosts($line)`: the host names of a known_hosts line, without brackets and ports.
pub fn git_known_hosts_line_hosts(line: &str) -> Vec<String> {
    line.split(' ')
        .find(|part| !part.is_empty())
        .unwrap_or_default()
        .split(',')
        .map(|host| {
            BRACKETED_HOST
                .captures(host)
                .map_or(host, |c| c.get(1).map_or(host, |m| m.as_str()))
                .to_lowercase()
        })
        .collect()
}

fn unique(hosts: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for host in hosts {
        if !out.contains(&host) {
            out.push(host);
        }
    }
    out
}

/// `gitPinnedHosts()`: the host names in `GIT_PINNED_HOST_KEYS`.
pub fn git_pinned_hosts() -> Vec<String> {
    unique(
        GIT_PINNED_HOST_KEYS
            .iter()
            .flat_map(|line| git_known_hosts_line_hosts(line)),
    )
}

/// `gitHostsWithPublishedKeys()`: the host names of the providers in
/// `GIT_PROVIDER_HOST_KEY_SOURCES`. Their keys are read over HTTPS, so the keys that are saved
/// for an application are not used for these hosts.
pub fn git_hosts_with_published_keys() -> Vec<String> {
    unique(
        GIT_PROVIDER_HOST_KEY_SOURCES
            .iter()
            .flat_map(|(hosts, _)| git_known_hosts_line_hosts(hosts)),
    )
}

/// `gitKnownHostsLineHasHostWithPublishedKeys($line)`. Deployments ignore such lines.
pub fn git_known_hosts_line_has_host_with_published_keys(line: &str) -> bool {
    let published = git_hosts_with_published_keys();
    git_known_hosts_line_hosts(line)
        .iter()
        .any(|host| published.contains(host))
}

/// `preg_split('/\R/', $text)`.
fn split_lines(text: &str) -> Vec<&str> {
    static NEWLINE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\r\n|[\n\x0B\x0C\r\u{85}\u{2028}\u{2029}]").unwrap_or_else(|_| unreachable!())
    });
    NEWLINE.split(text).collect()
}

/// `gitKnownHostsLines($text)`: the valid known_hosts lines of a text, without blank lines,
/// duplicates, and lines of `git_hosts_with_published_keys()`.
pub fn git_known_hosts_lines(text: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in split_lines(text.unwrap_or_default()) {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if is_valid_git_known_hosts_line(&line)
            && !git_known_hosts_line_has_host_with_published_keys(&line)
            && !out.contains(&line)
        {
            out.push(line);
        }
    }
    out
}

/// The `host:port` entries of a known_hosts line (`$entries` of `gitKnownHostsReplaceHostKeys()`).
fn host_port_entries(line: &str) -> Vec<String> {
    line.split(' ')
        .find(|part| !part.is_empty())
        .unwrap_or_default()
        .split(',')
        .map(|host| {
            let entry = match BRACKETED_HOST.captures(host) {
                Some(c) => format!(
                    "{}:{}",
                    c.get(1).map_or("", |m| m.as_str()),
                    c.get(2).map_or("", |m| m.as_str())
                ),
                None => format!("{host}:22"),
            };
            entry.to_lowercase()
        })
        .collect()
}

/// `gitKnownHostsReplaceHostKeys($saved, $fetchedLines)`: the saved known_hosts lines, with the
/// lines of the hosts in `fetched` replaced by `fetched`. A saved line belongs to a fetched host
/// when one of its host entries has the same host and port.
pub fn git_known_hosts_replace_host_keys(saved: Option<&str>, fetched: &[String]) -> Vec<String> {
    let fetched_entries: Vec<String> = fetched.iter().flat_map(|l| host_port_entries(l)).collect();
    let mut out: Vec<String> = git_known_hosts_lines(saved)
        .into_iter()
        .filter(|line| {
            !host_port_entries(line)
                .iter()
                .any(|e| fetched_entries.contains(e))
        })
        .collect();
    out.extend(fetched.iter().cloned());
    out
}

/// `gitKnownHostFingerprint($line)`: the host, key type, and SHA256 fingerprint of a known_hosts
/// line, in the format that `ssh-keygen -lf` shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownHostFingerprint {
    pub host: String,
    pub key_type: String,
    pub fingerprint: String,
}

pub fn git_known_host_fingerprint(line: &str) -> KnownHostFingerprint {
    use sha2::Digest as _;
    let mut parts = line.split(' ');
    let host = parts.next().unwrap_or_default().to_owned();
    let key_type = parts.next().unwrap_or_default().to_owned();
    let key = B64
        .decode(parts.next().unwrap_or_default())
        .unwrap_or_default();
    KnownHostFingerprint {
        host,
        key_type,
        fingerprint: format!("SHA256:{}", B64_NO_PAD.encode(sha2::Sha256::digest(&key))),
    }
}

fn write_command(content: &str, file: &str) -> String {
    format!(
        "echo '{}' | base64 -d | tee {file} > /dev/null",
        B64.encode(content)
    )
}

/// `gitSshSetupCommand($uuid, $knownHosts, $onServer)`: a shell command that writes the SSH
/// config and known_hosts file that `git_ssh_command()` uses. Run it in the place where git runs
/// (the helper container or the server) before git uses SSH.
///
/// The hosts in `GIT_PINNED_HOST_KEYS` must show one of their published keys, or for gitlab.com
/// also a key in `known_hosts`. Other hosts are trusted on first use: SSH adds the key of a new
/// host to the known_hosts file and rejects a changed key of a known host. `known_hosts` are the
/// keys that earlier deployments saved (`applications.git_ssh_known_hosts`).
///
/// With `on_server`, the command first tests whether the ssh of the server supports accept-new
/// (OpenSSH 7.6+). An older ssh rejects the whole config, so there every host is checked strictly
/// against the pinned and saved keys, and the config of root and the system config are appended
/// to the file instead of included.
pub fn git_ssh_setup_command(uuid: &str, known_hosts: Option<&str>, on_server: bool) -> String {
    git_ssh_setup_command_with(uuid, known_hosts, on_server, &git_provider_host_keys())
}

/// `git_ssh_setup_command()` with the provider keys given.
pub fn git_ssh_setup_command_with(
    uuid: &str,
    known_hosts: Option<&str>,
    on_server: bool,
    provider_keys: &[String],
) -> String {
    let known_hosts_file = git_known_hosts_file(uuid);
    let config_file = git_ssh_config_file(uuid);
    let pinned_hosts = git_pinned_hosts().join(" ");
    let config = format!(
        "Host {pinned_hosts}\n    StrictHostKeyChecking yes\nHost *\n    StrictHostKeyChecking accept-new\n    UserKnownHostsFile {known_hosts_file}\n    HashKnownHosts no\n    Include ~/.ssh/config /etc/ssh/ssh_config\n"
    );
    let mut lines: Vec<String> = provider_keys.to_vec();
    lines.extend(git_known_hosts_lines(known_hosts));
    let known_hosts_content = format!("{}\n", lines.join("\n"));
    let setup = format!(
        "mkdir -p /root/.ssh && {} && ",
        write_command(&known_hosts_content, &known_hosts_file)
    );
    if !on_server {
        return format!("{setup}{}", write_command(&config, &config_file));
    }
    let strict_config = format!(
        "Host *\n    StrictHostKeyChecking yes\n    UserKnownHostsFile {known_hosts_file}\n    HashKnownHosts no\n"
    );
    format!(
        "{setup}if ssh -F /dev/null -G -o StrictHostKeyChecking=accept-new localhost > /dev/null 2>&1; then {}; else {{ echo '{}' | base64 -d; cat /root/.ssh/config /etc/ssh/ssh_config 2> /dev/null; }} | tee {config_file} > /dev/null; fi",
        write_command(&config, &config_file),
        B64.encode(strict_config)
    )
}

/// `gitSshCommand($uuid, $port, $identityFile, $identitiesOnly)`: the SSH command that git uses
/// to reach a repository. Run `git_ssh_setup_command()` with the same `uuid` first. Without a
/// port, SSH uses the port of the repository URL. The keepalive options stop a stalled connection
/// after about one minute; without them git waits until the deployment timeout.
pub fn git_ssh_command(
    uuid: &str,
    port: Option<&str>,
    identity: Option<&str>,
    identities_only: bool,
) -> String {
    let mut command = format!(
        "ssh -F {} -o ConnectTimeout=30 -o ServerAliveInterval=15 -o ServerAliveCountMax=4",
        git_ssh_config_file(uuid)
    );
    if let Some(port) = port {
        command.push_str(&format!(" -p {port} -o Port={port}"));
    }
    command.push_str(" -o LogLevel=ERROR");
    if let Some(file) = identity {
        command.push_str(&format!(" -i {file}"));
        if identities_only {
            command.push_str(" -o IdentitiesOnly=yes");
        }
    }
    command
}

/// `$hostAndKeyType` of `save_git_ssh_known_hosts()`: the first two fields of a line.
fn host_and_key_type(line: &str) -> String {
    line.split(' ').take(2).collect::<Vec<_>>().join(" ")
}

/// The lines of the helper's known_hosts file that `save_git_ssh_known_hosts()` may save: not the
/// built-in keys, and no key of a host and key type that is already saved (two first deployments
/// can accept different keys at the same time; the key that was saved first is kept).
pub fn new_known_hosts_lines(
    file_content: &str,
    saved: Option<&str>,
    provider_keys: &[String],
) -> Vec<String> {
    let saved_types: Vec<String> = git_known_hosts_lines(saved)
        .iter()
        .map(|l| host_and_key_type(l))
        .collect();
    git_known_hosts_lines(Some(file_content))
        .into_iter()
        .filter(|line| !provider_keys.contains(line))
        .filter(|line| !saved_types.contains(&host_and_key_type(line)))
        .collect()
}

/// The result of merging new host keys into the stored ones under the row lock.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnownHostsMerge {
    /// The text to store (`implode("\n", [...$currentLines, ...$addedLines])`).
    pub merged: String,
    pub added: Vec<String>,
    /// Lines whose host and key type another deployment saved with a different key.
    pub conflicting: Vec<String>,
}

/// The transaction body of `save_git_ssh_known_hosts()`.
pub fn merge_known_hosts(current: Option<&str>, new_lines: &[String]) -> KnownHostsMerge {
    let current_lines = git_known_hosts_lines(current);
    let saved_types: Vec<String> = current_lines.iter().map(|l| host_and_key_type(l)).collect();
    let mut out = KnownHostsMerge::default();
    for line in new_lines {
        if current_lines.contains(line) {
            continue;
        }
        if saved_types.contains(&host_and_key_type(line)) {
            out.conflicting.push(line.clone());
        } else {
            out.added.push(line.clone());
        }
    }
    let mut all = current_lines;
    all.extend(out.added.iter().cloned());
    out.merged = all.join("\n");
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;

    const ED: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

    #[test]
    fn known_hosts_lines_are_validated() {
        assert!(is_valid_git_known_hosts_line(&format!(
            "git.example.com ssh-ed25519 {ED}"
        )));
        assert!(is_valid_git_known_hosts_line(&format!(
            "[git.example.com]:2222,10.0.0.1 ssh-ed25519 {ED}"
        )));
        // Markers, hashed names, comments and unknown key types are refused.
        assert!(!is_valid_git_known_hosts_line(&format!(
            "@cert-authority git.example.com ssh-ed25519 {ED}"
        )));
        assert!(!is_valid_git_known_hosts_line(&format!(
            "|1|abc=|def= ssh-ed25519 {ED}"
        )));
        assert!(!is_valid_git_known_hosts_line(&format!(
            "git.example.com ssh-ed25519 {ED} comment"
        )));
        assert!(!is_valid_git_known_hosts_line(&format!(
            "git.example.com ssh-dss {ED}"
        )));
        assert!(!is_valid_git_known_hosts_line(&format!(
            "git.example.com ssh-ed25519 {ED}\n"
        )));
        for line in GIT_PINNED_HOST_KEYS {
            assert!(is_valid_git_known_hosts_line(line), "{line}");
        }
    }

    #[test]
    fn hosts_of_a_line_lose_brackets_and_ports() {
        assert_eq!(
            git_known_hosts_line_hosts("GitHub.com,ssh.github.com,[ssh.github.com]:443 ssh-rsa x"),
            vec!["github.com", "ssh.github.com", "ssh.github.com"]
        );
        assert_eq!(
            git_pinned_hosts(),
            vec![
                "github.com",
                "ssh.github.com",
                "gitlab.com",
                "altssh.gitlab.com",
                "bitbucket.org",
                "altssh.bitbucket.org"
            ]
        );
        assert_eq!(
            git_hosts_with_published_keys(),
            vec![
                "github.com",
                "ssh.github.com",
                "bitbucket.org",
                "altssh.bitbucket.org"
            ]
        );
        assert_eq!(
            git_provider_host_key_source("SSH.GitHub.com"),
            Some("github.com,ssh.github.com,[ssh.github.com]:443")
        );
        assert_eq!(git_provider_host_key_source("gitlab.com"), None);
    }

    #[test]
    fn saved_lines_drop_duplicates_invalid_lines_and_published_hosts() {
        let text = format!(
            "git.example.com   ssh-ed25519\t{ED}\r\n\r\ngit.example.com ssh-ed25519 {ED}\nnot a line\ngithub.com ssh-ed25519 {ED}\ngitlab.com ssh-ed25519 {ED}\n"
        );
        assert_eq!(
            git_known_hosts_lines(Some(&text)),
            vec![
                format!("git.example.com ssh-ed25519 {ED}"),
                format!("gitlab.com ssh-ed25519 {ED}")
            ]
        );
        assert!(git_known_hosts_lines(None).is_empty());
    }

    #[test]
    fn fetched_keys_replace_the_lines_of_the_same_host_and_port() {
        let saved = format!(
            "git.example.com ssh-ed25519 {ED}\n[git.example.com]:2222 ssh-ed25519 {ED}\nother.example.com ssh-rsa {ED}"
        );
        let fetched = vec![format!("git.example.com ssh-rsa {ED}")];
        assert_eq!(
            git_known_hosts_replace_host_keys(Some(&saved), &fetched),
            vec![
                format!("[git.example.com]:2222 ssh-ed25519 {ED}"),
                format!("other.example.com ssh-rsa {ED}"),
                format!("git.example.com ssh-rsa {ED}"),
            ]
        );
    }

    #[test]
    fn fingerprint_is_the_unpadded_sha256_of_the_key() {
        let f = git_known_host_fingerprint(GIT_PINNED_HOST_KEYS[0]);
        assert_eq!(f.host, "github.com,ssh.github.com,[ssh.github.com]:443");
        assert_eq!(f.key_type, "ssh-ed25519");
        // The fingerprint GitHub documents for its Ed25519 key.
        assert_eq!(
            f.fingerprint,
            "SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU"
        );
    }

    #[test]
    fn ssh_command_uses_the_config_file_and_an_optional_port() {
        assert_eq!(
            git_ssh_command("dep", None, None, true),
            "ssh -F /root/.ssh/coolify_git_dep.config -o ConnectTimeout=30 -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -o LogLevel=ERROR"
        );
        assert_eq!(
            git_ssh_command(
                "dep",
                Some("2222"),
                Some("/root/.ssh/id_rsa_coolify_dep"),
                true
            ),
            "ssh -F /root/.ssh/coolify_git_dep.config -o ConnectTimeout=30 -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -p 2222 -o Port=2222 -o LogLevel=ERROR -i /root/.ssh/id_rsa_coolify_dep -o IdentitiesOnly=yes"
        );
        assert_eq!(
            git_ssh_command("dep", Some("22"), Some("/root/.ssh/id_rsa"), false),
            "ssh -F /root/.ssh/coolify_git_dep.config -o ConnectTimeout=30 -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -p 22 -o Port=22 -o LogLevel=ERROR -i /root/.ssh/id_rsa"
        );
    }

    fn decode_all(command: &str) -> Vec<String> {
        command
            .split("echo '")
            .skip(1)
            .map(|rest| {
                let b64 = rest.split('\'').next().unwrap();
                String::from_utf8(B64.decode(b64).unwrap()).unwrap()
            })
            .collect()
    }

    #[test]
    fn setup_command_writes_known_hosts_then_the_config() {
        let keys: Vec<String> = GIT_PINNED_HOST_KEYS
            .iter()
            .map(|l| (*l).to_owned())
            .collect();
        let saved = format!("git.example.com ssh-ed25519 {ED}\ngithub.com ssh-ed25519 {ED}");
        let command = git_ssh_setup_command_with("dep", Some(&saved), false, &keys);
        assert!(command.starts_with("mkdir -p /root/.ssh && echo '"));
        assert!(command.contains(
            "' | base64 -d | tee /root/.ssh/coolify_git_dep.known_hosts > /dev/null && echo '"
        ));
        assert!(
            command.ends_with("' | base64 -d | tee /root/.ssh/coolify_git_dep.config > /dev/null")
        );
        let written = decode_all(&command);
        assert_eq!(written.len(), 2);
        // The pinned keys, then the saved keys without the hosts whose keys are published.
        assert_eq!(
            written[0],
            format!("{}\ngit.example.com ssh-ed25519 {ED}\n", keys.join("\n"))
        );
        assert_eq!(
            written[1],
            "Host github.com ssh.github.com gitlab.com altssh.gitlab.com bitbucket.org altssh.bitbucket.org\n    StrictHostKeyChecking yes\nHost *\n    StrictHostKeyChecking accept-new\n    UserKnownHostsFile /root/.ssh/coolify_git_dep.known_hosts\n    HashKnownHosts no\n    Include ~/.ssh/config /etc/ssh/ssh_config\n"
        );
    }

    #[test]
    fn setup_command_on_a_server_falls_back_to_strict_checking_for_an_old_ssh() {
        let keys: Vec<String> = GIT_PINNED_HOST_KEYS
            .iter()
            .map(|l| (*l).to_owned())
            .collect();
        let command = git_ssh_setup_command_with("u", None, true, &keys);
        assert!(command.contains(
            " && if ssh -F /dev/null -G -o StrictHostKeyChecking=accept-new localhost > /dev/null 2>&1; then echo '"
        ));
        assert!(command.ends_with(
            "' | base64 -d; cat /root/.ssh/config /etc/ssh/ssh_config 2> /dev/null; } | tee /root/.ssh/coolify_git_u.config > /dev/null; fi"
        ));
        let written = decode_all(&command);
        assert_eq!(written.len(), 3);
        assert_eq!(
            written[2],
            "Host *\n    StrictHostKeyChecking yes\n    UserKnownHostsFile /root/.ssh/coolify_git_u.known_hosts\n    HashKnownHosts no\n"
        );
    }

    #[test]
    fn published_keys_replace_the_pinned_lines_of_their_provider() {
        let hosts = GIT_PROVIDER_HOST_KEY_SOURCES[0].0;
        let body = format!(r#"{{"ssh_keys": ["ssh-ed25519 {ED}", "ssh-ed25519 {ED}", 5]}}"#);
        let lines = published_host_key_lines(hosts, "https://api.github.com/meta", &body).unwrap();
        assert_eq!(lines, vec![format!("{hosts} ssh-ed25519 {ED}")]);
        let mut published = HashMap::new();
        published.insert(git_provider_host_keys_cache_key(hosts), lines.clone());
        let keys = provider_host_keys_with(&published);
        // Three GitHub lines became one, at the end.
        assert_eq!(keys.len(), 7);
        assert_eq!(keys.last(), lines.first());
        assert!(keys[0].starts_with("gitlab.com,"));

        assert_eq!(
            published_host_key_lines(hosts, "https://api.github.com/meta", "{}"),
            Err("The answer from https://api.github.com/meta has no valid host keys.".to_owned())
        );
    }

    #[test]
    fn bitbucket_keys_come_from_the_lines_of_its_first_host() {
        let hosts = GIT_PROVIDER_HOST_KEY_SOURCES[1].0;
        let body = format!(
            "bitbucket.org ssh-ed25519 {ED}\nother.org ssh-ed25519 {ED}\n\nbitbucket.org   ssh-rsa   {ED}\n"
        );
        assert_eq!(
            published_host_key_lines(hosts, "https://bitbucket.org/site/ssh", &body).unwrap(),
            vec![
                format!("{hosts} ssh-ed25519 {ED}"),
                format!("{hosts} ssh-rsa {ED}")
            ]
        );
    }

    #[test]
    fn only_new_hosts_and_key_types_are_saved() {
        let keys: Vec<String> = GIT_PINNED_HOST_KEYS
            .iter()
            .map(|l| (*l).to_owned())
            .collect();
        let other = "AAAAC3NzaC1lZDI1NTE5AAAAIAfuCHKVTjquxvt6CM6tdG4SLp1Btn/nOeHHE5UOzRdf";
        let file = format!(
            "{}\ngit.example.com ssh-ed25519 {other}\nnew.example.com ssh-ed25519 {ED}\n",
            keys.join("\n")
        );
        let saved = format!("git.example.com ssh-ed25519 {ED}");
        let new = new_known_hosts_lines(&file, Some(&saved), &keys);
        assert_eq!(new, vec![format!("new.example.com ssh-ed25519 {ED}")]);

        // Another deployment saved a different key for the same host and key type meanwhile.
        let current = format!("{saved}\nnew.example.com ssh-ed25519 {other}");
        let merge = merge_known_hosts(Some(&current), &new);
        assert!(merge.added.is_empty());
        assert_eq!(merge.conflicting, new);
        assert_eq!(merge.merged, current);

        let merge = merge_known_hosts(Some(&saved), &new);
        assert_eq!(merge.added, new);
        assert!(merge.conflicting.is_empty());
        assert_eq!(
            merge.merged,
            format!("{saved}\nnew.example.com ssh-ed25519 {ED}")
        );
    }
}
