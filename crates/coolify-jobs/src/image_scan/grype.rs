//! `GrypeReportParser` and `SyftPackageParser`: the NDJSON that the templates `grype.tmpl` and
//! `syft.tmpl` make the scanners write.

use std::sync::LazyLock;

use coolify_remote::text::php_trim;
use regex::Regex;
use serde_json::{Map, Value};

use super::finding::{Finding, Package, numeric};
use super::rules::{self, ExploitSignal};
use super::text;

const MAX_BYTES: usize = 10 * 1024 * 1024;
const SEVERITIES: [&str; 4] = ["critical", "high", "medium", "low"];
const MAX_LICENSES: usize = 32;
/// `json_decode($line, true, 8)`.
const MAX_DEPTH: usize = 8;

static FINDING_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$").unwrap_or_else(|_| unreachable!())
});

/// The end-of-life operating system of a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EolDistro {
    pub name: String,
    pub version: String,
}

impl EolDistro {
    /// `['name' => …, 'version' => …]`.
    pub fn to_value(&self) -> Value {
        serde_json::json!({"name": self.name, "version": self.version})
    }

    /// A stored `eol_distro`; `None` unless it is an object.
    pub fn from_value(value: Option<&Value>) -> Option<Self> {
        let map = value?.as_object()?;
        let text = |key: &str| {
            map.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        Some(Self {
            name: text("name"),
            version: text("version"),
        })
    }
}

/// The result of [`parse_grype_report`].
#[derive(Debug, Clone, PartialEq)]
pub struct GrypeReport {
    pub findings: Vec<Finding>,
    pub truncated: bool,
    pub eol: Option<EolDistro>,
}

/// The nesting depth of arrays and objects, as `json_decode()` counts it.
fn depth(value: &Value) -> usize {
    match value {
        Value::Array(items) => 1 + items.iter().map(depth).max().unwrap_or(0),
        Value::Object(map) => 1 + map.values().map(depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// One line of NDJSON as an array row (`is_array(json_decode($line, true, 8))`): an object, or
/// `None` for a list, a scalar, invalid JSON and a line that nests too deep. A list has none of
/// the keys the parsers read, so it never yields a row.
fn row(line: &str) -> Option<Map<String, Value>> {
    let line = php_trim(line);
    if line.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    if depth(&value) > MAX_DEPTH {
        return None;
    }
    match value {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

/// `(new GrypeReportParser)->parse($ndjson)` with the default limit of 5000 findings.
pub fn parse_grype_report(ndjson: &str) -> GrypeReport {
    parse_grype_report_with_limit(ndjson, 5000)
}

/// `(new GrypeReportParser)->parse($ndjson, $limit)`. The end-of-life line comes first in the
/// current template and last in older outputs; both are read. Over the limit, the findings with
/// an exploit signal (KEV, then likely) are kept first, then the highest risk, so a cut report
/// keeps the findings that matter most.
pub fn parse_grype_report_with_limit(ndjson: &str, limit: usize) -> GrypeReport {
    if ndjson.len() > MAX_BYTES {
        return GrypeReport {
            findings: Vec::new(),
            truncated: true,
            eol: None,
        };
    }
    let mut findings = Vec::new();
    let mut eol = None;
    for line in ndjson.split('\n') {
        let Some(row) = row(line) else {
            continue;
        };
        if row.contains_key("eol") && !row.contains_key("id") {
            eol = eol_of(row.get("eol"));
            continue;
        }
        if let Some(finding) = finding_of(&row) {
            findings.push(finding);
        }
    }
    let truncated = findings.len() > limit;
    if truncated {
        findings = most_important_first(findings);
        findings.truncate(limit);
    }
    GrypeReport {
        findings,
        truncated,
        eol,
    }
}

/// `mostImportantFirst()`: KEV first, then likely to be exploited, then the highest risk; the
/// report order breaks ties.
fn most_important_first(findings: Vec<Finding>) -> Vec<Finding> {
    let rank = |finding: &Finding| match rules::exploit_signal(finding) {
        Some(ExploitSignal::Exploited) => 0,
        Some(ExploitSignal::Likely) => 1,
        None => 2,
    };
    let mut keyed: Vec<(i32, f64, Finding)> = findings
        .into_iter()
        .map(|finding| (rank(&finding), -finding.risk.unwrap_or(0.0), finding))
        .collect();
    // A stable sort keeps the report order for equal keys.
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    keyed.into_iter().map(|(_, _, finding)| finding).collect()
}

fn eol_of(eol: Option<&Value>) -> Option<EolDistro> {
    let eol = eol?.as_object()?;
    let name = text::clean(eol.get("name"), 64);
    if name.is_empty() {
        return None;
    }
    Some(EolDistro {
        name,
        version: text::clean(eol.get("version"), 64),
    })
}

/// The values of a JSON list or object (`array_values()`), empty for anything else.
fn values(value: Option<&Value>) -> Vec<&Value> {
    match value {
        Some(Value::Array(items)) => items.iter().collect(),
        Some(Value::Object(map)) => map.values().collect(),
        _ => Vec::new(),
    }
}

fn finding_of(row: &Map<String, Value>) -> Option<Finding> {
    let id = text::clean(row.get("id"), 256);
    if !FINDING_ID.is_match(&id) {
        return None;
    }
    let severity = text::clean(row.get("sev"), 16).to_lowercase();
    let severity = if severity == "negligible" {
        "low".to_owned()
    } else {
        severity
    };
    let fix_state = text::clean(row.get("fix"), 32);
    let mut fixed_in = text::clean(row.get("sugg"), 128);
    if fixed_in.is_empty() && fix_state == "fixed" {
        fixed_in = text::clean(values(row.get("fixv")).first().copied(), 128);
    }
    let mut aliases: Vec<String> = Vec::new();
    for related in values(row.get("rel")) {
        let alias = text::clean(Some(related), 64);
        if !alias.is_empty() && alias != id && !aliases.contains(&alias) {
            aliases.push(alias);
        }
    }
    let severity = if SEVERITIES.contains(&severity.as_str()) {
        severity
    } else {
        "unknown".to_owned()
    };
    let package = text::clean(row.get("pkg"), 214);
    let version = text::clean(row.get("ver"), 128);
    let namespace = text::clean(row.get("ns"), 128);
    let title = text::title(
        row.get("desc").and_then(Value::as_str),
        row.get("rdesc").and_then(Value::as_str),
        &package,
        &version,
        &severity,
    );
    Some(Finding {
        display_id: Finding::display_id_for(&id, &aliases),
        r#type: text::clean(row.get("type"), 32),
        artifact_id: text::clean(row.get("art"), 64),
        fixed_in: (!fixed_in.is_empty()).then_some(fixed_in),
        fix_state,
        kev: row.get("kev") == Some(&Value::Bool(true)),
        epss: numeric(row.get("epss")),
        risk: numeric(row.get("risk")),
        summary: text::clean(row.get("desc"), 300),
        layer: "app".to_owned(),
        cvss_score: score(row.get("cvss"), row.get("rcvss"), &namespace),
        match_type: text::clean(row.get("mt"), 64),
        id,
        aliases,
        severity,
        package,
        version,
        title,
        namespace,
    })
}

#[derive(Debug, Clone, Copy)]
struct ScoreEntry {
    nvd: bool,
    version: f64,
    score: f64,
}

/// `score()`: the base score of the finding's own source (every entry for an NVD namespace,
/// otherwise the entries that are not from NVD), then the NVD score of the finding or its first
/// related vulnerability. Within a source the highest CVSS version wins (4.x, 3.1, 3.0); v2 is
/// ignored.
fn score(own: Option<&Value>, related: Option<&Value>, namespace: &str) -> Option<f64> {
    let own = score_entries(own);
    let related = score_entries(related);
    let is_nvd_namespace = namespace.to_lowercase().starts_with("nvd");
    let first: Vec<ScoreEntry> = if is_nvd_namespace {
        own.clone()
    } else {
        own.iter().copied().filter(|e| !e.nvd).collect()
    };
    let second: Vec<ScoreEntry> = own
        .iter()
        .chain(related.iter())
        .copied()
        .filter(|e| e.nvd)
        .collect();
    for group in [first, second] {
        let mut best: Option<ScoreEntry> = None;
        for entry in group {
            let better = match best {
                None => true,
                Some(b) => {
                    entry.version > b.version || (entry.version == b.version && entry.nvd && !b.nvd)
                }
            };
            if better {
                best = Some(entry);
            }
        }
        if let Some(best) = best {
            return Some(best.score);
        }
    }
    None
}

fn score_entries(entries: Option<&Value>) -> Vec<ScoreEntry> {
    let mut result = Vec::new();
    for entry in values(entries).into_iter().take(32) {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let (Some(score), Some(version)) =
            (numeric(entry.get("score")), numeric(entry.get("version")))
        else {
            continue;
        };
        if version < 3.0 || score <= 0.0 || score > 10.0 {
            continue;
        }
        result.push(ScoreEntry {
            nvd: text::clean(entry.get("source"), 128)
                .to_lowercase()
                .contains("nvd"),
            version,
            score,
        });
    }
    result
}

/// The result of [`parse_syft_packages`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyftPackages {
    pub packages: Vec<Package>,
    pub truncated: bool,
}

/// `(new SyftPackageParser)->parse($ndjson)` with the default limit of 10000 packages.
pub fn parse_syft_packages(ndjson: &str) -> SyftPackages {
    parse_syft_packages_with_limit(ndjson, 10000)
}

/// `(new SyftPackageParser)->parse($ndjson, $limit)`. Rows without a package `id` (such as the
/// `{"layers":…}` trailer of older templates) are skipped.
pub fn parse_syft_packages_with_limit(ndjson: &str, limit: usize) -> SyftPackages {
    if ndjson.len() > MAX_BYTES {
        return SyftPackages {
            packages: Vec::new(),
            truncated: true,
        };
    }
    let mut packages = Vec::new();
    let mut truncated = false;
    for line in ndjson.split('\n') {
        let Some(package) = row(line).and_then(|row| package_of(&row)) else {
            continue;
        };
        if packages.len() >= limit {
            truncated = true;
            break;
        }
        packages.push(package);
    }
    SyftPackages {
        packages,
        truncated,
    }
}

fn package_of(row: &Map<String, Value>) -> Option<Package> {
    let id = text::clean(row.get("id"), 64);
    let name = text::clean(row.get("name"), 214);
    if id.is_empty() || name.is_empty() {
        return None;
    }
    let mut licenses = Vec::new();
    for license in values(row.get("licenses")) {
        let license = text::clean(Some(license), 128);
        if !license.is_empty() && licenses.len() < MAX_LICENSES {
            licenses.push(license);
        }
    }
    Some(Package {
        id,
        name,
        version: text::clean(row.get("version"), 128),
        r#type: text::clean(row.get("type"), 32),
        licenses,
        purl: text::clean(row.get("purl"), 512),
        layer_id: text::clean(row.get("layer"), 80),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const GRYPE: &str = include_str!("../../tests/fixtures/image-scan/grype.ndjson");
    const GRYPE_KEV: &str = include_str!("../../tests/fixtures/image-scan/grype-kev.ndjson");
    const CANDIDATE_15_11: &str =
        include_str!("../../tests/fixtures/image-scan/grype-candidate-15.11.ndjson");
    const CANDIDATE_15_ALPINE: &str =
        include_str!("../../tests/fixtures/image-scan/grype-candidate-15-alpine.ndjson");
    const SYFT: &str = include_str!("../../tests/fixtures/image-scan/syft-packages.ndjson");

    fn grype_line(overrides: Value) -> String {
        let mut row = json!({
            "id": "CVE-2024-0001", "rel": [], "sev": "High", "pkg": "openssl", "ver": "1.0",
            "type": "apk", "art": "abc", "fix": "fixed", "fixv": ["1.1"], "kev": false, "desc": "x",
        });
        for (k, v) in overrides.as_object().unwrap() {
            row[k] = v.clone();
        }
        row.to_string()
    }

    fn parsed(overrides: Value) -> Finding {
        parse_grype_report(&grype_line(overrides))
            .findings
            .remove(0)
    }

    fn by_id<'a>(report: &'a GrypeReport, id: &str) -> &'a Finding {
        report.findings.iter().find(|f| f.id == id).unwrap()
    }

    #[test]
    fn parses_the_postgres_report() {
        let r = parse_grype_report(GRYPE);
        assert_eq!(r.findings.len(), 166);
        assert!(!r.truncated);
        assert_eq!(
            r.eol,
            Some(EolDistro {
                name: "alpine".into(),
                version: "3.16.2".into()
            })
        );
        let f = by_id(&r, "CVE-2025-1094");
        assert_eq!(f.severity, "high");
        assert_eq!(f.fixed_in.as_deref(), Some("15.11"));
        assert_eq!(f.fix_state, "fixed");
        assert_eq!(f.epss, Some(0.9003));
        assert!(!f.kev);
        assert_eq!(f.package, "postgresql");
        assert_eq!(f.version, "15.0");
        assert_eq!(f.r#type, "binary");
        assert_eq!(f.artifact_id, "0bbf5c9dd36d4a50");
        assert_eq!(f.risk, Some(70.2234));
        assert_eq!(f.layer, "app");
        assert!(f.aliases.is_empty());
        assert_eq!(f.cvss_score, Some(8.1));
        assert_eq!(f.display_id, "CVE-2025-1094");
        assert!(f.title.starts_with(
            "Improper neutralization of quoting syntax in PostgreSQL libpq functions"
        ));
        assert!(f.title.chars().count() <= 120);
        assert_eq!(f.namespace, "nvd:cpe");
        assert_eq!(f.match_type, "cpe-match");
    }

    #[test]
    fn reads_known_exploited_aliases_and_scores() {
        let r = parse_grype_report(GRYPE_KEV);
        assert_eq!(r.eol, None);
        let f = by_id(&r, "GHSA-jfh8-c2jp-5v3q");
        assert!(f.kev);
        assert_eq!(f.aliases, vec!["CVE-2021-44228"]);
        assert_eq!(f.severity, "critical");
        assert_eq!(f.display_id, "CVE-2021-44228");
        // Its own score comes before the NVD score.
        assert_eq!(f.cvss_score, Some(10.0));
        assert_eq!(f.title, "Remote code injection in Log4j");
        assert_eq!(f.namespace, "github:language:java");
        assert_eq!(f.match_type, "exact-direct-match");
        assert_eq!(by_id(&r, "GHSA-p6xc-xr62-6r2g").cvss_score, Some(8.6));

        // A finding survives the stored form.
        let first = &r.findings[0];
        assert_eq!(&Finding::from_value(&first.to_value()), first);
        let mut stored = first.to_value();
        stored["layer"] = json!("base");
        assert_eq!(Finding::from_value(&stored).layer, "base");
    }

    #[test]
    fn the_eol_line_is_not_a_finding_wherever_it_is() {
        let r = parse_grype_report(&format!(
            "{}\n{}",
            grype_line(json!({})),
            r#"{"eol":{"name":"alpine","version":"3.16"}}"#
        ));
        assert_eq!(r.findings.len(), 1);
        assert_eq!(
            r.eol,
            Some(EolDistro {
                name: "alpine".into(),
                version: "3.16".into()
            })
        );
        assert_eq!(parse_grype_report("{\"eol\": null}").eol, None);

        let mut lines: Vec<&str> = GRYPE.trim().split('\n').collect();
        let last = lines.pop().unwrap();
        lines.insert(0, last);
        let moved = parse_grype_report(&lines.join("\n"));
        assert_eq!(moved.findings.len(), 166);
        assert_eq!(moved.eol.map(|e| e.version), Some("3.16.2".to_owned()));
    }

    #[test]
    fn caps_findings_and_marks_truncation() {
        let r = parse_grype_report_with_limit(GRYPE, 10);
        assert_eq!(r.findings.len(), 10);
        assert!(r.truncated);
        assert_eq!(r.eol.map(|e| e.name), Some("alpine".to_owned()));
        let exact = parse_grype_report_with_limit(GRYPE_KEV, 7);
        assert_eq!(exact.findings.len(), 7);
        assert!(!exact.truncated);
    }

    #[test]
    fn a_cut_report_keeps_exploit_signals_then_the_highest_risk() {
        let lines = [
            grype_line(json!({"id": "LOW-RISK", "risk": 1.0})),
            grype_line(json!({"id": "HIGH-RISK", "risk": 50.0})),
            grype_line(
                json!({"id": "LIKELY", "risk": 0.5, "epss": 0.5, "cvss": [{"source": "nvd@nist.gov", "version": "3.1", "score": 8.0}]}),
            ),
            grype_line(json!({"id": "KEV", "risk": 0.1, "kev": true})),
        ];
        let r = parse_grype_report_with_limit(&lines.join("\n"), 3);
        assert!(r.truncated);
        let ids: Vec<&str> = r.findings.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["KEV", "LIKELY", "HIGH-RISK"]);
    }

    #[test]
    fn drops_invalid_lines_and_strips_control_characters() {
        let line = grype_line(json!({"id": "CVE-1\u{1b}", "pkg": "a\nb\u{1b}[31m", "fixv": []}));
        let r = parse_grype_report(&format!("not json\n{line}\n{}", json!({"id": "../../etc"})));
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].package, "ab[31m");
        assert_eq!(r.findings[0].id, "CVE-1");

        let deep = format!("{}{}", "[".repeat(20), "]".repeat(20));
        assert_eq!(
            parse_grype_report(&format!("{deep}\n{}", grype_line(json!({}))))
                .findings
                .len(),
            1
        );

        let long = parsed(
            json!({"pkg": "p".repeat(300), "ver": "v".repeat(300), "desc": "d".repeat(400)}),
        );
        assert_eq!(long.package.len(), 214);
        assert_eq!(long.version.len(), 128);
        assert_eq!(long.summary.len(), 300);

        let over = parse_grype_report(&"a".repeat(MAX_BYTES + 1));
        assert!(over.findings.is_empty());
        assert!(over.truncated);
        assert_eq!(over.eol, None);
    }

    #[test]
    fn severity_and_fix_version() {
        assert_eq!(parsed(json!({"sev": "Negligible"})).severity, "low");
        assert_eq!(parsed(json!({"sev": "Weird"})).severity, "unknown");
        assert_eq!(parsed(json!({"sev": null})).severity, "unknown");
        assert_eq!(
            parsed(json!({"sugg": "2.0", "fixv": ["1.1"]}))
                .fixed_in
                .as_deref(),
            Some("2.0")
        );
        assert_eq!(
            parsed(json!({"fixv": ["1.1", "1.2"]})).fixed_in.as_deref(),
            Some("1.1")
        );
        assert_eq!(
            parsed(json!({"fix": "not-fixed", "fixv": ["1.1"]})).fixed_in,
            None
        );
        assert_eq!(parsed(json!({"fix": "fixed", "fixv": []})).fixed_in, None);
        let plain = parsed(json!({}));
        assert_eq!(plain.epss, None);
        assert_eq!(plain.risk, None);
        assert_eq!(plain.cvss_score, None);
    }

    #[test]
    fn score_prefers_the_own_source_and_the_highest_version() {
        let nvd = |version: &str, score: f64| json!({"source": "nvd@nist.gov", "version": version, "score": score});
        let vendor = |version: &str, score: f64| json!({"source": "security@vendor.example", "version": version, "score": score});
        // The vendor score of a distro namespace wins over NVD.
        assert_eq!(
            parsed(json!({"ns": "debian:distro:debian:12", "cvss": [nvd("3.1", 9.8), vendor("3.1", 5.0)]})).cvss_score,
            Some(5.0)
        );
        // In an NVD namespace every entry counts; the same version prefers NVD.
        assert_eq!(
            parsed(json!({"ns": "nvd:cpe", "cvss": [vendor("3.1", 5.0), nvd("3.1", 9.8)]}))
                .cvss_score,
            Some(9.8)
        );
        // The highest CVSS version wins; v2 is ignored.
        assert_eq!(
            parsed(json!({"ns": "nvd:cpe", "cvss": [nvd("2.0", 10.0), nvd("3.0", 6.0), nvd("4.0", 7.0), nvd("3.1", 9.0)]})).cvss_score,
            Some(7.0)
        );
        // Without an own score, the NVD score of the related vulnerability.
        assert_eq!(
            parsed(json!({"ns": "github:language:go", "cvss": [], "rcvss": [nvd("3.1", 7.5)]}))
                .cvss_score,
            Some(7.5)
        );
        // Scores outside 0..10 are dropped.
        assert_eq!(
            parsed(json!({"cvss": [nvd("3.1", 0.0), nvd("3.1", 11.0)]})).cvss_score,
            None
        );
    }

    #[test]
    fn titles() {
        let title = |v: Value| parsed(v).title;
        assert_eq!(
            title(
                json!({"desc": "Fixes [the parser bug](https://e.com/x) in `libfoo` with **bold** and __two words__ at https://example.org/a?b=1 now"})
            ),
            "Fixes the parser bug in libfoo with bold and two words at now"
        );
        assert_eq!(
            title(json!({"desc": "Shows ![the alt text](https://e.com/i.png) and more words"})),
            "Shows the alt text and more words"
        );
        assert_eq!(
            title(json!({"desc": "Evil \u{202E}title with\u{200B} hidden\ncharacters and more"})),
            "Evil title with hidden characters and more"
        );
        assert_eq!(
            title(json!({"desc": "Reads client_encoding from PQescapeLiteral() calls today"})),
            "Reads client_encoding from PQescapeLiteral() calls today"
        );
        assert_eq!(
            title(json!({"desc": "Is this a problem? Yes it is a big problem."})),
            "Is this a problem?"
        );
        assert_eq!(
            title(
                json!({"desc": "A heap overflow in the parser. lower case continues here and more words"})
            ),
            "A heap overflow in the parser. lower case continues here and more words"
        );
        for (desc, expected) in [
            (
                "Issue summary: a crafted packet crashes the server. Impact: high.",
                "A crafted packet crashes the server.",
            ),
            (
                "SUMMARY: Heap overflow in the parser of headers",
                "Heap overflow in the parser of headers",
            ),
            (
                "Description: Vulnerability: the parser reads past the buffer end",
                "The parser reads past the buffer end",
            ),
            (
                "The summary: is not a label in the middle",
                "The summary: is not a label in the middle",
            ),
        ] {
            assert_eq!(title(json!({"desc": desc})), expected);
        }
        for desc in [
            "",
            "Buffer overflow",
            "**[x](https://e.com)**",
            "Heap buffer overflow",
        ] {
            assert_eq!(
                title(json!({"desc": desc, "rdesc": ""})),
                "openssl 1.0: high vulnerability",
                "{desc}"
            );
        }
        assert_eq!(
            title(
                json!({"desc": "", "rdesc": "Related vulnerability allows remote code execution"})
            ),
            "Related vulnerability allows remote code execution"
        );
        assert_eq!(
            title(
                json!({"desc": "Own description wins over the related one", "rdesc": "Related vulnerability allows remote code execution"})
            ),
            "Own description wins over the related one"
        );
    }

    #[test]
    fn long_titles_are_cut_at_a_word() {
        let f = parsed(json!({"desc": "overflow ".repeat(40)}));
        assert!(f.title.chars().count() <= 120);
        assert!(f.title.ends_with("overflow…"));
        assert!(!f.title.contains("  "));
        let long = parsed(json!({"desc": format!("{}{}", "a".repeat(100), " bb".repeat(40))}));
        assert!(long.title.chars().count() <= 120);
        assert!(long.title.ends_with("bb…"));
        assert_eq!(
            parsed(json!({"desc": format!("{} b c d", "a".repeat(300)), "rdesc": ""})).title,
            "openssl 1.0: high vulnerability"
        );
    }

    #[test]
    fn changelogs_yield_to_the_related_description() {
        let fips = "[3.0.7-28.0.1_fips] - Replace upstream references in fips man pages [Orabug: 35824276] - Add FIPS package change: add a provider";
        let brotli =
            "[1.0.9-10.0.1] - Fix out-of-bounds write in the Brotli decoder [CVE-2026-111497]";
        assert_eq!(
            parsed(json!({"desc": fips, "rdesc": "Issue summary: Processing a crafted certificate may crash OpenSSL. Impact: denial of service."})).title,
            "Processing a crafted certificate may crash OpenSSL."
        );
        assert_eq!(
            parsed(json!({"desc": fips, "rdesc": ""})).title,
            "Replace upstream references in fips man pages"
        );
        assert_eq!(
            parsed(json!({"desc": brotli, "rdesc": ""})).title,
            "Fix out-of-bounds write in the Brotli decoder"
        );
        assert_eq!(
            parsed(json!({"desc": "- Resolves: RHEL-1234 heap overflow in the zlib inflate code", "rdesc": "A heap overflow in inflate allows a crash"})).title,
            "A heap overflow in inflate allows a crash"
        );
        assert_eq!(
            text::tidy_title(
                "[3.0.7-28.0.1_fips] - Replace upstream references in fips man pages [Orabug: 35824276] - Add FIPS package change: add…"
            ),
            "Replace upstream references in fips man pages"
        );
        assert_eq!(
            text::tidy_title("A heap overflow in x-y - and more [see CVE-2026-1]"),
            "A heap overflow in x-y - and more [see CVE-2026-1]"
        );
    }

    #[test]
    fn parses_the_candidate_reports() {
        let a = parse_grype_report(CANDIDATE_15_11);
        let b = parse_grype_report(CANDIDATE_15_ALPINE);
        assert!(!a.findings.is_empty());
        assert_eq!(b.findings.len(), 64);
        assert_eq!(
            a.findings
                .iter()
                .filter(|f| f.severity == "critical")
                .count(),
            20
        );
    }

    fn syft_line(overrides: Value) -> String {
        let mut row = json!({
            "id": "abc123", "name": "openssl", "version": "1.0", "type": "apk",
            "licenses": ["MIT"], "purl": "pkg:apk/alpine/openssl@1.0", "layer": format!("sha256:{}", "a".repeat(64)),
        });
        for (k, v) in overrides.as_object().unwrap() {
            row[k] = v.clone();
        }
        row.to_string()
    }

    #[test]
    fn parses_the_postgres_package_list() {
        let r = parse_syft_packages(SYFT);
        assert_eq!(r.packages.len(), 46);
        assert!(!r.truncated);
        let p = &r.packages[0];
        assert_eq!(p.name, ".postgresql-rundeps");
        assert_eq!(p.r#type, "apk");
        assert!(p.layer_id.starts_with("sha256:"));
        assert!(p.licenses.is_empty());
        assert!(p.purl.starts_with("pkg:apk/alpine/.postgresql-rundeps@"));

        let capped = parse_syft_packages_with_limit(SYFT, 10);
        assert_eq!(capped.packages.len(), 10);
        assert!(capped.truncated);
    }

    #[test]
    fn syft_skips_rows_without_a_package() {
        let r = parse_syft_packages(&format!(
            "{}\n{}",
            syft_line(json!({})),
            r#"{"layers":["sha256:1","sha256:2"]}"#
        ));
        assert_eq!(r.packages.len(), 1);
        assert!(!r.truncated);
        assert_eq!(
            Package::from_value(&r.packages[0].to_value()),
            r.packages[0]
        );

        let good = syft_line(
            json!({"name": "a\u{1b}b\n", "licenses": ["MIT\u{1b}", ["x"], ""], "version": "v".repeat(300)}),
        );
        let r = parse_syft_packages(&format!("not json\n{good}\n{}", json!({"name": "no id"})));
        assert_eq!(r.packages.len(), 1);
        assert_eq!(r.packages[0].name, "ab");
        assert_eq!(r.packages[0].licenses, vec!["MIT"]);
        assert_eq!(r.packages[0].version.len(), 128);

        let over = parse_syft_packages(&"a".repeat(MAX_BYTES + 1));
        assert!(over.packages.is_empty());
        assert!(over.truncated);
    }
}
