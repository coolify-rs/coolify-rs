//! The panels' HTML, without the SDK's styles and bridge (the component adds them with `ui::panel`).
//!
//! A panel carries its first answer of `/metrics.json` as JSON in the page, so it shows numbers at once,
//! and renders it with the same script that renders the answers to the window toggle. The script builds
//! elements with `textContent` only, so names from users are never parsed as HTML.

use serde_json::Value;

/// JSON that is safe inside `<script type="application/json">`: no `<`, `>` or `&` survive, so the
/// data can't close the element. They only occur inside JSON strings, where `<` means the same.
pub fn embed(v: &Value) -> String {
    v.to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

const CSS: &str = r#"<style>
.bar { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: .5rem; margin-bottom: .75rem; }
.seg { display: inline-flex; gap: .25rem; }
.seg button { font: inherit; font-weight: 500; height: 1.75rem; padding: 0 .625rem; border-radius: .125rem; border: 2px solid #e5e5e5; background: #fff; color: #000; cursor: pointer; }
.seg button:hover { background: #f5f5f5; }
.seg button[aria-pressed="true"] { border-color: #6b16ed; }
.dark .seg button { border-color: #242424; background: #181818; color: #fff; }
.dark .seg button:hover { background: #202020; }
.dark .seg button[aria-pressed="true"] { border-color: #fcd452; }
.tiles { display: grid; grid-template-columns: repeat(auto-fill, minmax(11.5rem, 1fr)); gap: .75rem; }
.tile { min-width: 0; display: flex; flex-direction: column; gap: .125rem; }
.tile-head { display: flex; align-items: center; justify-content: space-between; gap: .5rem; }
.tile .sub { font-size: 12px; }
.band { font-size: 11px; font-weight: 600; line-height: 1; padding: .2rem .4rem; border-radius: 999px; white-space: nowrap; }
.band.elite { background: #dcfce7; color: #166534; }
.band.high { background: #dbeafe; color: #1e40af; }
.band.medium { background: #fef3c7; color: #92400e; }
.band.low { background: #fee2e2; color: #991b1b; }
.dark .band.elite { background: rgb(34 197 94 / .15); color: #4ade80; }
.dark .band.high { background: rgb(59 130 246 / .15); color: #60a5fa; }
.dark .band.medium { background: rgb(245 158 11 / .15); color: #fbbf24; }
.dark .band.low { background: rgb(239 68 68 / .15); color: #f87171; }
.table { width: 100%; overflow-x: auto; margin-top: 1rem; }
table { width: 100%; border-collapse: collapse; font-variant-numeric: tabular-nums; }
th { text-align: left; font-weight: 600; color: #000; white-space: nowrap; }
.dark th { color: #f4f4f4; }
th, td { padding: .375rem .5rem; border-bottom: 1px solid #e5e5e5; }
.dark th, .dark td { border-color: rgb(255 255 255 / .08); }
td.num, th.num { text-align: right; }
td.name { max-width: 16rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.dot { display: inline-block; width: .5rem; height: .5rem; border-radius: 999px; margin-right: .375rem; background: #ef4444; }
.dot.ok { background: #22c55e; }
.note { margin: .75rem 0 0; font-size: 12px; }
code { font: 12px ui-monospace, SFMono-Regular, Menlo, monospace; }
.error { color: #dc2626; }
.dark .error { color: #f87171; }
</style>"#;

const SCRIPT: &str = r#"<script>
(function () {
  var data = JSON.parse(document.getElementById('initial').textContent);
  var app = document.getElementById('panel').getAttribute('data-app');
  var BAND = { elite: 'Elite', high: 'High', medium: 'Medium', low: 'Low' };
  function el(tag, cls, text) {
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined && text !== null) e.textContent = text;
    return e;
  }
  function trim(x) { return x.toFixed(1).replace(/\.0$/, ''); }
  function dur(s) {
    if (s === null || s === undefined) return '–';
    if (s < 60) return s + 's';
    if (s < 3600) return Math.round(s / 60) + 'm';
    if (s < 86400) return trim(s / 3600) + 'h';
    return trim(s / 86400) + 'd';
  }
  function pct(r) { return r === null || r === undefined ? '–' : trim(r * 100) + '%'; }
  function when(at) { return at.replace('T', ' ').slice(0, 16) + ' UTC'; }
  function tile(label, value, sub, band) {
    var t = el('div', 'stat tile'), h = el('div', 'tile-head');
    h.appendChild(el('span', 'muted', label));
    if (band) h.appendChild(el('span', 'band ' + band, BAND[band]));
    t.appendChild(h);
    t.appendChild(el('b', null, value));
    if (sub) t.appendChild(el('span', 'muted sub', sub));
    return t;
  }
  function tiles(m) {
    var box = document.getElementById('tiles');
    box.textContent = '';
    var f = m.deployment_frequency, c = m.change_failure_rate, r = m.time_to_restore, l = m.lead_time;
    box.appendChild(tile('Deployment frequency',
      f.per_day >= 1 ? trim(f.per_day) + ' / day' : trim(f.per_week) + ' / week',
      f.deployments + ' successful', f.band));
    box.appendChild(tile('Change failure rate', pct(c.rate), c.failed + ' of ' + c.total + ' failed', c.band));
    box.appendChild(tile('Time to restore', dur(r.median_seconds),
      r.restored ? 'median, p90 ' + dur(r.p90_seconds) + (r.unrestored ? ', ' + r.unrestored + ' not restored' : '')
        : (r.unrestored ? r.unrestored + ' not restored yet' : 'No failed deployments'), r.band));
    box.appendChild(tile('Lead time for changes', l.available ? dur(l.median_seconds) : 'Not available',
      l.available ? 'median, p90 ' + dur(l.p90_seconds) : l.reason, l.band));
  }
  function table(head, rows) {
    var wrap = el('div', 'table'), t = el('table'), tr = el('tr');
    head.forEach(function (h) { tr.appendChild(el('th', h[1], h[0])); });
    var thead = el('thead'); thead.appendChild(tr); t.appendChild(thead);
    var body = el('tbody');
    rows.forEach(function (r) { body.appendChild(r); });
    t.appendChild(body); wrap.appendChild(t);
    return wrap;
  }
  function row(cells) {
    var tr = el('tr');
    cells.forEach(function (c) {
      var td = el('td', c[1]);
      if (c[0] instanceof Node) td.appendChild(c[0]); else td.textContent = c[0];
      if (c[2]) td.title = c[2];
      tr.appendChild(td);
    });
    return tr;
  }
  function outcome(d) {
    var s = el('span');
    s.appendChild(el('span', d.outcome === 'succeeded' ? 'dot ok' : 'dot'));
    s.appendChild(document.createTextNode(d.outcome === 'succeeded' ? 'Succeeded' : 'Failed'));
    return s;
  }
  function render(d) {
    data = d;
    document.querySelectorAll('.seg button').forEach(function (b) {
      b.setAttribute('aria-pressed', String(Number(b.getAttribute('data-window')) === d.window_days));
    });
    document.getElementById('range').textContent = d.from.slice(0, 10) + ' to ' + d.to.slice(0, 10);
    tiles(d.metrics);
    var list = document.getElementById('list');
    list.textContent = '';
    if (!d.metrics.deployments) {
      list.appendChild(el('p', 'muted note', 'No deployments in the last ' + d.window_days +
        ' days. Deployments are recorded from the moment the plugin is enabled for the team.'));
      return;
    }
    if (app) {
      list.appendChild(table([['Finished'], ['Outcome'], ['Environment'], ['Commit'], ['Preview']],
        d.recent.map(function (x) {
          return row([[when(x.at)], [outcome(x)], [x.environment || '–'],
            [x.commit ? x.commit.slice(0, 7) : '–', null, x.commit || ''],
            [x.pull_request ? '#' + x.pull_request : '–']]);
        })));
    } else {
      list.appendChild(table([['Application'], ['Deployments', 'num'], ['Failure rate', 'num'],
        ['Restore (median)', 'num'], ['Last deployment']],
        d.applications.map(function (a) {
          return row([[a.name || a.uuid, 'name', a.uuid], [a.deployments, 'num'], [pct(a.change_failure_rate), 'num'],
            [dur(a.restore_median_seconds), 'num'], [when(a.last_deployment_at)]]);
        })));
      if (d.applications_total > d.applications.length) {
        list.appendChild(el('p', 'muted note', (d.applications_total - d.applications.length) +
          ' more applications in the CSV export.'));
      }
    }
  }
  document.querySelectorAll('.seg button').forEach(function (b) {
    b.addEventListener('click', function () {
      var path = '/metrics.json?window=' + b.getAttribute('data-window') + (app ? '&app=' + encodeURIComponent(app) : '');
      var err = document.getElementById('error');
      coolify.json('GET', path).then(function (d) { err.hidden = true; render(d); })
        .catch(function (e) { err.hidden = false; err.textContent = 'Could not load the metrics: ' + e.message; });
    });
  });
  render(data);
})();
</script>"#;

fn page(initial: &Value, app: Option<&str>, note: &str) -> String {
    // The application uuid was checked by the caller: letters, digits, `-` and `_` only.
    let app_attr = app
        .map(|a| format!(" data-app=\"{a}\""))
        .unwrap_or_default();
    format!(
        r#"{CSS}<div id="panel"{app_attr}></div>
<div class="bar"><div class="seg" role="group" aria-label="Window"><button type="button" data-window="30">30 days</button><button type="button" data-window="90">90 days</button></div><span class="muted" id="range"></span></div>
<p class="error note" id="error" hidden></p>
<div class="tiles" id="tiles"></div>
<div id="list"></div>
{note}
<script type="application/json" id="initial">{data}</script>
{SCRIPT}"#,
        data = embed(initial),
    )
}

/// The dashboard panel: the team's four metrics, its busiest applications, and where the exports are.
pub fn dashboard(initial: &Value) -> String {
    page(
        initial,
        None,
        r#"<p class="muted note">CSV exports with an API token: <code>/api/v1/ext/coolify-rs/delivery-metrics/metrics.csv</code>, <code>deployments.csv</code> and <code>inventory.csv</code>.</p>"#,
    )
}

/// An application's panel: its four metrics and its latest deployments. `uuid` must be valid
/// (`report::valid_uuid`).
pub fn application(uuid: &str, initial: &Value) -> String {
    page(initial, Some(uuid), "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn embedded_data_cannot_close_the_script() {
        let v = json!({"name": "</script><script>alert(1)</script>&amp;"});
        let e = embed(&v);
        assert!(!e.contains('<') && !e.contains('>') && !e.contains('&'));
        assert_eq!(serde_json::from_str::<Value>(&e).unwrap(), v);
        let html = dashboard(&v);
        assert_eq!(html.matches("</script>").count(), 2, "only the panel's own");
    }

    #[test]
    fn panels_carry_their_data_and_application() {
        let v = json!({"scope": "team", "window_days": 30});
        let html = dashboard(&v);
        assert!(html.contains(r#"<script type="application/json" id="initial">{"scope":"team","window_days":30}</script>"#));
        assert!(html.contains("data-window=\"90\""));
        assert!(html.contains("metrics.csv"));
        assert!(!html.contains("data-app=\""));
        let html = application("app-1", &v);
        assert!(html.contains(r#"<div id="panel" data-app="app-1">"#));
    }
}
