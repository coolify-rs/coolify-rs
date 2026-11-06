//! `coolify-rs/alerts`: forwards failures (and, if a team wants, successes) to services Coolify does not
//! notify: ntfy.sh, Google Chat, PagerDuty (Events API v2) and Opsgenie (Alerts API, US and EU).
//!
//! The work is split so most of it is plain Rust, tested natively with `cargo test -p alerts` in `plugins/`:
//!
//! - [`event`] turns an event's payload into an [`event::Alert`]: a title, a few lines, a link, a severity,
//!   and the incident key PagerDuty and Opsgenie deduplicate on;
//! - [`deliver`] decides, from the team's settings and the incidents this plugin opened, which services get
//!   which requests, and builds those requests;
//! - `plugin` (wasm32 only) is the glue: it reads the settings, sends the requests with the `http`
//!   capability, keeps the open incidents in `storage`, and serves the routes and the panel.
//!
//! Routes, under `/api/v1/ext/coolify-rs/alerts` (and `/ui/ext/...` for the panel's bridge):
//! - `GET /status`: which services are configured (never their values), what is sent, and how many incidents
//!   are open;
//! - `POST /test`: sends a test alert to every configured service and answers each one's result. PagerDuty
//!   and Opsgenie get the alert and its resolution right after, so nobody stays paged. Needs `write`;
//! - `GET /panel`: the dashboard panel with a "Send test alert" button.

pub mod deliver;
pub mod event;
pub mod text;

#[cfg(target_arch = "wasm32")]
mod plugin;
