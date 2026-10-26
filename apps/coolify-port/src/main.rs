//! `coolify-port`: track, analyse and port upstream Coolify changes.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use coolify_port::config::PortConfig;
use coolify_port::inventory::{
    MIGRATIONS_FILE, ROUTES_FILE, migration_names, render_migrations_file, render_routes_file,
    route_inventory,
};
use coolify_port::run::{Engine, classify_range};
use coolify_port::tracker::TrackerState;

#[derive(Parser)]
#[command(name = "coolify-port", version, about)]
struct Cli {
    /// Path to port.toml.
    #[arg(long, default_value = "port.toml", global = true)]
    config: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Fetch the upstream mirror (read-only for the working tree).
    Sync,
    /// Show tracking state and pending upstream commits.
    Status,
    /// Set the baseline (the upstream revision coolify-rs was built against).
    Init {
        #[arg(long)]
        baseline: String,
    },
    /// Regenerate deterministic inventories from an upstream revision (layer A).
    Inventory {
        #[arg(long)]
        rev: String,
        /// Write into the repository; otherwise print a summary.
        #[arg(long)]
        write: bool,
    },
    /// Classify a range and print the evidence (no side effects).
    Classify {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
    },
    /// Analyse and plan a range; with --generate also patch, validate and repair.
    Run {
        /// Defaults to the last analysed commit (or the baseline).
        #[arg(long)]
        from: Option<String>,
        /// Defaults to the tracked upstream branch head.
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        generate: bool,
    },
    /// Record that a run's port was merged by a human.
    MarkIntegrated {
        #[arg(long)]
        run_id: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = PortConfig::load(&cli.config)
        .with_context(|| format!("loading {}", cli.config.display()))?;
    let engine = Engine::new(cfg);
    let branch = engine.cfg.upstream.branch.clone();

    match cli.cmd {
        Cmd::Sync => {
            engine.git.fetch(&engine.cfg.upstream.remote)?;
            println!(
                "fetched {}; {} = {}",
                engine.cfg.upstream.remote,
                branch,
                engine.git.rev_parse(&branch)?
            );
        }
        Cmd::Status => {
            let st = TrackerState::load(&engine.cfg)?;
            let s = st.status(&engine.git, &branch)?;
            println!("{}", serde_json::to_string_pretty(&s)?);
        }
        Cmd::Init { baseline } => {
            let sha = engine.git.rev_parse(&baseline)?;
            let mut st = TrackerState::load(&engine.cfg)?;
            st.upstream_branch = branch;
            st.baseline = sha.clone();
            st.record("baseline", &sha, None);
            st.save(&engine.cfg)?;
            println!("baseline set to {baseline} ({sha})");
        }
        Cmd::Inventory { rev, write } => {
            let inv = route_inventory(&engine.git, &rev)?;
            let names = migration_names(&engine.git, &inv.upstream_commit)?;
            if write {
                let root = &engine.cfg.root;
                std::fs::write(root.join(ROUTES_FILE), render_routes_file(&inv))?;
                std::fs::write(
                    root.join(MIGRATIONS_FILE),
                    render_migrations_file(&inv.upstream_commit, &names),
                )?;
                println!(
                    "wrote {ROUTES_FILE} ({} routes) and {MIGRATIONS_FILE} ({} migrations)",
                    inv.routes.len(),
                    names.len()
                );
            } else {
                println!(
                    "{} routes, {} migrations at {}",
                    inv.routes.len(),
                    names.len(),
                    inv.upstream_commit
                );
            }
        }
        Cmd::Classify { from, to } => {
            let c = classify_range(&engine.git, &from, &to)?;
            println!("{}", serde_json::to_string_pretty(&c)?);
        }
        Cmd::Run { from, to, generate } => {
            let mut st = TrackerState::load(&engine.cfg)?;
            let from = match from
                .or_else(|| st.last_analyzed.clone())
                .or_else(|| (!st.baseline.is_empty()).then(|| st.baseline.clone()))
            {
                Some(f) => f,
                None => bail!(
                    "no --from and no baseline; run `coolify-port init --baseline <rev>` first"
                ),
            };
            let to = to.unwrap_or_else(|| branch.clone());
            let state = engine.run(&from, &to, generate)?;
            st.record("analyzed", &state.upstream_to, Some(&state.run_id));
            st.save(&engine.cfg)?;
            let plan = state.plan.as_ref();
            println!("run:      {}", state.run_id);
            println!("commits:  {}", state.commits.len());
            println!("risk:     {:?}", plan.map(|p| p.risk));
            println!("verdict:  {:?}", state.verdict);
            println!("next:     {}", state.recommended_action.unwrap_or_default());
            println!(
                "report:   {}",
                engine.run_dir(&state.run_id).join("report.md").display()
            );
        }
        Cmd::MarkIntegrated { run_id } => {
            let dir = engine.run_dir(&run_id);
            let bytes = std::fs::read(dir.join("state.json"))
                .with_context(|| format!("no run {run_id}"))?;
            let state: coolify_port::run::RunState = serde_json::from_slice(&bytes)?;
            let mut st = TrackerState::load(&engine.cfg)?;
            st.record("integrated", &state.upstream_to, Some(&run_id));
            st.save(&engine.cfg)?;
            println!(
                "marked {} integrated at upstream {}",
                run_id, state.upstream_to
            );
        }
    }
    Ok(())
}
