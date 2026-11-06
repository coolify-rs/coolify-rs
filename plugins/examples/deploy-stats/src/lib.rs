//! Counts successful and failed deployments per team.
//! `GET /api/v1/ext/coolify-rs/deploy-stats/stats` returns the totals and the last deployment.

use coolify_rs_plugin::{Caller, Event, Plugin, Request, Response, storage};
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
struct Stats {
    succeeded: u64,
    failed: u64,
    last: Option<serde_json::Value>,
}

struct DeployStats;

impl Plugin for DeployStats {
    fn on_event(event: Event) -> Result<(), String> {
        let mut stats: Stats = storage::get_json("stats")?.unwrap_or_default();
        match event.kind.as_str() {
            "deployment.succeeded" => stats.succeeded += 1,
            "deployment.failed" => stats.failed += 1,
            _ => return Ok(()),
        }
        let data = event.data();
        stats.last = Some(serde_json::json!({
            "kind": event.kind,
            "application": data.get("application_name"),
            "deployment_uuid": data.get("deployment_uuid"),
        }));
        storage::set_json("stats", &stats)
    }

    fn handle_request(_caller: Caller, request: Request) -> Response {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/stats") => match storage::get_json::<Stats>("stats") {
                Ok(stats) => Response::json(200, &serde_json::json!(stats.unwrap_or_default())),
                Err(e) => Response::json(500, &serde_json::json!({ "message": e })),
            },
            _ => Response::json(404, &serde_json::json!({ "message": "Not found." })),
        }
    }
}

coolify_rs_plugin::export!(DeployStats);
