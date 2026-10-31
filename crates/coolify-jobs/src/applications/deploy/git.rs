//! Git helpers of the deployment job: `Application::customRepository()`,
//! `deploymentType()`, `generateGitImportCommands()` + `setGitImportSettings()`,
//! `gitSshCommand()` and the escaping helpers the build steps use.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use coolify_db::applications::ApplicationRow;
use coolify_db::records::Record;
use coolify_remote::shell::quote;

use super::{Cmd, in_docker};

pub(super) const GITHUB_APP: &str = r"App\Models\GithubApp";
pub(super) const GITLAB_APP: &str = r"App\Models\GitlabApp";
pub(super) const GITEA_APP: &str = r"App\Models\GiteaApp";

/// The application's git source (`$this->source`): a `github_apps`, `gitlab_apps` or
/// `gitea_apps` row.
#[derive(Debug, Clone)]
pub(super) struct Source {
    pub row: Record,
    pub morph: String,
}

impl Source {
    fn str(&self, k: &str) -> Option<&str> {
        self.row.get(k).and_then(Value::as_str)
    }
    pub fn html_url(&self) -> &str {
        self.str("html_url").unwrap_or_default()
    }
    pub fn is_public(&self) -> bool {
        self.row
            .get("is_public")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
    pub fn is_github(&self) -> bool {
        self.morph == GITHUB_APP
    }
    pub fn is_gitea(&self) -> bool {
        self.morph == GITEA_APP
    }
    /// `GitlabApp::isConnected()` / `GiteaApp::isConnected()`.
    pub fn is_connected(&self) -> bool {
        let filled = |k: &str| self.str(k).is_some_and(|v| !v.is_empty());
        filled("access_token") && filled("refresh_token")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum DeploymentType {
    DeployKey,
    Source,
    #[default]
    Other,
}

/// `Application::deploymentType()`.
pub(super) fn deployment_type(a: &ApplicationRow, source: Option<&Source>) -> DeploymentType {
    match a.int("private_key_id") {
        Some(id) if id > 0 => DeploymentType::DeployKey,
        _ if source.is_some() => DeploymentType::Source,
        Some(0) => DeploymentType::DeployKey,
        _ => DeploymentType::Other,
    }
}

/// `parseScpStyleGitUrl()`: `(user, host, port, path)`.
pub(super) fn parse_scp(repo: &str) -> Option<(String, String, Option<String>, String)> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?<user>[A-Za-z0-9._-]+)@(?<host>\[[0-9A-Fa-f:.]+\]|[^:\[\]]+):(?:(?<port>\d+)/)?(?<path>.+)$")
            .unwrap_or_else(|_| unreachable!())
    });
    let c = RE.captures(repo)?;
    let host = c["host"].trim().to_owned();
    let path = c["path"].trim_start_matches('/').to_owned();
    if host.is_empty() || path.is_empty() {
        return None;
    }
    let port = c
        .name("port")
        .map(|p| p.as_str().to_owned())
        .filter(|p| !p.is_empty());
    Some((c["user"].to_owned(), host, port, path))
}

/// The port of `parse_url($url)` for a URL with a scheme.
fn url_port(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let port = if host_port.starts_with('[') {
        host_port.split_once("]:").map(|(_, p)| p)
    } else {
        host_port.rsplit_once(':').map(|(_, p)| p)
    }?;
    (!port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
        .then(|| port.trim_start_matches('0').to_owned())
        .map(|p| if p.is_empty() { "0".into() } else { p })
}

/// `convertGitUrl($git_repository, $deploymentType, $source)`: `(repository, port)`.
pub(super) fn convert_git_url(
    repo: &str,
    kind: DeploymentType,
    source: Option<&Source>,
) -> (String, String) {
    static SSH: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"((.*?)://)?(.*@.*:.*)").unwrap_or_else(|_| unreachable!()));
    let mut repository = repo.to_owned();
    let mut port = "22".to_owned();
    if kind == DeploymentType::DeployKey
        && !SSH.is_match(repo)
        && let Some(s) = source
    {
        let host = url_host(s.html_url()).unwrap_or_default();
        let custom_port = s
            .row
            .get("custom_port")
            .and_then(Value::as_i64)
            .unwrap_or(22);
        let user = s.str("custom_user").unwrap_or("git").to_owned();
        if !host.is_empty() {
            port = custom_port.to_string();
            repository = if custom_port == 22 {
                format!("{user}@{host}:{repo}")
            } else {
                format!("ssh://{user}@{host}:{custom_port}/{repo}")
            };
        }
    }
    if repository.contains("://") {
        if let Some(p) = url_port(&repository) {
            port = p;
        }
    } else if let Some((user, host, Some(p), path)) = parse_scp(&repository) {
        port = p;
        repository = format!("{user}@{host}:{path}");
    }
    (repository, port)
}

/// `Url::fromString($url)->getHost()` / `parse_url($url)['host']`.
pub(super) fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if host_port.starts_with('[') {
        host_port.split_once(']').map(|(h, _)| format!("{h}]"))?
    } else {
        host_port.split(':').next().unwrap_or_default().to_owned()
    };
    (!host.is_empty()).then_some(host)
}

/// `gitSshCommand($port, $identityFile, $identitiesOnly)`.
pub(super) fn git_ssh_command(port: &str, identity: Option<&str>, identities_only: bool) -> String {
    let mut command = format!(
        "ssh -o ConnectTimeout=30 -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -p {port} -o Port={port} -o LogLevel=ERROR -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null"
    );
    if let Some(file) = identity {
        command.push_str(&format!(" -i {file}"));
        if identities_only {
            command.push_str(" -o IdentitiesOnly=yes");
        }
    }
    command
}

/// The settings `setGitImportSettings()` reads.
#[derive(Debug, Clone, Copy)]
pub(super) struct ImportSettings {
    pub shallow: bool,
    pub submodules: bool,
    pub lfs: bool,
}

impl ImportSettings {
    pub fn of(a: &ApplicationRow) -> Self {
        let flag = |k: &str| a.settings.get(k).and_then(Value::as_bool).unwrap_or(false);
        ImportSettings {
            shallow: flag("is_git_shallow_clone_enabled"),
            submodules: flag("is_git_submodules_enabled"),
            lfs: flag("is_git_lfs_enabled"),
        }
    }
}

/// `Application::setGitImportSettings(...)`.
pub(super) fn set_git_import_settings(
    clone: &str,
    s: ImportSettings,
    public: bool,
    commit: &str,
    git_ssh_command: Option<&str>,
    git_config_options: Option<&str>,
    base_dir: &str,
) -> String {
    let dir = quote(base_dir);
    let git = git_config_options.map_or_else(|| "git".to_owned(), |o| format!("git {o}"));
    let ssh = match git_ssh_command {
        Some(c) if c.starts_with("GIT_SSH_COMMAND=") => c.to_owned(),
        Some(c) => format!("GIT_SSH_COMMAND=\"{c}\""),
        None => {
            "GIT_SSH_COMMAND=\"ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null\""
                .to_owned()
        }
    };
    let mut cmd = clone.to_owned();
    if commit != "HEAD" {
        let c = quote(commit);
        if s.shallow {
            cmd = format!(
                "{cmd} && cd {dir} && echo 'Fetching the commit.' && {ssh} {git} fetch --depth=1 origin {c} && {git} -c advice.detachedHead=false checkout {c} >/dev/null 2>&1"
            );
        } else {
            cmd = format!(
                "{cmd} && cd {dir} && {ssh} {git} -c advice.detachedHead=false checkout {c} >/dev/null 2>&1"
            );
        }
    }
    if s.submodules {
        cmd = format!("{cmd} && cd {dir} && if [ -f .gitmodules ]; then");
        if public {
            cmd = format!(
                "{cmd} sed -i \"s#[A-Za-z0-9._-]*@\\(.*\\):#https://\\1/#g\" {dir}/.gitmodules || true &&"
            );
        }
        let flags = if s.shallow { "--depth=1" } else { "" };
        cmd = format!(
            "{cmd} echo 'Updating submodules.' && {git} submodule sync && {ssh} {git} submodule update --init --recursive {flags}; fi"
        );
    }
    if s.lfs {
        cmd = format!(
            "{cmd} && cd {dir} && if {git} lfs ls-files -n | grep -q .; then echo 'Downloading Git LFS objects.' && {ssh} {git} lfs pull; fi"
        );
    }
    cmd
}

/// What `generateGitImportCommands()` returns.
#[derive(Debug, Clone)]
pub(super) struct GitImport {
    pub commands: Vec<Cmd>,
    pub branch: String,
    pub full_repo_url: String,
}

/// Inputs of `generateGitImportCommands()` for the deployment (pull request 0, in the helper).
pub(super) struct ImportInput<'a> {
    pub dep: &'a str,
    pub branch: &'a str,
    pub repository: &'a str,
    pub port: &'a str,
    pub kind: DeploymentType,
    pub source: Option<&'a Source>,
    pub settings: ImportSettings,
    pub commit: &'a str,
    /// The decrypted deploy key (deploy_key type).
    pub private_key: Option<&'a str>,
    /// A GitHub App installation token (private GitHub App sources).
    pub github_token: Option<&'a str>,
    /// `generateGitlabCloneToken()` / `generateGiteaCloneToken()` for a connected source.
    pub oauth_token: Option<&'a str>,
    /// `gitlabAppPrivateKey()` / `giteaAppPrivateKey()` for a source without OAuth tokens.
    pub source_key: Option<&'a str>,
}

fn hidden(command: String) -> Cmd {
    Cmd::new(command).hidden()
}

/// `gitSshKeySetupCommands($uuid, $privateKey, true)`.
fn ssh_key_setup(dep: &str, private_key_b64: &str) -> Vec<Cmd> {
    let key = format!("/root/.ssh/id_rsa_coolify_{dep}");
    vec![
        hidden(in_docker(dep, "mkdir -p /root/.ssh")),
        hidden(in_docker(
            dep,
            &format!("echo '{private_key_b64}' | base64 -d | tee {key} > /dev/null"),
        ))
        .skip_command_log(),
        hidden(in_docker(dep, &format!("chmod 600 {key}"))),
    ]
}

/// `Application::generateGitImportCommands($uuid, 0, null, true, false, null, $commit)`.
pub(super) fn generate_git_import_commands(i: &ImportInput<'_>) -> Result<GitImport, String> {
    use base64::Engine as _;
    let base_dir = format!("/artifacts/{}", i.dep);
    let dir = quote(&base_dir);
    let key_location = format!("/root/.ssh/id_rsa_coolify_{}", i.dep);
    let depth = if i.settings.shallow { " --depth=1" } else { "" };
    let mut submodules = String::new();
    if i.settings.submodules {
        submodules.push_str(" --recurse-submodules");
        if i.settings.shallow {
            submodules.push_str(" --shallow-submodules");
        }
    }
    let mut clone = format!("git clone{depth}{submodules} -b {}", quote(i.branch));
    match i.kind {
        DeploymentType::Source => {
            let source = i.source.ok_or("The application has no git source.")?;
            if !source.is_github() {
                return oauth_import(i, source, clone, &base_dir);
            }
            if source.is_public() {
                let full = format!("{}/{}", source.html_url(), i.repository);
                clone = format!("{clone} {} {dir}", quote(&full));
                clone = set_git_import_settings(
                    &clone, i.settings, true, i.commit, None, None, &base_dir,
                );
                return Ok(GitImport {
                    commands: vec![hidden(in_docker(i.dep, &clone))],
                    branch: i.branch.to_owned(),
                    full_repo_url: full,
                });
            }
            let token = i.github_token.ok_or("No GitHub App installation token.")?;
            let encoded = urlencoding_raw(token);
            let html = source.html_url();
            let scheme = html.split_once("://").map_or("https", |(s, _)| s);
            let host = url_host(html).unwrap_or_default();
            let option = format!(
                "-c {}",
                quote(&format!(
                    "url.{scheme}://x-access-token:{encoded}@{host}/.insteadOf={scheme}://{host}/"
                ))
            );
            clone = clone.replacen("git clone", &format!("git {option} clone"), 1);
            let repo_url = format!(
                "{scheme}://x-access-token:{encoded}@{host}/{}.git",
                i.repository
            );
            clone = format!("{clone} {} {dir}", quote(&repo_url));
            clone = set_git_import_settings(
                &clone,
                i.settings,
                false,
                i.commit,
                None,
                Some(&option),
                &base_dir,
            );
            Ok(GitImport {
                commands: vec![hidden(in_docker(i.dep, &clone))],
                branch: i.branch.to_owned(),
                full_repo_url: repo_url,
            })
        }
        DeploymentType::DeployKey => {
            let key = i.private_key.ok_or(
                "Private key not found. Please add a private key to the application and try again.",
            )?;
            let key_b64 = base64::engine::general_purpose::STANDARD.encode(key);
            let ssh = git_ssh_command(i.port, Some(&key_location), true);
            let base = format!(
                "GIT_SSH_COMMAND=\"{ssh}\" {clone} {} {dir}",
                quote(i.repository)
            );
            let command = set_git_import_settings(
                &base,
                i.settings,
                false,
                i.commit,
                Some(&ssh),
                None,
                &base_dir,
            );
            let mut commands = ssh_key_setup(i.dep, &key_b64);
            commands.push(hidden(in_docker(i.dep, &command)));
            Ok(GitImport {
                commands,
                branch: i.branch.to_owned(),
                full_repo_url: i.repository.to_owned(),
            })
        }
        DeploymentType::Other => {
            clone = format!("{clone} {} {dir}", quote(i.repository));
            clone =
                set_git_import_settings(&clone, i.settings, true, i.commit, None, None, &base_dir);
            Ok(GitImport {
                commands: vec![hidden(in_docker(i.dep, &clone))],
                branch: i.branch.to_owned(),
                full_repo_url: i.repository.to_owned(),
            })
        }
    }
}

/// The `GitlabApp` / `GiteaApp` branch of `generateGitImportCommands()` (pull request 0).
fn oauth_import(
    i: &ImportInput<'_>,
    source: &Source,
    clone: String,
    base_dir: &str,
) -> Result<GitImport, String> {
    use base64::Engine as _;
    let dir = quote(base_dir);
    if source.is_connected() {
        let token = i.oauth_token.ok_or("No access token for the git source.")?;
        let encoded = urlencoding_raw(token);
        let (scheme, host, prefix) = crate::git_http::oauth_url_parts(source.html_url());
        let option = format!(
            "-c {}",
            quote(&format!(
                "url.{scheme}://oauth2:{encoded}@{host}{prefix}/.insteadOf={scheme}://{host}{prefix}/"
            ))
        );
        let clone = clone.replacen("git clone", &format!("git {option} clone"), 1);
        let repo_url = format!(
            "{scheme}://oauth2:{encoded}@{host}{prefix}/{}.git",
            i.repository
        );
        let base = format!("{clone} {} {dir}", quote(&repo_url));
        let command = set_git_import_settings(
            &base,
            i.settings,
            false,
            i.commit,
            None,
            Some(&option),
            base_dir,
        );
        return Ok(GitImport {
            commands: vec![hidden(in_docker(i.dep, &command))],
            branch: i.branch.to_owned(),
            full_repo_url: repo_url,
        });
    }
    if let Some(key) = i.source_key {
        let key_location = format!("/root/.ssh/id_rsa_coolify_{}", i.dep);
        let key_b64 = base64::engine::general_purpose::STANDARD.encode(key);
        let port = source
            .row
            .get("custom_port")
            .and_then(Value::as_i64)
            .unwrap_or(22)
            .to_string();
        let ssh = git_ssh_command(&port, Some(&key_location), true);
        let base = format!(
            "GIT_SSH_COMMAND=\"{ssh}\" {clone} {} {dir}",
            quote(i.repository)
        );
        let command = set_git_import_settings(
            &base,
            i.settings,
            false,
            i.commit,
            Some(&ssh),
            None,
            base_dir,
        );
        let mut commands = ssh_key_setup(i.dep, &key_b64);
        commands.push(hidden(in_docker(i.dep, &command)));
        return Ok(GitImport {
            commands,
            branch: i.branch.to_owned(),
            full_repo_url: i.repository.to_owned(),
        });
    }
    let clone = format!("{clone} {} {dir}", quote(i.repository));
    let clone = set_git_import_settings(&clone, i.settings, true, i.commit, None, None, base_dir);
    Ok(GitImport {
        commands: vec![hidden(in_docker(i.dep, &clone))],
        branch: i.branch.to_owned(),
        full_repo_url: i.repository.to_owned(),
    })
}

/// PHP `rawurlencode()`.
pub(super) fn urlencoding_raw(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `escapeShellValue()` / `escapeshellarg()`.
pub(super) fn shell_value(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\\''"))
}

/// `escapeBashEnvValue()`.
pub(super) fn bash_env_value(v: Option<&str>) -> String {
    match v {
        None | Some("") => "''".to_owned(),
        Some(v) => format!("'{}'", v.replace('\'', "'\\''")),
    }
}

/// `escapeBashDoubleQuoted()`.
pub(super) fn bash_double_quoted(v: Option<&str>) -> String {
    let Some(v) = v.filter(|v| !v.is_empty()) else {
        return "\"\"".to_owned();
    };
    let escaped = v
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`");
    // `$` not followed by a variable start: preg_replace walks left to right, so a run of `$`
    // escapes each one.
    let mut out = String::with_capacity(escaped.len());
    let chars: Vec<char> = escaped.chars().collect();
    for (idx, c) in chars.iter().enumerate() {
        if *c == '$' {
            let next = chars.get(idx + 1);
            let valid = next.is_some_and(|n| n.is_ascii_alphanumeric() || *n == '_' || *n == '{');
            if !valid {
                out.push('\\');
            }
        }
        out.push(*c);
    }
    format!("\"{out}\"")
}

/// `escapeEnvVariables()`.
pub(super) fn escape_env_variables(v: &str) -> String {
    crate::databases::env::escape_env_variables(v)
}

/// `EnvironmentVariable::getResolvedValueWithServer()` from the resolved value.
pub(super) fn resolved_with_server(
    resolved: Option<&str>,
    literal_or_multiline: bool,
) -> Option<String> {
    let v = resolved.unwrap_or_default();
    if (v.starts_with('{') || v.starts_with('[')) && serde_json::from_str::<Value>(v).is_ok() {
        return Some(v.to_owned());
    }
    Some(if literal_or_multiline {
        format!("'{v}'")
    } else {
        escape_env_variables(v)
    })
}

/// `defaultNginxConfiguration($type)`.
pub(super) fn default_nginx_configuration(spa: bool) -> &'static str {
    if spa {
        "server {\n    location / {\n        root /usr/share/nginx/html;\n        index index.html;\n        try_files $uri $uri/ /index.html;\n    }\n\n    # Handle 404 errors\n    error_page 404 /404.html;\n    location = /404.html {\n        root /usr/share/nginx/html;\n        internal;\n    }\n\n    # Handle server errors (50x)\n    error_page 500 502 503 504 /50x.html;\n    location = /50x.html {\n        root /usr/share/nginx/html;\n        internal;\n    }\n}"
    } else {
        "server {\n    location / {\n        root /usr/share/nginx/html;\n        index index.html index.htm;\n        try_files $uri $uri.html $uri/index.html $uri/index.htm $uri/ =404;\n    }\n\n    # Handle 404 errors\n    error_page 404 /404.html;\n    location = /404.html {\n        root /usr/share/nginx/html;\n        internal;\n    }\n\n    # Handle server errors (50x)\n    error_page 500 502 503 504 /50x.html;\n    location = /50x.html {\n        root /usr/share/nginx/html;\n        internal;\n    }\n}"
    }
}

/// `parseDockerfileInterval()`.
pub(super) fn parse_dockerfile_interval(s: &str) -> i64 {
    let digits: String = s.chars().filter(char::is_ascii_digit).collect();
    let unit: String = s.chars().filter(|c| !c.is_ascii_digit()).collect();
    let value: i64 = digits.parse().unwrap_or(0);
    match if unit.is_empty() { "s" } else { unit.as_str() } {
        "ns" => value / 1_000_000_000,
        "us" | "µs" => value / 1_000_000,
        "ms" => value / 1000,
        "m" => value * 60,
        "h" => value * 3600,
        _ => value,
    }
}

/// What `parseHealthcheckFromDockerfile()` changes on the application.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct HealthcheckUpdate {
    pub reset: bool,
    pub interval: Option<i64>,
    pub timeout: Option<i64>,
    pub start_period: Option<i64>,
    pub retries: Option<i64>,
    pub found: bool,
}

/// `Application::parseHealthcheckFromDockerfile($dockerfile)`.
pub(super) fn parse_healthcheck(
    dockerfile: &str,
    health_check_disabled: bool,
    custom_found: bool,
) -> HealthcheckUpdate {
    let content = dockerfile.trim();
    let has = content.contains("HEALTHCHECK");
    if !has && custom_found {
        return HealthcheckUpdate {
            reset: true,
            ..Default::default()
        };
    }
    let mut out = HealthcheckUpdate::default();
    if !has || !health_check_disabled {
        return out;
    }
    let trim_bs = |s: &str| s.trim_matches(|c| c == '\\' || c == ' ').to_owned();
    let mut command: Option<String> = None;
    for line in content.split('\n') {
        let t = line.trim();
        if t.starts_with("HEALTHCHECK") {
            command
                .get_or_insert_with(String::new)
                .push_str(&trim_bs(t));
            continue;
        }
        if let Some(c) = command.as_mut()
            && t.contains('\\')
        {
            c.push(' ');
            c.push_str(&trim_bs(t));
        }
        if let Some(c) = command.as_mut()
            && !t.contains('\\')
            && !c.is_empty()
        {
            c.push(' ');
            c.push_str(t);
            break;
        }
    }
    let Some(command) = command.filter(|c| !c.is_empty()) else {
        return out;
    };
    let grab = |re: &str| {
        Regex::new(re)
            .ok()
            .and_then(|r| r.captures(&command))
            .map(|c| c[1].to_owned())
    };
    out.interval = grab(r"--interval=([0-9]+[a-zµ]*)").map(|v| parse_dockerfile_interval(&v));
    out.timeout = grab(r"--timeout=([0-9]+[a-zµ]*)").map(|v| parse_dockerfile_interval(&v));
    out.start_period =
        grab(r"--start-period=([0-9]+[a-zµ]*)").map(|v| parse_dockerfile_interval(&v));
    out.retries = grab(r"--retries=(\d+)").and_then(|v| v.parse().ok());
    // Stringable results are always truthy, so any HEALTHCHECK line counts.
    out.found = true;
    out
}

/// `dockerfileInstructions()`: `(keyword, start, end)` per instruction.
pub(super) fn dockerfile_instructions(lines: &[String]) -> Vec<(String, usize, usize)> {
    static ESCAPE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^#\s*escape\s*=\s*([\\`])\s*$").unwrap_or_else(|_| unreachable!())
    });
    static HEREDOC: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"<<-?(["']?)([A-Za-z_][A-Za-z0-9_]*)(["']?)"#)
            .unwrap_or_else(|_| unreachable!())
    });
    let mut escape = '\\';
    let mut out: Vec<(String, usize, usize)> = Vec::new();
    let mut continues = false;
    let mut terminators: Vec<String> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let t = line.trim();
        if !terminators.is_empty() {
            if let Some(last) = out.last_mut() {
                last.2 = index;
            }
            if t == terminators[0] {
                terminators.remove(0);
            }
            continue;
        }
        if t.is_empty() || t.starts_with('#') {
            if out.is_empty()
                && let Some(c) = ESCAPE.captures(t)
            {
                escape = c[1].chars().next().unwrap_or('\\');
            }
            continue;
        }
        if continues {
            if let Some(last) = out.last_mut() {
                last.2 = index;
            }
        } else {
            let keyword = t
                .split([' ', '\t'])
                .next()
                .unwrap_or_default()
                .to_uppercase();
            out.push((keyword, index, index));
        }
        continues = t.ends_with(escape);
        let keyword = out.last().map(|l| l.0.clone()).unwrap_or_default();
        if matches!(keyword.as_str(), "RUN" | "COPY" | "ADD") {
            // `(?<!<)<<-?(["']?)(NAME)\1`: the closing quote must match the opening one.
            let mut found = Vec::new();
            for c in HEREDOC.captures_iter(line) {
                let m = c.get(0).map_or(0, |m| m.start());
                if m > 0 && line.as_bytes()[m - 1] == b'<' {
                    continue;
                }
                if c[1] == c[3] || c[1].is_empty() {
                    found.push(c[2].to_owned());
                }
            }
            if !found.is_empty() {
                terminators = found;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scp_and_ports() {
        assert_eq!(
            parse_scp("git@github.com:o/r.git"),
            Some(("git".into(), "github.com".into(), None, "o/r.git".into()))
        );
        assert_eq!(
            parse_scp("git@host:2222/o/r.git"),
            Some((
                "git".into(),
                "host".into(),
                Some("2222".into()),
                "o/r.git".into()
            ))
        );
        assert_eq!(
            convert_git_url("https://gitlab.com:8443/o/r", DeploymentType::Other, None),
            ("https://gitlab.com:8443/o/r".into(), "8443".into())
        );
        assert_eq!(
            convert_git_url("git@host:2222/o/r.git", DeploymentType::DeployKey, None),
            ("git@host:o/r.git".into(), "2222".into())
        );
        assert_eq!(
            convert_git_url("o/r", DeploymentType::Source, None),
            ("o/r".into(), "22".into())
        );
    }

    #[test]
    fn escaping() {
        assert_eq!(
            bash_double_quoted(Some("$COOLIFY_URL/api")),
            "\"$COOLIFY_URL/api\""
        );
        assert_eq!(bash_double_quoted(Some("a$&b$")), "\"a\\$&b\\$\"");
        assert_eq!(bash_double_quoted(Some("q\"`")), "\"q\\\"\\`\"");
        assert_eq!(bash_double_quoted(None), "\"\"");
        assert_eq!(bash_env_value(Some("it's")), "'it'\\''s'");
    }

    #[test]
    fn healthcheck() {
        let df = "FROM node\nHEALTHCHECK --interval=30s --timeout=3s \\\n  CMD curl -f http://localhost/ || exit 1\nCMD [\"node\"]";
        let u = parse_healthcheck(df, true, false);
        assert_eq!(u.interval, Some(30));
        assert_eq!(u.timeout, Some(3));
        assert!(u.found);
        assert_eq!(
            parse_healthcheck("FROM x", true, true),
            HealthcheckUpdate {
                reset: true,
                ..Default::default()
            }
        );
        assert_eq!(parse_dockerfile_interval("2m"), 120);
        assert_eq!(parse_dockerfile_interval("500ms"), 0);
    }

    #[test]
    fn instructions() {
        let lines: Vec<String> = [
            "# escape=`",
            "FROM a AS b",
            "RUN x `",
            "  y",
            "FROM c",
            "RUN <<EOF",
            "FROM fake",
            "EOF",
            "COPY . .",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let froms: Vec<usize> = dockerfile_instructions(&lines)
            .into_iter()
            .filter(|i| i.0 == "FROM")
            .map(|i| i.2)
            .collect();
        assert_eq!(froms, vec![1, 4]);
    }

    #[test]
    fn oauth_clone_command() {
        let source = Source {
            row: serde_json::from_str(
                r#"{"html_url":"http://git.example:8080/sub/","access_token":"x","refresh_token":"y"}"#,
            )
            .unwrap(),
            morph: GITLAB_APP.into(),
        };
        let s = ImportSettings {
            shallow: true,
            submodules: false,
            lfs: false,
        };
        let g = generate_git_import_commands(&ImportInput {
            dep: "abc",
            branch: "main",
            repository: "o/r",
            port: "22",
            kind: DeploymentType::Source,
            source: Some(&source),
            settings: s,
            commit: "HEAD",
            private_key: None,
            github_token: None,
            oauth_token: Some("t+k"),
            source_key: None,
        })
        .unwrap();
        assert_eq!(
            g.full_repo_url,
            "http://oauth2:t%2Bk@git.example:8080/sub/o/r.git"
        );
        assert!(g.commands[0].command.contains(
            "url.http://oauth2:t%2Bk@git.example:8080/sub/.insteadOf=http://git.example:8080/sub/"
        ));
    }

    #[test]
    fn clone_command() {
        let s = ImportSettings {
            shallow: true,
            submodules: true,
            lfs: true,
        };
        let source = Source {
            row: serde_json::from_str(r#"{"html_url":"https://github.com","is_public":true}"#)
                .unwrap(),
            morph: GITHUB_APP.into(),
        };
        let g = generate_git_import_commands(&ImportInput {
            dep: "abc",
            branch: "main",
            repository: "o/r",
            port: "22",
            kind: DeploymentType::Source,
            source: Some(&source),
            settings: s,
            commit: "deadbeef",
            private_key: None,
            github_token: None,
            oauth_token: None,
            source_key: None,
        })
        .unwrap();
        assert_eq!(g.full_repo_url, "https://github.com/o/r");
        assert_eq!(
            g.commands[0].command,
            r#"docker exec abc bash -c 'git clone --depth=1 --recurse-submodules --shallow-submodules -b '\''main'\'' '\''https://github.com/o/r'\'' '\''/artifacts/abc'\'' && cd '\''/artifacts/abc'\'' && echo '\''Fetching the commit.'\'' && GIT_SSH_COMMAND="ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null" git fetch --depth=1 origin '\''deadbeef'\'' && git -c advice.detachedHead=false checkout '\''deadbeef'\'' >/dev/null 2>&1 && cd '\''/artifacts/abc'\'' && if [ -f .gitmodules ]; then sed -i "s#[A-Za-z0-9._-]*@\(.*\):#https://\1/#g" '\''/artifacts/abc'\''/.gitmodules || true && echo '\''Updating submodules.'\'' && git submodule sync && GIT_SSH_COMMAND="ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null" git submodule update --init --recursive --depth=1; fi && cd '\''/artifacts/abc'\'' && if git lfs ls-files -n | grep -q .; then echo '\''Downloading Git LFS objects.'\'' && GIT_SSH_COMMAND="ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null" git lfs pull; fi'"#
        );
    }
}
