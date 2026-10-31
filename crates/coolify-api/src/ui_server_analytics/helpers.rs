//! The traffic helpers of upstream `bootstrap/helpers/shared.php` and `BuildsTrafficChartPayload`.

use coolify_jobs::compose_parser::php::parse_url;
use serde::Serialize;

use super::aggregator::{BreakdownRow, entry};
use super::countries::country_name;

fn is_country_code(code: &str) -> bool {
    code.len() == 2 && code.bytes().all(|b| b.is_ascii_uppercase())
}

/// `refererHost`.
pub fn referer_host(referer: &str) -> Option<String> {
    let referer = referer.trim();
    if referer.is_empty() {
        return None;
    }
    let with_scheme = if referer.contains("://") {
        referer.to_string()
    } else {
        format!("http://{referer}")
    };
    let host = parse_url(&with_scheme)?
        .host
        .filter(|h| !h.is_empty() && h != "0")?
        .to_lowercase();
    Some(
        host.strip_prefix("www.")
            .map(str::to_string)
            .unwrap_or(host),
    )
}

/// `deviceLabel`.
pub fn device_label(device: &str) -> String {
    let value = device.trim().to_lowercase();
    match value.as_str() {
        "" => "Unknown".to_string(),
        "pc" => "Desktop".to_string(),
        "smartphone" | "mobilephone" => "Mobile".to_string(),
        "appliance" => "Appliance".to_string(),
        "crawler" => "Bot".to_string(),
        _ => value
            .split_inclusive(|c: char| !c.is_alphanumeric())
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().chain(chars).collect(),
                    None => String::new(),
                }
            })
            .collect(),
    }
}

/// `isContainerToolingUserAgent`.
pub fn is_container_tooling_user_agent(user_agent: &str) -> bool {
    let lower = user_agent.trim().to_ascii_lowercase();
    [
        "docker/",
        "docker-client/",
        "buildkit/",
        "containerd/",
        "podman/",
        "skopeo/",
        "cri-o/",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

/// `refererFaviconUrl`.
pub fn referer_favicon_url(host: &str) -> String {
    let mut encoded = String::new();
    for b in host.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(b));
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("https://icons.duckduckgo.com/ip3/{encoded}.ico")
}

/// One row of `_breakdown-section.blade.php`, with what the view computes for it.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BreakdownOut {
    pub value: String,
    pub display: String,
    pub host: Option<String>,
    pub favicon: Option<String>,
    pub hidable: bool,
    pub requests: i64,
    pub bytes_out: i64,
}

/// The `@php` block of `_breakdown-section.blade.php`.
pub fn breakdown_out(dimension: &str, row: &BreakdownRow) -> BreakdownOut {
    let value = row.value.as_str();
    let other = value == "__other__";
    let host = if !other && dimension == "referer" {
        referer_host(value)
    } else {
        None
    };
    let plain = || {
        if value.is_empty() {
            "Unknown".to_string()
        } else {
            value.to_string()
        }
    };
    let display = if other {
        "Other".to_string()
    } else {
        match dimension {
            "device" => device_label(value),
            "referer" => host.clone().unwrap_or_else(|| "Direct / none".to_string()),
            "status" if value == "0" => "No response status (0)".to_string(),
            _ => plain(),
        }
    };
    BreakdownOut {
        value: value.to_string(),
        display,
        favicon: host.as_deref().map(referer_favicon_url),
        host,
        hidable: dimension == "useragent" && is_container_tooling_user_agent(value),
        requests: row.requests,
        bytes_out: row.bytes_out,
    }
}

/// `trafficCityRows`.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CityRow {
    pub key: String,
    pub country: String,
    pub label: String,
    pub name: String,
    pub detail: String,
    pub requests: i64,
    pub bytes_out: i64,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

pub fn traffic_city_rows(rows: &[BreakdownRow]) -> Vec<CityRow> {
    let mut out: Vec<(String, CityRow)> = Vec::new();
    let mut has_city = false;
    for row in rows {
        if row.requests <= 0 {
            continue;
        }
        let parts: Vec<&str> = row.value.splitn(3, '|').collect();
        let code = parts[0].to_uppercase();
        let mut city = CityRow {
            key: "__other__".to_string(),
            country: String::new(),
            label: "Other".to_string(),
            name: "Other".to_string(),
            detail: String::new(),
            requests: 0,
            bytes_out: 0,
            lat: None,
            lon: None,
        };
        if parts.len() == 3 && is_country_code(&code) {
            let (subdivision, name) = (parts[1].trim(), parts[2].trim());
            city.country = code.clone();
            city.key = row.value.clone();
            if name.is_empty() {
                city.label = format!("Unknown, {code}");
                city.name = "Unknown".to_string();
                city.detail = country_name(&code).to_string();
            } else {
                has_city = true;
                let region = if (2..=3).contains(&subdivision.len())
                    && subdivision.bytes().all(|b| b.is_ascii_uppercase())
                {
                    format!("{subdivision}, ")
                } else {
                    String::new()
                };
                city.label = format!("{name}, {region}{code}");
                city.name = name.to_string();
                city.detail = format!("{region}{}", country_name(&code));
                city.lat = row.lat;
                city.lon = row.lon;
            }
        }
        let key = city.key.clone();
        let merged = entry(&mut out, &key, || city);
        merged.requests += row.requests;
        merged.bytes_out += row.bytes_out;
    }
    if !has_city {
        return Vec::new();
    }
    let other = out
        .iter()
        .position(|(k, _)| k == "__other__")
        .map(|i| out.remove(i).1);
    let mut cities: Vec<CityRow> = out.into_iter().map(|(_, row)| row).collect();
    cities.sort_by_key(|row| std::cmp::Reverse(row.requests));
    cities.extend(other);
    cities
}

/// The country rows of `_geo.blade.php`: known codes, with the rest as one Unknown row.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CountryRow {
    pub value: String,
    pub name: String,
    pub requests: i64,
    pub bytes_out: i64,
}

pub fn country_rows(rows: &[BreakdownRow]) -> (Vec<CountryRow>, Vec<BreakdownRow>) {
    let mut known = Vec::new();
    let mut known_rows = Vec::new();
    let mut unknown: Option<CountryRow> = None;
    for row in rows {
        if row.requests <= 0 {
            continue;
        }
        let value = row.value.to_uppercase();
        if is_country_code(&value) && country_name(&value) != "Unknown" {
            known.push(CountryRow {
                name: country_name(&value).to_string(),
                value: value.clone(),
                requests: row.requests,
                bytes_out: row.bytes_out,
            });
            known_rows.push(BreakdownRow {
                value,
                ..row.clone()
            });
        } else {
            let u = unknown.get_or_insert(CountryRow {
                value: String::new(),
                name: "Unknown".to_string(),
                requests: 0,
                bytes_out: 0,
            });
            u.requests += row.requests;
            u.bytes_out += row.bytes_out;
        }
    }
    known.extend(unknown);
    known.sort_by_key(|row| std::cmp::Reverse(row.requests));
    (known, known_rows)
}

/// `trafficGeoMarkers`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CountryMarker {
    pub code: String,
    pub requests: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CityMarker {
    pub lat: f64,
    pub lon: f64,
    pub requests: i64,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct GeoMarkers {
    pub countries: Vec<CountryMarker>,
    pub cities: Vec<CityMarker>,
}

pub fn traffic_geo_markers(countries: &[BreakdownRow], cities: &[CityRow]) -> GeoMarkers {
    GeoMarkers {
        countries: countries
            .iter()
            .filter_map(|row| {
                let code = row.value.to_uppercase();
                (row.requests > 0 && is_country_code(&code)).then_some(CountryMarker {
                    code,
                    requests: row.requests,
                })
            })
            .collect(),
        cities: cities
            .iter()
            .filter_map(|row| match (row.requests > 0, row.lat, row.lon) {
                (true, Some(lat), Some(lon)) => Some(CityMarker {
                    lat,
                    lon,
                    requests: row.requests,
                }),
                _ => None,
            })
            .collect(),
    }
}

/// `deviceChartData`.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct DeviceChart {
    pub labels: Vec<String>,
    pub series: Vec<i64>,
}

pub fn device_chart_data(rows: &[BreakdownRow]) -> DeviceChart {
    let mut totals: Vec<(String, i64)> = Vec::new();
    for row in rows {
        let label = if row.value == "__other__" {
            "Other".to_string()
        } else {
            device_label(&row.value)
        };
        *entry(&mut totals, &label, || 0) += row.requests;
    }
    totals.sort_by_key(|(_, total)| std::cmp::Reverse(*total));
    let (labels, series) = totals.into_iter().unzip();
    DeviceChart { labels, series }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(value: &str, requests: i64) -> BreakdownRow {
        BreakdownRow {
            value: value.to_string(),
            requests,
            bytes_out: 1,
            lat: None,
            lon: None,
        }
    }

    #[test]
    fn referer_hosts() {
        assert_eq!(
            referer_host("https://WWW.Example.com/a"),
            Some("example.com".to_string())
        );
        assert_eq!(referer_host("example.com"), Some("example.com".to_string()));
        assert_eq!(referer_host("  "), None);
    }

    #[test]
    fn device_labels() {
        assert_eq!(device_label("pc"), "Desktop");
        assert_eq!(device_label("SmartPhone"), "Mobile");
        assert_eq!(device_label("crawler"), "Bot");
        assert_eq!(device_label(""), "Unknown");
        assert_eq!(device_label("misc"), "Misc");
    }

    #[test]
    fn tooling_agents() {
        assert!(is_container_tooling_user_agent("Docker/24.0"));
        assert!(is_container_tooling_user_agent(" buildkit/0.1"));
        assert!(!is_container_tooling_user_agent("Mozilla/5.0 docker/1"));
    }

    #[test]
    fn breakdown_display() {
        assert_eq!(
            breakdown_out("status", &row("0", 1)).display,
            "No response status (0)"
        );
        assert_eq!(breakdown_out("status", &row("", 1)).display, "Unknown");
        assert_eq!(
            breakdown_out("referer", &row("", 1)).display,
            "Direct / none"
        );
        assert_eq!(breakdown_out("os", &row("__other__", 1)).display, "Other");
        let referer = breakdown_out("referer", &row("a.com", 1));
        assert_eq!(
            referer.favicon.as_deref(),
            Some("https://icons.duckduckgo.com/ip3/a.com.ico")
        );
        assert!(breakdown_out("useragent", &row("Docker/1", 1)).hidable);
    }

    #[test]
    fn city_rows() {
        let mut berlin = row("de|BE|Berlin", 3);
        berlin.lat = Some(52.5);
        berlin.lon = Some(13.4);
        let rows = traffic_city_rows(&[row("__other__", 9), row("DE||", 1), berlin]);
        assert_eq!(rows[0].name, "Berlin");
        assert_eq!(rows[0].label, "Berlin, BE, DE");
        assert_eq!(rows[0].detail, "BE, Germany");
        assert_eq!(rows[0].country, "DE");
        assert_eq!(rows[1].label, "Unknown, DE");
        assert_eq!(rows[2].name, "Other");
        assert!(traffic_city_rows(&[row("DE||", 1)]).is_empty());
    }

    #[test]
    fn country_rows_fold_unknown_codes() {
        let (rows, known) = country_rows(&[row("de", 1), row("", 2), row("ZZ", 2), row("US", 0)]);
        assert_eq!(rows[0].name, "Unknown");
        assert_eq!(rows[0].requests, 4);
        assert_eq!(rows[1].value, "DE");
        assert_eq!(known.len(), 1);
    }

    #[test]
    fn geo_markers_and_device_chart() {
        let markers = traffic_geo_markers(&[row("de", 1), row("", 1)], &[]);
        assert_eq!(markers.countries.len(), 1);
        assert_eq!(markers.countries[0].code, "DE");
        let chart = device_chart_data(&[
            row("pc", 1),
            row("smartphone", 2),
            row("mobilephone", 2),
            row("__other__", 1),
        ]);
        assert_eq!(chart.labels, vec!["Mobile", "Desktop", "Other"]);
        assert_eq!(chart.series, vec![4, 1, 1]);
    }
}
