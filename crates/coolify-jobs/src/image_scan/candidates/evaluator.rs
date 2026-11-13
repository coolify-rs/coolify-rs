//! `App\Services\ImageScan\CandidateEvaluator`: decides whether a candidate image is better than
//! the current image (spec 15.4).

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::image_scan::finding::Finding;
use crate::image_scan::rules::{self, RelevanceRules};

/// `CandidateEvaluator::COUNTED_SEVERITIES`.
const COUNTED_SEVERITIES: [&str; 3] = ["critical", "high", "medium"];

/// `counts()`: findings by severity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeverityCounts {
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
}

impl SeverityCounts {
    /// `['critical' => …, 'high' => …, 'medium' => …]`.
    pub fn to_value(self) -> Value {
        json!({"critical": self.critical, "high": self.high, "medium": self.medium})
    }

    /// `array_sum($counts)`.
    pub fn total(self) -> usize {
        self.critical + self.high + self.medium
    }
}

/// `isBetter()`. Better: adds no finding with an exploit signal, and fixes a finding with an
/// exploit signal, leaves an end-of-life distro, or has fewer relevant findings (an end-of-life
/// distro counts as one). Ignores apply to both images.
///
/// `current_eol` and `candidate_eol` say whether the image runs an end-of-life distro.
pub fn is_better(
    current_findings: &[Finding],
    candidate_findings: &[Finding],
    rules: &RelevanceRules,
    current_eol: bool,
    candidate_eol: bool,
) -> bool {
    let current: HashSet<String> = current_findings
        .iter()
        .filter(|finding| rules.ignores.matches(finding).is_none())
        .flat_map(keys)
        .collect();

    let candidate_relevant = relevant(candidate_findings, rules);
    let adds_exploit = candidate_relevant.iter().any(|finding| {
        rules::exploit_signal(finding).is_some()
            && !keys(finding).iter().any(|key| current.contains(key))
    });
    if adds_exploit {
        return false;
    }

    let current_relevant = relevant(current_findings, rules);
    let fixes_exploit = fixed(&current_relevant, candidate_findings)
        .iter()
        .any(|finding| rules::exploit_signal(finding).is_some());

    fixes_exploit
        || (current_eol && !candidate_eol)
        || candidate_relevant.len() + usize::from(candidate_eol)
            < current_relevant.len() + usize::from(current_eol)
}

/// `fixedIds()`: the IDs and aliases of the relevant current findings that the candidate no
/// longer has under any of their IDs. `candidate` holds all findings of the candidate.
pub fn fixed_ids(
    current: &[Finding],
    candidate: &[Finding],
    rules: &RelevanceRules,
) -> Vec<String> {
    let mut fixed_ids: Vec<String> = Vec::new();
    for finding in fixed(&relevant(current, rules), candidate) {
        for id in ids(&finding) {
            if !fixed_ids.iter().any(|known| known == id) {
                fixed_ids.push(id.to_owned());
            }
        }
    }
    fixed_ids
}

/// `relevant()`.
pub fn relevant(findings: &[Finding], rules: &RelevanceRules) -> Vec<Finding> {
    findings
        .iter()
        .filter(|finding| rules::is_relevant(finding, rules))
        .cloned()
        .collect()
}

/// `fixed()`: the current findings that the candidate no longer has.
pub fn fixed(current: &[Finding], candidate: &[Finding]) -> Vec<Finding> {
    let remaining = key_set(candidate);
    current
        .iter()
        .filter(|finding| !keys(finding).iter().any(|key| remaining.contains(key)))
        .cloned()
        .collect()
}

/// `shared()`: the current findings that the other list also has under one of their IDs.
pub fn shared(current: &[Finding], other: &[Finding]) -> Vec<Finding> {
    let others = key_set(other);
    current
        .iter()
        .filter(|finding| keys(finding).iter().any(|key| others.contains(key)))
        .cloned()
        .collect()
}

/// `added()`: the relevant critical, high, and medium findings of the candidate that the
/// relevant current findings do not have under any of their IDs, counted once per finding ID.
/// The same basis as [`counts`] of [`fixed`]. `candidate` holds all findings of the candidate.
pub fn added(current_relevant: &[Finding], candidate: &[Finding], rules: &RelevanceRules) -> usize {
    let known = key_set(current_relevant);
    relevant(candidate, rules)
        .iter()
        .filter(|finding| {
            COUNTED_SEVERITIES.contains(&finding.severity.to_ascii_lowercase().as_str())
                && !keys(finding).iter().any(|key| known.contains(key))
        })
        .map(|finding| finding.id.to_lowercase())
        .collect::<HashSet<String>>()
        .len()
}

/// `counts()`.
pub fn counts(findings: &[Finding]) -> SeverityCounts {
    let mut counts = SeverityCounts::default();
    for finding in findings {
        match finding.severity.to_ascii_lowercase().as_str() {
            "critical" => counts.critical += 1,
            "high" => counts.high += 1,
            "medium" => counts.medium += 1,
            _ => {}
        }
    }
    counts
}

/// The keys of every finding of a list.
fn key_set(findings: &[Finding]) -> HashSet<String> {
    findings.iter().flat_map(keys).collect()
}

/// `ids()`: the ID and the aliases that are not empty, each once.
fn ids(finding: &Finding) -> Vec<&str> {
    let mut ids: Vec<&str> = Vec::new();
    for id in finding.ids() {
        if !id.is_empty() && !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// `keys()`: IDs compare without case, like ignores and the fix classifier.
fn keys(finding: &Finding) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for id in ids(finding) {
        let key = id.to_lowercase();
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::json;

    use super::*;
    use crate::image_scan::grype;
    use crate::image_scan::rules::tests::{ignore, now};

    pub(crate) const GRYPE: &str = include_str!("../../../tests/fixtures/image-scan/grype.ndjson");
    pub(crate) const GRYPE_15_11: &str =
        include_str!("../../../tests/fixtures/image-scan/grype-candidate-15.11.ndjson");
    pub(crate) const GRYPE_15_ALPINE: &str =
        include_str!("../../../tests/fixtures/image-scan/grype-candidate-15-alpine.ndjson");

    /// `evaluatorFixture($name)`.
    pub(crate) fn fixture(ndjson: &str) -> Vec<Finding> {
        grype::parse_grype_report(ndjson).findings
    }

    /// `evaluatorFinding()`.
    fn finding_with(
        id: &str,
        severity: &str,
        kev: bool,
        aliases: &[&str],
        epss: Option<f64>,
        score: Option<f64>,
    ) -> Finding {
        Finding::from_value(&json!({
            "id": id, "aliases": aliases, "severity": severity, "package": "openssl", "version": "1.0",
            "type": "apk", "artifactId": "a", "fixedIn": "1.1", "fixState": "fixed", "kev": kev,
            "epss": epss, "cvssScore": score,
        }))
    }

    pub(crate) fn finding(id: &str, severity: &str) -> Finding {
        finding_with(id, severity, false, &[], None, None)
    }

    fn kev(id: &str, severity: &str) -> Finding {
        finding_with(id, severity, true, &[], None, None)
    }

    fn likely(id: &str, severity: &str, epss: f64, score: f64) -> Finding {
        finding_with(id, severity, false, &[], Some(epss), Some(score))
    }

    fn aliased(id: &str, severity: &str, aliases: &[&str]) -> Finding {
        finding_with(id, severity, false, aliases, None, None)
    }

    /// `evaluatorIgnores(...$ids)`.
    pub(crate) fn ignores(ids: &[&str]) -> RelevanceRules {
        let rows = ids
            .iter()
            .map(|id| {
                let mut row = ignore(Some(*id), None);
                row.created_at = None;
                row
            })
            .collect();
        RelevanceRules::from_ignores(rows, false, now())
    }

    #[test]
    fn does_not_recommend_15_11_alpine_it_adds_findings_with_an_exploit_signal() {
        assert!(!is_better(
            &fixture(GRYPE),
            &fixture(GRYPE_15_11),
            &ignores(&[]),
            true,
            false
        ));
    }

    #[test]
    fn recommends_15_alpine_although_it_adds_critical_go_findings_in_gosu() {
        let rules = ignores(&[]);
        let current = fixture(GRYPE);
        let candidate = fixture(GRYPE_15_ALPINE);
        let fixed = fixed_ids(&current, &candidate, &rules);
        assert!(is_better(&current, &candidate, &rules, true, false));
        assert!(is_better(&current, &candidate, &rules, false, false));
        assert!(fixed.iter().any(|id| id == "CVE-2025-1094"));
        // A medium with an EPSS of 0.10 or more is relevant.
        assert!(fixed.iter().any(|id| id == "CVE-2023-2650"));
    }

    #[test]
    fn lists_only_the_relevant_findings_the_candidate_no_longer_has_under_any_of_their_ids() {
        let current = [
            aliased("GHSA-1111", "high", &["CVE-2024-1"]),
            aliased("GHSA-2222", "high", &["CVE-2024-2"]),
            finding("CVE-2024-3", "medium"),
            finding("CVE-2024-4", "critical"),
        ];
        let candidate = [finding("CVE-2024-1", "high")];
        assert_eq!(
            fixed_ids(&current, &candidate, &ignores(&["CVE-2024-4"])),
            vec!["GHSA-2222", "CVE-2024-2"]
        );
    }

    #[test]
    fn counts_a_candidate_with_fewer_relevant_findings_as_better_also_when_it_adds_a_critical() {
        let current = [
            finding("CVE-1", "high"),
            finding("CVE-2", "high"),
            finding("CVE-3", "medium"),
        ];
        let none = ignores(&[]);
        assert!(is_better(
            &current,
            &[finding("CVE-1", "high")],
            &none,
            false,
            false
        ));
        assert!(is_better(
            &current,
            &[finding("CVE-9", "critical")],
            &none,
            false,
            false
        ));
        assert!(is_better(
            &current,
            &[
                finding("CVE-1", "high"),
                finding("CVE-9", "medium"),
                finding("CVE-8", "low")
            ],
            &none,
            false,
            false
        ));
    }

    #[test]
    fn does_not_count_a_candidate_with_as_many_relevant_findings_and_no_fixed_exploit_signal() {
        let current = [finding("CVE-1", "high"), finding("CVE-2", "medium")];
        let none = ignores(&[]);
        assert!(!is_better(
            &current,
            &[finding("CVE-1", "high")],
            &none,
            false,
            false
        ));
        assert!(!is_better(
            &current,
            &[finding("CVE-3", "high")],
            &none,
            false,
            false
        ));
        assert!(!is_better(
            &current,
            &[finding("CVE-3", "critical"), finding("CVE-2", "medium")],
            &none,
            false,
            false
        ));
        assert!(!is_better(
            &current,
            &[],
            &ignores(&["CVE-1"]),
            false,
            false
        ));
    }

    #[test]
    fn counts_a_candidate_that_fixes_a_finding_with_an_exploit_signal_as_better() {
        let current = [likely("CVE-1", "high", 0.5, 8.1)];
        let candidate = [finding("CVE-2", "critical"), finding("CVE-3", "high")];
        assert!(is_better(&current, &candidate, &ignores(&[]), false, false));
        assert!(!is_better(
            &current,
            &candidate,
            &ignores(&["CVE-1"]),
            false,
            false
        ));
    }

    #[test]
    fn drives_the_candidate_search_with_a_fixable_high_known_exploited_finding() {
        let current = [kev("CVE-1", "high")];
        assert!(is_better(&current, &[], &ignores(&[]), false, false));
        assert_eq!(fixed_ids(&current, &[], &ignores(&[])), vec!["CVE-1"]);
    }

    #[test]
    fn never_counts_a_candidate_that_adds_a_finding_with_an_exploit_signal_as_better() {
        let current = [
            finding("CVE-1", "critical"),
            finding_with("GHSA-9", "low", true, &["CVE-9"], None, None),
            kev("CVE-5", "high"),
        ];
        assert!(!is_better(
            &current,
            &[kev("CVE-3", "low")],
            &ignores(&[]),
            true,
            false
        ));
        assert!(!is_better(
            &current,
            &[likely("CVE-4", "high", 0.3, 7.5)],
            &ignores(&[]),
            false,
            false
        ));
        assert!(is_better(
            &current,
            &[likely("CVE-4", "high", 0.3, 7.5)],
            &ignores(&["CVE-4"]),
            false,
            false
        ));
        assert!(is_better(
            &current,
            &[kev("cve-9", "low")],
            &ignores(&[]),
            false,
            false
        ));
    }

    #[test]
    fn does_not_treat_a_finding_of_the_current_image_whose_epss_went_up_as_added() {
        let current = [
            finding("CVE-1", "critical"),
            likely("CVE-2", "high", 0.05, 7.5),
        ];
        assert!(is_better(
            &current,
            &[likely("CVE-2", "high", 0.4, 7.5)],
            &ignores(&[]),
            false,
            false
        ));
    }

    #[test]
    fn counts_leaving_an_end_of_life_distro_as_better_and_an_eol_distro_as_one_problem() {
        let none = ignores(&[]);
        assert!(is_better(&[], &[], &none, true, false));
        assert!(is_better(
            &[finding("CVE-1", "high")],
            &[finding("CVE-1", "high"), finding("CVE-2", "critical")],
            &none,
            true,
            false
        ));
        assert!(!is_better(&[], &[], &none, true, true));
        assert!(!is_better(
            &[finding("CVE-1", "critical")],
            &[],
            &none,
            false,
            true
        ));
        assert!(is_better(
            &[finding("CVE-1", "critical"), finding("CVE-2", "high")],
            &[],
            &none,
            false,
            true
        ));
        assert!(is_better(
            &[finding("CVE-1", "critical")],
            &[],
            &none,
            true,
            true
        ));
        assert!(!is_better(&[], &[], &none, false, false));
    }

    #[test]
    fn counts_relevant_findings_by_severity_mediums_only_with_an_exploit_signal() {
        let findings = [
            finding("CVE-1", "critical"),
            finding("CVE-2", "high"),
            finding("CVE-3", "medium"),
            kev("CVE-4", "medium"),
            kev("CVE-5", "low"),
            finding("CVE-6", "high"),
        ];
        let counted = counts(&relevant(&findings, &ignores(&["CVE-6"])));
        assert_eq!(
            counted,
            SeverityCounts {
                critical: 1,
                high: 1,
                medium: 1
            }
        );
        assert_eq!(
            counted.to_value().to_string(),
            r#"{"critical":1,"high":1,"medium":1}"#
        );
        assert_eq!(counted.total(), 3);
    }

    #[test]
    fn keeps_the_current_findings_that_another_image_also_has_under_one_of_their_ids() {
        let in_base = aliased("GHSA-aaaa", "high", &["CVE-2026-0001"]);
        let added = finding("CVE-2099-0001", "high");
        assert_eq!(
            shared(
                &[in_base.clone(), added.clone()],
                &[finding("cve-2026-0001", "high")]
            ),
            vec![in_base.clone()]
        );
        assert_eq!(shared(&[in_base, added], &[]), Vec::<Finding>::new());
    }

    #[test]
    fn added_counts_new_relevant_findings_once_per_id() {
        let none = ignores(&[]);
        let current = [finding("CVE-1", "high")];
        let candidate = [
            finding("CVE-1", "high"),
            // The same finding in two packages counts once.
            finding("CVE-2", "critical"),
            finding("cve-2", "critical"),
            // Not relevant: a medium without a signal, and a low.
            finding("CVE-3", "medium"),
            finding("CVE-4", "low"),
            // Relevant, but a low is not counted.
            kev("CVE-5", "low"),
            kev("CVE-6", "medium"),
        ];
        assert_eq!(added(&current, &candidate, &none), 2);
        assert_eq!(added(&current, &candidate, &none.with_all_mediums()), 3);
        assert_eq!(added(&current, &candidate, &ignores(&["CVE-2"])), 1);
    }
}
