//! Math and aggregate functions.
//!
//! Expected values in tests are Excel-verified (Microsoft 365).

use super::{expect_args, gather, gather_numbers, num_result};
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
