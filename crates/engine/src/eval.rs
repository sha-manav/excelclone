//! Expression evaluation: coercions, operators, error propagation.
//!
//! Excel semantics implemented here (v1):
//! - Empty cells coerce to 0 in arithmetic and "" in concatenation.
//! - Booleans coerce to 1/0 in arithmetic; numeric text coerces to numbers.
//! - Comparisons order values as number < text < bool; text case-insensitive.
//! - Errors propagate through operators and (almost all) functions.
//! - A range used in scalar context is #VALUE! (no implicit intersection in
//!   v1; documented in DECISIONS.md).

use crate::addr::RangeAddr;
use crate::ast::{BinOp, Expr};
use crate::model::{CellKey, SheetId, Workbook};
use crate::value::{ErrorKind, Value};
use std::cmp::Ordering;

#[derive(Clone, Copy)]
pub struct EvalCtx<'a> {
    pub wb: &'a Workbook,
    /// Sheet the evaluated formula lives on (for unqualified refs).
    pub sheet: SheetId,
    /// Injected clock (ms since Unix epoch) so NOW/TODAY are replayable.
    pub now_ms: i64,
}

/// The result of evaluating a sub-expression: a scalar or a range reference.
pub enum Operand {
    Scalar(Value),
    Range { sheet: SheetId, range: RangeAddr },
}

impl<'a> EvalCtx<'a> {
    pub fn resolve_sheet(&self, name: &Option<String>) -> Result<SheetId, ErrorKind> {
        match name {
            None => Ok(self.sheet),
            Some(n) => self.wb.sheet_id_by_name(n).ok_or(ErrorKind::Ref),
        }
    }

    pub fn eval_operand(&self, e: &Expr) -> Operand {
        match e {
            Expr::Number(n) => Operand::Scalar(Value::Number(*n)),
            Expr::Text(s) => Operand::Scalar(Value::Text(s.clone())),
            Expr::Bool(b) => Operand::Scalar(Value::Bool(*b)),
            Expr::Error(k) => Operand::Scalar(Value::Error(*k)),
            Expr::Cell(c) => match self.resolve_sheet(&c.sheet) {
                Err(k) => Operand::Scalar(Value::Error(k)),
                Ok(sid) => {
                    let key = CellKey {
                        sheet: sid,
                        addr: c.r.addr(),
                    };
                    Operand::Scalar(self.wb.value(key))
                }
            },
            Expr::Range(r) => match self.resolve_sheet(&r.sheet) {
                Err(k) => Operand::Scalar(Value::Error(k)),
                Ok(sid) => Operand::Range {
                    sheet: sid,
                    range: RangeAddr::new(r.start.addr(), r.end.addr()),
                },
            },
            Expr::Func(name, args) => Operand::Scalar(crate::functions::call(self, name, args)),
            Expr::Binary(op, l, r) => Operand::Scalar(self.eval_binary(*op, l, r)),
            Expr::Neg(e) => Operand::Scalar(match self.eval_number(e) {
                Ok(n) => Value::Number(-n),
                Err(k) => Value::Error(k),
            }),
            Expr::Pos(e) => Operand::Scalar(self.eval_scalar(e)),
            Expr::Percent(e) => Operand::Scalar(match self.eval_number(e) {
                Ok(n) => Value::Number(n / 100.0),
                Err(k) => Value::Error(k),
            }),
        }
    }

    /// Evaluate to a scalar; ranges collapse to #VALUE! in v1.
    pub fn eval_scalar(&self, e: &Expr) -> Value {
        match self.eval_operand(e) {
            Operand::Scalar(v) => v,
            Operand::Range { sheet, range } => {
                // Single-cell range degrades gracefully.
                if range.start == range.end {
                    self.wb.value(CellKey {
                        sheet,
                        addr: range.start,
                    })
                } else {
                    Value::Error(ErrorKind::Value)
                }
            }
        }
    }

    pub fn eval_number(&self, e: &Expr) -> Result<f64, ErrorKind> {
        to_number(&self.eval_scalar(e))
    }

    pub fn eval_text(&self, e: &Expr) -> Result<String, ErrorKind> {
        to_text(&self.eval_scalar(e))
    }

    pub fn eval_bool(&self, e: &Expr) -> Result<bool, ErrorKind> {
        to_bool(&self.eval_scalar(e))
    }

    fn eval_binary(&self, op: BinOp, l: &Expr, r: &Expr) -> Value {
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow => {
                let a = match self.eval_number(l) {
                    Ok(n) => n,
                    Err(k) => return Value::Error(k),
                };
                let b = match self.eval_number(r) {
                    Ok(n) => n,
                    Err(k) => return Value::Error(k),
                };
                match op {
                    BinOp::Add => Value::Number(a + b),
                    BinOp::Sub => Value::Number(a - b),
                    BinOp::Mul => Value::Number(a * b),
                    BinOp::Div => {
                        if b == 0.0 {
                            Value::Error(ErrorKind::Div0)
                        } else {
                            Value::Number(a / b)
                        }
                    }
                    BinOp::Pow => {
                        if a == 0.0 && b == 0.0 {
                            return Value::Error(ErrorKind::Num);
                        }
                        let p = a.powf(b);
                        if p.is_finite() {
                            Value::Number(p)
                        } else {
                            Value::Error(ErrorKind::Num)
                        }
                    }
                    _ => unreachable!(),
                }
            }
            BinOp::Concat => {
                let a = match self.eval_text(l) {
                    Ok(s) => s,
                    Err(k) => return Value::Error(k),
                };
                let b = match self.eval_text(r) {
                    Ok(s) => s,
                    Err(k) => return Value::Error(k),
                };
                Value::Text(a + &b)
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let a = self.eval_scalar(l);
                let b = self.eval_scalar(r);
                if let Some(k) = a.as_error() {
                    return Value::Error(k);
                }
                if let Some(k) = b.as_error() {
                    return Value::Error(k);
                }
                let ord = compare_values(&a, &b);
                Value::Bool(match op {
                    BinOp::Eq => ord == Ordering::Equal,
                    BinOp::Ne => ord != Ordering::Equal,
                    BinOp::Lt => ord == Ordering::Less,
                    BinOp::Le => ord != Ordering::Greater,
                    BinOp::Gt => ord == Ordering::Greater,
                    BinOp::Ge => ord != Ordering::Less,
                    _ => unreachable!(),
                })
            }
        }
    }

    /// Dense rows × cols view of a range, including empty cells. Used by
    /// lookup and conditional-aggregation functions, which need positional
    /// alignment rather than "populated cells only".
    pub fn range_grid(&self, sheet: SheetId, range: RangeAddr) -> Vec<Vec<Value>> {
        let Some(s) = self.wb.sheet(sheet) else {
            return vec![vec![Value::Error(ErrorKind::Ref)]];
        };
        (range.start.row..=range.end.row)
            .map(|r| {
                (range.start.col..=range.end.col)
                    .map(|c| s.value(crate::addr::CellAddr::new(r, c)))
                    .collect()
            })
            .collect()
    }

    /// Evaluate an argument as a dense grid: a range yields its cells, any
    /// scalar yields a 1×1 grid. Also returns the range it came from, when
    /// the argument was a reference (needed by INDEX/MATCH-style functions).
    pub fn eval_grid(&self, e: &Expr) -> (Vec<Vec<Value>>, Option<(SheetId, RangeAddr)>) {
        match self.eval_operand(e) {
            Operand::Scalar(v) => (vec![vec![v]], None),
            Operand::Range { sheet, range } => {
                (self.range_grid(sheet, range), Some((sheet, range)))
            }
        }
    }

    /// Iterate populated cells of a range in deterministic (row, col) order.
    pub fn range_values(&self, sheet: SheetId, range: RangeAddr) -> Vec<Value> {
        let Some(s) = self.wb.sheet(sheet) else {
            return vec![Value::Error(ErrorKind::Ref)];
        };
        let mut keys: Vec<_> = s
            .cells
            .keys()
            .filter(|a| range.contains(**a))
            .copied()
            .collect();
        keys.sort();
        keys.into_iter().map(|a| s.value(a)).collect()
    }
}

/// Coerce to number: bools -> 1/0, numeric text parses, empty -> 0.
pub fn to_number(v: &Value) -> Result<f64, ErrorKind> {
    match v {
        Value::Number(n) => Ok(*n),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Empty => Ok(0.0),
        Value::Error(k) => Err(*k),
        Value::Text(s) => parse_number_text(s).ok_or(ErrorKind::Value),
    }
}

/// Parse text as a number the way cell entry does: trimmed, decimal,
/// scientific, percent suffix.
pub fn parse_number_text(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(stripped) = t.strip_suffix('%') {
        return stripped.trim().parse::<f64>().ok().map(|n| n / 100.0);
    }
    t.parse::<f64>().ok().filter(|n| n.is_finite())
}

pub fn to_text(v: &Value) -> Result<String, ErrorKind> {
    match v {
        Value::Error(k) => Err(*k),
        other => Ok(other.display()),
    }
}

pub fn to_bool(v: &Value) -> Result<bool, ErrorKind> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Number(n) => Ok(*n != 0.0),
        Value::Empty => Ok(false),
        Value::Error(k) => Err(*k),
        Value::Text(s) => {
            if s.eq_ignore_ascii_case("TRUE") {
                Ok(true)
            } else if s.eq_ignore_ascii_case("FALSE") {
                Ok(false)
            } else {
                Err(ErrorKind::Value)
            }
        }
    }
}

/// Excel comparison ordering: number < text < bool; text case-insensitive;
/// Empty coerces to the other side's zero value. Errors handled by caller.
pub fn compare_values(a: &Value, b: &Value) -> Ordering {
    fn rank(v: &Value) -> u8 {
        match v {
            Value::Number(_) => 0,
            Value::Text(_) => 1,
            Value::Bool(_) => 2,
            _ => 0,
        }
    }
    match (a, b) {
        (Value::Empty, Value::Empty) => Ordering::Equal,
        (Value::Empty, other) => match other {
            Value::Number(n) => cmp_f64(0.0, *n),
            Value::Text(s) => cmp_text("", s),
            Value::Bool(b) => (false).cmp(b),
            _ => Ordering::Equal,
        },
        (other, Value::Empty) => compare_values(&Value::Empty, other).reverse(),
        (Value::Number(x), Value::Number(y)) => cmp_f64(*x, *y),
        (Value::Text(x), Value::Text(y)) => cmp_text(x, y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (x, y) => rank(x).cmp(&rank(y)),
    }
}

fn cmp_f64(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

fn cmp_text(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
}
