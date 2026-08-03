//! Function registry and shared argument helpers.
//!
//! Function names are uppercase at parse time; unknown names evaluate to
//! `#NAME?` (fail loudly, never silently).

pub(crate) mod condagg;
mod date;
mod finance;
mod logic;
mod lookup;
mod math;
pub mod numfmt;
mod text;

use crate::ast::Expr;
use crate::eval::{EvalCtx, Operand};
use crate::value::{ErrorKind, Value};

/// Every function name `call` answers to, in dispatch order.
///
/// Exists so the parity harness can ask what is implemented without guessing,
/// and so "we support N functions" is a list somebody can read rather than a
/// number somebody remembers. `the_list_matches_the_dispatcher` keeps it from
/// drifting away from the `match` below.
pub const IMPLEMENTED: &[&str] = &[
    // Math
    "SUM",
    "PRODUCT",
    "AVERAGE",
    "MIN",
    "MAX",
    "COUNT",
    "COUNTA",
    "COUNTBLANK",
    "ROUND",
    "ROUNDUP",
    "ROUNDDOWN",
    "ABS",
    "INT",
    "MOD",
    "POWER",
    "SQRT",
    "CEILING",
    "FLOOR",
    "MROUND",
    "TRUNC",
    "SIGN",
    "EXP",
    "LN",
    "LOG",
    "LOG10",
    "GCD",
    "LCM",
    "MEDIAN",
    "LARGE",
    "SMALL",
    "RANK",
    "SUMPRODUCT",
    "MODE",
    "STDEV",
    "SUBTOTAL",
    "PMT",
    "FV",
    "PV",
    "NPER",
    "RATE",
    "NPV",
    "IRR",
    // Logic
    "IF",
    "IFS",
    "AND",
    "OR",
    "NOT",
    "IFERROR",
    "ISBLANK",
    "ISNUMBER",
    "ISTEXT",
    "ISERROR",
    "ISNA",
    "ISERR",
    "ISLOGICAL",
    "IFNA",
    "NA",
    "TYPE",
    "ISREF",
    // Lookup
    "VLOOKUP",
    "HLOOKUP",
    "INDEX",
    "MATCH",
    "XLOOKUP",
    "CHOOSE",
    "ROW",
    "COLUMN",
    "ROWS",
    "COLUMNS",
    "XMATCH",
    "LOOKUP",
    // Text
    "CONCAT",
    "CONCATENATE",
    "TEXTJOIN",
    "LEFT",
    "RIGHT",
    "MID",
    "LEN",
    "TRIM",
    "UPPER",
    "LOWER",
    "PROPER",
    "SUBSTITUTE",
    "REPLACE",
    "FIND",
    "SEARCH",
    "TEXT",
    "VALUE",
    "REPT",
    "EXACT",
    "CHAR",
    "CODE",
    "CLEAN",
    "TEXTBEFORE",
    "TEXTAFTER",
    "NUMBERVALUE",
    // Conditional aggregation
    "COUNTIF",
    "COUNTIFS",
    "SUMIF",
    "SUMIFS",
    "AVERAGEIF",
    "AVERAGEIFS",
    // Date/time
    "TODAY",
    "NOW",
    "DATE",
    "YEAR",
    "MONTH",
    "DAY",
    "EOMONTH",
    "DATEDIF",
    "WEEKDAY",
    "RAND",
    "RANDBETWEEN",
    "TIME",
    "HOUR",
    "MINUTE",
    "SECOND",
    "DATEVALUE",
    "EDATE",
    "DAYS",
    "TIMEVALUE",
    "NETWORKDAYS",
    "WORKDAY",
    "YEARFRAC",
];

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
        "CEILING" => math::ceiling(ctx, args),
        "FLOOR" => math::floor(ctx, args),
        "MROUND" => math::mround(ctx, args),
        "TRUNC" => math::trunc(ctx, args),
        "SIGN" => math::sign(ctx, args),
        "EXP" => math::exp(ctx, args),
        "LN" => math::ln(ctx, args),
        "LOG" => math::log(ctx, args),
        "LOG10" => math::log10(ctx, args),
        "GCD" => math::gcd(ctx, args),
        "LCM" => math::lcm(ctx, args),
        "MEDIAN" => math::median(ctx, args),
        "LARGE" => math::large(ctx, args),
        "SMALL" => math::small(ctx, args),
        "RANK" => math::rank(ctx, args),
        "SUMPRODUCT" => math::sumproduct(ctx, args),
        "MODE" => math::mode(ctx, args),
        "STDEV" => math::stdev(ctx, args),
        "SUBTOTAL" => math::subtotal(ctx, args),
        // Finance
        "PMT" => finance::pmt(ctx, args),
        "FV" => finance::fv(ctx, args),
        "PV" => finance::pv(ctx, args),
        "NPER" => finance::nper(ctx, args),
        "RATE" => finance::rate(ctx, args),
        "NPV" => finance::npv(ctx, args),
        "IRR" => finance::irr(ctx, args),
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
        "ISNA" => logic::isna(ctx, args),
        "ISERR" => logic::iserr(ctx, args),
        "ISLOGICAL" => logic::islogical(ctx, args),
        "IFNA" => logic::ifna(ctx, args),
        "NA" => logic::na(ctx, args),
        "TYPE" => logic::type_of(ctx, args),
        "ISREF" => logic::isref(ctx, args),
        // Lookup
        "VLOOKUP" => lookup::vlookup(ctx, args),
        "HLOOKUP" => lookup::hlookup(ctx, args),
        "INDEX" => lookup::index(ctx, args),
        "MATCH" => lookup::match_fn(ctx, args),
        "XLOOKUP" => lookup::xlookup(ctx, args),
        "CHOOSE" => lookup::choose(ctx, args),
        "ROW" => lookup::row(ctx, args),
        "COLUMN" => lookup::column(ctx, args),
        "ROWS" => lookup::rows(ctx, args),
        "COLUMNS" => lookup::columns(ctx, args),
        "XMATCH" => lookup::xmatch(ctx, args),
        "LOOKUP" => lookup::lookup(ctx, args),
        // Text
        "CONCAT" => text::concat(ctx, args),
        "CONCATENATE" => text::concatenate(ctx, args),
        "TEXTJOIN" => text::textjoin(ctx, args),
        "LEFT" => text::left(ctx, args),
        "RIGHT" => text::right(ctx, args),
        "MID" => text::mid(ctx, args),
        "LEN" => text::len(ctx, args),
        "TRIM" => text::trim(ctx, args),
        "UPPER" => text::upper(ctx, args),
        "LOWER" => text::lower(ctx, args),
        "PROPER" => text::proper(ctx, args),
        "SUBSTITUTE" => text::substitute(ctx, args),
        "REPLACE" => text::replace_fn(ctx, args),
        "FIND" => text::find(ctx, args),
        "SEARCH" => text::search(ctx, args),
        "TEXT" => text::text(ctx, args),
        "VALUE" => text::value(ctx, args),
        "REPT" => text::rept(ctx, args),
        "EXACT" => text::exact(ctx, args),
        "CHAR" => text::char_fn(ctx, args),
        "CODE" => text::code(ctx, args),
        "CLEAN" => text::clean(ctx, args),
        "TEXTBEFORE" => text::textbefore(ctx, args),
        "TEXTAFTER" => text::textafter(ctx, args),
        "NUMBERVALUE" => text::numbervalue(ctx, args),
        // Conditional aggregation
        "COUNTIF" => condagg::countif(ctx, args),
        "COUNTIFS" => condagg::countifs(ctx, args),
        "SUMIF" => condagg::sumif(ctx, args),
        "SUMIFS" => condagg::sumifs(ctx, args),
        "AVERAGEIF" => condagg::averageif(ctx, args),
        "AVERAGEIFS" => condagg::averageifs(ctx, args),
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
        "TIME" => date::time(ctx, args),
        "HOUR" => date::hour(ctx, args),
        "MINUTE" => date::minute(ctx, args),
        "SECOND" => date::second(ctx, args),
        "DATEVALUE" => date::datevalue(ctx, args),
        "EDATE" => date::edate(ctx, args),
        "DAYS" => date::days(ctx, args),
        "TIMEVALUE" => date::timevalue(ctx, args),
        "NETWORKDAYS" => date::networkdays(ctx, args),
        "WORKDAY" => date::workday(ctx, args),
        "YEARFRAC" => date::yearfrac(ctx, args),
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

#[cfg(test)]
mod registry_tests {
    use super::IMPLEMENTED;

    /// `IMPLEMENTED` is a hand-written copy of the dispatcher's arms. If it
    /// drifts, the parity report starts lying about what exists — in either
    /// direction — so the two are compared against each other here.
    #[test]
    fn the_list_matches_the_dispatcher() {
        let source = include_str!("mod.rs");
        let body = source
            .split_once("pub fn call(")
            .expect("the dispatcher")
            .1
            .split_once("_ => Value::Error(ErrorKind::Name)")
            .expect("the fallthrough")
            .0;
        let mut dispatched: Vec<&str> = body
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                let rest = line.strip_prefix('"')?;
                let (name, tail) = rest.split_once('"')?;
                tail.trim_start().starts_with("=>").then_some(name)
            })
            .collect();
        dispatched.sort_unstable();
        dispatched.dedup();

        let mut listed: Vec<&str> = IMPLEMENTED.to_vec();
        listed.sort_unstable();
        let before = listed.len();
        listed.dedup();
        assert_eq!(before, listed.len(), "IMPLEMENTED lists a name twice");

        assert_eq!(
            dispatched, listed,
            "IMPLEMENTED has drifted from the dispatcher"
        );
    }
}
