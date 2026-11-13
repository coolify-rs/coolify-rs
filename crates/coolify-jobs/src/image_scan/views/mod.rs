//! The view models of image scanning (spec 15): what the Security pages, the dashboard callout,
//! the resource lists, the notifications, and the vulnerability API read. Ports
//! `App\Services\ImageScan\{SecurityStatus, ResourceSecurity, SecurityAction, SecurityActions,
//! SecuritySentences, SecurityCard, TeamVulnerabilities, VulnerabilityMarkers, FindingsExport,
//! FleetExport, ExportCells}` and the enums `SecurityStatusType` and `SecurityActionType`.
//!
//! | Module | Ports | Main entry points |
//! |---|---|---|
//! | this file | `SecurityStatusType`, `SecurityActionType`, Carbon helpers | [`SecurityStatusType`], [`SecurityActionType`], [`diff_for_humans`], [`iso8601`] |
//! | [`action`] | `SecurityAction` | [`SecurityAction::to_value`], [`SecurityAction::from_value`] |
//! | [`resource_security`] | `ResourceSecurity` | [`ResourceSecurity::to_value`], [`ResourceSecurity::from_value`], [`Why`] |
//! | [`actions`] | `SecurityActions` | [`actions::for_scan`], [`actions::fix_states`], [`actions::image_kind`] |
//! | [`status`] | `SecurityStatus` | [`status::for_scan`], [`status::for_resource`], [`status::stored`], [`status::stored_many`], [`status::store`], [`status::versions_checked`], [`status::top_why`] |
//! | [`sentences`] | `SecuritySentences` | [`sentences::headline`], [`sentences::badge`], [`sentences::image_sentence`], … |
//! | [`card`] | `SecurityCard` | [`card::card`], [`card::images`], [`card::gate_action`], [`card::blocked_image_action`] |
//! | [`team`] | `TeamVulnerabilities` | [`team::TeamVulnerabilities::load`], [`team::update_now_resources`], [`team::vulnerabilities_path`] |
//! | [`markers`] | `VulnerabilityMarkers` | [`markers::markers_for`], [`markers::marker`] |
//! | [`export`] | `ExportCells`, `FindingsExport`, `FleetExport` | [`export::findings_csv`], [`export::fleet_json`], … |
//!
//! A scan and its owner travel together: upstream reads `$scan->resource`, here every function
//! that needs it takes the owner as well ([`OwnedScan`] for lists). Functions that read the
//! database are `async`, take `jobs: &Jobs` first, and return `Result<T, String>`.
//!
//! Times are UTC `NaiveDateTime`s (the application time zone of upstream is UTC); upstream's
//! `toIso8601String()` is [`iso8601`].

pub mod action;
pub mod actions;
pub mod card;
pub mod export;
pub mod markers;
pub mod resource_security;
pub mod sentences;
pub mod status;
pub mod team;

use chrono::{DateTime, Datelike, NaiveDateTime, Timelike};
use coolify_db::image_scans::{ImageScanRow, ScanOwner};

pub use action::{ActionPart, SecurityAction};
pub use resource_security::{ResourceSecurity, Why};

/// `App\Enums\SecurityStatusType`: the security status of a resource or an image (spec 15.6).
/// The cases are in the order the rules are applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecurityStatusType {
    NotChecked,
    CheckNeeded,
    UpdateNow,
    UpdateAvailable,
    NoFix,
    Ok,
}

impl SecurityStatusType {
    pub const ALL: [SecurityStatusType; 6] = [
        SecurityStatusType::NotChecked,
        SecurityStatusType::CheckNeeded,
        SecurityStatusType::UpdateNow,
        SecurityStatusType::UpdateAvailable,
        SecurityStatusType::NoFix,
        SecurityStatusType::Ok,
    ];

    pub fn value(self) -> &'static str {
        match self {
            SecurityStatusType::NotChecked => "not_checked",
            SecurityStatusType::CheckNeeded => "check_needed",
            SecurityStatusType::UpdateNow => "update_now",
            SecurityStatusType::UpdateAvailable => "update_available",
            SecurityStatusType::NoFix => "no_fix",
            SecurityStatusType::Ok => "ok",
        }
    }

    /// `SecurityStatusType::tryFrom($value)`.
    pub fn from_value(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.value() == value)
    }

    /// `update_now` or `update_available`.
    pub fn needs_update(self) -> bool {
        matches!(
            self,
            SecurityStatusType::UpdateNow | SecurityStatusType::UpdateAvailable
        )
    }
}

/// `App\Enums\SecurityActionType`: what fixes a relevant finding of a scanned image (spec 15.5).
/// The cases are in the order the actions are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SecurityActionType {
    UpdateImage,
    PullLatest,
    ChangeBase,
    Rebuild,
    UpgradeInDockerfile,
    UpgradePackage,
    RebuildInCi,
}

impl SecurityActionType {
    pub const ALL: [SecurityActionType; 7] = [
        SecurityActionType::UpdateImage,
        SecurityActionType::PullLatest,
        SecurityActionType::ChangeBase,
        SecurityActionType::Rebuild,
        SecurityActionType::UpgradeInDockerfile,
        SecurityActionType::UpgradePackage,
        SecurityActionType::RebuildInCi,
    ];

    pub fn value(self) -> &'static str {
        match self {
            SecurityActionType::UpdateImage => "update_image",
            SecurityActionType::PullLatest => "pull_latest",
            SecurityActionType::ChangeBase => "change_base",
            SecurityActionType::Rebuild => "rebuild",
            SecurityActionType::UpgradeInDockerfile => "upgrade_in_dockerfile",
            SecurityActionType::UpgradePackage => "upgrade_package",
            SecurityActionType::RebuildInCi => "rebuild_in_ci",
        }
    }

    /// `SecurityActionType::tryFrom($value)`.
    pub fn from_value(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.value() == value)
    }

    /// The position in `SecurityActionType::cases()`.
    pub fn order(self) -> usize {
        Self::ALL
            .iter()
            .position(|t| *t == self)
            .unwrap_or(Self::ALL.len())
    }
}

/// A scan with its owner (`$scan` with `$scan->resource` set).
#[derive(Debug, Clone)]
pub struct OwnedScan {
    pub scan: ImageScanRow,
    pub owner: ScanOwner,
}

/// `ResourceScans::current($resource)` with the owner of each scan (`$scan->resource` set): the
/// current scans of every owner of a resource page, owner by owner.
pub async fn current_owned(
    jobs: &crate::Jobs,
    resource: &ScanOwner,
) -> Result<Vec<OwnedScan>, String> {
    let db = &jobs.0.db;
    let mut out = Vec::new();
    for owner in coolify_db::image_scans::owners(db, resource)
        .await
        .map_err(|e| e.to_string())?
    {
        for scan in super::resource_scans::current_for(db, &owner).await? {
            out.push(OwnedScan {
                scan,
                owner: owner.clone(),
            });
        }
    }
    Ok(out)
}

/// `now()`: the time the view models compare with, in UTC, without fractions of a second.
pub fn now() -> NaiveDateTime {
    crate::Jobs::now()
}

/// `CarbonImmutable::toIso8601String()` of a UTC time: `2026-11-10T12:00:00+00:00`.
pub fn iso8601(time: NaiveDateTime) -> String {
    time.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

/// `CarbonImmutable::parse($value)` of a stored time, as UTC. `None` when it cannot be read.
pub fn parse_time(value: &str) -> Option<NaiveDateTime> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(at) = DateTime::parse_from_rfc3339(value) {
        return Some(at.naive_utc());
    }
    [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
    ]
    .iter()
    .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok())
    .or_else(|| {
        chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
    })
}

/// `CarbonImmutable::parse($time)->diffForHumans()` relative to `now`: "2 hours ago", "1 day
/// ago", "3 weeks from now". Carbon's defaults: one part, the count cut (not rounded), and never
/// zero ("1 second ago" for the same time).
pub fn diff_for_humans(time: NaiveDateTime, now: NaiveDateTime) -> String {
    let (earlier, later, past) = if time <= now {
        (time, now, true)
    } else {
        (now, time, false)
    };
    let mut months =
        (later.year() - earlier.year()) * 12 + later.month() as i32 - earlier.month() as i32;
    let later_rest = (later.day(), later.hour(), later.minute(), later.second());
    let earlier_rest = (
        earlier.day(),
        earlier.hour(),
        earlier.minute(),
        earlier.second(),
    );
    if later_rest < earlier_rest {
        months -= 1;
    }
    let seconds = (later - earlier).num_seconds();
    let (count, unit) = if months >= 12 {
        (i64::from(months / 12), "year")
    } else if months >= 1 {
        (i64::from(months), "month")
    } else if seconds >= 7 * 86_400 {
        (seconds / (7 * 86_400), "week")
    } else if seconds >= 86_400 {
        (seconds / 86_400, "day")
    } else if seconds >= 3_600 {
        (seconds / 3_600, "hour")
    } else if seconds >= 60 {
        (seconds / 60, "minute")
    } else {
        (seconds.max(1), "second")
    };
    let unit = if count == 1 {
        unit.to_owned()
    } else {
        format!("{unit}s")
    };
    if past {
        format!("{count} {unit} ago")
    } else {
        format!("{count} {unit} from now")
    }
}

/// `ucfirst($value)`.
pub(crate) fn ucfirst(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// `number_format($value, 1)`: one decimal, rounded half away from zero.
pub(crate) fn number_format_1(value: f64) -> String {
    format!("{:.1}", (value * 10.0).round() / 10.0)
}

/// `version_compare($a, $b, '>')`.
pub(crate) fn version_greater(a: &str, b: &str) -> bool {
    crate::compose_parser::php::version_compare(a, b) > 0
}

/// `filled($value)` of an optional string.
pub(crate) fn filled(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, s)
            .unwrap()
    }

    #[test]
    fn enums_round_trip_their_values() {
        for status in SecurityStatusType::ALL {
            assert_eq!(SecurityStatusType::from_value(status.value()), Some(status));
        }
        for action in SecurityActionType::ALL {
            assert_eq!(SecurityActionType::from_value(action.value()), Some(action));
        }
        assert_eq!(SecurityStatusType::from_value("bad"), None);
        assert_eq!(SecurityActionType::UpdateImage.order(), 0);
        assert_eq!(SecurityActionType::RebuildInCi.order(), 6);
    }

    #[test]
    fn diff_for_humans_reads_like_carbon() {
        let now = at(2026, 11, 10, 12, 0, 0);
        assert_eq!(diff_for_humans(now, now), "1 second ago");
        assert_eq!(
            diff_for_humans(at(2026, 11, 10, 11, 59, 30), now),
            "30 seconds ago"
        );
        assert_eq!(
            diff_for_humans(at(2026, 11, 10, 10, 0, 0), now),
            "2 hours ago"
        );
        assert_eq!(diff_for_humans(at(2026, 11, 9, 12, 0, 0), now), "1 day ago");
        assert_eq!(
            diff_for_humans(at(2026, 10, 27, 12, 0, 0), now),
            "2 weeks ago"
        );
        assert_eq!(
            diff_for_humans(at(2026, 9, 10, 12, 0, 0), now),
            "2 months ago"
        );
        assert_eq!(
            diff_for_humans(at(2024, 11, 10, 12, 0, 0), now),
            "2 years ago"
        );
        assert_eq!(
            diff_for_humans(at(2026, 11, 10, 13, 0, 0), now),
            "1 hour from now"
        );
    }

    #[test]
    fn times_are_written_and_read_like_carbon() {
        let time = at(2026, 11, 10, 12, 0, 0);
        assert_eq!(iso8601(time), "2026-11-10T12:00:00+00:00");
        assert_eq!(parse_time("2026-11-10T12:00:00+00:00"), Some(time));
        assert_eq!(parse_time("2026-11-10T14:00:00+02:00"), Some(time));
        assert_eq!(parse_time("2026-11-10 12:00:00"), Some(time));
        assert_eq!(parse_time("nope"), None);
        assert_eq!(number_format_1(7.25), "7.3");
        assert_eq!(number_format_1(9.0), "9.0");
        assert_eq!(ucfirst("debian"), "Debian");
    }
}
