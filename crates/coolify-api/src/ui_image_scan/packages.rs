//! `App\Livewire\ImageScan\Packages`: the packages of the images a resource runs, from the
//! stored package lists, with the SBOM downloads. The browser filters and pages the list (the
//! package lists are capped at 10,000 per image).

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Router};
use coolify_db::image_scans;
use coolify_jobs::image_scan::views;
use serde_json::{Value, json};

use super::{internal, jobs, load_resource};
use crate::auth::AuthCtx;
use crate::response::{ApiResult, ok};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/ui/image-scan/{kind}/{uuid}/packages", get(render))
}

/// One package of a stored package list (`render()`).
pub(crate) fn package_value(scan_uuid: &str, index: usize, image: &str, row: &Value) -> Value {
    let text = |key: &str| match row.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(true)) => "1".to_owned(),
        _ => String::new(),
    };
    let licenses: Vec<String> = match row.get("licenses") {
        Some(Value::Array(list)) => list
            .iter()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            })
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    };
    json!({
        "key": format!("{scan_uuid}-{index}"),
        "name": text("name"),
        "version": text("version"),
        "type": text("type"),
        "licenses": licenses,
        "purl": text("purl"),
        "image": image,
    })
}

/// `render()`.
async fn render(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((kind, uuid)): Path<(String, String)>,
) -> ApiResult {
    let jobs = jobs(&state)?;
    let resource = load_resource(&state, &ctx, &kind, &uuid).await?;
    let scans: Vec<_> = views::current_owned(jobs, &resource)
        .await
        .map_err(internal)?
        .into_iter()
        .filter(|s| s.scan.image_sbom_id.is_some())
        .collect();

    // `route("project.{type}.vulnerabilities.sbom", …)`.
    let environment_id = resource
        .record
        .get("environment_id")
        .and_then(Value::as_i64);
    let place = match environment_id {
        Some(id) => image_scans::views::places(&state.db, &[id])
            .await?
            .into_iter()
            .next(),
        None => None,
    };
    let download_base = place.map(|p| {
        format!(
            "/project/{}/environment/{}/{kind}/{uuid}/vulnerabilities/sbom",
            p.project_uuid, p.environment_uuid
        )
    });

    let mut packages: Vec<Value> = Vec::new();
    let mut truncated = false;
    let mut sbom_ids: Vec<i64> = Vec::new();
    for scan in &scans {
        let Some(sbom_id) = scan.scan.image_sbom_id else {
            continue;
        };
        let Some(sbom) = image_scans::find_sbom_by_id(&state.db, sbom_id).await? else {
            continue;
        };
        if !sbom_ids.contains(&sbom_id) {
            sbom_ids.push(sbom_id);
            truncated = truncated || sbom.packages_truncated;
        }
        if let Value::Array(rows) = &sbom.packages {
            for (index, row) in rows.iter().enumerate() {
                if row.is_object() {
                    packages.push(package_value(
                        &scan.scan.uuid,
                        index,
                        &scan.scan.image_ref,
                        row,
                    ));
                }
            }
        }
    }

    let mut image_options: Vec<String> = Vec::new();
    for scan in &scans {
        if !image_options.contains(&scan.scan.image_ref) {
            image_options.push(scan.scan.image_ref.clone());
        }
    }
    let downloads: Vec<Value> = scans
        .iter()
        .map(|s| {
            json!({
                "image": s.scan.image_ref,
                "cyclonedx": download_base.as_ref().map(|b| format!("{b}/{}/cyclonedx", s.scan.uuid)),
                "spdx": download_base.as_ref().map(|b| format!("{b}/{}/spdx", s.scan.uuid)),
            })
        })
        .collect();
    let shows_image = image_options.len() > 1;
    Ok(ok(json!({
        "has_scans": !scans.is_empty(),
        "packages": packages,
        "image_options": if shows_image { image_options } else { Vec::new() },
        "shows_image": shows_image,
        "truncated": truncated,
        "downloads": downloads,
    }))
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_read_the_stored_rows() {
        let row = json!({"name": "openssl", "version": "3.0.1", "type": "deb", "licenses": ["Apache-2.0", "MIT"], "purl": "pkg:deb/openssl"});
        let v = package_value("s1", 3, "nginx:1", &row);
        assert_eq!(v["key"], "s1-3");
        assert_eq!(v["licenses"], json!(["Apache-2.0", "MIT"]));
        assert_eq!(v["image"], "nginx:1");
        let empty = package_value("s1", 0, "nginx:1", &json!({}));
        assert_eq!(empty["name"], "");
        assert_eq!(empty["licenses"], json!([]));
    }
}
