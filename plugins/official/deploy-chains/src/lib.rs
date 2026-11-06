//! Deploy chains: when an application deploys successfully, deploy the applications that depend on it.
//!
//! [`chain`] holds every decision (rule validation, loops, what an event deploys, the history) as plain
//! functions, tested natively with `cargo test -p deploy-chains`. The plugin interface (`plugin.rs`) only
//! exists in WebAssembly, so it is compiled for `wasm32` only.

pub mod chain;

#[cfg(target_arch = "wasm32")]
mod plugin;
