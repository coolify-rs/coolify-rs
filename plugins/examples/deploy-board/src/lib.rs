//! Counts deployments per team and per application, and shows them in two UI panels: one on the dashboard
//! and one in each application's Plugins tab.
//!
//! Routes, under `/api/v1/ext/coolify-rs/deploy-board` (and `/ui/ext/...` for the panels' bridge):
//! - `GET /stats`: the team's totals and its last five deployments.
//! - `GET /apps/{uuid}`: one application's totals.
//! - `POST /reset`: sets the team's totals back to zero.
//! - `GET /panels/dashboard`, `GET /panels/application?resource={uuid}`: the panels' HTML.

use coolify_rs_plugin::{Caller, Event, Plugin, Request, Response, storage, ui};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Default, Serialize, Deserialize)]
struct Totals {
    succeeded: u64,
    failed: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Team {
    succeeded: u64,
    failed: u64,
    /// Newest first, at most five.
    recent: Vec<Recent>,
}

#[derive(Serialize, Deserialize)]
struct Recent {
    application: String,
    succeeded: bool,
}

struct DeployBoard;

fn app_key(uuid: &str) -> String {
    format!("app:{uuid}")
}

impl Plugin for DeployBoard {
    fn on_event(event: Event) -> Result<(), String> {
        let succeeded = match event.kind.as_str() {
            "deployment.succeeded" => true,
            "deployment.failed" => false,
            _ => return Ok(()),
        };
        let data = event.data();
        let text = |k: &str| {
            data.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned()
        };

        let mut team: Team = storage::get_json("team")?.unwrap_or_default();
        if succeeded {
            team.succeeded += 1;
        } else {
            team.failed += 1;
        }
        team.recent.insert(
            0,
            Recent {
                application: text("application_name"),
                succeeded,
            },
        );
        team.recent.truncate(5);
        storage::set_json("team", &team)?;

        let uuid = text("application_uuid");
        if !uuid.is_empty() {
            let mut app: Totals = storage::get_json(&app_key(&uuid))?.unwrap_or_default();
            if succeeded {
                app.succeeded += 1;
            } else {
                app.failed += 1;
            }
            storage::set_json(&app_key(&uuid), &app)?;
        }
        Ok(())
    }

    fn handle_request(_caller: Caller, request: Request) -> Response {
        let result = match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/stats") => team().map(|t| Response::json(200, &json!(t))),
            ("POST", "/reset") => {
                storage::delete("team").map(|()| Response::json(200, &json!(Team::default())))
            }
            ("GET", path) if path.starts_with("/apps/") => {
                app(&path["/apps/".len()..]).map(|a| Response::json(200, &json!(a)))
            }
            ("GET", "/panels/dashboard") => team().map(|t| dashboard_panel(&t)),
            ("GET", "/panels/application") => {
                let uuid = query(&request.query, "resource").unwrap_or_default();
                app(&uuid).map(|a| application_panel(&uuid, &a))
            }
            _ => return Response::json(404, &json!({ "message": "Not found." })),
        };
        result.unwrap_or_else(|e| Response::json(500, &json!({ "message": e })))
    }
}

fn team() -> Result<Team, String> {
    Ok(storage::get_json("team")?.unwrap_or_default())
}

fn app(uuid: &str) -> Result<Totals, String> {
    if uuid.is_empty() || !uuid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Ok(Totals::default());
    }
    Ok(storage::get_json(&app_key(uuid))?.unwrap_or_default())
}

/// One value of a query string whose values need no decoding (coolify-rs passes plain ids).
fn query(q: &str, key: &str) -> Option<String> {
    q.split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_owned())
}

/// Application names come from users, so everything rendered here is escaped.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn dashboard_panel(t: &Team) -> Response {
    let recent: String = t
        .recent
        .iter()
        .map(|r| {
            format!(
                "<li><span class=\"dot {}\"></span>{}</li>",
                if r.succeeded { "ok" } else { "bad" },
                escape(if r.application.is_empty() {
                    "(unnamed)"
                } else {
                    &r.application
                })
            )
        })
        .collect();
    ui::panel(&format!(
        r#"<style>
ul {{ list-style: none; margin: .75rem 0 0; padding: 0; }}
li {{ display: flex; align-items: center; gap: .5rem; padding: .125rem 0; }}
.dot {{ width: .5rem; height: .5rem; border-radius: 999px; background: #ef4444; }}
.dot.ok {{ background: #22c55e; }}
button {{ margin-top: .75rem; font: inherit; padding: .25rem .625rem; border-radius: .5rem; border: 1px solid #d4d4d4; background: #fff; color: inherit; cursor: pointer; }}
.dark button {{ border-color: rgb(255 255 255 / .14); background: rgb(255 255 255 / .05); }}
</style>
<div class="stats">
  <div class="stat"><span class="muted">Succeeded</span><b id="succeeded">{succeeded}</b></div>
  <div class="stat"><span class="muted">Failed</span><b id="failed">{failed}</b></div>
</div>
<ul id="recent">{recent}</ul>
<p class="muted" id="empty"{hidden}>No deployments since the plugin was enabled.</p>
<button type="button" id="reset">Reset counts</button>
<script>
document.getElementById('reset').addEventListener('click', function () {{
  coolify.json('POST', '/reset').then(function (t) {{
    document.getElementById('succeeded').textContent = t.succeeded;
    document.getElementById('failed').textContent = t.failed;
    document.getElementById('recent').textContent = '';
    document.getElementById('empty').hidden = false;
  }});
}});
setInterval(function () {{
  coolify.json('GET', '/stats').then(function (t) {{
    document.getElementById('succeeded').textContent = t.succeeded;
    document.getElementById('failed').textContent = t.failed;
  }}).catch(function () {{}});
}}, 30000);
</script>"#,
        succeeded = t.succeeded,
        failed = t.failed,
        hidden = if t.recent.is_empty() { "" } else { " hidden" },
    ))
}

fn application_panel(uuid: &str, a: &Totals) -> Response {
    ui::panel(&format!(
        r#"<div class="stats">
  <div class="stat"><span class="muted">Succeeded</span><b>{succeeded}</b></div>
  <div class="stat"><span class="muted">Failed</span><b>{failed}</b></div>
</div>
<p class="muted">Deployments of this application since the plugin was enabled ({uuid}).</p>"#,
        succeeded = a.succeeded,
        failed = a.failed,
        uuid = escape(uuid),
    ))
}

coolify_rs_plugin::export!(DeployBoard);
