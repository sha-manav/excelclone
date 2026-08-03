//! Expression evaluation: coercions, operators, error propagation.
//!
//! Excel semantics implemented here (v1):
//! - Empty cells coerce to 0 in arithmetic and "" in concatenation.
//! - Booleans coerce to 1/0 in arithmetic; numeric text coerces to numbers.
//! - Comparisons order values as number < text < bool; text case-insensitive.
//! - Errors propagate through operators and (almost all) functions.
//! - A range used in scalar context is #VALUE! (no implicit intersection in
//!   v1; documented in DECISIONS.md).

use crate::addr::{CellAddr, RangeAddr};
use crate::ast::{BinOp, Expr};
use crate::model::{CellKey, SheetId, Workbook};
use crate::value::{ErrorKind, Value};
use std::cmp::Ordering;

#[derive(Clone, Copy)]
pub struct EvalCtx<'a> {
    pub wb: &'a Workbook,
    /// Sheet the evaluated formula lives on (for unqualified refs).
    pub sheet: SheetId,
    /// The cell being evaluated. `ROW()` and `COLUMN()` with no argument are
    /// asking about it, and nothing else in the evaluator needs to know where
    /// it is — which is why it took until the reference functions to appear.
    pub at: CellAddr,
    /// Injected clock (ms since Unix epoch) so NOW/TODAY are replayable.
    pub now_ms: i64,
    /// Names in scope, innermost last. `LET` pushes onto this; nothing else
    /// writes to it yet, and defined names will when they arrive.
    ///
    /// A slice rather than a map because a LET has a handful of bindings and
    /// the later ones shadow the earlier ones, which a reverse scan gives for
    /// free.
    pub bindings: &'a [(String, Value)],
}

/// A rectangular block of computed values with no home on the grid.
///
/// This is what a dynamic-array function returns and what an operator
/// produces from a range. It is deliberately *not* a range: the values were
/// computed, so there are no addresses to hand back, and a function that
/// wants to read them must be given them rather than told where to look.
#[derive(Debug, Clone, PartialEq)]
pub struct Array {
    pub rows: u32,
    pub cols: u32,
    /// Row-major, `rows * cols` long.
    pub values: Vec<Value>,
}

impl Array {
    pub fn new(rows: u32, cols: u32, values: Vec<Value>) -> Array {
        debug_assert_eq!(values.len(), (rows as usize) * (cols as usize));
        Array { rows, cols, values }
    }

    pub fn scalar(v: Value) -> Array {
        Array {
            rows: 1,
            cols: 1,
            values: vec![v],
        }
    }

    /// One column, top to bottom — the shape most dynamic-array functions
    /// produce.
    pub fn column(values: Vec<Value>) -> Array {
        Array {
            rows: values.len() as u32,
            cols: 1,
            values,
        }
    }

    pub fn row(values: Vec<Value>) -> Array {
        Array {
            rows: 1,
            cols: values.len() as u32,
            values,
        }
    }

    pub fn empty_error(k: ErrorKind) -> Array {
        Array::scalar(Value::Error(k))
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn is_single(&self) -> bool {
        self.rows == 1 && self.cols == 1
    }

    pub fn at(&self, row: u32, col: u32) -> Value {
        if row >= self.rows || col >= self.cols {
            return Value::Error(ErrorKind::NA);
        }
        self.values[(row * self.cols + col) as usize].clone()
    }

    /// Rows of values, the shape `eval_grid` hands to functions.
    pub fn grid(&self) -> Vec<Vec<Value>> {
        (0..self.rows)
            .map(|r| (0..self.cols).map(|c| self.at(r, c)).collect())
            .collect()
    }

    /// The element an operator should use at this position when broadcasting.
    ///
    /// A 1x1 array is the same value everywhere; a single row repeats down and
    /// a single column repeats across, which is what makes `A1:A3*B1:C1` a
    /// 3x2 block. Anything else out of range is `#N/A`, as Excel says.
    fn broadcast(&self, row: u32, col: u32) -> Value {
        let r = if self.rows == 1 { 0 } else { row };
        let c = if self.cols == 1 { 0 } else { col };
        self.at(r, c)
    }
}

/// The result of evaluating a sub-expression: a scalar, a range reference, or
/// a computed block.
pub enum Operand {
    Scalar(Value),
    Range { sheet: SheetId, range: RangeAddr },
    Array(Array),
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
            Expr::Name(n) => Operand::Scalar(self.lookup_name(n)),
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
            // A few functions produce a *reference* rather than a value, so
            // `SUM(OFFSET(A1,0,0,3,1))` has a range to sum. They are asked
            // first and fall back to the ordinary value path, which is what
            // keeps `OFFSET` usable in both positions.
            Expr::Func(name, args) => crate::functions::call_operand(self, name, args)
                .unwrap_or_else(|| Operand::Scalar(crate::functions::call(self, name, args))),
            // An operator over anything wider than one cell works element by
            // element and produces a block. That is what makes
            // `(A1:A3>2)*1` — and, once it spills, `A1:A3*2` — mean what
            // Excel means by it.
            Expr::Binary(op, l, r) => {
                let a = self.eval_operand(l);
                let b = self.eval_operand(r);
                match self.zip(*op, a, b) {
                    Some(arr) => Operand::Array(arr),
                    None => Operand::Scalar(self.eval_binary(*op, l, r)),
                }
            }
            Expr::Neg(e) => self.map_unary(e, |n| finite(-n)),
            Expr::Pos(e) => self.eval_operand(e),
            Expr::Percent(e) => self.map_unary(e, |n| finite(n / 100.0)),
        }
    }

    /// Apply a numeric unary operator across whatever the argument turned
    /// out to be, so `-A1:A3` is a block and `-A1` is not.
    fn map_unary(&self, e: &Expr, f: impl Fn(f64) -> Value) -> Operand {
        let op = self.eval_operand(e);
        match self.widen(op) {
            Ok(arr) => Operand::Array(Array::new(
                arr.rows,
                arr.cols,
                arr.values
                    .iter()
                    .map(|v| match to_number(v) {
                        Ok(n) => f(n),
                        Err(k) => Value::Error(k),
                    })
                    .collect(),
            )),
            Err(v) => Operand::Scalar(match to_number(&v) {
                Ok(n) => f(n),
                Err(k) => Value::Error(k),
            }),
        }
    }

    /// An operand as an array when it covers more than one cell, or as the
    /// single value it is otherwise.
    fn widen(&self, op: Operand) -> Result<Array, Value> {
        match op {
            Operand::Scalar(v) => Err(v),
            Operand::Array(a) if a.is_single() => Err(a.values[0].clone()),
            Operand::Array(a) => Ok(a),
            Operand::Range { sheet, range } => {
                if range.start == range.end {
                    return Err(self.wb.value(CellKey {
                        sheet,
                        addr: range.start,
                    }));
                }
                let grid = self.range_grid(sheet, range);
                let rows = grid.len() as u32;
                let cols = grid.first().map(|r| r.len()).unwrap_or(0) as u32;
                Ok(Array::new(rows, cols, grid.into_iter().flatten().collect()))
            }
        }
    }

    /// Combine two operands element by element, or None when both are single
    /// values and the ordinary scalar path applies.
    fn zip(&self, op: BinOp, a: Operand, b: Operand) -> Option<Array> {
        let (a, b) = (self.widen(a), self.widen(b));
        if a.is_err() && b.is_err() {
            return None;
        }
        let shape = |x: &Result<Array, Value>| match x {
            Ok(arr) => (arr.rows, arr.cols),
            Err(_) => (1, 1),
        };
        let (ar, ac) = shape(&a);
        let (br, bc) = shape(&b);
        // Excel broadcasts a single row down and a single column across, and
        // pads the rest with #N/A rather than refusing — a 3x1 against a 2x1
        // gives three rows, the last of which is #N/A.
        let rows = ar.max(br);
        let cols = ac.max(bc);
        let pick = |x: &Result<Array, Value>, r: u32, c: u32| match x {
            Ok(arr) => arr.broadcast(r, c),
            Err(v) => v.clone(),
        };
        let mut values = Vec::with_capacity((rows * cols) as usize);
        for r in 0..rows {
            for c in 0..cols {
                values.push(apply_binary(op, pick(&a, r, c), pick(&b, r, c)));
            }
        }
        Some(Array::new(rows, cols, values))
    }

    /// The value bound to a name, or `#NAME?` when nothing bound it.
    ///
    /// Innermost first, so `LET(x,1,LET(x,2,x))` is 2 — the inner binding
    /// shadows rather than collides.
    pub fn lookup_name(&self, name: &str) -> Value {
        self.bindings
            .iter()
            .rev()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
            .unwrap_or(Value::Error(ErrorKind::Name))
    }

    /// The same context with one more name in scope.
    pub fn with_bindings<'b>(&self, bindings: &'b [(String, Value)]) -> EvalCtx<'b>
    where
        'a: 'b,
    {
        EvalCtx {
            wb: self.wb,
            sheet: self.sheet,
            at: self.at,
            now_ms: self.now_ms,
            bindings,
        }
    }

    /// Evaluate an argument as a block: a range becomes its cells, a scalar
    /// a 1x1 block, and a computed block itself.
    pub fn eval_array(&self, e: &Expr) -> Array {
        match self.widen(self.eval_operand(e)) {
            Ok(a) => a,
            Err(v) => Array::scalar(v),
        }
    }

    /// Evaluate to a scalar; ranges collapse to #VALUE! in v1.
    pub fn eval_scalar(&self, e: &Expr) -> Value {
        self.scalar_of(self.eval_operand(e))
    }

    /// Read an operand as a scalar, degrading a one-cell range to its value.
    ///
    /// Separate from `eval_scalar` because a reference-returning function has
    /// an operand in hand already; going back through the expression would
    /// evaluate it twice, and for a volatile function that is not merely
    /// wasteful.
    pub fn scalar_of(&self, op: Operand) -> Value {
        match op {
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
            // Same rule as a range: one cell degrades, anything larger in
            // scalar position is #VALUE!. Implicit intersection is out of
            // scope, and guessing would be worse than saying so.
            Operand::Array(a) => {
                if a.is_single() {
                    a.values.into_iter().next().unwrap_or(Value::Empty)
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
        apply_binary(op, self.eval_scalar(l), self.eval_scalar(r))
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
            // A computed block has no addresses, so there is no range to
            // report — which is exactly why INDEX and MATCH ask for one and
            // get None here.
            Operand::Array(a) => (a.grid(), None),
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

/// Combine two values with a binary operator.
///
/// A free function rather than a method because the element-wise path needs
/// it per pair, with the values already in hand: going back through the
/// expression for each element would evaluate the whole operand once per
/// cell.
pub fn apply_binary(op: BinOp, a: Value, b: Value) -> Value {
    match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow => {
            let a = match to_number(&a) {
                Ok(n) => n,
                Err(k) => return Value::Error(k),
            };
            let b = match to_number(&b) {
                Ok(n) => n,
                Err(k) => return Value::Error(k),
            };
            match op {
                BinOp::Add => finite(a + b),
                BinOp::Sub => finite(a - b),
                BinOp::Mul => finite(a * b),
                BinOp::Div => {
                    if b == 0.0 {
                        Value::Error(ErrorKind::Div0)
                    } else {
                        finite(a / b)
                    }
                }
                BinOp::Pow => {
                    if a == 0.0 && b == 0.0 {
                        return Value::Error(ErrorKind::Num);
                    }
                    finite(a.powf(b))
                }
                _ => unreachable!(),
            }
        }
        BinOp::Concat => {
            let a = match to_text(&a) {
                Ok(s) => s,
                Err(k) => return Value::Error(k),
            };
            let b = match to_text(&b) {
                Ok(s) => s,
                Err(k) => return Value::Error(k),
            };
            Value::Text(a + &b)
        }
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
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

/// A number, or `#NUM!` when the arithmetic left the real line.
///
/// Infinity and NaN are not spreadsheet values: Excel answers an overflow with
/// `#NUM!`, and a cell displaying `inf` would travel from the grid into the
/// state snapshot, the event log and the exported dataset. Found by the parity
/// harness on `=1E+308*10`, which every operator except `^` was happy to
/// return.
fn finite(n: f64) -> Value {
    if n.is_finite() {
        Value::Number(n)
    } else {
        Value::Error(ErrorKind::Num)
    }
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
