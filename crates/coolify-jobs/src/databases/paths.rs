//! Path checks of file mounts: `validateFileMountPath`, `validateHostFileMountPath`,
//! `confineFileMountPath`, `normalizeUnixPath` (bootstrap/helpers/shared.php) and the
//! static parts of `LocalFileVolume`.

use crate::compose_parser::validate::validate_shell_safe_path;

fn shell_safe(input: &str, context: &str) -> Result<(), String> {
    validate_shell_safe_path(input, context).map(|_| ())
}

/// `str($input)->replaceMatches('#/+#', '/')`.
fn squeeze_slashes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut previous = false;
    for c in input.chars() {
        if c == '/' {
            if !previous {
                out.push(c);
            }
            previous = true;
        } else {
            out.push(c);
            previous = false;
        }
    }
    out
}

/// `validateFileMountPath`.
pub fn validate_file_mount_path(input: &str, context: &str) -> Result<String, String> {
    shell_safe(input, context)?;
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
    let path = squeeze_slashes(&started);
    if path
        .trim_matches('/')
        .split('/')
        .any(|s| s == "." || s == "..")
    {
        return Err(format!(
            "Invalid {context}: relative path segments ('.' or '..') are not allowed."
        ));
    }
    Ok(path)
}

/// `validateHostFileMountPath`.
pub fn validate_host_file_mount_path(input: &str, context: &str) -> Result<String, String> {
    shell_safe(input, context)?;
    if input.contains('\0') {
        return Err(format!("Invalid {context}: contains null byte."));
    }
    if input.contains('\\') {
        return Err(format!(
            "Invalid {context}: backslash directory separators are not allowed."
        ));
    }
    let path = squeeze_slashes(input.trim());
    if path.is_empty() || !path.starts_with('/') {
        return Err(format!("Invalid {context}: must be an absolute path."));
    }
    if path == "/" || path.ends_with('/') {
        return Err(format!(
            "Invalid {context}: must point to a file, not a directory."
        ));
    }
    if path
        .trim_matches('/')
        .split('/')
        .any(|s| s == "." || s == "..")
    {
        return Err(format!(
            "Invalid {context}: relative path segments ('.' or '..') are not allowed."
        ));
    }
    normalize_unix_path(&path)
}

/// `normalizeUnixPath`.
pub fn normalize_unix_path(path: &str) -> Result<String, String> {
    shell_safe(path, "path")?;
    if path.contains('\0') {
        return Err("Invalid path: contains null byte.".into());
    }
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
                    if !absolute {
                        segments.push("..");
                    }
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
        ".".into()
    } else {
        joined
    })
}

/// `confineFileMountPath`.
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

/// `LocalFileVolume::resolveHostPath` with one resource directory.
pub fn resolve_host_path(directory: &str, path: &str, context: &str) -> Result<String, String> {
    let directory = normalize_unix_path(directory)?;
    let path = path.trim();
    if path.is_empty() {
        return Err(format!("Invalid {context}: the path is empty."));
    }
    if path.starts_with('~') {
        return Err(format!(
            "Invalid {context}: use an absolute path instead of ~."
        ));
    }
    shell_safe(path, context)?;
    let absolute = path.starts_with('/');
    let resolved = normalize_unix_path(&if absolute {
        path.to_owned()
    } else {
        format!("{directory}/{path}")
    })?;
    if inside(&resolved, &directory) {
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

/// `resourceDirectoryContaining` for one directory.
pub fn inside(path: &str, directory: &str) -> bool {
    path == directory || path.starts_with(&format!("{directory}/"))
}

/// `resolvedFsPath`: a relative path is inside the resource directory.
pub fn resolved_fs_path(fs_path: &str, workdir: &str) -> String {
    let path = fs_path.trim();
    if path.starts_with('/') || path.starts_with('~') {
        return path.to_owned();
    }
    let path = path.strip_prefix('.').unwrap_or(path);
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        workdir.trim_end_matches('/').to_owned()
    } else {
        format!("{}/{path}", workdir.trim_end_matches('/'))
    }
}

/// `isOutsideResourceDirectory`: a path that cannot be resolved counts as outside.
pub fn is_outside_resource_directory(fs_path: &str, workdir: &str) -> bool {
    let Ok(path) = normalize_unix_path(&resolved_fs_path(fs_path, workdir)) else {
        return true;
    };
    let Ok(directory) = normalize_unix_path(workdir) else {
        return true;
    };
    path.starts_with('~') || !inside(&path, &directory)
}

/// `LocalFileVolume::REMOTE_PATH_CONFINEMENT_SCRIPT`.
pub const REMOTE_PATH_CONFINEMENT_SCRIPT: &str = r#"resolve() {
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
pub const REMOTE_FILE_STATE_SCRIPT: &str = r#"i=0
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_paths() {
        assert_eq!(validate_file_mount_path("a//b", "x").as_deref(), Ok("/a/b"));
        assert!(validate_file_mount_path("/a/../b", "x").is_err());
        assert!(validate_file_mount_path("/a;b", "x").is_err());
    }

    #[test]
    fn host_paths() {
        assert_eq!(
            resolve_host_path("/data/x", "foo/bar", "p").as_deref(),
            Ok("/data/x/foo/bar")
        );
        assert_eq!(
            resolve_host_path("/data/x", "/etc/y", "p").as_deref(),
            Ok("/etc/y")
        );
        assert!(resolve_host_path("/data/x", "../y", "p").is_err());
        assert!(resolve_host_path("/data/x", "~/y", "p").is_err());
        assert!(resolve_host_path("/data/x", "/", "p").is_err());
    }

    #[test]
    fn outside() {
        assert!(!is_outside_resource_directory("/data/x/a", "/data/x"));
        assert!(is_outside_resource_directory("/etc/a", "/data/x"));
        assert!(!is_outside_resource_directory("./a", "/data/x"));
    }
}
