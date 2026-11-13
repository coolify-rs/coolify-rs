//! The check for newer versions of a scanned image that fix its findings (spec 7.2 to 7.5, 15.4).
//!
//! | Module | Ports |
//! |---|---|
//! | [`tag_selector`] | `App\Services\ImageScan\TagSelector` |
//! | [`evaluator`] | `App\Services\ImageScan\CandidateEvaluator` |
//! | [`base_image_check`] | `App\Services\ImageScan\BaseImageCheck` |
//! | [`refresh`] | `App\Actions\ImageScan\RefreshCandidates` |
//! | [`job`] | `App\Jobs\RefreshCandidatesJob` |
//! | `registry_login` | `DockerRegistryLogins::registryAuthConfig()` |
//!
//! The check writes `image_scans.candidates`:
//! `{status, checked_at, error, tags_truncated, entries, base?}`, where `base` (the base image of
//! a Dockerfile build) is `{image, status, checked_at, error, tags_truncated, attributable,
//! attributable_ids, current_digest, entries}` and an entry is `{ref, digest, kind, better,
//! fixed_ids, fixes, added, left, eol, scanned_at}` (a scanned `major` entry also has
//! `fixes_problems`). Statuses: `ok`, `no_better`, `cannot_compare`, `auth_required`, `failed`,
//! and `skipped` when only the base image was checked. Kinds: `same_tag`, `line`, `major`.

pub mod base_image_check;
pub mod evaluator;
pub mod job;
pub mod refresh;
mod registry_login;
pub mod tag_selector;

pub use base_image_check::{BaseImageUpdate, BetterBase, base_image, better_base};
pub use job::queue_check;
pub use refresh::{
    DAILY_CANDIDATE_SCANS, FRESH_HOURS, MAJOR_SCAN_DAILY_RESERVE, MAX_CANDIDATE_SCANS,
    RefreshError, forget_queued, is_fresh, is_queued, mark_queued, message, message_on,
    needs_candidates, relevant_findings, run, should_refresh,
};
