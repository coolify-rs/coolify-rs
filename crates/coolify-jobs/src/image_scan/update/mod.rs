//! Switching a resource to a fixed image version.
//!
//! | Module | Ports |
//! |---|---|
//! | [`resource_image`] | `App\Actions\ImageScan\UpdateResourceImage`, `ImageScan::updateCandidates()`, `Application::composeImageOverride()` and `activeComposeImageOverrides()` |
//! | [`image_start_pin`] | `App\Services\ImageScan\ImageStartPin` |
//! | [`dockerfile_base_image`] | `App\Services\ImageScan\DockerfileBaseImage` |
//!
//! The scan pages call [`handle`] ("Update to …", "Pull latest and restart") and
//! [`handle_parts`] (a merged update of a service) after they authorized `update` on the
//! resource, and [`blocked_reason`] to decide whether an update is offered. The starts
//! (`StartService`, the database starts, the deploy job) use up the pins of
//! [`image_start_pin`].

pub mod dockerfile_base_image;
pub mod image_start_pin;
pub mod resource_image;

pub use resource_image::{
    ImageOverride, ImageOverrides, UpdateError, UpdatePart, active_compose_image_overrides,
    blocked_reason, compose_image_override, handle, handle_parts, sub_resources_using,
    update_candidates,
};
