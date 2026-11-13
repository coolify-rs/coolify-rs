//! `App\Services\ImageScan\Finding` and `Package`: one vulnerability match of a Grype report and
//! one package of an SBOM, as they are stored in `image_scans.findings` and
//! `image_sboms.packages`.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value, json};

use super::text;

static CVE_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^CVE-\d{4}-\d{4,}$").unwrap_or_else(|_| unreachable!()));

/// PHP `(string) $value` of a decoded JSON scalar.
fn php_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string(),
            (_, Some(u), _) => u.to_string(),
            (_, _, Some(f)) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{f:.0}"),
            (_, _, Some(f)) => f.to_string(),
            _ => String::new(),
        },
        Some(Value::Bool(true)) => "1".to_owned(),
        _ => String::new(),
    }
}

/// `array_values(array_map('strval', (array) $value))`.
fn php_string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items.iter().map(|v| php_string(Some(v))).collect(),
        Some(Value::Object(map)) => map.values().map(|v| php_string(Some(v))).collect(),
        None | Some(Value::Null) => Vec::new(),
        Some(scalar) => vec![php_string(Some(scalar))],
    }
}

/// `(float) $value` of a set value, `None` for null and missing.
fn php_float(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => Some(s.trim().parse().unwrap_or(0.0)),
        Some(Value::Bool(b)) => Some(f64::from(u8::from(*b))),
        _ => None,
    }
}

/// `is_numeric($value) ? (float) $value : null`.
pub(crate) fn numeric(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) if coolify_compat::yaml::php_is_numeric(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// A JSON number for a float, as PHP's `json_encode` writes it; null for a missing one.
fn float_value(f: Option<f64>) -> Value {
    f.and_then(serde_json::Number::from_f64)
        .map_or(Value::Null, Value::Number)
}

/// One vulnerability match.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub id: String,
    pub aliases: Vec<String>,
    /// critical, high, medium, low or unknown.
    pub severity: String,
    pub package: String,
    pub version: String,
    /// The package type (`deb`, `apk`, `rpm`, `npm`, …).
    pub r#type: String,
    pub artifact_id: String,
    pub fixed_in: Option<String>,
    /// `fixed`, `not-fixed`, `wont-fix` or `unknown`.
    pub fix_state: String,
    /// In CISA KEV.
    pub kev: bool,
    pub epss: Option<f64>,
    pub risk: Option<f64>,
    pub summary: String,
    /// New findings are always "app"; kept because stored findings carry it.
    pub layer: String,
    pub cvss_score: Option<f64>,
    pub title: String,
    pub display_id: String,
    pub namespace: String,
    pub match_type: String,
}

impl Finding {
    /// `toArray()`, in upstream's key order.
    pub fn to_value(&self) -> Value {
        json!({
            "id": self.id,
            "aliases": self.aliases,
            "severity": self.severity,
            "package": self.package,
            "version": self.version,
            "type": self.r#type,
            "artifactId": self.artifact_id,
            "fixedIn": self.fixed_in,
            "fixState": self.fix_state,
            "kev": self.kev,
            "epss": float_value(self.epss),
            "risk": float_value(self.risk),
            "summary": self.summary,
            "layer": self.layer,
            "cvssScore": float_value(self.cvss_score),
            "title": self.title,
            "displayId": self.display_id,
            "namespace": self.namespace,
            "matchType": self.match_type,
        })
    }

    /// `Finding::fromArray($data)`: a stored finding. The title is cleaned for display and a
    /// missing display ID is derived.
    pub fn from_value(data: &Value) -> Self {
        let empty = Map::new();
        let data = data.as_object().unwrap_or(&empty);
        let get = |key: &str| data.get(key).filter(|v| !v.is_null());
        let id = php_string(get("id"));
        let aliases = php_string_list(get("aliases"));
        let severity =
            get("severity").map_or_else(|| "unknown".to_owned(), |v| php_string(Some(v)));
        let package = php_string(get("package"));
        let version = php_string(get("version"));
        let title = text::stored_title(&php_string(get("title")), &package, &version, &severity);
        let display_id = php_string(get("displayId"));
        let display_id = if display_id.is_empty() {
            Self::display_id_for(&id, &aliases)
        } else {
            display_id
        };
        Self {
            r#type: php_string(get("type")),
            artifact_id: php_string(get("artifactId")),
            fixed_in: get("fixedIn").map(|v| php_string(Some(v))),
            fix_state: php_string(get("fixState")),
            kev: match get("kev") {
                Some(Value::Bool(b)) => *b,
                Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
                Some(Value::String(s)) => !s.is_empty() && s != "0",
                Some(Value::Array(a)) => !a.is_empty(),
                Some(Value::Object(_)) => true,
                _ => false,
            },
            epss: php_float(get("epss")),
            risk: php_float(get("risk")),
            summary: php_string(get("summary")),
            layer: get("layer").map_or_else(|| "app".to_owned(), |v| php_string(Some(v))),
            cvss_score: numeric(get("cvssScore")),
            namespace: php_string(get("namespace")),
            match_type: php_string(get("matchType")),
            id,
            aliases,
            severity,
            package,
            version,
            title,
            display_id,
        }
    }

    /// The findings of a stored list (`image_scans.findings`); rows that are not objects are
    /// read as empty findings, like `Finding::fromArray((array) $row)`.
    pub fn list_from_value(findings: Option<&Value>) -> Vec<Self> {
        match findings {
            Some(Value::Array(rows)) => rows.iter().map(Self::from_value).collect(),
            Some(Value::Object(rows)) => rows.values().map(Self::from_value).collect(),
            _ => Vec::new(),
        }
    }

    /// `Finding::displayIdFor()`: a CVE ID from the finding ID or its aliases, else the finding ID.
    pub fn display_id_for(id: &str, aliases: &[String]) -> String {
        std::iter::once(id)
            .chain(aliases.iter().map(String::as_str))
            .find(|candidate| CVE_ID.is_match(candidate))
            .unwrap_or(id)
            .to_owned()
    }

    /// The ID and the aliases (`[$finding->id, ...$finding->aliases]`).
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.id.as_str()).chain(self.aliases.iter().map(String::as_str))
    }
}

/// One package of an SBOM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub id: String,
    pub name: String,
    pub version: String,
    pub r#type: String,
    pub licenses: Vec<String>,
    pub purl: String,
    pub layer_id: String,
}

impl Package {
    /// `toArray()`, in upstream's key order.
    pub fn to_value(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "version": self.version,
            "type": self.r#type,
            "licenses": self.licenses,
            "purl": self.purl,
            "layerId": self.layer_id,
        })
    }

    /// `Package::fromArray($data)`.
    pub fn from_value(data: &Value) -> Self {
        let empty = Map::new();
        let data = data.as_object().unwrap_or(&empty);
        let get = |key: &str| data.get(key).filter(|v| !v.is_null());
        Self {
            id: php_string(get("id")),
            name: php_string(get("name")),
            version: php_string(get("version")),
            r#type: php_string(get("type")),
            licenses: php_string_list(get("licenses")),
            purl: php_string(get("purl")),
            layer_id: php_string(get("layerId")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding() -> Finding {
        Finding {
            id: "GHSA-aaaa-bbbb-cccc".into(),
            aliases: vec!["CVE-2024-12345".into()],
            severity: "high".into(),
            package: "libfoo".into(),
            version: "1.0".into(),
            r#type: "deb".into(),
            artifact_id: "abc".into(),
            fixed_in: Some("1.1".into()),
            fix_state: "fixed".into(),
            kev: true,
            epss: Some(0.25),
            risk: Some(12.5),
            summary: "A flaw.".into(),
            layer: "app".into(),
            cvss_score: Some(7.5),
            title: "A flaw in the parser".into(),
            display_id: "CVE-2024-12345".into(),
            namespace: "debian:distro:debian:12".into(),
            match_type: "exact-direct-match".into(),
        }
    }

    #[test]
    fn to_value_keeps_upstreams_key_order() {
        let value = finding().to_value();
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "id",
                "aliases",
                "severity",
                "package",
                "version",
                "type",
                "artifactId",
                "fixedIn",
                "fixState",
                "kev",
                "epss",
                "risk",
                "summary",
                "layer",
                "cvssScore",
                "title",
                "displayId",
                "namespace",
                "matchType"
            ]
        );
    }

    #[test]
    fn round_trips_through_the_stored_form() {
        let f = finding();
        assert_eq!(Finding::from_value(&f.to_value()), f);
    }

    #[test]
    fn from_value_fills_defaults() {
        let f =
            Finding::from_value(&json!({"id": "CVE-2024-0001", "package": "p", "version": "2"}));
        assert_eq!(f.severity, "unknown");
        assert_eq!(f.layer, "app");
        assert_eq!(f.display_id, "CVE-2024-0001");
        assert_eq!(f.title, "p 2: unknown vulnerability");
        assert_eq!(f.fixed_in, None);
        assert_eq!(f.epss, None);
        assert_eq!(f.cvss_score, None);
        assert!(!f.kev);

        let empty = Finding::from_value(&json!("junk"));
        assert_eq!(empty.id, "");
        assert_eq!(empty.title, "unknown vulnerability");
    }

    #[test]
    fn from_value_derives_the_display_id_from_an_alias() {
        let f = Finding::from_value(
            &json!({"id": "GHSA-x", "aliases": ["cve-2023-4444"], "cvssScore": "7.1"}),
        );
        assert_eq!(f.display_id, "cve-2023-4444");
        assert_eq!(f.cvss_score, Some(7.1));
        assert_eq!(
            Finding::display_id_for("GHSA-x", &["CVE-1-2".into()]),
            "GHSA-x"
        );
    }

    #[test]
    fn package_round_trips() {
        let p = Package {
            id: "1".into(),
            name: "openssl".into(),
            version: "3.0".into(),
            r#type: "apk".into(),
            licenses: vec!["Apache-2.0".into()],
            purl: "pkg:apk/alpine/openssl@3.0".into(),
            layer_id: "sha256:abc".into(),
        };
        assert_eq!(Package::from_value(&p.to_value()), p);
        assert_eq!(Package::from_value(&json!({})).name, "");
    }
}
