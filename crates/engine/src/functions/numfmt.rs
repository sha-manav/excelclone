//! Minimal Excel number-format engine: the display half of TEXT (and, later,
//! of cell formatting).
//!
//! Only the common codes are implemented for v1: digit placeholders, the
//! thousands separator, percent, a currency prefix, quoted literals, and the
//! date/time codes. Anything richer (multi-section codes, colors, conditions,
//! fractions, scientific notation) renders as General instead of erroring —
//! this is display only, so failing soft beats blocking a formula.

use crate::serial::{serial_to_date, serial_to_hms};
use crate::value::{ErrorKind, Value};
use chrono::Datelike;

const MONTHS_FULL: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

const MONTHS_ABBR: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

const DAYS_FULL: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

const DAYS_ABBR: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Format a value with an Excel format code. Returns the display string.
pub fn format_value(v: &Value, code: &str) -> Result<String, ErrorKind> {
    if let Value::Error(k) = v {
        return Err(*k);
    }
    let trimmed = code.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("general") {
        return Ok(v.display());
    }
    // Bracketed colours and conditions ([Red], [<100]) are still out of
    // scope; anything containing one falls through to General.
    if code.contains('[') {
        return Ok(v.display());
    }
    let sections = split_sections(code);
    if sections.len() > 1 {
        return format_sectioned(v, &sections);
    }
    if has_date_tokens(code) {
        let serial = value_to_serial(v)?;
        let toks = resolve_minutes(tokenize_date(code));
        return render_date(serial, &toks);
    }
    if let Some(pat) = parse_num_pattern(code) {
        let n = match v {
            Value::Number(n) => *n,
            Value::Empty => 0.0,
            // Excel leaves text and logical values alone rather than erroring
            // when a numeric code is applied to them.
            other => return Ok(other.display()),
        };
        return Ok(render_number(n, &pat));
    }
    Ok(v.display())
}

/// A date code needs a real number; date text is accepted as a convenience so
/// TEXT("2020-01-01", "mmm") behaves like Excel's implicit coercion.
fn value_to_serial(v: &Value) -> Result<f64, ErrorKind> {
    match v {
        Value::Number(n) => Ok(*n),
        Value::Text(s) => crate::eval::parse_number_text(s)
            .or_else(|| crate::serial::parse_date_text(s))
            .ok_or(ErrorKind::Value),
        _ => Err(ErrorKind::Value),
    }
}

/// Split a code on its unquoted, unescaped semicolons.
///
/// A `;` inside `"..."` or after a backslash is a literal character, not a
/// section break — which is how `0;"; not a section"` stays one section.
fn split_sections(code: &str) -> Vec<String> {
    let cs: Vec<char> = code.chars().collect();
    let mut out = vec![String::new()];
    let mut i = 0;
    while i < cs.len() {
        match cs[i] {
            '"' => {
                let start = i;
                i += 1;
                while i < cs.len() && cs[i] != '"' {
                    i += 1;
                }
                i = (i + 1).min(cs.len());
                out.last_mut().expect("a section").extend(&cs[start..i]);
            }
            '\\' => {
                let end = (i + 2).min(cs.len());
                out.last_mut().expect("a section").extend(&cs[i..end]);
                i = end;
            }
            ';' => {
                out.push(String::new());
                i += 1;
            }
            c => {
                out.last_mut().expect("a section").push(c);
                i += 1;
            }
        }
    }
    out
}

/// Apply a multi-section code.
///
/// Excel reads the sections as positive; negative; zero; text, and a code with
/// fewer than four says less: two sections mean [positive and zero] and
/// [negative], three add a separate zero. The negative section formats the
/// *absolute* value, which is the whole point of `0;(0)` — the parentheses
/// carry the sign, so a minus as well would say it twice.
fn format_sectioned(v: &Value, sections: &[String]) -> Result<String, ErrorKind> {
    // The text section, when there is one, applies to text and nothing else.
    if let Value::Text(t) = v {
        return Ok(match sections.get(3) {
            Some(code) => code.replace('@', t),
            None => t.clone(),
        });
    }
    let n = match v {
        Value::Number(n) => *n,
        Value::Empty => 0.0,
        other => return Ok(other.display()),
    };

    let (code, magnitude) = if n < 0.0 && sections.len() >= 2 {
        // Two or more sections: the negative one takes the magnitude.
        (&sections[1], n.abs())
    } else if n == 0.0 && sections.len() >= 3 {
        (&sections[2], n)
    } else {
        (&sections[0], n)
    };

    // An empty section means "show nothing", which is how `0;;` hides zeros.
    if code.trim().is_empty() {
        return Ok(String::new());
    }
    // A section with no digit placeholder is a literal: `0;(0);"zero"` shows
    // the word rather than the number. Without this it would fall through to
    // General and print `0`, which is the value the section exists to hide.
    if !code.chars().any(|c| matches!(c, '0' | '#' | '?')) && !has_date_tokens(code) {
        return Ok(unquote_literal(code));
    }
    format_value(&Value::Number(magnitude), code)
}

/// A format section's literal text, with its quoting removed.
fn unquote_literal(code: &str) -> String {
    let cs: Vec<char> = code.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < cs.len() {
        match cs[i] {
            '"' => {
                i += 1;
                while i < cs.len() && cs[i] != '"' {
                    out.push(cs[i]);
                    i += 1;
                }
                i += 1;
            }
            '\\' => {
                if let Some(c) = cs.get(i + 1) {
                    out.push(*c);
                }
                i += 2;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn has_date_tokens(code: &str) -> bool {
    let cs: Vec<char> = code.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        match cs[i] {
            '"' => {
                i += 1;
                while i < cs.len() && cs[i] != '"' {
                    i += 1;
                }
                i += 1;
            }
            '\\' => i += 2,
            c if matches!(c.to_ascii_lowercase(), 'y' | 'm' | 'd' | 'h' | 's') => return true,
            _ => i += 1,
        }
    }
    false
}

// ---------------------------------------------------------------- date codes

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// 4 or 2 digits.
    Year(usize),
    /// `mm` / `m`, still ambiguous between month and minute.
    MonthOrMinute(bool),
    MonthNum(bool),
    MonthAbbr,
    MonthFull,
    Day(bool),
    DayAbbr,
    DayFull,
    Hour(bool),
    Minute(bool),
    Second(bool),
    AmPm,
    Lit(String),
}

fn starts_ampm(cs: &[char], i: usize) -> bool {
    let want = ['a', 'm', '/', 'p', 'm'];
    cs.len() >= i + want.len() && (0..want.len()).all(|k| cs[i + k].to_ascii_lowercase() == want[k])
}

fn tokenize_date(code: &str) -> Vec<Tok> {
    let cs: Vec<char> = code.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        let lc = c.to_ascii_lowercase();
        match lc {
            '"' => {
                let mut s = String::new();
                i += 1;
                while i < cs.len() && cs[i] != '"' {
                    s.push(cs[i]);
                    i += 1;
                }
                if i < cs.len() {
                    i += 1;
                }
                out.push(Tok::Lit(s));
            }
            '\\' => {
                i += 1;
                if i < cs.len() {
                    out.push(Tok::Lit(cs[i].to_string()));
                    i += 1;
                }
            }
            'a' if starts_ampm(&cs, i) => {
                out.push(Tok::AmPm);
                i += 5;
            }
            'y' | 'm' | 'd' | 'h' | 's' => {
                let mut n = 0;
                while i + n < cs.len() && cs[i + n].to_ascii_lowercase() == lc {
                    n += 1;
                }
                i += n;
                out.push(match lc {
                    'y' => Tok::Year(if n >= 3 { 4 } else { 2 }),
                    'm' => match n {
                        1 | 2 => Tok::MonthOrMinute(n == 2),
                        3 => Tok::MonthAbbr,
                        _ => Tok::MonthFull,
                    },
                    'd' => match n {
                        1 => Tok::Day(false),
                        2 => Tok::Day(true),
                        3 => Tok::DayAbbr,
                        _ => Tok::DayFull,
                    },
                    'h' => Tok::Hour(n >= 2),
                    _ => Tok::Second(n >= 2),
                });
            }
            _ => {
                out.push(Tok::Lit(c.to_string()));
                i += 1;
            }
        }
    }
    out
}

/// Excel's disambiguation rule: `m`/`mm` means minutes when it directly
/// follows an hour token or directly precedes a seconds token (separators in
/// between don't count), and months otherwise.
fn resolve_minutes(toks: Vec<Tok>) -> Vec<Tok> {
    let mut out = Vec::with_capacity(toks.len());
    for (i, t) in toks.iter().enumerate() {
        match t {
            Tok::MonthOrMinute(padded) => {
                let prev = toks[..i].iter().rev().find(|t| !matches!(t, Tok::Lit(_)));
                let next = toks[i + 1..].iter().find(|t| !matches!(t, Tok::Lit(_)));
                let minutes =
                    matches!(prev, Some(Tok::Hour(_))) || matches!(next, Some(Tok::Second(_)));
                out.push(if minutes {
                    Tok::Minute(*padded)
                } else {
                    Tok::MonthNum(*padded)
                });
            }
            other => out.push(other.clone()),
        }
    }
    out
}

fn render_date(serial: f64, toks: &[Tok]) -> Result<String, ErrorKind> {
    let needs_date = toks.iter().any(|t| {
        matches!(
            t,
            Tok::Year(_)
                | Tok::MonthNum(_)
                | Tok::MonthAbbr
                | Tok::MonthFull
                | Tok::Day(_)
                | Tok::DayAbbr
                | Tok::DayFull
        )
    });
    let date = if needs_date {
        Some(serial_to_date(serial).ok_or(ErrorKind::Value)?)
    } else {
        None
    };
    let (year, month, day, weekday) = match date {
        Some(d) => (
            d.year(),
            d.month() as usize,
            d.day(),
            d.weekday().num_days_from_sunday() as usize,
        ),
        None => (0, 1, 1, 0),
    };
    let (hour, minute, second) = serial_to_hms(serial);
    let ampm = toks.iter().any(|t| matches!(t, Tok::AmPm));
    let shown_hour = if ampm {
        if hour % 12 == 0 {
            12
        } else {
            hour % 12
        }
    } else {
        hour
    };

    let mut out = String::new();
    for t in toks {
        match t {
            Tok::Year(4) => out.push_str(&format!("{:04}", year)),
            Tok::Year(_) => out.push_str(&format!("{:02}", year % 100)),
            Tok::MonthNum(true) => out.push_str(&format!("{:02}", month)),
            Tok::MonthNum(false) => out.push_str(&month.to_string()),
            Tok::MonthAbbr => out.push_str(MONTHS_ABBR[month - 1]),
            Tok::MonthFull => out.push_str(MONTHS_FULL[month - 1]),
            Tok::Day(true) => out.push_str(&format!("{:02}", day)),
            Tok::Day(false) => out.push_str(&day.to_string()),
            Tok::DayAbbr => out.push_str(DAYS_ABBR[weekday]),
            Tok::DayFull => out.push_str(DAYS_FULL[weekday]),
            Tok::Hour(true) => out.push_str(&format!("{:02}", shown_hour)),
            Tok::Hour(false) => out.push_str(&shown_hour.to_string()),
            Tok::Minute(true) => out.push_str(&format!("{:02}", minute)),
            Tok::Minute(false) => out.push_str(&minute.to_string()),
            Tok::Second(true) => out.push_str(&format!("{:02}", second)),
            Tok::Second(false) => out.push_str(&second.to_string()),
            Tok::AmPm => out.push_str(if hour < 12 { "AM" } else { "PM" }),
            Tok::MonthOrMinute(_) => {}
            Tok::Lit(s) => out.push_str(s),
        }
    }
    Ok(out)
}

// ------------------------------------------------------------- number codes

#[derive(Debug, Default, PartialEq)]
struct NumPattern {
    prefix: String,
    suffix: String,
    /// Minimum integer digits (count of `0` left of the point).
    int_zeros: usize,
    /// Mandatory decimals (`0`) and optional ones (`#`).
    dec_zeros: usize,
    dec_hashes: usize,
    thousands: bool,
    percent: u32,
}

fn is_placeholder(cs: &[char], i: usize) -> bool {
    matches!(cs.get(i), Some(c) if *c == '0' || *c == '#')
}

fn parse_num_pattern(code: &str) -> Option<NumPattern> {
    let cs: Vec<char> = code.chars().collect();
    let mut p = NumPattern::default();
    let mut seen_placeholder = false;
    let mut past_int = false;
    let mut in_dec = false;
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        match c {
            '"' => {
                let mut s = String::new();
                i += 1;
                while i < cs.len() && cs[i] != '"' {
                    s.push(cs[i]);
                    i += 1;
                }
                if i < cs.len() {
                    i += 1;
                }
                if past_int {
                    p.suffix.push_str(&s);
                } else {
                    p.prefix.push_str(&s);
                }
            }
            '\\' => {
                i += 1;
                if i < cs.len() {
                    if past_int {
                        p.suffix.push(cs[i]);
                    } else {
                        p.prefix.push(cs[i]);
                    }
                    i += 1;
                }
            }
            '0' | '#' => {
                seen_placeholder = true;
                past_int = true;
                if in_dec {
                    if c == '0' {
                        p.dec_zeros += 1;
                    } else {
                        p.dec_hashes += 1;
                    }
                } else if c == '0' {
                    p.int_zeros += 1;
                }
                i += 1;
            }
            '.' if !in_dec => {
                in_dec = true;
                past_int = true;
                i += 1;
            }
            // A comma between digit placeholders is the thousands separator.
            ',' if past_int && !in_dec && is_placeholder(&cs, i + 1) => {
                p.thousands = true;
                i += 1;
            }
            '%' => {
                p.percent += 1;
                if past_int {
                    p.suffix.push('%');
                } else {
                    p.prefix.push('%');
                }
                i += 1;
            }
            _ => {
                if past_int {
                    p.suffix.push(c);
                } else {
                    p.prefix.push(c);
                }
                i += 1;
            }
        }
    }
    if seen_placeholder {
        Some(p)
    } else {
        None
    }
}

/// Excel rounds half away from zero, with a small snap so decimal-looking
/// halves stored as binary floats (2.675) round the way users expect.
fn round_half_away(x: f64) -> f64 {
    let y = x.abs();
    let fl = y.floor();
    let r = if y - fl >= 0.5 - 1e-9 { fl + 1.0 } else { fl };
    r.copysign(x)
}

fn group_thousands(digits: &str) -> String {
    let n = digits.len();
    let mut out = String::with_capacity(n + n / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (n - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn render_number(n: f64, p: &NumPattern) -> String {
    let mut x = n;
    for _ in 0..p.percent {
        x *= 100.0;
    }
    let max_dec = p.dec_zeros + p.dec_hashes;
    let factor = 10f64.powi(max_dec as i32);
    let scaled = round_half_away(x.abs() * factor);

    let mut digits = format!("{:.0}", scaled);
    if digits.len() < max_dec + 1 {
        digits = format!("{}{}", "0".repeat(max_dec + 1 - digits.len()), digits);
    }
    let split = digits.len() - max_dec;
    let mut int_digits = digits[..split].trim_start_matches('0').to_string();
    let mut frac = digits[split..].to_string();

    // Trailing `#` decimals are optional.
    while frac.len() > p.dec_zeros && frac.ends_with('0') {
        frac.pop();
    }
    if int_digits.len() < p.int_zeros {
        int_digits = format!(
            "{}{}",
            "0".repeat(p.int_zeros - int_digits.len()),
            int_digits
        );
    }
    if int_digits.is_empty() && frac.is_empty() {
        int_digits.push('0');
    }

    let int_out = if p.thousands {
        group_thousands(&int_digits)
    } else {
        int_digits
    };
    let sign = if x < 0.0 && scaled != 0.0 { "-" } else { "" };
    let point = if frac.is_empty() { "" } else { "." };
    format!(
        "{}{}{}{}{}{}",
        sign, p.prefix, int_out, point, frac, p.suffix
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(n: f64, code: &str) -> String {
        format_value(&Value::Number(n), code).expect("formats")
    }

    #[test]
    fn general_and_unsupported_fall_back_to_display() {
        assert_eq!(fmt(5.0, "General"), "5");
        assert_eq!(fmt(5.0, "general"), "5");
        assert_eq!(fmt(1.5, ""), "1.5");
        // Colours and conditions are still out of scope; a half-understood
        // `[Red]0.00` would be worse than falling back to General. Sections
        // *are* implemented now — see `section_tests`.
        assert_eq!(fmt(5.0, "[Red]0.00"), "5");
        assert_eq!(fmt(5.0, "0.00;(0.00)"), "5.00");
    }

    #[test]
    fn digit_placeholders() {
        assert_eq!(fmt(5.0, "0"), "5");
        assert_eq!(fmt(5.0, "00"), "05");
        assert_eq!(fmt(5.0, "0.00"), "5.00");
        assert_eq!(fmt(1234.5, "#,##0"), "1,235");
        assert_eq!(fmt(1234.5, "#,##0.00"), "1,234.50");
        assert_eq!(fmt(1234567.0, "#,##0.000"), "1,234,567.000");
        assert_eq!(fmt(0.0, "0.00"), "0.00");
        assert_eq!(fmt(1.5, "0.##"), "1.5");
        assert_eq!(fmt(1.0, "0.##"), "1");
    }

    #[test]
    fn rounds_half_away_from_zero() {
        assert_eq!(fmt(0.5, "0"), "1");
        assert_eq!(fmt(1.5, "0"), "2");
        assert_eq!(fmt(2.5, "0"), "3");
        assert_eq!(fmt(2.675, "0.00"), "2.68");
        assert_eq!(fmt(-2.5, "0"), "-3");
    }

    #[test]
    fn negatives_and_currency() {
        assert_eq!(fmt(-1234.5, "#,##0.00"), "-1,234.50");
        assert_eq!(fmt(1234.5, "$#,##0.00"), "$1,234.50");
        assert_eq!(fmt(-1234.5, "$#,##0.00"), "-$1,234.50");
        // Rounds to zero: no phantom minus sign.
        assert_eq!(fmt(-0.001, "0.00"), "0.00");
    }

    #[test]
    fn percent_codes() {
        assert_eq!(fmt(0.5, "0%"), "50%");
        assert_eq!(fmt(0.12345, "0.00%"), "12.35%");
        assert_eq!(fmt(1.0, "0%"), "100%");
    }

    #[test]
    fn quoted_literals() {
        assert_eq!(fmt(5.0, "0.00\" kg\""), "5.00 kg");
        assert_eq!(fmt(5.0, "\"~\"0.00"), "~5.00");
    }

    #[test]
    fn date_codes() {
        // 43831 == 2020-01-01 (a Wednesday), 45351 == 2024-02-29.
        assert_eq!(fmt(43831.0, "yyyy-mm-dd"), "2020-01-01");
        assert_eq!(fmt(45351.0, "yyyy-mm-dd"), "2024-02-29");
        assert_eq!(fmt(43831.0, "m/d/yyyy"), "1/1/2020");
        assert_eq!(fmt(43831.0, "d-mmm-yyyy"), "1-Jan-2020");
        assert_eq!(fmt(43831.0, "mmm yyyy"), "Jan 2020");
        assert_eq!(fmt(43831.0, "mmmm d, yyyy"), "January 1, 2020");
        assert_eq!(fmt(43831.0, "dddd"), "Wednesday");
        assert_eq!(fmt(43831.0, "ddd"), "Wed");
        assert_eq!(fmt(43831.0, "yy"), "20");
    }

    #[test]
    fn time_codes_and_minute_disambiguation() {
        assert_eq!(fmt(0.75, "hh:mm:ss"), "18:00:00");
        assert_eq!(fmt(0.75, "h:mm AM/PM"), "6:00 PM");
        assert_eq!(fmt(0.5, "h:mm AM/PM"), "12:00 PM");
        assert_eq!(fmt(0.0, "h:mm AM/PM"), "12:00 AM");
        assert_eq!(fmt(43831.5, "yyyy-mm-dd hh:mm"), "2020-01-01 12:00");
        // Bare mm next to a date token is the month.
        assert_eq!(fmt(43831.0, "mm"), "01");
    }

    #[test]
    fn bad_input_for_date_codes() {
        assert_eq!(
            format_value(&Value::Text("abc".into()), "yyyy-mm-dd"),
            Err(ErrorKind::Value)
        );
        // Serial 0 has no calendar date.
        assert_eq!(
            format_value(&Value::Number(0.0), "yyyy-mm-dd"),
            Err(ErrorKind::Value)
        );
        assert_eq!(
            format_value(&Value::Bool(true), "yyyy"),
            Err(ErrorKind::Value)
        );
    }

    #[test]
    fn non_numeric_values_pass_through_numeric_codes() {
        assert_eq!(
            format_value(&Value::Text("abc".into()), "0.00"),
            Ok("abc".to_string())
        );
        assert_eq!(
            format_value(&Value::Bool(true), "0.00"),
            Ok("TRUE".to_string())
        );
        assert_eq!(format_value(&Value::Empty, "0.00"), Ok("0.00".to_string()));
        assert_eq!(
            format_value(&Value::Error(ErrorKind::Div0), "0.00"),
            Err(ErrorKind::Div0)
        );
    }

    #[test]
    fn pattern_parsing() {
        let p = parse_num_pattern("$#,##0.00").expect("pattern");
        assert_eq!(p.prefix, "$");
        assert_eq!(p.int_zeros, 1);
        assert_eq!(p.dec_zeros, 2);
        assert!(p.thousands);
        assert_eq!(p.percent, 0);
        assert!(parse_num_pattern("abc").is_none());
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(group_thousands("1"), "1");
        assert_eq!(group_thousands("123"), "123");
        assert_eq!(group_thousands("1234"), "1,234");
        assert_eq!(group_thousands("1234567"), "1,234,567");
    }
}

#[cfg(test)]
mod section_tests {
    use super::*;

    fn text(n: f64, code: &str) -> String {
        format_value(&Value::Number(n), code).expect("formats")
    }

    #[test]
    fn a_semicolon_inside_quotes_is_not_a_section_break() {
        // Otherwise `0" items; each"` would split into two sections and the
        // negative branch would take a piece of the positive one's text.
        assert_eq!(split_sections(r#"0" items; each""#).len(), 1);
        assert_eq!(split_sections(r"0\;0").len(), 1);
        assert_eq!(split_sections("0;(0)").len(), 2);
        assert_eq!(split_sections("0;(0);-;@").len(), 4);
    }

    #[test]
    fn the_negative_section_formats_the_magnitude() {
        // The parentheses carry the sign; printing a minus as well would say
        // it twice, which is exactly what the code exists to avoid.
        assert_eq!(text(-5.0, "0;(0)"), "(5)");
        assert_eq!(text(5.0, "0;(0)"), "5");
        assert_eq!(text(-1234.5, "$#,##0.00;($#,##0.00)"), "($1,234.50)");
    }

    #[test]
    fn a_single_section_still_prints_the_sign() {
        assert_eq!(text(-5.0, "0"), "-5");
        assert_eq!(text(-0.5, "0%"), "-50%");
    }

    #[test]
    fn zero_goes_with_the_positives_until_there_are_three_sections() {
        assert_eq!(text(0.0, "0;(0)"), "0");
        assert_eq!(text(0.0, r#"0;(0);"zero""#), "zero");
        assert_eq!(text(0.0, "0;(0);"), "", "an empty section hides the value");
    }

    #[test]
    fn the_fourth_section_belongs_to_text_and_nothing_else() {
        let t = Value::Text("abc".into());
        assert_eq!(format_value(&t, "0;(0);0;@ (text)").unwrap(), "abc (text)");
        // With no text section the text passes through untouched.
        assert_eq!(format_value(&t, "0;(0)").unwrap(), "abc");
        // ...and a number never reaches the text section.
        assert_eq!(text(-5.0, "0;(0);0;@"), "(5)");
    }

    #[test]
    fn a_bracketed_code_is_still_left_alone() {
        // Colours and conditions are out of scope, and a half-understood
        // `[Red]-0` would be worse than General.
        assert_eq!(text(-5.0, "[Red]0;[Blue]0"), "-5");
    }
}
