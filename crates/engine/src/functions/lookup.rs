//! Lookup and reference functions.
//!
//! Expected values are Excel-verified (Microsoft 365). Two conventions run
//! through the whole file:
//! - Table/array arguments come from `eval_grid`, so empty cells keep their
//!   position: lookups are positional, not "populated cells only".
//! - A cell that a lookup actually returns hands back its own error, while
//!   errors merely sitting in a scanned row/column never match and never
//!   abort the scan.

use super::expect_args;
use crate::ast::Expr;
use crate::eval::{compare_values, EvalCtx};
use crate::value::{ErrorKind, Value};
use std::cmp::Ordering;

/// VLOOKUP(lookup_value, table_array, col_index_num, [range_lookup=TRUE]).
pub fn vlookup(ctx: &EvalCtx, args: &[Expr]) -> Value {
    hv_lookup(ctx, args, true)
}

/// HLOOKUP(lookup_value, table_array, row_index_num, [range_lookup=TRUE]).
pub fn hlookup(ctx: &EvalCtx, args: &[Expr]) -> Value {
    hv_lookup(ctx, args, false)
}

/// Shared body: VLOOKUP scans the first column and indexes across columns,
/// HLOOKUP scans the first row and indexes down rows.
fn hv_lookup(ctx: &EvalCtx, args: &[Expr], vertical: bool) -> Value {
    if let Err(k) = expect_args(args, 3, 4) {
        return Value::Error(k);
    }
    let lookup = ctx.eval_scalar(&args[0]);
    if let Some(k) = lookup.as_error() {
        return Value::Error(k);
    }
    let table = match grid_arg(ctx, &args[1]) {
        Ok(g) => g,
        Err(k) => return Value::Error(k),
    };
    let index = match ctx.eval_number(&args[2]).and_then(positive_index) {
        Ok(i) => i,
        Err(k) => return Value::Error(k),
    };
    // Omitted range_lookup defaults to TRUE (approximate), the Excel default.
    let approximate = match args.get(3) {
        Some(e) => match ctx.eval_bool(e) {
            Ok(b) => b,
            Err(k) => return Value::Error(k),
        },
        None => true,
    };

    let (rows, cols) = match dims(&table) {
        Some(d) => d,
        None => return Value::Error(ErrorKind::Ref),
    };
    // An index past the table's other dimension is #REF!, not #VALUE!.
    if index > if vertical { cols } else { rows } {
        return Value::Error(ErrorKind::Ref);
    }

    let line: Vec<Value> = if vertical {
        table.iter().map(|r| r[0].clone()).collect()
    } else {
        table[0].clone()
    };
    let found = if approximate {
        last_ordered(&lookup, &line, Ordering::Greater)
    } else {
        // Exact mode honours `*`/`?` wildcards in a text lookup value.
        line.iter().position(|v| lookup_equal(&lookup, v, true))
    };
    let Some(pos) = found else {
        return Value::Error(ErrorKind::NA);
    };
    if vertical {
        cell_result(&table[pos][index - 1])
    } else {
        cell_result(&table[index - 1][pos])
    }
}

/// INDEX(array, row_num, [col_num]), 1-based.
pub fn index(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 3) {
        return Value::Error(k);
    }
    let grid = match grid_arg(ctx, &args[0]) {
        Ok(g) => g,
        Err(k) => return Value::Error(k),
    };
    let (rows, cols) = match dims(&grid) {
        Some(d) => d,
        None => return Value::Error(ErrorKind::Ref),
    };
    let row_num = match ctx.eval_number(&args[1]).and_then(nonneg_index) {
        Ok(i) => i,
        Err(k) => return Value::Error(k),
    };
    let col_num = match args.get(2) {
        Some(e) => match ctx.eval_number(e).and_then(nonneg_index) {
            Ok(i) => Some(i),
            Err(k) => return Value::Error(k),
        },
        None => None,
    };

    // Excel reads a 0 index as "the whole row/column", which is an array
    // result; v1 is scalar-only, so it is honoured only when that dimension
    // is a single line and otherwise fails loudly.
    let (r, c) = match col_num {
        Some(c) => {
            let r = if row_num == 0 {
                if rows == 1 {
                    1
                } else {
                    return Value::Error(ErrorKind::Value);
                }
            } else {
                row_num
            };
            let c = if c == 0 {
                if cols == 1 {
                    1
                } else {
                    return Value::Error(ErrorKind::Value);
                }
            } else {
                c
            };
            (r, c)
        }
        // A single index walks the array's one line: down a column, or across
        // a row. Against a 2-D array it would select a whole row (an array),
        // which v1 cannot represent.
        None => {
            if row_num == 0 {
                if rows == 1 && cols == 1 {
                    (1, 1)
                } else {
                    return Value::Error(ErrorKind::Value);
                }
            } else if cols == 1 {
                (row_num, 1)
            } else if rows == 1 {
                (1, row_num)
            } else {
                return Value::Error(ErrorKind::Value);
            }
        }
    };
    if r > rows || c > cols {
        return Value::Error(ErrorKind::Ref);
    }
    cell_result(&grid[r - 1][c - 1])
}

/// MATCH(lookup_value, lookup_array, [match_type=1]) -> 1-based position.
pub fn match_fn(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 3) {
        return Value::Error(k);
    }
    let lookup = ctx.eval_scalar(&args[0]);
    if let Some(k) = lookup.as_error() {
        return Value::Error(k);
    }
    let grid = match grid_arg(ctx, &args[1]) {
        Ok(g) => g,
        Err(k) => return Value::Error(k),
    };
    let match_type = match args.get(2) {
        Some(e) => match ctx.eval_number(e) {
            Ok(n) => n.trunc(),
            Err(k) => return Value::Error(k),
        },
        None => 1.0,
    };
    // A 2-D lookup array has no single position sequence.
    let Some(line) = as_vector(&grid) else {
        return Value::Error(ErrorKind::NA);
    };
    // Excel reads only the sign of match_type: any positive acts like 1, any
    // negative like -1.
    let found = if match_type > 0.0 {
        // Ascending array: largest value <= lookup.
        last_ordered(&lookup, &line, Ordering::Greater)
    } else if match_type < 0.0 {
        // Descending array: smallest value >= lookup.
        last_ordered(&lookup, &line, Ordering::Less)
    } else {
        line.iter().position(|v| lookup_equal(&lookup, v, true))
    };
    match found {
        Some(i) => Value::Number(i as f64 + 1.0),
        None => Value::Error(ErrorKind::NA),
    }
}

/// XLOOKUP(lookup_value, lookup_array, return_array, [if_not_found],
/// [match_mode=0]). v1 implements the exact-match modes only.
pub fn xlookup(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 3, 5) {
        return Value::Error(k);
    }
    let lookup = ctx.eval_scalar(&args[0]);
    if let Some(k) = lookup.as_error() {
        return Value::Error(k);
    }
    let lookup_grid = match grid_arg(ctx, &args[1]) {
        Ok(g) => g,
        Err(k) => return Value::Error(k),
    };
    let return_grid = match grid_arg(ctx, &args[2]) {
        Ok(g) => g,
        Err(k) => return Value::Error(k),
    };
    // match_mode 0 = exact, 2 = exact with wildcards. Modes -1/1 (next
    // smaller/larger) and 2's binary-search siblings arrive in a later
    // milestone; until then an unsupported mode fails loudly.
    let match_mode = match args.get(4) {
        Some(e) => match ctx.eval_number(e) {
            Ok(n) => n.trunc(),
            Err(k) => return Value::Error(k),
        },
        None => 0.0,
    };
    let wildcards = if match_mode == 0.0 {
        false
    } else if match_mode == 2.0 {
        true
    } else {
        return Value::Error(ErrorKind::Value);
    };

    // Both arrays must be vectors of the same shape, so position i in one
    // corresponds to position i in the other.
    let (Some(line), Some(results)) = (as_vector(&lookup_grid), as_vector(&return_grid)) else {
        return Value::Error(ErrorKind::Value);
    };
    if dims(&lookup_grid) != dims(&return_grid) {
        return Value::Error(ErrorKind::Value);
    }

    match line
        .iter()
        .position(|v| lookup_equal(&lookup, v, wildcards))
    {
        Some(i) => cell_result(&results[i]),
        // if_not_found is evaluated only when it is needed.
        None => match args.get(3) {
            Some(e) => ctx.eval_scalar(e),
            None => Value::Error(ErrorKind::NA),
        },
    }
}

/// CHOOSE(index_num, value1, [value2], ...): only the selected argument is
/// evaluated, so an unselected error or expensive expression costs nothing.
pub fn choose(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, usize::MAX) {
        return Value::Error(k);
    }
    let n = match ctx.eval_number(&args[0]).and_then(positive_index) {
        Ok(i) => i,
        Err(k) => return Value::Error(k),
    };
    // args[0] is the index, so the nth value is args[n].
    if n >= args.len() {
        return Value::Error(ErrorKind::Value);
    }
    ctx.eval_scalar(&args[n])
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Evaluate an argument as a dense grid. A scalar argument that is an error
/// (including a one-cell reference holding one) propagates; errors inside a
/// larger range do not, since only the cell a lookup returns can raise them.
fn grid_arg(ctx: &EvalCtx, e: &Expr) -> Result<Vec<Vec<Value>>, ErrorKind> {
    let (grid, _) = ctx.eval_grid(e);
    if grid.len() == 1 && grid[0].len() == 1 {
        if let Some(k) = grid[0][0].as_error() {
            return Err(k);
        }
    }
    Ok(grid)
}

/// (rows, cols) of a grid; None if it is degenerate.
fn dims(grid: &[Vec<Value>]) -> Option<(usize, usize)> {
    let cols = grid.first()?.len();
    if cols == 0 {
        return None;
    }
    Some((grid.len(), cols))
}

/// Flatten a single-column or single-row grid into positional order.
fn as_vector(grid: &[Vec<Value>]) -> Option<Vec<Value>> {
    let (rows, cols) = dims(grid)?;
    if cols == 1 {
        Some(grid.iter().map(|r| r[0].clone()).collect())
    } else if rows == 1 {
        Some(grid[0].clone())
    } else {
        None
    }
}

/// The value a lookup hands back. A blank cell reads as 0, the way a blank
/// coerces everywhere else in Excel; a cell holding an error yields it.
fn cell_result(v: &Value) -> Value {
    match v {
        Value::Empty => Value::Number(0.0),
        other => other.clone(),
    }
}

/// Index arguments truncate toward zero; below 1 (or not a number) is #VALUE!.
fn positive_index(n: f64) -> Result<usize, ErrorKind> {
    let t = n.trunc();
    if !t.is_finite() || t < 1.0 {
        Err(ErrorKind::Value)
    } else {
        Ok(t as usize)
    }
}

/// Same, but 0 is allowed (INDEX reads it as "the whole row/column").
fn nonneg_index(n: f64) -> Result<usize, ErrorKind> {
    let t = n.trunc();
    if !t.is_finite() || t < 0.0 {
        Err(ErrorKind::Value)
    } else {
        Ok(t as usize)
    }
}

/// Excel compares only within a type during an ordered lookup.
fn same_type(a: &Value, b: &Value) -> bool {
    matches!(
        (a, b),
        (Value::Number(_), Value::Number(_))
            | (Value::Text(_), Value::Text(_))
            | (Value::Bool(_), Value::Bool(_))
    )
}

/// Last entry that is not on the `forbidden` side of the lookup value:
/// `Greater` gives the largest value <= lookup (ascending arrays), `Less` the
/// smallest value >= lookup (descending arrays). Cells of another type are
/// skipped, since Excel never orders across types. Scanned linearly rather
/// than by binary search, so unsorted data degrades predictably.
fn last_ordered(lookup: &Value, line: &[Value], forbidden: Ordering) -> Option<usize> {
    let mut best = None;
    for (i, v) in line.iter().enumerate() {
        if same_type(lookup, v) && compare_values(v, lookup) != forbidden {
            best = Some(i);
        }
    }
    best
}

/// Excel's "equal for lookup purposes": numbers compare numerically, text
/// case-insensitively, and in wildcard modes `*`/`?` in a text lookup value
/// match cell text. An error sitting in the scanned line never matches.
fn lookup_equal(lookup: &Value, cell: &Value, wildcards: bool) -> bool {
    match (lookup, cell) {
        (_, Value::Error(_)) => false,
        (Value::Text(p), Value::Text(t)) if wildcards => wildcard_matches(p, t),
        _ => compare_values(lookup, cell) == Ordering::Equal,
    }
}

/// One unit of a wildcard pattern.
enum Tok {
    Star,
    Any,
    Lit(char),
}

/// Split a pattern into tokens; `~` escapes the next `*`, `?` or `~` and is
/// otherwise a literal tilde.
fn tokenize(pattern: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '~' => match chars.next() {
                Some(n @ ('*' | '?' | '~')) => out.push(Tok::Lit(n)),
                Some(n) => {
                    out.push(Tok::Lit('~'));
                    out.push(Tok::Lit(n));
                }
                None => out.push(Tok::Lit('~')),
            },
            '*' => out.push(Tok::Star),
            '?' => out.push(Tok::Any),
            other => out.push(Tok::Lit(other)),
        }
    }
    out
}

/// Excel wildcard match: case-insensitive, `*` = any run of characters,
/// `?` = exactly one character, `~` escapes. Greedy with backtracking to the
/// most recent `*`, which is linear enough for lookup-sized data.
fn wildcard_matches(pattern: &str, text: &str) -> bool {
    let pat = tokenize(&pattern.to_lowercase());
    let txt: Vec<char> = text.to_lowercase().chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    // Position of the last `*` seen, and the text position to resume from.
    let (mut star, mut resume) = (None, 0usize);
    while t < txt.len() {
        match pat.get(p) {
            Some(Tok::Star) => {
                star = Some(p);
                resume = t;
                p += 1;
            }
            Some(Tok::Any) => {
                p += 1;
                t += 1;
            }
            Some(Tok::Lit(c)) if *c == txt[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some(s) => {
                    // Let the `*` swallow one more character and retry.
                    p = s + 1;
                    resume += 1;
                    t = resume;
                }
                None => return false,
            },
        }
    }
    // Whatever is left of the pattern must be able to match nothing.
    pat[p..].iter().all(|tk| matches!(tk, Tok::Star))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_star_and_question() {
        assert!(wildcard_matches("a*c", "abc"));
        assert!(wildcard_matches("a*c", "ac"));
        assert!(wildcard_matches("a*c", "abbbbc"));
        assert!(!wildcard_matches("a*c", "ab"));
        assert!(wildcard_matches("a?c", "abc"));
        assert!(!wildcard_matches("a?c", "ac"));
        assert!(!wildcard_matches("a?c", "abbc"));
        assert!(wildcard_matches("*", ""));
        assert!(wildcard_matches("", ""));
        assert!(!wildcard_matches("", "a"));
        assert!(wildcard_matches("*b*", "abc"));
        assert!(wildcard_matches("ab*", "abc"));
        assert!(!wildcard_matches("*d", "abc"));
    }

    #[test]
    fn wildcard_escapes() {
        assert!(wildcard_matches("~*", "*"));
        assert!(!wildcard_matches("~*", "x"));
        assert!(wildcard_matches("a~?", "a?"));
        assert!(!wildcard_matches("a~?", "ab"));
        assert!(wildcard_matches("a~~b", "a~b"));
        // A tilde before an ordinary character is just a tilde.
        assert!(wildcard_matches("a~b", "a~b"));
        assert!(wildcard_matches("~**", "*xyz"));
    }

    #[test]
    fn wildcard_is_case_insensitive() {
        assert!(wildcard_matches("ABC", "abc"));
        assert!(wildcard_matches("a*C", "AbbC"));
        assert!(wildcard_matches("?B", "ab"));
    }

    #[test]
    fn lookup_equality() {
        let text = |s: &str| Value::Text(s.to_string());
        assert!(lookup_equal(&text("Apple"), &text("apple"), false));
        assert!(lookup_equal(
            &Value::Number(3.0),
            &Value::Number(3.0),
            false
        ));
        assert!(!lookup_equal(&Value::Number(3.0), &text("3"), false));
        assert!(lookup_equal(&text("a*"), &text("abc"), true));
        // Without wildcard mode the pattern is compared literally.
        assert!(!lookup_equal(&text("a*"), &text("abc"), false));
        assert!(lookup_equal(&text("a*"), &text("a*"), false));
        // Errors in the scanned line never match.
        assert!(!lookup_equal(
            &text("*"),
            &Value::Error(ErrorKind::NA),
            true
        ));
    }

    #[test]
    fn ordered_scan_skips_other_types() {
        let line = vec![
            Value::Number(10.0),
            Value::Text("zzz".into()),
            Value::Number(20.0),
            Value::Empty,
            Value::Number(30.0),
        ];
        // Largest value <= 25 is the 20 at position 2.
        assert_eq!(
            last_ordered(&Value::Number(25.0), &line, Ordering::Greater),
            Some(2)
        );
        assert_eq!(
            last_ordered(&Value::Number(5.0), &line, Ordering::Greater),
            None
        );
        // Descending direction: smallest value >= 25 is the 30 at position 4.
        assert_eq!(
            last_ordered(&Value::Number(25.0), &line, Ordering::Less),
            Some(4)
        );
    }

    #[test]
    fn index_argument_coercion() {
        assert_eq!(positive_index(1.0), Ok(1));
        assert_eq!(positive_index(2.9), Ok(2));
        assert_eq!(positive_index(0.0), Err(ErrorKind::Value));
        assert_eq!(positive_index(-1.0), Err(ErrorKind::Value));
        assert_eq!(positive_index(f64::NAN), Err(ErrorKind::Value));
        assert_eq!(nonneg_index(0.5), Ok(0));
        assert_eq!(nonneg_index(-0.5), Ok(0));
        assert_eq!(nonneg_index(-1.5), Err(ErrorKind::Value));
    }

    #[test]
    fn vectors_and_dims() {
        let col = vec![
            vec![Value::Number(1.0)],
            vec![Value::Number(2.0)],
            vec![Value::Number(3.0)],
        ];
        let row = vec![vec![Value::Number(1.0), Value::Number(2.0)]];
        let grid = vec![
            vec![Value::Number(1.0), Value::Number(2.0)],
            vec![Value::Number(3.0), Value::Number(4.0)],
        ];
        assert_eq!(dims(&col), Some((3, 1)));
        assert_eq!(dims(&row), Some((1, 2)));
        assert_eq!(as_vector(&col).map(|v| v.len()), Some(3));
        assert_eq!(as_vector(&row).map(|v| v.len()), Some(2));
        assert!(as_vector(&grid).is_none());
    }

    #[test]
    fn blank_cells_read_as_zero() {
        assert_eq!(cell_result(&Value::Empty), Value::Number(0.0));
        assert_eq!(
            cell_result(&Value::Error(ErrorKind::Div0)),
            Value::Error(ErrorKind::Div0)
        );
    }
}
