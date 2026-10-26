//! Migration-run orchestration: analyse → plan → generate (isolated branch)
//! → validate → bounded repair → report. Runs are resumable: each stage is
//! persisted to `<runs_dir>/<run_id>/state.json` and skipped when complete.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::classify::{Category, Classification, classify};
use crate::config::PortConfig;
use crate::git::{Git, GitError, validate_revision};
use crate::inventory::{
    MIGRATIONS_FILE, ROUTES_FILE, diff_routes, migration_names, render_migrations_file,
    render_routes_file, route_inventory,
};
use crate::mapping::{ChangeQuery, Registry};
use crate::php::migrations::summarize_migration;
use crate::plan::{MigrationChange, Plan, PlanInput, Risk, build_plan};
use crate::validate::{CheckResult, Verdict, run_checks, verdict};

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    Registry(#[from] crate::mapping::RegistryError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Created,
    Analyzed,
    Generated,
    Validated,
    Reported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitAnalysis {
    pub sha: String,
    pub subject: String,
    pub categories: Vec<Category>,
    pub security_sensitive: bool,
    pub files: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub number: u32,
    pub kind: String,
    pub checks: Vec<CheckResult>,
    pub verdict: Verdict,
    #[serde(default)]
    pub tamper_findings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunState {
    pub schema_version: u32,
    pub run_id: String,
    pub upstream_from: String,
    pub upstream_to: String,
    pub stage: Stage,
    pub commits: Vec<CommitAnalysis>,
    pub plan: Option<Plan>,
    pub branch: Option<String>,
    #[serde(default)]
    pub base_commit: Option<String>,
    pub worktree: Option<PathBuf>,
    pub generated_files: Vec<String>,
    pub patch_file: Option<PathBuf>,
    pub prompt_pack: Option<PathBuf>,
    pub attempts: Vec<Attempt>,
    pub verdict: Option<Verdict>,
    pub recommended_action: Option<String>,
    pub started_at: String,
    pub updated_at: String,
}

pub struct Engine {
    pub cfg: PortConfig,
    pub git: Git,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("root", &self.cfg.root)
            .finish_non_exhaustive()
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl Engine {
    pub fn new(cfg: PortConfig) -> Self {
        let git = Git::new(cfg.path(&cfg.upstream.mirror));
        Self { cfg, git }
    }

    pub fn registry(&self) -> Result<Registry, RunError> {
        Ok(Registry::load(&self.cfg.path(&self.cfg.engine.mapping))?)
    }

    pub fn run_dir(&self, run_id: &str) -> PathBuf {
        self.cfg.path(&self.cfg.engine.runs_dir).join(run_id)
    }

    pub fn run_id(from: &str, to: &str) -> String {
        format!(
            "run-{}-{}",
            &from[..from.len().min(9)],
            &to[..to.len().min(9)]
        )
    }

    /// Runs (or resumes) the full pipeline for `from..to`.
    pub fn run(&self, from: &str, to: &str, generate: bool) -> Result<RunState, RunError> {
        validate_revision(from)?;
        validate_revision(to)?;
        let from_sha = self.git.rev_parse(from)?;
        let to_sha = self.git.rev_parse(to)?;
        if !self.git.is_ancestor(&from_sha, &to_sha)? {
            return Err(RunError::Other(format!(
                "{from} is not an ancestor of {to}"
            )));
        }
        let run_id = Self::run_id(&from_sha, &to_sha);
        let dir = self.run_dir(&run_id);
        std::fs::create_dir_all(&dir)?;
        let state_path = dir.join("state.json");
        let mut state = match std::fs::read(&state_path) {
            Ok(bytes) => serde_json::from_slice::<RunState>(&bytes)?,
            Err(_) => RunState {
                schema_version: 1,
                run_id: run_id.clone(),
                upstream_from: from_sha.clone(),
                upstream_to: to_sha.clone(),
                stage: Stage::Created,
                commits: vec![],
                plan: None,
                branch: None,
                base_commit: None,
                worktree: None,
                generated_files: vec![],
                patch_file: None,
                prompt_pack: None,
                attempts: vec![],
                verdict: None,
                recommended_action: None,
                started_at: now(),
                updated_at: now(),
            },
        };
        let save = |s: &mut RunState| -> Result<(), RunError> {
            s.updated_at = now();
            std::fs::write(&state_path, serde_json::to_vec_pretty(s)?)?;
            Ok(())
        };

        if state.stage < Stage::Analyzed {
            let (plan, commits) = self.analyze(&from_sha, &to_sha)?;
            state.plan = Some(plan);
            state.commits = commits;
            state.stage = Stage::Analyzed;
            save(&mut state)?;
        }
        let plan = state
            .plan
            .clone()
            .ok_or_else(|| RunError::Other("analysed run without plan".into()))?;

        if generate && state.stage < Stage::Generated {
            self.generate(&mut state, &plan, &dir)?;
            state.stage = Stage::Generated;
            save(&mut state)?;
        }

        if generate && state.stage < Stage::Validated {
            self.validate_with_repair(&mut state, &plan, &dir)?;
            state.stage = Stage::Validated;
            save(&mut state)?;
        }

        if state.stage < Stage::Reported {
            state.recommended_action = Some(recommend(&state, &plan));
            crate::report::write_reports(self, &state, &dir)?;
            state.stage = Stage::Reported;
            save(&mut state)?;
            crate::report::update_metrics(self, &state)?;
        }
        Ok(state)
    }

    pub fn analyze(&self, from: &str, to: &str) -> Result<(Plan, Vec<CommitAnalysis>), RunError> {
        let registry = self.registry()?;
        let commits = self.git.commits(from, to)?;
        let files = self.git.changed_files(from, to)?;
        let diff = self.git.run(&[
            "diff",
            "-M",
            from,
            to,
            "--",
            "app",
            "bootstrap",
            "routes",
            "database",
            "config",
        ])?;
        let subjects: Vec<&str> = commits.iter().map(|c| c.subject.as_str()).collect();
        let classification = classify(&files, &diff, &subjects.join("\n"));

        // Per-commit classification (for the ledger and calibration data).
        let mut per_commit = Vec::new();
        for c in &commits {
            let parent = c.parents.first().cloned().unwrap_or_default();
            if parent.is_empty() {
                continue;
            }
            let cf = self.git.changed_files(&parent, &c.sha)?;
            let cd = self.git.run(&[
                "diff",
                "-M",
                &parent,
                &c.sha,
                "--",
                "app",
                "bootstrap",
                "routes",
                "database",
                "config",
            ])?;
            let cc = classify(&cf, &cd, &c.subject);
            per_commit.push(CommitAnalysis {
                sha: c.sha.clone(),
                subject: c.subject.clone(),
                categories: cc.categories.keys().copied().collect(),
                security_sensitive: cc.security_sensitive(),
                files: cf.len(),
            });
        }

        // Migrations added or modified in the range.
        let mapped_tables: BTreeSet<String> = registry
            .components
            .iter()
            .filter(|c| c.status != crate::mapping::Status::Planned)
            .flat_map(|c| c.upstream.tables.iter().cloned())
            .collect();
        let mut migrations = Vec::new();
        let mut touched_tables = Vec::new();
        for f in files
            .iter()
            .filter(|f| f.path.starts_with("database/migrations/") && f.status != "D")
        {
            let src = self.git.show(to, &f.path)?.unwrap_or_default();
            let summary = summarize_migration(&src);
            let names = summary.table_names();
            touched_tables.extend(names.iter().cloned());
            let mapped: Vec<String> = names
                .into_iter()
                .filter(|t| mapped_tables.contains(t))
                .collect();
            let name = f
                .path
                .trim_start_matches("database/migrations/")
                .trim_end_matches(".php")
                .to_string();
            migrations.push(MigrationChange {
                name,
                summary,
                mapped_tables: mapped,
            });
        }

        let before = route_inventory(&self.git, from)?;
        let after = route_inventory(&self.git, to)?;
        let route_diff = diff_routes(&before.routes, &after.routes);
        let route_keys = route_diff.keys();

        let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
        let components = registry.affected(&ChangeQuery {
            paths: &paths,
            symbols: &classification.changed_symbols,
            tables: &touched_tables,
            routes: &route_keys,
        });

        let plan = build_plan(PlanInput {
            from,
            to,
            commits: commits.iter().map(|c| c.sha.clone()).collect(),
            classification: &classification,
            files: paths,
            components,
            route_diff,
            migrations,
        });
        Ok((plan, per_commit))
    }

    /// Creates the isolated branch/worktree and applies Layer A regeneration.
    fn generate(&self, state: &mut RunState, plan: &Plan, dir: &Path) -> Result<(), RunError> {
        let branch = format!("port/{}", state.run_id);
        let worktree = dir.join("worktree");
        if !worktree.exists() {
            state.base_commit = Some(
                repo_git(&self.cfg.root, &["rev-parse", "HEAD"])?
                    .trim()
                    .to_string(),
            );
            repo_git(
                &self.cfg.root,
                &[
                    "worktree",
                    "add",
                    "-B",
                    &branch,
                    worktree.to_str().unwrap_or_default(),
                    "HEAD",
                ],
            )?;
        }
        state.branch = Some(branch);
        state.worktree = Some(worktree.clone());

        let mut generated = Vec::new();
        let to = &state.upstream_to;
        if plan
            .actions
            .iter()
            .any(|a| a.id == "regenerate_route_inventory")
            || !plan.route_diff.is_empty()
        {
            let inv = route_inventory(&self.git, to)?;
            write_file(&worktree.join(ROUTES_FILE), &render_routes_file(&inv))?;
            generated.push(ROUTES_FILE.to_string());
        }
        if plan
            .actions
            .iter()
            .any(|a| a.id == "regenerate_migration_inventory")
        {
            let names = migration_names(&self.git, to)?;
            write_file(
                &worktree.join(MIGRATIONS_FILE),
                &render_migrations_file(to, &names),
            )?;
            generated.push(MIGRATIONS_FILE.to_string());
        }
        state.generated_files = generated;

        let status = repo_git(&worktree, &["status", "--porcelain"])?;
        if !status.trim().is_empty() {
            repo_git(&worktree, &["add", "--all"])?;
            let msg = format!(
                "port: deterministic updates for upstream {}..{}\n\nGenerated by coolify-port (layer A). Run: {}\nRisk: {:?}",
                &state.upstream_from[..9],
                &state.upstream_to[..9],
                state.run_id,
                plan.risk
            );
            repo_git(
                &worktree,
                &[
                    "-c",
                    "user.name=coolify-port",
                    "-c",
                    "user.email=coolify-port@localhost",
                    "commit",
                    "--no-verify",
                    "-m",
                    &msg,
                ],
            )?;
        }
        let base = state
            .base_commit
            .clone()
            .ok_or_else(|| RunError::Other("no base commit".into()))?;
        let patch = repo_git(&worktree, &["diff", "--binary", &base, "HEAD"])?;
        let patch_path = dir.join("patch.diff");
        std::fs::write(&patch_path, &patch)?;
        state.patch_file = Some(patch_path);

        let pack = crate::report::prompt_pack(self, state, plan)?;
        let pack_path = dir.join("prompt-pack.md");
        std::fs::write(&pack_path, pack)?;
        state.prompt_pack = Some(pack_path);
        Ok(())
    }

    fn validate_with_repair(
        &self,
        state: &mut RunState,
        plan: &Plan,
        dir: &Path,
    ) -> Result<(), RunError> {
        let worktree = state
            .worktree
            .clone()
            .ok_or_else(|| RunError::Other("no worktree".into()))?;
        let timeout = Duration::from_secs(self.cfg.validation.timeout_secs);
        let envs = vec![(
            "CARGO_TARGET_DIR".to_string(),
            self.cfg
                .path(&self.cfg.validation.cargo_target_dir)
                .display()
                .to_string(),
        )];
        let checks = run_checks(&plan.validation, &worktree, timeout, &envs);
        let mut v = verdict(&checks);
        state.attempts.push(Attempt {
            number: 0,
            kind: "initial".into(),
            checks,
            verdict: v,
            tamper_findings: vec![],
        });

        let repair = &self.cfg.repair;
        let mut n = 0;
        while v == Verdict::Failed && !repair.agent_command.is_empty() && n < repair.max_attempts {
            n += 1;
            let before = repo_git(&worktree, &["rev-parse", "HEAD"])?
                .trim()
                .to_string();
            let agent_ok = run_agent(
                &repair.agent_command,
                &worktree,
                dir,
                state,
                repair.agent_timeout_secs,
            );
            let agent_diff = repo_git(&worktree, &["diff", &before]).unwrap_or_default();
            let tamper = tamper_findings(&agent_diff);
            let checks = if agent_ok {
                run_checks(&plan.validation, &worktree, timeout, &envs)
            } else {
                vec![]
            };
            v = if !tamper.is_empty() {
                Verdict::Failed
            } else {
                verdict(&checks)
            };
            state.attempts.push(Attempt {
                number: n,
                kind: "agent_repair".into(),
                checks,
                verdict: v,
                tamper_findings: tamper,
            });
        }
        state.verdict = Some(v);
        Ok(())
    }
}

fn recommend(state: &RunState, plan: &Plan) -> String {
    match (state.verdict, plan.risk) {
        (None, _) => "analysis only: re-run with generation to produce a patch".into(),
        (Some(Verdict::Validated), Risk::Low) => {
            "validated low-risk patch: open a draft PR for routine review".into()
        }
        (Some(Verdict::Validated), Risk::Medium) => {
            "validated: open a draft PR; complete the listed layer C/D actions before merge".into()
        }
        (Some(Verdict::Validated), Risk::High) => {
            "validated deterministic part, but HIGH risk: human approval and semantic port required (see escalation)".into()
        }
        (Some(Verdict::Failed), _) => {
            "validation failed within the repair budget: escalate with the attached failure output".into()
        }
        (Some(Verdict::Inconclusive), _) => {
            "inconclusive (infrastructure or skipped checks): fix the environment and resume the run".into()
        }
    }
}

/// Flags agent edits that weaken tests instead of fixing code.
pub fn tamper_findings(diff: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut file = String::new();
    for line in diff.lines() {
        if let Some(p) = line.strip_prefix("+++ b/") {
            file = p.to_string();
            continue;
        }
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        let protected = file.starts_with("compat/baseline/")
            || file.starts_with("crates/coolify-compat/tests/fixtures/")
            || file.starts_with(".github/");
        if protected && (line.starts_with('+') || line.starts_with('-')) {
            out.push(format!("{file}: protected file modified"));
        }
        if line.starts_with('+') && line.contains("#[ignore") {
            out.push(format!("{file}: test ignored"));
        }
        if line.starts_with('-') && line.trim_start_matches('-').trim() == "#[test]" {
            out.push(format!("{file}: test removed"));
        }
        if line.starts_with('-') && (line.contains("assert") && !line.contains("debug_assert")) {
            out.push(format!(
                "{file}: assertion removed or changed: {}",
                line.trim()
            ));
        }
    }
    out.sort();
    out.dedup();
    out
}

fn run_agent(cmd: &[String], worktree: &Path, dir: &Path, state: &RunState, timeout: u64) -> bool {
    let Some((program, args)) = cmd.split_first() else {
        return false;
    };
    let pack = state.prompt_pack.clone().unwrap_or_default();
    let mut child = match Command::new(program)
        .args(args)
        .current_dir(worktree)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("PORT_RUN_DIR", dir)
        .env("PORT_PROMPT_PACK", pack)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return s.success(),
            Ok(None) if started.elapsed().as_secs() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(_) => return false,
        }
    }
}

/// git against the coolify-rs repository (not the upstream mirror).
pub fn repo_git(cwd: &Path, args: &[&str]) -> Result<String, RunError> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()?;
    if !out.status.success() {
        return Err(RunError::Other(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn write_file(path: &Path, content: &str) -> Result<(), RunError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

/// Classification helper exposed for the CLI `classify` command.
pub fn classify_range(git: &Git, from: &str, to: &str) -> Result<Classification, RunError> {
    let files = git.changed_files(from, to)?;
    let diff = git.run(&[
        "diff",
        "-M",
        from,
        to,
        "--",
        "app",
        "bootstrap",
        "routes",
        "database",
        "config",
    ])?;
    let commits = git.commits(from, to)?;
    let subjects: Vec<&str> = commits.iter().map(|c| c.subject.as_str()).collect();
    Ok(classify(&files, &diff, &subjects.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_test_tampering() {
        let diff = "\
+++ b/crates/coolify-api/tests/projects.rs
-    #[test]
+    #[ignore]
-    assert_eq!(status, 201);
+++ b/compat/baseline/supported-migrations.txt
+2099_01_01_000000_evil
+++ b/crates/coolify-api/src/lib.rs
+    let x = 1;
";
        let f = tamper_findings(diff);
        assert!(f.iter().any(|s| s.contains("test removed")));
        assert!(f.iter().any(|s| s.contains("test ignored")));
        assert!(f.iter().any(|s| s.contains("assertion removed")));
        assert!(f.iter().any(|s| s.contains("protected file")));
        assert!(!f.iter().any(|s| s.contains("lib.rs")));
    }

    #[test]
    fn run_ids_are_deterministic() {
        assert_eq!(
            Engine::run_id("aaaaaaaaaaaa", "bbbbbbbbbbbb"),
            "run-aaaaaaaaa-bbbbbbbbb"
        );
    }
}
