//! Cell values and Excel error kinds.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorKind {
    Div0,
    Value,
    Ref,
    Name,
    NA,
    Num,
    Circ,
    /// A dynamic array had nowhere to land: something is already sitting in
    /// the cells it would have filled.
    Spill,
    /// A block calculation produced nothing — FILTER matched no rows, UNIQUE
    /// was handed nothing. An empty block is not a value, so Excel says this
    /// rather than showing a blank that looks like a working formula.
    Calc,
}

impl ErrorKind {
    pub fn code(&self) -> &'static str {
        match self {
            ErrorKind::Div0 => "#DIV/0!",
            ErrorKind::Value => "#VALUE!",
            ErrorKind::Ref => "#REF!",
            ErrorKind::Name => "#NAME?",
            ErrorKind::NA => "#N/A",
            ErrorKind::Num => "#NUM!",
            ErrorKind::Circ => "#CIRC!",
            ErrorKind::Spill => "#SPILL!",
            ErrorKind::Calc => "#CALC!",
        }
    }

    pub fn from_code(s: &str) -> Option<ErrorKind> {
        match s.to_ascii_uppercase().as_str() {
            "#DIV/0!" => Some(ErrorKind::Div0),
            "#VALUE!" => Some(ErrorKind::Value),
            "#REF!" => Some(ErrorKind::Ref),
            "#NAME?" => Some(ErrorKind::Name),
            "#N/A" => Some(ErrorKind::NA),
            "#NUM!" => Some(ErrorKind::Num),
            "#CIRC!" => Some(ErrorKind::Circ),
            "#SPILL!" => Some(ErrorKind::Spill),
            "#CALC!" => Some(ErrorKind::Calc),
            _ => None,
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code())
    }
}

/// The computed value of a cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Number(f64),
    Text(String),
    Bool(bool),
    Error(ErrorKind),
    Empty,
}

impl Value {
    pub fn is_empty(&self) -> bool {
        matches!(self, Value::Empty)
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Value::Error(_))
    }

    pub fn as_error(&self) -> Option<ErrorKind> {
        match self {
            Value::Error(e) => Some(*e),
            _ => None,
        }
    }

    /// Display string, matching Excel's General-format conventions closely
    /// enough for v1 (documented in DECISIONS.md).
    pub fn display(&self) -> String {
        match self {
            Value::Number(n) => format_number_general(*n),
            Value::Text(s) => s.clone(),
            Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
            Value::Error(e) => e.code().to_string(),
            Value::Empty => String::new(),
        }
    }
}

/// General number formatting: integers without a decimal point, otherwise at
/// most 15 significant digits with no trailing zeros.
///
/// The 15-digit cap is what makes Gridline agree with Excel on ordinary
/// arithmetic. A double cannot represent 0.1 exactly, so `0.1*3` is really
/// 0.30000000000000004; Excel rounds display to 15 significant digits and
/// shows `0.3`. Printing the shortest round-trip representation instead would
/// expose float noise on almost every decimal a user ever sees.
pub fn format_number_general(n: f64) -> String {
    if n == n.trunc() && n.abs() < 1e15 {
        // Integral values print without a decimal point.
        return format!("{}", n as i64);
    }
    // Round to 15 significant digits, then print the shortest representation
    // of *that* value.
    let rounded: f64 = format!("{:.14e}", n).parse().unwrap_or(n);
    if rounded == rounded.trunc() && rounded.abs() < 1e15 {
        return format!("{}", rounded as i64);
    }
    let mut out = format!("{}", rounded);
    if out.contains('e') {
        out = format!("{:E}", rounded);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_round_trip() {
        for e in [
            ErrorKind::Div0,
            ErrorKind::Value,
            ErrorKind::Ref,
            ErrorKind::Name,
            ErrorKind::NA,
            ErrorKind::Num,
            ErrorKind::Circ,
        ] {
            assert_eq!(ErrorKind::from_code(e.code()), Some(e));
        }
    }

    #[test]
    fn general_format_matches_excel_at_15_significant_digits() {
        // All Excel-verified. Without the 15-digit cap the first three would
        // show their raw float representation instead.
        assert_eq!(format_number_general(0.1 * 3.0), "0.3");
        assert_eq!(format_number_general(0.1 + 0.2), "0.3");
        assert_eq!(format_number_general(1.1 * 3.0), "3.3");
        assert_eq!(format_number_general(1.0 / 3.0), "0.333333333333333");
        assert_eq!(format_number_general(2.0 / 3.0), "0.666666666666667");
        assert_eq!(format_number_general(1.5), "1.5");
        assert_eq!(format_number_general(-2.25), "-2.25");
        assert_eq!(format_number_general(42.0), "42");
        assert_eq!(format_number_general(-0.0), "0");
        // Rounding at the cap must not turn a fraction into a bogus integer.
        assert_eq!(format_number_general(0.9999999999999999), "1");
    }

    #[test]
    fn display_values() {
        assert_eq!(Value::Number(42.0).display(), "42");
        assert_eq!(Value::Number(1.5).display(), "1.5");
        assert_eq!(Value::Number(-3.0).display(), "-3");
        assert_eq!(Value::Bool(true).display(), "TRUE");
        assert_eq!(Value::Text("hi".into()).display(), "hi");
        assert_eq!(Value::Empty.display(), "");
        assert_eq!(Value::Error(ErrorKind::Div0).display(), "#DIV/0!");
    }
}
