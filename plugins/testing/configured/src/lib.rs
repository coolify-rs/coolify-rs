//! Answers the team's settings, so tests can check that a plugin reads its own settings for the call's team
//! and nothing else:
//!
//! - `GET /config/<key>`: `{"key": ..., "value": ...}` with what `config::get` returned (null for none);
//! - an event stores what `config::get("topic")` returned as `topic` in storage, so events are checked too.
//!
//! A real plugin never returns a secret from a route; this one does on purpose, to prove which value the host
//! hands over.

use coolify_rs_plugin::{Caller, Event, Plugin, Request, Response, config, storage};

struct Configured;

impl Plugin for Configured {
    fn on_event(_event: Event) -> Result<(), String> {
        let topic = config::get_or("topic", "(none)");
        storage::set("topic", topic.as_bytes())
    }

    fn handle_request(_caller: Caller, request: Request) -> Response {
        let parts: Vec<&str> = request.path.trim_matches('/').split('/').collect();
        match (request.method.as_str(), parts.as_slice()) {
            ("GET", ["config", key]) => Response::json(
                200,
                &serde_json::json!({ "key": key, "value": config::get(key) }),
            ),
            ("GET", ["topic"]) => {
                let seen = storage::get("topic").ok().flatten().unwrap_or_default();
                Response::json(
                    200,
                    &serde_json::json!({ "topic": String::from_utf8_lossy(&seen) }),
                )
            }
            _ => Response::json(404, &serde_json::json!({ "message": "Not found." })),
        }
    }
}

coolify_rs_plugin::export!(Configured);
