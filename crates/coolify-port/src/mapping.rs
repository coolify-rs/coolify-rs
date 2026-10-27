//! The upstream → Rust traceability registry (`compat/mapping/registry.toml`).
//!
//! Each component links upstream evidence (paths, symbols, routes, tables,
//! tests) to the Rust code, contracts and tests that implement it. The engine
//! uses it to answer "which Rust parts are likely affected by these upstream
//! changes?" without relying on file-name similarity.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const REGISTRY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registry {
    pub schema_version: u32,
    #[serde(default, rename = "component")]
    pub components: Vec<Component>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Planned,
    Partial,
    Implemented,
    Verified,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpstreamRefs {
    /// Path globs (`*` within a segment, `**` across segments).
    #[serde(default)]
    pub paths: Vec<String>,
    /// `path::symbol()` as produced by the classifier.
    #[serde(default)]
    pub symbols: Vec<String>,
    /// Route keys `METHOD uri` (uri may end with `*`).
    #[serde(default)]
    pub routes: Vec<String>,
    #[serde(default)]
    pub tables: Vec<String>,
    #[serde(default)]
    pub tests: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RustRefs {
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub tests: Vec<String>,
    /// Cargo packages whose tests must run when this component changes.
    #[serde(default)]
    pub packages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Component {
    pub id: String,
    pub title: String,
    pub status: Status,
    #[serde(default)]
    pub upstream: UpstreamRefs,
    #[serde(default)]
    pub rust: RustRefs,
    #[serde(default)]
    pub contracts: Vec<String>,
    #[serde(default)]
    pub divergences: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ChangeQuery<'a> {
    pub paths: &'a [String],
    pub symbols: &'a BTreeSet<String>,
    pub tables: &'a [String],
    pub routes: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentMatch {
    pub component: String,
    pub status: Status,
    pub reasons: Vec<String>,
    pub rust_paths: Vec<String>,
    pub rust_packages: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("reading {0}: {1}")]
    Io(String, std::io::Error),
    #[error("parsing registry: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("unsupported registry schema_version {0} (expected {REGISTRY_SCHEMA_VERSION})")]
    Version(u32),
    #[error("duplicate component id {0}")]
    Duplicate(String),
}

impl Registry {
    pub fn load(path: &Path) -> Result<Self, RegistryError> {
        let src = std::fs::read_to_string(path)
            .map_err(|e| RegistryError::Io(path.display().to_string(), e))?;
        Self::parse(&src)
    }

    pub fn parse(src: &str) -> Result<Self, RegistryError> {
        let reg: Registry = toml::from_str(src)?;
        if reg.schema_version != REGISTRY_SCHEMA_VERSION {
            return Err(RegistryError::Version(reg.schema_version));
        }
        let mut ids = BTreeSet::new();
        for c in &reg.components {
            if !ids.insert(c.id.clone()) {
                return Err(RegistryError::Duplicate(c.id.clone()));
            }
        }
        Ok(reg)
    }

    pub fn affected(&self, q: &ChangeQuery<'_>) -> Vec<ComponentMatch> {
        let mut out = Vec::new();
        for c in &self.components {
            let mut reasons = Vec::new();
            for p in q.paths {
                if c.upstream.paths.iter().any(|g| glob_match(g, p)) {
                    reasons.push(format!("path {p}"));
                }
                if c.upstream.tests.iter().any(|g| glob_match(g, p)) {
                    reasons.push(format!("upstream test {p}"));
                }
            }
            for s in q.symbols {
                if c.upstream.symbols.iter().any(|x| x == s) {
                    reasons.push(format!("symbol {s}"));
                }
            }
            for t in q.tables {
                if c.upstream.tables.iter().any(|x| x == t) {
                    reasons.push(format!("table {t}"));
                }
            }
            for r in q.routes {
                if c.upstream.routes.iter().any(|x| route_match(x, r)) {
                    reasons.push(format!("route {r}"));
                }
            }
            if !reasons.is_empty() {
                out.push(ComponentMatch {
                    component: c.id.clone(),
                    status: c.status,
                    reasons,
                    rust_paths: c.rust.paths.clone(),
                    rust_packages: c.rust.packages.clone(),
                });
            }
        }
        out
    }
}

fn route_match(pattern: &str, key: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => key.starts_with(prefix),
        None => pattern == key,
    }
}

/// Minimal glob: `**` matches any sequence, `*` matches within one segment.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn inner(p: &[u8], s: &[u8]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(b'*') if p.get(1) == Some(&b'*') => {
                let rest = p[2..].strip_prefix(b"/").unwrap_or(&p[2..]);
                (0..=s.len()).any(|i| inner(rest, &s[i..]))
            }
            Some(b'*') => (0..=s.len())
                .take_while(|&i| i == 0 || s[i - 1] != b'/')
                .any(|i| inner(&p[1..], &s[i..])),
            Some(c) => s.first() == Some(c) && inner(&p[1..], &s[1..]),
        }
    }
    inner(pattern.as_bytes(), path.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_registry_loads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../compat/mapping/registry.toml");
        let reg = Registry::load(&path).expect("compat/mapping/registry.toml");
        for id in ["ui.styles", "ui.frontend", "ui.read-models"] {
            assert!(reg.components.iter().any(|c| c.id == id), "{id} missing");
        }
    }

    #[test]
    fn globbing() {
        assert!(glob_match(
            "app/Policies/*.php",
            "app/Policies/ProjectPolicy.php"
        ));
        assert!(!glob_match("app/Policies/*.php", "app/Policies/sub/X.php"));
        assert!(glob_match(
            "tests/**/Project*Test.php",
            "tests/Feature/Api/ProjectFooTest.php"
        ));
        assert!(glob_match("routes/api.php", "routes/api.php"));
        assert!(glob_match("database/**", "database/migrations/x.php"));
    }

    #[test]
    fn queries_registry() {
        let reg = Registry::parse(
            r#"
schema_version = 1
[[component]]
id = "api.projects"
title = "Projects"
status = "implemented"
upstream.paths = ["app/Http/Controllers/Api/ProjectController.php"]
upstream.tables = ["projects", "environments"]
upstream.routes = ["GET api/v1/projects*"]
rust.paths = ["crates/coolify-api/src/routes/projects.rs"]
rust.packages = ["coolify-api"]
"#,
        )
        .unwrap();
        let paths = vec!["app/Http/Controllers/Api/ProjectController.php".to_string()];
        let tables = vec!["projects".to_string(), "servers".to_string()];
        let routes = vec!["GET api/v1/projects/{uuid}".to_string()];
        let symbols = BTreeSet::new();
        let m = reg.affected(&ChangeQuery {
            paths: &paths,
            symbols: &symbols,
            tables: &tables,
            routes: &routes,
        });
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].reasons.len(), 3);
        assert_eq!(m[0].rust_packages, vec!["coolify-api"]);
    }

    #[test]
    fn rejects_duplicates_and_versions() {
        assert!(matches!(
            Registry::parse("schema_version = 2"),
            Err(RegistryError::Version(2))
        ));
        let dup = "schema_version = 1\n[[component]]\nid='a'\ntitle='a'\nstatus='planned'\n[[component]]\nid='a'\ntitle='b'\nstatus='planned'\n";
        assert!(matches!(
            Registry::parse(dup),
            Err(RegistryError::Duplicate(_))
        ));
    }
}
