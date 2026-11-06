//! Unix seconds and UTC calendar dates, without a date library.

use serde_json::Value;

pub const MINUTE: i64 = 60;
pub const HOUR: i64 = 3_600;
pub const DAY: i64 = 86_400;

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day number from [`days_from_civil`].
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (
        if m <= 2 {
            yoe + era * 400 + 1
        } else {
            yoe + era * 400
        },
        m,
        d,
    )
}

/// `2026-11-06T12:00:00Z`.
pub fn rfc3339(ts: i64) -> String {
    let (y, m, d) = civil_from_days(ts.div_euclid(DAY));
    let s = ts.rem_euclid(DAY);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / HOUR,
        s % HOUR / MINUTE,
        s % MINUTE
    )
}

/// The UTC month of a time, `2026-11`: the storage shard it belongs to.
pub fn month(ts: i64) -> String {
    let (y, m, _) = civil_from_days(ts.div_euclid(DAY));
    format!("{y:04}-{m:02}")
}

/// A time from JSON: Unix seconds (or milliseconds, when it is too large to be seconds), or an RFC 3339
/// string such as `2026-11-06T12:00:00Z`, `2026-11-06T13:00:00.5+01:00` or `2026-11-06 12:00:00`.
pub fn parse(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => {
            let n = n.as_i64().or_else(|| n.as_f64().map(|f| f as i64))?;
            Some(if n.abs() >= 100_000_000_000 {
                n / 1000
            } else {
                n
            })
        }
        Value::String(s) => parse_rfc3339(s.trim()),
        _ => None,
    }
}

fn digits(s: &[u8], from: usize, len: usize) -> Option<i64> {
    let part = s.get(from..from + len)?;
    if !part.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(part).ok()?.parse().ok()
}

fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    if !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let (y, mo, d) = (digits(b, 0, 4)?, digits(b, 5, 2)?, digits(b, 8, 2)?);
    let (h, mi, se) = (digits(b, 11, 2)?, digits(b, 14, 2)?, digits(b, 17, 2)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut i = 19;
    if b.get(i) == Some(&b'.') {
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    let offset = match b.get(i) {
        None | Some(b'Z' | b'z') if b.len() <= i + 1 => 0,
        Some(sign @ (b'+' | b'-')) if b.len() == i + 6 && b[i + 3] == b':' => {
            let o = digits(b, i + 1, 2)? * HOUR + digits(b, i + 4, 2)? * MINUTE;
            if *sign == b'+' { o } else { -o }
        }
        _ => return None,
    };
    let days = days_from_civil(y, mo as u32, d as u32);
    Some(days * DAY + h * HOUR + mi * MINUTE + se - offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dates_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        for z in [-800_000, -1, 0, 59, 60, 11_016, 20_763, 2_000_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(rfc3339(1_793_966_400), "2026-11-06T12:00:00Z");
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(month(1_793_966_400), "2026-11");
        assert_eq!(
            month(days_from_civil(2024, 2, 29) * DAY + DAY - 1),
            "2024-02"
        );
    }

    #[test]
    fn parses_times() {
        let t = 1_793_966_400;
        assert_eq!(parse(&json!(t)), Some(t));
        assert_eq!(parse(&json!(t * 1000)), Some(t));
        assert_eq!(parse(&json!("2026-11-06T12:00:00Z")), Some(t));
        assert_eq!(parse(&json!("2026-11-06 12:00:00")), Some(t));
        assert_eq!(parse(&json!("2026-11-06T13:30:00.250+01:30")), Some(t));
        assert_eq!(parse(&json!("2026-11-06T10:00:00-02:00")), Some(t));
        for bad in [
            json!("yesterday"),
            json!("2026-13-06T12:00:00Z"),
            json!("2026-11-06T12:00:00Zjunk"),
            json!("2026-11-06T12:00"),
            json!(null),
            json!(true),
            json!("２０２６-11-06T12:00:00Z"),
        ] {
            assert_eq!(parse(&bad), None, "{bad}");
        }
    }
}
