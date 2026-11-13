//! `App\Services\ImageScan\{ExportCells, FindingsExport, FleetExport}`: the findings of the
//! Vulnerabilities list and the resources of the team Security page as text to copy (Markdown,
//! IDs) or a file to download (CSV, JSON), written exactly as upstream writes them.

use chrono::NaiveDateTime;
use coolify_db::image_scans::ImageScanIgnoreRow;
use serde::Serialize;
use serde_json::{Map, Value, json};

use super::super::finding::Finding;
use super::super::resource_scans;
use super::team::{SearchMatch, TeamEntry, images_line, state_label, status_label};

/// `Vulnerabilities::FIX_STATES`: the state labels of the findings exports of the Security page
/// and of the host page (an ignored finding keeps its state, `ignored`).
pub const FIX_STATES: [(&str, &str); 2] = [
    ("fixed", "Fix available upstream"),
    ("no_fix", "No fix yet"),
];

/// `$stateLabels[$state] ?? $state`.
fn label(labels: &[(&str, &str)], state: &str) -> String {
    labels
        .iter()
        .find(|(k, _)| *k == state)
        .map_or_else(|| state.to_owned(), |(_, l)| (*l).to_owned())
}
use super::{iso8601, team, ucfirst};

// ---------------------------------------------------------------------------------------------
// ExportCells
// ---------------------------------------------------------------------------------------------

/// A spreadsheet runs a cell that starts with one of these characters as a formula.
const FORMULA_PREFIXES: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

/// `ExportCells::markdown($value)`: a Markdown cell that cannot break the table.
pub fn markdown_cell(value: &str) -> String {
    php_trim(value)
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
}

/// `(string) $value` of a cell value, as PHP writes it.
fn cell_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => if *b { "yes" } else { "no" }.to_owned(),
        Value::String(s) => s.clone(),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => i.to_string(),
            (None, Some(f)) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{f:.0}"),
            (None, Some(f)) => f.to_string(),
            _ => n.to_string(),
        },
        other => other.to_string(),
    }
}

/// `ExportCells::csv($value)`: a CSV cell that a spreadsheet does not run as a formula; booleans
/// are `yes` and `no`.
pub fn csv_cell(value: &Value) -> String {
    let value = cell_string(value);
    match value.chars().next() {
        Some(first) if FORMULA_PREFIXES.contains(&first) => format!("'{value}"),
        _ => value,
    }
}

/// One field as `fputcsv($handle, $fields, escape: '')` writes it: enclosed in double quotes
/// when it holds a comma, a quote, a line break, a tab, or a space.
fn csv_field(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r', '\t', ' ']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_owned()
    }
}

fn csv_line(fields: &[String]) -> String {
    let mut line = fields
        .iter()
        .map(|f| csv_field(f))
        .collect::<Vec<_>>()
        .join(",");
    line.push('\n');
    line
}

/// `ExportCells::csvDocument($header, $rows)`.
pub fn csv_document(header: &[&str], rows: &[Vec<Value>]) -> String {
    let mut out = csv_line(&header.iter().map(|h| (*h).to_owned()).collect::<Vec<_>>());
    for row in rows {
        out.push_str(&csv_line(&row.iter().map(csv_cell).collect::<Vec<_>>()));
    }
    out
}

/// `ExportCells::markdownTable($title, $header, $rows)`: `rows` are Markdown cells already.
pub fn markdown_table(title: &str, header: &[&str], rows: &[Vec<String>]) -> String {
    let mut lines = vec![
        format!("**{}**", markdown_cell(title)),
        String::new(),
        format!("| {} |", header.join(" | ")),
        format!("|{}", " --- |".repeat(header.len())),
    ];
    for row in rows {
        lines.push(format!("| {} |", row.join(" | ")));
    }
    lines.join("\n") + "\n"
}

/// `ExportCells::markdownLink($text, $url)`.
pub fn markdown_link(text: &str, url: Option<&str>) -> String {
    match url.filter(|u| !u.is_empty()) {
        None => markdown_cell(text),
        Some(url) => format!(
            "[{}]({})",
            markdown_cell(text).replace('[', "\\[").replace(']', "\\]"),
            url.replace(' ', "%20").replace(')', "%29")
        ),
    }
}

fn php_trim(value: &str) -> &str {
    value.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B'])
}

/// `json_encode($value, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE)`.
fn pretty_unescaped(value: &Value) -> String {
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    if value.serialize(&mut serializer).is_err() {
        return String::new();
    }
    String::from_utf8(out).unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// FindingsExport
// ---------------------------------------------------------------------------------------------

/// One finding of the Vulnerabilities list (`ExportRow`).
#[derive(Debug, Clone)]
pub struct ExportRow {
    pub finding: Finding,
    /// `fixed`, `no_fix`, or `ignored`.
    pub state: String,
    pub ignore: Option<ImageScanIgnoreRow>,
    pub image: String,
    /// The ID the page shows (the host page shows the display ID); `None`: the finding ID.
    pub id: Option<String>,
}

impl ExportRow {
    fn id(&self) -> &str {
        self.id.as_deref().unwrap_or(&self.finding.id)
    }
}

fn severity(finding: &Finding) -> String {
    let severity = finding.severity.to_lowercase();
    match severity.as_str() {
        "critical" | "high" | "medium" | "low" => ucfirst(&severity),
        _ => "Unknown".to_owned(),
    }
}

/// `FindingsExport::markdown()`: a Markdown table for an issue, a pull request, or a chat. The
/// image column shows only for more than one image.
pub fn findings_markdown(
    title: &str,
    rows: &[ExportRow],
    with_image: bool,
    ignored: bool,
) -> String {
    let mut header = vec!["ID", "Severity", "Package", "Version", "Fixed in"];
    if with_image {
        header.push("Image");
    }
    if ignored {
        header.push("Ignored because");
    }
    let lines: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            let finding = &row.finding;
            let id = row.id();
            let mut cells = vec![
                markdown_link(id, resource_scans::advisory_url(id).as_deref()),
                format!(
                    "{}{}",
                    severity(finding),
                    if finding.kev { ", known exploited" } else { "" }
                ),
                markdown_cell(&finding.package),
                markdown_cell(&finding.version),
                markdown_cell(finding.fixed_in.as_deref().unwrap_or_default()),
            ];
            if with_image {
                cells.push(markdown_cell(&row.image));
            }
            if ignored {
                cells.push(markdown_cell(
                    row.ignore
                        .as_ref()
                        .map(|i| i.reason.as_str())
                        .unwrap_or_default(),
                ));
            }
            cells
        })
        .collect();
    markdown_table(title, &header, &lines)
}

/// `FindingsExport::ids()`: the finding IDs, once each, one per line.
pub fn findings_ids(rows: &[ExportRow]) -> String {
    let mut ids: Vec<&str> = Vec::new();
    for row in rows {
        if !ids.contains(&row.id()) {
            ids.push(row.id());
        }
    }
    if ids.is_empty() {
        String::new()
    } else {
        ids.join("\n") + "\n"
    }
}

/// `FindingsExport::entries()`.
fn finding_entries(rows: &[ExportRow], state_labels: &[(&str, &str)]) -> Vec<Value> {
    rows.iter()
        .map(|row| {
            let finding = &row.finding;
            let id = row.id();
            let mut aliases: Vec<String> = Vec::new();
            for alias in finding.ids() {
                if alias != id && !aliases.iter().any(|a| a == alias) {
                    aliases.push(alias.to_owned());
                }
            }
            json!({
                "id": id,
                "aliases": aliases,
                "severity": severity(finding),
                "known_exploited": finding.kev,
                "package": finding.package,
                "version": finding.version,
                "fixed_in": finding.fixed_in.as_deref().filter(|f| !f.is_empty()),
                "package_type": finding.r#type,
                "image": row.image,
                "fix_state": label(state_labels, &row.state),
                "epss": finding.epss,
                "cvss": finding.cvss_score,
                "url": resource_scans::advisory_url(id),
                "summary": finding.summary,
                "ignored": row.ignore.as_ref().map(|ignore| json!({
                    "reason": ignore.reason,
                    "until": ignore.expires_at.map(|t| t.format("%Y-%m-%d").to_string()),
                })),
            })
        })
        .collect()
}

/// `FindingsExport::csv($rows, $stateLabels)`; the pages pass [`FIX_STATES`].
pub fn findings_csv(rows: &[ExportRow], state_labels: &[(&str, &str)]) -> String {
    let header = [
        "id",
        "severity",
        "known_exploited",
        "package",
        "version",
        "fixed_in",
        "package_type",
        "image",
        "fix_state",
        "epss",
        "cvss",
        "aliases",
        "ignored_reason",
        "ignored_until",
        "url",
        "summary",
    ];
    let lines: Vec<Vec<Value>> = finding_entries(rows, state_labels)
        .into_iter()
        .map(|e| {
            let aliases = e["aliases"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            vec![
                e["id"].clone(),
                e["severity"].clone(),
                e["known_exploited"].clone(),
                e["package"].clone(),
                e["version"].clone(),
                e["fixed_in"].clone(),
                e["package_type"].clone(),
                e["image"].clone(),
                e["fix_state"].clone(),
                e["epss"].clone(),
                e["cvss"].clone(),
                Value::String(aliases),
                e["ignored"].get("reason").cloned().unwrap_or(Value::Null),
                e["ignored"].get("until").cloned().unwrap_or(Value::Null),
                e["url"].clone(),
                e["summary"].clone(),
            ]
        })
        .collect();
    csv_document(&header, &lines)
}

/// `FindingsExport::json($resource, $rows, $stateLabels)`; the pages pass [`FIX_STATES`].
pub fn findings_json(
    resource: Value,
    rows: &[ExportRow],
    state_labels: &[(&str, &str)],
    now: NaiveDateTime,
) -> String {
    coolify_compat::php_json::encode_pretty(&json!({
        "resource": resource,
        "exported_at": iso8601(now),
        "count": rows.len(),
        "findings": finding_entries(rows, state_labels),
    })) + "\n"
}

// ---------------------------------------------------------------------------------------------
// FleetExport
// ---------------------------------------------------------------------------------------------

/// `str(class_basename($resource))->snake()`: `application`, `service`,
/// `standalone_postgresql`, …
fn snake_type(morph: &str) -> String {
    let base = morph.rsplit('\\').next().unwrap_or(morph);
    let mut out = String::new();
    for (i, c) in base.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// `FleetExport::entries($rows, $searching, $stateLabels)`: one entry per resource. `rows` are
/// the entries of [`team::TeamVulnerabilities`], with their match while the page searches.
/// `base_url` makes the link of each resource absolute (`route()` writes the instance URL).
pub fn fleet_entries(
    rows: &[(TeamEntry, Option<SearchMatch>)],
    searching: bool,
    base_url: &str,
    now: NaiveDateTime,
) -> Vec<Map<String, Value>> {
    rows.iter()
        .map(|(entry, found)| {
            let security = &entry.security;
            let checked_at = security
                .checked_at
                .or_else(|| entry.scans.iter().filter_map(|s| s.scanned_at).max());
            let mut values = Map::new();
            values.insert("resource".into(), json!(entry.resource.name()));
            values.insert("type".into(), json!(snake_type(entry.resource.morph)));
            values.insert(
                "project".into(),
                json!(entry.place.as_ref().map(|p| p.project_name.clone())),
            );
            values.insert(
                "environment".into(),
                json!(entry.place.as_ref().map(|p| p.environment_name.clone())),
            );
            values.insert(
                "server".into(),
                json!(entry.server.as_ref().map(|s| s.name.clone())),
            );
            values.insert("status".into(), json!(status_label(security.status)));
            match (searching, found) {
                (true, Some(found)) => {
                    values.insert("match_id".into(), json!(found.id));
                    values.insert("severity".into(), json!(ucfirst(&found.severity)));
                    values.insert("package".into(), json!(found.package));
                    values.insert("version".into(), json!(found.version));
                    values.insert("state".into(), json!(state_label(&found.state)));
                    values.insert(
                        "fixed_in".into(),
                        json!(found.fixed_in.as_deref().filter(|f| !f.trim().is_empty())),
                    );
                    values.insert("other_matches".into(), json!(found.more));
                }
                _ => {
                    values.insert(
                        "action".into(),
                        json!(team::action(security, now).map(|a| a.text)),
                    );
                    values.insert("images".into(), json!(images_line(security, entry.images)));
                }
            }
            values.insert("checked_at".into(), json!(checked_at.map(iso8601)));
            values.insert(
                "url".into(),
                json!(
                    entry
                        .link
                        .as_ref()
                        .map(|path| format!("{}{path}", base_url.trim_end_matches('/')))
                ),
            );
            values
        })
        .collect()
}

/// `FleetExport::markdown($title, $entries, $searching)`.
pub fn fleet_markdown(title: &str, entries: &[Map<String, Value>], searching: bool) -> String {
    let header: Vec<&str> = if searching {
        vec![
            "Resource",
            "Project / Environment",
            "Server",
            "Match",
            "State",
        ]
    } else {
        vec![
            "Resource",
            "Project / Environment",
            "Server",
            "Status",
            "Action",
        ]
    };
    let s = |entry: &Map<String, Value>, key: &str| {
        entry
            .get(key)
            .map(super::action::value_string)
            .unwrap_or_default()
    };
    let rows: Vec<Vec<String>> = entries
        .iter()
        .map(|entry| {
            let place = format!("{} / {}", s(entry, "project"), s(entry, "environment"));
            let mut cells = vec![
                markdown_link(
                    &s(entry, "resource"),
                    entry.get("url").and_then(Value::as_str),
                ),
                markdown_cell(place.trim_matches([' ', '/'])),
                markdown_cell(&s(entry, "server")),
            ];
            if searching {
                let other = entry
                    .get("other_matches")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                cells.push(markdown_cell(&format!(
                    "{} ({}) {} {}{}",
                    s(entry, "match_id"),
                    s(entry, "severity"),
                    s(entry, "package"),
                    s(entry, "version"),
                    if other > 0 {
                        format!(", +{other} more")
                    } else {
                        String::new()
                    }
                )));
                cells.push(markdown_cell(&format!(
                    "{}{}",
                    s(entry, "state"),
                    match entry.get("fixed_in").and_then(Value::as_str) {
                        Some(fixed) => format!(" → {fixed}"),
                        None => String::new(),
                    }
                )));
            } else {
                cells.push(markdown_cell(&s(entry, "status")));
                cells.push(markdown_cell(&s(entry, "action")));
            }
            cells
        })
        .collect();
    markdown_table(title, &header, &rows)
}

/// `FleetExport::csv($entries, $searching)`.
pub fn fleet_csv(entries: &[Map<String, Value>], searching: bool) -> String {
    let header: Vec<&str> = if searching {
        vec![
            "resource",
            "type",
            "project",
            "environment",
            "server",
            "status",
            "match_id",
            "severity",
            "package",
            "version",
            "state",
            "fixed_in",
            "other_matches",
            "checked_at",
            "url",
        ]
    } else {
        vec![
            "resource",
            "type",
            "project",
            "environment",
            "server",
            "status",
            "action",
            "images",
            "checked_at",
            "url",
        ]
    };
    let rows: Vec<Vec<Value>> = entries
        .iter()
        .map(|entry| {
            header
                .iter()
                .map(|column| entry.get(*column).cloned().unwrap_or(Value::Null))
                .collect()
        })
        .collect();
    csv_document(&header, &rows)
}

/// `FleetExport::json($meta, $entries)`.
pub fn fleet_json(
    meta: Map<String, Value>,
    entries: &[Map<String, Value>],
    now: NaiveDateTime,
) -> String {
    let mut document = meta;
    document.insert("exported_at".into(), json!(iso8601(now)));
    document.insert("count".into(), json!(entries.len()));
    document.insert(
        "resources".into(),
        Value::Array(entries.iter().cloned().map(Value::Object).collect()),
    );
    pretty_unescaped(&Value::Object(document)) + "\n"
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 10)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
    }

    fn row(id: &str, kev: bool) -> ExportRow {
        ExportRow {
            finding: Finding::from_value(&json!({
                "id": id,
                "aliases": ["GHSA-aaaa-bbbb-cccc"],
                "severity": "high",
                "package": "open|ssl",
                "version": "3.0.1",
                "type": "deb",
                "fixedIn": "3.0.2",
                "fixState": "fixed",
                "kev": kev,
                "epss": 0.25,
                "cvssScore": 7.5,
                "summary": "=cmd()",
            })),
            state: "fixed".to_owned(),
            ignore: None,
            image: "nginx:1.27".to_owned(),
            id: None,
        }
    }

    #[test]
    fn cells_cannot_break_tables_or_run_formulas() {
        assert_eq!(markdown_cell(" a|b\\c\nd "), "a\\|b\\\\c d");
        assert_eq!(csv_cell(&json!("=SUM(A1)")), "'=SUM(A1)");
        assert_eq!(csv_cell(&json!(true)), "yes");
        assert_eq!(csv_cell(&json!(null)), "");
        assert_eq!(csv_cell(&json!(0.25)), "0.25");
        assert_eq!(
            markdown_link("CVE-1", Some("https://x/a b)")),
            "[CVE-1](https://x/a%20b%29)"
        );
        assert_eq!(markdown_link("[x]", None), "[x]");
        assert_eq!(
            csv_document(&["a", "b"], &[vec![json!("x y"), json!("q\"r")]]),
            "a,b\n\"x y\",\"q\"\"r\"\n"
        );
    }

    #[test]
    fn findings_as_markdown_ids_csv_and_json() {
        let rows = vec![row("CVE-2024-0001", true), row("CVE-2024-0001", false)];
        let markdown = findings_markdown("api: 2 findings", &rows, false, false);
        assert_eq!(markdown.lines().next(), Some("**api: 2 findings**"));
        assert!(markdown.contains(
            "| ID | Severity | Package | Version | Fixed in |\n| --- | --- | --- | --- | --- |\n"
        ));
        assert!(markdown.contains("| [CVE-2024-0001](https://nvd.nist.gov/vuln/detail/CVE-2024-0001) | High, known exploited | open\\|ssl | 3.0.1 | 3.0.2 |"));
        assert_eq!(findings_ids(&rows), "CVE-2024-0001\n");
        let csv = findings_csv(&rows[..1], &FIX_STATES);
        assert!(csv.starts_with("id,severity,known_exploited,package,version,fixed_in,package_type,image,fix_state,epss,cvss,aliases,ignored_reason,ignored_until,url,summary\n"));
        assert!(csv.contains("CVE-2024-0001,High,yes,open|ssl,3.0.1,3.0.2,deb,nginx:1.27,\"Fix available upstream\",0.25,7.5,GHSA-aaaa-bbbb-cccc,,,https://nvd.nist.gov/vuln/detail/CVE-2024-0001,'=cmd()\n"));
        let document = findings_json(json!({"name": "api"}), &rows[..1], &FIX_STATES, now());
        assert!(document.starts_with("{\n    \"resource\": {\n        \"name\": \"api\"\n    },\n    \"exported_at\": \"2026-11-10T12:00:00+00:00\",\n    \"count\": 1,"));
        assert!(document.ends_with("}\n"));
    }

    #[test]
    fn class_basenames_in_snake_case() {
        assert_eq!(snake_type(r"App\Models\Application"), "application");
        assert_eq!(
            snake_type(r"App\Models\StandalonePostgresql"),
            "standalone_postgresql"
        );
    }

    #[test]
    fn fleet_json_keeps_slashes_and_unicode() {
        let mut meta = Map::new();
        meta.insert("team".into(), json!("Acmé/x"));
        let document = fleet_json(meta, &[], now());
        assert!(document.contains("\"team\": \"Acmé/x\""));
        assert!(document.contains("\"resources\": []"));
    }
}
