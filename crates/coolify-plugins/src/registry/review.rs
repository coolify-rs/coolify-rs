//! The registry's pull request checks that need no build (ADR 0010, section 7): metadata, the capability
//! difference to the previous version, immutability of published versions, and the review tier. The CI
//! runs them with `coolify-rs plugin review <head> --base <main>` and then rebuilds every new version.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use super::index::{Build, Entry, Release, compare_versions, github_owner};
use crate::manifest::{Capabilities, Manifest};

/// OSI-approved licenses accepted for plugins, by SPDX id.
pub const OSI_LICENSES: &[&str] = &[
    "0BSD",
    "AFL-3.0",
    "AGPL-3.0-only",
    "AGPL-3.0-or-later",
    "Apache-2.0",
    "Artistic-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "BSL-1.0",
    "CDDL-1.0",
    "EPL-2.0",
    "EUPL-1.2",
    "GPL-2.0-only",
    "GPL-2.0-or-later",
    "GPL-3.0-only",
    "GPL-3.0-or-later",
    "ISC",
    "LGPL-2.1-only",
    "LGPL-2.1-or-later",
    "LGPL-3.0-only",
    "LGPL-3.0-or-later",
    "MIT",
    "MIT-0",
    "MPL-2.0",
    "MulanPSL-2.0",
    "NCSA",
    "OSL-3.0",
    "PostgreSQL",
    "UPL-1.0",
    "Unlicense",
    "Zlib",
];
/// License exceptions allowed after `WITH`.
const EXCEPTIONS: &[&str] = &["LLVM-exception", "Classpath-exception-2.0"];

/// Publishers that cannot be used: `catalog` would collide with `/api/v1/plugins/catalog/...`.
const RESERVED_PUBLISHERS: &[&str] = &["catalog"];

static AFFILIATION: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)\b(official|officially|endorsed|certified|sponsored|affiliated|partnered)\b",
        r"(?i)\b(by|from)\s+(the\s+)?coolify(-rs)?\b",
        r"(?i)\bcoolify(-rs)?\s+(team|labs|maintainers|developers)\b",
    ]
    .iter()
    .map(|p| Regex::new(p).unwrap_or_else(|_| unreachable!()))
    .collect()
});

/// Who has to approve a change (the table in ADR 0010, section 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Yanking, revoking: one maintainer.
    OneMaintainer,
    /// A new version with the same or fewer capabilities: one maintainer checks the source diff.
    OneMaintainerSourceDiff,
    /// A new plugin: one maintainer reads the source and confirms the capabilities match its purpose.
    OneMaintainerFullReview,
    /// More capabilities or any new `http` host: two maintainers.
    TwoMaintainers,
}

impl Tier {
    pub fn describe(self) -> &'static str {
        match self {
            Tier::OneMaintainer => "one maintainer",
            Tier::OneMaintainerSourceDiff => {
                "one maintainer checks the source diff since the approved version"
            }
            Tier::OneMaintainerFullReview => {
                "one maintainer reads the source and confirms the capabilities match the stated purpose"
            }
            Tier::TwoMaintainers => "two maintainers (more capabilities, or a new http host)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub id: String,
    pub version: Option<String>,
    /// `new_plugin`, `new_version`, `yanked`, `unyanked`, `revoked`, `unrevoked`.
    pub kind: &'static str,
    /// The version the capabilities are compared with.
    pub previous: Option<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub tier: Tier,
}

/// A version the CI must rebuild and run `coolify-rs plugin check` on.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BuildJob {
    pub id: String,
    pub version: String,
    pub source: String,
    pub commit: String,
    pub toolchain: String,
    pub builder: String,
    pub build: Build,
    pub sha256: String,
    pub url: String,
    /// `plugin.toml` of the version, for `coolify-rs plugin check`.
    pub manifest_toml: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Review {
    pub changes: Vec<Change>,
    pub builds: Vec<BuildJob>,
    pub errors: Vec<String>,
}

impl Review {
    pub fn tier(&self) -> Option<Tier> {
        self.changes.iter().map(|c| c.tier).max()
    }

    /// A Markdown summary for the pull request.
    pub fn markdown(&self) -> String {
        let mut out = String::from("## Plugin registry review\n\n");
        if self.changes.is_empty() {
            out.push_str("No plugin changes.\n");
        }
        for c in &self.changes {
            let version = c.version.as_deref().unwrap_or("");
            out.push_str(&format!("### `{}` {version} ({})\n\n", c.id, c.kind));
            if let Some(p) = &c.previous {
                out.push_str(&format!("Compared with {p}.\n\n"));
            }
            for a in &c.added {
                out.push_str(&format!("- **added** {a}\n"));
            }
            for r in &c.removed {
                out.push_str(&format!("- removed {r}\n"));
            }
            out.push_str(&format!("\nReview: {}.\n\n", c.tier.describe()));
        }
        if let Some(t) = self.tier() {
            out.push_str(&format!("**Required review: {}.**\n\n", t.describe()));
        }
        if !self.errors.is_empty() {
            out.push_str("### Problems\n\n");
            for e in &self.errors {
                out.push_str(&format!("- {e}\n"));
            }
        }
        out
    }
}

/// The capability list as single items, so additions are visible one by one.
pub fn capability_items(c: &Capabilities) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for e in &c.events {
        out.insert(format!("events: {e}"));
    }
    if c.storage {
        out.insert("storage".into());
    }
    if c.routes {
        out.insert("routes".into());
    }
    for h in &c.http {
        out.insert(format!("http: {h}"));
    }
    out
}

/// Whether `license` is an SPDX expression of OSI-approved licenses only.
pub fn license_allowed(license: &str) -> bool {
    let text = license.replace(['(', ')'], " ");
    let mut tokens = text.split_whitespace().peekable();
    let mut seen = false;
    while let Some(t) = tokens.next() {
        match t {
            "OR" | "AND" => {}
            "WITH" => match tokens.next() {
                Some(e) if EXCEPTIONS.contains(&e) => {}
                _ => return false,
            },
            id => {
                if !OSI_LICENSES.contains(&id) {
                    return false;
                }
                seen = true;
            }
        }
    }
    seen
}

/// A phrase claiming affiliation with coolify-rs or Coolify, if any.
pub fn claims_affiliation(text: &str) -> Option<String> {
    AFFILIATION
        .iter()
        .find_map(|re| re.find(text).map(|m| m.as_str().to_owned()))
}

/// Edit distance (insertions, deletions, substitutions).
pub fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// How an id looks: characters that are easy to confuse become one, separators disappear.
fn skeleton(id: &str) -> String {
    id.to_lowercase()
        .replace("rn", "m")
        .replace("vv", "w")
        .chars()
        .filter_map(|c| match c {
            '-' | '_' | '.' => None,
            '0' => Some('o'),
            '1' | 'i' => Some('l'),
            '3' => Some('e'),
            '5' => Some('s'),
            c => Some(c),
        })
        .collect()
}

/// An existing id of another publisher that `id` looks like.
pub fn typosquats<'a>(id: &str, existing: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let publisher = id.split('/').next().unwrap_or_default();
    existing.into_iter().find(|other| {
        let other_publisher = other.split('/').next().unwrap_or_default();
        if *other == id || other_publisher == publisher {
            return false;
        }
        let limit = if id.len().min(other.len()) < 12 { 1 } else { 2 };
        skeleton(id) == skeleton(other) || levenshtein(id, other) <= limit
    })
}

fn manifest_toml(m: &Manifest) -> String {
    toml::to_string(m).unwrap_or_default()
}

/// Compares the registry of a pull request (`head`) with the main branch (`base`).
pub fn review(base: &[Entry], head: &[Entry]) -> Review {
    let mut out = Review::default();
    let base_ids: Vec<&str> = base.iter().map(|e| e.id.as_str()).collect();

    for old in base {
        if !head.iter().any(|e| e.id == old.id) {
            out.errors.push(format!(
                "{}: plugins are never removed from the registry; revoke it instead",
                old.id
            ));
        }
    }

    for entry in head {
        let id = &entry.id;
        let before = base.iter().find(|e| e.id == *id);
        let publisher = id.split('/').next().unwrap_or_default();

        if before.is_none() {
            if RESERVED_PUBLISHERS.contains(&publisher) {
                out.errors.push(format!(
                    "{id}: the publisher name {publisher:?} is reserved"
                ));
            }
            let others = base_ids
                .iter()
                .copied()
                .chain(head.iter().map(|e| e.id.as_str()));
            if let Some(similar) = typosquats(id, others) {
                out.errors.push(format!(
                    "{id}: the id looks too much like {similar} (possible typosquatting); a maintainer must confirm"
                ));
            }
        }

        // Revocation.
        let was_revoked = before.and_then(|b| b.revoked.as_ref()).is_some();
        match (was_revoked, entry.revoked.is_some()) {
            (false, true) => out
                .changes
                .push(simple(id, None, "revoked", Tier::OneMaintainer)),
            (true, false) => out
                .changes
                .push(simple(id, None, "unrevoked", Tier::TwoMaintainers)),
            _ => {}
        }

        if let Some(b) = before {
            for old in &b.versions {
                match entry.find(&old.version) {
                    None => out.errors.push(format!(
                        "{id} {}: published versions are never removed; yank it instead",
                        old.version
                    )),
                    Some(new) => {
                        let mut same = new.clone();
                        same.yanked = old.yanked;
                        if same != *old {
                            out.errors.push(format!(
                                "{id} {}: a published version cannot change; only yanked can",
                                old.version
                            ));
                        } else if new.yanked != old.yanked {
                            let kind = if new.yanked { "yanked" } else { "unyanked" };
                            out.changes.push(simple(
                                id,
                                Some(&new.version),
                                kind,
                                Tier::OneMaintainer,
                            ));
                        }
                    }
                }
            }
        }

        let mut new_versions: Vec<&Release> = entry
            .versions
            .iter()
            .filter(|r| before.is_none_or(|b| b.find(&r.version).is_none()))
            .collect();
        new_versions.sort_by(|a, b| compare_versions(&a.version, &b.version));
        for r in new_versions {
            review_version(&mut out, entry, r, before.is_none(), publisher);
        }
    }
    out
}

fn simple(id: &str, version: Option<&str>, kind: &'static str, tier: Tier) -> Change {
    Change {
        id: id.to_owned(),
        version: version.map(str::to_owned),
        kind,
        previous: None,
        added: vec![],
        removed: vec![],
        tier,
    }
}

fn review_version(out: &mut Review, entry: &Entry, r: &Release, new_plugin: bool, publisher: &str) {
    let id = &entry.id;
    let v = &r.version;
    let manifest = match r.manifest() {
        Ok(m) => m,
        Err(e) => {
            out.errors.push(format!("{id} {v}: {e}"));
            return;
        }
    };
    if !license_allowed(&manifest.license) {
        out.errors.push(format!(
            "{id} {v}: license {:?} is not an OSI-approved SPDX expression we accept",
            manifest.license
        ));
    }
    match github_owner(&r.source) {
        Some(owner) if owner.eq_ignore_ascii_case(publisher) => {}
        owner => out.errors.push(format!(
            "{id} {v}: the publisher {publisher:?} must own the source repository (owner {:?})",
            owner.unwrap_or_default()
        )),
    }
    if publisher != "coolify-rs" {
        for text in [&manifest.name, &manifest.description] {
            if let Some(phrase) = claims_affiliation(text) {
                out.errors.push(format!(
                    "{id} {v}: {phrase:?} claims an affiliation with coolify-rs or Coolify"
                ));
            }
        }
    }

    // Compare with the closest earlier version that this coolify-rs can read.
    let previous = entry
        .sorted()
        .into_iter()
        .filter(|o| compare_versions(&o.version, v).is_lt())
        .find_map(|o| Some((o.version.clone(), o.manifest().ok()?)));
    let now = capability_items(&manifest.capabilities);
    let (prev_version, then) = match &previous {
        Some((pv, m)) => (Some(pv.clone()), capability_items(&m.capabilities)),
        None => (None, BTreeSet::new()),
    };
    let added: Vec<String> = now.difference(&then).cloned().collect();
    let removed: Vec<String> = then.difference(&now).cloned().collect();
    let new_host = added.iter().any(|a| a.starts_with("http: "));
    let tier = if new_host || (previous.is_some() && !added.is_empty()) {
        Tier::TwoMaintainers
    } else if new_plugin || previous.is_none() {
        Tier::OneMaintainerFullReview
    } else {
        Tier::OneMaintainerSourceDiff
    };
    out.changes.push(Change {
        id: id.clone(),
        version: Some(v.clone()),
        kind: if new_plugin {
            "new_plugin"
        } else {
            "new_version"
        },
        previous: prev_version,
        added,
        removed,
        tier,
    });
    out.builds.push(BuildJob {
        id: id.clone(),
        version: v.clone(),
        source: r.source.clone(),
        commit: r.commit.clone(),
        toolchain: r.toolchain.clone(),
        builder: r.builder.clone(),
        build: r.build.clone(),
        sha256: r.sha256.clone(),
        url: r.url.clone(),
        manifest_toml: manifest_toml(&manifest),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::index::tests::release;
    use serde_json::json;

    fn entry(id: &str, versions: Vec<Release>) -> Entry {
        Entry {
            id: id.into(),
            revoked: None,
            versions,
        }
    }

    #[test]
    fn finds_typosquatting() {
        let existing = ["coolify-rs/deploy-stats", "acme/slack-notify"];
        let it = || existing.iter().copied();
        assert_eq!(
            typosquats("co0lify-rs/deploy-stats", it()),
            Some("coolify-rs/deploy-stats")
        );
        assert_eq!(
            typosquats("coolifyrs/deploy-stats", it()),
            Some("coolify-rs/deploy-stats")
        );
        assert_eq!(
            typosquats("coolify-rs/dep1oy-stats", it()),
            None,
            "same publisher"
        );
        assert_eq!(
            typosquats("evil/deploy-stats", it()),
            None,
            "a different publisher may reuse a name"
        );
        assert_eq!(
            typosquats("acrne/slack-notify", it()),
            Some("acme/slack-notify")
        );
        assert_eq!(
            typosquats("acme1/slack-notify", it()),
            Some("acme/slack-notify")
        );
        assert_eq!(typosquats("bob/slack-notify", it()), None);
        assert_eq!(typosquats("acme/slack-notify", it()), None, "itself");
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
    }

    #[test]
    fn checks_licenses_and_affiliation_claims() {
        for ok in [
            "MIT",
            "Apache-2.0 OR MIT",
            "(MIT AND Zlib)",
            "Apache-2.0 WITH LLVM-exception",
        ] {
            assert!(license_allowed(ok), "{ok}");
        }
        for bad in [
            "",
            "Proprietary",
            "MIT OR SSPL-1.0",
            "BUSL-1.1",
            "MIT WITH Commons-Clause",
            "CC-BY-4.0",
        ] {
            assert!(!license_allowed(bad), "{bad}");
        }
        assert!(claims_affiliation("The official Slack plugin").is_some());
        assert!(claims_affiliation("Made by the Coolify team").is_some());
        assert!(claims_affiliation("Brought to you by coolify-rs").is_some());
        assert!(claims_affiliation("Sends Coolify deployments to Slack").is_none());
    }

    #[test]
    fn diffs_capabilities_and_picks_the_tier() {
        let id = "acme/deploy-stats";
        let v1 = release(
            id,
            "1.0.0",
            json!({"events": ["deployment.succeeded"], "storage": true}),
        );
        let base = vec![entry(id, vec![v1.clone()])];

        // Same capabilities: one maintainer, source diff.
        let same = release(
            id,
            "1.1.0",
            json!({"events": ["deployment.succeeded"], "storage": true}),
        );
        let r = review(&base, &[entry(id, vec![v1.clone(), same])]);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.changes[0].tier, Tier::OneMaintainerSourceDiff);
        assert_eq!(r.builds.len(), 1);
        assert!(r.builds[0].manifest_toml.contains("[capabilities]"));

        // A new host: two maintainers, and the diff names it.
        let more = release(
            id,
            "2.0.0",
            json!({"events": ["deployment.succeeded"], "http": ["hooks.example.com"]}),
        );
        let r = review(&base, &[entry(id, vec![v1.clone(), more])]);
        let c = &r.changes[0];
        assert_eq!(c.tier, Tier::TwoMaintainers);
        assert_eq!(c.previous.as_deref(), Some("1.0.0"));
        assert_eq!(c.added, ["http: hooks.example.com"]);
        assert_eq!(c.removed, ["storage"]);
        assert!(r.markdown().contains("**added** http: hooks.example.com"));

        // A new plugin: full review.
        let r = review(&[], &base);
        assert_eq!(r.changes[0].kind, "new_plugin");
        assert_eq!(r.changes[0].tier, Tier::OneMaintainerFullReview);
    }

    #[test]
    fn published_versions_are_immutable() {
        let id = "acme/deploy-stats";
        let v1 = release(id, "1.0.0", json!({}));
        let base = vec![entry(id, vec![v1.clone()])];

        let mut moved = v1.clone();
        moved.commit = "f".repeat(40);
        let r = review(&base, &[entry(id, vec![moved])]);
        assert!(r.errors[0].contains("cannot change"), "{:?}", r.errors);

        let mut yanked = v1.clone();
        yanked.yanked = true;
        let r = review(&base, &[entry(id, vec![yanked])]);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.changes[0].kind, "yanked");
        assert!(r.builds.is_empty());

        let r = review(&base, &[entry(id, vec![])]);
        assert!(r.errors[0].contains("never removed"), "{:?}", r.errors);
        let r = review(&base, &[]);
        assert!(r.errors[0].contains("revoke it instead"), "{:?}", r.errors);
    }

    #[test]
    fn checks_publisher_ownership_and_metadata() {
        let mut stolen = release("acme/deploy-stats", "1.0.0", json!({}));
        stolen.source = "https://github.com/mallory/deploy-stats".into();
        stolen.manifest["license"] = "Proprietary".into();
        stolen.manifest["description"] = "The official deploy stats plugin.".into();
        let r = review(&[], &[entry("acme/deploy-stats", vec![stolen])]);
        let all = r.errors.join("\n");
        assert!(all.contains("must own the source repository"), "{all}");
        assert!(all.contains("OSI-approved"), "{all}");
        assert!(all.contains("affiliation"), "{all}");

        let r = review(
            &[],
            &[entry(
                "catalog/install",
                vec![release("catalog/install", "1.0.0", json!({}))],
            )],
        );
        assert!(r.errors.join("\n").contains("reserved"), "{:?}", r.errors);
    }
}
