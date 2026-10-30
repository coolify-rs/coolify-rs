//! Applications: proxy labels, domains, the configuration hash, the deployment
//! queue and the deployment job (Docker image build pack).

pub mod compose;
mod deploy;
pub mod detect;
pub mod domains;
pub mod labels;
mod lifecycle;
pub mod ops;
pub mod queue;
pub mod snapshot;
pub mod status;
