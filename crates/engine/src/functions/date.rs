//! Date/time functions plus the two volatile random functions.
//!
//! Dates are Excel serials (see `crate::serial`). Expected values in tests
//! are Excel-verified (Microsoft 365).

use super::{expect_args, num_result};
use crate::ast::Expr;
use crate::eval::EvalCtx;
use crate::serial;
use crate::value::{ErrorKind, Value};
use chrono::{Datelike, Duration, NaiveDate};

/// Coerce an argument to a date serial: numbers pass through, empty is 0,
/// and text is parsed as a date first, then as a plain number (Excel accepts
/// both "2020-01-01" and "43831" where a date is expected). Booleans are not
/// dates in Excel and give #VALUE!.
fn arg_serial(ctx: &EvalCtx, e: &Expr) -> Result<f64, ErrorKind> {
    match ctx.eval_scalar(e) {
        Value::Number(n) => Ok(n),
        Value::Empty => Ok(0.0),
        Value::Error(k) => Err(k),
        Value::Bool(_) => Err(ErrorKind::Value),
        Value::Text(s) => serial::parse_date_text(&s)
            .or_else(|| crate::eval::parse_number_text(&s))
            .ok_or(ErrorKind::Value),
    }
}

/// The calendar date behind an argument; serials without one are #NUM!.
fn arg_date(ctx: &EvalCtx, e: &Expr) -> Result<NaiveDate, ErrorKind> {
    serial::serial_to_date(arg_serial(ctx, e)?).ok_or(ErrorKind::Num)
}

/// TODAY(): whole-day serial from the injected clock. VOLATILE.
pub fn today(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 0, 0) {
        return Value::Error(k);
    }
    num_result(Ok(serial::now_ms_to_serial(ctx.now_ms).floor()))
}

/// NOW(): serial including the time-of-day fraction. VOLATILE.
pub fn now(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 0, 0) {
        return Value::Error(k);
    }
    num_result(Ok(serial::now_ms_to_serial(ctx.now_ms)))
}

/// Excel's DATE normalization, kept pure for testing: years 0..1899 are
/// relative to 1900, then months fold into a year/month pair and the day is
/// applied as an offset from the 1st, so out-of-range parts roll over
/// instead of erroring: DATE(2020,13,1) is 2021-01-01 and DATE(2020,1,0) is
/// 2019-12-31.
fn date_from_parts(year: i64, month: i64, day: i64) -> Option<NaiveDate> {
    if !(0..=9999).contains(&year) {
        return None;
    }
    // Guard the day offset so the Duration below cannot overflow; Excel's
    // whole serial range is under 3M days anyway.
    if !(-4_000_000..=4_000_000).contains(&day) {
        return None;
    }
    let year = if year < 1900 { year + 1900 } else { year };
    let total = year.checked_mul(12)?.checked_add(month.checked_sub(1)?)?;
    let (y, m) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    let first = NaiveDate::from_ymd_opt(i32::try_from(y).ok()?, m, 1)?;
    first.checked_add_signed(Duration::days(day - 1))
}

pub fn date(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 3, 3) {
        return Value::Error(k);
    }
    num_result((|| {
        let y = ctx.eval_number(&args[0])?;
        let m = ctx.eval_number(&args[1])?;
        let d = ctx.eval_number(&args[2])?;
        let date = date_from_parts(y.trunc() as i64, m.trunc() as i64, d.trunc() as i64)
            .ok_or(ErrorKind::Num)?;
        // Anything before serial 1 is outside Excel's date system.
        serial::date_to_serial(date).ok_or(ErrorKind::Num)
    })())
}

fn date_part(ctx: &EvalCtx, args: &[Expr], f: impl Fn(NaiveDate) -> f64) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    num_result(arg_date(ctx, &args[0]).map(f))
}

pub fn year(ctx: &EvalCtx, args: &[Expr]) -> Value {
    date_part(ctx, args, |d| d.year() as f64)
}

pub fn month(ctx: &EvalCtx, args: &[Expr]) -> Value {
    date_part(ctx, args, |d| d.month() as f64)
}

pub fn day(ctx: &EvalCtx, args: &[Expr]) -> Value {
    date_part(ctx, args, |d| d.day() as f64)
}

/// EOMONTH(start_date, months): last day of the month `months` away.
pub fn eomonth(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 2) {
        return Value::Error(k);
    }
    num_result((|| {
        let start = arg_serial(ctx, &args[0])?;
        let months = ctx.eval_number(&args[1])?.trunc() as i64;
        let shifted = serial::add_months(start, months).ok_or(ErrorKind::Num)?;
        let (y, m) = (shifted.year(), shifted.month());
        let last = serial::last_day_of_month(y, m).ok_or(ErrorKind::Num)?;
        let end = NaiveDate::from_ymd_opt(y, m, last).ok_or(ErrorKind::Num)?;
        serial::date_to_serial(end).ok_or(ErrorKind::Num)
    })())
}

/// Whole years elapsed, the way DATEDIF "Y" counts them: an anniversary that
/// has not been reached yet in the end year does not count.
fn whole_years(start: NaiveDate, end: NaiveDate) -> i64 {
    let mut y = (end.year() - start.year()) as i64;
    if (end.month(), end.day()) < (start.month(), start.day()) {
        y -= 1;
    }
    y
}

/// Whole months elapsed (DATEDIF "M").
fn whole_months(start: NaiveDate, end: NaiveDate) -> i64 {
    let mut m = (end.year() - start.year()) as i64 * 12 + end.month() as i64 - start.month() as i64;
    if end.day() < start.day() {
        m -= 1;
    }
    m
}

/// Same calendar day in a later year, clamped for Feb 29 starts.
fn add_years_clamped(d: NaiveDate, years: i64) -> Option<NaiveDate> {
    let y = i32::try_from(d.year() as i64 + years).ok()?;
    let last = serial::last_day_of_month(y, d.month())?;
    NaiveDate::from_ymd_opt(y, d.month(), d.day().min(last))
}

/// DATEDIF's unit math, kept pure for testing. None means an unknown unit.
fn datedif_between(start: NaiveDate, end: NaiveDate, unit: &str) -> Option<f64> {
    let n = match unit.to_ascii_uppercase().as_str() {
        "Y" => whole_years(start, end),
        "M" => whole_months(start, end),
        "D" => end.signed_duration_since(start).num_days(),
        // Days ignoring months and years. Excel borrows from the month
        // *preceding the end date*, which is why "MD" can go negative for
        // starts late in a longer month; that quirk is reproduced here.
        "MD" => {
            if end.day() >= start.day() {
                (end.day() - start.day()) as i64
            } else {
                let (py, pm) = if end.month() == 1 {
                    (end.year() - 1, 12)
                } else {
                    (end.year(), end.month() - 1)
                };
                end.day() as i64 + serial::last_day_of_month(py, pm)? as i64 - start.day() as i64
            }
        }
        // Whole months left over after the whole years.
        "YM" => whole_months(start, end) - whole_years(start, end) * 12,
        // Days left over after the whole years.
        "YD" => {
            let anchor = add_years_clamped(start, whole_years(start, end))?;
            end.signed_duration_since(anchor).num_days()
        }
        _ => return None,
    };
    Some(n as f64)
}

/// DATEDIF(start, end, unit): start after end is #NUM!, as is a unit Excel
/// does not recognize.
pub fn datedif(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 3, 3) {
        return Value::Error(k);
    }
    num_result((|| {
        let start = arg_date(ctx, &args[0])?;
        let end = arg_date(ctx, &args[1])?;
        let unit = ctx.eval_text(&args[2])?;
        if start > end {
            return Err(ErrorKind::Num);
        }
        datedif_between(start, end, &unit).ok_or(ErrorKind::Num)
    })())
}

/// Map a Sunday-based weekday index onto WEEKDAY's return_type numbering.
fn weekday_number(days_from_sunday: u32, return_type: i64) -> Option<f64> {
    let d = days_from_sunday as i64;
    match return_type {
        1 => Some((d + 1) as f64),
        2 => Some(((d + 6) % 7 + 1) as f64),
        3 => Some(((d + 6) % 7) as f64),
        _ => None,
    }
}

pub fn weekday(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 2) {
        return Value::Error(k);
    }
    num_result((|| {
        let d = arg_date(ctx, &args[0])?;
        let rt = match args.get(1) {
            Some(e) => ctx.eval_number(e)?.trunc() as i64,
            None => 1,
        };
        weekday_number(d.weekday().num_days_from_sunday(), rt).ok_or(ErrorKind::Num)
    })())
}

/// SplitMix64's finalizer: a pure avalanche mixer over 64 bits.
fn mix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Top 53 bits of a hash as a double in [0, 1).
fn unit_float(h: u64) -> f64 {
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// Deterministic replacement for system entropy.
///
/// The engine is a pure library with no I/O, and replaying the action log
/// from an empty workbook must reproduce the exact final state, so RAND and
/// RANDBETWEEN cannot touch `rand::random` or any other source of real
/// entropy. Instead they hash the injected clock together with a salt
/// derived from their arguments. This trades statistical quality for
/// replayability, and it has a visible limitation: within a single recalc
/// the clock is fixed, so two RAND() calls return the same value, as do two
/// RANDBETWEEN calls with identical bounds.
fn draw(now_ms: i64, salt: u64) -> f64 {
    unit_float(mix64((now_ms as u64) ^ mix64(salt)))
}

/// RAND(): a number in [0, 1). VOLATILE.
pub fn rand(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 0, 0) {
        return Value::Error(k);
    }
    num_result(Ok(draw(ctx.now_ms, 0)))
}

/// RANDBETWEEN(bottom, top): inclusive integer range. VOLATILE.
/// Non-integer bounds are pulled inward (bottom up, top down) so the result
/// always lies inside the requested interval.
pub fn randbetween(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 2) {
        return Value::Error(k);
    }
    num_result((|| {
        let bottom = ctx.eval_number(&args[0])?.ceil();
        let top = ctx.eval_number(&args[1])?.floor();
        if bottom > top {
            return Err(ErrorKind::Num);
        }
        let salt = mix64(bottom.to_bits()) ^ mix64(top.to_bits().rotate_left(17));
        let span = top - bottom + 1.0;
        Ok(bottom + (draw(ctx.now_ms, salt) * span).floor())
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn date_parts_roll_over() {
        assert_eq!(date_from_parts(2020, 1, 1), Some(ymd(2020, 1, 1)));
        // Excel-verified rollovers.
        assert_eq!(date_from_parts(2020, 13, 1), Some(ymd(2021, 1, 1)));
        assert_eq!(date_from_parts(2020, 1, 0), Some(ymd(2019, 12, 31)));
        assert_eq!(date_from_parts(2020, 0, 1), Some(ymd(2019, 12, 1)));
        assert_eq!(date_from_parts(2020, 25, 1), Some(ymd(2022, 1, 1)));
        assert_eq!(date_from_parts(2019, 2, 29), Some(ymd(2019, 3, 1)));
        assert_eq!(date_from_parts(2020, 2, 29), Some(ymd(2020, 2, 29)));
        assert_eq!(date_from_parts(2020, -1, 1), Some(ymd(2019, 11, 1)));
    }

    #[test]
    fn date_years_below_1900_are_offsets() {
        assert_eq!(date_from_parts(0, 1, 1), Some(ymd(1900, 1, 1)));
        assert_eq!(date_from_parts(20, 1, 1), Some(ymd(1920, 1, 1)));
        assert_eq!(date_from_parts(1899, 1, 1), Some(ymd(3799, 1, 1)));
        assert_eq!(date_from_parts(1900, 1, 1), Some(ymd(1900, 1, 1)));
        // Out of Excel's accepted year range.
        assert_eq!(date_from_parts(-1, 1, 1), None);
        assert_eq!(date_from_parts(10_000, 1, 1), None);
    }

    #[test]
    fn datedif_units() {
        let start = ymd(1969, 7, 16);
        let end = ymd(2020, 1, 1);
        assert_eq!(datedif_between(start, end, "Y"), Some(50.0));
        assert_eq!(datedif_between(start, end, "M"), Some(605.0));
        assert_eq!(datedif_between(start, end, "D"), Some(18431.0));
        assert_eq!(datedif_between(start, end, "MD"), Some(16.0));
        assert_eq!(datedif_between(start, end, "YM"), Some(5.0));
        assert_eq!(datedif_between(start, end, "YD"), Some(169.0));
        // Units are case-insensitive; anything else is unknown.
        assert_eq!(datedif_between(start, end, "yd"), Some(169.0));
        assert_eq!(datedif_between(start, end, "W"), None);
    }

    #[test]
    fn datedif_boundaries() {
        // Exact anniversaries count; one day short does not.
        assert_eq!(
            datedif_between(ymd(2020, 3, 1), ymd(2021, 3, 1), "Y"),
            Some(1.0)
        );
        assert_eq!(
            datedif_between(ymd(2020, 3, 1), ymd(2021, 2, 28), "Y"),
            Some(0.0)
        );
        assert_eq!(
            datedif_between(ymd(2020, 1, 31), ymd(2020, 3, 1), "M"),
            Some(1.0)
        );
        // Leap-day start with a non-leap end year clamps to Feb 28.
        assert_eq!(
            datedif_between(ymd(2020, 2, 29), ymd(2021, 3, 1), "YD"),
            Some(1.0)
        );
        // Excel's "MD" borrow can go negative; reproduced deliberately.
        assert_eq!(
            datedif_between(ymd(2020, 1, 31), ymd(2020, 3, 1), "MD"),
            Some(-1.0)
        );
    }

    #[test]
    fn weekday_return_types() {
        // 2020-01-01 was a Wednesday (Sunday-based index 3).
        assert_eq!(weekday_number(3, 1), Some(4.0));
        assert_eq!(weekday_number(3, 2), Some(3.0));
        assert_eq!(weekday_number(3, 3), Some(2.0));
        // Sunday and Saturday, the ends of each numbering.
        assert_eq!(weekday_number(0, 1), Some(1.0));
        assert_eq!(weekday_number(0, 2), Some(7.0));
        assert_eq!(weekday_number(0, 3), Some(6.0));
        assert_eq!(weekday_number(6, 1), Some(7.0));
        assert_eq!(weekday_number(6, 2), Some(6.0));
        assert_eq!(weekday_number(6, 3), Some(5.0));
        assert_eq!(weekday_number(1, 3), Some(0.0));
        assert_eq!(weekday_number(3, 0), None);
        assert_eq!(weekday_number(3, 4), None);
    }

    #[test]
    fn mixer_is_deterministic() {
        assert_eq!(mix64(0), mix64(0));
        assert_eq!(mix64(1_700_000_000_000), mix64(1_700_000_000_000));
        assert_ne!(mix64(0), mix64(1));
        assert_ne!(draw(1_700_000_000_000, 0), draw(1_700_000_001_000, 0));
        // Same clock, different arguments -> different draws.
        assert_ne!(draw(1_700_000_000_000, 7), draw(1_700_000_000_000, 8));
    }

    #[test]
    fn draws_stay_in_unit_interval() {
        assert_eq!(unit_float(0), 0.0);
        assert!(unit_float(u64::MAX) < 1.0);
        for i in 0..1_000i64 {
            let x = draw(1_700_000_000_000 + i, i as u64);
            assert!((0.0..1.0).contains(&x), "draw out of range: {}", x);
        }
    }
}
