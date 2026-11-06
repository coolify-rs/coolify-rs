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
}

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

    fn validate(&self) -> Result<(), PluginError> {
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
        out
    }
}

impl Capabilities {
    /// Whether `self` asks for anything `granted` does not have.
    pub fn exceeds(&self, granted: &Capabilities) -> bool {
        self.events.iter().any(|e| !granted.events.contains(e))
            || (self.storage && !granted.storage)
            || (self.routes && !granted.routes)
            || self.http.iter().any(|h| !granted.http.contains(h))
    }
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
                "http: hooks.example.com"
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
    }
}
