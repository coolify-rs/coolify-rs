//! Git helpers of the deployment job: `Application::customRepository()`,
//! `deploymentType()`, `generateGitImportCommands()` + `setGitImportSettings()`,
//! `gitSshCommand()` and the escaping helpers the build steps use.

use crate::applications::git_ssh;
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

/// `parse_url($this->git_repository)` reduced to `scheme://host[:port]/` for an http(s) URL with a
/// host (`gitHttpsCredentialOrigin()`).
pub(crate) fn git_https_credential_origin(repository: &str) -> Option<String> {
    let (scheme, rest) = repository.split_once("://")?;
    let scheme = scheme.to_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = if let Some(end) = host_port.strip_prefix('[').and_then(|r| r.find(']')) {
        let host = &host_port[..end + 2];
        let port = host_port[end + 2..].strip_prefix(':');
        (host, port)
    } else {
        match host_port.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (host_port, None),
        }
    };
    if host.trim().is_empty() {
        return None;
    }
    // `isset($url['port'])`: parse_url drops an empty port and fails on a non-numeric one.
    let port = match port {
        Some("") | None => String::new(),
        Some(p) if p.bytes().all(|b| b.is_ascii_digit()) => format!(":{}", p.parse::<u32>().ok()?),
        Some(_) => return None,
    };
    Some(format!("{scheme}://{host}{port}/"))
}

/// `gitHttpsCredentialBasicToken()`: `base64(username:password)`.
pub(crate) fn git_https_basic_token(username: Option<&str>, password: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .encode(format!("{}:{password}", username.unwrap_or_default()))
}

/// `gitHttpsCredentialConfigOption()`: a `git -c` option that sends the HTTPS credential as a Basic
/// auth header, only to the host of the repository, when the application clones from a custom Git
/// host (`deploymentType() === 'other'`). `password` is the decrypted `git_https_password`.
pub(crate) fn git_https_config_option(
    repository: Option<&str>,
    other_deployment: bool,
    username: Option<&str>,
    password: Option<&str>,
) -> Option<String> {
    let password = password.filter(|p| !p.trim().is_empty())?;
    if !other_deployment {
        return None;
    }
    let origin = git_https_credential_origin(repository.unwrap_or_default())?;
    Some(format!(
        "-c {}",
        quote(&format!(
            "http.{origin}.extraHeader=Authorization: Basic {}",
            git_https_basic_token(username, password)
        ))
    ))
}

/// The decrypted `git_https_password` of an application (`encrypted` cast); `None` when the
/// column is missing, null or does not decrypt.
pub(crate) fn git_https_password(
    a: &ApplicationRow,
    enc: Option<&coolify_compat::Encrypter>,
) -> Option<String> {
    let cipher = a.str("git_https_password")?;
    enc?.decrypt_string(cipher).ok()
}

/// `$application->gitHttpsCredentialConfigOption()` of a deployment.
pub(super) fn application_git_https_option(
    a: &ApplicationRow,
    kind: DeploymentType,
    enc: Option<&coolify_compat::Encrypter>,
) -> Option<String> {
    git_https_config_option(
        a.str("git_repository"),
        kind == DeploymentType::Other,
        a.str("git_https_username"),
        git_https_password(a, enc).as_deref(),
    )
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

/// `gitSshCommand($uuid, $port, $identityFile, $identitiesOnly)` with a port. The helper container
/// (or the server) must have run `gitSshSetupCommand()` with the same uuid first.
pub(super) fn git_ssh_command(
    dep: &str,
    port: &str,
    identity: Option<&str>,
    identities_only: bool,
) -> String {
    git_ssh::git_ssh_command(dep, Some(port), identity, identities_only)
}

/// The settings `setGitImportSettings()` reads.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ImportSettings {
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

/// `Application::gitLfsSkipSmudgePrefix($onlyCheckout)`: the helper image installs the Git LFS
/// filter system-wide, so a checkout downloads LFS objects unless the smudge filter is skipped.
pub(crate) fn git_lfs_skip_smudge_prefix(lfs: bool, only_checkout: bool) -> &'static str {
    if lfs && !only_checkout {
        ""
    } else {
        "export GIT_LFS_SKIP_SMUDGE=1 && "
    }
}

/// `Application::setGitImportSettings(...)`.
#[allow(clippy::too_many_arguments)]
pub(super) fn set_git_import_settings(
    dep: &str,
    clone: &str,
    s: ImportSettings,
    public: bool,
    commit: &str,
    git_ssh_command: Option<&str>,
    git_config_options: Option<&str>,
    base_dir: &str,
) -> String {
    set_git_import_settings_with(
        dep,
        clone,
        s,
        public,
        commit,
        git_ssh_command,
        git_config_options,
        base_dir,
        false,
    )
}

/// `Application::setGitImportSettings(..., onlyCheckout: $onlyCheckout)`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn set_git_import_settings_with(
    dep: &str,
    clone: &str,
    s: ImportSettings,
    public: bool,
    commit: &str,
    git_ssh_command: Option<&str>,
    git_config_options: Option<&str>,
    base_dir: &str,
    only_checkout: bool,
) -> String {
    static SHORT_SHA: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\A[0-9a-fA-F]{7,39}\z").unwrap_or_else(|_| unreachable!())
    });
    let dir = quote(base_dir);
    let git = git_config_options.map_or_else(|| "git".to_owned(), |o| format!("git {o}"));
    let ssh = match git_ssh_command {
        Some(c) if c.starts_with("GIT_SSH_COMMAND=") => c.to_owned(),
        Some(c) => format!("GIT_SSH_COMMAND=\"{c}\""),
        // `gitSshCommand($deployment_uuid)`.
        None => format!(
            "GIT_SSH_COMMAND=\"{}\"",
            git_ssh::git_ssh_command(dep, None, None, true)
        ),
    };
    let mut cmd = format!(
        "{}{clone}",
        git_lfs_skip_smudge_prefix(s.lfs, only_checkout)
    );
    if commit != "HEAD" {
        let c = quote(commit);
        if s.shallow {
            let checkout =
                format!("{git} -c advice.detachedHead=false checkout {c} >/dev/null 2>&1");
            if SHORT_SHA.is_match(commit) {
                // A remote cannot fetch a short SHA: fetch the tip of each branch and let the
                // checkout expand it, then the full history when it is not a branch tip.
                let refspec = quote("+refs/heads/*:refs/remotes/origin/*");
                cmd = format!(
                    "{cmd} && cd {dir} && echo 'Fetching the commit.' && {ssh} {git} fetch --depth=1 origin {refspec} && ({checkout} || (echo 'The commit is not a branch tip. Fetching the full history.' && {ssh} {git} fetch --unshallow origin {refspec} && {checkout}))"
                );
            } else {
                cmd = format!(
                    "{cmd} && cd {dir} && echo 'Fetching the commit.' && {ssh} {git} fetch --depth=1 origin {c} && {checkout}"
                );
            }
        } else {
            cmd = format!(
                "{cmd} && cd {dir} && {ssh} {git} -c advice.detachedHead=false checkout {c} >/dev/null 2>&1"
            );
        }
    }
    if s.submodules && !only_checkout {
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
    if s.lfs && !only_checkout {
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
    pub pull_request_id: i64,
    pub git_type: Option<&'a str>,
    /// `gitHttpsCredentialConfigOption()` ("other" type with HTTPS credentials).
    pub https_option: Option<&'a str>,
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

/// `buildGitCheckoutCommand($target, $gitSshCommand, $gitConfigOptions)`.
fn git_checkout_command(
    target: &str,
    settings: ImportSettings,
    ssh: &str,
    git_config: Option<&str>,
) -> String {
    let git = git_config.map_or_else(|| "git".to_owned(), |o| format!("git {o}"));
    let mut command = format!("{git} checkout {}", quote(target));
    if settings.submodules {
        command.push_str(&format!(
            " && GIT_SSH_COMMAND=\"{ssh}\" {git} submodule update --init --recursive"
        ));
    }
    command
}

/// The pull request part of `generateGitImportCommands()` for a GitHub App source: a second
/// command, run after the clone (`gitCommandsAsShellCommand()` joins them).
fn github_pull_request_fetch(
    i: &ImportInput<'_>,
    clone: String,
    git_config: Option<&str>,
) -> (Vec<Cmd>, String) {
    let first = in_docker(i.dep, &clone);
    if i.pull_request_id == 0 {
        return (vec![hidden(first)], i.branch.to_owned());
    }
    let pr_branch = format!("pr-{}-coolify", i.pull_request_id);
    let branch = format!("pull/{}/head:{pr_branch}", i.pull_request_id);
    let dir = quote(&format!("/artifacts/{}", i.dep));
    let checkout = git_checkout_command(
        &pr_branch,
        i.settings,
        &git_ssh::git_ssh_command(i.dep, None, None, true),
        git_config,
    );
    let second = in_docker(
        i.dep,
        &format!(
            "{}cd {dir} && git fetch origin {} && {checkout}",
            git_lfs_skip_smudge_prefix(i.settings.lfs, false),
            quote(&branch)
        ),
    );
    (vec![hidden(format!("{first} && {second}"))], branch)
}

/// The pull request part of the deploy key and "other" types: an `echo` command and the clone
/// command extended with the fetch and the checkout. `ssh_prefix` goes before `git fetch`.
fn pull_request_clone(
    i: &ImportInput<'_>,
    clone: String,
    ssh_prefix: &str,
    ssh: &str,
    git_config: Option<&str>,
    commands: &mut Vec<Cmd>,
) -> (String, String) {
    let git = git_config.map_or_else(|| "git".to_owned(), |o| format!("git {o}"));
    if i.pull_request_id == 0 {
        return (clone, i.branch.to_owned());
    }
    let dir = quote(&format!("/artifacts/{}", i.dep));
    let pr_branch = format!("pr-{}-coolify", i.pull_request_id);
    let echo = |branch: &str| hidden(in_docker(i.dep, &format!("echo 'Checking out {branch}'")));
    let fetch = |branch: &str| {
        format!(
            "{clone} && cd {dir} && {ssh_prefix} {git} fetch origin {branch} && {}",
            git_checkout_command(&pr_branch, i.settings, ssh, git_config)
        )
    };
    match i.git_type {
        Some("gitlab") => {
            let branch = format!("merge-requests/{}/head:{pr_branch}", i.pull_request_id);
            commands.push(echo(&branch));
            (fetch(&branch), branch)
        }
        Some("github" | "gitea") => {
            let branch = format!("pull/{}/head:{pr_branch}", i.pull_request_id);
            commands.push(echo(&branch));
            (fetch(&branch), branch)
        }
        Some("bitbucket") => {
            commands.push(echo(i.branch));
            (
                format!(
                    "{clone} && cd {dir} && {ssh_prefix} {}",
                    git_checkout_command(i.commit, i.settings, ssh, git_config)
                ),
                i.branch.to_owned(),
            )
        }
        _ => (clone, i.branch.to_owned()),
    }
}

/// The error for a repository that git would read as an option.
pub const INVALID_REPOSITORY: &str = "The git repository is not valid.";

/// Whether git would read `value` as an option rather than a repository or branch.
pub fn is_option_like(value: &str) -> bool {
    value.trim_start().starts_with('-')
}

/// `Application::generateGitImportCommands($uuid, $pull_request_id, $git_type, true, false, null, $commit)`.
pub(super) fn generate_git_import_commands(i: &ImportInput<'_>) -> Result<GitImport, String> {
    use base64::Engine as _;
    // git reads an argument that starts with `-` as an option (`--upload-pack=...` runs a program).
    if is_option_like(i.repository) {
        return Err(INVALID_REPOSITORY.into());
    }
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
    // `--` ends the options, so a repository value that starts with `-` is not read as a git option.
    let mut clone = format!("git clone{depth}{submodules} -b {} --", quote(i.branch));
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
                    i.dep, &clone, i.settings, true, i.commit, None, None, &base_dir,
                );
                let (commands, branch) = github_pull_request_fetch(i, clone, None);
                return Ok(GitImport {
                    commands,
                    branch,
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
                i.dep,
                &clone,
                i.settings,
                false,
                i.commit,
                None,
                Some(&option),
                &base_dir,
            );
            let (commands, branch) = github_pull_request_fetch(i, clone, Some(&option));
            Ok(GitImport {
                commands,
                branch,
                full_repo_url: repo_url,
            })
        }
        DeploymentType::DeployKey => {
            let key = i.private_key.ok_or(
                "Private key not found. Please add a private key to the application and try again.",
            )?;
            let key_b64 = base64::engine::general_purpose::STANDARD.encode(key);
            let ssh = git_ssh_command(i.dep, i.port, Some(&key_location), true);
            let base = format!(
                "GIT_SSH_COMMAND=\"{ssh}\" {clone} {} {dir}",
                quote(i.repository)
            );
            let command = set_git_import_settings(
                i.dep,
                &base,
                i.settings,
                false,
                i.commit,
                Some(&ssh),
                None,
                &base_dir,
            );
            let mut commands = ssh_key_setup(i.dep, &key_b64);
            let prefix = format!("GIT_SSH_COMMAND=\"{ssh}\"");
            let (command, branch) =
                pull_request_clone(i, command, &prefix, &ssh, None, &mut commands);
            commands.push(hidden(in_docker(i.dep, &command)));
            Ok(GitImport {
                commands,
                branch,
                full_repo_url: i.repository.to_owned(),
            })
        }
        DeploymentType::Other => {
            if let Some(option) = i.https_option {
                clone = clone.replace("git clone", &format!("git {option} clone"));
            }
            clone = format!("{clone} {} {dir}", quote(i.repository));
            clone = set_git_import_settings(
                i.dep,
                &clone,
                i.settings,
                true,
                i.commit,
                None,
                i.https_option,
                &base_dir,
            );
            let ssh = git_ssh_command(i.dep, i.port, Some("/root/.ssh/id_rsa"), false);
            let prefix = format!("GIT_SSH_COMMAND=\"{ssh}\"");
            let mut commands = Vec::new();
            let (clone, branch) =
                pull_request_clone(i, clone, &prefix, &ssh, i.https_option, &mut commands);
            commands.push(hidden(in_docker(i.dep, &clone)));
            // `gitCommandsAsShellCommand($commands)`: one command joined with `&&`.
            let joined = commands
                .iter()
                .map(|c| c.command.as_str())
                .collect::<Vec<_>>()
                .join(" && ");
            Ok(GitImport {
                commands: vec![hidden(joined)],
                branch,
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
            i.dep,
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
        let ssh = git_ssh_command(i.dep, &port, Some(&key_location), true);
        let base = format!(
            "GIT_SSH_COMMAND=\"{ssh}\" {clone} {} {dir}",
            quote(i.repository)
        );
        let command = set_git_import_settings(
            i.dep,
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
    let clone = set_git_import_settings(
        i.dep, &clone, i.settings, true, i.commit, None, None, base_dir,
    );
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

/// A variable's value for a build: remote secret values stay exactly as they are
/// (`resolve_environment_variable_raw()`), others use `getResolvedValueWithServer()`.
pub(super) fn var_value(v: &crate::databases::env::Var) -> Option<String> {
    if v.remote {
        return v.resolved.clone();
    }
    resolved_with_server(v.resolved.as_deref(), v.is_literal || v.is_multiline)
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

/// What `parseHealthcheckFromDockerfile()` changes on the application.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct HealthcheckUpdate {
    pub reset: bool,
    pub interval: Option<i64>,
    pub timeout: Option<i64>,
    pub start_period: Option<i64>,
    pub retries: Option<i64>,
    pub found: bool,
}

/// `Application::parseHealthcheckFromDockerfile($dockerfile, $isInit)`. `target` is
/// `dockerfile_target_build`: only the HEALTHCHECK of the stage that is built counts.
/// `health_check_disabled` is `isHealthcheckDisabled() || $isInit`.
pub(crate) fn parse_healthcheck(
    dockerfile: &str,
    target: Option<&str>,
    health_check_disabled: bool,
    custom_found: bool,
) -> HealthcheckUpdate {
    use crate::applications::dockerfile_healthcheck as hc;
    let instruction = hc::dockerfile_healthcheck_instruction(dockerfile, target);
    // Always check if healthcheck was removed, regardless of health_check_enabled setting
    let Some(instruction) = instruction else {
        return HealthcheckUpdate {
            reset: custom_found,
            ..Default::default()
        };
    };
    if !health_check_disabled {
        return HealthcheckUpdate::default();
    }
    // An option that is missing or not valid keeps the current value.
    let options = hc::dockerfile_healthcheck_options(&instruction);
    HealthcheckUpdate {
        reset: false,
        interval: options.interval,
        timeout: options.timeout,
        start_period: options.start_period,
        retries: options.retries,
        found: true,
    }
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

    /// `gitHttpsCredentialOrigin()` and `gitHttpsCredentialConfigOption()` (v4.6.0).
    #[test]
    fn https_credential_option() {
        let origin = git_https_credential_origin;
        assert_eq!(
            origin("https://dev.azure.com/org/p/_git/r").as_deref(),
            Some("https://dev.azure.com/")
        );
        assert_eq!(
            origin("HTTP://user:pw@git.example.test:8443/a.git").as_deref(),
            Some("http://git.example.test:8443/")
        );
        assert_eq!(
            origin("https://[::1]:3000/a").as_deref(),
            Some("https://[::1]:3000/")
        );
        assert_eq!(origin("git@git.example.test:o/r.git"), None);
        assert_eq!(origin("ssh://git@git.example.test/o/r.git"), None);
        assert_eq!(git_https_basic_token(Some("u"), "p"), "dTpw");
        assert_eq!(git_https_basic_token(None, "p"), "OnA=");
        let opt = git_https_config_option(Some("https://h.test/r.git"), true, Some("u"), Some("p"));
        assert_eq!(
            opt.as_deref(),
            Some("-c 'http.https://h.test/.extraHeader=Authorization: Basic dTpw'")
        );
        // Only the "other" type, with a password, and an http(s) URL.
        assert_eq!(
            git_https_config_option(Some("https://h.test/r.git"), false, Some("u"), Some("p")),
            None
        );
        assert_eq!(
            git_https_config_option(Some("https://h.test/r.git"), true, Some("u"), Some("")),
            None
        );
        assert_eq!(
            git_https_config_option(Some("git@h.test:r.git"), true, Some("u"), Some("p")),
            None
        );
    }

    /// The "other" branch of `generateGitImportCommands()` with HTTPS credentials (v4.6.0).
    #[test]
    fn https_credential_clone_command() {
        let option = "-c 'http.https://h.test/.extraHeader=Authorization: Basic dTpw'";
        let input = |pull_request_id, git_type| ImportInput {
            dep: "abc",
            branch: "main",
            repository: "https://h.test/r.git",
            port: "22",
            kind: DeploymentType::Other,
            source: None,
            settings: ImportSettings {
                shallow: false,
                submodules: true,
                lfs: false,
            },
            commit: "HEAD",
            private_key: None,
            github_token: None,
            oauth_token: None,
            source_key: None,
            pull_request_id,
            git_type,
            https_option: Some(option),
        };
        let g = generate_git_import_commands(&input(0, None)).unwrap();
        let c = &g.commands[0].command;
        assert!(
            c.contains(r#"git -c '\''http.https://h.test/.extraHeader=Authorization: Basic dTpw'\'' clone --recurse-submodules -b '\''main'\'' -- '\''https://h.test/r.git'\''"#),
            "{c}"
        );
        assert!(
            c.contains(r#"git -c '\''http.https://h.test/.extraHeader=Authorization: Basic dTpw'\'' submodule sync"#),
            "{c}"
        );
        let g = generate_git_import_commands(&input(5, Some("gitlab"))).unwrap();
        let c = &g.commands[0].command;
        assert!(
            c.contains(r#"git -c '\''http.https://h.test/.extraHeader=Authorization: Basic dTpw'\'' fetch origin merge-requests/5/head:pr-5-coolify && git -c '\''http.https://h.test/.extraHeader=Authorization: Basic dTpw'\'' checkout '\''pr-5-coolify'\''"#),
            "{c}"
        );
    }

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
        let u = parse_healthcheck(df, None, true, false);
        assert_eq!(u.interval, Some(30));
        assert_eq!(u.timeout, Some(3));
        assert_eq!(u.start_period, None);
        assert!(u.found);
        // Health checks are enabled: the options of the Dockerfile are not imported.
        assert_eq!(
            parse_healthcheck(df, None, false, false),
            HealthcheckUpdate::default()
        );
        assert_eq!(
            parse_healthcheck("FROM x", None, true, true),
            HealthcheckUpdate {
                reset: true,
                ..Default::default()
            }
        );
        // "ignores a HEALTHCHECK of another stage than the build target" and "resets a custom
        // healthcheck found earlier when the build target has no HEALTHCHECK".
        let multi = "FROM node AS builder\nRUN npm ci\nFROM node AS api\nHEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \\\n    CMD node probe.js\nFROM node AS worker\nCMD [\"node\", \"worker.js\"]";
        assert_eq!(
            parse_healthcheck(multi, Some("worker"), true, false),
            HealthcheckUpdate::default()
        );
        assert!(parse_healthcheck(multi, Some("worker"), true, true).reset);
        // "reads the HEALTHCHECK of the build target stage".
        let api = parse_healthcheck(multi, Some("API"), true, false);
        assert_eq!(
            (api.interval, api.timeout, api.start_period, api.retries),
            (Some(30), Some(5), Some(15), Some(3))
        );
        // "keeps the current value when a HEALTHCHECK duration is not valid".
        let invalid = parse_healthcheck(
            "FROM scratch\nHEALTHCHECK --interval=1x --timeout=3000000h --retries=2 CMD [\"/probe\"]",
            None,
            true,
            false,
        );
        assert_eq!(
            (
                invalid.interval,
                invalid.timeout,
                invalid.retries,
                invalid.found
            ),
            (None, None, Some(2), true)
        );
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
            pull_request_id: 0,
            git_type: None,
            https_option: None,
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
    fn short_commits_and_git_lfs() {
        let s = ImportSettings {
            shallow: true,
            submodules: false,
            lfs: false,
        };
        assert_eq!(
            set_git_import_settings(
                "abc",
                "git clone x '/a'",
                s,
                true,
                "abc1234",
                None,
                None,
                "/a"
            ),
            r#"export GIT_LFS_SKIP_SMUDGE=1 && git clone x '/a' && cd '/a' && echo 'Fetching the commit.' && GIT_SSH_COMMAND="ssh -F /root/.ssh/coolify_git_abc.config -o ConnectTimeout=30 -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -o LogLevel=ERROR" git fetch --depth=1 origin '+refs/heads/*:refs/remotes/origin/*' && (git -c advice.detachedHead=false checkout 'abc1234' >/dev/null 2>&1 || (echo 'The commit is not a branch tip. Fetching the full history.' && GIT_SSH_COMMAND="ssh -F /root/.ssh/coolify_git_abc.config -o ConnectTimeout=30 -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -o LogLevel=ERROR" git fetch --unshallow origin '+refs/heads/*:refs/remotes/origin/*' && git -c advice.detachedHead=false checkout 'abc1234' >/dev/null 2>&1))"#
        );
        // A full SHA and a branch name are fetched by name; HEAD needs no checkout.
        assert!(
            set_git_import_settings("abc", "c", s, true, "v1.2", None, None, "/a")
                .contains("fetch --depth=1 origin 'v1.2' && git -c")
        );
        assert_eq!(
            set_git_import_settings(
                "abc",
                "c",
                ImportSettings { lfs: true, ..s },
                true,
                "HEAD",
                None,
                None,
                "/a"
            ),
            "c && cd '/a' && if git lfs ls-files -n | grep -q .; then echo 'Downloading Git LFS objects.' && GIT_SSH_COMMAND=\"ssh -F /root/.ssh/coolify_git_abc.config -o ConnectTimeout=30 -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -o LogLevel=ERROR\" git lfs pull; fi"
        );
        // A checkout that only reads files skips LFS, submodules and the LFS pull.
        assert_eq!(
            set_git_import_settings_with(
                "abc",
                "c",
                ImportSettings {
                    lfs: true,
                    submodules: true,
                    ..s
                },
                true,
                "HEAD",
                None,
                None,
                "/a",
                true
            ),
            "export GIT_LFS_SKIP_SMUDGE=1 && c"
        );
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
            commit: "0123456789abcdef0123456789abcdef01234567",
            private_key: None,
            github_token: None,
            oauth_token: None,
            source_key: None,
            pull_request_id: 0,
            git_type: None,
            https_option: None,
        })
        .unwrap();
        assert_eq!(g.full_repo_url, "https://github.com/o/r");
        assert_eq!(
            g.commands[0].command,
            r#"docker exec abc bash -c 'git clone --depth=1 --recurse-submodules --shallow-submodules -b '\''main'\'' -- '\''https://github.com/o/r'\'' '\''/artifacts/abc'\'' && cd '\''/artifacts/abc'\'' && echo '\''Fetching the commit.'\'' && GIT_SSH_COMMAND="ssh -F /root/.ssh/coolify_git_abc.config -o ConnectTimeout=30 -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -o LogLevel=ERROR" git fetch --depth=1 origin '\''0123456789abcdef0123456789abcdef01234567'\'' && git -c advice.detachedHead=false checkout '\''0123456789abcdef0123456789abcdef01234567'\'' >/dev/null 2>&1 && cd '\''/artifacts/abc'\'' && if [ -f .gitmodules ]; then sed -i "s#[A-Za-z0-9._-]*@\(.*\):#https://\1/#g" '\''/artifacts/abc'\''/.gitmodules || true && echo '\''Updating submodules.'\'' && git submodule sync && GIT_SSH_COMMAND="ssh -F /root/.ssh/coolify_git_abc.config -o ConnectTimeout=30 -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -o LogLevel=ERROR" git submodule update --init --recursive --depth=1; fi && cd '\''/artifacts/abc'\'' && if git lfs ls-files -n | grep -q .; then echo '\''Downloading Git LFS objects.'\'' && GIT_SSH_COMMAND="ssh -F /root/.ssh/coolify_git_abc.config -o ConnectTimeout=30 -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -o LogLevel=ERROR" git lfs pull; fi'"#
        );
    }

    #[test]
    fn a_repository_git_would_read_as_an_option_is_refused() {
        let s = ImportSettings {
            shallow: true,
            submodules: false,
            lfs: false,
        };
        for kind in [DeploymentType::Other, DeploymentType::DeployKey] {
            for repository in [
                "--upload-pack=touch /tmp/x;",
                "-utouch /tmp/x",
                " --config=x",
            ] {
                let r = generate_git_import_commands(&ImportInput {
                    dep: "abc",
                    branch: "main",
                    repository,
                    port: "22",
                    kind,
                    source: None,
                    settings: s,
                    commit: "HEAD",
                    private_key: Some("key"),
                    github_token: None,
                    oauth_token: None,
                    source_key: None,
                    pull_request_id: 0,
                    git_type: None,
                    https_option: None,
                });
                assert_eq!(r.err().as_deref(), Some(INVALID_REPOSITORY), "{repository}");
            }
        }
        assert!(!is_option_like("https://example.com/-o/r"));
        assert!(!is_option_like("git@example.com:o/r.git"));
    }
}
