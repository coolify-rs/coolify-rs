//! Write coolify-rs plugins in Rust.
//!
//! ```ignore
//! use coolify_rs_plugin::{Plugin, Event, Caller, Request, Response};
//!
//! struct Hello;
//!
//! impl Plugin for Hello {
//!     fn on_event(event: Event) -> Result<(), String> {
//!         coolify_rs_plugin::log::info(&format!("{} for team {}", event.kind, event.team_id));
//!         Ok(())
//!     }
//! }
//!
//! coolify_rs_plugin::export!(Hello);
//! ```
//!
//! Build with `cargo build --release --target wasm32-wasip2`. The interface is
//! `crates/coolify-plugins/wit/plugin.wit` in the coolify-rs repository; this crate only wraps it.

#[doc(hidden)]
pub mod bindings {
    wit_bindgen::generate!({
        world: "plugin",
        path: "../../crates/coolify-plugins/wit",
        pub_export_macro: true,
        export_macro_name: "__export_plugin",
        default_bindings_module: "coolify_rs_plugin::bindings",
    });
}

pub use bindings::coolify_rs::plugin::types::{Caller, Event, Request, Response};

/// What a plugin implements. Both methods have defaults, so implement the ones the manifest's
/// capabilities use: `on_event` for `events`, `handle_request` for `routes`.
pub trait Plugin {
    fn on_event(event: Event) -> Result<(), String> {
        let _ = event;
        Ok(())
    }

    fn handle_request(caller: Caller, request: Request) -> Response {
        let _ = (caller, request);
        Response::json(404, &serde_json::json!({ "message": "Not found." }))
    }
}

#[doc(hidden)]
pub struct Adapter<P>(std::marker::PhantomData<P>);

impl<P: Plugin> bindings::Guest for Adapter<P> {
    fn on_event(event: Event) -> Result<(), String> {
        P::on_event(event)
    }

    fn handle_request(caller: Caller, request: Request) -> Response {
        P::handle_request(caller, request)
    }
}

/// Exports a type implementing [`Plugin`] as the component's entry points.
#[macro_export]
macro_rules! export {
    ($plugin:ty) => {
        type __CoolifyRsPluginAdapter = $crate::Adapter<$plugin>;
        $crate::bindings::__export_plugin!(__CoolifyRsPluginAdapter);
    };
}

impl Response {
    /// A response with a JSON body.
    pub fn json(status: u16, body: &serde_json::Value) -> Response {
        Response {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: serde_json::to_vec(body).unwrap_or_default(),
        }
    }
}

impl Event {
    /// The payload, parsed.
    pub fn data(&self) -> serde_json::Value {
        serde_json::from_str(&self.payload).unwrap_or_default()
    }
}

/// Capability-free logging into coolify-rs's log.
pub mod log {
    use crate::bindings::coolify_rs::plugin::log::{Level, write};

    pub fn debug(message: &str) {
        write(Level::Debug, message);
    }
    pub fn info(message: &str) {
        write(Level::Info, message);
    }
    pub fn warn(message: &str) {
        write(Level::Warn, message);
    }
    pub fn error(message: &str) {
        write(Level::Error, message);
    }
}

/// Capability `storage`: the plugin's key-value data for the team of the current call.
pub mod storage {
    pub use crate::bindings::coolify_rs::plugin::storage::{delete, get, keys, set};

    /// A stored JSON value, or `None` when the key is missing or not JSON.
    pub fn get_json<T: serde::de::DeserializeOwned>(key: &str) -> Result<Option<T>, String> {
        Ok(get(key)?.and_then(|v| serde_json::from_slice(&v).ok()))
    }

    pub fn set_json<T: serde::Serialize>(key: &str, value: &T) -> Result<(), String> {
        set(key, &serde_json::to_vec(value).map_err(|e| e.to_string())?)
    }
}

/// The team's settings: the `[[settings]]` of the manifest, as the team of the current call set them under
/// Team → Plugins (docs/PLUGINS.md, "Settings"). Only a plugin whose manifest declares settings may use it;
/// coolify-rs refuses to load a component that imports it otherwise.
///
/// ```ignore
/// let Some(topic) = coolify_rs_plugin::config::get("topic") else {
///     return Ok(()); // Not configured for this team yet.
/// };
/// let server = coolify_rs_plugin::config::get_or("server", "https://ntfy.sh");
/// ```
///
/// Secrets come back in clear: only the plugin sees them. Never log them or return them from a route.
pub mod config {
    /// The team's value of `key`, or the manifest's default; `None` when it is unset without a default
    /// (a required setting can be unset too) or when the manifest declares no such key.
    pub use crate::bindings::coolify_rs::plugin::config::get;

    /// The team's value of `key`, or `default` when there is none.
    pub fn get_or(key: &str, default: &str) -> String {
        get(key).unwrap_or_else(|| default.to_owned())
    }

    /// The team's value of `key`, or an error saying the setting is not configured, for `?` in `on_event`.
    pub fn require(key: &str) -> Result<String, String> {
        get(key).ok_or_else(|| format!("the setting {key} is not configured for this team"))
    }
}

/// Capability `http`: HTTPS requests to the hosts in the manifest.
pub mod http {
    pub use crate::bindings::coolify_rs::plugin::http::{Outgoing, fetch};
}

/// Capability `ui`: panels the web UI shows in its slots (docs/PLUGINS.md, "UI panels").
///
/// A panel is the HTML answer of one of the plugin's `GET` routes. coolify-rs shows it in a sandboxed
/// frame with no network access: the panel's scripts can't fetch anything, and reach the plugin's own
/// routes only through [`BRIDGE`], which asks the page to make the request for them.
///
/// ```ignore
/// ("GET", "/panel") => coolify_rs_plugin::ui::panel("<h2>Deployments</h2><p id=n>…</p><script>
///     coolify.json('GET', '/stats').then(s => n.textContent = s.succeeded);
/// </script>"),
/// ```
pub mod ui {
    use crate::Response;

    /// The panel side of the message bridge. It defines `window.coolify`:
    ///
    /// - `coolify.request(method, path, body?)` resolves to `{status, contentType, body}` for a request to
    ///   the plugin's own route `path` (relative to its prefix, starting with `/`); `body` is a string, or
    ///   an object sent as JSON. `coolify.json(...)` does the same and parses the answer.
    /// - `coolify.theme` is `"light"` or `"dark"`; the root element gets the `dark` class to match the
    ///   page, and `coolify.onTheme = (theme) => ...` hears about changes.
    ///
    /// It also tells the page the panel's height, so the frame fits its content.
    pub const BRIDGE: &str = r#"<script>
(function () {
  var next = 0, waiting = {}, root = document.documentElement;
  function send(m) { parent.postMessage(m, '*'); }
  var coolify = window.coolify = {
    theme: 'light',
    onTheme: null,
    request: function (method, path, body) {
      return new Promise(function (resolve, reject) {
        var id = ++next;
        waiting[id] = { resolve: resolve, reject: reject };
        if (body !== undefined && body !== null && typeof body !== 'string') body = JSON.stringify(body);
        send({ type: 'coolify-rs:request', id: id, method: method, path: path, body: body === undefined ? null : body });
      });
    },
    json: function (method, path, body) {
      return coolify.request(method, path, body).then(function (r) {
        if (r.status < 200 || r.status > 299) throw new Error('HTTP ' + r.status + ': ' + r.body);
        return JSON.parse(r.body);
      });
    }
  };
  window.addEventListener('message', function (e) {
    if (e.source !== parent || !e.data) return;
    var m = e.data;
    if (m.type === 'coolify-rs:theme') {
      coolify.theme = m.theme === 'dark' ? 'dark' : 'light';
      root.classList.toggle('dark', coolify.theme === 'dark');
      root.style.colorScheme = coolify.theme;
      if (typeof coolify.onTheme === 'function') coolify.onTheme(coolify.theme);
    } else if (m.type === 'coolify-rs:response' && waiting[m.id]) {
      var w = waiting[m.id];
      delete waiting[m.id];
      if (m.error) w.reject(new Error(m.error));
      else w.resolve({ status: m.status, contentType: m.contentType, body: m.body });
    }
  });
  // The content's height, not the frame's: scrollHeight never drops below the frame.
  function size() { send({ type: 'coolify-rs:resize', height: Math.ceil(root.getBoundingClientRect().height) }); }
  if (window.ResizeObserver) new ResizeObserver(size).observe(root);
  window.addEventListener('load', size);
  send({ type: 'coolify-rs:ready' });
})();
</script>"#;

    /// Plain styles close to coolify-rs's own: system font, its text colours in both themes, a
    /// transparent background, and `.stat`/`.muted` helpers. Web fonts and images from URLs can't load in
    /// a panel.
    pub const STYLE: &str = r#"<style>
:root { color-scheme: light; font: 13px/1.5 ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif; color: #404040; }
:root.dark { color-scheme: dark; color: #c4c4c4; }
html, body { margin: 0; background: transparent; }
h1, h2, h3 { margin: 0 0 .5rem; font-size: 13px; font-weight: 600; color: #000; }
.dark h1, .dark h2, .dark h3 { color: #f4f4f4; }
.muted { color: #737373; }
.dark .muted { color: #8f8f8f; }
.stats { display: flex; flex-wrap: wrap; gap: .75rem; }
.stat { min-width: 8rem; padding: .625rem .75rem; border: 1px solid #e5e5e5; border-radius: .75rem; background: #fff; }
.dark .stat { border-color: rgb(255 255 255 / .08); background: rgb(255 255 255 / .05); }
.stat b { display: block; font-size: 20px; line-height: 1.2; color: #000; font-variant-numeric: tabular-nums; }
.dark .stat b { color: #f4f4f4; }
</style>"#;

    /// A panel response: [`STYLE`], `body`, then [`BRIDGE`], as `text/html`.
    pub fn panel(body: &str) -> Response {
        Response::html(200, &format!("{STYLE}{body}{BRIDGE}"))
    }
}

impl Response {
    /// A response with an HTML body. Only panels are shown as HTML; under `/api/v1/ext` coolify-rs serves
    /// HTML as a download so it never runs on coolify-rs's origin.
    pub fn html(status: u16, body: &str) -> Response {
        Response {
            status,
            headers: vec![("content-type".into(), "text/html; charset=utf-8".into())],
            body: body.as_bytes().to_vec(),
        }
    }
}

/// Capability `resources`: the team's resources, read-only, as the REST API shows them to a token without
/// `read:sensitive`. Each function needs its kind in the manifest's `resources` list.
pub mod resources {
    pub use crate::bindings::coolify_rs::plugin::resources::{Kind, all, environments, get};

    fn parse(text: String) -> Result<serde_json::Value, String> {
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }

    /// Every resource of `kind`, as `GET /api/v1/<kind>` lists them.
    pub fn list(kind: Kind) -> Result<Vec<serde_json::Value>, String> {
        match parse(all(kind)?)? {
            serde_json::Value::Array(items) => Ok(items),
            other => Err(format!("expected a list, got {other}")),
        }
    }

    /// One resource, as `GET /api/v1/<kind>/<uuid>` shows it; `None` when the team has no such resource.
    pub fn find(kind: Kind, uuid: &str) -> Result<Option<serde_json::Value>, String> {
        get(kind, uuid)?.map(parse).transpose()
    }

    pub fn projects() -> Result<Vec<serde_json::Value>, String> {
        list(Kind::Projects)
    }
    pub fn servers() -> Result<Vec<serde_json::Value>, String> {
        list(Kind::Servers)
    }
    pub fn applications() -> Result<Vec<serde_json::Value>, String> {
        list(Kind::Applications)
    }
    pub fn services() -> Result<Vec<serde_json::Value>, String> {
        list(Kind::Services)
    }
    pub fn databases() -> Result<Vec<serde_json::Value>, String> {
        list(Kind::Databases)
    }
}

/// Capability `deploy`: queue deployments of the team's applications, as `POST /api/v1/deploy` does.
///
/// From a route, the deployment runs on the caller's authority: it fails unless the caller's token could
/// deploy the application through the API. From an event, it runs on the authority of the team admin who
/// granted `deploy` to the plugin. Either way the team must have granted `deploy` when enabling the plugin,
/// and coolify-rs limits how many deployments a team's plugins may queue.
pub mod deploy {
    pub use crate::bindings::coolify_rs::plugin::deploy::application;

    /// Queues a deployment and returns the API's answer, parsed.
    pub fn queue(uuid: &str, force: bool) -> Result<serde_json::Value, String> {
        let text = application(uuid, force)?;
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }
}
