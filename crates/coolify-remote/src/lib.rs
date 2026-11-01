//! Remote execution on managed servers (ADR 0009).
//!
//! The only place in coolify-rs that spawns `ssh`. Semantics follow upstream's
//! `instant_remote_process`; the security controls are documented in
//! SECURITY_MODEL.md section 7.

pub mod exec;
pub mod retry;
pub mod shell;
pub mod sudo;
pub mod text;

pub use exec::{
    HostKeyPolicy, PrivateKey, Remote, RemoteConfig, RemoteError, RunOptions, SetupError, Stream,
    Tap, Target, TtyInvocation,
};
