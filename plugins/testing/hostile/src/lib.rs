//! Does what the event payload's `do` says, so tests can check each limit of the sandbox:
//! `loop`, `alloc`, `panic`, `store` (key, size), `fetch` (url), `read-env`, `read-file`, `deploy` (uuid,
//! force; the answer or error is stored as `deployed`), `ok`.
//!
//! Requests reach the data capabilities (ADR 0011), so tests can check team scoping:
//! - `GET /resources/<kind>` and `GET /resources/<kind>/<uuid>`: `resources`;
//! - `GET /environments/<project uuid>`;
//! - `POST /deploy` with `uuid=...` and optionally `force=1` as the query: `deploy`.
//!
//! The answer is the host's JSON as is (200), `{"error": ...}` (422), or 404 for none. Any other request
//! echoes the caller and the team's stored keys, to check that teams can't see each other's data.

use coolify_rs_plugin::resources::Kind;
use coolify_rs_plugin::{
    Caller, Event, Plugin, Request, Response, deploy, http, resources, storage,
};

struct Hostile;

fn kind(name: &str) -> Option<Kind> {
    Some(match name {
        "projects" => Kind::Projects,
        "servers" => Kind::Servers,
        "applications" => Kind::Applications,
        "services" => Kind::Services,
        "databases" => Kind::Databases,
        _ => return None,
    })
}

fn raw(result: Result<Option<String>, String>) -> Response {
    match result {
        Ok(Some(text)) => Response {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: text.into_bytes(),
        },
        Ok(None) => Response::json(404, &serde_json::json!({ "message": "none" })),
        Err(e) => Response::json(422, &serde_json::json!({ "error": e })),
    }
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|p| p.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_owned())
}

impl Plugin for Hostile {
    fn on_event(event: Event) -> Result<(), String> {
        let data = event.data();
        let s = |k: &str| {
            data.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned()
        };
        match s("do").as_str() {
            "loop" => {
                let mut n: u64 = 0;
                loop {
                    n = std::hint::black_box(n.wrapping_add(1));
                }
            }
            "alloc" => {
                let mut hoard: Vec<Vec<u8>> = Vec::new();
                loop {
                    hoard.push(vec![1u8; 1024 * 1024]);
                    std::hint::black_box(&hoard);
                }
            }
            "panic" => panic!("on purpose"),
            "store" => {
                let size = data.get("size").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
                storage::set(&s("key"), &vec![7u8; size])
            }
            "fetch" => {
                let r = http::fetch(&http::Outgoing {
                    method: "GET".into(),
                    url: s("url"),
                    headers: vec![],
                    body: vec![],
                })?;
                storage::set("fetched", &r.body)
            }
            "read-env" => {
                let vars: Vec<String> = std::env::vars().map(|(k, _)| k).collect();
                storage::set("env", vars.join(",").as_bytes())
            }
            "read-file" => match std::fs::read("/etc/passwd") {
                Ok(_) => storage::set("file", b"read"),
                Err(e) => storage::set("file", e.to_string().as_bytes()),
            },
            "deploy" => {
                let force = data.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
                let answer = match deploy::application(&s("uuid"), force) {
                    Ok(text) => text,
                    Err(e) => serde_json::json!({ "error": e }).to_string(),
                };
                storage::set("deployed", answer.as_bytes())
            }
            _ => storage::set("last", event.kind.as_bytes()),
        }
    }

    fn handle_request(caller: Caller, request: Request) -> Response {
        let parts: Vec<&str> = request.path.trim_matches('/').split('/').collect();
        match (request.method.as_str(), parts.as_slice()) {
            ("GET", ["resources", k]) => match kind(k) {
                Some(k) => raw(resources::all(k).map(Some)),
                None => raw(Ok(None)),
            },
            ("GET", ["resources", k, uuid]) => match kind(k) {
                Some(k) => raw(resources::get(k, uuid)),
                None => raw(Ok(None)),
            },
            ("GET", ["environments", uuid]) => raw(resources::environments(uuid)),
            ("POST", ["deploy"]) => {
                let uuid = query_param(&request.query, "uuid").unwrap_or_default();
                let force = query_param(&request.query, "force").is_some_and(|v| v == "1");
                raw(deploy::application(&uuid, force).map(Some))
            }
            _ => {
                let keys = storage::keys("").unwrap_or_default();
                Response::json(
                    200,
                    &serde_json::json!({
                        "team_id": caller.team_id,
                        "user_id": caller.user_id,
                        "abilities": caller.abilities,
                        "method": request.method,
                        "path": request.path,
                        "query": request.query,
                        "keys": keys,
                    }),
                )
            }
        }
    }
}

coolify_rs_plugin::export!(Hostile);
