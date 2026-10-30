//! The server side of `LocalFileVolume` for services: `saveStorageOnServer()`,
//! `deleteStorageOnServer()`, `getFilesystemVolumesFromServer()` and
//! `EnsureContentFilesOnServer`.

use base64::Engine as _;
use coolify_compat::yaml::PhpValue;
use coolify_db::servers::ColumnValue;
use coolify_db::services::{self as svc, ServiceRow, SubKind, SubRow};
use coolify_remote::{RunOptions, Target, shell::quote};
use serde_json::Value;

use super::workdir;
use crate::Jobs;
use crate::compose_parser::{php, validate, volumes};

const BINARY_PLACEHOLDER: &str = "[binary file]";
const TOO_LARGE_PLACEHOLDER: &str = "[file too large to display]";

/// `LocalFileVolume::REMOTE_PATH_CONFINEMENT_SCRIPT`.
const CONFINEMENT_SCRIPT: &str = r#"resolve() {
    path=$1
    rest=
    case $path in /*) ;; *) return 1 ;; esac
    while [ ! -e "$path" ]; do
        if [ -L "$path" ]; then return 1; fi
        rest=/${path##*/}$rest
        path=${path%/*}
        [ -n "$path" ] || path=/
    done
    case "$rest/" in */./*|*/../*) return 1 ;; esac
    path=$(readlink -f "$path") || return 1
    printf "%s\n" "${path%/}$rest"
}
base=$(resolve "$1") || exit 1
target=$(resolve "$2") || exit 1
case $target in "$base"|"$base"/*) echo OK ;; *) echo NOK ;; esac"#;

/// `LocalFileVolume::REMOTE_FILE_STATE_SCRIPT`.
const FILE_STATE_SCRIPT: &str = r#"i=0
for path in "$@"; do
    i=$((i + 1))
    if [ -L "$path" ]; then
        if [ -f "$path" ]; then state=file; else state=other; fi
    elif [ -f "$path" ]; then
        state=file
    elif [ -d "$path" ]; then
        state=directory
        if entries=$(ls -A "$path" 2>/dev/null); then
            if [ -z "$entries" ]; then state=empty-directory; fi
        fi
    elif [ -e "$path" ]; then
        state=other
    else
        state=missing
    fi
    printf "%s:%s\n" "$i" "$state"
done"#;

/// `normalizeUnixPath($path)`.
pub fn normalize_unix_path(path: &str) -> Result<String, String> {
    validate::validate_shell_safe_path(path, "path")?;
    if path.contains('\\') {
        return Err("Invalid path: backslash directory separators are not allowed.".into());
    }
    let absolute = path.starts_with('/');
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.is_empty() || segments.last() == Some(&"..") {
                    if absolute {
                        return Err(
                            "Invalid path: resolved path escapes the base directory.".into()
                        );
                    }
                    segments.push("..");
                } else {
                    segments.pop();
                }
            }
            other => segments.push(other),
        }
    }
    let joined = segments.join("/");
    Ok(if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_owned()
    } else {
        joined
    })
}

static SLASHES: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new("/+").unwrap_or_else(|_| unreachable!()));

fn no_relative_segments(path: &str, context: &str) -> Result<(), String> {
    if path
        .trim_matches('/')
        .split('/')
        .any(|s| s == "." || s == "..")
    {
        return Err(format!(
            "Invalid {context}: relative path segments ('.' or '..') are not allowed."
        ));
    }
    Ok(())
}

/// `validateFileMountPath($input, $context)`.
pub fn validate_file_mount_path(input: &str, context: &str) -> Result<String, String> {
    validate::validate_shell_safe_path(input, context)?;
    if input.contains('\0') {
        return Err(format!(
            "Invalid {context}: contains null byte. Null bytes are not allowed in file mount paths for security reasons."
        ));
    }
    if input.contains('\\') {
        return Err(format!(
            "Invalid {context}: backslash directory separators are not allowed."
        ));
    }
    let trimmed = input.trim();
    let started = if trimmed.starts_with('/') {
        trimmed.to_owned()
    } else {
        format!("/{trimmed}")
    };
    let path = SLASHES.replace_all(&started, "/").into_owned();
    no_relative_segments(&path, context)?;
    Ok(path)
}

/// `validateHostFileMountPath($input, $context)`.
pub fn validate_host_file_mount_path(input: &str, context: &str) -> Result<String, String> {
    validate::validate_shell_safe_path(input, context)?;
    if input.contains('\0') {
        return Err(format!("Invalid {context}: contains null byte."));
    }
    if input.contains('\\') {
        return Err(format!(
            "Invalid {context}: backslash directory separators are not allowed."
        ));
    }
    let path = SLASHES.replace_all(input.trim(), "/").into_owned();
    if !path.starts_with('/') {
        return Err(format!("Invalid {context}: must be an absolute path."));
    }
    if path == "/" || path.ends_with('/') {
        return Err(format!(
            "Invalid {context}: must point to a file, not a directory."
        ));
    }
    no_relative_segments(&path, context)?;
    normalize_unix_path(&path)
}

/// `confineFileMountPath($baseDirectory, $path, $context)`.
pub fn confine_file_mount_path(base: &str, path: &str, context: &str) -> Result<String, String> {
    let base = normalize_unix_path(base)?;
    let mount = validate_file_mount_path(path, context)?;
    let resolved = normalize_unix_path(&format!("{base}/{mount}"))?;
    if resolved != base && !resolved.starts_with(&format!("{base}/")) {
        return Err(format!(
            "Invalid {context}: resolved path must stay inside the resource configuration directory."
        ));
    }
    Ok(resolved)
}

/// `LocalFileVolume::isReadOnlyVolume()` of a service part.
pub fn file_is_read_only(raw: Option<&str>, part: &str, mount_path: &str) -> bool {
    let Some(yaml) = raw.and_then(|r| validate::parse_docker_compose_yaml(r).ok()) else {
        return false;
    };
    let Some(PhpValue::Array(list)) = php::get(&yaml, "services")
        .and_then(|s| php::get(s, part))
        .and_then(|s| php::get(s, "volumes"))
    else {
        return false;
    };
    let wanted = mount_path.trim_start_matches('/');
    for (_, volume) in list {
        match volume {
            PhpValue::Str(s) => {
                let parts: Vec<&str> = s.split(':').collect();
                if parts.len() >= 2
                    && (parts[1].trim_start_matches('/') == wanted || parts[1] == mount_path)
                {
                    return parts.get(2) == Some(&"ro");
                }
            }
            PhpValue::Array(_) => {
                let target = php::data_get_str(volume, "target", "");
                if target.trim_start_matches('/') == wanted || target == mount_path {
                    return php::get(volume, "read_only") == Some(&PhpValue::Bool(true));
                }
            }
            _ => {}
        }
    }
    false
}

/// `LocalFileVolume::resolvedFsPath($workdir)`.
pub fn resolved_fs_path(fs_path: &str, dir: &str) -> String {
    let path = fs_path.trim();
    if path.starts_with('/') || path.starts_with('~') {
        return path.to_owned();
    }
    let path = path
        .strip_prefix('.')
        .unwrap_or(path)
        .trim_start_matches('/');
    let dir = dir.trim_end_matches('/');
    if path.is_empty() {
        dir.to_owned()
    } else {
        format!("{dir}/{path}")
    }
}

/// `LocalFileVolume::isOutsideResourceDirectory()`.
pub fn is_outside_resource_directory(fs_path: &str, dir: &str) -> bool {
    let Ok(path) = normalize_unix_path(&resolved_fs_path(fs_path, dir)) else {
        return true;
    };
    let Ok(dir) = normalize_unix_path(dir) else {
        return true;
    };
    path.starts_with('~') || !(path == dir || path.starts_with(&format!("{dir}/")))
}

/// `LocalFileVolume::resolveHostPath($resourceDirectories, $path, $context)`.
pub fn resolve_host_path(dir: &str, path: &str, context: &str) -> Result<String, String> {
    let dir = normalize_unix_path(dir)?;
    let path = path.trim();
    if path.is_empty() {
        return Err(format!("Invalid {context}: the path is empty."));
    }
    if path.starts_with('~') {
        return Err(format!(
            "Invalid {context}: use an absolute path instead of ~."
        ));
    }
    validate::validate_shell_safe_path(path, context)?;
    let absolute = path.starts_with('/');
    let resolved = normalize_unix_path(&if absolute {
        path.to_owned()
    } else {
        format!("{dir}/{path}")
    })?;
    if resolved == dir || resolved.starts_with(&format!("{dir}/")) {
        return Ok(resolved);
    }
    if !absolute {
        return Err(format!(
            "Invalid {context}: a relative path must stay inside the resource directory."
        ));
    }
    if path.split('/').any(|s| s == "." || s == "..") {
        return Err(format!(
            "Invalid {context}: '.' and '..' segments are not allowed."
        ));
    }
    if resolved == "/" {
        return Err(format!(
            "Invalid {context}: the root directory cannot be mounted."
        ));
    }
    Ok(resolved)
}

/// `LocalFileVolume::isAdminControlledComposeMount()` for a service part.
fn is_admin_controlled(raw: Option<&str>, fs_path: &str, mount_path: &str, dir: &str) -> bool {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return false;
    };
    let Ok(yaml) = validate::parse_docker_compose_yaml(raw) else {
        return false;
    };
    let Ok(wanted) = normalize_unix_path(fs_path) else {
        return false;
    };
    let Some(PhpValue::Array(services)) = php::get(&yaml, "services") else {
        return false;
    };
    for (_, service) in services {
        let Some(PhpValue::Array(list)) = php::get(service, "volumes") else {
            continue;
        };
        for (_, volume) in list {
            let (source, target) = match volume {
                PhpValue::Str(s) => {
                    let Ok(p) = validate::parse_docker_volume_string(s) else {
                        return false;
                    };
                    (p.source.unwrap_or_default(), p.target.unwrap_or_default())
                }
                other => (
                    php::data_get_str(other, "source", ""),
                    php::data_get_str(other, "target", ""),
                ),
            };
            if target != mount_path || !volumes::source_is_local(&source) {
                continue;
            }
            let legacy = volumes::legacy_replace_local_source(&source, dir);
            if normalize_unix_path(&legacy).is_ok_and(|p| p == wanted) {
                return true;
            }
            if let Ok(resolved) = volumes::replace_local_source(&source, dir)
                && normalize_unix_path(&resolved).is_ok_and(|p| p == wanted)
            {
                return true;
            }
        }
    }
    false
}

struct FileRow {
    id: i64,
    fs_path: String,
    mount_path: String,
    content: Option<String>,
    is_directory: bool,
    is_host_file: bool,
    is_based_on_git: bool,
    chown: Option<String>,
    chmod: Option<String>,
}

fn file_row(jobs: &Jobs, r: &serde_json::Map<String, Value>) -> FileRow {
    let content = r.get("content").and_then(Value::as_str).and_then(|c| {
        jobs.0
            .encrypter
            .as_ref()
            .and_then(|e| e.decrypt_string(c).ok())
    });
    let text = |k: &str| r.get(k).and_then(Value::as_str).map(str::to_owned);
    let flag = |k: &str| r.get(k).and_then(Value::as_bool).unwrap_or(false);
    FileRow {
        id: r.get("id").and_then(Value::as_i64).unwrap_or_default(),
        fs_path: text("fs_path").unwrap_or_default(),
        mount_path: text("mount_path").unwrap_or_default(),
        content,
        is_directory: flag("is_directory"),
        is_host_file: flag("is_host_file"),
        is_based_on_git: flag("is_based_on_git"),
        chown: text("chown"),
        chmod: text("chmod"),
    }
}

async fn run(jobs: &Jobs, t: &Target, command: &str) -> Result<String, String> {
    jobs.remote()
        .run(t, &[command], RunOptions::op("service.storage"))
        .await
        .map(Option::unwrap_or_default)
        .map_err(|e| e.message)
}

async fn test_path(jobs: &Jobs, t: &Target, flag: &str, escaped: &str) -> Result<bool, String> {
    let c = format!("test {flag} {escaped} && echo OK || echo NOK");
    Ok(run(jobs, t, &c).await?.trim() == "OK")
}

/// `LocalFileVolume::remoteFileStates($paths, $server)`.
async fn remote_file_states(jobs: &Jobs, t: &Target, paths: &[String]) -> Vec<String> {
    let args = paths.iter().map(|p| quote(p)).collect::<Vec<_>>().join(" ");
    let c = format!("sh -c {} sh {args}", quote(FILE_STATE_SCRIPT));
    let out = jobs
        .remote()
        .run_quiet(t, &[c.as_str()], RunOptions::op("service.storage"))
        .await
        .unwrap_or_default();
    (0..paths.len())
        .map(|i| {
            let prefix = format!("{}:", i + 1);
            out.lines()
                .find_map(|l| l.trim().strip_prefix(&prefix).map(str::to_owned))
                .unwrap_or_else(|| "unknown".into())
        })
        .collect()
}

/// `LocalFileVolume::assertRemotePathIsConfined($base, $path, $server)`.
async fn assert_confined(jobs: &Jobs, t: &Target, base: &str, path: &str) -> Result<(), String> {
    let c = format!(
        "sh -c {} sh {} {}",
        quote(CONFINEMENT_SCRIPT),
        quote(base),
        quote(path)
    );
    let out = jobs
        .remote()
        .run_quiet(t, &[c.as_str()], RunOptions::op("service.storage"))
        .await
        .unwrap_or_default();
    if out.trim() == "OK" {
        Ok(())
    } else {
        Err("Invalid storage path: resolved path must stay inside the resource configuration directory.".into())
    }
}

/// `LocalFileVolume::assertRemotePathIsNotSymlink($path, $server)`.
pub async fn assert_not_symlink(jobs: &Jobs, t: &Target, path: &str) -> Result<(), String> {
    let c = format!("test -L {} && echo LINK || echo OK", quote(path));
    if run(jobs, t, &c).await?.trim() == "OK" {
        Ok(())
    } else {
        Err(format!(
            "{path} is a symbolic link on the server. Coolify does not write a file through a symbolic link. Remove the link or use another path."
        ))
    }
}

/// `LocalFileVolume::assertHostPathOnServer($dir, $path, $server, $isDirectory)`.
pub async fn assert_host_path_on_server(
    jobs: &Jobs,
    t: &Target,
    dir: &str,
    path: &str,
    is_directory: bool,
) -> Result<(), String> {
    let normalized = normalize_unix_path(dir)?;
    if path == normalized || path.starts_with(&format!("{normalized}/")) {
        assert_confined(jobs, t, dir, path).await
    } else if !is_directory {
        assert_not_symlink(jobs, t, path).await
    } else {
        Ok(())
    }
}

async fn file_row_by_id(jobs: &Jobs, id: i64) -> Result<Option<FileRow>, String> {
    let row = svc::file_storage_by_id(&jobs.0.db, id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(row.map(|r| file_row(jobs, &r)))
}

/// `LocalFileVolume::saveStorageOnServer($server)`.
pub async fn save_storage_on_server(
    jobs: &Jobs,
    s: &ServiceRow,
    t: &Target,
    id: i64,
) -> Result<(), String> {
    let Some(f) = file_row_by_id(jobs, id).await? else {
        return Ok(());
    };
    if f.is_host_file {
        return Ok(());
    }
    let dir = workdir(&s.uuid);
    let mut path = resolved_fs_path(&f.fs_path, &dir);
    if !is_admin_controlled(s.str("docker_compose_raw"), &f.fs_path, &f.mount_path, &dir) {
        let host = resolve_host_path(&dir, &path, "storage path")?;
        let inside = host == dir || host.starts_with(&format!("{dir}/"));
        if inside {
            assert_confined(jobs, t, &dir, &host).await?;
        } else if !f.is_directory {
            assert_not_symlink(jobs, t, &host).await?;
        }
        path = host;
    }
    let outside = is_outside_resource_directory(&f.fs_path, &dir);
    let mut commands: Vec<String> = Vec::new();
    if f.is_directory {
        validate::validate_shell_safe_path(&path, "storage path")?;
        commands.push(format!(
            "mkdir -p {} > /dev/null 2>&1 || true",
            quote(&path)
        ));
        commands.push(format!("mkdir -p {} > /dev/null 2>&1 || true", quote(&dir)));
    }
    if path.starts_with('/') || path.starts_with('~') {
        let parent = php::before_last(&path, "/");
        if !parent.is_empty() {
            commands.push(format!(
                "mkdir -p {} > /dev/null 2>&1 || true",
                quote(&parent)
            ));
        }
    }
    validate::validate_shell_safe_path(&path, "storage path")?;
    let escaped = quote(&path);
    let is_file = test_path(jobs, t, "-f", &escaped).await?;
    let is_dir = test_path(jobs, t, "-d", &escaped).await?;
    let mut replaces_empty = false;
    if is_file && f.is_directory {
        let content = run(jobs, t, &format!("head -c 5242881 {escaped}"))
            .await
            .unwrap_or_default();
        let content = if content.len() > 5_242_880 {
            TOO_LARGE_PLACEHOLDER.to_owned()
        } else {
            content
        };
        set_file_content(jobs, id, Some(&content), false).await?;
        return Err("The following file is a file on the server, but you are trying to mark it as a directory. Please delete the file on the server or mark it as directory.".into());
    } else if is_dir && !f.is_directory {
        if ["/", ".", "..", ""].contains(&path.as_str()) {
            set_file_content(jobs, id, f.content.as_deref(), true).await?;
            return Err("The following file is a directory on the server, but you are trying to mark it as a file. <br><br>Please delete the directory on the server or mark it as directory.".into());
        }
        let states = remote_file_states(jobs, t, std::slice::from_ref(&path)).await;
        if outside || states.first().map(String::as_str) != Some("empty-directory") {
            return Err(format!(
                "The following file is a directory on the server, but you are trying to mark it as a file: {path}<br><br>Please delete the directory on the server or mark it as directory."
            ));
        }
        replaces_empty = true;
    }
    if (!is_dir || replaces_empty) && !f.is_directory {
        if replaces_empty {
            commands.push(format!("rmdir {escaped}"));
        }
        match f
            .content
            .as_deref()
            .filter(|c| php::truthy(&PhpValue::Str((*c).to_owned())))
        {
            Some(content) => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(content);
                commands.push(format!(
                    "echo '{encoded}' | base64 -d | tee {escaped} > /dev/null"
                ));
            }
            None => commands.push(format!("touch {escaped}")),
        }
        commands.push(format!("chmod +x {escaped}"));
        if let Some(chown) = f.chown.as_deref().filter(|c| !c.is_empty()) {
            commands.push(format!("chown -- {} {escaped}", quote(chown)));
        }
        if let Some(chmod) = f.chmod.as_deref().filter(|c| !c.is_empty()) {
            commands.push(format!("chmod -- {} {escaped}", quote(chmod)));
        }
    } else if !is_dir && f.is_directory {
        commands.push(format!("mkdir -p {escaped} > /dev/null 2>&1 || true"));
    }
    if commands.is_empty() {
        return Ok(());
    }
    let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
    jobs.remote()
        .run(t, &refs, RunOptions::op("service.storage"))
        .await
        .map(|_| ())
        .map_err(|e| e.message)
}

/// `LocalFileVolume::deleteStorageOnServer($server)`.
pub async fn delete_storage_on_server(
    jobs: &Jobs,
    s: &ServiceRow,
    t: &Target,
    id: i64,
) -> Result<(), String> {
    let Some(f) = file_row_by_id(jobs, id).await? else {
        return Ok(());
    };
    if f.is_host_file {
        return Ok(());
    }
    let dir = workdir(&s.uuid);
    let mut path = resolved_fs_path(&f.fs_path, &dir);
    if is_outside_resource_directory(&f.fs_path, &dir) {
        return Ok(());
    }
    if !is_admin_controlled(s.str("docker_compose_raw"), &f.fs_path, &f.mount_path, &dir) {
        path = normalize_unix_path(&path)?;
        assert_confined(jobs, t, &dir, &path).await?;
    }
    validate::validate_shell_safe_path(&path, "storage path")?;
    let escaped = quote(&path);
    let is_file = test_path(jobs, t, "-f", &escaped).await?;
    let is_dir = test_path(jobs, t, "-d", &escaped).await?;
    let mut commands: Vec<String> = Vec::new();
    if !path.is_empty() && !["/", ".", ".."].contains(&path.as_str()) {
        if is_file {
            commands.push(format!("rm -rf {escaped} > /dev/null 2>&1 || true"));
        } else if is_dir {
            commands.push(format!("rm -rf {escaped} > /dev/null 2>&1 || true"));
            commands.push(format!("rmdir {escaped} > /dev/null 2>&1 || true"));
        }
    }
    if commands.is_empty() {
        return Ok(());
    }
    let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
    jobs.remote()
        .run(t, &refs, RunOptions::op("service.storage"))
        .await
        .map(|_| ())
        .map_err(|e| e.message)
}

/// `$fileVolume->content = ...; $fileVolume->is_directory = ...; $fileVolume->save()`.
async fn set_file_content(
    jobs: &Jobs,
    id: i64,
    content: Option<&str>,
    is_directory: bool,
) -> Result<(), String> {
    let encrypted = match (content, jobs.0.encrypter.as_ref()) {
        (Some(c), Some(e)) => Some(e.encrypt_string(c)),
        _ => None,
    };
    svc::storage_update(
        &jobs.0.db,
        "local_file_volumes",
        id,
        &[
            ("content", ColumnValue::Text(encrypted)),
            ("is_directory", ColumnValue::Bool(is_directory)),
        ],
        Jobs::now(),
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

async fn set_is_directory(jobs: &Jobs, id: i64, is_directory: bool) -> Result<(), String> {
    svc::storage_update(
        &jobs.0.db,
        "local_file_volumes",
        id,
        &[("is_directory", ColumnValue::Bool(is_directory))],
        Jobs::now(),
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// `getFilesystemVolumesFromServer($oneService, $isInit)`.
pub async fn files_from_server(
    jobs: &Jobs,
    s: &ServiceRow,
    t: &Target,
    kind: SubKind,
    sub_id: i64,
    is_init: bool,
) -> Result<(), String> {
    let dir = workdir(&s.uuid);
    run(
        jobs,
        t,
        &format!("mkdir -p -- {} > /dev/null 2>&1 || true", quote(&dir)),
    )
    .await?;
    let rows = svc::file_storages(&jobs.0.db, kind, sub_id)
        .await
        .map_err(|e| e.to_string())?;
    for r in &rows {
        let f = file_row(jobs, r);
        let trimmed = f.fs_path.trim();
        let location = if trimmed.starts_with('$') {
            trimmed.to_owned()
        } else {
            resolved_fs_path(&f.fs_path, &dir)
        };
        let escaped = shell_argument(&location)?;
        let is_file = test_path(jobs, t, "-f", &escaped).await?;
        let is_dir = test_path(jobs, t, "-d", &escaped).await?;
        let outside = is_outside_resource_directory(&f.fs_path, &dir);
        let has_content = f
            .content
            .as_deref()
            .is_some_and(|c| !c.is_empty() && c != "0");
        if is_file {
            set_is_directory(jobs, f.id, false).await?;
        } else if is_dir && !f.is_directory && has_content {
            if !outside {
                let states = remote_file_states(jobs, t, std::slice::from_ref(&location)).await;
                if states.first().map(String::as_str) == Some("empty-directory") {
                    let _ = save_storage_on_server(jobs, s, t, f.id).await;
                }
            }
        } else if is_dir {
            set_file_content(jobs, f.id, None, true).await?;
        } else if !f.is_directory && is_init && has_content {
            if outside {
                continue;
            }
            set_is_directory(jobs, f.id, false).await?;
            let encoded = base64::engine::general_purpose::STANDARD
                .encode(f.content.as_deref().unwrap_or_default());
            let mkdir = format!("mkdir -p -- \"$(dirname -- {escaped})\"");
            let write = format!("echo '{encoded}' | base64 -d | tee -- {escaped}");
            jobs.remote()
                .run(
                    t,
                    &[mkdir.as_str(), write.as_str()],
                    RunOptions::op("service.storage"),
                )
                .await
                .map_err(|e| e.message)?;
        } else if is_init {
            // Missing: a directory, also a file flag without content.
            set_file_content(jobs, f.id, None, true).await?;
            if !outside {
                run(jobs, t, &format!("mkdir -p -- {escaped}")).await?;
            }
        }
    }
    Ok(())
}

/// `filesystemVolumeShellArgument($path)`.
fn shell_argument(path: &str) -> Result<String, String> {
    if path.trim().is_empty() {
        return Err("Invalid storage path: path is empty.".into());
    }
    validate::validate_compose_array_volume_source(path)?;
    let (tilde, rest) = if path.starts_with('~') {
        let end = path.find('/').map_or(path.len(), |i| i + 1);
        path.split_at(end)
    } else {
        ("", path)
    };
    if !rest.contains('$') {
        return Ok(if tilde.is_empty() {
            quote(rest)
        } else if rest.is_empty() {
            tilde.to_owned()
        } else {
            format!("{tilde}{}", quote(rest))
        });
    }
    let quoted = rest.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(format!("{tilde}\"{quoted}\""))
}

/// `EnsureContentFilesOnServer::echoCommands($fileStorages, $server)`.
pub async fn ensure_content_files(
    jobs: &Jobs,
    s: &ServiceRow,
    t: &Target,
    subs: &[SubRow],
) -> Vec<String> {
    let dir = workdir(&s.uuid);
    let echo = |m: &str| format!("echo {}", quote(m));
    let mut out = Vec::new();
    let mut candidates: Vec<(FileRow, String)> = Vec::new();
    for sub in subs {
        let Ok(rows) = svc::file_storages(&jobs.0.db, sub.kind, sub.id).await else {
            continue;
        };
        for r in &rows {
            let f = file_row(jobs, r);
            let content = f.content.as_deref().unwrap_or_default();
            if f.is_directory
                || f.is_host_file
                || f.is_based_on_git
                || content.is_empty()
                || content == BINARY_PLACEHOLDER
                || content == TOO_LARGE_PLACEHOLDER
            {
                continue;
            }
            let resolved = resolved_fs_path(&f.fs_path, &dir);
            let path = if is_admin_controlled(
                s.str("docker_compose_raw"),
                &f.fs_path,
                &f.mount_path,
                &dir,
            ) {
                Ok(resolved)
            } else {
                resolve_host_path(&dir, &resolved, "storage path")
            };
            match path {
                Ok(p) => candidates.push((f, p)),
                Err(e) => out.push(echo(&format!("Warning: {e}"))),
            }
        }
    }
    if candidates.is_empty() {
        return out;
    }
    let paths: Vec<String> = candidates.iter().map(|(_, p)| p.clone()).collect();
    let states = remote_file_states(jobs, t, &paths).await;
    let mut to_write: Vec<(&FileRow, &String)> = Vec::new();
    for ((f, path), state) in candidates.iter().zip(&states) {
        match state.as_str() {
            "missing" | "empty-directory" => to_write.push((f, path)),
            "file" => {}
            "directory" => out.push(echo(&format!("Warning: A directory that is not empty is at {path} on the server. Coolify did not write the configuration file for {}. Remove the directory on the server, then start the resource again.", f.mount_path))),
            "other" => out.push(echo(&format!("Warning: A symbolic link or a special file is at {path} on the server. Coolify did not write the configuration file for {}.", f.mount_path))),
            _ => out.push(echo(&format!("Warning: Coolify cannot check the configuration file {path} on the server."))),
        }
    }
    if to_write.is_empty() {
        return out;
    }
    let count = to_write.len();
    out.push(echo(&if count == 1 {
        "Writing 1 missing configuration file.".to_owned()
    } else {
        format!("Writing {count} missing configuration files.")
    }));
    for (f, path) in to_write {
        if let Err(e) = save_storage_on_server(jobs, s, t, f.id).await {
            out.push(echo(&format!(
                "Warning: Coolify cannot write the configuration file {path}: {e}"
            )));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(
            normalize_unix_path("/a/./b/../c").unwrap_or_default(),
            "/a/c"
        );
        assert_eq!(resolved_fs_path("./x/y", "/d/u"), "/d/u/x/y");
        assert_eq!(resolved_fs_path("/abs", "/d/u"), "/abs");
        assert!(!is_outside_resource_directory("/d/u/f", "/d/u"));
        assert!(is_outside_resource_directory("/etc/f", "/d/u"));
    }
}
