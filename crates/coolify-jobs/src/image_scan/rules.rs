//! `SecurityRules`, `RelevanceRules` and `IgnoreMatcher`: which findings matter (spec 15.2).

use std::collections::HashMap;

use chrono::{Duration, NaiveDateTime};
use coolify_db::Db;
use coolify_db::image_scans::{self, ImageScanIgnoreRow, ScanOwner};

use super::finding::Finding;
use super::policy;

/// `SecurityRules::EXPLOITED` and `SecurityRules::LIKELY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExploitSignal {
    /// In CISA KEV.
    Exploited,
    /// EPSS 0.10 or more with CVSS 7.0 or more (severity high or critical without a score).
    Likely,
}

impl ExploitSignal {
    /// The stored value: `exploited` or `likely`.
    pub fn value(self) -> &'static str {
        match self {
            ExploitSignal::Exploited => "exploited",
            ExploitSignal::Likely => "likely",
        }
    }
}

const LIKELY_EPSS: f64 = 0.10;
const LIKELY_SCORE: f64 = 7.0;
const SERIOUS_SEVERITIES: [&str; 2] = ["critical", "high"];
const OS_PACKAGE_TYPES: [&str; 3] = ["apk", "deb", "rpm"];

/// `SecurityRules::exploitSignal($finding)`.
pub fn exploit_signal(finding: &Finding) -> Option<ExploitSignal> {
    if finding.kev {
        return Some(ExploitSignal::Exploited);
    }
    let severe = match finding.cvss_score {
        Some(score) => score >= LIKELY_SCORE,
        None => is_serious(finding),
    };
    (finding.epss.is_some_and(|epss| epss >= LIKELY_EPSS) && severe)
        .then_some(ExploitSignal::Likely)
}

/// `SecurityRules::isRelevant($finding, $rules)`: not ignored, not a CPE-only OS match, and an
/// exploit signal, severity critical or high, or a counted medium.
pub fn is_relevant(finding: &Finding, rules: &RelevanceRules) -> bool {
    !is_cpe_only_os_match(finding)
        && (exploit_signal(finding).is_some()
            || is_serious(finding)
            || is_counted_medium(finding, rules.count_medium))
        && rules.ignores.matches(finding).is_none()
}

/// `SecurityRules::isCpeOnlyOsMatch($finding)`: an OS package (apk, deb, rpm) that Grype matched
/// only by CPE.
pub fn is_cpe_only_os_match(finding: &Finding) -> bool {
    finding.namespace.to_lowercase().starts_with("nvd:cpe")
        && OS_PACKAGE_TYPES.contains(&finding.r#type.to_lowercase().as_str())
}

/// `SecurityRules::relevantIds($findings, $rules)`: the IDs and aliases of the relevant
/// findings, lowercase, each once, in the order of the findings.
pub fn relevant_ids(findings: &[Finding], rules: &RelevanceRules) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for finding in findings {
        if !is_relevant(finding, rules) {
            continue;
        }
        for id in finding.ids() {
            let id = id.to_lowercase();
            if !id.is_empty() && !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

/// A medium finding with EPSS 0.10 or more, or any medium finding when the rules count medium
/// problems. Not an exploit signal.
fn is_counted_medium(finding: &Finding, count_medium: bool) -> bool {
    finding.severity.to_lowercase() == "medium"
        && (count_medium || finding.epss.is_some_and(|epss| epss >= LIKELY_EPSS))
}

fn is_serious(finding: &Finding) -> bool {
    SERIOUS_SEVERITIES.contains(&finding.severity.to_lowercase().as_str())
}

/// `IgnoreMatcher`: matches findings against the active ignores of one resource and its team.
#[derive(Debug, Clone, Default)]
pub struct IgnoreMatcher {
    by_vulnerability: HashMap<String, Vec<ImageScanIgnoreRow>>,
    by_package: HashMap<String, Vec<ImageScanIgnoreRow>>,
    now: NaiveDateTime,
}

impl IgnoreMatcher {
    /// An ignore of a problem with an exploit signal applies for at most this many days after it
    /// was made (spec 15.2).
    pub const EXPLOIT_SIGNAL_IGNORE_DAYS: i64 = 90;

    /// `IgnoreMatcher::fromIgnores($ignores)`. Ignores that ended before `now` are left out.
    pub fn from_ignores(ignores: Vec<ImageScanIgnoreRow>, now: NaiveDateTime) -> Self {
        let mut matcher = Self {
            now,
            ..Self::default()
        };
        for ignore in ignores {
            if ignore.expires_at.is_some_and(|at| at < now) {
                continue;
            }
            let vulnerability_id = normalize(ignore.vulnerability_id.as_deref());
            let package = normalize_package(ignore.package.as_deref()).map(str::to_owned);
            if let Some(id) = vulnerability_id {
                matcher.by_vulnerability.entry(id).or_default().push(ignore);
            } else if let Some(package) = package {
                matcher.by_package.entry(package).or_default().push(ignore);
            }
        }
        matcher
    }

    /// `IgnoreMatcher::forResource($resource)`: the active ignores of the team of the resource
    /// that are team-wide or its own.
    pub async fn for_resource(
        db: &Db,
        resource: &ScanOwner,
        now: NaiveDateTime,
    ) -> Result<Self, String> {
        let Some(team_id) = resource.team_id else {
            return Ok(Self::from_ignores(Vec::new(), now));
        };
        let ignores =
            image_scans::active_ignores_for(db, team_id, resource.morph, resource.id, now)
                .await
                .map_err(|e| e.to_string())?;
        Ok(Self::from_ignores(ignores, now))
    }

    /// `matches($finding)`: the ignore that hides the finding, if any.
    pub fn matches(&self, finding: &Finding) -> Option<&ImageScanIgnoreRow> {
        let exploit_signal = exploit_signal(finding).is_some() && !is_cpe_only_os_match(finding);
        for candidate in finding.ids() {
            let Some(key) = normalize(Some(candidate)) else {
                continue;
            };
            for ignore in self.by_vulnerability.get(&key).into_iter().flatten() {
                let package = normalize_package(ignore.package.as_deref());
                if package.is_none_or(|p| p == finding.package)
                    && self.applies(ignore, exploit_signal)
                {
                    return Some(ignore);
                }
            }
        }
        self.by_package
            .get(&finding.package)
            .into_iter()
            .flatten()
            .find(|ignore| self.applies(ignore, exploit_signal))
    }

    /// An ignore of a problem with an exploit signal ends 90 days after it was made, also when it
    /// was set to never end or the problem got the signal later (a team-wide or package ignore, a
    /// new KEV entry).
    fn applies(&self, ignore: &ImageScanIgnoreRow, exploit_signal: bool) -> bool {
        match ignore.created_at {
            Some(created_at) if exploit_signal => {
                created_at >= self.now - Duration::days(Self::EXPLOIT_SIGNAL_IGNORE_DAYS)
            }
            _ => true,
        }
    }
}

fn normalize(value: Option<&str>) -> Option<String> {
    let value = coolify_remote::text::php_trim(value.unwrap_or_default());
    (!value.is_empty()).then(|| value.to_lowercase())
}

fn normalize_package(value: Option<&str>) -> Option<&str> {
    value.filter(|v| !v.is_empty())
}

/// `RelevanceRules`: what decides relevance for one resource (spec 15.2): its active ignores and
/// the "Count medium problems" rule.
#[derive(Debug, Clone, Default)]
pub struct RelevanceRules {
    pub ignores: IgnoreMatcher,
    pub count_medium: bool,
}

impl RelevanceRules {
    pub fn new(ignores: IgnoreMatcher, count_medium: bool) -> Self {
        Self {
            ignores,
            count_medium,
        }
    }

    /// `RelevanceRules::forResource($resource)`.
    pub async fn for_resource(
        db: &Db,
        resource: &ScanOwner,
        now: NaiveDateTime,
    ) -> Result<Self, String> {
        Ok(Self {
            ignores: IgnoreMatcher::for_resource(db, resource, now).await?,
            count_medium: policy::count_medium_for(db, resource).await?,
        })
    }

    /// `RelevanceRules::fromIgnores($ignores, $countMedium)`.
    pub fn from_ignores(
        ignores: Vec<ImageScanIgnoreRow>,
        count_medium: bool,
        now: NaiveDateTime,
    ) -> Self {
        Self {
            ignores: IgnoreMatcher::from_ignores(ignores, now),
            count_medium,
        }
    }

    /// `withAllMediums()`: the same ignores with every medium counted. Version and base checks
    /// store the IDs they fix with these, so a change of the rule needs no new check.
    pub fn with_all_mediums(&self) -> Self {
        Self {
            ignores: self.ignores.clone(),
            count_medium: true,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use chrono::NaiveDate;

    use super::*;

    pub(crate) fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 10)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
    }

    pub(crate) fn finding(id: &str, severity: &str) -> Finding {
        Finding {
            id: id.to_owned(),
            aliases: Vec::new(),
            severity: severity.to_owned(),
            package: "openssl".to_owned(),
            version: "3.0.1".to_owned(),
            r#type: "deb".to_owned(),
            artifact_id: "a1".to_owned(),
            fixed_in: None,
            fix_state: "not-fixed".to_owned(),
            kev: false,
            epss: None,
            risk: None,
            summary: String::new(),
            layer: "app".to_owned(),
            cvss_score: None,
            title: String::new(),
            display_id: id.to_owned(),
            namespace: "debian:distro:debian:12".to_owned(),
            match_type: "exact-direct-match".to_owned(),
        }
    }

    pub(crate) fn ignore(
        vulnerability_id: Option<&str>,
        package: Option<&str>,
    ) -> ImageScanIgnoreRow {
        ImageScanIgnoreRow {
            id: 1,
            uuid: "ign".to_owned(),
            team_id: 1,
            resource_type: None,
            resource_id: None,
            vulnerability_id: vulnerability_id.map(str::to_owned),
            package: package.map(str::to_owned),
            reason: "accepted".to_owned(),
            expires_at: None,
            created_by: None,
            created_at: Some(now() - Duration::days(1)),
            updated_at: None,
        }
    }

    fn rules(ignores: Vec<ImageScanIgnoreRow>, count_medium: bool) -> RelevanceRules {
        RelevanceRules::from_ignores(ignores, count_medium, now())
    }

    #[test]
    fn kev_is_exploited() {
        let mut f = finding("CVE-2024-0001", "low");
        f.kev = true;
        assert_eq!(exploit_signal(&f), Some(ExploitSignal::Exploited));
        assert_eq!(ExploitSignal::Exploited.value(), "exploited");
    }

    #[test]
    fn likely_needs_epss_and_a_high_score() {
        let mut f = finding("CVE-2024-0001", "medium");
        f.epss = Some(0.10);
        f.cvss_score = Some(7.0);
        assert_eq!(exploit_signal(&f), Some(ExploitSignal::Likely));
        f.cvss_score = Some(6.9);
        assert_eq!(exploit_signal(&f), None);
        // Without a score the severity decides.
        f.cvss_score = None;
        assert_eq!(exploit_signal(&f), None);
        f.severity = "High".to_owned();
        assert_eq!(exploit_signal(&f), Some(ExploitSignal::Likely));
        f.epss = Some(0.09);
        assert_eq!(exploit_signal(&f), None);
        f.epss = None;
        assert_eq!(exploit_signal(&f), None);
        // A high score with a low severity is still likely.
        let mut low = finding("CVE-2024-0002", "low");
        low.epss = Some(0.5);
        low.cvss_score = Some(9.8);
        assert_eq!(exploit_signal(&low), Some(ExploitSignal::Likely));
    }

    #[test]
    fn relevance_by_severity_and_the_medium_rule() {
        let none = rules(Vec::new(), false);
        assert!(is_relevant(&finding("CVE-1", "critical"), &none));
        assert!(is_relevant(&finding("CVE-1", "high"), &none));
        assert!(!is_relevant(&finding("CVE-1", "medium"), &none));
        assert!(!is_relevant(&finding("CVE-1", "low"), &none));
        assert!(!is_relevant(&finding("CVE-1", "unknown"), &none));

        let mut medium = finding("CVE-1", "medium");
        medium.epss = Some(0.2);
        assert!(is_relevant(&medium, &none));
        assert_eq!(exploit_signal(&medium), None);

        let counted = rules(Vec::new(), true);
        assert!(is_relevant(&finding("CVE-1", "medium"), &counted));
        assert!(!is_relevant(&finding("CVE-1", "low"), &counted));
        assert!(none.with_all_mediums().count_medium);
    }

    #[test]
    fn cpe_only_os_matches_are_never_relevant() {
        let mut f = finding("CVE-1", "critical");
        f.namespace = "nvd:cpe".to_owned();
        f.kev = true;
        assert!(is_cpe_only_os_match(&f));
        assert!(!is_relevant(&f, &rules(Vec::new(), true)));
        f.r#type = "npm".to_owned();
        assert!(!is_cpe_only_os_match(&f));
        assert!(is_relevant(&f, &rules(Vec::new(), false)));
    }

    #[test]
    fn relevant_ids_are_lowercase_and_unique() {
        let mut a = finding("GHSA-AAAA", "high");
        a.aliases = vec!["CVE-2024-1".to_owned(), String::new()];
        let mut b = finding("CVE-2024-1", "critical");
        b.aliases = vec!["GHSA-bbbb".to_owned()];
        let c = finding("CVE-2024-2", "low");
        assert_eq!(
            relevant_ids(&[a, b, c], &rules(Vec::new(), false)),
            vec!["ghsa-aaaa", "cve-2024-1", "ghsa-bbbb"]
        );
    }

    #[test]
    fn ignores_match_by_id_alias_and_package() {
        let matcher = IgnoreMatcher::from_ignores(vec![ignore(Some(" cve-2024-1 "), None)], now());
        assert!(matcher.matches(&finding("CVE-2024-1", "high")).is_some());
        assert!(matcher.matches(&finding("CVE-2024-2", "high")).is_none());
        let mut by_alias = finding("GHSA-x", "high");
        by_alias.aliases = vec!["CVE-2024-1".to_owned()];
        assert!(matcher.matches(&by_alias).is_some());

        // An ignore of a vulnerability in one package.
        let scoped =
            IgnoreMatcher::from_ignores(vec![ignore(Some("CVE-2024-1"), Some("zlib"))], now());
        assert!(scoped.matches(&finding("CVE-2024-1", "high")).is_none());
        let mut zlib = finding("CVE-2024-1", "high");
        zlib.package = "zlib".to_owned();
        assert!(scoped.matches(&zlib).is_some());
        // It is not a package ignore.
        let mut other = finding("CVE-2024-9", "high");
        other.package = "zlib".to_owned();
        assert!(scoped.matches(&other).is_none());

        let package = IgnoreMatcher::from_ignores(vec![ignore(None, Some("openssl"))], now());
        assert!(package.matches(&finding("CVE-2024-7", "high")).is_some());
        assert!(package.matches(&zlib).is_none());
        // Package names are compared as they are.
        let upper = IgnoreMatcher::from_ignores(vec![ignore(None, Some("OpenSSL"))], now());
        assert!(upper.matches(&finding("CVE-2024-7", "high")).is_none());
    }

    #[test]
    fn ended_ignores_do_not_match() {
        let mut ended = ignore(Some("CVE-2024-1"), None);
        ended.expires_at = Some(now() - Duration::minutes(1));
        let mut running = ignore(Some("CVE-2024-2"), None);
        running.expires_at = Some(now() + Duration::days(1));
        let matcher = IgnoreMatcher::from_ignores(vec![ended, running], now());
        assert!(matcher.matches(&finding("CVE-2024-1", "high")).is_none());
        assert!(matcher.matches(&finding("CVE-2024-2", "high")).is_some());
    }

    #[test]
    fn an_ignore_of_an_exploited_problem_ends_after_90_days() {
        let mut old = ignore(Some("CVE-2024-1"), None);
        old.created_at = Some(now() - Duration::days(91));
        let matcher = IgnoreMatcher::from_ignores(vec![old.clone()], now());
        // Without an exploit signal the old ignore still applies.
        assert!(matcher.matches(&finding("CVE-2024-1", "high")).is_some());
        let mut kev = finding("CVE-2024-1", "high");
        kev.kev = true;
        assert!(matcher.matches(&kev).is_none());
        assert!(is_relevant(&kev, &RelevanceRules::new(matcher, false)));

        let mut recent = old;
        recent.created_at = Some(now() - Duration::days(90));
        let matcher = IgnoreMatcher::from_ignores(vec![recent], now());
        assert!(matcher.matches(&kev).is_some());

        // A CPE-only OS match has no exploit signal for this rule.
        let mut cpe = kev.clone();
        cpe.namespace = "nvd:cpe".to_owned();
        let mut old = ignore(Some("CVE-2024-1"), None);
        old.created_at = Some(now() - Duration::days(400));
        assert!(
            IgnoreMatcher::from_ignores(vec![old], now())
                .matches(&cpe)
                .is_some()
        );
    }
}
