//! The plugin interface: the `deployment.succeeded` handler, the routes and the panels. Every decision is
//! in [`crate::chain`]; this module reads and writes storage, asks coolify-rs for the team's applications
//! and queues deployments.
//!
//! Routes, under `/api/v1/ext/coolify-rs/deploy-chains` (and `/ui/ext/...` for the panels' bridge):
//! - `GET /rules`: every rule of the team, with the applications' names.
//! - `GET /rules/{source}`: the rule of one application (no targets when it has none).
//! - `PUT /rules/{source}` with `{"targets": [{"uuid", "force"}], "branch": null}`: saves a rule; an empty
//!   `targets` removes it. Needs `write` or `root`.
//! - `DELETE /rules/{source}`: removes a rule. Needs `write` or `root`.
//! - `GET /history[?application={uuid}]`: the triggered deployments, newest first.
//! - `GET /applications`: the team's applications, for the picker.
//! - `GET /panels/application?resource={uuid}`, `GET /panels/overview`: the panels.

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use coolify_rs_plugin::{
    Caller, Event, Plugin, Request, Response, deploy, log, resources, storage, ui,
};
use serde_json::{Value, json};

use crate::chain::{self, Entry, HISTORY_KEY, Rule, RuleInput, Status, Target};

/// What an event may spend deploying, of the 5 s the host gives it. Targets left when it runs out are
/// recorded as skipped.
const EVENT_BUDGET: Duration = Duration::from_millis(3500);
/// History entries a panel shows.
const PANEL_HISTORY: usize = 20;

struct DeployChains;

/// A route's answer; the error side is an answer too (a refusal or a failure of coolify-rs).
type Answer = Result<Response, Response>;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn message(status: u16, text: &str) -> Response {
    Response::json(status, &json!({ "message": text }))
}

fn not_found() -> Response {
    message(404, "Not found.")
}

/// A host call failed (storage or resources): the request can't be answered.
fn unavailable(e: String) -> Response {
    log::error(&format!("deploy chains: {e}"));
    message(502, &e)
}

fn invalid(errors: chain::Errors) -> Response {
    Response::json(
        422,
        &json!({ "message": "Validation failed.", "errors": errors }),
    )
}

fn load_rule(source: &str) -> Result<Option<Rule>, String> {
    if !chain::plausible_uuid(source) {
        return Ok(None);
    }
    storage::get_json(&chain::rule_key(source))
}

fn load_rules() -> Result<BTreeMap<String, Rule>, String> {
    let mut rules = BTreeMap::new();
    for key in storage::keys(chain::RULE_PREFIX)? {
        let source = &key[chain::RULE_PREFIX.len()..];
        if let Some(rule) = load_rule(source)? {
            rules.insert(source.to_owned(), rule);
        }
    }
    Ok(rules)
}

fn load_history() -> Result<Vec<Entry>, String> {
    Ok(storage::get_json(HISTORY_KEY)?.unwrap_or_default())
}

/// The team's applications, uuid → name, as `GET /api/v1/applications` lists them.
fn team_applications() -> Result<BTreeMap<String, String>, String> {
    Ok(resources::applications()?
        .iter()
        .filter_map(|a| {
            let uuid = a.get("uuid")?.as_str()?.to_owned();
            let name = a.get("name").and_then(Value::as_str).unwrap_or_default();
            Some((uuid, name.to_owned()))
        })
        .collect())
}

fn app_view(apps: &BTreeMap<String, String>, uuid: &str) -> Value {
    json!({
        "uuid": uuid,
        "name": chain::name_of(apps, uuid),
        "missing": !apps.contains_key(uuid),
    })
}

fn rule_view(apps: &BTreeMap<String, String>, source: &str, rule: Option<&Rule>) -> Value {
    let rule = rule.cloned().unwrap_or_default();
    let targets: Vec<Value> = rule
        .targets
        .iter()
        .map(|t| {
            let mut v = app_view(apps, &t.uuid);
            v["force"] = json!(t.force);
            v
        })
        .collect();
    json!({
        "source": app_view(apps, source),
        "branch": rule.branch,
        "targets": targets,
    })
}

fn history_view(apps: &BTreeMap<String, String>, entries: &[&Entry]) -> Value {
    Value::Array(
        entries
            .iter()
            .map(|e| {
                let mut v = json!(e);
                v["source_name"] = json!(chain::name_of(apps, &e.source));
                v["target_name"] = json!(chain::name_of(apps, &e.target));
                v
            })
            .collect(),
    )
}

fn applications_view(apps: &BTreeMap<String, String>) -> Value {
    let mut list: Vec<(&String, &String)> = apps.iter().collect();
    list.sort_by(|a, b| {
        a.1.to_lowercase()
            .cmp(&b.1.to_lowercase())
            .then(a.0.cmp(b.0))
    });
    Value::Array(
        list.into_iter()
            .map(|(uuid, name)| json!({ "uuid": uuid, "name": name }))
            .collect(),
    )
}

impl Plugin for DeployChains {
    fn on_event(event: Event) -> Result<(), String> {
        if event.kind != "deployment.succeeded" || event.version != 1 {
            return Ok(());
        }
        let Some(trigger) = chain::trigger(&event.data()) else {
            return Ok(());
        };
        let rule = load_rule(&trigger.application)?;
        let targets: Vec<Target> = match chain::wanted(&trigger, rule.as_ref()) {
            Ok(targets) => targets.to_vec(),
            Err(why) => {
                if rule.is_some() {
                    log::info(&format!(
                        "deploy chain of {} not run: {why}",
                        trigger.application
                    ));
                }
                return Ok(());
            }
        };
        let started = Instant::now();
        let at = now();
        let entry =
            |t: &Target, status: Status, deployment: Option<String>, message: String| Entry {
                at,
                source: trigger.application.clone(),
                source_deployment: trigger.deployment.clone(),
                target: t.uuid.clone(),
                force: t.force,
                status,
                deployment,
                message,
            };

        // Rules are checked for loops when saved; two saves at once could still close one, so check again.
        let uuids: Vec<String> = targets.iter().map(|t| t.uuid.clone()).collect();
        let cycle = chain::find_cycle(&trigger.application, &uuids, |u| {
            Ok::<_, String>(load_rule(u)?.map(|r| r.target_uuids()).unwrap_or_default())
        })?;
        let mut entries = Vec::with_capacity(targets.len());
        if let Some(cycle) = cycle {
            let names = team_applications().unwrap_or_default();
            let why = format!(
                "Not deployed: the deploy chain loops ({}). Remove one of its rules.",
                chain::describe_cycle(&cycle, &names)
            );
            log::warn(&format!("deploy chain of {}: {why}", trigger.application));
            for t in &targets {
                entries.push(entry(t, Status::Skipped, None, why.clone()));
            }
        } else {
            // One at a time: a refusal (consent, rate limit, a deleted application) is recorded and the
            // next target still gets its turn.
            for t in &targets {
                if started.elapsed() > EVENT_BUDGET {
                    entries.push(entry(
                        t,
                        Status::Skipped,
                        None,
                        "Not deployed: the event ran out of time.".into(),
                    ));
                    continue;
                }
                let (status, deployment, message) = chain::outcome(deploy::queue(&t.uuid, t.force));
                if status == Status::Failed {
                    log::warn(&format!(
                        "deploy chain of {}: {} was not deployed: {message}",
                        trigger.application, t.uuid
                    ));
                }
                entries.push(entry(t, status, deployment, message));
            }
        }
        // Read the history last, so a concurrent event's entries are lost only in a narrow window.
        let mut history = load_history()?;
        chain::record(&mut history, entries);
        storage::set_json(HISTORY_KEY, &history)
    }

    fn handle_request(caller: Caller, request: Request) -> Response {
        let segments: Vec<&str> = request.path.trim_start_matches('/').split('/').collect();
        let answer = match (request.method.as_str(), segments.as_slice()) {
            ("GET", ["rules"]) => list_rules(&caller),
            ("GET", ["rules", source]) => get_rule(source),
            ("PUT", ["rules", source]) => put_rule(&caller, source, &request.body),
            ("DELETE", ["rules", source]) => delete_rule(&caller, source),
            ("GET", ["history"]) => history(&request.query),
            ("GET", ["applications"]) => team_applications()
                .map(|apps| Response::json(200, &applications_view(&apps)))
                .map_err(unavailable),
            ("GET", ["panels", "application"]) => Ok(application_panel(&caller, &request.query)),
            ("GET", ["panels", "overview"]) => Ok(overview_panel(&caller)),
            _ => Err(not_found()),
        };
        answer.unwrap_or_else(|r| r)
    }
}

fn require_write(caller: &Caller) -> Result<(), Response> {
    if chain::can_write(&caller.abilities) {
        Ok(())
    } else {
        Err(message(
            403,
            "Changing deploy chains needs write access: a team admin, or a token with write.",
        ))
    }
}

fn list_rules(caller: &Caller) -> Answer {
    let apps = team_applications().map_err(unavailable)?;
    let rules = load_rules().map_err(unavailable)?;
    let views: Vec<Value> = rules
        .iter()
        .map(|(source, rule)| rule_view(&apps, source, Some(rule)))
        .collect();
    Ok(Response::json(
        200,
        &json!({ "can_write": chain::can_write(&caller.abilities), "rules": views }),
    ))
}

fn get_rule(source: &str) -> Answer {
    let apps = team_applications().map_err(unavailable)?;
    let rule = load_rule(source).map_err(unavailable)?;
    Ok(Response::json(
        200,
        &rule_view(&apps, source, rule.as_ref()),
    ))
}

fn put_rule(caller: &Caller, source: &str, body: &[u8]) -> Answer {
    require_write(caller)?;
    let input: RuleInput = serde_json::from_slice(body).map_err(|e| {
        let mut errors = chain::Errors::new();
        errors.insert(
            "body",
            vec![format!(
                "Send {{\"targets\": [{{\"uuid\": ..., \"force\": false}}], \"branch\": null}} ({e})."
            )],
        );
        invalid(errors)
    })?;
    let apps = team_applications().map_err(unavailable)?;
    let keys = storage::keys(chain::RULE_PREFIX).map_err(unavailable)?;
    let key = chain::rule_key(source);
    let rule =
        chain::check(source, input, &apps, keys.len(), keys.contains(&key)).map_err(invalid)?;
    if rule.targets.is_empty() {
        storage::delete(&key).map_err(unavailable)?;
        return Ok(Response::json(200, &rule_view(&apps, source, None)));
    }
    let cycle = chain::find_cycle(source, &rule.target_uuids(), |u| {
        Ok::<_, String>(load_rule(u)?.map(|r| r.target_uuids()).unwrap_or_default())
    })
    .map_err(unavailable)?;
    if let Some(cycle) = cycle {
        return Err(invalid(chain::cycle_errors(&cycle, &apps)));
    }
    storage::set_json(&key, &rule).map_err(unavailable)?;
    Ok(Response::json(200, &rule_view(&apps, source, Some(&rule))))
}

fn delete_rule(caller: &Caller, source: &str) -> Answer {
    require_write(caller)?;
    if load_rule(source).map_err(unavailable)?.is_none() {
        return Err(message(404, "This application has no deploy chain."));
    }
    storage::delete(&chain::rule_key(source)).map_err(unavailable)?;
    Ok(Response::json(200, &json!({ "deleted": true })))
}

fn history(query: &str) -> Answer {
    let apps = team_applications().map_err(unavailable)?;
    let all = load_history().map_err(unavailable)?;
    let entries: Vec<&Entry> = match chain::query_value(query, "application") {
        Some(app) => chain::involving(&all, &app),
        None => all.iter().collect(),
    };
    Ok(Response::json(
        200,
        &json!({ "history": history_view(&apps, &entries) }),
    ))
}

/// Escapes text for HTML; only used for error messages rendered on the server.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn failed_panel(e: &str) -> Response {
    ui::panel(&format!(
        "{STYLE}<div class=\"error\">Deploy chains are unavailable: {}</div>",
        escape(e)
    ))
}

/// A panel: the shared styles and scripts, the data as JSON, and the panel's own script. Everything the
/// panel shows from the data is set with `textContent`, so names are never parsed as HTML.
fn panel(markup: &str, data: &Value, script: &str) -> Response {
    ui::panel(&format!(
        "{STYLE}{markup}<script type=\"application/json\" id=\"data\">{}</script><script>{COMMON}{script}</script>",
        chain::json_for_html(data)
    ))
}

fn application_panel(caller: &Caller, query: &str) -> Response {
    let Some(app) = chain::query_value(query, "resource").filter(|u| chain::plausible_uuid(u))
    else {
        return ui::panel(&format!(
            "{STYLE}<p class=\"muted\">Open this panel from an application.</p>"
        ));
    };
    let loaded = (|| -> Result<Value, String> {
        let apps = team_applications()?;
        let rules = load_rules()?;
        let history = load_history()?;
        let sources: Vec<Value> = chain::sources_of(&rules, &app)
            .iter()
            .map(|s| app_view(&apps, s))
            .collect();
        let involving: Vec<&Entry> = chain::involving(&history, &app)
            .into_iter()
            .take(PANEL_HISTORY)
            .collect();
        Ok(json!({
            "app": app,
            "can_write": chain::can_write(&caller.abilities),
            "rule": rule_view(&apps, &app, rules.get(&app)),
            "sources": sources,
            "applications": applications_view(&apps),
            "history": history_view(&apps, &involving),
        }))
    })();
    match loaded {
        Ok(data) => panel(APPLICATION_MARKUP, &data, APPLICATION_SCRIPT),
        Err(e) => failed_panel(&e),
    }
}

fn overview_panel(caller: &Caller) -> Response {
    let loaded = (|| -> Result<Value, String> {
        let apps = team_applications()?;
        let rules = load_rules()?;
        let history = load_history()?;
        let views: Vec<Value> = rules
            .iter()
            .map(|(source, rule)| rule_view(&apps, source, Some(rule)))
            .collect();
        let recent: Vec<&Entry> = history.iter().take(PANEL_HISTORY).collect();
        Ok(json!({
            "can_write": chain::can_write(&caller.abilities),
            "rules": views,
            "history": history_view(&apps, &recent),
        }))
    })();
    match loaded {
        Ok(data) => panel(OVERVIEW_MARKUP, &data, OVERVIEW_SCRIPT),
        Err(e) => failed_panel(&e),
    }
}

/// Coolify's controls, on top of the SDK's `ui::STYLE`: its buttons, inputs, boxes and badges, in both
/// themes.
const STYLE: &str = r#"<style>
.rows { list-style: none; margin: 0 0 .75rem; padding: 0; display: flex; flex-direction: column; gap: .375rem; }
.rows li { display: flex; flex-wrap: wrap; align-items: center; gap: .75rem; padding: .5rem .75rem; border: 1px solid #e5e5e5; border-radius: .5rem; background: #fff; }
.dark .rows li { border-color: rgb(255 255 255 / .08); background: rgb(255 255 255 / .04); }
.grow { flex: 1; min-width: 10rem; overflow-wrap: anywhere; }
.name { font-weight: 600; color: #000; }
.dark .name { color: #f4f4f4; }
.small { font-size: 12px; }
.row { display: flex; flex-wrap: wrap; align-items: center; gap: .5rem; margin: .5rem 0; }
.section { margin-top: 1.25rem; }
.button { display: inline-flex; align-items: center; justify-content: center; gap: .375rem; height: 2rem; padding: 0 .625rem; font: inherit; font-weight: 500; white-space: nowrap; border-radius: .375rem; border: 1px solid #e5e5e5; background: #fff; color: #000; cursor: pointer; box-shadow: 0 2px 0 rgb(0 0 0 / .22); transition: background-color .15s, transform .15s; }
.button:hover:not(:disabled) { background: #f5f5f5; transform: translateY(-1px); box-shadow: 0 3px 0 rgb(0 0 0 / .22); }
.button:focus-visible { outline: none; box-shadow: 0 2px 0 rgb(0 0 0 / .22), 0 0 0 1px #6b16ed; }
.button:disabled { cursor: not-allowed; color: #d4d4d4; background: transparent; box-shadow: none; }
.dark .button:not(:disabled) { background: color-mix(in srgb, #6b16ed 14%, transparent); border-color: color-mix(in srgb, #a391ff 24%, transparent); color: #ece8ff; box-shadow: 0 2px 0 color-mix(in srgb, #6b16ed 35%, transparent); }
.dark .button:hover:not(:disabled) { background: color-mix(in srgb, #6b16ed 18%, transparent); border-color: color-mix(in srgb, #a391ff 34%, transparent); }
.dark .button:disabled { color: #8f8f8f; border-color: rgb(255 255 255 / .06); }
.button.danger:not(:disabled) { color: #dc2626; }
.dark .button.danger:not(:disabled) { color: #f87171; background: rgb(248 113 113 / .08); border-color: rgb(248 113 113 / .24); box-shadow: 0 2px 0 rgb(153 27 27 / .6); }
.input { height: 2rem; box-sizing: border-box; padding: 0 .75rem; font: inherit; color: #000; border: 1px solid #e5e5e5; border-radius: .375rem; background: #fafafa; min-width: 12rem; }
.dark .input { color: #f4f4f4; border-color: rgb(255 255 255 / .08); background: rgb(255 255 255 / .03); }
.input:focus-visible { outline: none; border-color: #6b16ed; }
.input:disabled { color: #a3a3a3; }
.check { display: inline-flex; align-items: center; gap: .375rem; cursor: pointer; }
input[type=checkbox] { accent-color: #6b16ed; margin: 0; }
.error { margin: .5rem 0; padding: .5rem .75rem; border-radius: .5rem; border: 1px solid #fecaca; background: #fef2f2; color: #b91c1c; }
.dark .error { border-color: rgb(248 113 113 / .3); background: rgb(248 113 113 / .08); color: #fca5a5; }
.error ul { margin: .25rem 0 0; padding-left: 1.25rem; }
.table { overflow-x: auto; }
table { width: 100%; border-collapse: collapse; }
th, td { text-align: left; padding: .375rem .5rem; border-bottom: 1px solid #e5e5e5; vertical-align: top; }
.dark th, .dark td { border-color: rgb(255 255 255 / .08); }
th { font-weight: 500; color: #737373; white-space: nowrap; }
.dark th { color: #8f8f8f; }
.badge { display: inline-block; padding: 0 .4rem; border-radius: 999px; font-size: 12px; font-weight: 500; white-space: nowrap; }
.badge.queued { background: #dcfce7; color: #15803d; }
.dark .badge.queued { background: rgb(34 197 94 / .15); color: #4ade80; }
.badge.failed { background: #fee2e2; color: #b91c1c; }
.dark .badge.failed { background: rgb(248 113 113 / .15); color: #f87171; }
.badge.skipped, .badge.force { background: #f5f5f5; color: #525252; }
.dark .badge.skipped, .dark .badge.force { background: rgb(255 255 255 / .08); color: #c4c4c4; }
</style>"#;

/// Shared by both panels: the bridge calls with their errors, and the history table.
const COMMON: &str = r#"
var data = JSON.parse(document.getElementById('data').textContent);
function $(id) { return document.getElementById(id); }
function el(tag, cls, text) {
  var e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined && text !== null) e.textContent = text;
  return e;
}
function call(method, path, body) {
  return coolify.request(method, path, body).then(function (r) {
    var parsed = null;
    try { parsed = JSON.parse(r.body); } catch (e) {}
    if (r.status >= 200 && r.status < 300) return parsed;
    var details = [];
    if (parsed && parsed.errors) Object.keys(parsed.errors).forEach(function (k) { details = details.concat(parsed.errors[k]); });
    throw { message: (parsed && parsed.message) || ('HTTP ' + r.status), details: details };
  });
}
function showError(err) {
  var box = $('error');
  box.textContent = '';
  if (!err) { box.hidden = true; return; }
  box.appendChild(el('div', null, err.message || String(err)));
  if (err.details && err.details.length) {
    var ul = el('ul');
    err.details.forEach(function (d) { ul.appendChild(el('li', null, d)); });
    box.appendChild(ul);
  }
  box.hidden = false;
}
function renderHistory(entries) {
  var body = $('history-rows');
  body.textContent = '';
  entries.forEach(function (e) {
    var tr = el('tr');
    tr.appendChild(el('td', 'muted', new Date(e.at * 1000).toLocaleString()));
    tr.appendChild(el('td', null, e.source_name + ' → ' + e.target_name + (e.force ? ' (force)' : '')));
    var status = el('td');
    status.appendChild(el('span', 'badge ' + e.status, e.status));
    tr.appendChild(status);
    tr.appendChild(el('td', null, e.message));
    body.appendChild(tr);
  });
  $('history').hidden = entries.length === 0;
  $('no-history').hidden = entries.length !== 0;
}
"#;

const APPLICATION_MARKUP: &str = concat!(
    r#"<p>After this application deploys, deploy:</p>
<ul id="targets" class="rows"></ul>
<p id="none" class="muted" hidden>Nothing yet.</p>
<div id="editor" hidden>
  <div class="row">
    <select id="pick" class="input" aria-label="Application to deploy"></select>
    <label class="check"><input type="checkbox" id="force"> Force rebuild (no cache)</label>
    <button type="button" class="button" id="add">Add</button>
  </div>
  <div class="row">
    <label for="branch" class="muted">Only when deployed from branch</label>
    <input id="branch" class="input" placeholder="Any branch" maxlength="255" autocomplete="off">
    <button type="button" class="button" id="save-branch">Save</button>
  </div>
</div>
<p id="readonly" class="muted" hidden>You can view this chain. Changing it needs a team admin.</p>
<div id="error" class="error" role="alert" hidden></div>
<div id="sources-box" class="section" hidden>
  <h3>Deployed after</h3>
  <ul id="sources" class="rows"></ul>
</div>
<p class="muted small">Preview deployments never trigger a chain. Deployments run as the team admin who allowed this plugin to deploy.</p>
"#,
    r#"<div class="section">
  <h3>Recent triggered deploys</h3>
  <div class="table"><table id="history" hidden>
    <thead><tr><th>When</th><th>Chain</th><th>Result</th><th>Message</th></tr></thead>
    <tbody id="history-rows"></tbody>
  </table></div>
  <p id="no-history" class="muted">No deployments triggered yet.</p>
</div>"#
);

const APPLICATION_SCRIPT: &str = r#"
var busy = false;
function save(targets, branch) {
  if (busy) return;
  busy = true;
  render();
  call('PUT', '/rules/' + encodeURIComponent(data.app), { targets: targets, branch: branch })
    .then(function (rule) { data.rule = rule; showError(null); })
    .catch(showError)
    .then(function () { busy = false; render(); });
}
function current() {
  return data.rule.targets.map(function (t) { return { uuid: t.uuid, force: t.force }; });
}
function render() {
  var list = $('targets');
  list.textContent = '';
  var chosen = {};
  data.rule.targets.forEach(function (t, i) {
    chosen[t.uuid] = true;
    var li = el('li');
    var label = el('div', 'grow');
    label.appendChild(el('span', 'name', t.name));
    if (t.missing) label.appendChild(el('span', 'muted', ' (no longer exists)'));
    li.appendChild(label);
    var force = el('label', 'check');
    var box = el('input');
    box.type = 'checkbox';
    box.checked = !!t.force;
    box.disabled = !data.can_write || busy;
    box.addEventListener('change', function () {
      var next = current();
      next[i].force = box.checked;
      save(next, data.rule.branch);
    });
    force.appendChild(box);
    force.appendChild(document.createTextNode(' Force rebuild'));
    li.appendChild(force);
    if (data.can_write) {
      var remove = el('button', 'button danger', 'Remove');
      remove.type = 'button';
      remove.disabled = busy;
      remove.addEventListener('click', function () {
        var next = current();
        next.splice(i, 1);
        save(next, data.rule.branch);
      });
      li.appendChild(remove);
    }
    list.appendChild(li);
  });
  $('none').hidden = data.rule.targets.length !== 0;
  $('editor').hidden = !data.can_write;
  $('readonly').hidden = data.can_write;
  var pick = $('pick');
  pick.textContent = '';
  data.applications.forEach(function (a) {
    if (a.uuid === data.app || chosen[a.uuid]) return;
    var o = el('option', null, a.name || a.uuid);
    o.value = a.uuid;
    pick.appendChild(o);
  });
  var none = pick.options.length === 0;
  if (none) pick.appendChild(el('option', null, 'No other applications'));
  pick.disabled = none || busy;
  $('add').disabled = none || busy;
  $('save-branch').disabled = busy;
  if (document.activeElement !== $('branch')) $('branch').value = data.rule.branch || '';
  var sources = $('sources');
  sources.textContent = '';
  data.sources.forEach(function (s) {
    var li = el('li');
    li.appendChild(el('span', 'name', s.name));
    sources.appendChild(li);
  });
  $('sources-box').hidden = data.sources.length === 0;
  renderHistory(data.history);
}
$('add').addEventListener('click', function () {
  var next = current();
  next.push({ uuid: $('pick').value, force: $('force').checked });
  $('force').checked = false;
  save(next, data.rule.branch);
});
$('save-branch').addEventListener('click', function () {
  var branch = $('branch').value.trim();
  save(current(), branch === '' ? null : branch);
  $('branch').blur();
});
render();
setInterval(function () {
  call('GET', '/history?application=' + encodeURIComponent(data.app)).then(function (h) {
    data.history = h.history.slice(0, 20);
    renderHistory(data.history);
  }).catch(function () {});
}, 30000);
"#;

const OVERVIEW_MARKUP: &str = concat!(
    r#"<ul id="rules" class="rows"></ul>
<p id="no-rules" class="muted" hidden>No deploy chains yet. Add one from an application's Plugins tab, under Deploy chain.</p>
<div id="error" class="error" role="alert" hidden></div>
"#,
    r#"<div class="section">
  <h3>Recent triggered deploys</h3>
  <div class="table"><table id="history" hidden>
    <thead><tr><th>When</th><th>Chain</th><th>Result</th><th>Message</th></tr></thead>
    <tbody id="history-rows"></tbody>
  </table></div>
  <p id="no-history" class="muted">No deployments triggered yet.</p>
</div>"#
);

const OVERVIEW_SCRIPT: &str = r#"
function render() {
  var list = $('rules');
  list.textContent = '';
  data.rules.forEach(function (r) {
    var li = el('li');
    var text = el('div', 'grow');
    text.appendChild(el('span', 'name', r.source.name));
    text.appendChild(el('span', 'muted', ' then deploys '));
    r.targets.forEach(function (t, i) {
      if (i > 0) text.appendChild(document.createTextNode(', '));
      text.appendChild(el('span', 'name', t.name));
      if (t.force) { text.appendChild(document.createTextNode(' ')); text.appendChild(el('span', 'badge force', 'force')); }
    });
    if (r.branch) text.appendChild(el('span', 'muted', ' (branch ' + r.branch + ' only)'));
    li.appendChild(text);
    if (data.can_write) {
      var remove = el('button', 'button danger', 'Delete');
      remove.type = 'button';
      remove.addEventListener('click', function () {
        remove.disabled = true;
        call('DELETE', '/rules/' + encodeURIComponent(r.source.uuid))
          .then(function () { return call('GET', '/rules'); })
          .then(function (all) { data.rules = all.rules; showError(null); render(); })
          .catch(function (e) { remove.disabled = false; showError(e); });
      });
      li.appendChild(remove);
    }
    list.appendChild(li);
  });
  $('no-rules').hidden = data.rules.length !== 0;
  renderHistory(data.history);
}
render();
"#;

coolify_rs_plugin::export!(DeployChains);
