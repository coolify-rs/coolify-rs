//! Upstream-compatible Coolify REST API.
//!
//! The contract is documented in docs/API_COMPATIBILITY.md. Only routes listed
//! in [`routes::ROUTES`] are implemented; other upstream routes answer 501,
//! and unknown routes answer upstream's 404.

pub mod auth;
pub mod fallback;
pub mod git_urls;
pub mod input;
pub mod ip;
pub mod laravel_url;
pub mod response;
pub mod routes;
pub mod safe_url;
pub mod serialize;
pub mod session;
pub mod state;
pub mod ui;
pub mod ui_applications;
pub mod ui_databases;
pub mod ui_server_analytics;
pub mod ui_server_cloud;
pub mod ui_server_destinations;
pub mod ui_server_general;
pub mod ui_server_github_runners;
pub mod ui_server_metrics;
pub mod ui_server_ops;
pub mod ui_server_patches;
pub mod ui_server_proxy;
pub mod ui_server_tunnel;
pub mod validation;

pub use routes::router;
pub use state::AppState;
