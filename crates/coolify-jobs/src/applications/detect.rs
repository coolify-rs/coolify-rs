//! Smart Scan of the New Resource repository forms: `App\Services\RepositoryDetector`,
//! `App\Data\RepositoryDetectionResult` and the clone `Application::serverCheckoutCommand()`
//! builds for an unsaved public application (`HasRepositoryDetection::unsavedApplicationForDetection`:
//! shallow, no submodules, commit `HEAD`). Also the GitHub branch lookup of
//! `PublicGitRepository::getBranch()` (`githubApi()` with the public source) and
//! `parseEnvFormatToArray()` for the env file import.

use std::time::Duration;

use coolify_db::servers;
use coolify_remote::RunOptions;
use coolify_remote::shell::quote;
use serde_json::Value;

use crate::Jobs;
use crate::server::ServerInfo;

/// `RepositoryDetector::MAX_ENV_FILE_BYTES`.
const MAX_ENV_FILE_BYTES: usize = 65536;

/// `RepositoryDetectionResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detection {
    pub dockerfiles: Vec<String>,
    pub docker_compose_files: Vec<String>,
    /// File name and content (`null` content is kept), in scan order.
    pub env_files: Vec<(String, Option<String>)>,
    pub dockerfile_ports: Vec<(String, Option<i64>)>,
}

impl Detection {
    /// `baseDirectoryFile()`: the first file directly in the base directory.
    pub fn base_directory_file(files: &[String]) -> Option<&String> {
        files.iter().find(|f| !f.contains('/'))
    }

    /// `getSuggestedBuildPack()`.
    pub fn suggested_build_pack(&self) -> Option<&'static str> {
        if Self::base_directory_file(&self.docker_compose_files).is_some() {
            return Some("dockercompose");
        }
        if Self::base_directory_file(&self.dockerfiles).is_some() {
            return Some("dockerfile");
        }
        None
    }

    /// `$dockerfilePorts[$file] ?? null`.
    pub fn port_of(&self, file: &str) -> Option<i64> {
        self.dockerfile_ports
            .iter()
            .find(|(f, _)| f == file)
            .and_then(|(_, p)| *p)
    }
}

/// `Application::serverCheckoutCommand($uuid, $checkoutDir)` for a public repository: the
/// clone without a checkout (`--quiet` added like upstream's `str_replace`).
pub fn checkout_command(clone_url: &str, branch: &str, checkout_dir: &str) -> String {
    format!(
        "git clone --quiet --depth=1 --no-checkout -b {} {} {}",
        quote(branch),
        quote(clone_url),
        quote(checkout_dir)
    )
}

/// The awk program of `scanCommands()` that classifies `git ls-tree` lines.
const CLASSIFY_FILES: &str = r#"{ split($1, meta, " "); if (meta[1] != "100644" && meta[1] != "100755") next; path = $2; lower = tolower(path); name = lower; sub(/.*\//, "", name); if (name ~ /^dockerfile(\.[a-z0-9_-]+)?$/) kind = "dockerfile"; else if (name ~ /^(docker-)?compose\.ya?ml$/) kind = "compose"; else if (lower ~ /^\.env\.(example|sample|template|dist|local\.example)$/) kind = "env"; else next; print kind "\t" meta[3] "\t" path }"#;

/// The awk program that reads the first `EXPOSE` port of a Dockerfile.
const EXPOSED_PORT: &str = r#"{ sub(/\r$/, "") } toupper($1) == "EXPOSE" { split($2, p, "/"); if (p[1] ~ /^[0-9]+$/ && p[1] + 0 > 0 && p[1] + 0 <= 65535) print p[1] + 0; exit }"#;

/// The jq program that folds the scan lines into one result.
const SUMMARY: &str = r#"{dockerfiles: map(select(.kind == "dockerfile") | .file), dockerComposeFiles: map(select(.kind == "compose") | .file), envFiles: (map(select(.kind == "env") | {(.file): .content}) | add // {}), dockerfilePorts: (map(select(.kind == "dockerfile") | {(.file): .port}) | add // {})}"#;

/// `RepositoryDetector::scanCommands($uuid)`: one line per command, because a non-root server
/// sends every line through `parseCommandsByLineForSudo()`.
pub fn scan_commands(
    uuid: &str,
    clone_url: &str,
    branch: &str,
    base_directory: &str,
) -> Result<Vec<String>, String> {
    scan_commands_with(uuid, clone_url, branch, base_directory, None)
}

/// `scanCommands()`, with the checkout command of a repository that needs credentials.
fn scan_commands_with(
    uuid: &str,
    clone_url: &str,
    branch: &str,
    base_directory: &str,
    checkout_override: Option<&str>,
) -> Result<Vec<String>, String> {
    let checkout_dir = format!("/tmp/{uuid}/checkout");
    let base = base_directory.trim_matches('/');
    let dots = regex::Regex::new(r"(^|/)\.\.?(/|$)").map_err(|e| e.to_string())?;
    let safe = regex::Regex::new(r"^[a-zA-Z0-9_./-]*$").map_err(|e| e.to_string())?;
    if dots.is_match(base) || !safe.is_match(base) {
        return Err("Invalid repository base directory.".into());
    }
    let tree = quote(&if base.is_empty() {
        "HEAD".to_owned()
    } else {
        format!("HEAD:{base}")
    });
    let scan = format!(
        "tab=$(printf \"\\t\"); git -c core.quotePath=false ls-tree -r {tree} | awk -F '\\t' {classify} \
         | while IFS=\"$tab\" read -r kind object file; do case \"$kind\" in \
         dockerfile) port=$(git cat-file blob \"$object\" | awk {exposed}); \
         jq -nc --arg file \"$file\" --argjson port \"${{port:-null}}\" '{{kind: \"dockerfile\", file: $file, port: $port}}' ;; \
         compose) jq -nc --arg file \"$file\" '{{kind: \"compose\", file: $file}}' ;; \
         env) git cat-file blob \"$object\" | head -c {MAX_ENV_FILE_BYTES} | jq -Rsc --arg file \"$file\" '{{kind: \"env\", file: $file, content: .}}' ;; \
         esac; done | jq -sc {summary}",
        classify = quote(CLASSIFY_FILES),
        exposed = quote(EXPOSED_PORT),
        summary = quote(SUMMARY),
    );
    let checkout = checkout_override.map_or_else(
        || checkout_command(clone_url, branch, &checkout_dir),
        str::to_owned,
    );
    Ok(vec![
        format!("rm -rf /tmp/{uuid}"),
        format!("mkdir -p /tmp/{uuid}"),
        format!("cd /tmp/{uuid}"),
        format!("sh -c {}", quote(&checkout)),
        format!("cd {checkout_dir}"),
        format!("sh -c {}", quote(&scan)),
    ])
}

/// How `detect_private_repository()` reaches a repository.
#[derive(Debug)]
pub enum CheckoutCredentials<'a> {
    Github(&'a coolify_db::records::Record),
    /// A GitLab (`false`) or Gitea (`true`) source.
    Oauth(&'a coolify_db::records::Record, bool),
    DeployKey {
        pem: &'a str,
        port: &'a str,
    },
}

/// `sortByPreference()`: nearest to the base directory first, then the default name.
fn sort_by_preference(mut files: Vec<String>, default_name: &regex::Regex) -> Vec<String> {
    files.sort_by_key(|f| {
        let base = f.rsplit('/').next().unwrap_or(f);
        (f.matches('/').count(), !default_name.is_match(base))
    });
    files
}

/// `RepositoryDetector::parseOutput()` of the last output line.
pub fn parse_output(output: &str) -> Detection {
    let Ok(Value::Object(data)) = serde_json::from_str::<Value>(output.trim()) else {
        return Detection::default();
    };
    // PHP's json_decode(assoc) turns `{}` into an empty array too.
    let list_like = |v: &Value| v.is_array() || v.is_object();
    for k in [
        "dockerfiles",
        "dockerComposeFiles",
        "envFiles",
        "dockerfilePorts",
    ] {
        if data.get(k).is_some_and(|v| !v.is_null() && !list_like(v)) {
            return Detection::default();
        }
    }
    let strings = |k: &str| -> Vec<String> {
        match data.get(k) {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            Some(Value::Object(o)) => o
                .values()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        }
    };
    let dockerfile_name = regex::Regex::new(r"(?i)^dockerfile$").unwrap_or_else(|_| unreachable!());
    let compose_name =
        regex::Regex::new(r"(?i)^docker-compose\.ya?ml$").unwrap_or_else(|_| unreachable!());
    let env_files = match data.get("envFiles") {
        Some(Value::Object(o)) => o
            .iter()
            .filter_map(|(k, v)| match v {
                Value::String(s) => Some((k.clone(), Some(s.clone()))),
                Value::Null => Some((k.clone(), None)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let dockerfile_ports = match data.get("dockerfilePorts") {
        Some(Value::Object(o)) => o
            .iter()
            .map(|(k, v)| {
                let n = match v {
                    Value::Number(n) => n.as_i64(),
                    Value::String(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
                        s.parse().ok()
                    }
                    _ => None,
                };
                (k.clone(), n.filter(|p| (1..=65535).contains(p)))
            })
            .collect(),
        _ => Vec::new(),
    };
    Detection {
        dockerfiles: sort_by_preference(strings("dockerfiles"), &dockerfile_name),
        docker_compose_files: sort_by_preference(strings("dockerComposeFiles"), &compose_name),
        env_files,
        dockerfile_ports,
    }
}

impl Jobs {
    /// `RepositoryDetector::detect()` on the destination server: clones into `/tmp/{uuid}`,
    /// lists the files with `git ls-tree` and always removes the folder afterwards.
    pub async fn detect_repository(
        &self,
        server_id: i64,
        clone_url: &str,
        branch: &str,
        base_directory: &str,
    ) -> Result<Detection, String> {
        self.detect_with_checkout(server_id, clone_url, branch, base_directory, None)
            .await
    }

    /// `RepositoryDetector::detect()` of an unsaved private application
    /// (`unsavedApplicationForDetection()`): a shallow clone without submodules.
    pub async fn detect_private_repository(
        &self,
        server_id: i64,
        credentials: CheckoutCredentials<'_>,
        repository: &str,
        branch: &str,
        base_directory: &str,
    ) -> Result<Detection, String> {
        let uuid = coolify_domain::new_public_id();
        let checkout = self
            .private_checkout(&credentials, repository, branch, &uuid)
            .await?;
        self.detect_with_uuid(
            &uuid,
            server_id,
            repository,
            branch,
            base_directory,
            Some(&checkout),
        )
        .await
    }

    /// `generateGitImportCommands(only_checkout: true, exec_in_docker: false)` with
    /// `str_replace(' clone ', ' clone --quiet ')`.
    async fn private_checkout(
        &self,
        c: &CheckoutCredentials<'_>,
        repository: &str,
        branch: &str,
        uuid: &str,
    ) -> Result<String, String> {
        use base64::Engine as _;
        let db = &self.0.db;
        let enc = self
            .0
            .encrypter
            .as_ref()
            .ok_or("No application encryption key has been specified.")?;
        let checkout_dir = quote(&format!("/tmp/{uuid}/checkout"));
        let clone = format!("git clone --depth=1 --no-checkout -b {}", quote(branch));
        let command = match c {
            CheckoutCredentials::Github(source) => {
                let token = crate::github::installation_token(db, Some(enc), Some(source)).await?;
                let encoded = crate::gitea::raw_url_encode(&token);
                let html = source
                    .get("html_url")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let scheme = html.split_once("://").map_or("https", |(s, _)| s);
                let rest = html.split_once("://").map_or(html, |(_, r)| r);
                let host = rest.split(['/', ':', '?', '#']).next().unwrap_or_default();
                let option = format!(
                    "-c {}",
                    quote(&format!(
                        "url.{scheme}://x-access-token:{encoded}@{host}/.insteadOf={scheme}://{host}/"
                    ))
                );
                let url = format!("{scheme}://x-access-token:{encoded}@{host}/{repository}");
                format!(
                    "{} {} {checkout_dir}",
                    clone.replacen("git clone", &format!("git {option} clone"), 1),
                    quote(&url)
                )
            }
            CheckoutCredentials::Oauth(source, gitea) => {
                let token = if *gitea {
                    crate::gitea::clone_token(db, enc, source).await?
                } else {
                    crate::gitlab::clone_token(db, enc, source).await?
                };
                let encoded = crate::gitea::raw_url_encode(&token);
                let html = source
                    .get("html_url")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let (scheme, host, prefix) = crate::git_http::oauth_url_parts(html);
                let option = format!(
                    "-c {}",
                    quote(&format!(
                        "url.{scheme}://oauth2:{encoded}@{host}{prefix}/.insteadOf={scheme}://{host}{prefix}/"
                    ))
                );
                let url = format!("{scheme}://oauth2:{encoded}@{host}{prefix}/{repository}.git");
                format!(
                    "{} {} {checkout_dir}",
                    clone.replacen("git clone", &format!("git {option} clone"), 1),
                    quote(&url)
                )
            }
            CheckoutCredentials::DeployKey { pem, port } => {
                let location = format!("/root/.ssh/id_rsa_coolify_{uuid}");
                let key_b64 = base64::engine::general_purpose::STANDARD.encode(pem.as_bytes());
                let ssh = format!(
                    "ssh -o ConnectTimeout=30 -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -p {port} -o Port={port} -o LogLevel=ERROR -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -i {location} -o IdentitiesOnly=yes"
                );
                format!(
                    "mkdir -p /root/.ssh && echo '{key_b64}' | base64 -d | tee {location} > /dev/null && chmod 600 {location} && GIT_SSH_COMMAND=\"{ssh}\" {clone} {} {checkout_dir}",
                    quote(repository)
                )
            }
        };
        Ok(command.replace(" clone ", " clone --quiet "))
    }

    async fn detect_with_checkout(
        &self,
        server_id: i64,
        clone_url: &str,
        branch: &str,
        base_directory: &str,
        checkout: Option<&str>,
    ) -> Result<Detection, String> {
        let uuid = coolify_domain::new_public_id();
        self.detect_with_uuid(
            &uuid,
            server_id,
            clone_url,
            branch,
            base_directory,
            checkout,
        )
        .await
    }

    async fn detect_with_uuid(
        &self,
        uuid: &str,
        server_id: i64,
        clone_url: &str,
        branch: &str,
        base_directory: &str,
        checkout: Option<&str>,
    ) -> Result<Detection, String> {
        let commands = scan_commands_with(uuid, clone_url, branch, base_directory, checkout)?;
        let row = servers::find_by_id(&self.0.db, server_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("Server not found.")?;
        let target = ServerInfo::from_row(&row).target(self).await?;
        let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
        let result = self
            .remote()
            .run(
                &target,
                &refs,
                RunOptions::op("application.detect").timeout(Duration::from_secs(60)),
            )
            .await;
        let cleanup = format!("rm -rf /tmp/{uuid}");
        let _ = self
            .remote()
            .run(
                &target,
                &[cleanup.as_str()],
                RunOptions::op("application.detect.cleanup"),
            )
            .await;
        let output = result.map_err(|e| e.message)?.unwrap_or_default();
        let last = output
            .trim()
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .to_owned();
        Ok(parse_output(&last))
    }
}

/// PHP `urlencode()`.
fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The rate limit headers of a successful `githubApi()` call.
#[derive(Debug, Clone, Default)]
pub struct RateLimit {
    pub remaining: Option<String>,
    pub reset: Option<String>,
}

/// `githubApi($source, "/repos/{$repository}/branches/{urlencode($branch)}")` for a public
/// source (no token): the rate limit headers, or upstream's exception message.
pub async fn github_branch(
    api_url: &str,
    repository: &str,
    branch: &str,
) -> Result<RateLimit, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("coolify-rs")
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!(
        "{}/repos/{repository}/branches/{}",
        api_url.trim_end_matches('/'),
        urlencode(branch)
    );
    let response = client
        .get(url)
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let header = |k: &str| {
        response
            .headers()
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let limit = RateLimit {
        remaining: header("X-RateLimit-Remaining"),
        reset: header("X-RateLimit-Reset"),
    };
    let status = response.status();
    if !status.is_success() {
        let body: Value = response.json().await.unwrap_or(Value::Null);
        let message = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no error message found");
        let reset = format_timestamp(limit.reset.as_deref(), "%Y-%m-%d %H:%M:%S");
        let remaining = limit.remaining.as_deref().unwrap_or("0");
        return Err(format!(
            "GitHub API call failed:<br>Error: {message}<br>Rate Limit Status:<br>- Remaining Calls: {remaining}<br>- Reset Time: {reset} UTC"
        ));
    }
    Ok(limit)
}

/// `Carbon::parse((int) $timestamp)->format(...)` in UTC.
pub fn format_timestamp(timestamp: Option<&str>, format: &str) -> String {
    let secs = timestamp
        .and_then(|t| t.trim().parse::<i64>().ok())
        .unwrap_or(0);
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| t.format(format).to_string())
        .unwrap_or_default()
}

/// `parseEnvFormatToArray()` flattened to `KEY => value` (`HasRepositoryDetection::applyDetectionResult`).
pub fn parse_env_format(contents: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let comment = regex::Regex::new(r"\s+#").unwrap_or_else(|_| unreachable!());
    for line in contents.split('\n') {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(eq) = line.find('=') else {
            continue;
        };
        let key = &line[..eq];
        let rest = &line[eq + 1..];
        let value = match rest.chars().next() {
            Some(q @ ('"' | '\'')) => match rest[1..].find(q) {
                Some(close) => rest[1..1 + close].to_owned(),
                None => rest[1..].to_owned(),
            },
            _ => match comment.find(rest) {
                Some(m) => rest[..m.start()].trim_end().to_owned(),
                None => rest.to_owned(),
            },
        };
        // PHP array assignment: a repeated key keeps its first position.
        match out.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = value,
            None => out.push((key.to_owned(), value)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scan_output() {
        let out = r#"{"dockerfiles":["api/Dockerfile","Dockerfile.dev","Dockerfile"],"dockerComposeFiles":[],"envFiles":{".env.example":"A=1\n"},"dockerfilePorts":{"Dockerfile":3000,"api/Dockerfile":null,"Dockerfile.dev":"99999"}}"#;
        let d = parse_output(out);
        assert_eq!(
            d.dockerfiles,
            ["Dockerfile", "Dockerfile.dev", "api/Dockerfile"]
        );
        assert_eq!(d.port_of("Dockerfile"), Some(3000));
        assert_eq!(d.port_of("Dockerfile.dev"), None);
        assert_eq!(d.suggested_build_pack(), Some("dockerfile"));
        assert_eq!(parse_output("nope"), Detection::default());
        let empty = parse_output(
            r#"{"dockerfiles":[],"dockerComposeFiles":[],"envFiles":{},"dockerfilePorts":{}}"#,
        );
        assert_eq!(empty.suggested_build_pack(), None);
    }

    #[test]
    fn env_format() {
        let vars = parse_env_format("# c\nA=1 # note\nB=\"x # y\" # z\nC='q\nD=plain#x\n\nA=2");
        assert_eq!(
            vars,
            [
                ("A".to_owned(), "2".to_owned()),
                ("B".to_owned(), "x # y".to_owned()),
                ("C".to_owned(), "q".to_owned()),
                ("D".to_owned(), "plain#x".to_owned()),
            ]
        );
    }

    #[test]
    fn commands() {
        assert!(scan_commands("u", "https://github.com/a/b", "main", "/../x").is_err());
        let c = scan_commands("u", "https://github.com/a/b", "main", "/static").unwrap_or_default();
        assert_eq!(
            c[3],
            "sh -c 'git clone --quiet --depth=1 --no-checkout -b '\\''main'\\'' '\\''https://github.com/a/b'\\'' '\\''/tmp/u/checkout'\\'''"
        );
        assert!(c[5].contains("'\\''HEAD:static'\\''"));
        assert_eq!(urlencode("feat/a b"), "feat%2Fa+b");
    }
}
