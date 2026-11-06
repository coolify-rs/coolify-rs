//! UI panels (ADR 0013): which panels a team sees, and fetching one panel's HTML from the plugin.
//!
//! A panel is a `GET` route of the plugin, declared in `[[capabilities.ui]]` and granted with the rest of
//! the manifest. Fetching it is an ordinary [`Host::handle_request`] call for the caller's team, so the
//! plugin must be loaded, granted `routes`, and enabled for that team. What comes back must be `200` with
//! `text/html` and fit in [`MAX_PANEL`]; the web UI then shows it in a sandboxed frame.

use crate::manifest::Panel;
use crate::runtime::{CallError, Caller, Host, Request};
use crate::{PluginError, store};

/// The largest panel document a plugin may return.
pub const MAX_PANEL: usize = 512 * 1024;

/// A panel a team can see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamPanel {
    /// The plugin's id, `publisher/name`.
    pub plugin: String,
    /// The plugin's display name.
    pub name: String,
    pub verified: bool,
    /// Position among the plugin's panels in the same slot.
    pub index: usize,
    pub panel: Panel,
}

/// Why a panel could not be shown.
#[derive(Debug, thiserror::Error)]
pub enum PanelError {
    #[error("the plugin has no such panel")]
    NoPanel,
    #[error("the resource id is not valid")]
    BadResource,
    #[error(transparent)]
    Call(#[from] CallError),
    #[error("the panel answered {0}, not 200")]
    Status(u16),
    #[error("the panel is not text/html")]
    NotHtml,
    #[error("the panel is larger than {MAX_PANEL} bytes")]
    TooLarge,
    #[error("the panel is not UTF-8")]
    NotUtf8,
}

impl From<PluginError> for PanelError {
    fn from(e: PluginError) -> Self {
        PanelError::Call(CallError::Store(e))
    }
}

/// Resource ids are uuids or Coolify's cuid2-style ids: letters, digits and dashes.
fn valid_resource(r: &str) -> bool {
    !r.is_empty() && r.len() <= 64 && r.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

impl Host {
    /// Panels of plugins that are loaded, enabled for the whole instance and for `team_id`, and were
    /// granted `routes`; by plugin id, then in manifest order.
    pub async fn panels_for_team(&self, team_id: i64) -> Result<Vec<TeamPanel>, PluginError> {
        let ids = store::ids_for_team(self.db(), team_id).await?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let loaded = self.loaded_ids();
        let mut out = Vec::new();
        for p in store::list(self.db()).await? {
            if !p.enabled || !p.granted.routes || !ids.contains(&p.id) || !loaded.contains(&p.id) {
                continue;
            }
            let mut seen: Vec<&str> = Vec::new();
            for panel in &p.granted.ui {
                let index = seen.iter().filter(|s| **s == panel.slot).count();
                seen.push(&panel.slot);
                out.push(TeamPanel {
                    plugin: p.id.clone(),
                    name: p.manifest.name.clone(),
                    verified: p.verified,
                    index,
                    panel: panel.clone(),
                });
            }
        }
        Ok(out)
    }

    /// Fetches the HTML of the plugin's `index`th granted panel in `slot` for the caller's team.
    /// `resource` is the application or server the page shows, passed to the plugin as a query parameter.
    pub async fn panel(
        &self,
        id: &str,
        caller: Caller,
        slot: &str,
        index: usize,
        resource: Option<&str>,
    ) -> Result<String, PanelError> {
        let installed = store::find(self.db(), id)
            .await?
            .ok_or(PanelError::NoPanel)?;
        let panel = installed
            .granted
            .panels(slot)
            .nth(index)
            .ok_or(PanelError::NoPanel)?
            .clone();
        let mut query = format!("slot={slot}");
        if let Some(r) = resource {
            if !valid_resource(r) {
                return Err(PanelError::BadResource);
            }
            query.push_str("&resource=");
            query.push_str(r);
        }
        let request = Request {
            method: "GET".into(),
            path: panel.path,
            query,
            headers: vec![("accept".into(), "text/html".into())],
            body: Vec::new(),
        };
        let response = self.handle_request(id, caller, request).await?;
        if response.status != 200 {
            return Err(PanelError::Status(response.status));
        }
        let html = response.headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("content-type")
                && v.split(';')
                    .next()
                    .is_some_and(|t| t.trim().eq_ignore_ascii_case("text/html"))
        });
        if !html {
            return Err(PanelError::NotHtml);
        }
        if response.body.len() > MAX_PANEL {
            return Err(PanelError::TooLarge);
        }
        String::from_utf8(response.body).map_err(|_| PanelError::NotUtf8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_ids_are_plain() {
        assert!(valid_resource("b1c2d3e4-0000-4000-8000-000000000001"));
        assert!(valid_resource("k8s0w4c0oog4o8cw"));
        for bad in ["", "a&slot=settings", "../x", "a b", &"a".repeat(65)] {
            assert!(!valid_resource(bad), "{bad}");
        }
    }
}
