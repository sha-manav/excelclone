//! Function registry and shared argument helpers.
//!
//! Function names are uppercase at parse time; unknown names evaluate to
//! `#NAME?` (fail loudly, never silently).

mod date;
mod logic;
mod lookup;
mod math;

use crate::ast::Expr;
use crate::eval::{EvalCtx, Operand};
use crate::value::{ErrorKind, Value};

pub fn call(ctx: &EvalCtx, name: &str, args: &[Expr]) -> Value {
    match name {
        // Math
        "SUM" => math::sum(ctx, args),
        "PRODUCT" => math::product(ctx, args),
        "AVERAGE" => math::average(ctx, args),
        "MIN" => math::min(ctx, args),
        "MAX" => math::max(ctx, args),
        "COUNT" => math::count(ctx, args),
        "COUNTA" => math::counta(ctx, args),
        "COUNTBLANK" => math::countblank(ctx, args),
        "ROUND" => math::round(ctx, args),
        "ROUNDUP" => math::roundup(ctx, args),
        "ROUNDDOWN" => math::rounddown(ctx, args),
        "ABS" => math::abs(ctx, args),
        "INT" => math::int(ctx, args),
        "MOD" => math::mod_fn(ctx, args),
        "POWER" => math::power(ctx, args),
        "SQRT" => math::sqrt(ctx, args),
        // Logic
        "IF" => logic::if_fn(ctx, args),
        "IFS" => logic::ifs(ctx, args),
        "AND" => logic::and(ctx, args),
        "OR" => logic::or(ctx, args),
        "NOT" => logic::not(ctx, args),
        "IFERROR" => logic::iferror(ctx, args),
        "ISBLANK" => logic::isblank(ctx, args),
        "ISNUMBER" => logic::isnumber(ctx, args),
        "ISTEXT" => logic::istext(ctx, args),
        "ISERROR" => logic::iserror(ctx, args),
        // Lookup
        "VLOOKUP" => lookup::vlookup(ctx, args),
        "HLOOKUP" => lookup::hlookup(ctx, args),
        "INDEX" => lookup::index(ctx, args),
        "MATCH" => lookup::match_fn(ctx, args),
        "XLOOKUP" => lookup::xlookup(ctx, args),
        "CHOOSE" => lookup::choose(ctx, args),
        // Date/time
        "TODAY" => date::today(ctx, args),
        "NOW" => date::now(ctx, args),
        "DATE" => date::date(ctx, args),
        "YEAR" => date::year(ctx, args),
        "MONTH" => date::month(ctx, args),
        "DAY" => date::day(ctx, args),
        "EOMONTH" => date::eomonth(ctx, args),
        "DATEDIF" => date::datedif(ctx, args),
        "WEEKDAY" => date::weekday(ctx, args),
        "RAND" => date::rand(ctx, args),
        "RANDBETWEEN" => date::randbetween(ctx, args),
        _ => Value::Error(ErrorKind::Name),
    }
}

/// A value gathered from an argument list, tagged with whether it came from
/// a reference (cell or range). Excel's aggregates ignore text and booleans
/// found behind references but coerce them when passed directly.
pub struct Gathered {
    pub value: Value,
    pub from_range: bool,
}

/// Flatten scalar args and range contents into a single list. Range contents
/// only include populated cells, in deterministic (row, col) order.
pub fn gather(ctx: &EvalCtx, args: &[Expr]) -> Vec<Gathered> {
    let mut out = Vec::new();
    for a in args {
        // A single-cell reference counts as a reference, not a literal:
        // =AVERAGE(A1) with text in A1 is #DIV/0!, while =AVERAGE("x") is #VALUE!.
        let is_ref = matches!(a, Expr::Cell(_) | Expr::Range(_));
        match ctx.eval_operand(a) {
            Operand::Scalar(v) => out.push(Gathered {
                value: v,
                from_range: is_ref,
            }),
            Operand::Range { sheet, range } => {
                for v in ctx.range_values(sheet, range) {
                    out.push(Gathered {
                        value: v,
                        from_range: true,
                    });
                }
            }
        }
    }
    out
}

/// Collect the numbers an Excel numeric aggregate (SUM/AVERAGE/MIN/MAX/
/// PRODUCT) operates on: direct args coerce (numeric text, bools) with
/// #VALUE! on failure; range cells count only real numbers; errors propagate.
pub fn gather_numbers(ctx: &EvalCtx, args: &[Expr]) -> Result<Vec<f64>, ErrorKind> {
    let mut out = Vec::new();
    for g in gather(ctx, args) {
        if let Some(k) = g.value.as_error() {
            return Err(k);
        }
        if g.from_range {
            if let Value::Number(n) = g.value {
                out.push(n);
            }
        } else {
            out.push(crate::eval::to_number(&g.value)?);
        }
    }
    Ok(out)
}

pub fn expect_args(args: &[Expr], min: usize, max: usize) -> Result<(), ErrorKind> {
    if args.len() < min || args.len() > max {
        Err(ErrorKind::Value)
    } else {
        Ok(())
    }
}

/// Wrap a Result-returning body into a Value.
pub fn num_result(r: Result<f64, ErrorKind>) -> Value {
    match r {
        Ok(n) if n.is_finite() => Value::Number(n),
        Ok(_) => Value::Error(ErrorKind::Num),
        Err(k) => Value::Error(k),
    }
}
