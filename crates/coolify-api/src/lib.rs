//! Upstream-compatible Coolify REST API.
//!
//! The contract is documented in docs/API_COMPATIBILITY.md. Only routes listed
//! in [`routes::ROUTES`] are implemented; other upstream routes answer 501,
//! and unknown routes answer upstream's 404.

pub mod audit;
pub mod auth;
pub mod fallback;
pub mod git_urls;
pub mod input;
pub mod ip;
pub mod laravel_url;
pub mod response;
pub mod routes;
pub mod rules;
pub mod safe_url;
pub mod serialize;
pub mod session;
pub mod state;
pub mod ui;
pub mod ui_applications;
pub mod ui_databases;
pub mod ui_notifications;
pub mod ui_security;
pub mod ui_shared_variables;
pub mod ui_storages;
pub mod ui_tags;
pub mod validation;

pub use routes::router;
pub use state::AppState;
