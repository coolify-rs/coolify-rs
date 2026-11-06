//! The four DORA metrics over a window of recorded deployments, and their performance bands.
//!
//! The bands are those of the 2021 Accelerate State of DevOps report (DORA, Google Cloud), table "Software
//! delivery performance metric", page 9. Its ranges leave gaps and overlap, so each band is decided by its
//! published limit on the side that matters, and a value that meets several limits gets the best of those
//! bands; the README lists the exact cut-offs.

use serde::Serialize;

use crate::record::Deployment;
use crate::time::{DAY, HOUR};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Band {
    Elite,
    High,
    Medium,
    Low,
}

impl Band {
    pub fn as_str(self) -> &'static str {
        match self {
            Band::Elite => "elite",
            Band::High => "high",
            Band::Medium => "medium",
            Band::Low => "low",
        }
    }
}

/// Successful deployments per day. Elite: on demand, multiple deploys per day (here: one or more a day
/// on average). High: at least once per month. Medium: at least once per six months. Low: fewer.
pub fn frequency_band(per_day: f64) -> Band {
    if per_day >= 1.0 {
        Band::Elite
    } else if per_day * 30.0 >= 1.0 {
        Band::High
    } else if per_day * 182.5 >= 1.0 {
        Band::Medium
    } else {
        Band::Low
    }
}

/// Lead time, in seconds. Elite: less than one hour. High: up to one week. Medium: up to six months.
pub fn lead_time_band(seconds: i64) -> Band {
    if seconds < HOUR {
        Band::Elite
    } else if seconds <= 7 * DAY {
        Band::High
    } else if seconds <= 183 * DAY {
        Band::Medium
    } else {
        Band::Low
    }
}

/// Time to restore, in seconds. Elite: less than one hour. High: less than one day. Medium: up to one
/// week. Low: longer.
pub fn restore_band(seconds: i64) -> Band {
    if seconds < HOUR {
        Band::Elite
    } else if seconds < DAY {
        Band::High
    } else if seconds <= 7 * DAY {
        Band::Medium
    } else {
        Band::Low
    }
}

/// Change failure rate, 0 to 1. Elite: 0–15%. High, medium and low share 16–30% in the report, so up to
/// 30% is high; above it is low.
pub fn failure_band(rate: f64) -> Band {
    if rate <= 0.15 {
        Band::Elite
    } else if rate <= 0.30 {
        Band::High
    } else {
        Band::Low
    }
}

/// Which deployments count, from the team's settings.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub include_previews: bool,
    /// Environment names, compared without case; empty counts every environment.
    pub environments: Vec<String>,
    /// One application only.
    pub app: Option<String>,
}

impl Filter {
    /// The `environments` setting: names separated by commas.
    pub fn environments(setting: Option<&str>) -> Vec<String> {
        setting
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    }

    pub fn counts(&self, d: &Deployment) -> bool {
        (self.include_previews || !d.is_preview())
            && (self.environments.is_empty()
                || self.environments.contains(&d.environment.to_lowercase()))
            && self.app.as_ref().is_none_or(|a| *a == d.app)
    }
}

/// The window: `days` days up to `end`.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub days: u32,
    pub end: i64,
}

impl Window {
    pub fn start(&self) -> i64 {
        self.end - i64::from(self.days) * DAY
    }
    pub fn contains(&self, t: i64) -> bool {
        t >= self.start() && t <= self.end
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Frequency {
    /// Successful deployments in the window.
    pub deployments: u64,
    pub per_day: f64,
    pub per_week: f64,
    /// `None` when nothing was deployed at all, successfully or not.
    pub band: Option<Band>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FailureRate {
    pub failed: u64,
    pub total: u64,
    /// failed / total; `None` without deployments.
    pub rate: Option<f64>,
    pub band: Option<Band>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Restore {
    /// Failures followed by a successful deployment of the same application.
    pub restored: u64,
    /// Failures with no successful deployment since.
    pub unrestored: u64,
    pub median_seconds: Option<i64>,
    pub p90_seconds: Option<i64>,
    pub band: Option<Band>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LeadTime {
    pub available: bool,
    /// Successful deployments whose commit time is known.
    pub samples: u64,
    pub median_seconds: Option<i64>,
    pub p90_seconds: Option<i64>,
    pub band: Option<Band>,
    /// Why there is no number, when there isn't.
    pub reason: Option<&'static str>,
}

pub const NO_COMMIT_TIMES: &str = "Coolify's deployment events carry the commit's SHA, message and branch, but not when it was committed, so the time from commit to deployment can't be measured.";
pub const NO_SUCCESSES: &str = "No successful deployments in this window.";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Metrics {
    pub deployments: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub deployment_frequency: Frequency,
    pub change_failure_rate: FailureRate,
    pub time_to_restore: Restore,
    pub lead_time: LeadTime,
}

/// The `p`th percentile (0 < p ≤ 1) by the nearest-rank method; `values` must be sorted.
pub fn percentile(values: &[i64], p: f64) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    let rank = (p * values.len() as f64).ceil() as usize;
    Some(values[rank.clamp(1, values.len()) - 1])
}

fn round(x: f64, places: i32) -> f64 {
    let f = 10f64.powi(places);
    (x * f).round() / f
}

/// Failure streaks: from the first failed deployment after a success (or the first deployment) to the
/// next successful deployment of the same application, preview deployments of a pull request counting
/// as their own application. Returns (start, restored at) per streak; `deps` must be oldest first.
pub fn incidents(deps: &[&Deployment]) -> Vec<(i64, Option<i64>)> {
    let mut open: Vec<((&str, i64), i64)> = Vec::new();
    let mut out = Vec::new();
    for d in deps {
        let key = (d.app.as_str(), d.pull_request);
        let at = open.iter().position(|(k, _)| *k == key);
        match (d.succeeded, at) {
            (false, None) => open.push((key, d.at)),
            (true, Some(i)) => {
                let (_, start) = open.remove(i);
                out.push((start, Some(d.at)));
            }
            _ => {}
        }
    }
    out.extend(open.into_iter().map(|(_, start)| (start, None)));
    out.sort();
    out
}

/// The metrics of the deployments `filter` counts in `window`. `deps` (oldest first) may reach before the
/// window: earlier failures tell whether a failure in the window continues a streak that began before it.
pub fn compute(deps: &[Deployment], window: Window, filter: &Filter) -> Metrics {
    let counted: Vec<&Deployment> = deps.iter().filter(|d| filter.counts(d)).collect();
    let in_window: Vec<&Deployment> = counted
        .iter()
        .copied()
        .filter(|d| window.contains(d.at))
        .collect();
    let succeeded = in_window.iter().filter(|d| d.succeeded).count() as u64;
    let failed = in_window.len() as u64 - succeeded;
    let total = in_window.len() as u64;

    let per_day = succeeded as f64 / f64::from(window.days.max(1));
    let deployment_frequency = Frequency {
        deployments: succeeded,
        per_day: round(per_day, 3),
        per_week: round(per_day * 7.0, 3),
        band: (total > 0).then(|| frequency_band(per_day)),
    };

    let rate = (total > 0).then(|| failed as f64 / total as f64);
    let change_failure_rate = FailureRate {
        failed,
        total,
        rate: rate.map(|r| round(r, 4)),
        band: rate.map(failure_band),
    };

    let streaks: Vec<(i64, Option<i64>)> = incidents(&counted)
        .into_iter()
        .filter(|(start, _)| window.contains(*start))
        .collect();
    let mut restore_times: Vec<i64> = streaks
        .iter()
        .filter_map(|(start, end)| end.map(|e| e - start))
        .collect();
    restore_times.sort_unstable();
    let median = percentile(&restore_times, 0.5);
    let time_to_restore = Restore {
        restored: restore_times.len() as u64,
        unrestored: streaks.iter().filter(|(_, end)| end.is_none()).count() as u64,
        median_seconds: median,
        p90_seconds: percentile(&restore_times, 0.9),
        band: median.map(restore_band),
    };

    let mut leads: Vec<i64> = in_window.iter().filter_map(|d| d.lead_time()).collect();
    leads.sort_unstable();
    let lead_median = percentile(&leads, 0.5);
    let lead_time = LeadTime {
        available: !leads.is_empty(),
        samples: leads.len() as u64,
        median_seconds: lead_median,
        p90_seconds: percentile(&leads, 0.9),
        band: lead_median.map(lead_time_band),
        reason: match (leads.is_empty(), succeeded) {
            (false, _) => None,
            (true, 0) => Some(NO_SUCCESSES),
            (true, _) => Some(NO_COMMIT_TIMES),
        },
    };

    Metrics {
        deployments: total,
        succeeded,
        failed,
        deployment_frequency,
        change_failure_rate,
        time_to_restore,
        lead_time,
    }
}

/// One application's line in the team's table.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AppSummary {
    pub uuid: String,
    pub name: String,
    pub environment: String,
    pub deployments: u64,
    pub failed: u64,
    pub change_failure_rate: Option<f64>,
    pub restore_median_seconds: Option<i64>,
    pub last_deployment_at: i64,
    #[serde(skip)]
    pub metrics: Metrics,
}

/// Every application with deployments in the window, most deployments first (then by name), with its
/// newest name and environment.
pub fn per_application(deps: &[Deployment], window: Window, filter: &Filter) -> Vec<AppSummary> {
    let mut apps: Vec<&str> = deps
        .iter()
        .filter(|d| window.contains(d.at) && filter.counts(d))
        .map(|d| d.app.as_str())
        .collect();
    apps.sort_unstable();
    apps.dedup();
    let mut out: Vec<AppSummary> = apps
        .into_iter()
        .map(|app| {
            let f = Filter {
                app: Some(app.to_owned()),
                ..filter.clone()
            };
            let m = compute(deps, window, &f);
            let newest = deps
                .iter()
                .rev()
                .find(|d| d.app == app && window.contains(d.at) && f.counts(d));
            AppSummary {
                uuid: app.to_owned(),
                name: newest.map(|d| d.app_name.clone()).unwrap_or_default(),
                environment: newest.map(|d| d.environment.clone()).unwrap_or_default(),
                deployments: m.deployments,
                failed: m.failed,
                change_failure_rate: m.change_failure_rate.rate,
                restore_median_seconds: m.time_to_restore.median_seconds,
                last_deployment_at: newest.map_or(0, |d| d.at),
                metrics: m,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.deployments
            .cmp(&a.deployments)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.uuid.cmp(&b.uuid))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{NOW, dep};
    use crate::time::MINUTE;

    fn w(days: u32) -> Window {
        Window { days, end: NOW }
    }

    fn all() -> Filter {
        Filter::default()
    }

    #[test]
    fn no_deployments() {
        let m = compute(&[], w(30), &all());
        assert_eq!(m.deployments, 0);
        assert_eq!(m.deployment_frequency.per_day, 0.0);
        assert_eq!(m.deployment_frequency.band, None);
        assert_eq!(m.change_failure_rate.rate, None);
        assert_eq!(m.change_failure_rate.band, None);
        assert_eq!(m.time_to_restore.median_seconds, None);
        assert_eq!(m.time_to_restore.band, None);
        assert!(!m.lead_time.available);
        assert_eq!(m.lead_time.reason, Some(NO_SUCCESSES));
        assert!(per_application(&[], w(30), &all()).is_empty());
    }

    #[test]
    fn deployment_frequency_and_its_band() {
        // 45 successful deployments and 15 failed ones in 30 days, and some outside the window.
        let mut deps = vec![
            dep(NOW - 31 * DAY, "a", true),
            dep(NOW - 40 * DAY, "a", true),
        ];
        for i in 0..60 {
            deps.push(dep(NOW - 29 * DAY + i * 11 * HOUR, "a", i % 4 != 0));
        }
        deps.sort_by_key(|d| d.at);
        let m = compute(&deps, w(30), &all());
        assert_eq!((m.succeeded, m.failed, m.deployments), (45, 15, 60));
        assert_eq!(m.deployment_frequency.deployments, 45);
        assert_eq!(m.deployment_frequency.per_day, 1.5);
        assert_eq!(m.deployment_frequency.per_week, 10.5);
        assert_eq!(m.deployment_frequency.band, Some(Band::Elite));
        // Over 90 days the same deployments are 47 in 90 days: about one every two days.
        let m = compute(&deps, w(90), &all());
        assert_eq!(m.deployment_frequency.per_day, 0.522);
        assert_eq!(m.deployment_frequency.band, Some(Band::High));

        assert_eq!(frequency_band(1.0), Band::Elite);
        assert_eq!(frequency_band(0.99), Band::High);
        assert_eq!(frequency_band(1.0 / 30.0), Band::High);
        assert_eq!(frequency_band(1.0 / 31.0), Band::Medium);
        assert_eq!(frequency_band(1.0 / 182.5), Band::Medium);
        assert_eq!(frequency_band(0.0), Band::Low);
    }

    #[test]
    fn change_failure_rate_and_previews() {
        let mut deps = Vec::new();
        for i in 0..10 {
            deps.push(dep(NOW - DAY * (10 - i), "a", i != 3));
        }
        // Preview deployments, all failed: excluded unless the team counts them.
        for i in 0..5 {
            let mut d = dep(NOW - HOUR * (5 - i), "a", false);
            d.pull_request = 7;
            deps.push(d);
        }
        let m = compute(&deps, w(30), &all());
        assert_eq!(m.change_failure_rate.rate, Some(0.1));
        assert_eq!(
            (m.change_failure_rate.failed, m.change_failure_rate.total),
            (1, 10)
        );
        assert_eq!(m.change_failure_rate.band, Some(Band::Elite));
        let with = Filter {
            include_previews: true,
            ..all()
        };
        let m = compute(&deps, w(30), &with);
        assert_eq!(m.change_failure_rate.rate, Some(0.4));
        assert_eq!(m.change_failure_rate.band, Some(Band::Low));
        // The preview's streak is its own: it never restores the application's.
        assert_eq!(m.time_to_restore.restored, 1);
        assert_eq!(m.time_to_restore.unrestored, 1);

        assert_eq!(failure_band(0.15), Band::Elite);
        assert_eq!(failure_band(0.155), Band::High);
        assert_eq!(failure_band(0.30), Band::High);
        assert_eq!(failure_band(0.31), Band::Low);
    }

    #[test]
    fn all_failures() {
        let deps: Vec<_> = (0..4)
            .map(|i| dep(NOW - DAY * (4 - i), "a", false))
            .collect();
        let m = compute(&deps, w(30), &all());
        assert_eq!(m.change_failure_rate.rate, Some(1.0));
        assert_eq!(m.change_failure_rate.band, Some(Band::Low));
        assert_eq!(m.deployment_frequency.deployments, 0);
        assert_eq!(m.deployment_frequency.band, Some(Band::Low));
        // One streak, never restored.
        assert_eq!(m.time_to_restore.restored, 0);
        assert_eq!(m.time_to_restore.unrestored, 1);
        assert_eq!(m.time_to_restore.median_seconds, None);
        assert_eq!(m.time_to_restore.band, None);
        assert_eq!(m.lead_time.reason, Some(NO_SUCCESSES));
    }

    #[test]
    fn time_to_restore_from_the_first_failure_to_the_next_success() {
        let t = NOW - 20 * DAY;
        let deps = vec![
            dep(t, "a", true),
            dep(t + HOUR, "a", false),     // a: streak from here...
            dep(t + 2 * HOUR, "b", false), // b: 30 minutes
            dep(t + 2 * HOUR + 30 * MINUTE, "b", true),
            dep(t + 3 * HOUR, "a", false), // ...not from here
            dep(t + 5 * HOUR, "a", true),  // a: 4 hours
            dep(t + 6 * HOUR, "c", false), // c: 3 days
            dep(t + 3 * DAY + 6 * HOUR, "c", true),
            dep(t + 4 * DAY, "a", false), // a: 10 days
            dep(t + 14 * DAY, "a", true),
            dep(t + 15 * DAY, "b", false), // b: never restored
        ];
        let m = compute(&deps, w(30), &all());
        let r = &m.time_to_restore;
        assert_eq!((r.restored, r.unrestored), (4, 1));
        // Sorted: 30m, 4h, 3d, 10d. Nearest rank: the median is the 2nd, p90 the 4th.
        assert_eq!(r.median_seconds, Some(4 * HOUR));
        assert_eq!(r.p90_seconds, Some(10 * DAY));
        assert_eq!(r.band, Some(Band::High));

        // A streak that began before the window doesn't count in it, even if restored inside.
        let deps = vec![
            dep(NOW - 31 * DAY, "a", false),
            dep(NOW - 29 * DAY, "a", false),
            dep(NOW - 28 * DAY, "a", true),
            dep(NOW - 2 * DAY, "a", false),
            dep(NOW - 2 * DAY + 20 * MINUTE, "a", true),
        ];
        let r = compute(&deps, w(30), &all()).time_to_restore;
        assert_eq!((r.restored, r.median_seconds), (1, Some(20 * MINUTE)));
        assert_eq!(r.band, Some(Band::Elite));

        assert_eq!(restore_band(HOUR - 1), Band::Elite);
        assert_eq!(restore_band(HOUR), Band::High);
        assert_eq!(restore_band(DAY), Band::Medium);
        assert_eq!(restore_band(7 * DAY), Band::Medium);
        assert_eq!(restore_band(7 * DAY + 1), Band::Low);
    }

    #[test]
    fn lead_time_only_with_commit_times() {
        let mut deps: Vec<_> = (0..3)
            .map(|i| dep(NOW - DAY * (3 - i), "a", true))
            .collect();
        let m = compute(&deps, w(30), &all());
        assert!(!m.lead_time.available);
        assert_eq!(m.lead_time.reason, Some(NO_COMMIT_TIMES));
        assert_eq!(m.lead_time.band, None);

        for (d, lead) in deps.iter_mut().zip([30 * MINUTE, 2 * DAY, 9 * DAY]) {
            d.commit_at = Some(d.at - lead);
        }
        let mut failed = dep(NOW - HOUR, "a", false);
        failed.commit_at = Some(NOW - 2 * HOUR);
        deps.push(failed);
        let m = compute(&deps, w(30), &all());
        assert!(m.lead_time.available);
        assert_eq!(m.lead_time.samples, 3);
        assert_eq!(m.lead_time.median_seconds, Some(2 * DAY));
        assert_eq!(m.lead_time.p90_seconds, Some(9 * DAY));
        assert_eq!(m.lead_time.band, Some(Band::High));
        assert_eq!(m.lead_time.reason, None);

        assert_eq!(lead_time_band(59 * MINUTE), Band::Elite);
        assert_eq!(lead_time_band(HOUR), Band::High);
        assert_eq!(lead_time_band(7 * DAY), Band::High);
        assert_eq!(lead_time_band(8 * DAY), Band::Medium);
        assert_eq!(lead_time_band(200 * DAY), Band::Low);
    }

    #[test]
    fn percentiles_by_nearest_rank() {
        assert_eq!(percentile(&[], 0.5), None);
        assert_eq!(percentile(&[5], 0.5), Some(5));
        assert_eq!(percentile(&[1, 2], 0.5), Some(1));
        assert_eq!(percentile(&[1, 2, 3], 0.5), Some(2));
        assert_eq!(percentile(&(1..=10).collect::<Vec<_>>(), 0.9), Some(9));
        assert_eq!(percentile(&(1..=11).collect::<Vec<_>>(), 0.9), Some(10));
    }

    #[test]
    fn environments_and_applications() {
        let mut staging = dep(NOW - DAY, "a", false);
        staging.environment = "Staging".into();
        let deps = vec![
            dep(NOW - 3 * DAY, "b", true),
            dep(NOW - 2 * DAY, "a", true),
            staging,
            dep(NOW - HOUR, "a", true),
        ];
        let prod = Filter {
            environments: Filter::environments(Some(" Production, ,qa")),
            ..all()
        };
        assert_eq!(prod.environments, ["production", "qa"]);
        assert_eq!(compute(&deps, w(30), &prod).failed, 0);
        assert_eq!(compute(&deps, w(30), &all()).failed, 1);

        let apps = per_application(&deps, w(30), &all());
        assert_eq!(
            apps.iter()
                .map(|a| (a.uuid.as_str(), a.deployments))
                .collect::<Vec<_>>(),
            [("a", 3), ("b", 1)]
        );
        assert_eq!(apps[0].name, "a-name");
        assert_eq!(apps[0].change_failure_rate, Some(0.3333));
        assert_eq!(apps[0].restore_median_seconds, Some(DAY - HOUR));
        assert_eq!(apps[0].last_deployment_at, NOW - HOUR);
        let only_a = Filter {
            app: Some("a".into()),
            ..all()
        };
        assert_eq!(compute(&deps, w(30), &only_a).deployments, 3);
    }
}
