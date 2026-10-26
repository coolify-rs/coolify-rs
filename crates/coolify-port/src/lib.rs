//! coolify-port: the upstream migration engine.
//!
//! Tracks upstream Coolify, classifies changes with path/content/message
//! evidence, maps them to Rust components, plans the port, regenerates
//! deterministic artifacts in an isolated branch, validates, performs bounded
//! repair through an optional external agent, and writes auditable reports.
//!
//! Upstream content is treated as untrusted data throughout: the engine never
//! executes upstream code, runs git with argument vectors only, and only runs
//! allowlisted validation programs.

pub mod classify;
pub mod config;
pub mod git;
pub mod inventory;
pub mod mapping;
pub mod php;
pub mod plan;
pub mod report;
pub mod run;
pub mod tracker;
pub mod validate;
