//! Session endpoints of the image scanning pages (upstream `App\Livewire\ImageScan\*` and
//! `App\Livewire\Server\Security\HostVulnerabilities`):
//!
//! - [`vulnerabilities`]: the Security section of an application, service, or database
//!   (`ImageScan\Vulnerabilities`), with its actions and exports.
//! - [`packages`]: "Packages and SBOM" (`ImageScan\Packages`).
//! - [`settings`]: scan rules and ignores of a level (`ImageScan\ScanSettings`).
//! - [`server`]: the Vulnerabilities page of a server (`ImageScan\ServerImageScanning`) and its
//!   host package findings (`Server\Security\HostVulnerabilities`).
//! - [`fleet`]: the team Security page (`ImageScan\FleetVulnerabilities`) and the security status
//!   of one resource for the menus (`SecurityStatus::stored()`).
//!
//! coolify-rs has no push channel to the browser, so the pages poll instead of listening for
//! `ImageScanUpdated`. The view models are `coolify_jobs::image_scan::views`; they need the
//! background jobs, so these endpoints answer 501 without them.

pub mod fleet;
pub mod packages;
pub mod server;
pub mod settings;
pub mod vulnerabilities;

use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::NaiveDateTime;
use coolify_db::image_scans::{self, ScanOwner};
use coolify_domain::grants::AccessLevel;
use coolify_jobs::Jobs;
use serde_json::{Value, json};

use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, ok};
use crate::routes::vulnerabilities::can_access;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(vulnerabilities::routes())
        .merge(packages::routes())
        .merge(settings::routes())
        .merge(server::routes())
        .merge(fleet::routes())
}

/// `ResourceScans::RESOURCE_TYPES`.
pub(crate) const RESOURCE_TYPES: [&str; 3] = ["application", "service", "database"];

pub(crate) fn jobs(state: &AppState) -> Result<&Jobs, ApiError> {
    state.jobs.as_ref().ok_or(ApiError::NotPorted)
}

pub(crate) fn internal(e: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %e, "image scanning page request failed");
    ApiError::Internal
}

pub(crate) fn not_found() -> ApiError {
    ApiError::Abort(StatusCode::NOT_FOUND, "Not Found")
}

pub(crate) fn forbidden() -> ApiError {
    ApiError::Abort(StatusCode::FORBIDDEN, "This action is unauthorized.")
}

/// `$this->dispatch('error', $message)`: the error toast of a page action.
pub(crate) fn toast_error(message: impl Into<String>) -> ApiResult {
    crate::ui_server_destinations::toast_error(message)
}

/// `$this->dispatch('success', $message)`.
pub(crate) fn toast_success(message: &str) -> ApiResult {
    Ok(ok(json!({"message": message})).into_response())
}

/// `response()->streamDownload()` of a page action: the browser saves `content` as `name`.
pub(crate) fn download(name: String, mime: &str, content: String) -> ApiResult {
    Ok(ok(json!({"name": name, "mime": mime, "content": content})).into_response())
}

/// `Vulnerabilities::resource()` and `Packages::resource()`: the resource of a Security page by
/// type and uuid, only in the session team, and the `view` policy.
pub(crate) async fn load_resource(
    state: &AppState,
    ctx: &AuthCtx,
    kind: &str,
    uuid: &str,
) -> Result<ScanOwner, ApiError> {
    if !RESOURCE_TYPES.contains(&kind) {
        return Err(not_found());
    }
    let resource = image_scans::find_resource(&state.db, kind, uuid, &[ctx.team_id])
        .await?
        .ok_or_else(not_found)?;
    if !can_access(ctx, &resource, AccessLevel::Read) {
        return Err(forbidden());
    }
    Ok(resource)
}

/// `number_format($value)`: thousands separated by commas.
pub(crate) fn number_format(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `trans_choice('{1} 1 finding|[2,*] :count findings', $count)` and `str($noun)->plural($count)`
/// with `number_format()`.
pub(crate) fn count_text(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{} {plural}", number_format(count))
    }
}

/// Carbon `toDayDateTimeString()`: "Tue, Nov 10, 2026 12:00 PM".
pub(crate) fn day_date_time(time: NaiveDateTime) -> String {
    time.format("%a, %b %-d, %Y %-I:%M %p").to_string()
}

/// Carbon `toFormattedDateString()`: "Nov 10, 2026".
pub(crate) fn formatted_date(time: NaiveDateTime) -> String {
    time.format("%b %-d, %Y").to_string()
}

/// `formatBytes($bytes, $precision)`.
pub(crate) fn format_bytes(bytes: i64, precision: i32) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes <= 0 {
        return "0 B".to_owned();
    }
    let exponent = ((bytes as f64).ln() / 1024f64.ln()).floor() as usize;
    let exponent = exponent.min(UNITS.len() - 1);
    let value = bytes as f64 / 1024f64.powi(exponent as i32);
    let factor = 10f64.powi(precision);
    let rounded = (value * factor).round() / factor;
    let text = if rounded.fract() == 0.0 {
        format!("{}", rounded as i64)
    } else {
        format!("{rounded}")
    };
    format!("{text} {}", UNITS[exponent])
}

/// `Str::slug($name) ?: $fallback`.
pub(crate) fn slug_or(name: &str, fallback: &str) -> String {
    let slug = coolify_jobs::compose_parser::php::slug(name);
    if slug.is_empty() {
        fallback.to_owned()
    } else {
        slug
    }
}

/// `strnatcmp()`: digit runs compare as numbers.
pub(crate) fn strnatcmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let si = i;
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            let sj = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let na = std::str::from_utf8(&a[si..i])
                .unwrap_or("0")
                .trim_start_matches('0');
            let nb = std::str::from_utf8(&b[sj..j])
                .unwrap_or("0")
                .trim_start_matches('0');
            let ord = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
            if ord != Ordering::Equal {
                return ord;
            }
        } else {
            let ord = a[i].cmp(&b[j]);
            if ord != Ordering::Equal {
                return ord;
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

/// `strnatcasecmp()`.
pub(crate) fn strnatcasecmp(a: &str, b: &str) -> std::cmp::Ordering {
    strnatcmp(&a.to_lowercase(), &b.to_lowercase())
}

/// `VulnerabilityMarkers::for($resources)` for the lists of a page (`Resource\Index`): adds
/// `security` (the marker, or null) to every item, found by its `uuid`. `groups` are the items
/// with their page type (`application`, `service`, `database`).
pub(crate) async fn add_security_markers(
    state: &AppState,
    team_id: i64,
    groups: &mut [(&str, &mut Vec<Value>)],
) -> Result<(), ApiError> {
    let Some(jobs) = state.jobs.as_ref() else {
        return Ok(());
    };
    let mut owners: Vec<ScanOwner> = Vec::new();
    let mut keys: Vec<Vec<Option<String>>> = Vec::new();
    for (kind, items) in groups.iter() {
        let mut group_keys = Vec::new();
        for item in items.iter() {
            let uuid = item.get("uuid").and_then(Value::as_str).unwrap_or_default();
            let owner = if uuid.is_empty() {
                None
            } else {
                image_scans::find_resource(&state.db, kind, uuid, &[team_id]).await?
            };
            group_keys.push(owner.as_ref().map(ScanOwner::key));
            owners.extend(owner);
        }
        keys.push(group_keys);
    }
    let markers = coolify_jobs::image_scan::views::markers::markers_for(jobs, &owners)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "the security markers could not be read");
            Default::default()
        });
    for ((_, items), group_keys) in groups.iter_mut().zip(keys) {
        for (item, key) in items.iter_mut().zip(group_keys) {
            let marker = key
                .and_then(|k| markers.get(&k))
                .and_then(|m| serde_json::to_value(m).ok())
                .unwrap_or(Value::Null);
            if let Value::Object(map) = item {
                map.insert("security".to_owned(), marker);
            }
        }
    }
    Ok(())
}

/// `VulnerabilityMarkers::for($resources)` of resources given by morph class and id: the marker
/// of each, keyed by `ResourceScans::key()` (`{morph}:{id}`). Resources without a marker have no
/// entry; empty without the background jobs.
pub(crate) async fn markers_by_key(
    state: &AppState,
    resources: &[(&str, i64)],
) -> std::collections::HashMap<String, Value> {
    let Some(jobs) = state.jobs.as_ref() else {
        return Default::default();
    };
    let mut owners = Vec::new();
    for (morph, id) in resources {
        if let Ok(Some(owner)) = image_scans::find_owner(&state.db, morph, *id).await {
            owners.push(owner);
        }
    }
    match coolify_jobs::image_scan::views::markers::markers_for(jobs, &owners).await {
        Ok(markers) => markers
            .into_iter()
            .filter_map(|(k, m)| serde_json::to_value(m).ok().map(|v| (k, v)))
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "the security markers could not be read");
            Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    #[test]
    fn numbers_and_counts_read_like_php() {
        assert_eq!(number_format(0), "0");
        assert_eq!(number_format(999), "999");
        assert_eq!(number_format(1000), "1,000");
        assert_eq!(number_format(1234567), "1,234,567");
        assert_eq!(count_text(1, "finding", "findings"), "1 finding");
        assert_eq!(count_text(0, "finding", "findings"), "0 findings");
        assert_eq!(count_text(1200, "finding", "findings"), "1,200 findings");
    }

    #[test]
    fn dates_read_like_carbon() {
        let t = NaiveDate::from_ymd_opt(2026, 11, 10)
            .unwrap()
            .and_hms_opt(14, 5, 0)
            .unwrap();
        assert_eq!(day_date_time(t), "Tue, Nov 10, 2026 2:05 PM");
        assert_eq!(formatted_date(t), "Nov 10, 2026");
    }

    #[test]
    fn bytes_read_like_format_bytes() {
        assert_eq!(format_bytes(0, 0), "0 B");
        assert_eq!(format_bytes(1024 * 1024 * 1024, 0), "1 GB");
        assert_eq!(format_bytes(1536 * 1024 * 1024, 1), "1.5 GB");
        assert_eq!(format_bytes(2048 * 1024 * 1024, 0), "2 GB");
    }

    #[test]
    fn natural_order() {
        use std::cmp::Ordering;
        assert_eq!(strnatcmp("1.10", "1.9"), Ordering::Greater);
        assert_eq!(strnatcmp("a2", "a10"), Ordering::Less);
        assert_eq!(strnatcasecmp("Web", "api"), Ordering::Greater);
        assert_eq!(strnatcmp("x", "x"), Ordering::Equal);
    }
}
