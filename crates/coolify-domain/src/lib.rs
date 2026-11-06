//! Domain types and invariants shared by every coolify-rs layer.
//!
//! Nothing here performs I/O. Behaviour mirrors upstream Coolify; the
//! upstream source of each rule is cited next to it.

pub mod access;
pub mod ids;
pub mod naming;
pub mod plugin_events;

pub use access::{Abilities, Ability, Role};
pub use ids::{ROOT_TEAM_ID, TeamId, new_public_id};
