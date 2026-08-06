//! Time-value-of-money functions.
//!
//! All of them are the same equation rearranged, which Microsoft states in
//! the documentation for each one:
//!
//! ```text
//! pv(1+r)^n + pmt(1 + r·type)·((1+r)^n − 1)/r + fv = 0
//! ```
//!
//! with `r` the rate per period, `n` the number of periods, and `type` 0 for
//! payments at the end of a period and 1 for the beginning. When `r` is zero
//! the middle term is a division by zero in the limit, so each function has
//! an explicit `r == 0` branch — the simple `pmt·n` case.
//!
//! # Signs
//!
//! Excel's convention is cash-flow direction: money you receive is positive
//! and money you pay is negative. A loan is entered as a positive `pv` (the
//! bank gave you the money) and `PMT` comes back negative (you pay it back).
//! Reproducing that sign is most of what makes these functions agree with a
//! real amortisation schedule, and getting it backwards produces answers that
//! look plausible and are wrong.

use super::{expect_args, gather_numbers, num_result};
use crate::ast::Expr;
use crate::eval::EvalCtx;
use crate::value::{ErrorKind, Value};

/// An optional numeric argument with a default.
fn opt(ctx: &EvalCtx, args: &[Expr], i: usize, default: f64) -> Result<f64, ErrorKind> {
    match args.get(i) {
        Some(e) => ctx.eval_number(e),
        None => Ok(default),
    }
}

/// `(1 + r)^n`, and the annuity factor that goes with it.
fn factors(rate: f64, nper: f64, type_: f64) -> (f64, f64) {
    let growth = (1.0 + rate).powf(nper);
    let annuity = if rate == 0.0 {
        nper
    } else {
        (growth - 1.0) / rate * (1.0 + rate * type_)
    };
    (growth, annuity)
}

/// PMT(rate, nper, pv, [fv], [type]): the payment per period.
pub fn pmt(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 3, 5) {
        return Value::Error(k);
    }
    num_result((|| {
        let rate = ctx.eval_number(&args[0])?;
        let nper = ctx.eval_number(&args[1])?;
        let pv = ctx.eval_number(&args[2])?;
        let fv = opt(ctx, args, 3, 0.0)?;
        let type_ = opt(ctx, args, 4, 0.0)?;
        if nper == 0.0 {
            return Err(ErrorKind::Num);
        }
        let (growth, annuity) = factors(rate, nper, type_);
        Ok(-(pv * growth + fv) / annuity)
    })())
}

/// FV(rate, nper, pmt, [pv], [type]): what it is worth at the end.
pub fn fv(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 3, 5) {
        return Value::Error(k);
    }
    num_result((|| {
        let rate = ctx.eval_number(&args[0])?;
        let nper = ctx.eval_number(&args[1])?;
        let payment = ctx.eval_number(&args[2])?;
        let pv = opt(ctx, args, 3, 0.0)?;
        let type_ = opt(ctx, args, 4, 0.0)?;
        let (growth, annuity) = factors(rate, nper, type_);
        Ok(-(pv * growth + payment * annuity))
    })())
}

/// PV(rate, nper, pmt, [fv], [type]): what it is worth now.
pub fn pv(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 3, 5) {
        return Value::Error(k);
    }
    num_result((|| {
        let rate = ctx.eval_number(&args[0])?;
        let nper = ctx.eval_number(&args[1])?;
        let payment = ctx.eval_number(&args[2])?;
        let future = opt(ctx, args, 3, 0.0)?;
        let type_ = opt(ctx, args, 4, 0.0)?;
        let (growth, annuity) = factors(rate, nper, type_);
        if growth == 0.0 {
            return Err(ErrorKind::Num);
        }
        Ok(-(future + payment * annuity) / growth)
    })())
}

/// NPER(rate, pmt, pv, [fv], [type]): how many periods it takes.
pub fn nper(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 3, 5) {
        return Value::Error(k);
    }
    num_result((|| {
        let rate = ctx.eval_number(&args[0])?;
        let payment = ctx.eval_number(&args[1])?;
        let present = ctx.eval_number(&args[2])?;
        let future = opt(ctx, args, 3, 0.0)?;
        let type_ = opt(ctx, args, 4, 0.0)?;
        if rate == 0.0 {
            if payment == 0.0 {
                return Err(ErrorKind::Num);
            }
            return Ok(-(present + future) / payment);
        }
        // Solve the same equation for n, which needs logarithms rather than
        // iteration because n appears only in the exponent.
        let adjusted = payment * (1.0 + rate * type_) / rate;
        let numerator = adjusted - future;
        let denominator = present + adjusted;
        if denominator == 0.0 || numerator / denominator <= 0.0 {
            return Err(ErrorKind::Num);
        }
        Ok((numerator / denominator).ln() / (1.0 + rate).ln())
    })())
}

/// RATE(nper, pmt, pv, [fv], [type], [guess]): solved numerically.
///
/// There is no closed form for the rate, so Excel iterates and gives up with
/// `#NUM!`. This bisects a bracket rather than following Newton's method:
/// slower, and it cannot wander off to a nonsensical root the way Newton does
/// on a badly conditioned cash flow.
pub fn rate(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 3, 6) {
        return Value::Error(k);
    }
    num_result((|| {
        let nper = ctx.eval_number(&args[0])?;
        let payment = ctx.eval_number(&args[1])?;
        let present = ctx.eval_number(&args[2])?;
        let future = opt(ctx, args, 3, 0.0)?;
        let type_ = opt(ctx, args, 4, 0.0)?;
        if nper <= 0.0 {
            return Err(ErrorKind::Num);
        }
        let residual = |r: f64| {
            let (growth, annuity) = factors(r, nper, type_);
            present * growth + payment * annuity + future
        };
        solve(residual, -0.999_999, 10.0).ok_or(ErrorKind::Num)
    })())
}

/// NPV(rate, value, ...): the present value of a series starting one period
/// out.
///
/// The first value is discounted once, not zero times — which is why an
/// initial outlay at time zero is added *outside* the call and is the single
/// most common mistake made with this function.
pub fn npv(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, usize::MAX) {
        return Value::Error(k);
    }
    num_result((|| {
        let rate = ctx.eval_number(&args[0])?;
        if rate == -1.0 {
            return Err(ErrorKind::Num);
        }
        let flows = gather_numbers(ctx, &args[1..])?;
        Ok(flows
            .iter()
            .enumerate()
            .map(|(i, v)| v / (1.0 + rate).powi(i as i32 + 1))
            .sum())
    })())
}

/// IRR(values, [guess]): the rate at which NPV is zero.
///
/// The first value is at time zero here, unlike NPV — the two functions
/// disagree about that on purpose, and a sheet that pairs them has to account
/// for it.
pub fn irr(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 2) {
        return Value::Error(k);
    }
    num_result((|| {
        let flows = gather_numbers(ctx, &args[..1])?;
        if flows.len() < 2 {
            return Err(ErrorKind::Num);
        }
        // Without both signs there is no root to find, and iterating anyway
        // would return whichever end of the bracket happened to be closer.
        if !flows.iter().any(|v| *v > 0.0) || !flows.iter().any(|v| *v < 0.0) {
            return Err(ErrorKind::Num);
        }
        let residual = |r: f64| {
            flows
                .iter()
                .enumerate()
                .map(|(i, v)| v / (1.0 + r).powi(i as i32))
                .sum::<f64>()
        };
        solve(residual, -0.999_999, 10.0).ok_or(ErrorKind::Num)
    })())
}

/// Bisect `f` for a sign change in `[lo, hi]`.
///
/// Returns `None` when the interval does not bracket a root, which the caller
/// turns into `#NUM!` — the same answer Excel gives when its own iteration
/// does not converge.
fn solve(f: impl Fn(f64) -> f64, lo: f64, hi: f64) -> Option<f64> {
    let (mut lo, mut hi) = (lo, hi);
    let (mut flo, fhi) = (f(lo), f(hi));
    if !flo.is_finite() || !fhi.is_finite() || flo.signum() == fhi.signum() {
        return None;
    }
    // 200 halvings takes the interval far below any precision a double holds;
    // the tolerance check below almost always ends it first.
    for _ in 0..200 {
        let mid = (lo + hi) / 2.0;
        let fmid = f(mid);
        if !fmid.is_finite() {
            return None;
        }
        if fmid.abs() < 1e-12 || (hi - lo).abs() < 1e-12 {
            return Some(mid);
        }
        if fmid.signum() == flo.signum() {
            lo = mid;
            flo = fmid;
        } else {
            hi = mid;
        }
    }
    Some((lo + hi) / 2.0)
}
