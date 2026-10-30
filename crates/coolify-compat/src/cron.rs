//! `Poliander\Cron\CronExpression::isValid()` (poliander/cron 3.3.1, the
//! library behind upstream's `validate_cron_expression()`), ported rule for
//! rule: five whitespace-separated segments, month and weekday names only as a
//! whole segment, `*`, values, `a-b` ranges and `/n` steps on ranges, and at
//! least one calendar date in the next 27 years matching day, month and
//! weekday together.

use chrono::{Datelike, NaiveDate, Utc};

/// `[min, max, mod]` per segment (`VALUE_BOUNDARIES`).
const BOUNDS: [(i64, i64, i64); 5] = [(0, 59, 1), (0, 23, 1), (1, 31, 1), (1, 12, 1), (0, 7, 0)];

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

type Register = Vec<i64>;

/// PHP `(int)` on a string: leading whitespace and sign, then digits.
pub(crate) fn php_int(s: &str) -> i64 {
    let t = s.trim_start();
    let (neg, rest) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let n = digits
        .parse::<i64>()
        .unwrap_or(if digits.is_empty() { 0 } else { i64::MAX });
    if neg { -n } else { n }
}

/// PHP `is_numeric` for strings.
pub(crate) fn php_is_numeric(s: &str) -> bool {
    let t = s
        .trim_start_matches([' ', '\t', '\n', '\r', '\x0B', '\x0C'])
        .trim_end_matches([' ', '\t', '\n', '\r', '\x0B', '\x0C']);
    if t.is_empty() {
        return false;
    }
    let body = t.strip_prefix(['+', '-']).unwrap_or(t);
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    let mantissa_ok = {
        let mut parts = mantissa.splitn(2, '.');
        let int = parts.next().unwrap_or("");
        let frac = parts.next();
        let digits = |p: &str| p.chars().all(|c| c.is_ascii_digit());
        digits(int)
            && frac.is_none_or(digits)
            && !(int.is_empty() && frac.is_none_or(str::is_empty))
    };
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        !e.is_empty() && e.chars().all(|c| c.is_ascii_digit())
    });
    mantissa_ok && exponent_ok
}

fn validate_value(value: &str, index: usize, step: i64) -> Result<(), ()> {
    if value.is_empty() || !value.chars().all(|c| c.is_ascii_digit()) {
        return Err(());
    }
    let v = value.parse::<i64>().map_err(|_| ())?;
    let (min, max, _) = BOUNDS[index];
    if v < min || v > max || step != 1 {
        return Err(());
    }
    Ok(())
}

fn set(register: &mut Register, v: i64) {
    if !register.contains(&v) {
        register.push(v);
    }
}

fn parse_range(register: &mut Register, index: usize, range: &str, step: i64) -> Result<(), ()> {
    let (min, max, modulo) = BOUNDS[index];
    let parts: Vec<String> = if range == "*" {
        vec![min.to_string(), max.to_string()]
    } else {
        range.split('-').map(str::to_string).collect()
    };
    if parts.len() != 2 {
        return Err(());
    }
    for p in &parts {
        validate_value(p, index, 1)?;
    }
    let (lo, hi) = (
        parts[0].parse::<i64>().map_err(|_| ())?,
        parts[1].parse::<i64>().map_err(|_| ())?,
    );
    if lo > hi {
        return Err(());
    }
    let boundary = max + modulo;
    let mut i = 0;
    while i <= hi - lo {
        set(register, (lo + i) % boundary);
        i += step;
    }
    Ok(())
}

fn parse_element(register: &mut Register, index: usize, element: &str) -> Result<(), ()> {
    let segments: Vec<&str> = element.split('/').collect();
    let mut step = 1;
    let mut element = element;
    if segments.len() > 1 {
        if segments.len() != 2 {
            return Err(());
        }
        let s = php_int(segments[1]);
        if s < 1 || s > BOUNDS[index].1 {
            return Err(());
        }
        element = segments[0];
        step = s;
    }
    if php_is_numeric(element) {
        validate_value(element, index, step)?;
        set(register, element.parse::<i64>().map_err(|_| ())?);
        Ok(())
    } else {
        parse_range(register, index, element, step)
    }
}

fn parse_segment(register: &mut Register, index: usize, segment: &str) -> Result<(), ()> {
    let lower = segment.to_lowercase();
    let named = match index {
        3 => MONTHS
            .iter()
            .position(|m| *m == lower)
            .map(|p| p as i64 + 1),
        4 => WEEKDAYS.iter().position(|d| *d == lower).map(|p| p as i64),
        _ => None,
    };
    if let Some(v) = named {
        set(register, v);
        return Ok(());
    }
    for element in segment.split(',') {
        parse_element(register, index, element)?;
    }
    Ok(())
}

fn some_date_matches(registers: &[Register; 5], year: i32) -> bool {
    for y in 0..27 {
        for month in &registers[3] {
            for day in &registers[2] {
                let Some(date) = u32::try_from(*month)
                    .ok()
                    .zip(u32::try_from(*day).ok())
                    .and_then(|(m, d)| NaiveDate::from_ymd_opt(year + y, m, d))
                else {
                    continue;
                };
                let weekday = i64::from(date.weekday().num_days_from_sunday());
                if registers[4].contains(&weekday) {
                    return true;
                }
            }
        }
    }
    false
}

/// `(new CronExpression($expression))->isValid()`.
pub fn is_valid(expression: &str) -> bool {
    is_valid_in_year(expression, Utc::now().year())
}

fn is_valid_in_year(expression: &str, year: i32) -> bool {
    // preg_split('/\s+/', trim($expression)): PHP's trim and \s sets.
    let trimmed = expression.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0B']);
    let segments: Vec<&str> = trimmed
        .split([' ', '\t', '\n', '\r', '\x0B', '\x0C'])
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() != 5 {
        return false;
    }
    let mut registers: [Register; 5] = Default::default();
    for (i, segment) in segments.iter().enumerate() {
        if parse_segment(&mut registers[i], i, segment).is_err() {
            return false;
        }
    }
    if registers[4].contains(&7) {
        set(&mut registers[4], 0);
    }
    some_date_matches(&registers, year)
}

/// `VALID_CRON_STRINGS` (`bootstrap/helpers/constants.php`).
pub const VALID_CRON_STRINGS: [(&str, &str); 11] = [
    ("every_minute", "* * * * *"),
    ("hourly", "0 * * * *"),
    ("daily", "0 0 * * *"),
    ("weekly", "0 0 * * 0"),
    ("monthly", "0 0 1 * *"),
    ("yearly", "0 0 1 1 *"),
    ("@hourly", "0 * * * *"),
    ("@daily", "0 0 * * *"),
    ("@weekly", "0 0 * * 0"),
    ("@monthly", "0 0 1 * *"),
    ("@yearly", "0 0 1 1 *"),
];

/// Upstream `validate_cron_expression()`.
pub fn validate_cron_expression(expression: &str) -> bool {
    if expression.is_empty() || expression == "0" {
        return false;
    }
    is_valid(expression) || VALID_CRON_STRINGS.iter().any(|(k, _)| *k == expression)
}

/// Upstream `translate_cron_expression()`.
pub fn translate_cron_expression(expression: &str) -> &str {
    VALID_CRON_STRINGS
        .iter()
        .find(|(k, _)| *k == expression)
        .map(|(_, v)| *v)
        .unwrap_or(expression)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_poliander() {
        for ok in [
            "* * * * *",
            "0 23 * * *",
            "*/5 1-3 1,15 jan mon",
            "0 0 * * 7",
            "0 0 29 2 *",
            " 1  2 3 4 5 ",
            "0-59/2 * * * *",
        ] {
            assert!(is_valid_in_year(ok, 2026), "{ok}");
        }
        for bad in [
            "garbage",
            "61 * * * *",
            "* * *",
            "5/2 * * * *",
            "* * * * mon-fri",
            "0 0 31 2 *",
            "0 0 30 2 *",
            "1,,2 * * * *",
            "* * * * * *",
            "*/0 * * * *",
            "3-1 * * * *",
        ] {
            assert!(!is_valid_in_year(bad, 2026), "{bad}");
        }
        assert!(validate_cron_expression("@daily"));
        assert!(!validate_cron_expression(""));
        assert_eq!(translate_cron_expression("weekly"), "0 0 * * 0");
    }
}
