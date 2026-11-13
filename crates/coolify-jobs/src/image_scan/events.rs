//! `App\Events\ImageScanUpdated`: tells the pages of a team that a scan of a resource made
//! progress or ended, and `App\Events\ServerPatchCheckFinished`: tells the patches page of a
//! server that the host package scan ended. Upstream broadcasts them on the private team
//! channel; here a sink set at startup receives them (the API layer forwards them to its
//! clients).

use std::sync::OnceLock;

use serde_json::{Value, json};

/// One `ImageScanUpdated` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageScanUpdated {
    pub team_id: i64,
    pub resource_uuid: String,
    /// The uuid of the scan row, or the run ID while no row exists.
    pub scan_uuid: String,
    /// False for a progress update of a running scan; pages that show only results ignore it.
    pub is_final: bool,
}

impl ImageScanUpdated {
    /// `broadcastWith()`.
    pub fn payload(&self) -> Value {
        json!({"resourceUuid": self.resource_uuid, "scanUuid": self.scan_uuid, "final": self.is_final})
    }
}

/// Receives the events. Set once at startup; without one, events are dropped.
pub trait ScanEventSink: Send + Sync {
    /// Called on the scanning task: hand the event off and return at once.
    fn image_scan_updated(&self, event: &ImageScanUpdated);

    /// `ServerPatchCheckFinished` (`broadcastWith()`: `{"serverUuid": …}`). Sinks that do not
    /// forward it can leave it out.
    fn server_patch_check_finished(&self, _team_id: i64, _server_uuid: &str) {}
}

static SINK: OnceLock<Box<dyn ScanEventSink>> = OnceLock::new();

/// Sets the sink. Returns false when one was already set.
pub fn set_event_sink(sink: Box<dyn ScanEventSink>) -> bool {
    SINK.set(sink).is_ok()
}

/// `ImageScanUpdated::dispatch($teamId, $resourceUuid, $scanUuid, $final)`. Without a team
/// nothing is broadcast (`BroadcastsToTeam`).
pub fn dispatch(team_id: Option<i64>, resource_uuid: &str, scan_uuid: &str, is_final: bool) {
    let (Some(team_id), Some(sink)) = (team_id, SINK.get()) else {
        return;
    };
    sink.image_scan_updated(&ImageScanUpdated {
        team_id,
        resource_uuid: resource_uuid.to_owned(),
        scan_uuid: scan_uuid.to_owned(),
        is_final,
    });
}

/// `ServerPatchCheckFinished::dispatch($teamId, $serverUuid)`: the host package scan of the
/// server ended, so its patches page reloads.
pub fn dispatch_server_patch_check_finished(team_id: i64, server_uuid: &str) {
    if let Some(sink) = SINK.get() {
        sink.server_patch_check_finished(team_id, server_uuid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_matches_upstream() {
        let event = ImageScanUpdated {
            team_id: 1,
            resource_uuid: "app".into(),
            scan_uuid: "scan".into(),
            is_final: false,
        };
        assert_eq!(
            event.payload().to_string(),
            r#"{"resourceUuid":"app","scanUuid":"scan","final":false}"#
        );
    }
}
