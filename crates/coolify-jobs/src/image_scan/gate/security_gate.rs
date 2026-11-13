//! `App\Services\ImageScan\SecurityGate`: "Block deploys" (spec 15.8): decides from the stored
//! findings of the new image whether a deploy starts. No remote calls.
//!
//! - On: blocks when the new image has a relevant finding with an exploit signal that the running
//!   image of the app does not have (by ID and aliases). Such findings that the running image has
//!   too are listed as a warning, so an EPSS change of a finding that already runs never blocks.
//!   Before the first deploy every finding is new. When the app ran before but Coolify has no
//!   earlier scan to compare with (`baseline_missing`), nothing blocks: the findings are listed
//!   as a warning and this deploy becomes the baseline.
//! - Strict: blocks on any relevant finding with an exploit signal, any relevant finding with a
//!   package fix (a medium one only when it is relevant, see `rules::is_relevant`), and an
//!   end-of-life OS.
//! - Off never blocks. A scan that cannot run, or whose report was cut (`summary.truncated`),
//!   follows "If the scan cannot run".

use std::cmp::Ordering;
use std::collections::HashSet;

use coolify_db::image_scans::ImageScanRow;
use serde_json::{Map, Value, json};

use crate::image_scan::error::ErrorCode;
use crate::image_scan::finding::Finding;
use crate::image_scan::policy::{GateMode, OnScanError};
use crate::image_scan::resource_scans;
use crate::image_scan::rules::{self, ExploitSignal, RelevanceRules};
use crate::image_scan::text;

/// `SecurityGate::LOG_LIMIT`: the problems a deploy log and a stored result list.
pub const LOG_LIMIT: usize = 5;

/// One problem of a gate result (`SecurityGate::row()`).
#[derive(Debug, Clone, PartialEq)]
pub struct GateRow {
    pub id: String,
    pub display_id: String,
    /// Lowercase.
    pub severity: String,
    pub score: Option<f64>,
    pub package: String,
    pub version: String,
    pub fixed_in: Option<String>,
    pub kev: bool,
    pub signal: Option<ExploitSignal>,
}

impl GateRow {
    /// The row as it is stored in `scan_gate_result`, in upstream's key order.
    pub fn to_value(&self) -> Value {
        json!({
            "id": self.id,
            "display_id": self.display_id,
            "severity": self.severity,
            "score": self.score.and_then(serde_json::Number::from_f64).map_or(Value::Null, Value::Number),
            "package": self.package,
            "version": self.version,
            "fixed_in": self.fixed_in,
            "kev": self.kev,
            "signal": self.signal.map(ExploitSignal::value),
        })
    }
}

/// The end-of-life OS of a result (`eol`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateEol {
    pub name: String,
    pub version: String,
}

/// The result of [`evaluate`]. `headline` counts the problems (email, deploy log);
/// `headline_plain` does not (chat and webhook channels).
#[derive(Debug, Clone, PartialEq)]
pub struct GateResult {
    pub blocked: bool,
    pub headline: String,
    pub headline_plain: String,
    /// The blocking problems, exploited first.
    pub new: Vec<GateRow>,
    /// The problems with an exploit signal that the running image has too.
    pub carried: Vec<GateRow>,
    pub error: Option<ErrorCode>,
    pub scan_ran: bool,
    pub mode: GateMode,
    pub eol: Option<GateEol>,
    pub baseline_missing: bool,
}

impl GateResult {
    /// `SecurityGate::result()` with its defaults.
    fn of(mode: GateMode) -> Self {
        Self {
            blocked: false,
            headline: String::new(),
            headline_plain: String::new(),
            new: Vec::new(),
            carried: Vec::new(),
            error: None,
            scan_ran: false,
            mode,
            eol: None,
            baseline_missing: false,
        }
    }
}

/// `SecurityGate::evaluate()`. `new` is the scan of the new image, `running` the scan of the
/// image the application ran before the deployment, `ctx` the context of the error message
/// (server). Upstream takes the mode as a string and reads an unknown one as off; the resolved
/// policy holds a [`GateMode`] here.
#[allow(clippy::too_many_arguments)]
pub fn evaluate(
    new: Option<&ImageScanRow>,
    running: Option<&ImageScanRow>,
    mode: GateMode,
    rules: &RelevanceRules,
    error: Option<ErrorCode>,
    ctx: &Map<String, Value>,
    on_scan_error: OnScanError,
    baseline_missing: bool,
) -> GateResult {
    let completed = new.filter(|scan| scan.is_completed());

    if mode == GateMode::Off {
        return GateResult {
            scan_ran: completed.is_some() && error.is_none(),
            ..GateResult::of(mode)
        };
    }

    let error = error.or(match completed {
        None => Some(ErrorCode::Unknown),
        // A cut report can miss the findings that block, so it counts as a scan that could not
        // run.
        Some(scan) if php_bool(summary_value(scan, "truncated")) => Some(ErrorCode::Truncated),
        Some(_) => None,
    });

    let new = match (error, completed) {
        (None, Some(scan)) => scan,
        (error, _) => {
            let error = error.unwrap_or(ErrorCode::Unknown);
            let blocked = on_scan_error == OnScanError::Block;
            let headline = format!(
                "{}{}",
                if blocked {
                    "Deploy blocked: "
                } else {
                    "Scan skipped: "
                },
                error.message(ctx)
            );
            return GateResult {
                blocked,
                headline_plain: headline.clone(),
                headline,
                error: Some(error),
                ..GateResult::of(mode)
            };
        }
    };

    let running_ids = running
        .map(|scan| id_set(&resource_scans::findings(scan)))
        .unwrap_or_default();
    let mut blocking = Vec::new();
    let mut carried = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for finding in resource_scans::findings(new) {
        let key = finding.id.to_lowercase();
        if seen.contains(&key) || !rules::is_relevant(&finding, rules) {
            continue;
        }
        seen.insert(key);

        let signal = rules::exploit_signal(&finding);
        if mode == GateMode::Strict {
            if signal.is_some() || has_package_fix(&finding) {
                blocking.push(row(&finding, signal));
            }
            continue;
        }

        if signal.is_none() {
            continue;
        }
        if baseline_missing || in_set(&finding, &running_ids) {
            carried.push(row(&finding, signal));
        } else {
            blocking.push(row(&finding, signal));
        }
    }

    let eol = match summary_value(new, "eol_distro") {
        Some(distro) if mode == GateMode::Strict && (distro.is_object() || distro.is_array()) => {
            Some(GateEol {
                name: text::clean(distro.get("name"), 64),
                version: text::clean(distro.get("version"), 32),
            })
        }
        _ => None,
    };

    let blocking = sorted(blocking);
    let blocked = !blocking.is_empty() || eol.is_some();

    GateResult {
        blocked,
        headline: if blocked {
            headline(&blocking, eol.as_ref(), true)
        } else {
            String::new()
        },
        headline_plain: if blocked {
            headline(&blocking, eol.as_ref(), false)
        } else {
            String::new()
        },
        new: blocking,
        carried: sorted(carried),
        error: None,
        scan_ran: true,
        mode,
        eol,
        baseline_missing: baseline_missing && mode == GateMode::On,
    }
}

/// `SecurityGate::logLines()`: the deploy log lines of a result: the headline and the blocking
/// problems, or "passed", then the problems with an exploit signal that the running image has
/// too.
pub fn log_lines(result: &GateResult) -> Vec<String> {
    if result.error.is_some() {
        return if result.headline.is_empty() {
            Vec::new()
        } else {
            vec![result.headline.clone()]
        };
    }

    if !result.scan_ran {
        return Vec::new();
    }

    let mut lines = Vec::new();
    if result.blocked {
        lines.push(result.headline.clone());
        lines.extend(row_lines(&result.new, None));
        if let Some(eol) = &result.eol {
            let sentence = format!("This image uses {} {}", ucfirst(&eol.name), eol.version);
            lines.push(format!(
                "  {}, which no longer gets security fixes.",
                coolify_remote::text::php_trim(&sentence)
            ));
        }
        lines.push("See the Security page for all problems.".to_owned());
    } else {
        lines.push("Image scan: passed.".to_owned());
    }

    if result.baseline_missing {
        lines.push(
            "Coolify has no earlier scan of this app to compare with. This deploy becomes the baseline."
                .to_owned(),
        );
    }

    if !result.carried.is_empty() {
        lines.push(format!(
            "{}{} that attackers {}.",
            if result.baseline_missing {
                "This image has "
            } else {
                "Already in the running image: "
            },
            problems(result.carried.len()),
            use_phrase(&result.carried)
        ));
        lines.extend(row_lines(
            &result.carried,
            if result.baseline_missing {
                None
            } else {
                Some("already running")
            },
        ));
    }

    lines
}

/// `SecurityGate::stored()`: the result as it is stored on the deployment (`scan_gate_result`):
/// the surfaces read `blocked`, `scan_ran`, `error`, `count`, `headline`, and `top` (the blocking
/// problems).
pub fn stored(result: &GateResult) -> Value {
    let rows = |rows: &[GateRow]| -> Vec<Value> {
        rows.iter().take(LOG_LIMIT).map(GateRow::to_value).collect()
    };
    json!({
        "blocked": result.blocked,
        "scan_ran": result.scan_ran,
        "error": result.error.map(ErrorCode::value),
        "mode": result.mode.value(),
        "count": result.new.len() + usize::from(result.eol.is_some()),
        "headline": result.headline,
        "headline_plain": result.headline_plain,
        "top": rows(&result.new),
        "carried": rows(&result.carried),
        "carried_count": result.carried.len(),
        "baseline_missing": result.baseline_missing,
    })
}

/// `data_get($scan->summary, $key)`: null and missing are `None`.
fn summary_value<'a>(scan: &'a ImageScanRow, key: &str) -> Option<&'a Value> {
    scan.summary
        .as_ref()
        .and_then(|summary| summary.get(key))
        .filter(|value| !value.is_null())
}

/// `(bool) $value` of a decoded JSON value.
fn php_bool(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty() && s != "0",
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        _ => false,
    }
}

/// `filled($value)` of a string.
fn filled(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.trim().is_empty())
}

/// `ucfirst($text)`.
fn ucfirst(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// "are using" when a row is exploited, else "are likely to use".
fn use_phrase(rows: &[GateRow]) -> &'static str {
    if rows
        .iter()
        .any(|row| row.signal == Some(ExploitSignal::Exploited))
    {
        "are using"
    } else {
        "are likely to use"
    }
}

/// `SecurityGate::headline()`.
fn headline(blocking: &[GateRow], eol: Option<&GateEol>, counted: bool) -> String {
    let signal: Vec<GateRow> = blocking
        .iter()
        .filter(|row| row.signal.is_some())
        .cloned()
        .collect();
    if !signal.is_empty() {
        let phrase = use_phrase(&signal);
        return if counted {
            format!(
                "Deploy blocked: this image has {} that attackers {phrase}.",
                problems(signal.len())
            )
        } else {
            format!("Deploy blocked: this image has security problems that attackers {phrase}.")
        };
    }

    if !counted {
        return "Deploy blocked: this image needs a security update.".to_owned();
    }

    let count = blocking.len() + usize::from(eol.is_some());
    format!(
        "Deploy blocked: this image has {} that {} a security update.",
        problems(count),
        if count == 1 { "needs" } else { "need" }
    )
}

/// `SecurityGate::problems()`.
fn problems(count: usize) -> String {
    if count > 1 {
        format!("{count} security problems")
    } else {
        "a security problem".to_owned()
    }
}

/// `number_format($score, 1)`: one decimal, rounded half away from zero. Without the thousands
/// separator: a CVSS score is 10 at most.
fn score_text(score: f64) -> String {
    format!("{:.1}", (score * 10.0).round() / 10.0)
}

/// `SecurityGate::rowLines()`.
fn row_lines(rows: &[GateRow], suffix: Option<&str>) -> Vec<String> {
    let mut lines = Vec::new();
    for row in rows.iter().take(LOG_LIMIT) {
        let score = row
            .score
            .map(|score| format!("{} ", score_text(score)))
            .unwrap_or_default();
        let note = match (suffix, row.signal) {
            (Some(suffix), _) => suffix.to_owned(),
            (None, Some(ExploitSignal::Exploited)) => "used by attackers".to_owned(),
            (None, Some(ExploitSignal::Likely)) => "likely to be exploited".to_owned(),
            (None, None) => match row.fixed_in.as_deref() {
                Some(fixed_in) if filled(Some(fixed_in)) => format!("fixed in {fixed_in}"),
                _ => String::new(),
            },
        };
        let line = format!(
            "  {}  {score}{}  {} {}  {note}",
            row.display_id,
            ucfirst(&row.severity),
            row.package,
            row.version
        );
        lines.push(
            line.trim_end_matches([' ', '\n', '\r', '\t', '\x0B', '\0'])
                .to_owned(),
        );
    }
    if rows.len() > LOG_LIMIT {
        lines.push(format!("  and {} more", rows.len() - LOG_LIMIT));
    }
    lines
}

/// `SecurityGate::hasPackageFix()`.
fn has_package_fix(finding: &Finding) -> bool {
    finding.fix_state == "fixed" && filled(finding.fixed_in.as_deref())
}

/// `SecurityGate::idSet()`: the IDs and aliases of the findings, lowercase.
fn id_set(findings: &[Finding]) -> HashSet<String> {
    findings
        .iter()
        .flat_map(Finding::ids)
        .filter(|id| !id.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// `SecurityGate::inSet()`.
fn in_set(finding: &Finding, ids: &HashSet<String>) -> bool {
    finding
        .ids()
        .any(|id| !id.is_empty() && ids.contains(&id.to_lowercase()))
}

/// `SecurityGate::row()`.
fn row(finding: &Finding, signal: Option<ExploitSignal>) -> GateRow {
    GateRow {
        id: text::clean_str(&finding.id, 128),
        display_id: text::clean_str(&finding.display_id, 128),
        severity: text::clean_str(&finding.severity, 16).to_ascii_lowercase(),
        score: finding.cvss_score,
        package: text::clean_str(&finding.package, 200),
        version: text::clean_str(&finding.version, 100),
        fixed_in: finding
            .fixed_in
            .as_deref()
            .map(|fixed_in| text::clean_str(fixed_in, 100)),
        kev: finding.kev,
        signal,
    }
}

/// `SecurityGate::sorted()`: exploited first, then likely, then the score.
fn sorted(mut rows: Vec<GateRow>) -> Vec<GateRow> {
    fn key(row: &GateRow) -> (i32, f64, i32) {
        let rank = match row.signal {
            Some(ExploitSignal::Exploited) => 2,
            Some(ExploitSignal::Likely) => 1,
            None => 0,
        };
        let severity = match row.severity.as_str() {
            "critical" => 2,
            "high" => 1,
            _ => 0,
        };
        (rank, row.score.unwrap_or(-1.0), severity)
    }
    rows.sort_by(|a, b| key(b).partial_cmp(&key(a)).unwrap_or(Ordering::Equal));
    rows
}

#[cfg(test)]
pub(crate) mod tests {
    use coolify_db::image_scans::{STATUS_COMPLETED, STATUS_FAILED};

    use super::*;
    use crate::image_scan::resource_scans::tests::scan;
    use crate::image_scan::rules::tests::{ignore, now};

    /// `securityGateFinding()`.
    pub(crate) fn finding(id: &str, overrides: Value) -> Value {
        let mut row = json!({
            "id": id, "aliases": [], "severity": "high", "package": "openssl", "version": "3.0.13", "type": "deb",
            "artifactId": format!("a-{id}"), "fixedIn": null, "fixState": "not-fixed", "kev": false, "epss": null,
            "cvssScore": 8.1, "namespace": "debian:distro:debian:12",
        });
        if let (Some(row), Some(overrides)) = (row.as_object_mut(), overrides.as_object()) {
            for (key, value) in overrides {
                row.insert(key.clone(), value.clone());
            }
        }
        row
    }

    /// `securityGateExploited()`.
    pub(crate) fn exploited(id: &str, overrides: Value) -> Value {
        let mut row = finding(id, json!({"kev": true}));
        if let (Some(row), Some(overrides)) = (row.as_object_mut(), overrides.as_object()) {
            for (key, value) in overrides {
                row.insert(key.clone(), value.clone());
            }
        }
        row
    }

    /// `securityGateScan()`: a completed scan with the findings and the summary.
    pub(crate) fn gate_scan(findings: Vec<Value>, summary: Value) -> ImageScanRow {
        let mut scan = scan("app:1", STATUS_COMPLETED, None);
        scan.builds_image = true;
        scan.findings = Some(Value::Array(findings));
        scan.summary = Some(summary);
        scan
    }

    /// `securityGateIgnores()` without ignores.
    fn no_ignores() -> RelevanceRules {
        RelevanceRules::from_ignores(Vec::new(), false, now())
    }

    fn no_context() -> Map<String, Value> {
        Map::new()
    }

    /// `SecurityGate::evaluate($new, $running, $mode, $rules)` with the other defaults.
    fn gate(
        new: &ImageScanRow,
        running: Option<&ImageScanRow>,
        mode: GateMode,
        rules: &RelevanceRules,
    ) -> GateResult {
        evaluate(
            Some(new),
            running,
            mode,
            rules,
            None,
            &no_context(),
            OnScanError::Deploy,
            false,
        )
    }

    fn ids(rows: &[GateRow]) -> Vec<&str> {
        rows.iter().map(|row| row.id.as_str()).collect()
    }

    #[test]
    fn on_blocks_a_new_finding_that_attackers_are_using() {
        let result = gate(
            &gate_scan(vec![exploited("CVE-2026-1", json!({}))], json!({})),
            Some(&gate_scan(vec![], json!({}))),
            GateMode::On,
            &no_ignores(),
        );

        assert!(result.blocked);
        assert_eq!(result.error, None);
        assert_eq!(
            result.headline,
            "Deploy blocked: this image has a security problem that attackers are using."
        );
        assert_eq!(ids(&result.new), ["CVE-2026-1"]);
        assert!(result.carried.is_empty());
    }

    #[test]
    fn on_passes_a_finding_that_the_running_image_has_too_and_lists_it_as_a_warning() {
        let result = gate(
            &gate_scan(
                vec![exploited(
                    "CVE-2026-1",
                    json!({"cvssScore": 9.8, "severity": "critical"}),
                )],
                json!({}),
            ),
            Some(&gate_scan(
                vec![exploited("CVE-2026-1", json!({}))],
                json!({}),
            )),
            GateMode::On,
            &no_ignores(),
        );

        assert!(!result.blocked);
        assert_eq!(result.headline, "");
        assert_eq!(ids(&result.carried), ["CVE-2026-1"]);
        assert_eq!(
            log_lines(&result),
            [
                "Image scan: passed.",
                "Already in the running image: a security problem that attackers are using.",
                "  CVE-2026-1  9.8 Critical  openssl 3.0.13  already running",
            ]
        );
    }

    #[test]
    fn on_does_not_block_without_an_earlier_scan_of_an_app_that_ran_before() {
        let result = evaluate(
            Some(&gate_scan(
                vec![exploited("CVE-2026-1", json!({}))],
                json!({}),
            )),
            None,
            GateMode::On,
            &no_ignores(),
            None,
            &no_context(),
            OnScanError::Deploy,
            true,
        );

        assert!(!result.blocked);
        assert!(result.baseline_missing);
        assert_eq!(
            log_lines(&result),
            [
                "Image scan: passed.",
                "Coolify has no earlier scan of this app to compare with. This deploy becomes the baseline.",
                "This image has a security problem that attackers are using.",
                "  CVE-2026-1  8.1 High  openssl 3.0.13  used by attackers",
            ]
        );
    }

    #[test]
    fn strict_still_blocks_without_a_baseline() {
        let result = evaluate(
            Some(&gate_scan(
                vec![exploited("CVE-2026-1", json!({}))],
                json!({}),
            )),
            None,
            GateMode::Strict,
            &no_ignores(),
            None,
            &no_context(),
            OnScanError::Deploy,
            true,
        );

        assert!(result.blocked);
        assert!(!result.baseline_missing);
    }

    #[test]
    fn on_matches_a_carried_over_finding_by_its_aliases() {
        let result = gate(
            &gate_scan(
                vec![exploited(
                    "GHSA-aaaa-bbbb-cccc",
                    json!({"aliases": ["CVE-2026-7"]}),
                )],
                json!({}),
            ),
            Some(&gate_scan(
                vec![finding("CVE-2026-7", json!({"severity": "low"}))],
                json!({}),
            )),
            GateMode::On,
            &no_ignores(),
        );

        assert!(!result.blocked);
        assert_eq!(result.carried.len(), 1);
    }

    #[test]
    fn on_treats_every_finding_as_new_on_the_first_deploy() {
        let result = gate(
            &gate_scan(
                vec![
                    exploited("CVE-2026-1", json!({})),
                    finding("CVE-2026-2", json!({"epss": 0.4})),
                ],
                json!({}),
            ),
            None,
            GateMode::On,
            &no_ignores(),
        );

        assert!(result.blocked);
        assert_eq!(
            result.headline,
            "Deploy blocked: this image has 2 security problems that attackers are using."
        );
        assert_eq!(ids(&result.new), ["CVE-2026-1", "CVE-2026-2"]);
    }

    #[test]
    fn on_does_not_block_when_the_epss_of_a_finding_that_already_runs_went_up() {
        let result = gate(
            &gate_scan(
                vec![finding("CVE-2026-1", json!({"epss": 0.35}))],
                json!({}),
            ),
            Some(&gate_scan(
                vec![finding("CVE-2026-1", json!({"epss": 0.04}))],
                json!({}),
            )),
            GateMode::On,
            &no_ignores(),
        );

        assert!(!result.blocked);
        let signals: Vec<_> = result.carried.iter().map(|row| row.signal).collect();
        assert_eq!(signals, [Some(ExploitSignal::Likely)]);
        assert_eq!(
            log_lines(&result)[1],
            "Already in the running image: a security problem that attackers are likely to use."
        );
    }

    #[test]
    fn on_says_likely_to_use_when_no_blocking_finding_is_exploited() {
        let result = gate(
            &gate_scan(vec![finding("CVE-2026-1", json!({"epss": 0.2}))], json!({})),
            None,
            GateMode::On,
            &no_ignores(),
        );

        assert_eq!(
            result.headline,
            "Deploy blocked: this image has a security problem that attackers are likely to use."
        );
    }

    #[test]
    fn on_does_not_block_on_critical_findings_with_a_fix_and_no_exploit_signal() {
        let result = gate(
            &gate_scan(
                vec![finding(
                    "CVE-2026-1",
                    json!({"severity": "critical", "fixState": "fixed", "fixedIn": "3.0.15"}),
                )],
                json!({"eol_distro": {"name": "debian", "version": "9"}}),
            ),
            None,
            GateMode::On,
            &no_ignores(),
        );

        assert!(!result.blocked);
        assert!(result.new.is_empty());
    }

    #[test]
    fn on_skips_ignored_findings_and_cpe_only_os_matches() {
        let rules =
            RelevanceRules::from_ignores(vec![ignore(Some("CVE-2026-1"), None)], false, now());
        let result = gate(
            &gate_scan(
                vec![
                    exploited("CVE-2026-1", json!({})),
                    exploited("CVE-2026-2", json!({"namespace": "nvd:cpe"})),
                ],
                json!({}),
            ),
            None,
            GateMode::On,
            &rules,
        );

        assert!(!result.blocked);
    }

    #[test]
    fn on_does_not_block_a_relevant_medium_finding_it_has_no_exploit_signal() {
        let medium = finding(
            "CVE-2026-1",
            json!({"severity": "medium", "cvssScore": 5.3, "epss": 0.5, "fixState": "fixed", "fixedIn": "3.0.15"}),
        );
        for count_medium in [false, true] {
            let rules = RelevanceRules::from_ignores(Vec::new(), count_medium, now());
            let result = gate(
                &gate_scan(vec![medium.clone()], json!({})),
                Some(&gate_scan(vec![], json!({}))),
                GateMode::On,
                &rules,
            );

            assert!(!result.blocked);
            assert!(result.carried.is_empty());
        }
    }

    #[test]
    fn strict_blocks_a_critical_finding_with_a_package_fix() {
        let result = gate(
            &gate_scan(
                vec![finding(
                    "CVE-2026-1",
                    json!({"severity": "critical", "fixState": "fixed", "fixedIn": "3.0.15"}),
                )],
                json!({}),
            ),
            None,
            GateMode::Strict,
            &no_ignores(),
        );

        assert!(result.blocked);
        assert_eq!(
            result.headline,
            "Deploy blocked: this image has a security problem that needs a security update."
        );
        assert_eq!(
            log_lines(&result),
            [
                "Deploy blocked: this image has a security problem that needs a security update.",
                "  CVE-2026-1  8.1 Critical  openssl 3.0.13  fixed in 3.0.15",
                "See the Security page for all problems.",
            ]
        );
    }

    #[test]
    fn strict_blocks_a_medium_finding_with_a_package_fix_only_when_it_is_relevant() {
        let cases = [
            // medium without EPSS
            (json!({}), false, false),
            // medium with EPSS 0.10 or more
            (json!({"epss": 0.2}), false, true),
            // medium when medium problems count
            (json!({}), true, true),
            // low when medium problems count
            (json!({"severity": "low", "cvssScore": 3.1}), true, false),
        ];
        for (overrides, count_medium, blocked) in cases {
            let mut medium = finding(
                "CVE-2026-1",
                json!({"severity": "medium", "cvssScore": 5.3, "fixState": "fixed", "fixedIn": "3.0.15"}),
            );
            if let (Some(row), Some(overrides)) = (medium.as_object_mut(), overrides.as_object()) {
                for (key, value) in overrides {
                    row.insert(key.clone(), value.clone());
                }
            }
            let rules = RelevanceRules::from_ignores(Vec::new(), count_medium, now());
            let result = gate(
                &gate_scan(vec![medium], json!({})),
                None,
                GateMode::Strict,
                &rules,
            );

            assert_eq!(result.blocked, blocked, "{overrides} {count_medium}");
        }
    }

    #[test]
    fn strict_does_not_block_a_relevant_medium_finding_without_a_package_fix() {
        let medium = finding(
            "CVE-2026-1",
            json!({"severity": "medium", "cvssScore": 5.3, "epss": 0.2}),
        );
        let result = gate(
            &gate_scan(vec![medium], json!({})),
            None,
            GateMode::Strict,
            &no_ignores(),
        );

        assert!(!result.blocked);
    }

    #[test]
    fn strict_blocks_an_end_of_life_os() {
        let result = gate(
            &gate_scan(
                vec![],
                json!({"eol_distro": {"name": "debian", "version": "9"}}),
            ),
            None,
            GateMode::Strict,
            &no_ignores(),
        );

        assert!(result.blocked);
        assert_eq!(
            result.headline,
            "Deploy blocked: this image has a security problem that needs a security update."
        );
        assert!(log_lines(&result).contains(
            &"  This image uses Debian 9, which no longer gets security fixes.".to_owned()
        ));
        assert_eq!(stored(&result)["count"], json!(1));
    }

    #[test]
    fn strict_uses_the_plural_for_several_problems_that_need_an_update() {
        let result = gate(
            &gate_scan(
                vec![finding(
                    "CVE-2026-1",
                    json!({"fixState": "fixed", "fixedIn": "1.1"}),
                )],
                json!({"eol_distro": {"name": "alpine", "version": "3.16"}}),
            ),
            None,
            GateMode::Strict,
            &no_ignores(),
        );

        assert_eq!(
            result.headline,
            "Deploy blocked: this image has 2 security problems that need a security update."
        );
    }

    #[test]
    fn strict_blocks_an_exploited_finding_that_the_running_image_has_too() {
        let result = gate(
            &gate_scan(vec![exploited("CVE-2026-1", json!({}))], json!({})),
            Some(&gate_scan(
                vec![exploited("CVE-2026-1", json!({}))],
                json!({}),
            )),
            GateMode::Strict,
            &no_ignores(),
        );

        assert!(result.blocked);
        assert_eq!(
            result.headline,
            "Deploy blocked: this image has a security problem that attackers are using."
        );
        assert!(result.carried.is_empty());
    }

    #[test]
    fn strict_does_not_block_on_medium_findings_or_findings_without_a_package_fix() {
        let result = gate(
            &gate_scan(
                vec![
                    finding(
                        "CVE-2026-1",
                        json!({"severity": "medium", "cvssScore": 5.0, "fixState": "fixed", "fixedIn": "1.1"}),
                    ),
                    finding("CVE-2026-2", json!({"severity": "critical"})),
                ],
                json!({}),
            ),
            None,
            GateMode::Strict,
            &no_ignores(),
        );

        assert!(!result.blocked);
    }

    #[test]
    fn never_blocks_when_the_mode_is_off() {
        // The scan ran, and a scan error.
        for error in [None, Some(ErrorCode::Timeout)] {
            let result = evaluate(
                Some(&gate_scan(
                    vec![exploited("CVE-2026-1", json!({}))],
                    json!({"eol_distro": {"name": "debian", "version": "9"}}),
                )),
                None,
                GateMode::Off,
                &no_ignores(),
                error,
                &no_context(),
                OnScanError::Block,
                false,
            );

            assert!(!result.blocked);
            assert_eq!(result.headline, "");
            assert_eq!(result.scan_ran, error.is_none());
        }
    }

    #[test]
    fn follows_if_the_scan_cannot_run_for_a_scan_error() {
        let cases = [
            (
                OnScanError::Deploy,
                false,
                "Scan skipped: Not enough disk space on prod: 3 GB needed.",
            ),
            (
                OnScanError::Block,
                true,
                "Deploy blocked: Not enough disk space on prod: 3 GB needed.",
            ),
        ];
        for (on_scan_error, blocked, headline) in cases {
            let mut context = Map::new();
            context.insert("server".to_owned(), json!("prod"));
            context.insert("n".to_owned(), json!(3));
            let result = evaluate(
                None,
                None,
                GateMode::On,
                &no_ignores(),
                Some(ErrorCode::DiskSpace),
                &context,
                on_scan_error,
                false,
            );

            assert_eq!(result.blocked, blocked);
            assert!(!result.scan_ran);
            assert_eq!(result.error, Some(ErrorCode::DiskSpace));
            assert_eq!(stored(&result)["error"], json!("disk_space"));
            assert_eq!(result.headline, headline);
            assert_eq!(log_lines(&result), [headline]);
        }
    }

    #[test]
    fn treats_a_cut_scan_result_as_a_scan_that_could_not_run() {
        let cases = [
            (
                GateMode::On,
                OnScanError::Deploy,
                false,
                "Scan skipped: The scan result was too large to read completely.",
            ),
            (
                GateMode::Strict,
                OnScanError::Block,
                true,
                "Deploy blocked: The scan result was too large to read completely.",
            ),
        ];
        for (mode, on_scan_error, blocked, headline) in cases {
            let result = evaluate(
                Some(&gate_scan(vec![], json!({"truncated": true}))),
                None,
                mode,
                &no_ignores(),
                None,
                &no_context(),
                on_scan_error,
                false,
            );

            assert_eq!(result.blocked, blocked);
            assert!(!result.scan_ran);
            assert_eq!(result.error, Some(ErrorCode::Truncated));
            assert_eq!(stored(&result)["error"], json!("truncated"));
            assert_eq!(result.headline, headline);
        }
    }

    #[test]
    fn treats_a_failed_scan_as_an_unknown_error() {
        let mut failed = gate_scan(vec![], json!({}));
        failed.status = STATUS_FAILED.to_owned();
        let result = gate(&failed, None, GateMode::Strict, &no_ignores());

        assert!(!result.blocked);
        assert_eq!(result.error, Some(ErrorCode::Unknown));
        assert_eq!(result.headline, "Scan skipped: The scan could not run.");
    }

    #[test]
    fn orders_the_blocking_problems_exploited_first_and_caps_the_log_at_five() {
        let result = gate(
            &gate_scan(
                vec![
                    finding("LIKELY-1", json!({"epss": 0.5, "cvssScore": 9.9})),
                    exploited("KEV-LOW", json!({"cvssScore": 7.0})),
                    exploited("KEV-HIGH", json!({"cvssScore": 9.0})),
                    finding("LIKELY-2", json!({"epss": 0.5, "cvssScore": 7.5})),
                    finding("LIKELY-3", json!({"epss": 0.5, "cvssScore": 7.4})),
                    finding("LIKELY-4", json!({"epss": 0.5, "cvssScore": 7.3})),
                ],
                json!({}),
            ),
            None,
            GateMode::On,
            &no_ignores(),
        );

        let lines = log_lines(&result);
        let stored = stored(&result);

        assert_eq!(
            ids(&result.new),
            [
                "KEV-HIGH", "KEV-LOW", "LIKELY-1", "LIKELY-2", "LIKELY-3", "LIKELY-4"
            ]
        );
        assert_eq!(
            lines[1],
            "  KEV-HIGH  9.0 High  openssl 3.0.13  used by attackers"
        );
        assert_eq!(
            lines[3],
            "  LIKELY-1  9.9 High  openssl 3.0.13  likely to be exploited"
        );
        assert_eq!(lines[6], "  and 1 more");
        assert_eq!(stored["count"], json!(6));
        assert_eq!(stored["top"].as_array().map(Vec::len), Some(5));
        assert_eq!(stored["blocked"], json!(true));
        assert_eq!(stored["scan_ran"], json!(true));
        assert_eq!(stored["error"], Value::Null);
        assert_eq!(stored["mode"], json!("on"));
    }

    #[test]
    fn leaves_out_the_score_of_a_finding_without_one() {
        let result = gate(
            &gate_scan(
                vec![exploited("CVE-2026-1", json!({"cvssScore": null}))],
                json!({}),
            ),
            None,
            GateMode::On,
            &no_ignores(),
        );

        assert_eq!(
            log_lines(&result)[1],
            "  CVE-2026-1  High  openssl 3.0.13  used by attackers"
        );
    }

    #[test]
    fn counts_a_finding_once_when_it_appears_for_several_packages() {
        let result = gate(
            &gate_scan(
                vec![
                    exploited("CVE-2026-1", json!({})),
                    exploited("CVE-2026-1", json!({"package": "libssl3"})),
                ],
                json!({}),
            ),
            None,
            GateMode::On,
            &no_ignores(),
        );

        assert_eq!(result.new.len(), 1);
        assert_eq!(
            result.headline,
            "Deploy blocked: this image has a security problem that attackers are using."
        );
    }

    #[test]
    fn writes_a_headline_without_counts_for_chat_and_webhook_channels() {
        let cases = [
            // exploited
            (
                vec![
                    exploited("CVE-2026-1", json!({})),
                    exploited("CVE-2026-2", json!({})),
                ],
                json!({}),
                GateMode::On,
                "Deploy blocked: this image has 2 security problems that attackers are using.",
                "Deploy blocked: this image has security problems that attackers are using.",
            ),
            // likely
            (
                vec![finding("CVE-2026-1", json!({"epss": 0.3}))],
                json!({}),
                GateMode::On,
                "Deploy blocked: this image has a security problem that attackers are likely to use.",
                "Deploy blocked: this image has security problems that attackers are likely to use.",
            ),
            // update
            (
                vec![finding(
                    "CVE-2026-1",
                    json!({"fixState": "fixed", "fixedIn": "1.1"}),
                )],
                json!({"eol_distro": {"name": "debian", "version": "9"}}),
                GateMode::Strict,
                "Deploy blocked: this image has 2 security problems that need a security update.",
                "Deploy blocked: this image needs a security update.",
            ),
        ];
        for (findings, summary, mode, counted, plain) in cases {
            let result = gate(&gate_scan(findings, summary), None, mode, &no_ignores());

            assert_eq!(result.headline, counted);
            assert_eq!(result.headline_plain, plain);
            assert_eq!(stored(&result)["headline_plain"], json!(plain));
            assert!(!plain.chars().any(|c| c.is_ascii_digit()));
        }
    }

    #[test]
    fn stores_the_result_in_upstreams_key_order() {
        let result = gate(
            &gate_scan(
                vec![exploited("CVE-2026-1", json!({"fixedIn": "3.0.15"}))],
                json!({}),
            ),
            None,
            GateMode::On,
            &no_ignores(),
        );
        let stored = stored(&result);

        let keys: Vec<&str> = stored
            .as_object()
            .map(|map| map.keys().map(String::as_str).collect())
            .unwrap_or_default();
        assert_eq!(
            keys,
            [
                "blocked",
                "scan_ran",
                "error",
                "mode",
                "count",
                "headline",
                "headline_plain",
                "top",
                "carried",
                "carried_count",
                "baseline_missing",
            ]
        );
        assert_eq!(
            stored["top"][0],
            json!({
                "id": "CVE-2026-1",
                "display_id": "CVE-2026-1",
                "severity": "high",
                "score": 8.1,
                "package": "openssl",
                "version": "3.0.13",
                "fixed_in": "3.0.15",
                "kev": true,
                "signal": "exploited",
            })
        );
        assert_eq!(stored["carried"], json!([]));
        assert_eq!(stored["carried_count"], json!(0));
        assert_eq!(stored["baseline_missing"], json!(false));
    }

    #[test]
    fn formats_scores_like_number_format() {
        assert_eq!(score_text(9.0), "9.0");
        assert_eq!(score_text(8.1), "8.1");
        assert_eq!(score_text(7.25), "7.3");
        assert_eq!(score_text(10.0), "10.0");
    }
}
