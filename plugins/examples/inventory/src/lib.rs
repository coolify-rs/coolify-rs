//! Lists the team's applications through the `resources` capability.
//!
//! - `GET /api/v1/ext/coolify-rs/inventory/applications`: every application of the caller's team, with its
//!   uuid, name, status, build pack and domains.
//! - `GET /api/v1/ext/coolify-rs/inventory/applications/<uuid>`: one of them, as the API shows it.
//!
//! The plugin never names a team: coolify-rs answers for the team of the token that called the route.

use coolify_rs_plugin::resources::{self, Kind};
use coolify_rs_plugin::{Caller, Plugin, Request, Response};
use serde_json::{Value, json};

struct Inventory;

fn summary(app: &Value) -> Value {
    json!({
        "uuid": app.get("uuid"),
        "name": app.get("name"),
        "status": app.get("status"),
        "build_pack": app.get("build_pack"),
        "fqdn": app.get("fqdn"),
    })
}

fn failed(e: String) -> Response {
    Response::json(502, &json!({ "message": e }))
}

impl Plugin for Inventory {
    fn handle_request(_caller: Caller, request: Request) -> Response {
        let path: Vec<&str> = request.path.trim_matches('/').split('/').collect();
        match (request.method.as_str(), path.as_slice()) {
            ("GET", ["applications"]) => match resources::applications() {
                Ok(apps) => Response::json(200, &Value::Array(apps.iter().map(summary).collect())),
                Err(e) => failed(e),
            },
            ("GET", ["applications", uuid]) => match resources::find(Kind::Applications, uuid) {
                Ok(Some(app)) => Response::json(200, &app),
                Ok(None) => Response::json(404, &json!({ "message": "Application not found." })),
                Err(e) => failed(e),
            },
            _ => Response::json(404, &json!({ "message": "Not found." })),
        }
    }
}

coolify_rs_plugin::export!(Inventory);
