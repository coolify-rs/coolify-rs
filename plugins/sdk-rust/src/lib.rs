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

/// Capability `http`: HTTPS requests to the hosts in the manifest.
pub mod http {
    pub use crate::bindings::coolify_rs::plugin::http::{Outgoing, fetch};
}
