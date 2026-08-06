//! Dynamic-array functions: the ones that return a block rather than a value.
//!
//! They go through `call_operand`, like OFFSET and INDIRECT, because the
//! answer is an [`Array`] and squeezing it into a `Value` would lose all but
//! the first element. The engine places the block on the grid afterwards; see
//! `Engine::place_spills`.
//!
//! Each of them also has a scalar arm in the dispatcher, so a block used
//! where one value is wanted degrades the same way a range does — one cell
//! gives its value, anything larger is `#VALUE!`.

use crate::ast::Expr;
use crate::eval::{compare_values, to_bool, to_number, to_text, Array, EvalCtx, Operand};
use crate::value::{ErrorKind, Value};

use super::expect_args;

pub fn call_operand(ctx: &EvalCtx, name: &str, args: &[Expr]) -> Option<Operand> {
    let arr = match name {
        "UNIQUE" => unique(ctx, args),
        "SORT" => sort(ctx, args),
        "SORTBY" => sortby(ctx, args),
        "FILTER" => filter(ctx, args),
        "SEQUENCE" => sequence(ctx, args),
        "TRANSPOSE" => transpose(ctx, args),
        "TEXTSPLIT" => textsplit(ctx, args),
        _ => return None,
    };
    Some(Operand::Array(arr))
}

fn fail(k: ErrorKind) -> Array {
    Array::scalar(Value::Error(k))
}

/// The first error anywhere in a block, so a block built on a broken cell
/// reports that rather than spilling errors across the sheet.
fn first_error(a: &Array) -> Option<ErrorKind> {
    a.values.iter().find_map(|v| v.as_error())
}

/// Rows of a block as vectors, which is the shape every function here works
/// in — `UNIQUE` compares whole rows, `SORT` reorders them, `FILTER` keeps
/// them.
fn rows_of(a: &Array) -> Vec<Vec<Value>> {
    a.grid()
}

fn from_rows(rows: Vec<Vec<Value>>, cols: u32) -> Array {
    let n = rows.len() as u32;
    Array::new(n, cols, rows.into_iter().flatten().collect())
}

/// Two values are the same entry for UNIQUE when they compare equal.
///
/// The same ordering the comparison operators use, so `UNIQUE` agrees with
/// `=` about what a duplicate is — including that text is matched
/// case-insensitively, which is Excel's rule and surprises people who expect
/// otherwise.
fn same(a: &[Value], b: &[Value]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| compare_values(x, y) == std::cmp::Ordering::Equal)
}

/// UNIQUE(array, [by_col], [exactly_once]).
fn unique(ctx: &EvalCtx, args: &[Expr]) -> Array {
    if let Err(k) = expect_args(args, 1, 3) {
        return fail(k);
    }
    let source = ctx.eval_array(&args[0]);
    if let Some(k) = first_error(&source) {
        return fail(k);
    }
    let by_col = match flag(ctx, args.get(1), false) {
        Ok(b) => b,
        Err(k) => return fail(k),
    };
    let exactly_once = match flag(ctx, args.get(2), false) {
        Ok(b) => b,
        Err(k) => return fail(k),
    };

    let source = if by_col { transposed(&source) } else { source };
    let rows = rows_of(&source);
    let width = source.cols;

    let mut kept: Vec<Vec<Value>> = Vec::new();
    for row in &rows {
        let count = rows.iter().filter(|r| same(r, row)).count();
        // `exactly_once` keeps the rows that appear once, which is a
        // different question from "one of each" and the reason the argument
        // exists at all.
        if exactly_once && count != 1 {
            continue;
        }
        if !kept.iter().any(|k| same(k, row)) {
            kept.push(row.clone());
        }
    }
    if kept.is_empty() {
        // Excel's answer when a filter leaves nothing.
        return fail(ErrorKind::Calc);
    }
    let out = from_rows(kept, width);
    if by_col {
        transposed(&out)
    } else {
        out
    }
}

/// SORT(array, [sort_index], [sort_order], [by_col]).
fn sort(ctx: &EvalCtx, args: &[Expr]) -> Array {
    if let Err(k) = expect_args(args, 1, 4) {
        return fail(k);
    }
    let source = ctx.eval_array(&args[0]);
    if let Some(k) = first_error(&source) {
        return fail(k);
    }
    let index = match number(ctx, args.get(1), 1.0) {
        Ok(n) => n as i64,
        Err(k) => return fail(k),
    };
    let order = match number(ctx, args.get(2), 1.0) {
        Ok(n) => n,
        Err(k) => return fail(k),
    };
    let by_col = match flag(ctx, args.get(3), false) {
        Ok(b) => b,
        Err(k) => return fail(k),
    };
    if order != 1.0 && order != -1.0 {
        return fail(ErrorKind::Value);
    }

    let work = if by_col { transposed(&source) } else { source };
    let width = work.cols;
    if index < 1 || index as u32 > width {
        return fail(ErrorKind::Value);
    }
    let key = (index - 1) as usize;
    let mut rows = rows_of(&work);
    // A stable sort, so rows equal on the key keep the order they arrived in
    // — which is what makes a two-pass sort (by minor key, then major) work.
    rows.sort_by(|a, b| {
        let ord = compare_values(&a[key], &b[key]);
        if order < 0.0 {
            ord.reverse()
        } else {
            ord
        }
    });
    let out = from_rows(rows, width);
    if by_col {
        transposed(&out)
    } else {
        out
    }
}

/// SORTBY(array, by_array1, [order1], [by_array2, order2], ...).
fn sortby(ctx: &EvalCtx, args: &[Expr]) -> Array {
    if args.len() < 2 {
        return fail(ErrorKind::Value);
    }
    let source = ctx.eval_array(&args[0]);
    if let Some(k) = first_error(&source) {
        return fail(k);
    }
    // Pairs of (key column, direction). The direction is optional on each,
    // which is why this is a manual walk rather than a chunk of two.
    let mut keys: Vec<(Vec<Value>, f64)> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let by = ctx.eval_array(&args[i]);
        if let Some(k) = first_error(&by) {
            return fail(k);
        }
        if by.len() != source.rows as usize {
            // A key of the wrong length cannot be lined up with the rows, and
            // guessing which end to pad would silently sort by the wrong
            // thing.
            return fail(ErrorKind::Value);
        }
        let order = match args.get(i + 1) {
            Some(e) => match ctx.eval_number(e) {
                Ok(n) => {
                    i += 1;
                    n
                }
                Err(k) => return fail(k),
            },
            None => 1.0,
        };
        if order != 1.0 && order != -1.0 {
            return fail(ErrorKind::Value);
        }
        keys.push((by.values, order));
        i += 1;
    }

    let mut order: Vec<usize> = (0..source.rows as usize).collect();
    order.sort_by(|&a, &b| {
        for (values, dir) in &keys {
            let ord = compare_values(&values[a], &values[b]);
            let ord = if *dir < 0.0 { ord.reverse() } else { ord };
            if ord != std::cmp::Ordering::Equal {
                return ord;
            }
        }
        std::cmp::Ordering::Equal
    });
    let rows = rows_of(&source);
    from_rows(
        order.into_iter().map(|i| rows[i].clone()).collect(),
        source.cols,
    )
}

/// FILTER(array, include, [if_empty]).
fn filter(ctx: &EvalCtx, args: &[Expr]) -> Array {
    if let Err(k) = expect_args(args, 2, 3) {
        return fail(k);
    }
    let source = ctx.eval_array(&args[0]);
    if let Some(k) = first_error(&source) {
        return fail(k);
    }
    let include = ctx.eval_array(&args[1]);
    if let Some(k) = first_error(&include) {
        return fail(k);
    }

    // The mask runs down the rows or across the columns, whichever it
    // matches. A mask matching neither is a mistake worth naming.
    let by_row = include.len() == source.rows as usize;
    let by_col = include.len() == source.cols as usize;
    if !by_row && !by_col {
        return fail(ErrorKind::Value);
    }
    let keep: Vec<bool> = include
        .values
        .iter()
        .map(|v| to_bool(v).unwrap_or(false))
        .collect();

    let out = if by_row {
        let rows: Vec<Vec<Value>> = rows_of(&source)
            .into_iter()
            .enumerate()
            .filter(|(i, _)| keep[*i])
            .map(|(_, r)| r)
            .collect();
        from_rows(rows, source.cols)
    } else {
        let cols: Vec<usize> = (0..source.cols as usize).filter(|c| keep[*c]).collect();
        let rows: Vec<Vec<Value>> = rows_of(&source)
            .into_iter()
            .map(|r| cols.iter().map(|c| r[*c].clone()).collect())
            .collect();
        from_rows(rows, cols.len() as u32)
    };
    if out.is_empty() || out.rows == 0 || out.cols == 0 {
        return match args.get(2) {
            Some(e) => Array::scalar(ctx.eval_scalar(e)),
            // Excel's answer when nothing matched and no fallback was given.
            None => fail(ErrorKind::Calc),
        };
    }
    out
}

/// SEQUENCE(rows, [columns], [start], [step]).
fn sequence(ctx: &EvalCtx, args: &[Expr]) -> Array {
    if let Err(k) = expect_args(args, 1, 4) {
        return fail(k);
    }
    let rows = match number(ctx, args.first(), 1.0) {
        Ok(n) => n.trunc(),
        Err(k) => return fail(k),
    };
    let cols = match number(ctx, args.get(1), 1.0) {
        Ok(n) => n.trunc(),
        Err(k) => return fail(k),
    };
    let start = match number(ctx, args.get(2), 1.0) {
        Ok(n) => n,
        Err(k) => return fail(k),
    };
    let step = match number(ctx, args.get(3), 1.0) {
        Ok(n) => n,
        Err(k) => return fail(k),
    };
    if rows < 1.0 || cols < 1.0 {
        return fail(ErrorKind::Value);
    }
    // A sequence bigger than the grid is a typo, not a request; refusing
    // beats allocating a hundred million values to find that out.
    if rows > crate::addr::MAX_ROWS as f64 || cols > crate::addr::MAX_COLS as f64 {
        return fail(ErrorKind::Num);
    }
    let (rows, cols) = (rows as u32, cols as u32);
    let values = (0..rows as u64 * cols as u64)
        .map(|i| Value::Number(start + step * i as f64))
        .collect();
    Array::new(rows, cols, values)
}

/// TRANSPOSE(array).
fn transpose(ctx: &EvalCtx, args: &[Expr]) -> Array {
    if let Err(k) = expect_args(args, 1, 1) {
        return fail(k);
    }
    transposed(&ctx.eval_array(&args[0]))
}

fn transposed(a: &Array) -> Array {
    let values = (0..a.cols)
        .flat_map(|c| (0..a.rows).map(move |r| (r, c)))
        .map(|(r, c)| a.at(r, c))
        .collect();
    Array::new(a.cols, a.rows, values)
}

/// TEXTSPLIT(text, col_delimiter, [row_delimiter], [ignore_empty], , [pad_with]).
///
/// The match-mode argument Excel takes fifth is not accepted: it selects
/// case-insensitive matching, and silently ignoring it would split on the
/// wrong things.
fn textsplit(ctx: &EvalCtx, args: &[Expr]) -> Array {
    if let Err(k) = expect_args(args, 2, 4) {
        return fail(k);
    }
    let text = match ctx.eval_text(&args[0]) {
        Ok(t) => t,
        Err(k) => return fail(k),
    };
    let col_delims = match delimiters(ctx, args.get(1)) {
        Ok(d) => d,
        Err(k) => return fail(k),
    };
    let row_delims = match delimiters(ctx, args.get(2)) {
        Ok(d) => d,
        Err(k) => return fail(k),
    };
    let ignore_empty = match flag(ctx, args.get(3), false) {
        Ok(b) => b,
        Err(k) => return fail(k),
    };
    if col_delims.is_empty() && row_delims.is_empty() {
        return fail(ErrorKind::Value);
    }

    let lines = if row_delims.is_empty() {
        vec![text]
    } else {
        split_on(&text, &row_delims)
    };
    let mut rows: Vec<Vec<Value>> = Vec::new();
    for line in lines {
        let parts = if col_delims.is_empty() {
            vec![line]
        } else {
            split_on(&line, &col_delims)
        };
        let parts: Vec<String> = if ignore_empty {
            parts.into_iter().filter(|p| !p.is_empty()).collect()
        } else {
            parts
        };
        if ignore_empty && parts.is_empty() {
            continue;
        }
        rows.push(parts.into_iter().map(Value::Text).collect());
    }
    if rows.is_empty() {
        return fail(ErrorKind::Calc);
    }
    // Ragged lines are padded, because a block is a rectangle. Excel pads
    // with #N/A unless told otherwise, and saying "there was nothing here" is
    // more useful than an empty string that looks like data.
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    for row in &mut rows {
        while row.len() < width {
            row.push(Value::Error(ErrorKind::NA));
        }
    }
    from_rows(rows, width as u32)
}

/// The delimiter or delimiters an argument names. An absent argument means
/// "do not split on this axis".
fn delimiters(ctx: &EvalCtx, e: Option<&Expr>) -> Result<Vec<String>, ErrorKind> {
    let Some(e) = e else {
        return Ok(Vec::new());
    };
    let arr = ctx.eval_array(e);
    let mut out = Vec::new();
    for v in &arr.values {
        if v.is_empty() {
            continue;
        }
        let s = to_text(v)?;
        if !s.is_empty() {
            out.push(s);
        }
    }
    Ok(out)
}

/// Split on any of several delimiters, longest first so `", "` wins over `","`.
fn split_on(text: &str, delims: &[String]) -> Vec<String> {
    let mut ordered: Vec<&String> = delims.iter().collect();
    ordered.sort_by_key(|d| std::cmp::Reverse(d.len()));
    let mut out = Vec::new();
    let mut rest = text;
    'outer: loop {
        for i in 0..rest.len() {
            for d in &ordered {
                if rest[i..].starts_with(d.as_str()) {
                    out.push(rest[..i].to_string());
                    rest = &rest[i + d.len()..];
                    continue 'outer;
                }
            }
        }
        out.push(rest.to_string());
        break;
    }
    out
}

fn flag(ctx: &EvalCtx, e: Option<&Expr>, default: bool) -> Result<bool, ErrorKind> {
    match e {
        None => Ok(default),
        Some(e) => match ctx.eval_scalar(e) {
            Value::Empty => Ok(default),
            v => to_bool(&v),
        },
    }
}

fn number(ctx: &EvalCtx, e: Option<&Expr>, default: f64) -> Result<f64, ErrorKind> {
    match e {
        None => Ok(default),
        Some(e) => match ctx.eval_scalar(e) {
            Value::Empty => Ok(default),
            v => to_number(&v),
        },
    }
}
