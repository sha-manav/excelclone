//! Math and aggregate functions.
//!
//! Expected values in tests are Excel-verified (Microsoft 365).

use super::{expect_args, gather, gather_numbers, num_result};
use crate::addr::CellAddr;
use crate::ast::Expr;
use crate::eval::EvalCtx;
use crate::value::{ErrorKind, Value};

pub fn sum(ctx: &EvalCtx, args: &[Expr]) -> Value {
    num_result(gather_numbers(ctx, args).map(|ns| ns.iter().sum()))
}

pub fn product(ctx: &EvalCtx, args: &[Expr]) -> Value {
    num_result(gather_numbers(ctx, args).map(|ns| {
        if ns.is_empty() {
            0.0
        } else {
            ns.iter().product()
        }
    }))
}

pub fn average(ctx: &EvalCtx, args: &[Expr]) -> Value {
    match gather_numbers(ctx, args) {
        Err(k) => Value::Error(k),
        Ok(ns) if ns.is_empty() => Value::Error(ErrorKind::Div0),
        Ok(ns) => Value::Number(ns.iter().sum::<f64>() / ns.len() as f64),
    }
}

pub fn min(ctx: &EvalCtx, args: &[Expr]) -> Value {
    num_result(
        gather_numbers(ctx, args)
            .map(|ns| ns.iter().copied().fold(f64::INFINITY, f64::min))
            .map(|m| if m.is_finite() { m } else { 0.0 }),
    )
}

pub fn max(ctx: &EvalCtx, args: &[Expr]) -> Value {
    num_result(
        gather_numbers(ctx, args)
            .map(|ns| ns.iter().copied().fold(f64::NEG_INFINITY, f64::max))
            .map(|m| if m.is_finite() { m } else { 0.0 }),
    )
}

/// COUNT: numbers only; range bools/text ignored; direct args counted when
/// numerically coercible. Never propagates errors.
pub fn count(ctx: &EvalCtx, args: &[Expr]) -> Value {
    let mut n = 0u32;
    for g in gather(ctx, args) {
        let counts = if g.from_range {
            matches!(g.value, Value::Number(_))
        } else {
            crate::eval::to_number(&g.value).is_ok() && !g.value.is_empty()
        };
        if counts {
            n += 1;
        }
    }
    Value::Number(n as f64)
}

/// COUNTA: everything non-empty, including errors and "".
pub fn counta(ctx: &EvalCtx, args: &[Expr]) -> Value {
    let n = gather(ctx, args)
        .iter()
        .filter(|g| !g.value.is_empty())
        .count();
    Value::Number(n as f64)
}

/// COUNTBLANK(range): empty cells plus cells whose value is "".
pub fn countblank(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    match ctx.eval_operand(&args[0]) {
        crate::eval::Operand::Range { sheet, range } => {
            let populated_nonblank = ctx
                .range_values(sheet, range)
                .into_iter()
                .filter(|v| !v.is_empty() && *v != Value::Text(String::new()))
                .count() as u64;
            Value::Number((range.cell_count() - populated_nonblank) as f64)
        }
        crate::eval::Operand::Scalar(v) => {
            let blank = v.is_empty() || v == Value::Text(String::new());
            Value::Number(if blank { 1.0 } else { 0.0 })
        }
        crate::eval::Operand::Array(a) => Value::Number(
            a.values
                .iter()
                .filter(|v| v.is_empty() || **v == Value::Text(String::new()))
                .count() as f64,
        ),
    }
}

/// Excel ROUND: half away from zero, with a small snap so decimal-looking
/// halves stored as binary floats (e.g. 2.675) round the way users expect.
fn round_away(x: f64) -> f64 {
    let y = x.abs();
    let fl = y.floor();
    let frac = y - fl;
    let r = if frac >= 0.5 - 1e-9 { fl + 1.0 } else { fl };
    r.copysign(x)
}

fn digits_factor(d: f64) -> f64 {
    10f64.powi(d.trunc() as i32)
}

pub fn round(ctx: &EvalCtx, args: &[Expr]) -> Value {
    round_impl(ctx, args, round_away)
}

pub fn roundup(ctx: &EvalCtx, args: &[Expr]) -> Value {
    round_impl(ctx, args, |x| {
        let y = x.abs();
        let fl = y.floor();
        let r = if y - fl > 1e-9 { fl + 1.0 } else { fl };
        r.copysign(x)
    })
}

pub fn rounddown(ctx: &EvalCtx, args: &[Expr]) -> Value {
    round_impl(ctx, args, |x| {
        let y = x.abs();
        let fl = y.floor();
        // Snap float artifacts like 4.999999999 back up.
        let r = if 1.0 - (y - fl) < 1e-9 { fl + 1.0 } else { fl };
        r.copysign(x)
    })
}

fn round_impl(ctx: &EvalCtx, args: &[Expr], f: impl Fn(f64) -> f64) -> Value {
    if let Err(k) = expect_args(args, 2, 2) {
        return Value::Error(k);
    }
    num_result((|| {
        let n = ctx.eval_number(&args[0])?;
        let d = ctx.eval_number(&args[1])?;
        let factor = digits_factor(d);
        Ok(f(n * factor) / factor)
    })())
}

pub fn abs(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    num_result(ctx.eval_number(&args[0]).map(f64::abs))
}

/// INT floors toward negative infinity: INT(-1.5) = -2.
pub fn int(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    num_result(ctx.eval_number(&args[0]).map(|n| {
        if (n - n.round()).abs() < 1e-9 {
            n.round()
        } else {
            n.floor()
        }
    }))
}

/// MOD(n, d): result takes the sign of the divisor (Excel semantics).
pub fn mod_fn(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 2) {
        return Value::Error(k);
    }
    match (ctx.eval_number(&args[0]), ctx.eval_number(&args[1])) {
        (Ok(n), Ok(d)) => {
            if d == 0.0 {
                Value::Error(ErrorKind::Div0)
            } else {
                Value::Number(n - d * (n / d).floor())
            }
        }
        (Err(k), _) | (_, Err(k)) => Value::Error(k),
    }
}

pub fn power(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 2) {
        return Value::Error(k);
    }
    match (ctx.eval_number(&args[0]), ctx.eval_number(&args[1])) {
        (Ok(a), Ok(b)) => {
            if a == 0.0 && b == 0.0 {
                Value::Error(ErrorKind::Num)
            } else {
                num_result(Ok(a.powf(b)))
            }
        }
        (Err(k), _) | (_, Err(k)) => Value::Error(k),
    }
}

pub fn sqrt(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    match ctx.eval_number(&args[0]) {
        Ok(n) if n < 0.0 => Value::Error(ErrorKind::Num),
        r => num_result(r.map(f64::sqrt)),
    }
}

// ---------------------------------------------------------------------------
// Rounding to a multiple, and the elementary functions
// ---------------------------------------------------------------------------

/// One numeric argument, evaluated and handed to `f`.
fn unary(ctx: &EvalCtx, args: &[Expr], f: impl Fn(f64) -> Result<f64, ErrorKind>) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    num_result(ctx.eval_number(&args[0]).and_then(f))
}

/// CEILING(number, significance): away from zero to a multiple.
///
/// Excel's rule has three parts and every one of them catches somebody: a
/// significance of 0 gives 0 rather than dividing by zero; a positive number
/// with a negative significance is #NUM!; and a negative number rounds away
/// from zero, so CEILING(-4.2, -1) is -5.
pub fn ceiling(ctx: &EvalCtx, args: &[Expr]) -> Value {
    to_multiple(ctx, args, f64::ceil)
}

/// FLOOR(number, significance): towards zero to a multiple, same rules.
pub fn floor(ctx: &EvalCtx, args: &[Expr]) -> Value {
    to_multiple(ctx, args, f64::floor)
}

fn to_multiple(ctx: &EvalCtx, args: &[Expr], round: impl Fn(f64) -> f64) -> Value {
    if let Err(k) = expect_args(args, 2, 2) {
        return Value::Error(k);
    }
    num_result((|| {
        let n = ctx.eval_number(&args[0])?;
        let sig = ctx.eval_number(&args[1])?;
        if sig == 0.0 {
            return Ok(0.0);
        }
        if n > 0.0 && sig < 0.0 {
            return Err(ErrorKind::Num);
        }
        Ok(round(n / sig) * sig)
    })())
}

/// MROUND(number, multiple): to the *nearest* multiple, halves away from zero.
///
/// Excel refuses a number and a multiple with different signs rather than
/// guessing which one the user meant.
pub fn mround(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 2) {
        return Value::Error(k);
    }
    num_result((|| {
        let n = ctx.eval_number(&args[0])?;
        let m = ctx.eval_number(&args[1])?;
        if m == 0.0 {
            return Ok(0.0);
        }
        if n.signum() != m.signum() {
            return Err(ErrorKind::Num);
        }
        Ok(round_away(n / m) * m)
    })())
}

/// TRUNC(number, [digits]): cut, never round. INT is the one that floors.
pub fn trunc(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 2) {
        return Value::Error(k);
    }
    num_result((|| {
        let n = ctx.eval_number(&args[0])?;
        let digits = match args.get(1) {
            Some(a) => ctx.eval_number(a)?.trunc(),
            None => 0.0,
        };
        let scale = 10f64.powf(digits);
        Ok((n * scale).trunc() / scale)
    })())
}

pub fn sign(ctx: &EvalCtx, args: &[Expr]) -> Value {
    // `f64::signum` gives 1.0 for +0.0, and Excel gives 0.
    unary(ctx, args, |n| Ok(if n == 0.0 { 0.0 } else { n.signum() }))
}

pub fn exp(ctx: &EvalCtx, args: &[Expr]) -> Value {
    unary(ctx, args, |n| Ok(n.exp()))
}

/// LN(number): natural log. Zero and negatives are #NUM!, never -inf or NaN.
pub fn ln(ctx: &EvalCtx, args: &[Expr]) -> Value {
    unary(ctx, args, |n| {
        if n > 0.0 {
            Ok(n.ln())
        } else {
            Err(ErrorKind::Num)
        }
    })
}

pub fn log10(ctx: &EvalCtx, args: &[Expr]) -> Value {
    unary(ctx, args, |n| {
        if n > 0.0 {
            Ok(n.log10())
        } else {
            Err(ErrorKind::Num)
        }
    })
}

/// LOG(number, [base]): base 10 by default, which is why LOG and LN differ.
pub fn log(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 2) {
        return Value::Error(k);
    }
    num_result((|| {
        let n = ctx.eval_number(&args[0])?;
        let base = match args.get(1) {
            Some(a) => ctx.eval_number(a)?,
            None => 10.0,
        };
        if n <= 0.0 || base <= 0.0 || base == 1.0 {
            return Err(ErrorKind::Num);
        }
        Ok(n.log(base))
    })())
}

/// GCD/LCM: whole numbers only, fractions truncated, negatives refused.
pub fn gcd(ctx: &EvalCtx, args: &[Expr]) -> Value {
    whole_number_fold(ctx, args, 0, |a, b| {
        let (mut a, mut b) = (a, b);
        while b != 0 {
            let t = b;
            b = a % b;
            a = t;
        }
        a
    })
}

pub fn lcm(ctx: &EvalCtx, args: &[Expr]) -> Value {
    whole_number_fold(ctx, args, 1, |a, b| {
        if a == 0 || b == 0 {
            return 0;
        }
        let (mut x, mut y) = (a, b);
        while y != 0 {
            let t = y;
            y = x % y;
            x = t;
        }
        a / x * b
    })
}

fn whole_number_fold(
    ctx: &EvalCtx,
    args: &[Expr],
    identity: i64,
    f: impl Fn(i64, i64) -> i64,
) -> Value {
    if args.is_empty() {
        return Value::Error(ErrorKind::Value);
    }
    num_result((|| {
        let mut acc = identity;
        for n in gather_numbers(ctx, args)? {
            if n < 0.0 {
                return Err(ErrorKind::Num);
            }
            acc = f(acc, n.trunc() as i64);
        }
        Ok(acc as f64)
    })())
}

// ---------------------------------------------------------------------------
// Order statistics
// ---------------------------------------------------------------------------

/// Numbers from the arguments, sorted ascending. Errors propagate.
fn sorted_numbers(ctx: &EvalCtx, args: &[Expr]) -> Result<Vec<f64>, ErrorKind> {
    let mut ns = gather_numbers(ctx, args)?;
    ns.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Ok(ns)
}

/// MEDIAN: the middle value, or the mean of the two middle ones.
pub fn median(ctx: &EvalCtx, args: &[Expr]) -> Value {
    num_result((|| {
        let ns = sorted_numbers(ctx, args)?;
        if ns.is_empty() {
            return Err(ErrorKind::Num);
        }
        let mid = ns.len() / 2;
        Ok(if ns.len() % 2 == 1 {
            ns[mid]
        } else {
            (ns[mid - 1] + ns[mid]) / 2.0
        })
    })())
}

/// LARGE(range, k) / SMALL(range, k): the kth from one end, 1-based.
pub fn large(ctx: &EvalCtx, args: &[Expr]) -> Value {
    nth_from_end(ctx, args, true)
}

pub fn small(ctx: &EvalCtx, args: &[Expr]) -> Value {
    nth_from_end(ctx, args, false)
}

fn nth_from_end(ctx: &EvalCtx, args: &[Expr], from_top: bool) -> Value {
    if let Err(k) = expect_args(args, 2, 2) {
        return Value::Error(k);
    }
    num_result((|| {
        let ns = sorted_numbers(ctx, &args[..1])?;
        let k = ctx.eval_number(&args[1])?.trunc();
        // Excel is 1-based and refuses k outside the data with #NUM!, not an
        // empty answer — a silent clamp would hide a broken formula.
        if k < 1.0 || k > ns.len() as f64 {
            return Err(ErrorKind::Num);
        }
        let i = k as usize - 1;
        Ok(if from_top {
            ns[ns.len() - 1 - i]
        } else {
            ns[i]
        })
    })())
}

/// RANK(number, range, [order]): position in the sorted range, 1-based.
///
/// Ties share the best rank and the next one is skipped, which is what makes
/// it a competition ranking rather than a dense one.
pub fn rank(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 3) {
        return Value::Error(k);
    }
    num_result((|| {
        let target = ctx.eval_number(&args[0])?;
        let ns = sorted_numbers(ctx, &args[1..2])?;
        let ascending = match args.get(2) {
            Some(a) => ctx.eval_number(a)? != 0.0,
            None => false,
        };
        if !ns.contains(&target) {
            return Err(ErrorKind::NA);
        }
        let better = if ascending {
            ns.iter().filter(|n| **n < target).count()
        } else {
            ns.iter().filter(|n| **n > target).count()
        };
        Ok(better as f64 + 1.0)
    })())
}

/// SUMPRODUCT(range, ...): multiply the ranges cell by cell, then total.
///
/// The ranges must be the same shape — Excel answers #VALUE! rather than
/// aligning them, because two columns of different length almost always mean
/// one of them is pointing at the wrong rows. Non-numeric cells count as 0,
/// which is what lets SUMPRODUCT be used as a conditional sum: a comparison
/// yields TRUE/FALSE, and multiplying by it selects rows.
pub fn sumproduct(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if args.is_empty() {
        return Value::Error(ErrorKind::Value);
    }
    let mut columns: Vec<Vec<Value>> = Vec::new();
    for a in args {
        match ctx.eval_operand(a) {
            // Flattened row-major, which is the order SUMPRODUCT pairs cells
            // in when the ranges are the same shape.
            crate::eval::Operand::Range { sheet, range } => {
                columns.push(ctx.range_grid(sheet, range).into_iter().flatten().collect())
            }
            // A scalar multiplies every row, which is how SUMPRODUCT(A1:A3, 2)
            // behaves.
            crate::eval::Operand::Scalar(v) => columns.push(vec![v]),
            // A computed block pairs off exactly like a range, which is what
            // makes `SUMPRODUCT((A1:A3>2)*1)` — the idiom that predates
            // COUNTIFS — work at all.
            crate::eval::Operand::Array(a) => columns.push(a.values),
        }
    }
    let len = columns.iter().map(Vec::len).max().unwrap_or(0);
    if columns.iter().any(|c| c.len() != len && c.len() != 1) {
        return Value::Error(ErrorKind::Value);
    }

    let mut total = 0.0;
    for i in 0..len {
        let mut product = 1.0;
        for c in &columns {
            let v = if c.len() == 1 { &c[0] } else { &c[i] };
            // An error anywhere propagates; text and blanks are zero.
            if let Value::Error(k) = v {
                return Value::Error(*k);
            }
            product *= match v {
                Value::Number(n) => *n,
                Value::Bool(b) => {
                    if *b {
                        1.0
                    } else {
                        0.0
                    }
                }
                _ => 0.0,
            };
        }
        total += product;
    }
    Value::Number(total)
}

/// MODE(range): the most common value, earliest on a tie.
///
/// `#N/A` when nothing repeats, which is Excel's way of saying the question
/// has no answer rather than picking the first value arbitrarily.
pub fn mode(ctx: &EvalCtx, args: &[Expr]) -> Value {
    num_result((|| {
        let ns = gather_numbers(ctx, args)?;
        let mut best: Option<(f64, usize)> = None;
        for (i, v) in ns.iter().enumerate() {
            let count = ns.iter().filter(|o| *o == v).count();
            let earlier_index = ns.iter().position(|o| o == v).unwrap_or(i);
            let better = match best {
                None => count > 1,
                Some((bv, bc)) => {
                    count > bc
                        || (count == bc
                            && earlier_index < ns.iter().position(|o| *o == bv).unwrap_or(0))
                }
            };
            if count > 1 && better {
                best = Some((*v, count));
            }
        }
        best.map(|(v, _)| v).ok_or(ErrorKind::NA)
    })())
}

/// STDEV(range): the *sample* standard deviation, dividing by n-1.
///
/// STDEVP is the population one. Excel's plain `STDEV` being the sample
/// estimate surprises people coming from a statistics package, and getting it
/// wrong is a quiet error: the answer is close, just never right.
pub fn stdev(ctx: &EvalCtx, args: &[Expr]) -> Value {
    num_result((|| {
        let ns = gather_numbers(ctx, args)?;
        if ns.len() < 2 {
            return Err(ErrorKind::Div0);
        }
        let mean = ns.iter().sum::<f64>() / ns.len() as f64;
        let variance = ns.iter().map(|n| (n - mean).powi(2)).sum::<f64>() / (ns.len() as f64 - 1.0);
        Ok(variance.sqrt())
    })())
}

/// SUBTOTAL(function_num, ref, ...): an aggregate that skips filtered rows.
///
/// The whole point is that a total under a filtered table shows the total of
/// what is *visible*. Codes 1-11 pick the aggregate; 101-111 are the same
/// aggregates and additionally skip manually hidden rows — Gridline has no
/// manual row hiding yet, so the two behave identically and will diverge when
/// it does.
pub fn subtotal(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, usize::MAX) {
        return Value::Error(k);
    }
    let code = match ctx.eval_number(&args[0]) {
        Ok(n) => n.trunc() as i64,
        Err(k) => return Value::Error(k),
    };
    let mut visible: Vec<f64> = Vec::new();
    let mut non_empty = 0usize;
    for a in &args[1..] {
        match ctx.eval_operand(a) {
            crate::eval::Operand::Range { sheet, range } => {
                let hidden = ctx
                    .wb
                    .sheet(sheet)
                    .map(|s| s.hidden_rows.clone())
                    .unwrap_or_default();
                for row in range.start.row..=range.end.row {
                    if hidden.contains(&row) {
                        continue;
                    }
                    for col in range.start.col..=range.end.col {
                        let v = ctx
                            .wb
                            .sheet(sheet)
                            .map(|s| s.value(CellAddr::new(row, col)));
                        match v {
                            Some(Value::Number(n)) => {
                                visible.push(n);
                                non_empty += 1;
                            }
                            Some(Value::Error(k)) => return Value::Error(k),
                            Some(Value::Empty) | None => {}
                            Some(_) => non_empty += 1,
                        }
                    }
                }
            }
            crate::eval::Operand::Scalar(Value::Error(k)) => return Value::Error(k),
            crate::eval::Operand::Scalar(v) => {
                if let Value::Number(n) = v {
                    visible.push(n);
                    non_empty += 1;
                } else if !v.is_empty() {
                    non_empty += 1;
                }
            }
            // A computed block has no rows on the sheet, so nothing in it can
            // be hidden and all of it counts.
            crate::eval::Operand::Array(a) => {
                for v in a.values {
                    match v {
                        Value::Number(n) => {
                            visible.push(n);
                            non_empty += 1;
                        }
                        Value::Error(k) => return Value::Error(k),
                        Value::Empty => {}
                        _ => non_empty += 1,
                    }
                }
            }
        }
    }

    let n = visible.len() as f64;
    let sum: f64 = visible.iter().sum();
    let mean = if n > 0.0 { sum / n } else { 0.0 };
    let sample_var = || visible.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let population_var = || visible.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;

    // 101-111 mean "also skip manually hidden rows", which is the same set
    // here because manual hiding does not exist yet.
    match code % 100 {
        1 if n > 0.0 => Value::Number(mean),
        1 => Value::Error(ErrorKind::Div0),
        2 => Value::Number(n),
        3 => Value::Number(non_empty as f64),
        4 => Value::Number(
            visible
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max)
                .max(0.0),
        ),
        5 if n > 0.0 => Value::Number(visible.iter().copied().fold(f64::INFINITY, f64::min)),
        5 => Value::Number(0.0),
        6 => Value::Number(if visible.is_empty() {
            0.0
        } else {
            visible.iter().product()
        }),
        7 if n > 1.0 => Value::Number(sample_var().sqrt()),
        8 if n > 0.0 => Value::Number(population_var().sqrt()),
        9 => Value::Number(sum),
        10 if n > 1.0 => Value::Number(sample_var()),
        11 if n > 0.0 => Value::Number(population_var()),
        7 | 8 | 10 | 11 => Value::Error(ErrorKind::Div0),
        _ => Value::Error(ErrorKind::Value),
    }
}

/// AGGREGATE(function_num, options, ref1, [ref2]...)
///
/// SUBTOTAL's larger sibling: nineteen functions instead of eleven, and a
/// second argument saying what to skip. The one people actually reach for it
/// for is option 6 — ignore errors — which is what lets a total survive a
/// column with one #N/A in it.
///
/// The array form, `AGGREGATE(14, 6, array, k)`, takes its k as a final
/// argument; functions 14-19 are the ones that need it.
pub fn aggregate(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if args.len() < 3 {
        return Value::Error(ErrorKind::Value);
    }
    let code = match ctx.eval_number(&args[0]) {
        Ok(n) => n.trunc() as i64,
        Err(k) => return Value::Error(k),
    };
    let options = match ctx.eval_number(&args[1]) {
        Ok(n) => n.trunc() as i64,
        Err(k) => return Value::Error(k),
    };
    if !(1..=19).contains(&code) || !(0..=7).contains(&options) {
        return Value::Error(ErrorKind::Value);
    }
    // Options 2, 3, 6 and 7 ignore errors; the rest let one through, which is
    // the whole difference between AGGREGATE and the plain function.
    let ignore_errors = matches!(options, 2 | 3 | 6 | 7);
    // Options 1, 3, 5 and 7 ignore hidden rows, as SUBTOTAL always does.
    let ignore_hidden = matches!(options, 1 | 3 | 5 | 7);
    // 14-19 take a rank or a quantile as their last argument.
    let takes_k = (14..=19).contains(&code);
    let (refs, k) = if takes_k {
        if args.len() < 4 {
            return Value::Error(ErrorKind::Value);
        }
        let k = match ctx.eval_number(&args[args.len() - 1]) {
            Ok(n) => n,
            Err(e) => return Value::Error(e),
        };
        (&args[2..args.len() - 1], Some(k))
    } else {
        (&args[2..], None)
    };

    let mut numbers: Vec<f64> = Vec::new();
    let mut non_empty = 0usize;
    for a in refs {
        let hidden = match (ignore_hidden, ctx.eval_operand(a)) {
            (true, crate::eval::Operand::Range { sheet, range }) => ctx
                .wb
                .sheet(sheet)
                .map(|s| {
                    (range.start.row..=range.end.row)
                        .filter(|r| s.hidden_rows.contains(r))
                        .collect::<Vec<u32>>()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        for g in super::gather(ctx, std::slice::from_ref(a)) {
            if let Some(e) = g.value.as_error() {
                if ignore_errors {
                    continue;
                }
                return Value::Error(e);
            }
            match g.value {
                Value::Number(n) => {
                    numbers.push(n);
                    non_empty += 1;
                }
                Value::Empty => {}
                _ => non_empty += 1,
            }
        }
        // Hidden rows are dropped after gathering, because `gather` flattens
        // and loses the addresses. Only whole hidden rows inside a single
        // range argument are affected, which is the case the option is for.
        if !hidden.is_empty() {
            if let crate::eval::Operand::Range { sheet, range } = ctx.eval_operand(a) {
                for r in hidden {
                    for c in range.start.col..=range.end.col {
                        if let Some(Value::Number(n)) =
                            ctx.wb.sheet(sheet).map(|s| s.value(CellAddr::new(r, c)))
                        {
                            if let Some(pos) = numbers.iter().position(|x| *x == n) {
                                numbers.remove(pos);
                            }
                        }
                    }
                }
            }
        }
    }

    let n = numbers.len() as f64;
    let sum: f64 = numbers.iter().sum();
    let mean = if n > 0.0 { sum / n } else { 0.0 };
    let sample_var = || numbers.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let population_var = || numbers.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    let mut sorted = numbers.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    match code {
        1 if n > 0.0 => Value::Number(mean),
        2 => Value::Number(n),
        3 => Value::Number(non_empty as f64),
        4 if n > 0.0 => Value::Number(sorted[sorted.len() - 1]),
        5 if n > 0.0 => Value::Number(sorted[0]),
        6 => Value::Number(if numbers.is_empty() {
            0.0
        } else {
            numbers.iter().product()
        }),
        7 if n > 1.0 => Value::Number(sample_var().sqrt()),
        8 if n > 0.0 => Value::Number(population_var().sqrt()),
        9 => Value::Number(sum),
        10 if n > 1.0 => Value::Number(sample_var()),
        11 if n > 0.0 => Value::Number(population_var()),
        12 if n > 0.0 => Value::Number(median_of(&sorted)),
        13 if n > 0.0 => match mode_of(&numbers) {
            Some(m) => Value::Number(m),
            None => Value::Error(ErrorKind::NA),
        },
        14 | 15 => {
            let k = k.unwrap_or(0.0).trunc();
            if k < 1.0 || k > n {
                return Value::Error(ErrorKind::Num);
            }
            let i = k as usize - 1;
            Value::Number(if code == 14 {
                sorted[sorted.len() - 1 - i]
            } else {
                sorted[i]
            })
        }
        // 16 and 17 are the inclusive percentile and quartile, 18 and 19 the
        // exclusive ones. A quartile's argument is 0-4 and becomes a
        // fraction; a percentile's already is one.
        16..=19 => {
            let raw = k.unwrap_or(0.0);
            let q = if code == 17 || code == 19 {
                if !(0.0..=4.0).contains(&raw) {
                    return Value::Error(ErrorKind::Num);
                }
                raw.trunc() / 4.0
            } else {
                raw
            };
            if !(0.0..=1.0).contains(&q) || n == 0.0 {
                return Value::Error(ErrorKind::Num);
            }
            let exclusive = code == 18 || code == 19;
            match percentile_of(&sorted, q, exclusive) {
                Some(v) => Value::Number(v),
                None => Value::Error(ErrorKind::Num),
            }
        }
        // Every remaining arm is a function whose inputs were empty.
        1 | 4 | 5 | 12 | 13 => Value::Error(ErrorKind::Div0),
        _ => Value::Error(ErrorKind::Div0),
    }
}

fn median_of(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

fn mode_of(values: &[f64]) -> Option<f64> {
    let mut best: Option<(f64, usize)> = None;
    for v in values {
        let count = values.iter().filter(|x| *x == v).count();
        if count > 1 && best.map(|(_, c)| count > c).unwrap_or(true) {
            best = Some((*v, count));
        }
    }
    best.map(|(v, _)| v)
}

/// Linear interpolation between the two neighbouring ranks.
///
/// The two conventions differ in where they put the ends. *Inclusive* spreads
/// the data over 0..1, so the 0th and 100th percentiles are the smallest and
/// largest values. *Exclusive* treats the sample as coming from a larger
/// population, so a percentile below `1/(n+1)` or above `n/(n+1)` has no
/// answer at all — None here, `#NUM!` in the cell. Excel ships both because
/// which one is right depends on what the numbers are.
fn percentile_of(sorted: &[f64], q: f64, exclusive: bool) -> Option<f64> {
    let n = sorted.len();
    if n == 0 {
        return None;
    }
    let pos = if exclusive {
        let p = q * (n + 1) as f64;
        if p < 1.0 || p > n as f64 {
            return None;
        }
        p - 1.0
    } else {
        if n == 1 {
            return Some(sorted[0]);
        }
        q * (n - 1) as f64
    };
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        return Some(sorted[lo]);
    }
    Some(sorted[lo] + (pos - lo as f64) * (sorted[hi] - sorted[lo]))
}
