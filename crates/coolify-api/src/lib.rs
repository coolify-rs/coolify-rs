//! Upstream-compatible Coolify REST API.
//!
//! The contract is documented in docs/API_COMPATIBILITY.md. Only routes listed
//! in [`routes::ROUTES`] are implemented; other upstream routes answer 501,
//! and unknown routes answer upstream's 404.

pub mod audit;
pub mod auth;
pub mod dns;
pub mod fallback;
pub mod git_urls;
pub mod input;
pub mod integration_token_actions;
pub mod ip;
pub mod laravel_url;
mod plugin_bridge;
pub mod response;
pub mod routes;
pub mod rules;
pub mod safe_url;
pub mod serialize;
pub mod server_delete;
pub mod server_transfer;
pub mod session;
pub mod state;
pub mod terminal_ws;
pub mod ui;
pub mod ui_applications;
pub mod ui_auth;
pub mod ui_boarding;
pub mod ui_canvas;
pub mod ui_changelog;
pub mod ui_database_backups;
pub mod ui_database_general;
pub mod ui_database_import;
pub mod ui_databases;
pub mod ui_fleet_metrics;
pub mod ui_help;
pub mod ui_layout;
pub mod ui_notifications;
pub mod ui_plugins;
pub mod ui_profile;
pub mod ui_security;
pub mod ui_server_analytics;
pub mod ui_server_cloud;
pub mod ui_server_destinations;
pub mod ui_server_general;
pub mod ui_server_github_runners;
pub mod ui_server_link;
pub mod ui_server_metrics;
pub mod ui_server_ops;
pub mod ui_server_patches;
pub mod ui_server_proxy;
pub mod ui_server_transfer;
pub mod ui_server_tunnel;
pub mod ui_services;
pub mod ui_settings;
pub mod ui_shared;
pub mod ui_shared_variables;
pub mod ui_sources;
pub mod ui_sources_routes;
pub mod ui_storages;
pub mod ui_subscription;
pub mod ui_tags;
pub mod ui_teams;
pub mod ui_terminal;
pub mod ui_volume_backups;
pub mod validation;
pub mod webhooks;

pub use routes::router;
pub use state::AppState;
