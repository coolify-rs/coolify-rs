//! `App\Services\ImageScan\SbomDocument`: SBOM downloads made only from the stored package list,
//! so no image configuration (environment, history, labels) can be in them.

use std::sync::LazyLock;

use chrono::NaiveDateTime;
use rand::RngCore;
use regex::Regex;
use serde_json::{Map, Value, json};

use super::finding::Package;

/// `SbomDocument::FORMATS`.
pub const FORMATS: [&str; 2] = ["cyclonedx", "spdx"];

static PLAIN_LICENSE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9.+-]{1,64}\z").unwrap_or_else(|_| unreachable!()));

/// `Str::uuid()`: a random (version 4) UUID.
pub fn uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// `now()->toIso8601ZuluString()`.
fn zulu(now: NaiveDateTime) -> String {
    now.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// `SbomDocument::packages($sbom)`: the rows of `image_sboms.packages` that are objects.
pub fn packages(stored: &Value) -> Vec<Package> {
    let rows: Vec<&Value> = match stored {
        Value::Array(rows) => rows.iter().collect(),
        Value::Object(rows) => rows.values().collect(),
        _ => Vec::new(),
    };
    rows.into_iter()
        .filter(|row| row.is_object() || row.is_array())
        .map(Package::from_value)
        .collect()
}

/// `SbomDocument::cycloneDx($scan, $sbom)`: CycloneDX 1.6 JSON of the image `image_ref` with the
/// stored packages of its SBOM.
pub fn cyclone_dx(image_ref: &str, stored_packages: &Value, now: NaiveDateTime) -> Value {
    let components: Vec<Value> = packages(stored_packages)
        .iter()
        .map(|package| {
            let mut component = Map::new();
            component.insert("type".into(), json!("library"));
            component.insert("bom-ref".into(), json!(format!("package-{}", package.id)));
            component.insert("name".into(), json!(package.name));
            if !package.version.is_empty() {
                component.insert("version".into(), json!(package.version));
            }
            if !package.purl.is_empty() {
                component.insert("purl".into(), json!(package.purl));
            }
            if !package.licenses.is_empty() {
                let licenses: Vec<Value> = package
                    .licenses
                    .iter()
                    .map(|license| json!({"license": {"name": license}}))
                    .collect();
                component.insert("licenses".into(), Value::Array(licenses));
            }
            Value::Object(component)
        })
        .collect();
    json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "serialNumber": format!("urn:uuid:{}", uuid_v4()),
        "version": 1,
        "metadata": {
            "timestamp": zulu(now),
            "tools": {"components": [{"type": "application", "name": "Coolify"}]},
            "component": {
                "type": "container",
                "bom-ref": "image",
                "name": image_ref,
            },
        },
        "components": components,
    })
}

/// `SbomDocument::spdx($scan, $sbom)`: SPDX 2.3 JSON.
pub fn spdx(image_ref: &str, stored_packages: &Value, now: NaiveDateTime) -> Value {
    let mut spdx_packages = Vec::new();
    let mut relationships = Vec::new();
    for (index, package) in packages(stored_packages).iter().enumerate() {
        let spdx_id = format!("SPDXRef-Package-{}", index + 1);
        let mut entry = Map::new();
        entry.insert("name".into(), json!(package.name));
        entry.insert("SPDXID".into(), json!(spdx_id));
        if !package.version.is_empty() {
            entry.insert("versionInfo".into(), json!(package.version));
        }
        entry.insert("downloadLocation".into(), json!("NOASSERTION"));
        entry.insert("filesAnalyzed".into(), json!(false));
        entry.insert("licenseConcluded".into(), json!("NOASSERTION"));
        entry.insert(
            "licenseDeclared".into(),
            json!(license_expression(&package.licenses)),
        );
        entry.insert("copyrightText".into(), json!("NOASSERTION"));
        if !package.purl.is_empty() {
            entry.insert(
                "externalRefs".into(),
                json!([{
                    "referenceCategory": "PACKAGE-MANAGER",
                    "referenceType": "purl",
                    "referenceLocator": package.purl,
                }]),
            );
        }
        spdx_packages.push(Value::Object(entry));
        relationships.push(json!({
            "spdxElementId": "SPDXRef-DOCUMENT",
            "relationshipType": "DESCRIBES",
            "relatedSpdxElement": spdx_id,
        }));
    }
    json!({
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": image_ref,
        "documentNamespace": format!("https://coolify.io/spdx/{}", uuid_v4()),
        "creationInfo": {
            "created": zulu(now),
            "creators": ["Tool: Coolify"],
        },
        "packages": spdx_packages,
        "relationships": relationships,
    })
}

/// `licenseExpression()`: an SPDX expression only when every license is a plain identifier, else
/// NOASSERTION.
fn license_expression(licenses: &[String]) -> String {
    if licenses.is_empty() || licenses.iter().any(|l| !PLAIN_LICENSE.is_match(l)) {
        return "NOASSERTION".to_owned();
    }
    let mut unique: Vec<&str> = Vec::new();
    for license in licenses {
        if !unique.contains(&license.as_str()) {
            unique.push(license);
        }
    }
    unique.join(" AND ")
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 10)
            .unwrap()
            .and_hms_opt(8, 30, 5)
            .unwrap()
    }

    fn stored() -> Value {
        json!([
            {"id": "a1", "name": "openssl", "version": "3.0.1", "type": "apk", "licenses": ["Apache-2.0", "Apache-2.0", "MIT"], "purl": "pkg:apk/alpine/openssl@3.0.1", "layerId": "sha256:x"},
            {"id": "b2", "name": "custom", "version": "", "type": "binary", "licenses": ["See the LICENSE file"], "purl": "", "layerId": ""},
            {"id": "c3", "name": "bare", "version": "1", "type": "npm", "licenses": [], "purl": "", "layerId": ""},
            "junk"
        ])
    }

    #[test]
    fn cyclone_dx_lists_the_packages_without_empty_fields() {
        let doc = cyclone_dx("nginx:1.27", &stored(), now());
        assert_eq!(doc["bomFormat"], "CycloneDX");
        assert_eq!(doc["specVersion"], "1.6");
        assert_eq!(doc["version"], 1);
        assert_eq!(doc["metadata"]["timestamp"], "2026-11-10T08:30:05Z");
        assert_eq!(
            doc["metadata"]["component"],
            json!({"type": "container", "bom-ref": "image", "name": "nginx:1.27"})
        );
        let serial = doc["serialNumber"].as_str().unwrap();
        assert!(
            Regex::new(
                r"^urn:uuid:[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
            )
            .unwrap()
            .is_match(serial)
        );
        assert_eq!(
            doc["components"],
            json!([
                {"type": "library", "bom-ref": "package-a1", "name": "openssl", "version": "3.0.1", "purl": "pkg:apk/alpine/openssl@3.0.1",
                 "licenses": [{"license": {"name": "Apache-2.0"}}, {"license": {"name": "Apache-2.0"}}, {"license": {"name": "MIT"}}]},
                {"type": "library", "bom-ref": "package-b2", "name": "custom", "licenses": [{"license": {"name": "See the LICENSE file"}}]},
                {"type": "library", "bom-ref": "package-c3", "name": "bare", "version": "1"},
            ])
        );
        let keys: Vec<&str> = doc
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "bomFormat",
                "specVersion",
                "serialNumber",
                "version",
                "metadata",
                "components"
            ]
        );
    }

    #[test]
    fn spdx_describes_every_package() {
        let doc = spdx("nginx:1.27", &stored(), now());
        assert_eq!(doc["spdxVersion"], "SPDX-2.3");
        assert_eq!(doc["name"], "nginx:1.27");
        assert_eq!(
            doc["creationInfo"],
            json!({"created": "2026-11-10T08:30:05Z", "creators": ["Tool: Coolify"]})
        );
        assert!(
            doc["documentNamespace"]
                .as_str()
                .unwrap()
                .starts_with("https://coolify.io/spdx/")
        );
        assert_eq!(
            doc["packages"][0],
            json!({
                "name": "openssl", "SPDXID": "SPDXRef-Package-1", "versionInfo": "3.0.1", "downloadLocation": "NOASSERTION",
                "filesAnalyzed": false, "licenseConcluded": "NOASSERTION", "licenseDeclared": "Apache-2.0 AND MIT",
                "copyrightText": "NOASSERTION",
                "externalRefs": [{"referenceCategory": "PACKAGE-MANAGER", "referenceType": "purl", "referenceLocator": "pkg:apk/alpine/openssl@3.0.1"}],
            })
        );
        assert_eq!(doc["packages"][1]["licenseDeclared"], "NOASSERTION");
        assert!(doc["packages"][1].get("versionInfo").is_none());
        assert!(doc["packages"][1].get("externalRefs").is_none());
        assert_eq!(doc["packages"][2]["licenseDeclared"], "NOASSERTION");
        assert_eq!(doc["relationships"].as_array().unwrap().len(), 3);
        assert_eq!(
            doc["relationships"][2],
            json!({"spdxElementId": "SPDXRef-DOCUMENT", "relationshipType": "DESCRIBES", "relatedSpdxElement": "SPDXRef-Package-3"})
        );
    }
}
