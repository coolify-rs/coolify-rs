//! Does what the event payload's `do` says, so tests can check each limit of the sandbox:
//! `loop`, `alloc`, `panic`, `store` (key, size), `fetch` (url), `read-env`, `read-file`, `ok`.
//! Requests echo the caller and the team's stored keys, to check that teams can't see each other's data.

use coolify_rs_plugin::{Caller, Event, Plugin, Request, Response, http, storage};

struct Hostile;

impl Plugin for Hostile {
    fn on_event(event: Event) -> Result<(), String> {
        let data = event.data();
        let s = |k: &str| data.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_owned();
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
            _ => storage::set("last", event.kind.as_bytes()),
        }
    }

    fn handle_request(caller: Caller, request: Request) -> Response {
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

coolify_rs_plugin::export!(Hostile);
