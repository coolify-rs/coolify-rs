//! `App\Services\ImageScan\VulnerabilityMarkers`: the security marker of lists and canvas cards
//! (spec 15.8), read from the stored security status: "Update now" for update_now, "No fix yet"
//! for no_fix, nothing for other statuses. Resources that are not scanned now are not_checked
//! and have no marker.

use std::collections::HashMap;

use coolify_db::image_scans::ScanOwner;
use serde::Serialize;

use super::resource_security::ResourceSecurity;
use super::{SecurityStatusType, status};
use crate::Jobs;

/// `VulnerabilityMarkers::SCAN_COLUMNS`: the columns the team page reads of its scans. The
/// light scan rows (`resource_scans::current_for_many(.., true)`) have them.
pub const SCAN_COLUMNS: [&str; 11] = [
    "id",
    "uuid",
    "server_id",
    "resource_type",
    "resource_id",
    "deployment_queue_id",
    "image_ref",
    "summary",
    "candidates",
    "builds_image",
    "scanned_at",
];

/// One marker (`array{status, text, label, tooltip}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Marker {
    pub status: String,
    pub text: String,
    /// The aria-label: the status and the action.
    pub label: String,
    /// The primary action sentence of update_now.
    pub tooltip: Option<String>,
}

/// `VulnerabilityMarkers::for($resources)`: the markers of the given resources (applications,
/// services, standalone databases), keyed by `ScanOwner::key()`. Resources without a marker
/// have no entry.
pub async fn markers_for(
    jobs: &Jobs,
    resources: &[ScanOwner],
) -> Result<HashMap<String, Marker>, String> {
    if resources.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(status::stored_many(jobs, resources, None)
        .await?
        .into_iter()
        .filter_map(|(key, security)| marker(&security).map(|m| (key, m)))
        .collect())
}

/// `VulnerabilityMarkers::marker($security)`.
pub fn marker(security: &ResourceSecurity) -> Option<Marker> {
    let text = match security.status {
        SecurityStatusType::UpdateNow => "Update now",
        SecurityStatusType::NoFix => "No fix yet",
        _ => return None,
    };
    let action = if security.status == SecurityStatusType::UpdateNow {
        security
            .primary()
            .map(|a| a.sentence.clone())
            .filter(|s| !s.trim().is_empty())
    } else {
        None
    };
    Some(Marker {
        status: security.status.value().to_owned(),
        text: text.to_owned(),
        label: format!(
            "Security: {}{}",
            text.to_lowercase(),
            action
                .as_deref()
                .map_or_else(String::new, |a| format!(". {a}"))
        ),
        tooltip: action,
    })
}

#[cfg(test)]
mod tests {
    use super::super::{SecurityAction, SecurityActionType};
    use super::*;

    #[test]
    fn markers_of_each_status() {
        let mut security = ResourceSecurity::new(SecurityStatusType::UpdateNow);
        security.actions = vec![SecurityAction {
            action_type: SecurityActionType::UpdateImage,
            sentence: "Update to 1.28.".to_owned(),
            target: None,
            copy: None,
            fixed_ids: Vec::new(),
            button: false,
            image: None,
            reference: None,
            parts: Vec::new(),
        }];
        let m = marker(&security).unwrap();
        assert_eq!(m.text, "Update now");
        assert_eq!(m.label, "Security: update now. Update to 1.28.");
        assert_eq!(m.tooltip.as_deref(), Some("Update to 1.28."));
        let no_fix = marker(&ResourceSecurity::new(SecurityStatusType::NoFix)).unwrap();
        assert_eq!(no_fix.label, "Security: no fix yet");
        assert_eq!(no_fix.tooltip, None);
        assert_eq!(
            marker(&ResourceSecurity::new(SecurityStatusType::CheckNeeded)),
            None
        );
    }
}
