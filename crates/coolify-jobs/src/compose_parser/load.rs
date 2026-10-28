//! `Application::loadComposeFile($isInit)` (app/Models/Application.php): `getGitRemoteStatus()`,
//! `composeFileReadCommands()` (a clone without checkout on the destination server, the file read
//! from the branch tip's objects), `validateDockerComposeForInjection()`, `parse()` and
//! `reconcileDockerComposeDomains()`.
//!
//! Sources: public GitHub Apps, private GitHub Apps (installation token), deploy keys and other
//! public repositories. GitLab and Gitea sources are not ported.

use std::sync::LazyLock;

use chrono::NaiveDateTime;
use coolify_compat::yaml::PhpValue;
use coolify_db::records::Record;
use coolify_remote::RunOptions;
use serde_json::Value;

use super::application::{self, Run};
use super::domains;
use super::php::{self, escapeshellarg};
use super::validate;
use crate::Jobs;

/// `Application::MAX_DOCKER_COMPOSE_SIZE_BYTES`.
const MAX_COMPOSE_BYTES: usize = 5 * 1024 * 1024;
const GITHUB_APP: &str = r"App\Models\GithubApp";

/// What `loadComposeFile()` throws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// `RuntimeException` (and other exceptions): the message as upstream shows it.
    Runtime(String),
    /// `DeploymentException`: the message is HTML-escaped by upstream already.
    Deployment(String),
}

impl LoadError {
    pub fn message(&self) -> &str {
        match self {
            LoadError::Runtime(m) | LoadError::Deployment(m) => m,
        }
    }
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for LoadError {}

fn rt(m: impl Into<String>) -> LoadError {
    LoadError::Runtime(m.into())
}

/// `Application::deploymentType()` and the source it reads.
enum Kind {
    DeployKey,
    GithubApp(Record),
    Other,
}

/// `gitSshCommand($port, $identityFile)`.
fn git_ssh_command(port: &str, identity: Option<&str>) -> String {
    let mut c = format!(
        "ssh -o ConnectTimeout=30 -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -p {port} -o Port={port} -o LogLevel=ERROR -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null"
    );
    if let Some(i) = identity {
        c.push_str(&format!(" -i {i} -o IdentitiesOnly=yes"));
    }
    c
}

static SSH_MATCH: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"((.*?)://)?(.*@.*:.*)").unwrap_or_else(|_| unreachable!())
});
static SCP: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^(?P<user>[A-Za-z0-9._-]+)@(?P<host>\[[0-9A-Fa-f:.]+\]|[^:\[\]]+):(?:(?P<port>\d+)/)?(?P<path>.+)$")
        .unwrap_or_else(|_| unreachable!())
});

/// `convertGitUrl($gitRepository, $deploymentType, $source)`: `(repository, port)`.
fn convert_git_url(repo: &str, kind: &Kind, source: Option<&Record>) -> (String, String) {
    let mut repository = repo.to_owned();
    let mut port = "22".to_owned();
    if matches!(kind, Kind::DeployKey)
        && !SSH_MATCH.is_match(repo)
        && let Some(src) = source
    {
        let host = src
            .get("html_url")
            .and_then(Value::as_str)
            .and_then(|u| php::SpatieUrl::from_string(u).ok())
            .map(|u| u.host)
            .unwrap_or_default();
        let custom_port = src
            .get("custom_port")
            .map(|v| v.to_string().trim_matches('"').to_owned())
            .unwrap_or_default();
        let user = src
            .get("custom_user")
            .and_then(Value::as_str)
            .unwrap_or_default();
        port = custom_port.clone();
        if !host.is_empty() {
            repository = if src.get("custom_port").and_then(Value::as_i64) == Some(22) {
                format!("{user}@{host}:{repo}")
            } else {
                format!("ssh://{user}@{host}:{custom_port}/{repo}")
            };
        }
    }
    if repository.contains("://") {
        if let Some(p) = php::parse_url(&repository).and_then(|u| u.port) {
            port = p.to_string();
        }
    } else if let Some(c) = SCP.captures(&repository) {
        let host = c["host"].trim().to_owned();
        let path = c["path"].trim_start_matches('/').to_owned();
        if let Some(p) = c
            .name("port")
            .filter(|_| !host.is_empty() && !path.is_empty())
        {
            port = p.as_str().to_owned();
            repository = format!("{}@{host}:{path}", &c["user"]);
        }
    }
    (repository, port)
}

/// The commands of `generateGitLsRemoteCommands(exec_in_docker: false)` joined like
/// `gitCommandsAsShellCommand()`, and of `serverCheckout()`: `(ls_remote, clone, git)`.
struct GitCommands {
    ls_remote: String,
    clone: String,
    git: String,
}

fn ssh_key_setup(uuid: &str, key_b64: &str) -> Vec<String> {
    let loc = format!("/root/.ssh/id_rsa_coolify_{uuid}");
    vec![
        format!("trap 'rm -f {loc}' EXIT"),
        "mkdir -p /root/.ssh".into(),
        format!("echo '{key_b64}' | base64 -d | tee {loc} > /dev/null"),
        format!("chmod 600 {loc}"),
    ]
}

async fn git_commands(
    jobs: &Jobs,
    run: &Run<'_>,
    kind: &Kind,
    source: Option<&Record>,
    uuid: &str,
    checkout_dir: &str,
) -> Result<GitCommands, LoadError> {
    let app = run.app();
    let branch = app.str("git_branch").unwrap_or_default().to_owned();
    let (repository, port) =
        convert_git_url(app.str("git_repository").unwrap_or_default(), kind, source);
    let shallow = app.setting_bool("is_git_shallow_clone_enabled");
    let depth = if shallow { " --depth=1" } else { "" };
    let mut submodule_flags = String::new();
    if app.setting_bool("is_git_submodules_enabled") {
        submodule_flags.push_str(" --recurse-submodules");
        if shallow {
            submodule_flags.push_str(" --shallow-submodules");
        }
    }
    let mut clone = format!(
        "git clone{depth}{submodule_flags} --no-checkout -b {}",
        escapeshellarg(&branch)
    );
    let base = escapeshellarg(checkout_dir);
    let key_loc = format!("/root/.ssh/id_rsa_coolify_{uuid}");
    let out = match kind {
        Kind::GithubApp(src) => {
            let html_url = src
                .get("html_url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if src
                .get("is_public")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                let url = format!("{html_url}/{repository}");
                GitCommands {
                    ls_remote: format!("git ls-remote {}", escapeshellarg(&url)),
                    clone: format!("{clone} {} {base}", escapeshellarg(&url)),
                    git: "git".into(),
                }
            } else {
                let token = crate::github::installation_token(
                    &jobs.0.db,
                    jobs.0.encrypter.as_ref(),
                    Some(src),
                )
                .await
                .map_err(rt)?;
                let encoded = rawurlencode(&token);
                let parsed = php::parse_url(&html_url).unwrap_or_default();
                let (scheme, host) = (
                    parsed.scheme.unwrap_or_default(),
                    parsed.host.unwrap_or_default(),
                );
                let repo_url = format!("{scheme}://x-access-token:{encoded}@{host}/{repository}");
                let option = format!(
                    "-c {}",
                    escapeshellarg(&format!(
                        "url.{scheme}://x-access-token:{encoded}@{host}/.insteadOf={scheme}://{host}/"
                    ))
                );
                clone = clone.replacen("git clone", &format!("git {option} clone"), usize::MAX);
                GitCommands {
                    ls_remote: format!("git ls-remote {}", escapeshellarg(&repo_url)),
                    clone: format!("{clone} {} {base}", escapeshellarg(&repo_url)),
                    git: format!("git {option}"),
                }
            }
        }
        Kind::DeployKey => {
            let key_id = app.int("private_key_id").unwrap_or(0);
            let cipher = coolify_db::git_sources::private_key_ciphertext(&jobs.0.db, key_id)
                .await
                .map_err(|e| rt(e.to_string()))?
                .ok_or_else(|| rt("Private key not found. Please add a private key to the application and try again."))?;
            let pem = run
                .encrypter()
                .decrypt_string(&cipher)
                .or_else(|_| run.encrypter().decrypt_maybe_serialized(&cipher))
                .map_err(|_| rt("The MAC is invalid."))?;
            use base64::Engine as _;
            let key_b64 = base64::engine::general_purpose::STANDARD.encode(pem.as_bytes());
            let ssh = git_ssh_command(&port, Some(&key_loc));
            let env = format!("GIT_SSH_COMMAND=\"{ssh}\"");
            let mut ls = ssh_key_setup(uuid, &key_b64);
            ls.push(format!(
                "{env} git ls-remote '{}'",
                repository.replace('\'', "'\\''")
            ));
            let mut cl = ssh_key_setup(uuid, &key_b64);
            cl.push(format!(
                "{env} {clone} {} {base}",
                escapeshellarg(&repository)
            ));
            GitCommands {
                ls_remote: ls.join(" && "),
                clone: cl.join(" && "),
                git: format!("{env} git"),
            }
        }
        Kind::Other => {
            let mut c = format!("{clone} {} {base}", escapeshellarg(&repository));
            // setGitImportSettings(public: true, onlyCheckout: true): only the commit checkout.
            let commit = app.str("git_commit_sha").unwrap_or("HEAD").to_owned();
            if commit != "HEAD" {
                let ssh = "GIT_SSH_COMMAND=\"ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null\"";
                let esc = escapeshellarg(&commit);
                if shallow {
                    c = format!(
                        "{c} && cd {base} && echo 'Fetching the commit.' && {ssh} git fetch --depth=1 origin {esc} && git -c advice.detachedHead=false checkout {esc} >/dev/null 2>&1"
                    );
                } else {
                    c = format!(
                        "{c} && cd {base} && {ssh} git -c advice.detachedHead=false checkout {esc} >/dev/null 2>&1"
                    );
                }
            }
            GitCommands {
                ls_remote: format!("git ls-remote {}", escapeshellarg(&repository)),
                clone: c,
                git: "git".into(),
            }
        }
    };
    Ok(GitCommands {
        clone: out.clone.replace(" clone ", " clone --quiet "),
        ..out
    })
}

/// `rawurlencode()`.
fn rawurlencode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// `composeFileReadCommands($uuid)`.
fn compose_file_read_commands(run: &Run<'_>, uuid: &str, git: &GitCommands) -> Vec<String> {
    let app = run.app();
    let checkout_dir = format!("/tmp/{uuid}/checkout");
    let base = app.str("base_directory").unwrap_or("/");
    let location = app
        .str("docker_compose_location")
        .unwrap_or("/docker-compose.yaml");
    let compose_file = format!("{}{location}", base.trim_end_matches('/'))
        .trim_start_matches('/')
        .to_owned();
    let mut submodule_commands: Vec<String> = Vec::new();
    if app.setting_bool("is_git_submodules_enabled") {
        let depth = if app.setting_bool("is_git_shallow_clone_enabled") {
            " --depth=1"
        } else {
            ""
        };
        let escaped = escapeshellarg(&compose_file);
        submodule_commands.push("git read-tree HEAD".into());
        submodule_commands.push(format!(
            "sh -c {}",
            escapeshellarg(&format!(
                "git ls-files -s | while read -r mode object stage path; do if [ \"$mode\" = 160000 ]; then case {escaped} in \"$path\"/*) {} submodule update --init --recursive{depth} -- \"$path\" >/dev/null || exit 1 ;; esac; fi; done",
                git.git
            ))
        ));
    }
    let enter = if submodule_commands.is_empty() {
        String::new()
    } else {
        [
            "while sub=$(git ls-tree -r HEAD | while read -r mode type object path; do",
            "if [ \"$mode\" = 160000 ]; then case \"$file\" in \"$path\"/*) echo \"$path\"; break ;; esac; fi; done) && [ -n \"$sub\" ];",
            "do cd \"$sub\" || exit 1; file=\"${file#\"$sub\"/}\"; done;",
        ]
        .join(" ")
    };
    let read = [
        format!("file={};", escapeshellarg(&compose_file)),
        enter,
        "git ls-tree HEAD -- \"$file\" | {".into(),
        "read -r mode type object path;".into(),
        "case \"$mode\" in".into(),
        "100644|100755) ;;".into(),
        "120000) echo 'The Docker Compose file is a symbolic link.' >&2; exit 1 ;;".into(),
        "*) echo 'No such file' >&2; exit 1 ;;".into(),
        "esac;".into(),
        format!("if [ \"$(git cat-file -s \"$object\")\" -gt {MAX_COMPOSE_BYTES} ]; then echo '__COOLIFY_COMPOSE_TOO_LARGE__';"),
        "else git cat-file blob \"$object\"; fi;".into(),
        "}".into(),
    ]
    .join(" ");
    let mut cmds = vec![
        format!("rm -rf /tmp/{uuid}"),
        format!("mkdir -p /tmp/{uuid}"),
        format!("cd /tmp/{uuid}"),
        format!("sh -c {}", escapeshellarg(&git.clone)),
        format!("cd {checkout_dir}"),
    ];
    cmds.extend(submodule_commands);
    cmds.push(format!("sh -c {}", escapeshellarg(&read)));
    cmds
}

static CREDENTIALS: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"(?i)([a-z][a-z0-9+.-]*://)[^/\s@'"]+@"#).unwrap_or_else(|_| unreachable!())
});

/// `gitFailureReason($summary, $errorOutput)`.
fn git_failure_reason(summary: &str, error: &str, app_uuid: &str) -> String {
    let masked = CREDENTIALS.replace_all(error.trim(), "$1***@").into_owned();
    tracing::warn!(application_uuid = %app_uuid, error = %masked, "{summary}");
    let reason = masked
        .split('\n')
        .map(|l| l.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B']))
        .rfind(|l| php::truthy(&PhpValue::Str((*l).to_owned())));
    match reason {
        Some(r) => {
            let limited = if r.chars().count() > 300 {
                format!("{}...", r.chars().take(300).collect::<String>().trim_end())
            } else {
                r.to_owned()
            };
            format!("<br><br>Reason: {}", php::html_escape(&limited))
        }
        None => String::new(),
    }
}

/// The remote commands of one `loadComposeFile()` run and what interpreting their output needs.
#[derive(Debug, Clone)]
pub struct LoadPlan {
    /// `getGitRemoteStatus()`: one command.
    pub ls_remote: String,
    /// `composeFileReadCommands($uuid)`: one `instant_remote_process` call.
    pub read: Vec<String>,
    /// `rm -rf /tmp/{uuid}` (the `finally` block).
    pub cleanup: String,
    is_deploy_key: bool,
}

/// The commands `loadComposeFile()` runs on the destination server, with the checkout folder
/// `/tmp/{uuid}`.
pub async fn plan(jobs: &Jobs, run: &Run<'_>, uuid: &str) -> Result<LoadPlan, LoadError> {
    let app = run.app();
    let source_type = app.str("source_type").unwrap_or_default().to_owned();
    let source_id = app.int("source_id");
    let private_key_id = app.int("private_key_id");
    let source = match source_id {
        Some(id) if source_type == GITHUB_APP => {
            coolify_db::git_sources::github_app(&jobs.0.db, id)
                .await
                .map_err(|e| rt(e.to_string()))?
        }
        Some(_) if !source_type.is_empty() => {
            return Err(rt(
                "Loading a Docker Compose file from GitLab or Gitea sources is not ported.",
            ));
        }
        _ => None,
    };
    let kind = if private_key_id.is_some_and(|k| k > 0) {
        Kind::DeployKey
    } else if let Some(s) = source.clone() {
        Kind::GithubApp(s)
    } else if private_key_id == Some(0) {
        Kind::DeployKey
    } else {
        Kind::Other
    };
    let git = git_commands(
        jobs,
        run,
        &kind,
        source.as_ref(),
        uuid,
        &format!("/tmp/{uuid}/checkout"),
    )
    .await?;
    Ok(LoadPlan {
        read: compose_file_read_commands(run, uuid, &git),
        ls_remote: git.ls_remote,
        cleanup: format!("rm -rf /tmp/{uuid}"),
        is_deploy_key: matches!(kind, Kind::DeployKey),
    })
}

/// The rest of `loadComposeFile()` once the commands ran: `ls_remote` is the error message of
/// `getGitRemoteStatus()`, `read` the trimmed output (or the error message) of the read commands.
pub async fn apply(
    run: &mut Run<'_>,
    plan: &LoadPlan,
    ls_remote: Result<(), String>,
    read: Option<Result<Option<String>, String>>,
) -> Result<(), LoadError> {
    let app = run.app().clone();
    let branch = app.str("git_branch").unwrap_or_default().to_owned();
    let workdir = app
        .str("base_directory")
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_owned();
    let compose_location = app
        .str("docker_compose_location")
        .unwrap_or_default()
        .to_owned();
    if let Err(e) = ls_remote {
        return Err(rt(format!(
            "Failed to read Git source. Please verify repository access and try again.{}",
            git_failure_reason("Failed to read Git source.", &e, &app.uuid)
        )));
    }
    let content = match read.unwrap_or(Ok(None)) {
        Ok(out) => {
            let out = out.unwrap_or_default();
            if out == "__COOLIFY_COMPOSE_TOO_LARGE__" || out.len() > MAX_COMPOSE_BYTES {
                return Err(rt("Docker Compose file exceeds the 5 MiB size limit."));
            }
            out
        }
        Err(m) => {
            if m.contains("The Docker Compose file is a symbolic link.") {
                return Err(rt(format!(
                    "Docker Compose file at {workdir}{compose_location} (branch: {branch}) is a symbolic link. Coolify reads only regular files from the repository. Commit the file itself instead of a link to it."
                )));
            }
            if m.contains("No such file") {
                return Err(rt(format!(
                    "Docker Compose file not found at: {workdir}{compose_location} (branch: {branch})<br><br>Check if you used the right extension (.yaml or .yml) in the compose file name."
                )));
            }
            if m.contains("fatal: repository") && m.contains("does not exist") {
                if plan.is_deploy_key {
                    return Err(rt(
                        "Your deploy key does not have access to the repository. Please check your deploy key and try again.",
                    ));
                }
                return Err(rt(
                    "Repository does not exist. Please check your repository URL and try again.",
                ));
            }
            return Err(rt(format!(
                "Failed to read the Docker Compose file from the repository.{}",
                git_failure_reason(
                    "Failed to read the Docker Compose file from the repository.",
                    &m,
                    &app.uuid
                )
            )));
        }
    };
    if !php::truthy(&PhpValue::Str(content.clone())) {
        return Err(rt(format!(
            "Docker Compose file not found at: {workdir}{compose_location} (branch: {branch})<br><br>Check if you used the right extension (.yaml or .yml) in the compose file name."
        )));
    }
    if let Err(e) = validate::validate_docker_compose_for_injection(&content) {
        return Err(LoadError::Deployment(php::html_escape(&format!(
            "The Docker Compose file at {workdir}{compose_location} (branch: {branch}) is not safe to use, so Coolify did not load it. {e}"
        ))));
    }
    run.set_docker_compose_raw(Some(content));
    run.save().await.map_err(rt)?;
    if run.parsing_version() < 3 {
        return Err(rt(
            "Compose parsing versions before 3 (parseDockerComposeFile) are not ported.",
        ));
    }
    let parsed = application::application_parser(run, 0, None)
        .await
        .map_err(rt)?;
    if run
        .docker_compose_domains()
        .is_some_and(|d| php::truthy(&PhpValue::Str(d.to_owned())))
    {
        reconcile_docker_compose_domains(run, &parsed)
            .await
            .map_err(rt)?;
    }
    Ok(())
}

/// `Application::loadComposeFile($isInit)`.
pub async fn load_compose_file(
    jobs: &Jobs,
    app_id: i64,
    is_init: bool,
    now: NaiveDateTime,
) -> Result<(), LoadError> {
    let mut run = Run::load(jobs, app_id, now).await.map_err(rt)?;
    if is_init
        && run
            .docker_compose_raw()
            .is_some_and(|r| php::truthy(&PhpValue::Str(r.to_owned())))
    {
        return Ok(());
    }
    let uuid = coolify_domain::new_public_id();
    let plan = plan(jobs, &run, &uuid).await?;
    let server_id = run
        .app()
        .server_id()
        .ok_or_else(|| rt("The application has no destination server."))?;
    let row = coolify_db::servers::find_by_id(&jobs.0.db, server_id)
        .await
        .map_err(|e| rt(e.to_string()))?
        .ok_or_else(|| rt("Server not found."))?;
    let target = crate::server::ServerInfo::from_row(&row)
        .target(jobs)
        .await
        .map_err(rt)?;
    let ls_remote = jobs
        .remote()
        .run(
            &target,
            &[plan.ls_remote.as_str()],
            RunOptions::op("application.git_ls_remote"),
        )
        .await
        .map(|_| ())
        .map_err(|e| e.message);
    let read = if ls_remote.is_ok() {
        let refs: Vec<&str> = plan.read.iter().map(String::as_str).collect();
        let result = jobs
            .remote()
            .run(
                &target,
                &refs,
                RunOptions::op("application.load_compose_file"),
            )
            .await
            .map_err(|e| e.message);
        let _ = jobs
            .remote()
            .run_quiet(
                &target,
                &[plan.cleanup.as_str()],
                RunOptions::op("application.load_compose_file"),
            )
            .await;
        Some(result)
    } else {
        None
    };
    apply(&mut run, &plan, ls_remote, read).await
}

/// `reconcileDockerComposeDomains($parsedServices)`.
pub(crate) async fn reconcile_docker_compose_domains(
    run: &mut Run<'_>,
    parsed: &super::ParsedCompose,
) -> Result<(), String> {
    let names: Vec<String> = parsed
        .services()
        .iter()
        .map(|(k, _)| k.as_string())
        .collect();
    if names.is_empty() {
        return Ok(());
    }
    let decoded = match run.docker_compose_domains().and_then(php::json_decode) {
        Some(PhpValue::Array(items)) => items,
        _ => Vec::new(),
    };
    let rekeyed = domains::rekey_compose_domains_to_service_names(&decoded, &names);
    let kept: Vec<_> = rekeyed
        .into_iter()
        .filter(|(k, _)| domains::find_compose_service_name(&k.as_string(), &names).is_some())
        .collect();
    run.set_docker_compose_domains(
        (!kept.is_empty()).then(|| php::json_encode(&PhpValue::Array(kept))),
    );
    run.save().await
}
