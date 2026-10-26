//! Read-only access to the upstream mirror through the `git` CLI.
//!
//! Every invocation uses an explicit argument vector (never a shell) and an
//! explicit `--git-dir`, so analysis can never touch a user's working tree.
//! Revisions coming from configuration or upstream data are validated before
//! use so they cannot be mistaken for options.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git {args:?} failed with status {status}: {stderr}")]
    Failed {
        args: Vec<String>,
        status: i32,
        stderr: String,
    },
    #[error("git {0:?} timed out")]
    Timeout(Vec<String>),
    #[error("invalid revision {0:?}")]
    InvalidRevision(String),
    #[error("io error running git: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct Git {
    git_dir: PathBuf,
    timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitInfo {
    pub sha: String,
    pub parents: Vec<String>,
    pub author: String,
    pub committed_at: String,
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    /// `A`, `M`, `D`, `R`, `C`, `T`.
    pub status: String,
    pub path: String,
    /// Previous path for renames/copies.
    pub old_path: Option<String>,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
}

/// Accepts SHAs, tags, branch names and `a..b` ranges made of those; rejects
/// anything that could be parsed as a git option or contains odd characters.
pub fn validate_revision(rev: &str) -> Result<(), GitError> {
    let ok = !rev.is_empty()
        && !rev.starts_with('-')
        && rev.len() <= 256
        && rev
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/-^~@".contains(c))
        && !rev.contains("@{");
    if ok {
        Ok(())
    } else {
        Err(GitError::InvalidRevision(rev.to_string()))
    }
}

impl Git {
    pub fn new(git_dir: impl Into<PathBuf>) -> Self {
        Self {
            git_dir: git_dir.into(),
            timeout: Duration::from_secs(300),
        }
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    pub fn run(&self, args: &[&str]) -> Result<String, GitError> {
        let mut cmd = Command::new("git");
        cmd.arg("--git-dir")
            .arg(&self.git_dir)
            .args(args)
            // Never prompt, never use the user's pager or hooks.
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_PAGER", "cat")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        let started = Instant::now();
        // Drain stdout on a thread so large outputs cannot deadlock the pipe.
        let mut stdout = child.stdout.take();
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(out) = stdout.as_mut() {
                let _ = std::io::Read::read_to_end(out, &mut buf);
            }
            buf
        });
        let mut stderr = child.stderr.take();
        let err_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(err) = stderr.as_mut() {
                let _ = std::io::Read::read_to_end(err, &mut buf);
            }
            buf
        });
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if started.elapsed() > self.timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(GitError::Timeout(
                    args.iter().map(|s| s.to_string()).collect(),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let out = reader.join().unwrap_or_default();
        let err = err_reader.join().unwrap_or_default();
        if !status.success() {
            return Err(GitError::Failed {
                args: args.iter().map(|s| s.to_string()).collect(),
                status: status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&err).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    pub fn fetch(&self, remote: &str) -> Result<(), GitError> {
        validate_revision(remote)?;
        self.run(&["fetch", "--prune", "--tags", remote])?;
        Ok(())
    }

    pub fn rev_parse(&self, rev: &str) -> Result<String, GitError> {
        validate_revision(rev)?;
        let spec = format!("{rev}^{{commit}}");
        Ok(self
            .run(&["rev-parse", "--verify", "--quiet", &spec])?
            .trim()
            .to_string())
    }

    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool, GitError> {
        validate_revision(ancestor)?;
        validate_revision(descendant)?;
        match self.run(&["merge-base", "--is-ancestor", ancestor, descendant]) {
            Ok(_) => Ok(true),
            Err(GitError::Failed { status: 1, .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// First-parent commits in `from..to`, oldest first. First-parent keeps
    /// merge commits as single units, matching how upstream lands changes.
    pub fn commits(&self, from: &str, to: &str) -> Result<Vec<CommitInfo>, GitError> {
        validate_revision(from)?;
        validate_revision(to)?;
        let range = format!("{from}..{to}");
        let out = self.run(&[
            "log",
            "--first-parent",
            "--reverse",
            "--format=%H%x1f%P%x1f%an%x1f%cI%x1f%s%x1e",
            &range,
        ])?;
        Ok(out
            .split('\x1e')
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .filter_map(|record| {
                let f: Vec<&str> = record.split('\x1f').collect();
                (f.len() == 5).then(|| CommitInfo {
                    sha: f[0].to_string(),
                    parents: f[1].split_whitespace().map(str::to_string).collect(),
                    author: f[2].to_string(),
                    committed_at: f[3].to_string(),
                    subject: f[4].to_string(),
                })
            })
            .collect())
    }

    /// Files changed between two revisions (rename detection on).
    pub fn changed_files(&self, from: &str, to: &str) -> Result<Vec<FileChange>, GitError> {
        validate_revision(from)?;
        validate_revision(to)?;
        let names = self.run(&["diff", "--name-status", "-M", "-z", from, to])?;
        let nums = self.run(&["diff", "--numstat", "-M", "-z", from, to])?;
        let mut changes = parse_name_status_z(&names);
        let stats = parse_numstat_z(&nums);
        for c in &mut changes {
            if let Some((a, d)) = stats.get(&c.path) {
                c.additions = *a;
                c.deletions = *d;
            }
        }
        Ok(changes)
    }

    /// Unified diff of one path between two revisions.
    pub fn diff_path(&self, from: &str, to: &str, path: &str) -> Result<String, GitError> {
        validate_revision(from)?;
        validate_revision(to)?;
        self.run(&["diff", "-M", from, to, "--", path])
    }

    /// Contents of `path` at `rev`, or `None` if it does not exist there.
    pub fn show(&self, rev: &str, path: &str) -> Result<Option<String>, GitError> {
        validate_revision(rev)?;
        let spec = format!("{rev}:{path}");
        match self.run(&["show", &spec]) {
            Ok(s) => Ok(Some(s)),
            Err(GitError::Failed { status: 128, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Paths under `dir` at `rev`.
    pub fn ls_tree(&self, rev: &str, dir: &str) -> Result<Vec<String>, GitError> {
        validate_revision(rev)?;
        let out = self.run(&["ls-tree", "-r", "--name-only", "-z", rev, "--", dir])?;
        Ok(out
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect())
    }
}

fn parse_name_status_z(out: &str) -> Vec<FileChange> {
    let mut parts = out.split('\0').filter(|s| !s.is_empty());
    let mut changes = Vec::new();
    while let Some(status) = parts.next() {
        let kind = status.chars().next().unwrap_or('M').to_string();
        if kind == "R" || kind == "C" {
            let old = parts.next().unwrap_or_default().to_string();
            let new = parts.next().unwrap_or_default().to_string();
            changes.push(FileChange {
                status: kind,
                path: new,
                old_path: Some(old),
                additions: None,
                deletions: None,
            });
        } else if let Some(path) = parts.next() {
            changes.push(FileChange {
                status: kind,
                path: path.to_string(),
                old_path: None,
                additions: None,
                deletions: None,
            });
        }
    }
    changes
}

type NumStats = std::collections::HashMap<String, (Option<u64>, Option<u64>)>;

fn parse_numstat_z(out: &str) -> NumStats {
    // Format: "<add>\t<del>\t<path>\0" or for renames "<add>\t<del>\t\0<old>\0<new>\0".
    let mut map = NumStats::new();
    let mut parts = out.split('\0');
    while let Some(head) = parts.next() {
        if head.is_empty() {
            continue;
        }
        let mut f = head.splitn(3, '\t');
        let add = f.next().and_then(|v| v.parse().ok());
        let del = f.next().and_then(|v| v.parse().ok());
        let path = f.next().unwrap_or_default();
        let path = if path.is_empty() {
            let _old = parts.next();
            parts.next().unwrap_or_default().to_string()
        } else {
            path.to_string()
        };
        map.insert(path, (add, del));
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_validation() {
        for ok in [
            "main",
            "v4.5.0",
            "b5823c777b8d",
            "origin/main",
            "HEAD~3",
            "a^",
        ] {
            assert!(validate_revision(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "--upload-pack=x",
            "a b",
            "a;b",
            "$(x)",
            "main@{1}",
            "a\nb",
        ] {
            assert!(validate_revision(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parses_z_formats() {
        let ns = "M\0routes/api.php\0R100\0a.php\0b.php\0A\0new file.php\0";
        let c = parse_name_status_z(ns);
        assert_eq!(c.len(), 3);
        assert_eq!(c[1].status, "R");
        assert_eq!(c[1].path, "b.php");
        assert_eq!(c[1].old_path.as_deref(), Some("a.php"));
        assert_eq!(c[2].path, "new file.php");

        let num = "3\t1\troutes/api.php\0".to_string() + "0\t0\t\0a.php\0b.php\0-\t-\tlogo.png\0";
        let m = parse_numstat_z(&num);
        assert_eq!(m["routes/api.php"], (Some(3), Some(1)));
        assert_eq!(m["b.php"], (Some(0), Some(0)));
        assert_eq!(m["logo.png"], (None, None));
    }
}
