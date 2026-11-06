//! The component's glue: settings in, requests out, open incidents in storage, routes and the panel.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use coolify_rs_plugin::{Caller, Event, Plugin, Request, Response, config, http, storage, ui};
use serde_json::{Value, json};

use crate::deliver::{self, Config, Open, Service, Step};
use crate::event::{self, Category};
use crate::text::html;

/// No request starts after this much of an event's 5 s. Each request is also cut off by the host before the
/// call's deadline, so a slow service fails its own request and never the whole call.
const EVENT_BUDGET: Duration = Duration::from_millis(3500);
/// The same for `POST /test`, whose call may take 10 s.
const TEST_BUDGET: Duration = Duration::from_millis(7000);
const OPEN: &str = "open";

struct Alerts;

fn settings() -> Config {
    Config::load(config::get)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The outcome of one step. `detail` is an HTTP status or the host's reason, which never holds the URL.
struct Sent {
    service: Service,
    ok: bool,
    detail: String,
}

fn send(step: &Step, started: Instant, budget: Duration) -> Sent {
    let r = match step {
        Step::Refused(service, why) => {
            return Sent {
                service: *service,
                ok: false,
                detail: why.clone(),
            };
        }
        Step::Send(r) => r,
    };
    let (ok, detail) = if started.elapsed() > budget {
        (false, "skipped: no time left for this event".to_owned())
    } else {
        match http::fetch(&http::Outgoing {
            method: r.method.into(),
            url: r.url.clone(),
            headers: r.headers.clone(),
            body: r.body.clone(),
        }) {
            Ok(resp) => (
                (200..300).contains(&resp.status),
                format!("HTTP {}", resp.status),
            ),
            Err(e) => (false, e),
        }
    };
    Sent {
        service: r.service,
        ok,
        detail,
    }
}

impl Plugin for Alerts {
    fn on_event(event: Event) -> Result<(), String> {
        let cfg = settings();
        let Some(alert) = event::alert(&event.kind, &event.data(), cfg.coolify_url.as_deref())
        else {
            return Ok(());
        };
        if !Service::ALL.iter().any(|s| cfg.configured(*s)) {
            return Ok(());
        }
        let mut open: Open = storage::get_json(OPEN)?.unwrap_or_default();
        let mut changed = open.prune(&alert.key, &cfg);
        let steps = deliver::plan(&cfg, &alert, open.0.get(&alert.key));
        let started = Instant::now();
        let results: Vec<Sent> = steps
            .iter()
            .map(|s| send(s, started, EVENT_BUDGET))
            .collect();
        let accepted: Vec<Service> = results.iter().filter(|r| r.ok).map(|r| r.service).collect();
        changed |= open.record(&alert, &accepted, now());
        if changed {
            storage::set_json(OPEN, &open)?;
        }
        let failed: Vec<String> = results
            .iter()
            .filter(|r| !r.ok)
            .map(|r| format!("{}: {}", r.service.label(), r.detail))
            .collect();
        if failed.is_empty() {
            Ok(())
        } else {
            Err(failed.join("; "))
        }
    }

    fn handle_request(caller: Caller, request: Request) -> Response {
        let can_test = caller.abilities.iter().any(|a| a == "write" || a == "root");
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/status") => Response::json(200, &status()),
            ("POST", "/test") if can_test => Response::json(200, &test()),
            ("POST", "/test") => Response::json(
                403,
                &json!({ "message": "Only team owners and admins can send a test alert." }),
            ),
            ("GET", "/panel") => panel(&status(), can_test),
            _ => Response::json(404, &json!({ "message": "Not found." })),
        }
    }
}

/// What the team configured, without any value: services as configured or not, and what is sent.
fn status() -> Value {
    let cfg = settings();
    let open: Open = storage::get_json(OPEN).ok().flatten().unwrap_or_default();
    let services: Vec<Value> = Service::ALL
        .iter()
        .map(|s| json!({ "service": s.id(), "label": s.label(), "configured": cfg.configured(*s) }))
        .collect();
    let send: serde_json::Map<String, Value> = Category::ALL
        .iter()
        .map(|c| {
            let v = config::get(c.setting()).unwrap_or_else(|| "failed".into());
            (c.name().to_owned(), json!(v))
        })
        .collect();
    json!({ "services": services, "send": send, "open_incidents": open.0.len() })
}

/// Sends the test alert to every configured service; each service stops at its first failed request.
fn test() -> Value {
    let cfg = settings();
    let plan = deliver::plan_test(&cfg);
    let started = Instant::now();
    let results: Vec<Value> = Service::ALL
        .iter()
        .map(|s| {
            let Some((_, steps)) = plan.iter().find(|(p, _)| p == s) else {
                return json!({ "service": s.id(), "label": s.label(), "configured": false });
            };
            let mut last = None;
            for step in steps {
                let sent = send(step, started, TEST_BUDGET);
                let ok = sent.ok;
                last = Some(sent);
                if !ok {
                    break;
                }
            }
            let (ok, detail) = last.map_or((false, String::new()), |r| (r.ok, r.detail));
            json!({ "service": s.id(), "label": s.label(), "configured": true, "ok": ok, "detail": detail })
        })
        .collect();
    json!({ "results": results })
}

fn panel(status: &Value, can_test: bool) -> Response {
    let mut rows = String::new();
    for s in status["services"].as_array().into_iter().flatten() {
        let configured = s["configured"].as_bool().unwrap_or(false);
        rows.push_str(&format!(
            "<li><b>{}</b> <span class=\"muted\">{}</span></li>",
            html(s["label"].as_str().unwrap_or_default()),
            if configured {
                "configured"
            } else {
                "not configured"
            }
        ));
    }
    let mut send = String::new();
    for (k, v) in status["send"].as_object().into_iter().flatten() {
        send.push_str(&format!(
            "<span class=\"muted\">{}:</span> {} ",
            html(k),
            html(v.as_str().unwrap_or_default())
        ));
    }
    let open = status["open_incidents"].as_u64().unwrap_or(0);
    let action = if can_test {
        r#"<p><button id="test" type="button">Send test alert</button> <span id="state" class="muted"></span></p>
<ul id="results"></ul>
<script>
var button = document.getElementById('test'), state = document.getElementById('state'),
    results = document.getElementById('results');
button.addEventListener('click', function () {
  button.disabled = true;
  state.textContent = 'Sending…';
  results.textContent = '';
  coolify.json('POST', '/test').then(function (r) {
    state.textContent = '';
    r.results.forEach(function (x) {
      var li = document.createElement('li');
      li.textContent = x.label + ': ' + (!x.configured ? 'not configured' : (x.ok ? 'sent' : 'failed') + ' (' + x.detail + ')');
      li.className = x.configured && !x.ok ? 'failed' : '';
      results.appendChild(li);
    });
  }).catch(function (e) { state.textContent = e.message; })
    .then(function () { button.disabled = false; });
});
</script>"#
    } else {
        r#"<p class="muted">Team owners and admins can send a test alert.</p>"#
    };
    ui::panel(&format!(
        r#"<style>
ul {{ margin: .25rem 0 .5rem; padding-left: 1.1rem; }}
button {{ font: inherit; padding: .3rem .75rem; border: 1px solid #d4d4d4; border-radius: .5rem; background: #fff; color: inherit; cursor: pointer; }}
.dark button {{ border-color: rgb(255 255 255 / .15); background: rgb(255 255 255 / .05); }}
button:disabled {{ opacity: .6; cursor: default; }}
.failed {{ color: #dc2626; }}
</style>
<ul>{rows}</ul>
<p>{send}</p>
<p class="muted">Open incidents: {open}. Configure the services under Team → Plugins → Configure.</p>
{action}"#
    ))
}

coolify_rs_plugin::export!(Alerts);
