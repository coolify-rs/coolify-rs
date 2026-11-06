//! `plugin.toml`: who the plugin is and the capabilities it asks for (ADR 0010, section 3).

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::PluginError;

/// The plugin interface version this host implements (`coolify-rs:plugin@0.1.x`).
pub const API_VERSION: &str = "0.1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// `publisher/name`: the publisher is the GitHub user or organization owning the source.
    pub id: String,
    pub name: String,
    /// Semantic version.
    pub version: String,
    pub description: String,
    /// SPDX license expression.
    pub license: String,
    /// Where the source is.
    #[serde(default)]
    pub source: Option<String>,
    /// The plugin interface version the component targets.
    pub api: String,
    #[serde(default)]
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    /// Event kinds to receive (`coolify_domain::plugin_events::KINDS`).
    #[serde(default)]
    pub events: Vec<String>,
    /// A key-value store per team.
    #[serde(default)]
    pub storage: bool,
    /// API routes under `/api/v1/ext/<id>/`.
    #[serde(default)]
    pub routes: bool,
    /// Host names the plugin may send HTTPS requests to. Exact names, no wildcards or addresses.
    #[serde(default)]
    pub http: Vec<String>,
    /// Resource kinds of the team the plugin may read (`RESOURCE_KINDS`), as the API shows them to a token
    /// without `read:sensitive` (ADR 0011).
    #[serde(default)]
    pub resources: Vec<String>,
    /// Queue deployments of the team's applications. Each team must also grant it when enabling the plugin
    /// (ADR 0011).
    #[serde(default)]
    pub deploy: bool,
    /// Panels the web UI shows in its slots (ADR 0013). Each is a page of the plugin's own routes, so
    /// `ui` needs `routes`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ui: Vec<Panel>,
}

/// What `resources` can list, in the order of the WIT `kind` enum. `projects` includes their environments.
pub const RESOURCE_KINDS: &[&str] = &[
    "projects",
    "servers",
    "applications",
    "services",
    "databases",
];

/// Where the web UI can show a plugin's panel.
pub const UI_SLOTS: &[&str] = &["dashboard", "application", "server", "settings"];

/// One `[[capabilities.ui]]` entry: a panel in a slot of the web UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Panel {
    /// One of [`UI_SLOTS`].
    pub slot: String,
    /// The panel's heading.
    pub title: String,
    /// A `GET` path of the plugin's routes that answers the panel's HTML, such as `/panel`.
    pub path: String,
}

/// `/segment/segment...`: letters, digits and `._~-`, no empty, `.` or `..` segments, no query.
static PANEL_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:/[A-Za-z0-9_~-][A-Za-z0-9._~-]{0,63}){1,8}$")
        .unwrap_or_else(|_| unreachable!())
});

static ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-z0-9](?:[a-z0-9-]{0,37}[a-z0-9])?/[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$")
        .unwrap_or_else(|_| unreachable!())
});
static VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(0|[1-9]\d{0,8})\.(0|[1-9]\d{0,8})\.(0|[1-9]\d{0,8})(?:-[0-9A-Za-z.-]{1,32})?$")
        .unwrap_or_else(|_| unreachable!())
});
static HOST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z](?:[a-z0-9-]{0,61}[a-z0-9])?$")
        .unwrap_or_else(|_| unreachable!())
});

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, PluginError> {
        let m: Manifest =
            toml::from_str(text).map_err(|e| PluginError::Manifest(e.message().to_owned()))?;
        m.validate()?;
        Ok(m)
    }

    /// Checks every field; [`Manifest::parse`] does this already.
    pub fn validate(&self) -> Result<(), PluginError> {
        let bad = |msg: String| Err(PluginError::Manifest(msg));
        if !ID.is_match(&self.id) {
            return bad(format!(
                "id must be publisher/name in lowercase letters, digits and dashes, not {:?}",
                self.id
            ));
        }
        if !VERSION.is_match(&self.version) {
            return bad(format!(
                "version must be a semantic version, not {:?}",
                self.version
            ));
        }
        if self.name.trim().is_empty() || self.name.chars().count() > 64 {
            return bad("name must be 1 to 64 characters".into());
        }
        if self.description.trim().is_empty() || self.description.chars().count() > 280 {
            return bad("description must be 1 to 280 characters".into());
        }
        if self.license.trim().is_empty() || self.license.len() > 64 {
            return bad("license must be an SPDX expression".into());
        }
        if let Some(source) = &self.source
            && !source.starts_with("https://")
        {
            return bad("source must be an https URL".into());
        }
        if self.api != API_VERSION {
            return bad(format!(
                "this coolify-rs implements plugin interface {API_VERSION}, not {:?}",
                self.api
            ));
        }
        let caps = &self.capabilities;
        for (i, kind) in caps.events.iter().enumerate() {
            if !coolify_domain::plugin_events::KINDS.contains(&kind.as_str()) {
                return bad(format!("unknown event {kind:?}"));
            }
            if caps.events[..i].contains(kind) {
                return bad(format!("event {kind:?} is listed twice"));
            }
        }
        if caps.http.len() > 16 {
            return bad("at most 16 http hosts".into());
        }
        for host in &caps.http {
            if !HOST.is_match(host) || host.len() > 253 {
                return bad(format!(
                    "http hosts must be lowercase host names without scheme, port or wildcard, not {host:?}"
                ));
            }
        }
        for (i, kind) in caps.resources.iter().enumerate() {
            if !RESOURCE_KINDS.contains(&kind.as_str()) {
                return bad(format!(
                    "unknown resource kind {kind:?}; the kinds are {}",
                    RESOURCE_KINDS.join(", ")
                ));
            }
            if caps.resources[..i].contains(kind) {
                return bad(format!("resource kind {kind:?} is listed twice"));
            }
        }
        validate_ui(caps)?;
        Ok(())
    }

    /// The capability names, as shown when asking an admin to grant them.
    pub fn capability_summary(&self) -> Vec<String> {
        let caps = &self.capabilities;
        let mut out = Vec::new();
        if !caps.events.is_empty() {
            out.push(format!("events: {}", caps.events.join(", ")));
        }
        if caps.storage {
            out.push("storage".into());
        }
        if caps.routes {
            out.push("routes".into());
        }
        if !caps.http.is_empty() {
            out.push(format!("http: {}", caps.http.join(", ")));
        }
        if !caps.resources.is_empty() {
            out.push(format!("resources: {}", caps.resources.join(", ")));
        }
        if caps.deploy {
            out.push("deploy".into());
        }
        if !caps.ui.is_empty() {
            let mut slots: Vec<&str> = Vec::new();
            for p in &caps.ui {
                if !slots.contains(&p.slot.as_str()) {
                    slots.push(&p.slot);
                }
            }
            out.push(format!("ui: {}", slots.join(", ")));
        }
        out
    }
}

/// Whether `id` is a valid plugin id (`publisher/name`).
pub(crate) fn check_id(id: &str) -> Result<(), String> {
    if ID.is_match(id) {
        Ok(())
    } else {
        Err("the id must be publisher/name in lowercase letters, digits and dashes".into())
    }
}

impl Capabilities {
    /// Whether `self` asks for anything `granted` does not have.
    pub fn exceeds(&self, granted: &Capabilities) -> bool {
        self.events.iter().any(|e| !granted.events.contains(e))
            || (self.storage && !granted.storage)
            || (self.routes && !granted.routes)
            || self.http.iter().any(|h| !granted.http.contains(h))
            || self
                .resources
                .iter()
                .any(|k| !granted.resources.contains(k))
            || (self.deploy && !granted.deploy)
            || self.ui.iter().any(|p| !granted.ui.contains(p))
    }

    /// The panels of one slot.
    pub fn panels<'a>(&'a self, slot: &'a str) -> impl Iterator<Item = &'a Panel> {
        self.ui.iter().filter(move |p| p.slot == slot)
    }
}

/// `[[capabilities.ui]]`: known slots, a short heading, and a plain path of the plugin's own routes.
fn validate_ui(caps: &Capabilities) -> Result<(), PluginError> {
    let bad = |msg: String| Err(PluginError::Manifest(msg));
    if caps.ui.is_empty() {
        return Ok(());
    }
    if !caps.routes {
        return bad("ui panels are pages of the plugin's routes, so ui needs routes = true".into());
    }
    if caps.ui.len() > 8 {
        return bad("at most 8 ui panels".into());
    }
    for (i, p) in caps.ui.iter().enumerate() {
        if !UI_SLOTS.contains(&p.slot.as_str()) {
            return bad(format!(
                "ui slot must be one of {}, not {:?}",
                UI_SLOTS.join(", "),
                p.slot
            ));
        }
        if p.title.trim().is_empty()
            || p.title.chars().count() > 64
            || p.title.chars().any(char::is_control)
        {
            return bad("ui panel titles must be 1 to 64 characters".into());
        }
        if !PANEL_PATH.is_match(&p.path) || p.path.split('/').any(|s| s == "." || s == "..") {
            return bad(format!(
                "ui panel paths must be plain paths such as \"/panel\", without a query, not {:?}",
                p.path
            ));
        }
        if caps.ui[..i]
            .iter()
            .any(|q| q.slot == p.slot && q.title == p.title)
        {
            return bad(format!(
                "ui panel {:?} is listed twice in {}",
                p.title, p.slot
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = r#"
id = "acme/deploy-stats"
name = "Deploy stats"
version = "1.2.3"
description = "Counts deployments."
license = "MIT"
api = "0.1"

[capabilities]
events = ["deployment.succeeded"]
storage = true
http = ["hooks.example.com"]
resources = ["applications", "servers"]
deploy = true
"#;

    #[test]
    fn parses_a_valid_manifest() {
        let m = Manifest::parse(OK).unwrap();
        assert_eq!(m.id, "acme/deploy-stats");
        assert_eq!(
            m.capability_summary(),
            [
                "events: deployment.succeeded",
                "storage",
                "http: hooks.example.com",
                "resources: applications, servers",
                "deploy"
            ]
        );
    }

    #[test]
    fn rejects_bad_fields() {
        let cases = [
            ("acme/deploy-stats", "Acme/Deploy", "id must be"),
            (
                "version = \"1.2.3\"",
                "version = \"1.2\"",
                "semantic version",
            ),
            ("api = \"0.1\"", "api = \"0.2\"", "plugin interface"),
            (
                "deployment.succeeded",
                "deployment.exploded",
                "unknown event",
            ),
            ("hooks.example.com", "*.example.com", "http hosts"),
            ("hooks.example.com", "10.0.0.1", "http hosts"),
            (
                "hooks.example.com",
                "https://hooks.example.com",
                "http hosts",
            ),
            ("hooks.example.com", "hooks.example.com:8443", "http hosts"),
            (
                "storage = true",
                "storage = true\nshell = true",
                "unknown field",
            ),
            ("\"servers\"]", "\"secrets\"]", "unknown resource kind"),
            ("\"servers\"]", "\"applications\"]", "listed twice"),
            ("deploy = true", "deploy = \"yes\"", "invalid type"),
        ];
        for (from, to, want) in cases {
            let text = OK.replacen(from, to, 1);
            let err = Manifest::parse(&text).unwrap_err().to_string();
            assert!(err.contains(want), "{to}: {err}");
        }
    }

    #[test]
    fn detects_capability_escalation() {
        let old = Manifest::parse(OK).unwrap().capabilities;
        let mut new = old.clone();
        assert!(!new.exceeds(&old));
        new.http.push("evil.example.com".into());
        assert!(new.exceeds(&old));
        let mut fewer = old.clone();
        fewer.storage = false;
        assert!(!fewer.exceeds(&old));

        let mut wider = old.clone();
        wider.resources.push("databases".into());
        assert!(wider.exceeds(&old));
        let no_deploy = Capabilities {
            deploy: false,
            ..old.clone()
        };
        assert!(old.exceeds(&no_deploy));
        assert!(!no_deploy.exceeds(&old));

        let ui = Manifest::parse(&format!("{OK}{UI}")).unwrap().capabilities;
        let mut moved = ui.clone();
        moved.ui[0].slot = "settings".into();
        assert!(moved.exceeds(&ui));
        let mut repointed = ui.clone();
        repointed.ui[0].path = "/other".into();
        assert!(repointed.exceeds(&ui));
        let mut dropped = ui.clone();
        dropped.ui.pop();
        assert!(!dropped.exceeds(&ui));
    }

    /// Appended to `OK`: routes, then three panels in two slots.
    const UI: &str = r#"routes = true

[[capabilities.ui]]
slot = "dashboard"
title = "Deployments"
path = "/panel"

[[capabilities.ui]]
slot = "application"
title = "Deploy history"
path = "/panels/app"

[[capabilities.ui]]
slot = "dashboard"
title = "Failures"
path = "/panels/failures"
"#;

    #[test]
    fn parses_ui_panels() {
        let m = Manifest::parse(&format!("{OK}{UI}")).unwrap();
        assert_eq!(m.capabilities.ui.len(), 3);
        assert_eq!(m.capabilities.panels("dashboard").count(), 2);
        assert_eq!(
            m.capability_summary(),
            [
                "events: deployment.succeeded",
                "storage",
                "routes",
                "http: hooks.example.com",
                "resources: applications, servers",
                "deploy",
                "ui: dashboard, application"
            ]
        );
        // Without panels the summary is unchanged, so grants made before `ui` existed still match.
        let json = serde_json::to_value(Manifest::parse(OK).unwrap().capabilities).unwrap();
        assert!(json.get("ui").is_none());
    }

    #[test]
    fn rejects_bad_ui_panels() {
        let cases = [
            ("routes = true", "routes = false", "needs routes"),
            (
                "slot = \"dashboard\"",
                "slot = \"topbar\"",
                "ui slot must be",
            ),
            ("title = \"Deployments\"", "title = \" \"", "titles"),
            ("path = \"/panel\"", "path = \"panel\"", "plain paths"),
            ("path = \"/panel\"", "path = \"/panel?x=1\"", "plain paths"),
            ("path = \"/panel\"", "path = \"/../admin\"", "plain paths"),
            ("path = \"/panel\"", "path = \"/a/./b\"", "plain paths"),
            (
                "path = \"/panel\"",
                "path = \"//evil.example\"",
                "plain paths",
            ),
            (
                "path = \"/panel\"",
                "path = \"https://evil.example/x\"",
                "plain paths",
            ),
            (
                "title = \"Failures\"",
                "title = \"Deployments\"",
                "listed twice",
            ),
            (
                "path = \"/panel\"",
                "path = \"/panel\"\nscript = \"x\"",
                "unknown field",
            ),
        ];
        for (from, to, want) in cases {
            let text = format!("{OK}{}", UI.replacen(from, to, 1));
            let err = Manifest::parse(&text).unwrap_err().to_string();
            assert!(err.contains(want), "{to}: {err}");
        }
    }
}
