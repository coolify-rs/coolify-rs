//! `index.json`: every plugin of the registry with its versions, as the registry's CI publishes it.
//!
//! Each version names the source commit and builder it is reproducibly built from, the component's
//! SHA-256 and download URL, and the full manifest. A version can be *yanked* (new installs avoid it) and a
//! plugin *revoked* (instances switch it off). A manifest this coolify-rs cannot run (a newer plugin
//! interface, or a capability it does not know) does not invalidate the index; that version is only shown
//! as incompatible, so older instances keep receiving revocations.

use std::cmp::Ordering;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::manifest::Manifest;

/// The index format this coolify-rs reads.
pub const FORMAT: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub format: u32,
    pub generated_at: DateTime<Utc>,
    pub plugins: Vec<Entry>,
}

/// One plugin: `plugins/<publisher>/<name>.toml` in the registry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked: Option<Revoked>,
    pub versions: Vec<Release>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revoked {
    pub reason: String,
}

/// One published version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    pub version: String,
    /// The source repository, `https://github.com/<owner>/<repo>`.
    pub source: String,
    /// The exact commit the component is built from (40 hex digits).
    pub commit: String,
    /// The language toolchain, such as `rust 1.99.1`.
    pub toolchain: String,
    /// The builder image, pinned by digest.
    pub builder: String,
    pub build: Build,
    /// SHA-256 of the component, lowercase hex.
    pub sha256: String,
    /// Where the component is published.
    pub url: String,
    #[serde(default)]
    pub yanked: bool,
    /// `plugin.toml` of this version, capabilities included.
    pub manifest: serde_json::Value,
}

/// How the registry's CI rebuilds the component in the builder image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    /// Directory in the repository to run `command` in.
    #[serde(default = "dot")]
    pub dir: String,
    pub command: String,
    /// The component the command produces, relative to `dir`.
    pub artifact: String,
}

fn dot() -> String {
    ".".into()
}

static COMMIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9a-f]{40}$").unwrap_or_else(|_| unreachable!()));
static SHA256: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9a-f]{64}$").unwrap_or_else(|_| unreachable!()));
static PINNED_IMAGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-z0-9][a-z0-9./_:-]*@sha256:[0-9a-f]{64}$").unwrap_or_else(|_| unreachable!())
});
static GITHUB_REPO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^https://github\.com/([A-Za-z0-9](?:[A-Za-z0-9-]{0,38}))/([A-Za-z0-9._-]{1,100})$")
        .unwrap_or_else(|_| unreachable!())
});

impl Index {
    pub fn parse(bytes: &[u8]) -> Result<Index, String> {
        let index: Index = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if index.format != FORMAT {
            return Err(format!(
                "index format {} is not supported (this coolify-rs reads format {FORMAT})",
                index.format
            ));
        }
        index.validate()?;
        Ok(index)
    }

    /// The canonical bytes: what the registry's CI signs.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = serde_json::to_vec_pretty(self).unwrap_or_default();
        out.push(b'\n');
        out
    }

    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.plugins.iter().find(|e| e.id == id)
    }

    /// Checks what every coolify-rs must agree on. Manifests are checked per version, by
    /// [`Release::manifest`].
    pub fn validate(&self) -> Result<(), String> {
        for (i, e) in self.plugins.iter().enumerate() {
            if self.plugins[..i].iter().any(|o| o.id == e.id) {
                return Err(format!("{} is listed twice", e.id));
            }
            e.validate()?;
        }
        Ok(())
    }
}

impl Entry {
    pub fn validate(&self) -> Result<(), String> {
        let id = &self.id;
        crate::manifest::check_id(id).map_err(|e| format!("{id}: {e}"))?;
        if let Some(r) = &self.revoked
            && r.reason.trim().is_empty()
        {
            return Err(format!("{id}: a revocation needs a reason"));
        }
        for (i, r) in self.versions.iter().enumerate() {
            let v = &r.version;
            let bad = |what: &str| Err(format!("{id} {v}: {what}"));
            if self.versions[..i].iter().any(|o| o.version == *v) {
                return bad("the version is listed twice");
            }
            if parse_version(v).is_none() {
                return bad("the version must be a semantic version");
            }
            if !COMMIT.is_match(&r.commit) {
                return bad("commit must be 40 lowercase hex digits");
            }
            if !SHA256.is_match(&r.sha256) {
                return bad("sha256 must be 64 lowercase hex digits");
            }
            if github_owner(&r.source).is_none() {
                return bad("source must be https://github.com/<owner>/<repo>");
            }
            if reqwest::Url::parse(&r.url).is_err() {
                return bad("url must be a URL");
            }
            if !PINNED_IMAGE.is_match(&r.builder) {
                return bad("builder must be an image pinned by digest (name@sha256:...)");
            }
            if r.toolchain.trim().is_empty() || r.build.command.trim().is_empty() {
                return bad("toolchain and build.command are required");
            }
            for path in [&r.build.dir, &r.build.artifact] {
                if path.is_empty() || path.starts_with('/') || path.split('/').any(|p| p == "..") {
                    return bad("build paths must be relative and stay inside the repository");
                }
            }
            if !r.manifest.is_object() {
                return bad("manifest must be a table");
            }
            // The manifest's identity must be this entry's, even when this coolify-rs cannot run it.
            let field = |k: &str| r.manifest.get(k).and_then(|v| v.as_str());
            if field("id") != Some(id.as_str()) || field("version") != Some(v.as_str()) {
                return bad("the manifest's id and version must match the entry");
            }
        }
        Ok(())
    }

    /// The newest version that is not yanked and that this coolify-rs can run. Pre-releases count only when
    /// there is nothing else.
    pub fn latest(&self) -> Option<(&Release, Manifest)> {
        let mut candidates: Vec<(&Release, Manifest)> = self
            .versions
            .iter()
            .filter(|r| !r.yanked)
            .filter_map(|r| Some((r, r.manifest().ok()?)))
            .collect();
        candidates.sort_by(|a, b| compare_versions(&b.0.version, &a.0.version));
        let stable = candidates
            .iter()
            .position(|(r, _)| !r.version.contains('-'));
        match stable {
            Some(i) => Some(candidates.swap_remove(i)),
            None => candidates.into_iter().next(),
        }
    }

    pub fn find(&self, version: &str) -> Option<&Release> {
        self.versions.iter().find(|r| r.version == version)
    }

    /// Versions, newest first.
    pub fn sorted(&self) -> Vec<&Release> {
        let mut v: Vec<&Release> = self.versions.iter().collect();
        v.sort_by(|a, b| compare_versions(&b.version, &a.version));
        v
    }
}

impl Release {
    /// The manifest, if this coolify-rs can run it.
    pub fn manifest(&self) -> Result<Manifest, String> {
        let m: Manifest = serde_json::from_value(self.manifest.clone())
            .map_err(|e| format!("this coolify-rs cannot read the manifest: {e}"))?;
        m.validate().map_err(|e| e.to_string())?;
        Ok(m)
    }
}

/// The owner of a `https://github.com/<owner>/<repo>` URL.
pub fn github_owner(url: &str) -> Option<&str> {
    let caps = GITHUB_REPO.captures(url.trim_end_matches(".git"))?;
    Some(caps.get(1)?.as_str())
}

type Version<'a> = (u64, u64, u64, Option<&'a str>);

fn parse_version(v: &str) -> Option<Version<'_>> {
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let mut it = core.split('.').map(|n| n.parse::<u64>().ok());
    let out = (it.next()??, it.next()??, it.next()??, pre);
    if it.next().is_some() {
        return None;
    }
    Some(out)
}

/// Semantic version precedence. Versions that do not parse sort first.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    match (parse_version(a), parse_version(b)) {
        (Some(x), Some(y)) => {
            (x.0, x.1, x.2)
                .cmp(&(y.0, y.1, y.2))
                .then_with(|| match (x.3, y.3) {
                    (None, None) => Ordering::Equal,
                    (None, Some(_)) => Ordering::Greater,
                    (Some(_), None) => Ordering::Less,
                    (Some(p), Some(q)) => compare_pre(p, q),
                })
        }
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => a.cmp(b),
    }
}

fn compare_pre(a: &str, b: &str) -> Ordering {
    let mut x = a.split('.');
    let mut y = b.split('.');
    loop {
        match (x.next(), y.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(p), Some(q)) => {
                let o = match (p.parse::<u64>(), q.parse::<u64>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => p.cmp(q),
                };
                if o != Ordering::Equal {
                    return o;
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn release(id: &str, version: &str, caps: serde_json::Value) -> Release {
        Release {
            version: version.into(),
            source: "https://github.com/acme/deploy-stats".into(),
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            toolchain: "rust 1.99.1".into(),
            builder: format!("docker.io/library/rust:1.99.1@sha256:{}", "a".repeat(64)),
            build: Build {
                dir: ".".into(),
                command: "cargo build --release --target wasm32-wasip2 --locked".into(),
                artifact: "target/wasm32-wasip2/release/deploy_stats.wasm".into(),
            },
            sha256: "b".repeat(64),
            url: "https://github.com/acme/deploy-stats/releases/download/v1/deploy_stats.wasm"
                .into(),
            yanked: false,
            manifest: serde_json::json!({
                "id": id,
                "name": "Deploy stats",
                "version": version,
                "description": "Counts deployments.",
                "license": "MIT",
                "api": "0.1",
                "capabilities": caps,
            }),
        }
    }

    fn index(versions: Vec<Release>) -> Index {
        Index {
            format: FORMAT,
            generated_at: "2026-11-06T00:00:00Z".parse().unwrap(),
            plugins: vec![Entry {
                id: "acme/deploy-stats".into(),
                revoked: None,
                versions,
            }],
        }
    }

    #[test]
    fn parses_what_it_writes() {
        let i = index(vec![release(
            "acme/deploy-stats",
            "1.0.0",
            serde_json::json!({"storage": true}),
        )]);
        let parsed = Index::parse(&i.to_bytes()).unwrap();
        assert_eq!(parsed, i);
        let m = parsed.plugins[0].versions[0].manifest().unwrap();
        assert!(m.capabilities.storage);
    }

    #[test]
    fn refuses_invalid_indexes() {
        let ok = release("acme/deploy-stats", "1.0.0", serde_json::json!({}));
        type Change = Box<dyn Fn(&mut Release)>;
        let cases: Vec<(Change, &str)> = vec![
            (Box::new(|r| r.commit = "main".into()), "commit"),
            (Box::new(|r| r.sha256 = "B".repeat(64)), "sha256"),
            (Box::new(|r| r.url = "example.com/x.wasm".into()), "url"),
            (
                Box::new(|r| r.builder = "rust:latest".into()),
                "pinned by digest",
            ),
            (
                Box::new(|r| r.source = "https://gitlab.com/acme/x".into()),
                "source",
            ),
            (
                Box::new(|r| r.build.artifact = "../x.wasm".into()),
                "build paths",
            ),
            (
                Box::new(|r| r.manifest["id"] = "evil/other".into()),
                "must match the entry",
            ),
            (Box::new(|r| r.version = "1.0".into()), "semantic version"),
        ];
        for (change, want) in cases {
            let mut r = ok.clone();
            change(&mut r);
            let err = Index::parse(&index(vec![r]).to_bytes()).unwrap_err();
            assert!(err.contains(want), "{want}: {err}");
        }
        let twice = index(vec![ok.clone(), ok.clone()]);
        assert!(
            Index::parse(&twice.to_bytes())
                .unwrap_err()
                .contains("twice")
        );
        let mut future = index(vec![ok]);
        future.format = 2;
        assert!(
            Index::parse(&future.to_bytes())
                .unwrap_err()
                .contains("format 2")
        );
        assert!(Index::parse(b"{\"format\":1}").is_err());
    }

    #[test]
    fn incompatible_versions_do_not_break_the_index() {
        let mut newer = release("acme/deploy-stats", "2.0.0", serde_json::json!({}));
        newer.manifest["api"] = "0.2".into();
        let mut unknown_cap = release("acme/deploy-stats", "1.5.0", serde_json::json!({}));
        unknown_cap.manifest["capabilities"]["servers"] = true.into();
        let i = Index::parse(
            &index(vec![
                release("acme/deploy-stats", "1.0.0", serde_json::json!({})),
                newer,
                unknown_cap,
            ])
            .to_bytes(),
        )
        .unwrap();
        let e = &i.plugins[0];
        assert!(e.find("2.0.0").unwrap().manifest().is_err());
        assert!(e.find("1.5.0").unwrap().manifest().is_err());
        assert_eq!(e.latest().unwrap().0.version, "1.0.0");
    }

    #[test]
    fn latest_skips_yanked_and_prereleases() {
        let mut yanked = release("acme/deploy-stats", "1.2.0", serde_json::json!({}));
        yanked.yanked = true;
        let e = Entry {
            id: "acme/deploy-stats".into(),
            revoked: None,
            versions: vec![
                release("acme/deploy-stats", "1.0.0", serde_json::json!({})),
                release("acme/deploy-stats", "1.10.0-beta.1", serde_json::json!({})),
                yanked,
                release("acme/deploy-stats", "1.1.0", serde_json::json!({})),
            ],
        };
        assert_eq!(e.latest().unwrap().0.version, "1.1.0");
        let only_pre = Entry {
            versions: vec![release(
                "acme/deploy-stats",
                "0.1.0-rc.1",
                serde_json::json!({}),
            )],
            ..e
        };
        assert_eq!(only_pre.latest().unwrap().0.version, "0.1.0-rc.1");
    }

    #[test]
    fn orders_versions_semantically() {
        let mut v = vec![
            "1.10.0",
            "1.2.0",
            "1.2.0-rc.1",
            "1.2.0-beta.2",
            "1.2.0-beta.10",
            "0.9.9",
        ];
        v.sort_by(|a, b| compare_versions(a, b));
        assert_eq!(
            v,
            [
                "0.9.9",
                "1.2.0-beta.2",
                "1.2.0-beta.10",
                "1.2.0-rc.1",
                "1.2.0",
                "1.10.0"
            ]
        );
    }

    #[test]
    fn reads_github_owners() {
        assert_eq!(
            github_owner("https://github.com/acme/deploy-stats"),
            Some("acme")
        );
        assert_eq!(
            github_owner("https://github.com/acme/deploy-stats.git"),
            Some("acme")
        );
        assert_eq!(github_owner("https://github.com/acme"), None);
        assert_eq!(github_owner("https://github.com.evil/acme/x"), None);
        assert_eq!(github_owner("http://github.com/acme/x"), None);
    }
}
