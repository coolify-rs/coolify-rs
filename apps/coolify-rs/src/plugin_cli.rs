//! `coolify-rs plugin ...`: tools for plugin authors and the plugin registry's CI (ADR 0010, section 7).
//! None of them needs a database.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use coolify_plugins::registry::{conformance, index::Index, review, signing, source};

#[derive(Subcommand)]
pub enum PluginCmd {
    /// Conformance check: load the component in the real sandbox, send it every subscribed event and
    /// malformed input, call its routes. Prints a JSON report; exits non-zero on failure.
    Check {
        component: PathBuf,
        manifest: PathBuf,
    },
    /// Create a registry signing key pair. Prints only the public half.
    Keygen {
        /// Where to write the secret half (created; never overwritten).
        secret_out: PathBuf,
        /// Where to write the public half.
        public_out: PathBuf,
        #[arg(long)]
        key_id: String,
        /// Last day (YYYY-MM-DD) instances accept the key.
        #[arg(long)]
        not_after: String,
    },
    /// Build and sign `index.json` from a registry repository.
    BuildIndex {
        registry_dir: PathBuf,
        /// The secret key file from `keygen`.
        #[arg(long)]
        key: PathBuf,
        /// Output folder; `<registry-dir>/dist` by default.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Verify `index.json` and `index.json.sig` in a folder with the compiled-in keys.
    VerifyIndex {
        dist_dir: PathBuf,
        /// Also trust this public key file (from `keygen`).
        #[arg(long)]
        trust: Vec<PathBuf>,
    },
    /// Pull request checks of a registry repository against its main branch: metadata, capability diff,
    /// review tier, and the versions to rebuild. Exits non-zero when a check fails.
    Review {
        registry_dir: PathBuf,
        /// A checkout of the main branch; without it every plugin counts as new.
        #[arg(long)]
        base: Option<PathBuf>,
        /// Writes review.json, summary.md and one folder per version to rebuild.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

fn json(v: &impl serde::Serialize) -> Result<String> {
    Ok(serde_json::to_string_pretty(v)?)
}

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| path.display().to_string())
}

pub async fn run(cmd: PluginCmd) -> Result<()> {
    match cmd {
        PluginCmd::Check {
            component,
            manifest,
        } => {
            let component = read(&component)?;
            let manifest = std::fs::read_to_string(&manifest)
                .with_context(|| manifest.display().to_string())?;
            let report =
                conformance::check(&component, &manifest, coolify_plugins::Settings::default())
                    .await;
            println!("{}", json(&report)?);
            if !report.ok {
                std::process::exit(1);
            }
            Ok(())
        }
        PluginCmd::Keygen {
            secret_out,
            public_out,
            key_id,
            not_after,
        } => {
            let not_after = not_after
                .parse()
                .context("--not-after must be YYYY-MM-DD")?;
            let (secret, public) =
                signing::generate(&key_id, not_after).map_err(anyhow::Error::msg)?;
            write_new(&secret_out, &json(&secret)?, true)?;
            write_new(&public_out, &json(&public)?, false)?;
            println!("{}", json(&public)?);
            Ok(())
        }
        PluginCmd::BuildIndex {
            registry_dir,
            key,
            out,
        } => {
            let secret: signing::SecretKeyFile = serde_json::from_slice(&read(&key)?)
                .with_context(|| format!("{}: not a key file from keygen", key.display()))?;
            let index = source::build_index(&registry_dir, chrono::Utc::now())
                .map_err(anyhow::Error::msg)?;
            let bytes = index.to_bytes();
            let sig = signing::sign(&bytes, &secret).map_err(anyhow::Error::msg)?;
            let out = out.unwrap_or_else(|| registry_dir.join("dist"));
            std::fs::create_dir_all(&out)?;
            std::fs::write(out.join("index.json"), &bytes)?;
            std::fs::write(out.join("index.json.sig"), &sig)?;
            println!(
                "{}",
                json(&serde_json::json!({
                    "plugins": index.plugins.len(),
                    "key_id": secret.key_id,
                    "generated_at": index.generated_at,
                    "out": out,
                }))?
            );
            Ok(())
        }
        PluginCmd::VerifyIndex { dist_dir, trust } => {
            let mut keys = signing::production_keys();
            for path in trust {
                let public: signing::PublicKeyFile = serde_json::from_slice(&read(&path)?)
                    .with_context(|| format!("{}: not a public key file", path.display()))?;
                keys.push(public.trusted().map_err(anyhow::Error::msg)?);
            }
            if keys.is_empty() {
                bail!(
                    "no key to verify with: this build has no production registry key; pass --trust"
                );
            }
            let bytes = read(&dist_dir.join("index.json"))?;
            let sig = read(&dist_dir.join("index.json.sig"))?;
            let key = signing::verify(&bytes, &sig, &keys, chrono::Utc::now())?;
            let index = Index::parse(&bytes).map_err(anyhow::Error::msg)?;
            let revoked: Vec<&str> = index
                .plugins
                .iter()
                .filter(|e| e.revoked.is_some())
                .map(|e| e.id.as_str())
                .collect();
            println!(
                "{}",
                json(&serde_json::json!({
                    "verified": true,
                    "key_id": key.id,
                    "generated_at": index.generated_at,
                    "plugins": index.plugins.len(),
                    "revoked": revoked,
                }))?
            );
            Ok(())
        }
        PluginCmd::Review {
            registry_dir,
            base,
            out,
        } => {
            let head = source::load(&registry_dir).map_err(anyhow::Error::msg)?;
            let base = match base {
                Some(dir) if dir.join("plugins").is_dir() => {
                    source::load(&dir).map_err(anyhow::Error::msg)?
                }
                _ => vec![],
            };
            let r = review::review(&base, &head);
            let summary = r.markdown();
            if let Some(out) = out {
                std::fs::create_dir_all(out.join("builds"))?;
                std::fs::write(out.join("review.json"), json(&r)?)?;
                std::fs::write(out.join("summary.md"), &summary)?;
                for b in &r.builds {
                    let dir = out.join("builds").join(format!(
                        "{}__{}",
                        b.id.replace('/', "__"),
                        b.version
                    ));
                    std::fs::create_dir_all(&dir)?;
                    std::fs::write(dir.join("release.json"), json(b)?)?;
                    std::fs::write(dir.join("plugin.toml"), &b.manifest_toml)?;
                }
            }
            print!("{summary}");
            if !r.errors.is_empty() {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

/// Writes a file that must not exist yet; `secret` makes it readable by the owner only.
fn write_new(path: &Path, text: &str, secret: bool) -> Result<()> {
    use std::io::Write as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    if secret {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = secret;
    let mut f = opts
        .open(path)
        .with_context(|| format!("{}: refusing to overwrite or cannot create", path.display()))?;
    f.write_all(text.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}
