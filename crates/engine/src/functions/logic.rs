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

// ---------------------------------------------------------------------------
// The rest of the IS-family, and the errors you make on purpose
// ---------------------------------------------------------------------------

/// The value of an argument without letting an error abort the call, which is
/// the whole point of a predicate that asks *whether* it is an error.
fn peek(ctx: &EvalCtx, e: &Expr) -> Value {
    match ctx.eval_operand(e) {
        crate::eval::Operand::Scalar(v) => v,
        // A range where a scalar was wanted: Excel's implicit intersection is
        // out of scope, and #VALUE! is what it reports when that fails.
        crate::eval::Operand::Range { .. } => Value::Error(ErrorKind::Value),
        crate::eval::Operand::Array(a) if a.is_single() => a.values[0].clone(),
        crate::eval::Operand::Array(_) => Value::Error(ErrorKind::Value),
    }
}

fn predicate(ctx: &EvalCtx, args: &[Expr], f: impl Fn(&Value) -> bool) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    Value::Bool(f(&peek(ctx, &args[0])))
}

/// ISNA: #N/A and nothing else. ISERR is every error *except* #N/A, and
/// ISERROR is all of them — three predicates because "not found" is a
/// different thing from "broken".
pub fn isna(ctx: &EvalCtx, args: &[Expr]) -> Value {
    predicate(ctx, args, |v| matches!(v, Value::Error(ErrorKind::NA)))
}

pub fn iserr(ctx: &EvalCtx, args: &[Expr]) -> Value {
    predicate(
        ctx,
        args,
        |v| matches!(v, Value::Error(k) if *k != ErrorKind::NA),
    )
}

pub fn islogical(ctx: &EvalCtx, args: &[Expr]) -> Value {
    predicate(ctx, args, |v| matches!(v, Value::Bool(_)))
}

/// NA(): the error, on purpose. A placeholder that cannot be mistaken for
/// data, unlike a zero or an empty cell.
pub fn na(_ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 0, 0) {
        return Value::Error(k);
    }
    Value::Error(ErrorKind::NA)
}

/// IFNA(value, fallback): catches #N/A only.
///
/// The point of having it beside IFERROR is that a failed lookup is expected
/// and a #VALUE! is a bug — IFERROR would swallow both.
pub fn ifna(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 2) {
        return Value::Error(k);
    }
    match peek(ctx, &args[0]) {
        Value::Error(ErrorKind::NA) => peek(ctx, &args[1]),
        other => other,
    }
}

/// TYPE(value): 1 number, 2 text, 4 logical, 16 error, 64 array.
///
/// The numbering is not sequential because it is a bit field from 1985; the
/// values are what every sheet using TYPE compares against.
pub fn type_of(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    let code = match ctx.eval_operand(&args[0]) {
        // 64 is Excel's code for an array, which a reference in this
        // position also reports as.
        crate::eval::Operand::Range { .. } => 64.0,
        crate::eval::Operand::Array(a) if !a.is_single() => 64.0,
        crate::eval::Operand::Array(a) => match a.values[0] {
            Value::Number(_) | Value::Empty => 1.0,
            Value::Text(_) => 2.0,
            Value::Bool(_) => 4.0,
            Value::Error(_) => 16.0,
        },
        crate::eval::Operand::Scalar(v) => match v {
            // An empty cell reports as a number, which is consistent with it
            // coercing to 0 everywhere else.
            Value::Number(_) | Value::Empty => 1.0,
            Value::Text(_) => 2.0,
            Value::Bool(_) => 4.0,
            Value::Error(_) => 16.0,
        },
    };
    Value::Number(code)
}

/// ISREF(value): whether the argument is a reference at all.
///
/// Decided from the *expression* rather than the value, because by the time a
/// value arrives there is nothing left to tell a cell from the number in it.
pub fn isref(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    let is_reference = match &args[0] {
        Expr::Cell(_) | Expr::Range(_) => true,
        // A function that returns a reference would count too; we have none
        // yet, and OFFSET/INDIRECT are exactly what would change this.
        _ => false,
    };
    // Evaluated anyway so a #REF! inside the argument still surfaces — ISREF
    // asks about shape, not about whether the reference resolves.
    if let Value::Error(ErrorKind::Ref) = peek(ctx, &args[0]) {
        return Value::Bool(is_reference);
    }
    Value::Bool(is_reference)
}

/// LET(name1, value1, [name2, value2, ...], calculation)
///
/// Binds names so an expression can be written once and used several times.
/// The point is not brevity: a repeated subexpression is evaluated once per
/// appearance, and `LET` is how a formula that reads the same lookup four
/// times stops doing the lookup four times.
///
/// The names are the odd arguments and are taken from the *expression*, not
/// from its value — `Expr::Name` exists for exactly this. Each binding sees
/// the ones before it, which is what makes `LET(a,1,b,a+1,b)` work.
pub fn let_fn(ctx: &EvalCtx, args: &[Expr]) -> Value {
    // A name, its value, and eventually the calculation: at least three, and
    // an odd number of them.
    if args.len() < 3 || args.len().is_multiple_of(2) {
        return Value::Error(ErrorKind::Value);
    }
    let mut bound: Vec<(String, Value)> = Vec::new();
    for pair in args[..args.len() - 1].chunks(2) {
        let Expr::Name(name) = &pair[0] else {
            // Anything that parsed as a reference, a number or a function is
            // not a name. Excel says #NAME? and so does this: silently
            // treating `A1` as a binding would shadow the cell.
            return Value::Error(ErrorKind::Name);
        };
        let value = ctx.with_bindings(&bound).eval_scalar(&pair[1]);
        if let Some(k) = value.as_error() {
            return Value::Error(k);
        }
        bound.push((name.clone(), value));
    }
    ctx.with_bindings(&bound).eval_scalar(&args[args.len() - 1])
}
