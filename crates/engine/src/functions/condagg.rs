//! Conditional aggregation: COUNTIF(S), SUMIF(S), AVERAGEIF(S).
//!
//! These functions align cells by position, so every range argument is read as
//! a dense grid (`eval_grid`) that includes empty cells rather than as the
//! "populated cells only" list the plain aggregates use.
//!
//! Expected values in tests are Excel-verified (Microsoft 365).

use super::{expect_args, num_result};
use crate::addr::{CellAddr, RangeAddr, MAX_COLS, MAX_ROWS};
use crate::ast::Expr;
use crate::eval::{compare_values, parse_number_text, EvalCtx};
use crate::model::SheetId;
use crate::value::{ErrorKind, Value};
use std::cmp::Ordering;

// ---------------------------------------------------------------------------
// Criteria engine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Op {
    fn test(self, ord: Ordering) -> bool {
        match self {
            Op::Eq => ord == Ordering::Equal,
            Op::Ne => ord != Ordering::Equal,
            Op::Lt => ord == Ordering::Less,
            Op::Le => ord != Ordering::Greater,
            Op::Gt => ord == Ordering::Greater,
            Op::Ge => ord != Ordering::Less,
        }
    }
}

/// What the criteria compares against, after coercion of the criteria text.
#[derive(Debug, Clone)]
enum Target {
    /// The bare "=" / "<>" / "" forms, which test emptiness rather than a value.
    Blank,
    Number(f64),
    Bool(bool),
    /// Under `=`/`<>` this is a wildcard pattern; ordering operators compare it
    /// as plain (case-insensitive) text.
    Text(String),
    Error(ErrorKind),
}

/// A parsed Excel criteria expression such as ">100", "<>x", "apple", "*ple".
#[derive(Debug, Clone)]
pub struct Criteria {
    op: Op,
    target: Target,
}

impl Criteria {
    /// Parse a criteria value (the criteria argument is evaluated first).
    pub fn parse(v: &Value) -> Criteria {
        match v {
            Value::Text(s) => Criteria::parse_text(s),
            Value::Number(n) => Criteria {
                op: Op::Eq,
                target: Target::Number(*n),
            },
            Value::Bool(b) => Criteria {
                op: Op::Eq,
                target: Target::Bool(*b),
            },
            Value::Error(k) => Criteria {
                op: Op::Eq,
                target: Target::Error(*k),
            },
            Value::Empty => Criteria {
                op: Op::Eq,
                target: Target::Blank,
            },
        }
    }

    fn parse_text(s: &str) -> Criteria {
        let (op, rest) = split_op(s);
        let target = if rest.is_empty() {
            match op {
                // "=" matches blanks, "<>" matches non-blanks; an ordering
                // operator with nothing after it compares against "".
                Op::Eq | Op::Ne => Target::Blank,
                _ => Target::Text(String::new()),
            }
        } else if let Some(n) = parse_number_text(rest) {
            Target::Number(n)
        } else if rest.eq_ignore_ascii_case("TRUE") {
            Target::Bool(true)
        } else if rest.eq_ignore_ascii_case("FALSE") {
            Target::Bool(false)
        } else {
            Target::Text(rest.to_string())
        };
        Criteria { op, target }
    }

    /// Test a candidate cell value against this criteria.
    pub fn matches(&self, v: &Value) -> bool {
        if let Target::Blank = self.target {
            return if self.op == Op::Ne {
                !v.is_empty()
            } else {
                v.is_empty()
            };
        }
        if let Target::Error(k) = self.target {
            let same = matches!(v, Value::Error(e) if *e == k);
            return if self.op == Op::Ne { !same } else { same };
        }
        // A criteria with a value never matches an empty cell; error cells are
        // propagated by the callers rather than matched.
        if v.is_empty() || v.is_error() {
            return false;
        }
        match &self.target {
            // Excel compares only within a type: ">1" ignores text cells, and
            // "apple" ignores numeric ones. "<>" is the inverse, so a type
            // mismatch satisfies it.
            Target::Number(n) => match v {
                Value::Number(_) => self.op.test(compare_values(v, &Value::Number(*n))),
                _ => self.op == Op::Ne,
            },
            Target::Bool(b) => match v {
                Value::Bool(_) => self.op.test(compare_values(v, &Value::Bool(*b))),
                _ => self.op == Op::Ne,
            },
            Target::Text(t) => match v {
                Value::Text(s) => match self.op {
                    Op::Eq => wildcard_matches(t, s),
                    Op::Ne => !wildcard_matches(t, s),
                    _ => self.op.test(compare_values(v, &Value::Text(t.clone()))),
                },
                _ => self.op == Op::Ne,
            },
            Target::Blank | Target::Error(_) => unreachable!(),
        }
    }
}

/// Split a leading comparison operator off a criteria string. Two-character
/// operators must be tried first.
fn split_op(s: &str) -> (Op, &str) {
    if let Some(r) = s.strip_prefix(">=") {
        (Op::Ge, r)
    } else if let Some(r) = s.strip_prefix("<=") {
        (Op::Le, r)
    } else if let Some(r) = s.strip_prefix("<>") {
        (Op::Ne, r)
    } else if let Some(r) = s.strip_prefix('>') {
        (Op::Gt, r)
    } else if let Some(r) = s.strip_prefix('<') {
        (Op::Lt, r)
    } else if let Some(r) = s.strip_prefix('=') {
        (Op::Eq, r)
    } else {
        (Op::Eq, s)
    }
}

/// Case-insensitive glob match: `*` is any run, `?` is exactly one character,
/// `~` escapes the next character when it is `*`, `?` or `~` (otherwise `~` is
/// a literal). Iterative with backtracking so patterns like "*a*b*" are linear
/// in practice and never recurse.
/// Excel's `*` and `?` matching, shared with XMATCH's wildcard mode so the
/// two cannot disagree about what a pattern means.
pub(super) fn wildcard_matches(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.to_lowercase().chars().collect();
    let txt: Vec<char> = text.to_lowercase().chars().collect();
    let mut p = 0usize;
    let mut t = 0usize;
    // usize::MAX means "no `*` seen yet", so there is nothing to backtrack to.
    let mut star_p = usize::MAX;
    let mut star_t = 0usize;

    while t < txt.len() {
        if p < pat.len() {
            match pat[p] {
                '*' => {
                    star_p = p;
                    star_t = t;
                    p += 1;
                    continue;
                }
                '?' => {
                    p += 1;
                    t += 1;
                    continue;
                }
                '~' if p + 1 < pat.len() && matches!(pat[p + 1], '*' | '?' | '~') => {
                    if pat[p + 1] == txt[t] {
                        p += 2;
                        t += 1;
                        continue;
                    }
                }
                c => {
                    if c == txt[t] {
                        p += 1;
                        t += 1;
                        continue;
                    }
                }
            }
        }
        if star_p == usize::MAX {
            return false;
        }
        // Let the last `*` swallow one more character and retry from there.
        star_t += 1;
        t = star_t;
        p = star_p + 1;
    }
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

// ---------------------------------------------------------------------------
// Grid plumbing
// ---------------------------------------------------------------------------

/// One (range, criteria) pair, with the range read as a dense grid.
struct Condition {
    grid: Vec<Vec<Value>>,
    criteria: Criteria,
}

fn dims(g: &[Vec<Value>]) -> (usize, usize) {
    (g.len(), g.first().map_or(0, |r| r.len()))
}

fn eval_criteria(ctx: &EvalCtx, e: &Expr) -> Result<Criteria, ErrorKind> {
    let v = ctx.eval_scalar(e);
    match v.as_error() {
        Some(k) => Err(k),
        None => Ok(Criteria::parse(&v)),
    }
}

/// Evaluate `range, criteria, range, criteria, ...`. Every criteria range must
/// have the same dimensions, otherwise #VALUE!.
fn eval_conditions(ctx: &EvalCtx, args: &[Expr]) -> Result<Vec<Condition>, ErrorKind> {
    if args.is_empty() || !args.len().is_multiple_of(2) {
        return Err(ErrorKind::Value);
    }
    let mut out: Vec<Condition> = Vec::with_capacity(args.len() / 2);
    for pair in args.chunks(2) {
        let (grid, _) = ctx.eval_grid(&pair[0]);
        let criteria = eval_criteria(ctx, &pair[1])?;
        out.push(Condition { grid, criteria });
    }
    let shape = dims(&out[0].grid);
    for c in &out[1..] {
        if dims(&c.grid) != shape {
            return Err(ErrorKind::Value);
        }
    }
    Ok(out)
}

/// Like `eval_grid`, but a single-cell reference keeps its address instead of
/// collapsing to a scalar, so `SUMIF(A1:A4, c, B1)` can resize B1 to B1:B4.
fn eval_value_grid(ctx: &EvalCtx, e: &Expr) -> (Vec<Vec<Value>>, Option<(SheetId, RangeAddr)>) {
    if let Expr::Cell(c) = e {
        if let Ok(sid) = ctx.resolve_sheet(&c.sheet) {
            let range = RangeAddr::single(c.r.addr());
            return (ctx.range_grid(sid, range), Some((sid, range)));
        }
    }
    ctx.eval_grid(e)
}

/// Force a grid to `rows` × `cols`. Excel re-anchors a mis-shaped sum/average
/// range at its top-left corner and reads the criteria range's offsets from
/// there; when the argument was a reference we re-read the sheet, otherwise
/// (and past the sheet edge) the missing cells read as empty.
fn reshape(
    ctx: &EvalCtx,
    grid: Vec<Vec<Value>>,
    src: Option<(SheetId, RangeAddr)>,
    rows: usize,
    cols: usize,
) -> Vec<Vec<Value>> {
    if rows == 0 || cols == 0 || dims(&grid) == (rows, cols) {
        return grid;
    }
    match src {
        Some((sheet, range)) => {
            let end_row =
                (range.start.row as u64 + rows as u64 - 1).min(MAX_ROWS as u64 - 1) as u32;
            let end_col =
                (range.start.col as u64 + cols as u64 - 1).min(MAX_COLS as u64 - 1) as u32;
            let widened = ctx.range_grid(
                sheet,
                RangeAddr::new(range.start, CellAddr::new(end_row, end_col)),
            );
            pad(widened, rows, cols)
        }
        None => pad(grid, rows, cols),
    }
}

fn pad(mut g: Vec<Vec<Value>>, rows: usize, cols: usize) -> Vec<Vec<Value>> {
    for row in g.iter_mut() {
        row.resize(cols, Value::Empty);
    }
    g.resize(rows, vec![Value::Empty; cols]);
    g
}

/// Walk every position of the criteria grids, propagating errors found in them
/// and calling `f` where every criteria matches.
fn for_each_match(conds: &[Condition], mut f: impl FnMut(usize, usize)) -> Result<(), ErrorKind> {
    let (rows, cols) = dims(&conds[0].grid);
    for r in 0..rows {
        for c in 0..cols {
            let mut all = true;
            for cond in conds {
                let v = &cond.grid[r][c];
                // Scan every condition even after a miss so range errors still
                // propagate.
                if let Some(k) = v.as_error() {
                    return Err(k);
                }
                if !cond.criteria.matches(v) {
                    all = false;
                }
            }
            if all {
                f(r, c);
            }
        }
    }
    Ok(())
}

/// Shared body of SUMIF(S)/AVERAGEIF(S): the sum of the numeric cells of the
/// value range at matching positions, and how many there were. `value_arg` of
/// `None` aggregates the first criteria range itself.
fn aggregate(
    ctx: &EvalCtx,
    cond_args: &[Expr],
    value_arg: Option<&Expr>,
) -> Result<(f64, usize), ErrorKind> {
    let conds = eval_conditions(ctx, cond_args)?;
    let (rows, cols) = dims(&conds[0].grid);
    let values = match value_arg {
        None => conds[0].grid.clone(),
        Some(e) => {
            let (g, src) = eval_value_grid(ctx, e);
            reshape(ctx, g, src, rows, cols)
        }
    };
    let mut sum = 0.0f64;
    let mut count = 0usize;
    let mut err: Option<ErrorKind> = None;
    for_each_match(&conds, |r, c| match &values[r][c] {
        Value::Number(n) => {
            sum += n;
            count += 1;
        }
        Value::Error(k) if err.is_none() => err = Some(*k),
        _ => {}
    })?;
    match err {
        Some(k) => Err(k),
        None => Ok((sum, count)),
    }
}

// ---------------------------------------------------------------------------
// Functions
// ---------------------------------------------------------------------------

pub fn countif(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 2) {
        return Value::Error(k);
    }
    countifs(ctx, args)
}

pub fn countifs(ctx: &EvalCtx, args: &[Expr]) -> Value {
    let conds = match eval_conditions(ctx, args) {
        Ok(c) => c,
        Err(k) => return Value::Error(k),
    };
    let mut n = 0u64;
    let r = for_each_match(&conds, |_, _| n += 1);
    match r {
        Err(k) => Value::Error(k),
        Ok(()) => Value::Number(n as f64),
    }
}

/// SUMIF(range, criteria, [sum_range]): only numeric cells contribute.
pub fn sumif(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 3) {
        return Value::Error(k);
    }
    match aggregate(ctx, &args[..2], args.get(2)) {
        Err(k) => Value::Error(k),
        Ok((sum, _)) => num_result(Ok(sum)),
    }
}

/// SUMIFS(sum_range, range1, criteria1, ...): note the sum range comes first,
/// unlike SUMIF.
pub fn sumifs(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if args.len() < 3 || args.len().is_multiple_of(2) {
        return Value::Error(ErrorKind::Value);
    }
    match aggregate(ctx, &args[1..], Some(&args[0])) {
        Err(k) => Value::Error(k),
        Ok((sum, _)) => num_result(Ok(sum)),
    }
}

pub fn averageif(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 3) {
        return Value::Error(k);
    }
    average_result(aggregate(ctx, &args[..2], args.get(2)))
}

pub fn averageifs(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if args.len() < 3 || args.len().is_multiple_of(2) {
        return Value::Error(ErrorKind::Value);
    }
    average_result(aggregate(ctx, &args[1..], Some(&args[0])))
}

/// No matching numeric cell is #DIV/0!, as in AVERAGE.
fn average_result(r: Result<(f64, usize), ErrorKind>) -> Value {
    match r {
        Err(k) => Value::Error(k),
        Ok((_, 0)) => Value::Error(ErrorKind::Div0),
        Ok((sum, n)) => num_result(Ok(sum / n as f64)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crit(s: &str) -> Criteria {
        Criteria::parse(&Value::Text(s.to_string()))
    }

    fn num(n: f64) -> Value {
        Value::Number(n)
    }

    fn txt(s: &str) -> Value {
        Value::Text(s.to_string())
    }

    #[test]
    fn numeric_criteria() {
        let c = crit("100");
        assert!(c.matches(&num(100.0)));
        assert!(!c.matches(&num(99.0)));
        // Type-restricted: numeric criteria ignore text and boolean cells.
        assert!(!c.matches(&txt("100")));
        assert!(!c.matches(&Value::Bool(true)));
        assert!(!c.matches(&Value::Empty));

        // A Value::Number criteria behaves identically.
        let c = Criteria::parse(&num(100.0));
        assert!(c.matches(&num(100.0)));
        assert!(!c.matches(&num(0.0)));
    }

    #[test]
    fn percent_and_negative_numbers() {
        assert!(crit("5%").matches(&num(0.05)));
        assert!(crit("<-3").matches(&num(-5.0)));
        assert!(!crit("<-3").matches(&num(-3.0)));
        assert!(crit(">=1e3").matches(&num(1000.0)));
    }

    #[test]
    fn comparison_operators() {
        assert!(crit(">100").matches(&num(150.0)));
        assert!(!crit(">100").matches(&num(100.0)));
        assert!(crit(">=100").matches(&num(100.0)));
        assert!(crit("<100").matches(&num(99.5)));
        assert!(crit("<=100").matches(&num(100.0)));
        assert!(!crit("<=100").matches(&num(100.5)));
        assert!(crit("=100").matches(&num(100.0)));
        assert!(crit("<>100").matches(&num(101.0)));
        assert!(!crit("<>100").matches(&num(100.0)));
        // Ordering never crosses types, and blanks stay out of it.
        assert!(!crit(">100").matches(&txt("zebra")));
        assert!(!crit(">100").matches(&Value::Empty));
        assert!(!crit("<100").matches(&Value::Empty));
    }

    #[test]
    fn not_equal_spans_types() {
        // "<>" is inclusive: a cell of another type satisfies it.
        assert!(crit("<>x").matches(&txt("y")));
        assert!(!crit("<>x").matches(&txt("X")));
        assert!(crit("<>x").matches(&num(5.0)));
        assert!(crit("<>100").matches(&txt("apple")));
        // ...but empty cells still never match a value criteria.
        assert!(!crit("<>x").matches(&Value::Empty));
    }

    #[test]
    fn text_criteria_is_case_insensitive() {
        let c = crit("apple");
        assert!(c.matches(&txt("apple")));
        assert!(c.matches(&txt("APPLE")));
        assert!(c.matches(&txt("ApPlE")));
        assert!(!c.matches(&txt("apples")));
        assert!(!c.matches(&num(1.0)));
        assert!(crit("=apple").matches(&txt("APPLE")));
    }

    #[test]
    fn text_ordering_comparisons() {
        assert!(crit(">apple").matches(&txt("banana")));
        assert!(!crit(">apple").matches(&txt("Apple")));
        assert!(crit(">=apple").matches(&txt("APPLE")));
        assert!(crit("<banana").matches(&txt("apple")));
        // Text ordering ignores numeric cells even though number < text.
        assert!(!crit(">apple").matches(&num(1000.0)));
    }

    #[test]
    fn wildcard_criteria() {
        let c = crit("*ple");
        assert!(c.matches(&txt("apple")));
        assert!(c.matches(&txt("ple")));
        assert!(c.matches(&txt("PINEAPPLE")));
        assert!(!c.matches(&txt("plea")));
        // Wildcards only apply to text cells.
        assert!(!c.matches(&num(1.0)));
        assert!(!c.matches(&Value::Empty));

        assert!(crit("?at").matches(&txt("cat")));
        assert!(!crit("?at").matches(&txt("at")));
        assert!(!crit("?at").matches(&txt("chat")));
        assert!(crit("<>*ple").matches(&txt("banana")));
        assert!(!crit("<>*ple").matches(&txt("apple")));
    }

    #[test]
    fn tilde_escapes_wildcards() {
        assert!(crit("~*").matches(&txt("*")));
        assert!(!crit("~*").matches(&txt("a")));
        assert!(!crit("~*").matches(&txt("ab")));
        assert!(crit("a~*b").matches(&txt("a*b")));
        assert!(!crit("a~*b").matches(&txt("axb")));
        assert!(crit("~?").matches(&txt("?")));
        assert!(!crit("~?").matches(&txt("a")));
    }

    #[test]
    fn blank_criteria() {
        for c in [crit("="), crit(""), Criteria::parse(&Value::Empty)] {
            assert!(c.matches(&Value::Empty));
            assert!(!c.matches(&num(0.0)));
            assert!(!c.matches(&txt("x")));
            // A zero-length string is a value, not a blank cell.
            assert!(!c.matches(&txt("")));
        }
    }

    #[test]
    fn non_blank_criteria() {
        let c = crit("<>");
        assert!(!c.matches(&Value::Empty));
        assert!(c.matches(&num(0.0)));
        assert!(c.matches(&txt("")));
        assert!(c.matches(&txt("x")));
        assert!(c.matches(&Value::Bool(false)));
    }

    #[test]
    fn boolean_criteria() {
        let c = crit("TRUE");
        assert!(c.matches(&Value::Bool(true)));
        assert!(!c.matches(&Value::Bool(false)));
        // Booleans match booleans, not the text "TRUE" or the number 1.
        assert!(!c.matches(&txt("TRUE")));
        assert!(!c.matches(&num(1.0)));
        assert!(crit("false").matches(&Value::Bool(false)));
        assert!(Criteria::parse(&Value::Bool(true)).matches(&Value::Bool(true)));
        assert!(crit("<>TRUE").matches(&Value::Bool(false)));
        assert!(crit("<>TRUE").matches(&num(1.0)));
    }

    #[test]
    fn error_cells_never_match() {
        let e = Value::Error(ErrorKind::NA);
        assert!(!crit("x").matches(&e));
        assert!(!crit(">0").matches(&e));
        assert!(!crit("=").matches(&e));
        assert!(crit("<>").matches(&e));
    }

    #[test]
    fn wildcard_matcher_basics() {
        assert!(wildcard_matches("", ""));
        assert!(!wildcard_matches("", "a"));
        assert!(wildcard_matches("*", ""));
        assert!(wildcard_matches("*", "anything"));
        assert!(wildcard_matches("**", "ab"));
        assert!(wildcard_matches("abc", "ABC"));
        assert!(!wildcard_matches("abc", "abcd"));
        assert!(wildcard_matches("?", "a"));
        assert!(!wildcard_matches("?", ""));
        assert!(!wildcard_matches("?", "ab"));
    }

    #[test]
    fn wildcard_matcher_backtracks() {
        assert!(wildcard_matches("a*c", "abc"));
        assert!(wildcard_matches("a*c", "ac"));
        assert!(!wildcard_matches("a*c", "abd"));
        assert!(wildcard_matches("*a*", "bab"));
        assert!(wildcard_matches("a*b*c", "axxbyyc"));
        assert!(wildcard_matches("a*b*c", "abc"));
        assert!(!wildcard_matches("a*b*c", "acb"));
        assert!(wildcard_matches("*.txt", "notes.TXT"));
        assert!(!wildcard_matches("*.txt", "notes.txtx"));
    }

    #[test]
    fn wildcard_matcher_escapes() {
        assert!(wildcard_matches("~*", "*"));
        assert!(wildcard_matches("~?", "?"));
        assert!(wildcard_matches("~~", "~"));
        assert!(wildcard_matches("a~*b", "a*b"));
        assert!(!wildcard_matches("a~*b", "azzb"));
        // A tilde before an ordinary character is itself a literal.
        assert!(wildcard_matches("a~b", "a~b"));
        assert!(!wildcard_matches("a~b", "ab"));
        // A trailing tilde has nothing to escape.
        assert!(wildcard_matches("a~", "a~"));
    }
}
