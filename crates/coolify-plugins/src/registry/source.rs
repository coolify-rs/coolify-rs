//! The registry repository's files, `plugins/<publisher>/<name>.toml`, and the index built from them.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::index::{Entry, FORMAT, Index, Release, Revoked};

/// One `plugins/<publisher>/<name>.toml`. The id comes from the path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked: Option<Revoked>,
    #[serde(default)]
    pub versions: Vec<Release>,
}

/// Reads one plugin file; `id` is `<publisher>/<name>` from its path.
pub fn read_file(id: &str, text: &str) -> Result<Entry, String> {
    let file: PluginFile = toml::from_str(text).map_err(|e| format!("{id}: {}", e.message()))?;
    let entry = Entry {
        id: id.to_owned(),
        revoked: file.revoked,
        versions: file.versions,
    };
    entry.validate()?;
    // Instances download over HTTPS only; catch a wrong URL here rather than on every instance.
    if let Some(r) = entry
        .versions
        .iter()
        .find(|r| !r.url.starts_with("https://"))
    {
        return Err(format!("{id} {}: url must be https", r.version));
    }
    Ok(entry)
}

/// Every plugin of the registry at `dir` (the repository root), sorted by id.
pub fn load(dir: &Path) -> Result<Vec<Entry>, String> {
    let plugins = dir.join("plugins");
    let mut out = Vec::new();
    let publishers =
        std::fs::read_dir(&plugins).map_err(|e| format!("{}: {e}", plugins.display()))?;
    for publisher in publishers {
        let publisher = publisher.map_err(|e| e.to_string())?;
        if !publisher.file_type().map_err(|e| e.to_string())?.is_dir() {
            continue;
        }
        let pub_name = publisher.file_name().to_string_lossy().into_owned();
        for file in std::fs::read_dir(publisher.path()).map_err(|e| e.to_string())? {
            let path = file.map_err(|e| e.to_string())?.path();
            if path.extension().is_none_or(|e| e != "toml") {
                continue;
            }
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let id = format!("{pub_name}/{name}");
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            out.push(read_file(&id, &text)?);
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// Builds `index.json` from the registry at `dir`. Every manifest must be one this coolify-rs can run, so
/// the registry's CI must use a coolify-rs that knows the newest plugin interface.
pub fn build_index(dir: &Path, generated_at: DateTime<Utc>) -> Result<Index, String> {
    let plugins = load(dir)?;
    for e in &plugins {
        for r in &e.versions {
            r.manifest()
                .map_err(|err| format!("{} {}: {err}", e.id, r.version))?;
        }
    }
    let index = Index {
        format: FORMAT,
        generated_at,
        plugins,
    };
    index.validate()?;
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const FILE: &str = r#"
[[versions]]
version = "0.1.0"
source = "https://github.com/acme/deploy-stats"
commit = "0123456789abcdef0123456789abcdef01234567"
toolchain = "rust 1.99.1"
builder = "docker.io/library/rust:1.99.1@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
sha256 = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
url = "https://github.com/acme/deploy-stats/releases/download/v0.1.0/deploy_stats.wasm"

[versions.build]
command = "cargo build --release --locked --target wasm32-wasip2"
artifact = "target/wasm32-wasip2/release/deploy_stats.wasm"

[versions.manifest]
id = "acme/deploy-stats"
name = "Deploy stats"
version = "0.1.0"
description = "Counts deployments."
license = "MIT"
api = "0.1"

[versions.manifest.capabilities]
events = ["deployment.succeeded"]
storage = true
"#;

    #[test]
    fn reads_a_plugin_file() {
        let e = read_file("acme/deploy-stats", FILE).unwrap();
        assert_eq!(e.versions.len(), 1);
        assert_eq!(e.versions[0].build.dir, ".");
        let m = e.versions[0].manifest().unwrap();
        assert_eq!(m.capabilities.events, ["deployment.succeeded"]);
        assert!(!e.versions[0].yanked);

        let err = read_file("other/deploy-stats", FILE).unwrap_err();
        assert!(err.contains("must match the entry"), "{err}");
        let err = read_file("acme/deploy-stats", &format!("shell = true\n{FILE}")).unwrap_err();
        assert!(err.contains("unknown field"), "{err}");
        let http = FILE.replace("url = \"https://", "url = \"http://");
        let err = read_file("acme/deploy-stats", &http).unwrap_err();
        assert!(err.contains("url must be https"), "{err}");
    }

    #[test]
    fn builds_an_index_from_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let acme = dir.path().join("plugins/acme");
        std::fs::create_dir_all(&acme).unwrap();
        std::fs::write(acme.join("deploy-stats.toml"), FILE).unwrap();
        std::fs::write(acme.join("README.md"), "not a plugin").unwrap();
        let now = "2026-11-06T00:00:00Z".parse().unwrap();
        let index = build_index(dir.path(), now).unwrap();
        assert_eq!(index.plugins.len(), 1);
        assert_eq!(index.plugins[0].id, "acme/deploy-stats");
        assert_eq!(Index::parse(&index.to_bytes()).unwrap(), index);

        let newer = FILE.replace("api = \"0.1\"", "api = \"0.9\"");
        std::fs::write(acme.join("deploy-stats.toml"), newer).unwrap();
        let err = build_index(dir.path(), now).unwrap_err();
        assert!(err.contains("plugin interface"), "{err}");
    }
}
