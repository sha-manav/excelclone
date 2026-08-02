//! Excel serial date/time conversions.
//!
//! Excel stores dates as days since 1899-12-31 (serial 1 == 1900-01-01) with
//! the time of day in the fractional part, and treats 1900 as a leap year:
//! serial 60 is the non-existent 1900-02-29, so every date from 1900-03-01
//! onward is shifted one day later than a true calendar count.
//!
//! Gridline reproduces this mapping so serials round-trip with real
//! workbooks, with one deliberate exception: serial 60 has no calendar date
//! and converts to `None` (a loud `#NUM!`) rather than to a phantom
//! 1900-02-29. See DECISIONS.md.

use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};

/// Serials at or above this point are offset by Excel's phantom leap day.
const PHANTOM_LEAP_SERIAL: i64 = 60;

/// True calendar epoch for serials 1..=59 (serial 1 == 1900-01-01).
fn early_epoch() -> NaiveDate {
    NaiveDate::from_ymd_opt(1899, 12, 31).expect("valid epoch")
}

/// Shifted epoch for serials >= 61, absorbing the phantom 1900-02-29.
fn late_epoch() -> NaiveDate {
    NaiveDate::from_ymd_opt(1899, 12, 30).expect("valid epoch")
}

fn first_march_1900() -> NaiveDate {
    NaiveDate::from_ymd_opt(1900, 3, 1).expect("valid date")
}

/// Days from the epoch; None if before 1900-01-01 or past Excel's range.
pub fn date_to_serial(d: NaiveDate) -> Option<f64> {
    let epoch = if d >= first_march_1900() {
        late_epoch()
    } else {
        early_epoch()
    };
    let days = d.signed_duration_since(epoch).num_days();
    if !(1..=2_958_465).contains(&days) {
        return None;
    }
    Some(days as f64)
}

pub fn ymd_to_serial(y: i32, m: u32, d: u32) -> Option<f64> {
    date_to_serial(NaiveDate::from_ymd_opt(y, m, d)?)
}

/// Whole-day part of a serial back to a date. Serial 60 (Excel's phantom
/// 1900-02-29) has no calendar equivalent and yields None.
pub fn serial_to_date(serial: f64) -> Option<NaiveDate> {
    let days = serial.floor() as i64;
    if !(1..=2_958_465).contains(&days) || days == PHANTOM_LEAP_SERIAL {
        return None;
    }
    let epoch = if days > PHANTOM_LEAP_SERIAL {
        late_epoch()
    } else {
        early_epoch()
    };
    epoch.checked_add_signed(chrono::Duration::days(days))
}

/// Fractional part of a serial as (hour, minute, second).
pub fn serial_to_hms(serial: f64) -> (u32, u32, u32) {
    let frac = serial - serial.floor();
    // Round to the nearest second to absorb float error.
    let total = (frac * 86_400.0).round() as i64 % 86_400;
    (
        (total / 3600) as u32,
        ((total % 3600) / 60) as u32,
        (total % 60) as u32,
    )
}

pub fn datetime_to_serial(dt: NaiveDateTime) -> Option<f64> {
    let day = date_to_serial(dt.date())?;
    let frac = (dt.num_seconds_from_midnight() as f64) / 86_400.0;
    Some(day + frac)
}

/// Convert an injected wall clock (ms since Unix epoch, UTC) to a serial.
pub fn now_ms_to_serial(now_ms: i64) -> f64 {
    let dt = chrono::DateTime::from_timestamp_millis(now_ms)
        .map(|d| d.naive_utc())
        .unwrap_or_default();
    datetime_to_serial(dt).unwrap_or(0.0)
}

/// Excel's date components.
pub fn serial_year(serial: f64) -> Option<i32> {
    serial_to_date(serial).map(|d| d.year())
}

pub fn serial_month(serial: f64) -> Option<u32> {
    serial_to_date(serial).map(|d| d.month())
}

pub fn serial_day(serial: f64) -> Option<u32> {
    serial_to_date(serial).map(|d| d.day())
}

/// Add months to a serial, clamping the day to the target month's length
/// (Excel EDATE/EOMONTH behavior).
pub fn add_months(serial: f64, months: i64) -> Option<NaiveDate> {
    let d = serial_to_date(serial)?;
    let total = d.year() as i64 * 12 + (d.month() as i64 - 1) + months;
    let (y, m) = (
        (total.div_euclid(12)) as i32,
        (total.rem_euclid(12)) as u32 + 1,
    );
    let last = last_day_of_month(y, m)?;
    NaiveDate::from_ymd_opt(y, m, d.day().min(last))
}

pub fn last_day_of_month(y: i32, m: u32) -> Option<u32> {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    let first_next = NaiveDate::from_ymd_opt(ny, nm, 1)?;
    Some(first_next.pred_opt()?.day())
}

/// Parse a date/time string the way cell entry does (ISO and common US forms).
pub fn parse_date_text(s: &str) -> Option<f64> {
    let t = s.trim();
    for fmt in ["%Y-%m-%d", "%m/%d/%Y", "%m/%d/%y", "%d-%b-%Y", "%b %d, %Y"] {
        if let Ok(d) = NaiveDate::parse_from_str(t, fmt) {
            return date_to_serial(d);
        }
    }
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%m/%d/%Y %H:%M"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(t, fmt) {
            return datetime_to_serial(dt);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_serials_match_excel() {
        // All Excel-verified (Microsoft 365).
        assert_eq!(ymd_to_serial(1900, 1, 1), Some(1.0));
        assert_eq!(ymd_to_serial(1900, 2, 28), Some(59.0));
        assert_eq!(ymd_to_serial(1900, 3, 1), Some(61.0));
        assert_eq!(ymd_to_serial(2020, 1, 1), Some(43831.0));
        assert_eq!(ymd_to_serial(2024, 2, 29), Some(45351.0));
        assert_eq!(serial_to_date(1.0), NaiveDate::from_ymd_opt(1900, 1, 1));
        assert_eq!(serial_to_date(61.0), NaiveDate::from_ymd_opt(1900, 3, 1));
        assert_eq!(serial_to_date(43831.0), NaiveDate::from_ymd_opt(2020, 1, 1));
        assert_eq!(serial_year(45351.0), Some(2024));
        assert_eq!(serial_month(45351.0), Some(2));
        assert_eq!(serial_day(45351.0), Some(29));
        // Excel's phantom 1900-02-29 has no calendar date here.
        assert_eq!(serial_to_date(60.0), None);
        // Out of range.
        assert_eq!(ymd_to_serial(1899, 12, 31), None);
        assert_eq!(serial_to_date(0.0), None);
    }

    #[test]
    fn time_fractions() {
        let s = ymd_to_serial(2020, 1, 1).unwrap() + 0.5;
        assert_eq!(serial_to_hms(s), (12, 0, 0));
        let s = ymd_to_serial(2020, 1, 1).unwrap() + 0.75;
        assert_eq!(serial_to_hms(s), (18, 0, 0));
    }

    #[test]
    fn month_arithmetic_clamps() {
        let jan31 = ymd_to_serial(2024, 1, 31).unwrap();
        assert_eq!(add_months(jan31, 1), NaiveDate::from_ymd_opt(2024, 2, 29));
        assert_eq!(add_months(jan31, -1), NaiveDate::from_ymd_opt(2023, 12, 31));
        assert_eq!(last_day_of_month(2023, 2), Some(28));
        assert_eq!(last_day_of_month(2024, 2), Some(29));
    }

    #[test]
    fn parses_common_date_text() {
        assert_eq!(parse_date_text("2020-01-01"), Some(43831.0));
        assert_eq!(parse_date_text("1/1/2020"), Some(43831.0));
        assert_eq!(parse_date_text("not a date"), None);
    }
}
