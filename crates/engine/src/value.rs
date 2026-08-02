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

/// General number formatting: integers without decimal point, up to 15
/// significant digits, no trailing zeros.
pub fn format_number_general(n: f64) -> String {
    if n == n.trunc() && n.abs() < 1e15 {
        // Integral values print without a decimal point.
        format!("{}", n as i64)
    } else {
        let mut out = format!("{}", n);
        if out.contains('e') {
            out = format!("{:E}", n);
        }
        out
    }
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
