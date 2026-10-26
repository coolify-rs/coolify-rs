//! Upstream-compatible Coolify REST API.
//!
//! The contract is documented in docs/API_COMPATIBILITY.md. Only routes listed
//! in [`routes::ROUTES`] are implemented; other upstream routes answer 501,
//! and unknown routes answer upstream's 404.

pub mod auth;
pub mod fallback;
pub mod input;
pub mod ip;
pub mod response;
pub mod routes;
pub mod serialize;
pub mod state;
pub mod validation;

pub use routes::router;
pub use state::AppState;
