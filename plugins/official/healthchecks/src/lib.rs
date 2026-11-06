//! `coolify-rs/healthchecks`: pings Healthchecks.io for every run of a scheduled task and of a database or
//! storage backup, so a job that silently stops running raises an alarm there.
//!
//! - [`ping`] (plain Rust, tested natively with `cargo test -p healthchecks` in `plugins/`) turns events
//!   into pings: one check per job, named by a slug derived from the payload, a success or `/fail` ping with
//!   a short body, and the bounded log of recent pings the panel shows;
//! - `plugin` (wasm32 only) reads the settings, sends the pings with the `http` capability, keeps the log in
//!   `storage`, and serves it.
//!
//! Routes, under `/api/v1/ext/coolify-rs/healthchecks` (and `/ui/ext/...` for the panel's bridge):
//! - `GET /recent`: the checks pinged recently, newest first, with their last status;
//! - `GET /panel`: the same as the dashboard panel.

pub mod ping;

#[cfg(target_arch = "wasm32")]
mod plugin;
