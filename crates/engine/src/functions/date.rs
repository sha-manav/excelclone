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
        let (y, m, d) = (y.trunc() as i64, m.trunc() as i64, d.trunc() as i64);
        // Excel's February 1900 has 29 days. Asked for that date by name it
        // answers serial 60, and a workbook that contains it would otherwise
        // read as an error. Only the date named directly is handled: a
        // rollover that *lands* on the phantom day (DATE(1900,1,60)) would
        // need Excel's whole calendar rather than the real one, and is a
        // recorded difference rather than a silent one.
        let named = if y < 1900 { y + 1900 } else { y };
        if (named, m, d) == (1900, 2, 29) {
            return serial::ymd_to_serial(1900, 2, 29).ok_or(ErrorKind::Num);
        }
        let date = date_from_parts(y, m, d).ok_or(ErrorKind::Num)?;
        // Anything before serial 1 is outside Excel's date system.
        serial::date_to_serial(date).ok_or(ErrorKind::Num)
    })())
}

/// YEAR/MONTH/DAY answer from the serial's *components* rather than from a
/// calendar date, so serial 60 reports Excel's 1900-02-29 instead of erroring.
/// Month arithmetic still refuses it — see `serial::serial_to_parts`.
fn serial_part(ctx: &EvalCtx, args: &[Expr], f: impl Fn((i32, u32, u32)) -> f64) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    num_result(
        arg_serial(ctx, &args[0])
            .and_then(|s| serial::serial_to_parts(s).ok_or(ErrorKind::Num))
            .map(f),
    )
}

pub fn year(ctx: &EvalCtx, args: &[Expr]) -> Value {
    serial_part(ctx, args, |(y, _, _)| y as f64)
}

pub fn month(ctx: &EvalCtx, args: &[Expr]) -> Value {
    serial_part(ctx, args, |(_, m, _)| m as f64)
}

pub fn day(ctx: &EvalCtx, args: &[Expr]) -> Value {
    serial_part(ctx, args, |(_, _, d)| d as f64)
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

// ---------------------------------------------------------------------------
// Time of day, and the rest of the date arithmetic
// ---------------------------------------------------------------------------

/// TIME(hour, minute, second): a fraction of a day, always in [0, 1).
///
/// Excel wraps rather than erroring — TIME(25,0,0) is 1am — because the result
/// is a time of day and a time of day has nowhere else to go.
pub fn time(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 3, 3) {
        return Value::Error(k);
    }
    num_result((|| {
        let h = ctx.eval_number(&args[0])?.trunc();
        let m = ctx.eval_number(&args[1])?.trunc();
        let s = ctx.eval_number(&args[2])?.trunc();
        let total = h * 3600.0 + m * 60.0 + s;
        if total < 0.0 {
            return Err(ErrorKind::Num);
        }
        Ok((total % 86_400.0) / 86_400.0)
    })())
}

fn time_part(ctx: &EvalCtx, args: &[Expr], f: impl Fn((u32, u32, u32)) -> f64) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    num_result(arg_serial(ctx, &args[0]).map(|s| f(serial::serial_to_hms(s))))
}

pub fn hour(ctx: &EvalCtx, args: &[Expr]) -> Value {
    time_part(ctx, args, |(h, _, _)| h as f64)
}

pub fn minute(ctx: &EvalCtx, args: &[Expr]) -> Value {
    time_part(ctx, args, |(_, m, _)| m as f64)
}

pub fn second(ctx: &EvalCtx, args: &[Expr]) -> Value {
    time_part(ctx, args, |(_, _, s)| s as f64)
}

/// DATEVALUE(text): the serial for a date written as text.
///
/// Text that is already a number is refused: DATEVALUE("45292") is #VALUE! in
/// Excel, because the function converts a *date* and a bare number is not one.
pub fn datevalue(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    num_result((|| {
        let text = ctx.eval_text(&args[0])?;
        serial::parse_date_text(&text)
            .map(f64::floor)
            .ok_or(ErrorKind::Value)
    })())
}

/// EDATE(start, months): the same day, `months` away, clamped to the month's
/// length — so a month on from 31 January is 28 or 29 February, never March.
pub fn edate(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 2) {
        return Value::Error(k);
    }
    num_result((|| {
        let start = arg_serial(ctx, &args[0])?;
        let months = ctx.eval_number(&args[1])?.trunc() as i64;
        let shifted = serial::add_months(start, months).ok_or(ErrorKind::Num)?;
        serial::date_to_serial(shifted).ok_or(ErrorKind::Num)
    })())
}

/// DAYS(end, start): a signed day count, end minus start.
///
/// Note the argument order — end first, which is the opposite of DATEDIF and
/// of every other two-date function in the language.
pub fn days(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 2) {
        return Value::Error(k);
    }
    num_result((|| {
        let end = arg_serial(ctx, &args[0])?.floor();
        let start = arg_serial(ctx, &args[1])?.floor();
        Ok(end - start)
    })())
}

/// TIMEVALUE(text): the fraction of a day a written time represents.
///
/// The date part of the text is ignored if there is one, which is what makes
/// it the complement of DATEVALUE rather than a competitor to it.
pub fn timevalue(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    num_result((|| {
        let text = ctx.eval_text(&args[0])?;
        parse_time_of_day(text.trim()).ok_or(ErrorKind::Value)
    })())
}

/// `13:45`, `13:45:30`, `1:45 PM`. Returns a fraction in [0, 1).
fn parse_time_of_day(s: &str) -> Option<f64> {
    let upper = s.to_ascii_uppercase();
    let (body, meridiem) = match (upper.strip_suffix("AM"), upper.strip_suffix("PM")) {
        (Some(rest), _) => (rest.trim().to_string(), Some(false)),
        (_, Some(rest)) => (rest.trim().to_string(), Some(true)),
        _ => (upper.clone(), None),
    };
    let parts: Vec<&str> = body.trim().split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let h: u32 = parts[0].trim().parse().ok()?;
    let m: u32 = parts[1].trim().parse().ok()?;
    let sec: u32 = match parts.get(2) {
        Some(p) => p.trim().parse().ok()?,
        None => 0,
    };
    if m > 59 || sec > 59 {
        return None;
    }
    let h = match meridiem {
        // 12 AM is midnight and 12 PM is noon, which is the one place a
        // 12-hour clock is not simply "add twelve".
        Some(pm) => match (h, pm) {
            (12, false) => 0,
            (12, true) => 12,
            (h, true) if h < 12 => h + 12,
            (h, false) if h < 12 => h,
            _ => return None,
        },
        None if h < 24 => h,
        None => return None,
    };
    Some((h * 3600 + m * 60 + sec) as f64 / 86_400.0)
}

/// Dates listed as holidays, as whole-day serials.
fn holiday_serials(ctx: &EvalCtx, arg: Option<&Expr>) -> Result<Vec<f64>, ErrorKind> {
    let Some(e) = arg else { return Ok(Vec::new()) };
    Ok(super::gather_numbers(ctx, std::slice::from_ref(e))?
        .into_iter()
        .map(f64::floor)
        .collect())
}

/// Whether a serial is a working day, Monday to Friday.
///
/// Asked of the calendar date, which is where `WEEKDAY` asks too. The first
/// version of this counted `serial mod 7` and was a day out — Excel's serial
/// line contains a phantom 1900-02-29, so a modulus over it does not line up
/// with the weekday the rest of the engine reports. Two mappings for one
/// question is how they end up disagreeing; the parity harness caught it on
/// `WORKDAY(Friday, 1)`, which answered Sunday.
///
/// A serial with no calendar date — 60, the phantom day — is not a day
/// anybody can work on, so it is not a working day either.
fn is_workday(serial: f64) -> bool {
    serial::serial_to_date(serial)
        .map(|d| d.weekday().num_days_from_monday() < 5)
        .unwrap_or(false)
}

/// NETWORKDAYS(start, end, [holidays]): whole working days between two dates,
/// counting both ends.
///
/// Negative when the end is before the start, which is Excel's answer and
/// worth reproducing: a schedule that subtracts in the wrong order gets a
/// sign rather than a silent zero.
pub fn networkdays(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 3) {
        return Value::Error(k);
    }
    num_result((|| {
        let a = arg_serial(ctx, &args[0])?.floor();
        let b = arg_serial(ctx, &args[1])?.floor();
        let holidays = holiday_serials(ctx, args.get(2))?;
        let (lo, hi, sign) = if a <= b { (a, b, 1.0) } else { (b, a, -1.0) };
        let mut count = 0.0;
        let mut d = lo;
        while d <= hi {
            if is_workday(d) && !holidays.contains(&d) {
                count += 1.0;
            }
            d += 1.0;
        }
        Ok(count * sign)
    })())
}

/// WORKDAY(start, days, [holidays]): the date that many working days away.
///
/// The start date itself is never counted, in either direction — the answer
/// to "one working day after Friday" is Monday, not Friday.
pub fn workday(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 3) {
        return Value::Error(k);
    }
    num_result((|| {
        let start = arg_serial(ctx, &args[0])?.floor();
        let days = ctx.eval_number(&args[1])?.trunc() as i64;
        let holidays = holiday_serials(ctx, args.get(2))?;
        let step = if days < 0 { -1.0 } else { 1.0 };
        let mut remaining = days.abs();
        let mut d = start;
        // Bounded so a pathological holiday list cannot spin forever; the
        // limit is far beyond Excel's own date range.
        let mut guard = 0;
        while remaining > 0 && guard < 4_000_000 {
            d += step;
            guard += 1;
            if is_workday(d) && !holidays.contains(&d) {
                remaining -= 1;
            }
        }
        if remaining > 0 || d < 1.0 {
            return Err(ErrorKind::Num);
        }
        Ok(d)
    })())
}

/// YEARFRAC(start, end, [basis]): the fraction of a year between two dates.
///
/// The `basis` is a day-count convention, and finance runs on the difference
/// between them: 0 is US 30/360, 1 actual/actual, 2 actual/360, 3 actual/365,
/// 4 European 30/360. A bond priced on the wrong basis is wrong by a few
/// days' interest, every time.
pub fn yearfrac(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 3) {
        return Value::Error(k);
    }
    num_result((|| {
        let a = arg_serial(ctx, &args[0])?.floor();
        let b = arg_serial(ctx, &args[1])?.floor();
        let basis = match args.get(2) {
            Some(e) => ctx.eval_number(e)?.trunc() as i64,
            None => 0,
        };
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let start = serial::serial_to_parts(lo).ok_or(ErrorKind::Num)?;
        let end = serial::serial_to_parts(hi).ok_or(ErrorKind::Num)?;
        match basis {
            0 => Ok(thirty_360(start, end, false) / 360.0),
            4 => Ok(thirty_360(start, end, true) / 360.0),
            2 => Ok((hi - lo) / 360.0),
            3 => Ok((hi - lo) / 365.0),
            1 => {
                // Excel divides by the *average* length of the calendar years
                // the span touches, which is why a leap day inside the range
                // changes the answer for ranges that do not contain one.
                let years = (end.0 - start.0 + 1) as f64;
                let days: f64 = (start.0..=end.0)
                    .map(|y| if is_leap(y) { 366.0 } else { 365.0 })
                    .sum();
                Ok((hi - lo) / (days / years))
            }
            _ => Err(ErrorKind::Num),
        }
    })())
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// The 30/360 day count, in its US and European spellings.
///
/// Both pretend every month has 30 days; they disagree about what to do with
/// a 31st. The European rule simply caps both days at 30; the US rule only
/// caps the end date when the start date was already at 30 or 31, which makes
/// the count asymmetric and is the whole reason the two conventions exist
/// separately.
fn thirty_360(start: (i32, u32, u32), end: (i32, u32, u32), european: bool) -> f64 {
    let (y1, m1, mut d1) = start;
    let (y2, m2, mut d2) = end;
    if european {
        d1 = d1.min(30);
        d2 = d2.min(30);
    } else {
        if d1 == 31 {
            d1 = 30;
        }
        if d2 == 31 && d1 >= 30 {
            d2 = 30;
        }
    }
    ((y2 - y1) as f64) * 360.0
        + ((m2 as i64 - m1 as i64) as f64) * 30.0
        + (d2 as i64 - d1 as i64) as f64
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
