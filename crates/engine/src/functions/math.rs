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
