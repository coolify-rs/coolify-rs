//! Every result of the reference's dragonmantank/cron-expression in
//! `fixtures/cron_expression.json` (see `fixtures/cron_expression.php`), around the daylight saving
//! changes of several time zones.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use coolify_compat::cron_expression::{CronExpression, in_zone, time_zone};

fn format(r: Result<DateTime<Tz>, coolify_compat::cron_expression::CronError>) -> String {
    match r {
        Ok(d) => d.format("%Y-%m-%dT%H:%M:%S%:z").to_string(),
        Err(_) => "error".to_owned(),
    }
}

#[test]
fn matches_the_reference_library() {
    let raw = include_str!("fixtures/cron_expression.json");
    let rows: Vec<serde_json::Value> = serde_json::from_str(raw).expect("fixture");
    let mut failures = Vec::new();
    for row in &rows {
        let s = |i: usize| row[i].as_str().unwrap_or_default();
        let (expression, zone, instant) = (s(0), s(1), s(2));
        let cron = CronExpression::new(expression).expect(expression);
        let tz = time_zone(zone).expect(zone);
        let at: DateTime<Utc> = instant.parse().expect(instant);
        let time = in_zone(at, tz);
        let got = [
            format(cron.previous_run_date(&time, true)),
            format(cron.previous_run_date(&time, false)),
            format(cron.next_run_date(&time, true)),
            format(cron.next_run_date(&time, false)),
        ];
        let due = i64::from(cron.is_due(&time));
        for (i, value) in got.iter().enumerate() {
            if value != s(i + 4) {
                failures.push(format!(
                    "{expression} {zone} {instant} [{i}]: {value} != {}",
                    s(i + 4)
                ));
            }
        }
        if Some(due) != row[3].as_i64() {
            failures.push(format!("{expression} {zone} {instant} isDue: {due}"));
        }
    }
    assert!(rows.len() > 5000, "{} rows", rows.len());
    assert!(
        failures.is_empty(),
        "{} of {} rows differ:\n{}",
        failures.len(),
        rows.len(),
        failures[..failures.len().min(40)].join("\n")
    );
}
