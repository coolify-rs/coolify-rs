//! `Cron\CronExpression` (dragonmantank/cron-expression, the library behind `Cron::isDue` and
//! `getPreviousRunDate()` in upstream's scheduler), ported field by field with its daylight saving
//! behaviour. Validation of what a user may store is the other library, see [`crate::cron`].
//!
//! One deliberate difference: a negative step (`*/-5`) passes upstream's `filter_var` check and
//! then loops forever in `getRangeForExpression()`; here it is an invalid expression. The API
//! never stores one, because `validate_cron_expression()` refuses it.

use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, Offset};
use chrono::{TimeZone, Timelike, Utc};
use chrono_tz::{OffsetComponents, Tz};

use crate::cron::{php_int, php_is_numeric};

/// `CronExpression::MAPPINGS`.
const MAPPINGS: [(&str, &str); 7] = [
    ("@yearly", "0 0 1 1 *"),
    ("@annually", "0 0 1 1 *"),
    ("@monthly", "0 0 1 * *"),
    ("@weekly", "0 0 * * 0"),
    ("@daily", "0 0 * * *"),
    ("@midnight", "0 0 * * *"),
    ("@hourly", "0 * * * *"),
];

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];
const WEEKDAYS: [&str; 7] = ["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"];

/// What upstream throws: `InvalidArgumentException`, `OutOfRangeException`, `RuntimeException`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CronError {
    #[error("{0} is not a valid CRON expression")]
    Invalid(String),
    #[error("Invalid CRON field value {0} at position {1}")]
    InvalidField(String, usize),
    #[error("{0}")]
    Field(String),
    #[error("Impossible CRON expression")]
    Impossible,
}

type Date = DateTime<Tz>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Minutes,
    Hours,
    DayOfMonth,
    Month,
    DayOfWeek,
}

const FIELDS: [Field; 5] = [
    Field::Minutes,
    Field::Hours,
    Field::DayOfMonth,
    Field::Month,
    Field::DayOfWeek,
];

impl Field {
    fn range(self) -> (i64, i64) {
        match self {
            Field::Minutes => (0, 59),
            Field::Hours => (0, 23),
            Field::DayOfMonth => (1, 31),
            Field::Month => (1, 12),
            Field::DayOfWeek => (0, 7),
        }
    }

    fn literals(self) -> &'static [&'static str] {
        match self {
            Field::Month => &MONTHS,
            Field::DayOfWeek => &WEEKDAYS,
            _ => &[],
        }
    }

    /// `AbstractField::convertLiterals()`: the keys of `$literals` start at 1.
    fn convert_literals(self, value: &str) -> String {
        let upper = value.to_ascii_uppercase();
        match self.literals().iter().position(|l| *l == upper) {
            Some(p) => (p + 1).to_string(),
            None => value.to_owned(),
        }
    }

    /// `$this->fullRange[$step % count($this->fullRange)]`.
    fn full_range_at(self, step: i64) -> i64 {
        let (start, end) = self.range();
        start + step.rem_euclid(end - start + 1)
    }

    /// `FILTER_VALIDATE_INT` that is truthy and, see the module comment, positive.
    fn valid_step(step: &str) -> bool {
        let digits = step.strip_prefix('+').unwrap_or(step);
        !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) && !digits.starts_with('0')
    }

    /// `AbstractField::validate()`.
    fn validate_base(self, value: &str) -> bool {
        let value = self.convert_literals(value);
        if value == "*" {
            return true;
        }
        if value.contains(',') {
            return value.split(',').all(|item| self.validate(item));
        }
        if value.contains('/') {
            let mut chunks = value.split('/');
            let range = chunks.next().unwrap_or_default();
            let step = chunks.next().unwrap_or_default();
            if php_is_numeric(range) {
                return false;
            }
            return self.validate(range) && Self::valid_step(step);
        }
        if value.contains('-') {
            if value.matches('-').count() > 1 {
                return false;
            }
            let (a, b) = value.split_once('-').unwrap_or_default();
            let (a, b) = (self.convert_literals(a), self.convert_literals(b));
            if a == "*" || b == "*" {
                return false;
            }
            return self.validate(&a) && self.validate(&b);
        }
        if !php_is_numeric(&value) || value.contains('.') {
            return false;
        }
        let v = php_int(&value);
        let (start, end) = self.range();
        (start..=end).contains(&v)
    }

    /// `validate()` of each field class.
    fn validate(self, value: &str) -> bool {
        let basic = self.validate_base(value);
        match self {
            Field::DayOfMonth => {
                if value.contains(',') && (value.contains('W') || value.contains('L')) {
                    return false;
                }
                if basic {
                    return true;
                }
                if value == "?" || value == "L" {
                    return true;
                }
                match value.strip_suffix('W') {
                    Some(rest) => self.validate(rest),
                    None => false,
                }
            }
            Field::DayOfWeek => {
                if basic {
                    return true;
                }
                if value == "?" {
                    return true;
                }
                if value.contains('#') {
                    let mut chunks = value.split('#');
                    let weekday = self.convert_literals(chunks.next().unwrap_or_default());
                    let nth = chunks.next().unwrap_or_default();
                    if self.validate_base(&weekday)
                        && php_is_numeric(nth)
                        && (1..=5).contains(&php_int(nth))
                    {
                        return true;
                    }
                }
                match value.strip_suffix('L') {
                    Some(rest) => self.validate(rest),
                    None => false,
                }
            }
            _ => basic,
        }
    }

    /// `AbstractField::isSatisfied()`.
    fn is_satisfied(self, date_value: i64, value: &str) -> Result<bool, CronError> {
        if value.contains('/') {
            return self.is_in_increments_of_ranges(date_value, value);
        }
        if value.contains('-') {
            return Ok(self.is_in_range(date_value, value));
        }
        Ok(value == "*" || date_value == php_int(value))
    }

    /// `AbstractField::isInRange()`.
    fn is_in_range(self, date_value: i64, value: &str) -> bool {
        let (a, b) = value.split_once('-').unwrap_or((value, ""));
        let (a, b) = (
            self.convert_literals(a.trim()),
            self.convert_literals(b.trim()),
        );
        date_value >= php_int(&a) && date_value <= php_int(&b)
    }

    /// `AbstractField::isInIncrementsOfRanges()`.
    fn is_in_increments_of_ranges(self, date_value: i64, value: &str) -> Result<bool, CronError> {
        let (range, step) = value.split_once('/').unwrap_or((value, "0"));
        let (range, step) = (range.trim(), step.trim());
        if step == "0" {
            return Ok(false);
        }
        let step = php_int(step);
        let (start, end) = self.range();
        let whole = format!("{start}-{end}");
        let range = if range == "*" { whole.as_str() } else { range };
        let (range_start, range_end) = match range.split_once('-') {
            Some((a, b)) => (php_int(a), php_int(b)),
            None => (php_int(range), php_int(range)),
        };
        if range_start < start || range_start > end || range_start > range_end {
            return Err(CronError::Field("Invalid range start requested".into()));
        }
        if range_end < start || range_end > end || range_end < range_start {
            return Err(CronError::Field("Invalid range end requested".into()));
        }
        if step > end {
            return Ok(date_value == self.full_range_at(step));
        }
        if step > range_end - range_start {
            return Ok(date_value == range_start);
        }
        Ok(date_value >= range_start
            && date_value <= range_end
            && (date_value - range_start) % step == 0)
    }

    /// `AbstractField::getRangeForExpression()` for a part without a comma.
    fn range_for_expression(self, expression: &str, max: i64) -> Vec<i64> {
        let expression = self.convert_literals(expression);
        let (start, end) = self.range();
        if !expression.contains('-') && !expression.contains('/') {
            return vec![php_int(&expression)];
        }
        let (offset, to, step) = match expression.split_once('/') {
            None => {
                let (a, b) = expression.split_once('-').unwrap_or((&expression, ""));
                (
                    self.convert_literals(a),
                    php_int(&self.convert_literals(b)),
                    1,
                )
            }
            Some((range, step)) => {
                let (range, step) = (range.trim(), php_int(step.trim()));
                match range.split_once('-') {
                    Some((a, b)) => (a.to_owned(), php_int(b), step),
                    None => (range.to_owned(), max, step),
                }
            }
        };
        let offset = if offset == "*" {
            start
        } else {
            php_int(&offset)
        };
        if step >= end {
            return vec![self.full_range_at(step)];
        }
        let mut values = Vec::new();
        let mut i = offset;
        while step > 0 && i <= to {
            values.push(i);
            i += step;
        }
        values.sort_unstable();
        values
    }

    /// `isSatisfiedBy()` of each field class.
    fn is_satisfied_by(self, date: &Date, value: &str, invert: bool) -> Result<bool, CronError> {
        match self {
            Field::Minutes => {
                if value == "?" {
                    return Ok(true);
                }
                self.is_satisfied(i64::from(date.minute()), value)
            }
            Field::Hours => hours_satisfied_by(date, value, invert),
            Field::DayOfMonth => day_of_month_satisfied_by(date, value),
            Field::Month => {
                if value == "?" {
                    return Ok(true);
                }
                let value = self.convert_literals(value);
                self.is_satisfied(i64::from(date.month()), &value)
            }
            Field::DayOfWeek => day_of_week_satisfied_by(date, value),
        }
    }

    /// `increment()` of each field class.
    fn increment(self, date: &mut Date, invert: bool, part: Option<&str>) {
        match self {
            Field::Minutes => minutes_increment(date, invert, part),
            Field::Hours => hours_increment(date, invert, part),
            Field::DayOfMonth | Field::DayOfWeek => {
                *date = add_days(date, if invert { -1 } else { 1 });
                *date = if invert {
                    set_time(date, 23, 59)
                } else {
                    set_time(date, 0, 0)
                };
            }
            Field::Month => {
                let (y, m) = (date.year(), date.month());
                let target = if invert {
                    let (py, pm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
                    ymd(py, pm, days_in_month(py, pm))
                } else {
                    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
                    ymd(ny, nm, 1)
                };
                *date = on_date(date, target);
                *date = if invert {
                    set_time(date, 23, 59)
                } else {
                    set_time(date, 0, 0)
                };
            }
        }
    }
}

fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap_or(NaiveDate::MIN)
}

fn days_in_month(y: i32, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    ymd(ny, nm, 1)
        .pred_opt()
        .map(|d| d.day())
        .unwrap_or_default()
}

fn offset_seconds(d: &Date) -> i64 {
    i64::from(d.offset().fix().local_minus_utc())
}

fn is_dst(d: &Date) -> bool {
    !d.offset().dst_offset().is_zero()
}

/// How PHP resolves a local time: a time inside a gap moves forward by the gap, and a time that
/// exists twice stays on the side of the daylight saving flag the date already has.
fn resolve(tz: Tz, local: NaiveDateTime, prefer_dst: bool) -> Date {
    match tz.from_local_datetime(&local) {
        LocalResult::Single(d) => d,
        LocalResult::Ambiguous(a, b) => {
            if is_dst(&a) == prefer_dst {
                a
            } else {
                b
            }
        }
        LocalResult::None => {
            let before = tz
                .from_local_datetime(&(local - Duration::days(1)))
                .earliest()
                .map(|d| offset_seconds(&d))
                .unwrap_or_default();
            tz.from_utc_datetime(&(local - Duration::seconds(before)))
        }
    }
}

/// `$date->setTime($h, $m)`.
fn set_time(date: &Date, h: u32, m: u32) -> Date {
    let local = date
        .date_naive()
        .and_hms_opt(h, m, 0)
        .unwrap_or(NaiveDateTime::MIN);
    resolve(date.timezone(), local, is_dst(date))
}

/// `$date->add(new DateInterval('P1D'))` and `sub`: a wall clock change.
fn add_days(date: &Date, days: i64) -> Date {
    resolve(
        date.timezone(),
        date.naive_local() + Duration::days(days),
        is_dst(date),
    )
}

/// `$date->modify('first day of next month')` and `last day of previous month`.
fn on_date(date: &Date, target: NaiveDate) -> Date {
    resolve(date.timezone(), target.and_time(date.time()), is_dst(date))
}

/// `AbstractField::timezoneSafeModify()` with a seconds count.
fn safe_add(date: &Date, seconds: i64) -> Date {
    *date + Duration::seconds(seconds)
}

/// `AbstractField::setTimeHour()`.
fn set_time_hour(date: &mut Date, invert: bool, original_timestamp: i64) {
    *date = set_time(date, date.hour(), if invert { 59 } else { 0 });
    let actual = date.timestamp();
    if !invert && actual <= original_timestamp {
        *date = safe_add(date, 3600);
    } else if invert && actual >= original_timestamp {
        *date = safe_add(date, -3600);
    }
}

/// `HoursField::isSatisfiedBy()`. `getPastTransition()` only answers whether the time zone changed
/// offset in the last hour, which comparing the offsets answers as well.
fn hours_satisfied_by(date: &Date, value: &str, invert: bool) -> Result<bool, CronError> {
    let check = i64::from(date.hour());
    let satisfied = Field::Hours.is_satisfied(check, value)?;
    if satisfied {
        return Ok(true);
    }
    if offset_seconds(&safe_add(date, -3600)) != offset_seconds(date) {
        let change = offset_seconds(&safe_add(date, 3600)) - offset_seconds(&safe_add(date, -3600));
        if change >= 3600 {
            return Field::Hours.is_satisfied(check - 1, value);
        }
        if !invert && change <= -3600 {
            return Field::Hours.is_satisfied(check + 1, value);
        }
    }
    Ok(false)
}

/// `HoursField::increment()`.
fn hours_increment(date: &mut Date, invert: bool, part: Option<&str>) {
    let original_timestamp = date.timestamp();
    let part = part.filter(|p| *p != "*");
    let Some(part) = part else {
        *date = safe_add(date, if invert { -3600 } else { 3600 });
        set_time_hour(date, invert, original_timestamp);
        return;
    };
    let hours: Vec<i64> = part
        .split(',')
        .flat_map(|p| Field::Hours.range_for_expression(p, 23))
        .collect();
    let current = i64::from(date.hour());
    let target = hours[list_position(&hours, current, invert)];
    let mut original_hour = i64::from(date.hour());
    let original_dst = is_dst(date);
    let original_day = i64::from(date.day());
    let previous_offset = offset_seconds(date);

    if invert {
        if original_hour <= target {
            *date = safe_add(date, -(original_hour + 1) * 3600);
            if i64::from(date.day()) != original_day - 1 && date.hour() != 23 {
                *date = safe_add(date, previous_offset - offset_seconds(date));
            }
            original_hour = i64::from(date.hour());
        }
        *date = safe_add(date, -(original_hour - target) * 3600);
    } else {
        if original_hour >= target {
            *date = safe_add(date, (24 - original_hour) * 3600);
            if i64::from(date.day()) != original_day + 1 && date.hour() != 0 {
                *date = safe_add(date, previous_offset - offset_seconds(date));
            }
            original_hour = i64::from(date.hour());
        }
        *date = safe_add(date, (target - original_hour) * 3600);
    }
    if !original_dst && is_dst(date) {
        *date = safe_add(date, -3600);
    }
    set_time_hour(date, invert, original_timestamp);
    let actual = i64::from(date.hour());
    if invert && (actual == target - 1 || (actual == 23 && target == 0)) {
        *date = safe_add(date, 3600);
    }
}

/// The loop that picks `$position` in `HoursField::increment()` and `MinutesField::increment()`.
fn list_position(values: &[i64], current: i64, invert: bool) -> usize {
    let mut position = if invert { values.len() - 1 } else { 0 };
    for i in 0..values.len().saturating_sub(1) {
        let (a, b) = (values[i], values[i + 1]);
        if (!invert && current >= a && current < b) || (invert && current > a && current <= b) {
            position = if invert { i } else { i + 1 };
            break;
        }
    }
    position
}

/// `sort($parts)`: numeric strings compare as numbers, others as strings; PHP's sort is stable.
fn php_sort(parts: &mut [&str]) {
    let less = |a: &str, b: &str| {
        if php_is_numeric(a) && php_is_numeric(b) {
            php_int(a) < php_int(b)
        } else {
            a < b
        }
    };
    for i in 1..parts.len() {
        let mut j = i;
        while j > 0 && less(parts[j], parts[j - 1]) {
            parts.swap(j, j - 1);
            j -= 1;
        }
    }
}

/// `MinutesField::increment()`.
fn minutes_increment(date: &mut Date, invert: bool, part: Option<&str>) {
    let Some(part) = part else {
        *date = safe_add(date, if invert { -60 } else { 60 });
        return;
    };
    let mut parts: Vec<&str> = part.split(',').collect();
    php_sort(&mut parts);
    let minutes: Vec<i64> = parts
        .iter()
        .flat_map(|p| Field::Minutes.range_for_expression(p, 59))
        .collect();
    let current = i64::from(date.minute());
    let target = minutes[list_position(&minutes, current, invert)];
    let mut original = current;
    if invert {
        if original <= target {
            *date = safe_add(date, -(original + 1) * 60);
            original = i64::from(date.minute());
        }
        *date = safe_add(date, -(original - target) * 60);
    } else {
        if original >= target {
            *date = safe_add(date, (60 - original) * 60);
            original = i64::from(date.minute());
        }
        *date = safe_add(date, (target - original) * 60);
    }
}

/// `DayOfMonthField::getNearestWeekday()`; `createFromFormat` rolls a day past the month over.
fn nearest_weekday(year: i32, month: u32, target_day: i64) -> Option<NaiveDate> {
    let target = ymd(year, month, 1) + Duration::days(target_day - 1);
    if target.weekday().number_from_monday() < 6 {
        return Some(target);
    }
    let last = i64::from(days_in_month(target.year(), target.month()));
    for i in [-1, 1, -2, 2] {
        let adjusted = target_day + i;
        if adjusted > 0 && adjusted <= last {
            let candidate = ymd(year, month, 1) + Duration::days(adjusted - 1);
            if candidate.weekday().number_from_monday() < 6 && candidate.month() == month {
                return Some(candidate);
            }
        }
    }
    None
}

/// `DayOfMonthField::isSatisfiedBy()`.
fn day_of_month_satisfied_by(date: &Date, value: &str) -> Result<bool, CronError> {
    if value == "?" {
        return Ok(true);
    }
    if value == "L" {
        return Ok(date.day() == days_in_month(date.year(), date.month()));
    }
    if let Some(position) = value.find('W').filter(|p| *p > 0) {
        return match nearest_weekday(date.year(), date.month(), php_int(&value[..position])) {
            Some(nearest) => Ok(date.day() == nearest.day()),
            None => Err(CronError::Field("Unable to return nearest weekday".into())),
        };
    }
    Field::DayOfMonth.is_satisfied(i64::from(date.day()), value)
}

/// `DayOfWeekField::isSatisfiedBy()`.
fn day_of_week_satisfied_by(date: &Date, value: &str) -> Result<bool, CronError> {
    if value == "?" {
        return Ok(true);
    }
    let field = Field::DayOfWeek;
    let value = field.convert_literals(value);
    let (year, month) = (date.year(), date.month());
    let last_day = days_in_month(year, month);
    let w = i64::from(date.weekday().num_days_from_sunday());
    let n = i64::from(date.weekday().number_from_monday());

    if let Some(position) = value.find('L').filter(|p| *p > 0) {
        let weekday = php_int(&field.convert_literals(&value[..position])) % 7;
        return Ok(weekday == w && last_day - date.day() < 7);
    }
    if value.find('#').is_some_and(|p| p > 0) {
        let mut chunks = value.split('#');
        let weekday = chunks.next().unwrap_or_default();
        let nth = chunks.next().unwrap_or_default();
        if !php_is_numeric(nth) {
            return Err(CronError::Field(format!(
                "Hashed weekdays must be numeric, {nth} given"
            )));
        }
        let nth = php_int(nth);
        let weekday = if weekday == "0" {
            7
        } else {
            php_int(&field.convert_literals(weekday))
        };
        if !(0..=7).contains(&weekday) {
            return Err(CronError::Field(format!(
                "Weekday must be a value between 0 and 7. {weekday} given"
            )));
        }
        if !(1..=5).contains(&nth) {
            return Err(CronError::Field(format!(
                "There are never more than 5 or less than 1 of a given weekday in a month, {nth} given"
            )));
        }
        if n != weekday {
            return Ok(false);
        }
        let mut count = 0;
        let mut day = 1;
        while day < last_day + 1 {
            if i64::from(ymd(year, month, day).weekday().number_from_monday()) == weekday {
                count += 1;
                if count >= nth {
                    break;
                }
            }
            day += 1;
        }
        return Ok(date.day() == day);
    }
    let value = match value.split_once('-') {
        Some(("7", rest)) => format!("0-{rest}"),
        Some((first, "0")) => format!("{first}-7"),
        _ => value,
    };
    let field_value = if value.contains('7') { n } else { w };
    field.is_satisfied(field_value, &value)
}

/// The order `getRunDate()` tests the parts in (`CronExpression::$order`, no year).
const ORDER: [usize; 5] = [3, 2, 4, 1, 0];
const MAX_ITERATIONS: usize = 1000;

/// `Cron\CronExpression`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronExpression {
    parts: Vec<String>,
}

impl CronExpression {
    /// `new CronExpression($expression)`.
    pub fn new(expression: &str) -> Result<Self, CronError> {
        let lower = expression.to_ascii_lowercase();
        let value = MAPPINGS
            .iter()
            .find(|(alias, _)| *alias == lower)
            .map_or(expression, |(_, v)| *v);
        // preg_split('/\s/', ...) with PREG_SPLIT_NO_EMPTY.
        let split: Vec<&str> = value
            .split([' ', '\t', '\n', '\r', '\x0B', '\x0C'])
            .filter(|s| !s.is_empty())
            .collect();
        let question = |i: usize| split.get(i).is_some_and(|p| *p == "?");
        if split.len() < 5
            || question(0)
            || question(1)
            || question(3)
            || (question(2) && question(4))
        {
            return Err(CronError::Invalid(value.to_owned()));
        }
        for (position, part) in split.iter().enumerate() {
            // `FieldFactory::getField()` knows five positions.
            let valid = FIELDS.get(position).is_some_and(|f| f.validate(part));
            if !valid {
                return Err(CronError::InvalidField((*part).to_owned(), position));
            }
        }
        Ok(Self {
            parts: split.into_iter().map(str::to_owned).collect(),
        })
    }

    /// `getExpression(CronExpression::HOUR)`.
    pub fn hour_part(&self) -> &str {
        &self.parts[1]
    }

    /// `isDue($time)`: seconds are ignored.
    pub fn is_due(&self, time: &Date) -> bool {
        let current = drop_seconds(time);
        self.run_date(time, false, true)
            .is_ok_and(|d| d.timestamp() == current.timestamp())
    }

    /// `getNextRunDate($time, 0, $allowCurrentDate)`.
    pub fn next_run_date(&self, time: &Date, allow_current: bool) -> Result<Date, CronError> {
        self.run_date(time, false, allow_current)
    }

    /// `getPreviousRunDate($time, 0, $allowCurrentDate)`.
    pub fn previous_run_date(&self, time: &Date, allow_current: bool) -> Result<Date, CronError> {
        self.run_date(time, true, allow_current)
    }

    /// `getRunDate()` with `$nth = 0`.
    fn run_date(&self, time: &Date, invert: bool, allow_current: bool) -> Result<Date, CronError> {
        let current = drop_seconds(time);
        let mut next = current;
        let parts: Vec<(usize, &str)> = ORDER
            .iter()
            .map(|p| (*p, self.parts[*p].as_str()))
            .filter(|(_, part)| *part != "*")
            .collect();
        let has = |p: usize| parts.iter().any(|(position, _)| *position == p);

        if has(2) && has(4) {
            return self.day_or_weekday(time, invert, allow_current);
        }

        let minute_part = parts.iter().find(|(p, _)| *p == 0).map(|(_, part)| *part);
        'search: for _ in 0..MAX_ITERATIONS {
            for (position, part) in &parts {
                let field = FIELDS[*position];
                let satisfied = if part.contains(',') {
                    let mut any = false;
                    for item in part.split(',') {
                        if field.is_satisfied_by(&next, item.trim(), invert)? {
                            any = true;
                            break;
                        }
                    }
                    any
                } else {
                    field.is_satisfied_by(&next, part, invert)?
                };
                if !satisfied {
                    field.increment(&mut next, invert, Some(part));
                    continue 'search;
                }
            }
            if !allow_current && next == current {
                Field::Minutes.increment(&mut next, invert, minute_part);
                continue;
            }
            return Ok(next);
        }
        Err(CronError::Impossible)
    }

    /// A day of month and a weekday together match when either matches.
    fn day_or_weekday(
        &self,
        time: &Date,
        invert: bool,
        allow_current: bool,
    ) -> Result<Date, CronError> {
        let p = &self.parts;
        let dom = Self::new(&format!("{} {} {} {} *", p[0], p[1], p[2], p[3]))?;
        let dow = Self::new(&format!("{} {} * {} {}", p[0], p[1], p[3], p[4]))?;
        let mut runs = Vec::new();
        // getMultipleRunDates() ends at a RuntimeException.
        for (expression, part) in [(dom, &p[2]), (dow, &p[4])] {
            let run = match expression.run_date(time, invert, allow_current) {
                Ok(d) => Some(d),
                Err(CronError::Impossible) => None,
                Err(e) => return Err(e),
            };
            if let Some(d) = run.filter(|_| part != "?" && part != "*") {
                runs.push(d);
            }
        }
        // usort on the local time, then array_reverse when inverted; $combined[0].
        let pick = if invert {
            runs.into_iter().max_by_key(DateTime::naive_local)
        } else {
            runs.into_iter().min_by_key(DateTime::naive_local)
        };
        pick.ok_or(CronError::Impossible)
    }
}

/// `createFromFormat("!Y-m-d H:iO", ...)`: the same instant without its seconds.
fn drop_seconds(time: &Date) -> Date {
    let extra = i64::from(time.second());
    let nanos = i64::from(time.nanosecond());
    *time - Duration::seconds(extra) - Duration::nanoseconds(nanos)
}

/// A time zone by name, for a name `validate_timezone()` accepted.
pub fn time_zone(name: &str) -> Option<Tz> {
    name.parse().ok()
}

/// `$time->copy()->setTimezone($timezone)`.
pub fn in_zone(time: DateTime<Utc>, tz: Tz) -> Date {
    time.with_timezone(&tz)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(tz: Tz, y: i32, mo: u32, d: u32, h: u32, mi: u32) -> Date {
        tz.with_ymd_and_hms(y, mo, d, h, mi, 0).earliest().unwrap()
    }

    fn cron(s: &str) -> CronExpression {
        CronExpression::new(s).unwrap()
    }

    #[test]
    fn validates_like_the_field_classes() {
        for ok in [
            "* * * * *",
            "@daily",
            "@ANNUALLY",
            "*/5 1-3 1,15 JAN MON",
            "0 0 L * ?",
            "0 0 15W * *",
            "0 0 ? * 5#2",
            "0 0 ? * FRIL",
            "0 0 * * MON-FRI",
            "0 0 * * 7",
        ] {
            assert!(CronExpression::new(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "every_minute",
            "* * * *",
            "* * * * * *",
            "? * * * *",
            "* ? * * *",
            "* * * ? *",
            "* * ? * ?",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 13 *",
            "* * * * 8",
            "5/2 * * * *",
            "*/0 * * * *",
            "1-2-3 * * * *",
            "*-1 * * * *",
            "1.5 * * * *",
            "0 0 1W,2 * *",
            "*/-5 * * * *",
        ] {
            assert!(CronExpression::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn aliases_match_their_expressions() {
        let t = at(Tz::UTC, 2026, 10, 1, 0, 0);
        assert!(cron("@daily").is_due(&t));
        assert!(cron("@midnight").is_due(&t));
        assert!(cron("@hourly").is_due(&t));
        assert!(cron("@monthly").is_due(&t));
        assert!(!cron("@yearly").is_due(&t));
        assert!(cron("@yearly").is_due(&at(Tz::UTC, 2027, 1, 1, 0, 0)));
        // 2026-10-04 is a Sunday.
        assert!(cron("@weekly").is_due(&at(Tz::UTC, 2026, 10, 4, 0, 0)));
    }

    #[test]
    fn a_time_zone_moves_when_it_is_due() {
        let utc: DateTime<Utc> = Utc.with_ymd_and_hms(2026, 7, 1, 22, 30, 15).unwrap();
        let karachi = in_zone(utc, chrono_tz::Asia::Karachi);
        assert_eq!(karachi.hour(), 3);
        assert!(cron("30 3 * * *").is_due(&karachi));
        assert!(!cron("30 22 * * *").is_due(&karachi));
        assert!(cron("30 22 * * *").is_due(&in_zone(utc, Tz::UTC)));
    }

    #[test]
    fn previous_run_date_allows_the_current_minute() {
        let t = at(Tz::UTC, 2026, 3, 11, 12, 34);
        assert_eq!(
            cron("*/5 * * * *").previous_run_date(&t, true).unwrap(),
            at(Tz::UTC, 2026, 3, 11, 12, 30)
        );
        assert_eq!(
            cron("34 12 * * *").previous_run_date(&t, true).unwrap(),
            t,
            "allowCurrentDate"
        );
        assert_eq!(
            cron("34 12 * * *").previous_run_date(&t, false).unwrap(),
            at(Tz::UTC, 2026, 3, 10, 12, 34)
        );
    }

    #[test]
    fn day_of_month_or_weekday() {
        // 2026-10-02 is a Friday; the 15th is a Thursday.
        let friday = at(Tz::UTC, 2026, 10, 2, 0, 0);
        let fifteenth = at(Tz::UTC, 2026, 10, 15, 0, 0);
        let other = at(Tz::UTC, 2026, 10, 14, 0, 0);
        let c = cron("0 0 15 * FRI");
        assert!(c.is_due(&friday));
        assert!(c.is_due(&fifteenth));
        assert!(!c.is_due(&other));
    }
}
