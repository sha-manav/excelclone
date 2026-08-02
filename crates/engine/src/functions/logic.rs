//! Logical and information functions.

use super::{expect_args, gather};
use crate::ast::Expr;
use crate::eval::EvalCtx;
use crate::value::{ErrorKind, Value};

/// IF(cond, then, [else]): lazy branches; omitted else -> FALSE.
pub fn if_fn(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 3) {
        return Value::Error(k);
    }
    match ctx.eval_bool(&args[0]) {
        Err(k) => Value::Error(k),
        Ok(true) => ctx.eval_scalar(&args[1]),
        Ok(false) => match args.get(2) {
            Some(e) => ctx.eval_scalar(e),
            None => Value::Bool(false),
        },
    }
}

/// IFS(c1, v1, c2, v2, ...): first true condition wins; none -> #N/A.
pub fn ifs(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if args.len() < 2 || !args.len().is_multiple_of(2) {
        return Value::Error(ErrorKind::Value);
    }
    for pair in args.chunks(2) {
        match ctx.eval_bool(&pair[0]) {
            Err(k) => return Value::Error(k),
            Ok(true) => return ctx.eval_scalar(&pair[1]),
            Ok(false) => {}
        }
    }
    Value::Error(ErrorKind::NA)
}

/// Shared body for AND/OR: direct args coerce with #VALUE! on non-boolean
/// text; range text/empty cells are ignored; no logical values -> #VALUE!.
fn and_or(ctx: &EvalCtx, args: &[Expr], init: bool, f: impl Fn(bool, bool) -> bool) -> Value {
    let mut acc = init;
    let mut seen = false;
    for g in gather(ctx, args) {
        if let Some(k) = g.value.as_error() {
            return Value::Error(k);
        }
        if g.from_range {
            match g.value {
                Value::Bool(b) => {
                    acc = f(acc, b);
                    seen = true;
                }
                Value::Number(n) => {
                    acc = f(acc, n != 0.0);
                    seen = true;
                }
                _ => {}
            }
        } else {
            match crate::eval::to_bool(&g.value) {
                Ok(b) => {
                    acc = f(acc, b);
                    seen = true;
                }
                Err(k) => return Value::Error(k),
            }
        }
    }
    if seen {
        Value::Bool(acc)
    } else {
        Value::Error(ErrorKind::Value)
    }
}

pub fn and(ctx: &EvalCtx, args: &[Expr]) -> Value {
    and_or(ctx, args, true, |a, b| a && b)
}

pub fn or(ctx: &EvalCtx, args: &[Expr]) -> Value {
    and_or(ctx, args, false, |a, b| a || b)
}

pub fn not(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    match ctx.eval_bool(&args[0]) {
        Ok(b) => Value::Bool(!b),
        Err(k) => Value::Error(k),
    }
}

/// IFERROR(value, fallback): fallback only evaluated on error.
pub fn iferror(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 2) {
        return Value::Error(k);
    }
    let v = ctx.eval_scalar(&args[0]);
    if v.is_error() {
        ctx.eval_scalar(&args[1])
    } else {
        v
    }
}

/// ISBLANK: true only for genuinely empty cells; "" is not blank.
pub fn isblank(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    Value::Bool(ctx.eval_scalar(&args[0]).is_empty())
}

pub fn isnumber(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    Value::Bool(matches!(ctx.eval_scalar(&args[0]), Value::Number(_)))
}

pub fn istext(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    Value::Bool(matches!(ctx.eval_scalar(&args[0]), Value::Text(_)))
}

pub fn iserror(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    Value::Bool(ctx.eval_scalar(&args[0]).is_error())
}
