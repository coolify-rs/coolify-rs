//! `coolify-rs/delivery-metrics`: the four DORA metrics (deployment frequency, change failure rate, time to
//! restore, lead time for changes) per team and per application, from the team's deployment events, with
//! CSV exports of the metrics, the deployments and the team's inventory. README.md describes it for users.
//!
//! Everything but the component's entry points is plain Rust, tested natively with `cargo test`:
//!
//! - [`record`]: a deployment from an event;
//! - [`store`]: monthly shards in the plugin's storage, retention and the size cap;
//! - [`metrics`]: the metrics and their DORA bands;
//! - [`report`]: the routes' JSON and CSV;
//! - [`csv`]: RFC 4180 with formula-injection protection;
//! - [`panels`]: the UI panels' HTML.

pub mod csv;
pub mod metrics;
pub mod panels;
pub mod record;
pub mod report;
pub mod store;
pub mod time;

#[cfg(target_arch = "wasm32")]
mod plugin;
