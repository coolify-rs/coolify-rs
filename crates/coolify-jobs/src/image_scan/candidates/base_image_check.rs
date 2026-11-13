//! `App\Services\ImageScan\BaseImageCheck`: the newer base image of a Dockerfile build (spec 7.4,
//! 15.4), from the stored base check of a scan (`image_scans.candidates.base`).

use std::collections::HashSet;

use coolify_db::image_scans::{self, ImageScanRow};
use serde_json::Value;

use super::refresh;
use crate::Jobs;
use crate::image_scan::finding::Finding;
use crate::image_scan::image_ref::ImageRef;

/// What [`better_base`] returns.
#[derive(Debug, Clone, PartialEq)]
pub struct BetterBase {
    /// The base image as the Dockerfile writes it (`base_image_ref`).
    pub from: String,
    /// `from` with the tag of the candidate. Keeps the registry and name as the Dockerfile
    /// writes them.
    pub to: String,
    /// The stored `line` entry of the base check.
    pub entry: Value,
}

/// What [`base_image`] returns: of the relevant findings that the current base image has too
/// (`total`), how many the newer tag no longer has (`fixed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseImageUpdate {
    pub from: String,
    pub to: String,
    pub fixed: usize,
    pub total: usize,
}

/// `BaseImageCheck::baseImage($scan)`: a better base image for the final `FROM` of a Dockerfile
/// build. Both numbers are counted on the current findings and ignores, with the IDs stored by
/// the last check. `None` without a better line candidate that fixes one; failures of the base
/// check are not shown.
pub async fn base_image(
    jobs: &Jobs,
    scan: &ImageScanRow,
) -> Result<Option<BaseImageUpdate>, String> {
    let Some(better) = better_base(scan) else {
        return Ok(None);
    };
    let Some(owner) = image_scans::find_owner(&jobs.0.db, &scan.resource_type, scan.resource_id)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let relevant = refresh::relevant_findings(jobs, scan, &owner, None).await?;
    Ok(base_image_of(scan, better, &relevant))
}

/// The pure part of [`base_image`]: `relevant` holds the relevant findings of the scan
/// (`RefreshCandidates::relevantFindings($scan, $owner)`).
pub(crate) fn base_image_of(
    scan: &ImageScanRow,
    better: BetterBase,
    relevant: &[Finding],
) -> Option<BaseImageUpdate> {
    let base = scan.candidates.as_ref().and_then(|c| c.get("base"));
    let attributable_ids = id_set(base.and_then(|b| b.get("attributable_ids")));
    let fixed_ids = id_set(better.entry.get("fixed_ids"));
    /// Whether one of the IDs of the finding is in the list.
    fn has(finding: &Finding, ids: &HashSet<String>) -> bool {
        finding.ids().any(|id| ids.contains(&id.to_lowercase()))
    }
    let attributable: Vec<&Finding> = relevant
        .iter()
        .filter(|finding| has(finding, &attributable_ids))
        .collect();
    let total = attributable.len();
    let fixed = attributable
        .iter()
        .filter(|finding| has(finding, &fixed_ids))
        .count();
    if total == 0 || fixed == 0 {
        return None;
    }
    Some(BaseImageUpdate {
        from: better.from,
        to: better.to,
        fixed,
        total,
    })
}

/// `array_flip(array_map(fn ($id) => mb_strtolower((string) $id), (array) $ids))`.
fn id_set(ids: Option<&Value>) -> HashSet<String> {
    let text = |id: &Value| match id {
        Value::String(s) => Some(s.to_lowercase()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    match ids {
        Some(Value::Array(list)) => list.iter().filter_map(text).collect(),
        Some(Value::Object(map)) => map.values().filter_map(text).collect(),
        Some(scalar) => text(scalar).into_iter().collect(),
        None => HashSet::new(),
    }
}

/// `BaseImageCheck::betterBase($scan)`: the better `line` candidate of the base check, when the
/// check is for the current base image (`base_image_ref`) and the candidate is a newer tag of
/// the same image.
pub fn better_base(scan: &ImageScanRow) -> Option<BetterBase> {
    let from = scan.base_image_ref.as_deref().unwrap_or_default();
    let base = scan.candidates.as_ref()?.get("base")?.as_object()?;
    if from.is_empty()
        || base.get("status").and_then(Value::as_str) != Some("ok")
        || base.get("image").and_then(Value::as_str) != Some(from)
    {
        return None;
    }

    let entry = base.get("entries")?.as_array()?.iter().find(|entry| {
        entry.is_object()
            && entry.get("kind").and_then(Value::as_str) == Some("line")
            && entry.get("better") == Some(&Value::Bool(true))
            && entry.get("ref").is_some_and(Value::is_string)
    })?;

    let current = ImageRef::parse(from).ok()?;
    let candidate = ImageRef::parse(entry.get("ref")?.as_str()?).ok()?;
    let current_tag = current.tag.as_deref()?;
    let candidate_tag = candidate.tag.as_deref()?;
    if current.digest.is_some()
        || candidate_tag == current_tag
        || candidate.qualified_name() != current.qualified_name()
    {
        return None;
    }

    // `substr($from, 0, (int) strrpos($from, ':'))`: `from` has a tag, so it has a colon.
    let name = &from[..from.rfind(':').unwrap_or(0)];
    Some(BetterBase {
        from: from.to_owned(),
        to: format!("{name}:{candidate_tag}"),
        entry: entry.clone(),
    })
}

#[cfg(test)]
mod tests {
    use coolify_db::image_scans::STATUS_COMPLETED;
    use serde_json::json;

    use super::super::evaluator::tests::finding;
    use super::*;
    use crate::image_scan::resource_scans::tests::scan;

    fn line(reference: &str, better: bool, fixed_ids: Value) -> Value {
        json!({"ref": reference, "digest": format!("sha256:{}", "3".repeat(64)), "kind": "line",
            "better": better, "fixed_ids": fixed_ids, "fixes": {"critical": 0, "high": 1, "medium": 0},
            "added": 0, "left": {"critical": 0, "high": 0, "medium": 0}, "eol": null,
            "scanned_at": "2026-11-10T12:00:00+00:00"})
    }

    fn base_scan(from: &str, base: Value) -> ImageScanRow {
        let mut row = scan("appuuid:abc123", STATUS_COMPLETED, None);
        row.base_image_ref = Some(from.to_owned());
        row.builds_image = true;
        row.candidates = Some(
            json!({"status": "skipped", "checked_at": "2026-11-10T12:00:00+00:00",
            "error": null, "tags_truncated": false, "entries": [], "base": base}),
        );
        row
    }

    fn base(image: &str, status: &str, entries: Value, attributable_ids: Value) -> Value {
        json!({"image": image, "status": status, "checked_at": "2026-11-10T12:00:00+00:00", "error": null,
            "tags_truncated": false, "attributable": 2, "attributable_ids": attributable_ids,
            "current_digest": format!("sha256:{}", "1".repeat(64)), "entries": entries})
    }

    #[test]
    fn the_better_line_candidate_keeps_the_name_of_the_dockerfile() {
        let entry = line("postgres:15.15-alpine", true, json!(["CVE-1"]));
        let row = base_scan(
            "postgres:15.0-alpine",
            base(
                "postgres:15.0-alpine",
                "ok",
                json!([entry]),
                json!(["CVE-1"]),
            ),
        );
        let better = better_base(&row).unwrap();
        assert_eq!(better.from, "postgres:15.0-alpine");
        assert_eq!(better.to, "postgres:15.15-alpine");
        assert_eq!(better.entry, entry);

        // The Dockerfile writes the registry and `library/`; the stored ref is the short form.
        let row = base_scan(
            "docker.io/library/postgres:15.0-alpine",
            base(
                "docker.io/library/postgres:15.0-alpine",
                "ok",
                json!([entry]),
                json!([]),
            ),
        );
        assert_eq!(
            better_base(&row).unwrap().to,
            "docker.io/library/postgres:15.15-alpine"
        );

        let row = base_scan(
            "localhost:5000/team/app:1.2",
            base(
                "localhost:5000/team/app:1.2",
                "ok",
                json!([line("localhost:5000/team/app:1.3", true, json!([]))]),
                json!([]),
            ),
        );
        assert_eq!(better_base(&row).unwrap().to, "localhost:5000/team/app:1.3");
    }

    #[test]
    fn no_better_base_without_a_better_line_candidate_of_the_current_base_image() {
        let better = line("postgres:15.15-alpine", true, json!(["CVE-1"]));
        let none = |from: &str, base: Value| assert_eq!(better_base(&base_scan(from, base)), None);
        // The check failed or found nothing better.
        none(
            "postgres:15.0-alpine",
            base(
                "postgres:15.0-alpine",
                "no_better",
                json!([better]),
                json!([]),
            ),
        );
        none(
            "postgres:15.0-alpine",
            base(
                "postgres:15.0-alpine",
                "ok",
                json!([line("postgres:15.15-alpine", false, json!([]))]),
                json!([]),
            ),
        );
        none(
            "postgres:15.0-alpine",
            base("postgres:15.0-alpine", "ok", json!([]), json!([])),
        );
        // The check is of another base image than the Dockerfile has now.
        none(
            "postgres:16.0-alpine",
            base("postgres:15.0-alpine", "ok", json!([better]), json!([])),
        );
        // Not a newer tag of the same image.
        none(
            "postgres:15.15-alpine",
            base("postgres:15.15-alpine", "ok", json!([better]), json!([])),
        );
        none(
            "mysql:8.0",
            base("mysql:8.0", "ok", json!([better]), json!([])),
        );
        // A base image without a tag or pinned by digest has no line.
        none(
            "postgres",
            base("postgres", "ok", json!([better]), json!([])),
        );
        let pinned = format!("postgres@sha256:{}", "1".repeat(64));
        none(&pinned, base(&pinned, "ok", json!([better]), json!([])));
        // Entries of another kind, and rows that are not entries.
        let mut major = better.clone();
        major["kind"] = json!("major");
        none(
            "postgres:15.0-alpine",
            base(
                "postgres:15.0-alpine",
                "ok",
                json!([major, "text", null]),
                json!([]),
            ),
        );

        let mut row = scan("appuuid:abc123", STATUS_COMPLETED, None);
        assert_eq!(better_base(&row), None);
        row.base_image_ref = Some("postgres:15.0-alpine".to_owned());
        assert_eq!(better_base(&row), None);
        row.candidates = Some(json!({"status": "skipped", "entries": []}));
        assert_eq!(better_base(&row), None);
    }

    #[test]
    fn counts_the_attributable_findings_and_those_the_newer_base_image_fixes() {
        let entry = line(
            "postgres:15.15-alpine",
            true,
            json!(["CVE-1", "ghsa-2", "CVE-9"]),
        );
        let row = base_scan(
            "postgres:15.0-alpine",
            base(
                "postgres:15.0-alpine",
                "ok",
                json!([entry]),
                json!(["cve-1", "GHSA-2", "CVE-3"]),
            ),
        );
        let better = better_base(&row).unwrap();
        let mut aliased = finding("CVE-2", "high");
        aliased.aliases = vec!["GHSA-2".to_owned()];
        let relevant = [
            // In the base image and fixed by the newer tag.
            finding("CVE-1", "critical"),
            aliased,
            // In the base image, still in the newer tag.
            finding("CVE-3", "high"),
            // Added by the Dockerfile: not attributable, although the candidate does not have it.
            finding("CVE-9", "high"),
        ];
        assert_eq!(
            base_image_of(&row, better.clone(), &relevant),
            Some(BaseImageUpdate {
                from: "postgres:15.0-alpine".to_owned(),
                to: "postgres:15.15-alpine".to_owned(),
                fixed: 2,
                total: 3,
            })
        );
        // Nothing attributable is left (all ignored), or nothing of it is fixed.
        assert_eq!(
            base_image_of(&row, better.clone(), &[finding("CVE-9", "high")]),
            None
        );
        assert_eq!(
            base_image_of(&row, better.clone(), &[finding("CVE-3", "high")]),
            None
        );
        assert_eq!(base_image_of(&row, better, &[]), None);
    }
}
