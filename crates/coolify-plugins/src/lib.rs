//! coolify-rs plugins: WebAssembly components that observe events, keep their own data and serve their own
//! API routes, with only the capabilities an admin granted. docs/decisions/0010-plugins.md explains the
//! design; `wit/plugin.wit` is the interface.

pub mod data;
pub mod egress;
pub mod manifest;
pub mod portable;
pub mod runtime;
pub mod store;

pub use data::{Actor, DeployRequest, Deployer, ResourceKind, ResourceQuery, ResourceSource};
pub use manifest::{API_VERSION, Capabilities, Manifest};
pub use runtime::{CallError, Caller, Event, Host, Limits, Request, Response, Settings};
pub use store::Installed;

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("invalid manifest: {0}")]
    Manifest(String),
    #[error("invalid component: {0}")]
    Component(String),
    #[error("{0}")]
    Capability(String),
    #[error("plugin {0} is not installed")]
    NotFound(String),
    #[error("plugin engine: {0}")]
    Engine(String),
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
}
