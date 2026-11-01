//! Applications: proxy labels, domains, the configuration hash, the deployment
//! queue and the deployment job (Docker image build pack).

pub mod compose;
mod deploy;
pub use deploy::compose_pack::{inject_build_args, inject_compose_flags};
pub mod detect;
pub mod docker_run;
pub mod domains;
pub mod labels;
mod lifecycle;
pub mod ops;
pub mod previews;
pub mod queue;
pub mod snapshot;
pub mod status;
