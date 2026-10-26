//! Validation pipeline: runs checks with timeouts and records a five-state
//! outcome. Infrastructure problems are `Inconclusive`, never `Passed`.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Failed,
    Skipped,
    /// Not run because an earlier required check failed.
    Blocked,
    /// Could not determine a result (spawn failure, timeout, killed).
    Inconclusive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub command: Vec<String>,
    pub status: CheckStatus,
    pub exit_code: Option<i32>,
    pub duration_ms: u128,
    /// Last lines of combined output, for the report.
    pub output_tail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Validated,
    Failed,
    Inconclusive,
}

pub fn verdict(results: &[CheckResult]) -> Verdict {
    if results.iter().any(|r| r.status == CheckStatus::Failed) {
        Verdict::Failed
    } else if results.is_empty()
        || results
            .iter()
            .any(|r| matches!(r.status, CheckStatus::Inconclusive | CheckStatus::Blocked))
    {
        Verdict::Inconclusive
    } else {
        Verdict::Validated
    }
}

/// Programs the pipeline may run. Validation commands come from plans, which
/// are derived from untrusted upstream data, so they are allowlisted.
const ALLOWED_PROGRAMS: &[&str] = &["cargo"];

pub fn run_checks(
    commands: &[Vec<String>],
    cwd: &Path,
    timeout: Duration,
    envs: &[(String, String)],
) -> Vec<CheckResult> {
    let mut out = Vec::new();
    let mut blocked = false;
    for cmd in commands {
        if blocked {
            out.push(CheckResult {
                command: cmd.clone(),
                status: CheckStatus::Blocked,
                exit_code: None,
                duration_ms: 0,
                output_tail: "blocked by an earlier failure".into(),
            });
            continue;
        }
        let r = run_one(cmd, cwd, timeout, envs);
        if r.status != CheckStatus::Passed {
            blocked = true;
        }
        out.push(r);
    }
    out
}

fn run_one(
    cmd: &[String],
    cwd: &Path,
    timeout: Duration,
    envs: &[(String, String)],
) -> CheckResult {
    let started = Instant::now();
    let fail = |status, msg: String| CheckResult {
        command: cmd.to_vec(),
        status,
        exit_code: None,
        duration_ms: started.elapsed().as_millis(),
        output_tail: msg,
    };
    let Some((program, args)) = cmd.split_first() else {
        return fail(CheckStatus::Skipped, "empty command".into());
    };
    if !ALLOWED_PROGRAMS.contains(&program.as_str()) {
        return fail(
            CheckStatus::Skipped,
            format!("program {program:?} is not allowlisted"),
        );
    }
    let tmp = match tempfile::NamedTempFile::new() {
        Ok(t) => t,
        Err(e) => return fail(CheckStatus::Inconclusive, format!("temp file: {e}")),
    };
    let (Ok(stdout), Ok(stderr)) = (tmp.reopen(), tmp.reopen()) else {
        return fail(CheckStatus::Inconclusive, "temp file reopen failed".into());
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    for (k, v) in envs {
        command.env(k, v);
    }
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return fail(CheckStatus::Inconclusive, format!("spawn failed: {e}")),
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break None,
        }
    };
    let output = std::fs::read_to_string(tmp.path()).unwrap_or_default();
    let tail = tail_lines(&output, 40);
    match status {
        None => CheckResult {
            command: cmd.to_vec(),
            status: CheckStatus::Inconclusive,
            exit_code: None,
            duration_ms: started.elapsed().as_millis(),
            output_tail: format!("timed out after {timeout:?}\n{tail}"),
        },
        Some(s) => CheckResult {
            command: cmd.to_vec(),
            status: if s.success() {
                CheckStatus::Passed
            } else {
                CheckStatus::Failed
            },
            exit_code: s.code(),
            duration_ms: started.elapsed().as_millis(),
            output_tail: tail,
        },
    }
}

fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts() {
        let mk = |s| CheckResult {
            command: vec![],
            status: s,
            exit_code: None,
            duration_ms: 0,
            output_tail: String::new(),
        };
        assert_eq!(verdict(&[mk(CheckStatus::Passed)]), Verdict::Validated);
        assert_eq!(
            verdict(&[mk(CheckStatus::Passed), mk(CheckStatus::Failed)]),
            Verdict::Failed
        );
        assert_eq!(
            verdict(&[mk(CheckStatus::Inconclusive)]),
            Verdict::Inconclusive
        );
        assert_eq!(verdict(&[]), Verdict::Inconclusive);
    }

    #[test]
    fn non_allowlisted_programs_are_skipped_and_block() {
        let cmds = vec![
            vec!["sh".to_string(), "-c".into(), "echo hi".into()],
            vec!["cargo".into()],
        ];
        let r = run_checks(&cmds, Path::new("."), Duration::from_secs(5), &[]);
        assert_eq!(r[0].status, CheckStatus::Skipped);
        assert_eq!(r[1].status, CheckStatus::Blocked);
        assert_eq!(verdict(&r), Verdict::Inconclusive);
    }

    #[test]
    fn runs_cargo_and_captures_exit() {
        let ok = run_checks(
            &[vec!["cargo".into(), "--version".into()]],
            Path::new("."),
            Duration::from_secs(60),
            &[],
        );
        assert_eq!(ok[0].status, CheckStatus::Passed);
        assert!(ok[0].output_tail.contains("cargo"));
        let bad = run_checks(
            &[vec!["cargo".into(), "no-such-subcommand-xyz".into()]],
            Path::new("."),
            Duration::from_secs(60),
            &[],
        );
        assert_eq!(bad[0].status, CheckStatus::Failed);
    }
}
