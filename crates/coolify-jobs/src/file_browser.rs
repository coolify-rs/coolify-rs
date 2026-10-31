//! `App\Services\ContainerFilesystemService`: the commands of the file browser on the server.

use std::path::Path;

use coolify_remote::exec::RemoteRead;
use coolify_remote::shell::quote;
use coolify_remote::{RemoteError, RunOptions, Target};

use crate::Jobs;
use crate::compose_parser::validate::validate_shell_safe_path;
use crate::databases::common::helper_image;

/// `LocalFileVolume::MAX_CONTENT_SIZE`.
const MAX_CONTENT_SIZE: usize = 5_242_880;
/// `MAX_INLINE_WRITE_SIZE`.
const MAX_INLINE_WRITE_SIZE: usize = 64 * 1024;
/// `MAX_DOWNLOAD_SIZE`.
pub const MAX_DOWNLOAD_SIZE: u64 = 1024 * 1024 * 1024;

/// Linux capability names by bit number, as Docker expects them in `--cap-add`.
const CAPABILITIES: [&str; 41] = [
    "CHOWN",
    "DAC_OVERRIDE",
    "DAC_READ_SEARCH",
    "FOWNER",
    "FSETID",
    "KILL",
    "SETGID",
    "SETUID",
    "SETPCAP",
    "LINUX_IMMUTABLE",
    "NET_BIND_SERVICE",
    "NET_BROADCAST",
    "NET_ADMIN",
    "NET_RAW",
    "IPC_LOCK",
    "IPC_OWNER",
    "SYS_MODULE",
    "SYS_RAWIO",
    "SYS_CHROOT",
    "SYS_PTRACE",
    "SYS_PACCT",
    "SYS_ADMIN",
    "SYS_BOOT",
    "SYS_NICE",
    "SYS_RESOURCE",
    "SYS_TIME",
    "SYS_TTY_CONFIG",
    "MKNOD",
    "LEASE",
    "AUDIT_WRITE",
    "AUDIT_CONTROL",
    "SETFCAP",
    "MAC_OVERRIDE",
    "MAC_ADMIN",
    "SYSLOG",
    "WAKE_ALARM",
    "BLOCK_SUSPEND",
    "AUDIT_READ",
    "PERFMON",
    "BPF",
    "CHECKPOINT_RESTORE",
];

/// `App\Data\FileEntry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub kind: String,
    pub size: i64,
    pub mtime: i64,
    pub perms: String,
    pub owner: String,
    pub group: String,
}

/// What a failure means to the page.
#[derive(Debug)]
pub enum ListError {
    /// `ContainerShellUnavailableException`.
    ShellUnavailable(String),
}

/// `parseListing`.
pub fn parse_listing(raw: &str) -> Vec<FileEntry> {
    let mut entries = Vec::new();
    for line in raw.trim().split('\n') {
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(7, '\t').collect();
        let (kind, size, mtime, perms, owner, group, name) = match parts.as_slice() {
            [t, s, m, p, o, g, n] => (*t, *s, *m, *p, *o, *g, *n),
            [t, s, m, p, n] => (*t, *s, *m, *p, "", "", *n),
            [t, s, m, n] => (*t, *s, *m, "", "", "", *n),
            _ => continue,
        };
        let name = name.strip_prefix("./").unwrap_or(name);
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        let t = kind.trim().to_lowercase();
        let kind = if t == "dir" || t.contains("directory") {
            "dir"
        } else if t == "symlink" || t.contains("symbolic link") {
            "symlink"
        } else {
            "file"
        };
        entries.push(FileEntry {
            name: name.to_owned(),
            kind: kind.to_owned(),
            size: size.parse().unwrap_or(0),
            mtime: mtime.parse().unwrap_or(0),
            perms: perms.trim().to_owned(),
            owner: owner.trim().to_owned(),
            group: group.trim().to_owned(),
        });
    }
    // `FileEntry::sort`: directories first, then `strcasecmp`.
    entries.sort_by(|a, b| {
        (a.kind != "dir")
            .cmp(&(b.kind != "dir"))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries
}

/// `parseProcessStatus`: `(user, groups, capabilities)`.
pub fn parse_process_status(status: &str) -> Result<(String, Vec<String>, Vec<String>), String> {
    let field = |key: &str| -> Vec<String> {
        status
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                (k.trim() == key).then(|| v.split_whitespace().map(str::to_owned).collect())
            })
            .unwrap_or_default()
    };
    let uid = field("Uid").get(1).cloned().unwrap_or_default();
    let gid = field("Gid").get(1).cloned().unwrap_or_default();
    let mask = field("CapPrm").first().cloned().unwrap_or_default();
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    if !digits(&uid)
        || !digits(&gid)
        || mask.is_empty()
        || !mask.chars().all(|c| c.is_ascii_hexdigit())
        || mask.len() > 16
    {
        return Err("Could not read the user of the container process.".to_owned());
    }
    let groups: Vec<String> = field("Groups")
        .into_iter()
        .filter(|g| digits(g) && *g != gid)
        .collect();
    let rev: Vec<char> = mask.chars().rev().collect();
    let mut capabilities = Vec::new();
    for (bit, name) in CAPABILITIES.iter().enumerate() {
        let digit = rev
            .get(bit / 4)
            .and_then(|c| c.to_digit(16))
            .unwrap_or_default();
        if digit & (1 << (bit % 4)) != 0 {
            capabilities.push((*name).to_owned());
        }
    }
    Ok((format!("{uid}:{gid}"), groups, capabilities))
}

/// `coolifyHelperImage().':'.getHelperVersion()`.
pub fn helper_image_name(jobs: &Jobs) -> String {
    helper_image(jobs)
}

/// `ValidationPatterns::isValidContainerName`.
pub fn is_valid_container_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `ContainerFilesystemService` for one container.
#[allow(missing_debug_implementations)]
pub struct ContainerFiles<'a> {
    pub jobs: &'a Jobs,
    pub target: &'a Target,
    pub container: String,
    pub use_helper: bool,
    helper_identity: Option<(String, Vec<String>, Vec<String>)>,
}

impl<'a> ContainerFiles<'a> {
    pub fn new(jobs: &'a Jobs, target: &'a Target, container: &str, use_helper: bool) -> Self {
        Self {
            jobs,
            target,
            container: container.to_owned(),
            use_helper,
            helper_identity: None,
        }
    }

    async fn run(&self, command: &str) -> Result<String, RemoteError> {
        self.jobs
            .remote()
            .run(self.target, &[command], RunOptions::op("files.command"))
            .await
            .map(Option::unwrap_or_default)
    }

    fn escape_path(&self, path: &str, label: &str) -> Result<String, String> {
        validate_shell_safe_path(path, label)?;
        let prefix = if self.use_helper { "/proc/1/root" } else { "" };
        Ok(quote(&format!("{prefix}{path}")))
    }

    /// `helperIdentity()`.
    async fn identity(&mut self) -> Result<(String, Vec<String>, Vec<String>), RemoteError> {
        if let Some(i) = &self.helper_identity {
            return Ok(i.clone());
        }
        let escaped = quote(&self.container);
        let status = self
            .run(&format!(
                "cat /proc/$(docker inspect --format '{{{{.State.Pid}}}}' {escaped})/status"
            ))
            .await?;
        let identity = parse_process_status(&status).map_err(|message| RemoteError {
            message,
            code: None,
        })?;
        self.helper_identity = Some(identity.clone());
        Ok(identity)
    }

    /// `dockerExecShell()`.
    async fn exec(&mut self, inner: &str) -> Result<String, RemoteError> {
        let escaped = quote(&self.container);
        if !self.use_helper {
            return Ok(format!("docker exec {escaped} sh -c {}", quote(inner)));
        }
        let (user, groups, capabilities) = self.identity().await?;
        let mut options = vec![
            "--rm".to_owned(),
            format!("--pid container:{escaped}"),
            format!("--user {}", quote(&user)),
        ];
        options.extend(groups.iter().map(|g| format!("--group-add {}", quote(g))));
        options.push("--cap-drop ALL".to_owned());
        options.extend(capabilities.iter().map(|c| format!("--cap-add {c}")));
        options.extend(
            [
                "--security-opt no-new-privileges",
                "--network none",
                "--ipc none",
                "--read-only",
                "--log-driver none",
                "--pids-limit 64",
                "--label coolify.file-browser=true",
                "--entrypoint ''",
            ]
            .map(str::to_owned),
        );
        let image = quote(&helper_image(self.jobs));
        let script = format!("[ -x /proc/1/root/ ] || exit 126; {inner}");
        Ok(format!(
            "docker run {} {image} sh -c {}",
            options.join(" "),
            quote(&script)
        ))
    }

    async fn exec_path(
        &mut self,
        make: impl FnOnce(&str) -> String,
        path: &str,
        label: &str,
    ) -> Result<(), String> {
        let escaped = self.escape_path(path, label)?;
        let command = self
            .exec(&make(&escaped))
            .await
            .map_err(|e| e.message.clone())?;
        self.run(&command).await.map_err(|e| e.message)?;
        Ok(())
    }

    /// `list()`.
    pub async fn list(&mut self, path: &str) -> Result<Vec<FileEntry>, ListError> {
        let escaped = self
            .escape_path(path, "list path")
            .map_err(ListError::ShellUnavailable)?;
        let format = "%F\t%s\t%Y\t%a\t%U\t%G\t%n";
        let inner = format!(
            "command -v find >/dev/null && command -v stat >/dev/null || exit 127; cd {escaped} 2>/dev/null || exit 0; find . ! -name . -prune -exec stat -c \"{format}\" -- {{}} + 2>/dev/null; exit 0"
        );
        let command = self
            .exec(&inner)
            .await
            .map_err(|e| ListError::ShellUnavailable(e.message))?;
        match self.run(&command).await {
            Ok(raw) => Ok(parse_listing(&raw)),
            Err(e) if self.use_helper || matches!(e.code, Some(126 | 127)) => {
                Err(ListError::ShellUnavailable(e.message))
            }
            Err(_) => Ok(Vec::new()),
        }
    }

    /// `read()`: `Err` with `not editable` for a binary or oversized file.
    pub async fn read(&mut self, path: &str) -> Result<Result<String, ()>, String> {
        use base64::Engine as _;
        let escaped = self.escape_path(path, "read path")?;
        let cmd = format!(
            "[ -f {escaped} ] && [ -r {escaped} ] || exit 1; sz=$(stat -Lc %s -- {escaped}) || exit 1; if [ \"$sz\" -gt {MAX_CONTENT_SIZE} ]; then echo TOOBIG; exit 0; fi; if [ -s {escaped} ] && ! grep -qI '' -- {escaped}; then echo BINARY; exit 0; fi; echo OK; base64 -- {escaped} || exit 1"
        );
        let command = self.exec(&cmd).await.map_err(|e| e.message.clone())?;
        let raw = self.run(&command).await.map_err(|e| e.message)?;
        let (status, payload) = match raw.split_once('\n') {
            Some((s, p)) => (s.to_owned(), p.to_owned()),
            None => (raw.trim().to_owned(), String::new()),
        };
        if status == "TOOBIG" || status == "BINARY" {
            return Ok(Err(()));
        }
        let joined: String = payload.split_whitespace().collect();
        let content = base64::engine::general_purpose::STANDARD.decode(joined);
        let Ok(content) = content else {
            return Err("Could not read file.".to_owned());
        };
        if status != "OK" || content.len() > MAX_CONTENT_SIZE {
            return Err("Could not read file.".to_owned());
        }
        if content.contains(&0) {
            return Ok(Err(()));
        }
        Ok(Ok(String::from_utf8_lossy(&content).into_owned()))
    }

    /// `write()`.
    pub async fn write(&mut self, path: &str, content: &str) -> Result<(), String> {
        use base64::Engine as _;
        if content.len() > MAX_INLINE_WRITE_SIZE {
            validate_shell_safe_path(path, "write path")?;
            let dir = std::env::temp_dir();
            let local = dir.join(format!("coolify-write-{}", coolify_domain::new_public_id()));
            std::fs::write(&local, content).map_err(|e| e.to_string())?;
            let result = self.upload(&local, path).await;
            let _ = std::fs::remove_file(&local);
            return result;
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(content);
        self.exec_path(
            |escaped| format!("echo {b64} | base64 -d > {escaped}"),
            path,
            "write path",
        )
        .await
    }

    /// `makeDirectory()`.
    pub async fn make_directory(&mut self, path: &str) -> Result<(), String> {
        self.exec_path(|e| format!("mkdir -p -- {e}"), path, "mkdir path")
            .await
    }

    /// `createFile()`.
    pub async fn create_file(&mut self, path: &str) -> Result<(), String> {
        self.exec_path(
            |e| format!("[ -e {e} ] || : > {e}"),
            path,
            "create file path",
        )
        .await
    }

    /// `chmod()`.
    pub async fn chmod(&mut self, path: &str, mode: &str) -> Result<(), String> {
        let mode = mode.trim().to_owned();
        if !(3..=4).contains(&mode.len()) || !mode.chars().all(|c| ('0'..='7').contains(&c)) {
            return Err("Invalid permission mode. Use octal like 644 or 0755.".to_owned());
        }
        self.exec_path(|e| format!("chmod {mode} -- {e}"), path, "chmod path")
            .await
    }

    /// `rename()`.
    pub async fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        let escaped_from = self.escape_path(from, "rename source")?;
        self.exec_path(|e| format!("mv -- {escaped_from} {e}"), to, "rename target")
            .await
    }

    /// `delete()`.
    pub async fn delete(&mut self, path: &str) -> Result<(), String> {
        self.exec_path(|e| format!("rm -rf -- {e}"), path, "delete path")
            .await
    }

    /// `defaultRoot()`.
    pub async fn default_root(&self) -> String {
        let escaped = quote(&self.container);
        let dir = self
            .jobs
            .remote()
            .run_quiet(
                self.target,
                &[&format!(
                    "docker inspect --format '{{{{.Config.WorkingDir}}}}' {escaped}"
                )],
                RunOptions::op("files.command"),
            )
            .await
            .unwrap_or_default();
        let dir = dir.trim();
        if dir.is_empty() {
            "/".to_owned()
        } else {
            dir.to_owned()
        }
    }

    /// `upload()`.
    pub async fn upload(&self, local: &Path, dest: &str) -> Result<(), String> {
        validate_shell_safe_path(dest, "upload path")?;
        let server_tmp = format!("/tmp/coolify-upload-{}", coolify_domain::new_public_id());
        let remote = self.jobs.remote();
        let copied = remote.copy_file(self.target, local, &server_tmp).await;
        let result = match copied {
            Err(e) => Err(e.message),
            Ok(()) => self
                .run(&format!(
                    "docker cp {} {}:{}",
                    quote(&server_tmp),
                    quote(&self.container),
                    quote(dest)
                ))
                .await
                .map(|_| ())
                .map_err(|e| e.message),
        };
        let _ = remote
            .run_quiet(
                self.target,
                &[&format!("rm -f {}", quote(&server_tmp))],
                RunOptions::op("files.command"),
            )
            .await;
        result
    }

    /// `downloadInfo()`: `(is_dir, size)`.
    pub async fn download_info(&mut self, path: &str) -> Result<(bool, u64), String> {
        let escaped = self.escape_path(path, "download path")?;
        let cmd = format!(
            "if [ -d {escaped} ]; then echo \"dir $(du -sk {escaped} | cut -f1)\"; elif [ -e {escaped} ]; then echo \"file $(stat -L -c %s {escaped})\"; fi"
        );
        let command = self.exec(&cmd).await.map_err(|e| e.message.clone())?;
        let raw = self.run(&command).await.map_err(|e| e.message)?;
        let raw = raw.trim();
        let (kind, size) = raw.split_once(' ').unwrap_or((raw, ""));
        let parsed = size.parse::<u64>();
        match (kind, parsed) {
            ("dir", Ok(s)) => Ok((true, s * 1024)),
            ("file", Ok(s)) => Ok((false, s)),
            _ => Err("File not found.".to_owned()),
        }
    }

    /// `streamDownload()` with `buildDownloadCommand()`.
    pub async fn open_download(&mut self, path: &str, is_dir: bool) -> Result<RemoteRead, String> {
        let escaped = self.escape_path(path, "download path")?;
        let command = if self.use_helper {
            let inner = if is_dir {
                format!("tar czf - -C {escaped} .")
            } else {
                format!("cat -- {escaped}")
            };
            self.exec(&inner).await.map_err(|e| e.message.clone())?
        } else {
            let container = quote(&self.container);
            if is_dir {
                format!("docker exec {container} tar czf - -C {escaped} .")
            } else {
                format!("docker exec {container} cat -- {escaped}")
            }
        };
        self.jobs
            .remote()
            .open_command(self.target, &command)
            .await
            .map_err(|e| e.message)
    }
}

/// `guessLanguage()` of the component.
pub fn guess_language(name: &str) -> &'static str {
    let base = name.rsplit('/').next().unwrap_or(name).to_lowercase();
    match base.as_str() {
        "dockerfile" | "containerfile" => return "dockerfile",
        ".gitignore" | ".dockerignore" | ".editorconfig" | ".npmrc" | ".gitconfig" => return "ini",
        ".bashrc" | ".bash_profile" | ".bash_aliases" | ".profile" | ".zshrc" | ".zprofile" => {
            return "shell";
        }
        _ => {}
    }
    let dot_env = concat!(".", "env");
    if base == dot_env || base.starts_with(&format!("{dot_env}.")) {
        return "ini";
    }
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "js" | "mjs" | "cjs" => "javascript",
        "ts" => "typescript",
        "json" => "json",
        "php" => "php",
        "py" => "python",
        "rb" => "ruby",
        "go" => "go",
        "rs" => "rust",
        "java" => "java",
        "sh" | "bash" | "zsh" => "shell",
        "yml" | "yaml" => "yaml",
        "toml" | "ini" | "env" | "conf" | "cnf" => "ini",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "xml" | "svg" => "xml",
        "css" => "css",
        "scss" => "scss",
        "sql" => "sql",
        "dockerfile" => "dockerfile",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing() {
        let raw = "regular file\t5\t10\t644\troot\troot\t./b.txt\ndirectory\t0\t11\t755\troot\troot\t./A\nsymbolic link\t1\t12\t777\troot\troot\t./c\n";
        let e = parse_listing(raw);
        assert_eq!(e[0].name, "A");
        assert_eq!(e[0].kind, "dir");
        assert_eq!(e[1].name, "b.txt");
        assert_eq!(e[2].kind, "symlink");
    }

    #[test]
    fn process_status() {
        let s = "Uid:\t0\t1000\t0\t0\nGid:\t0\t1001\t0\t0\nGroups:\t1001 2000\nCapPrm:\t0000000000000401\n";
        let (user, groups, caps) = parse_process_status(s).unwrap_or_default();
        assert_eq!(user, "1000:1001");
        assert_eq!(groups, vec!["2000"]);
        assert_eq!(caps, vec!["CHOWN", "NET_BIND_SERVICE"]);
    }
}
