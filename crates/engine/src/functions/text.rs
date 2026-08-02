//! Text functions.
//!
//! Every position and length is 1-based and counted in characters, never
//! bytes, so multi-byte text can never panic or split mid-character.
//!
//! Expected values in tests are Excel-verified (Microsoft 365).

use super::numfmt;
use super::{expect_args, gather, num_result};
use crate::ast::Expr;
use crate::eval::EvalCtx;
use crate::value::{ErrorKind, Value};

/// Wrap a Result-returning body into a text Value.
fn text_result(r: Result<String, ErrorKind>) -> Value {
    match r {
        Ok(s) => Value::Text(s),
        Err(k) => Value::Error(k),
    }
}

/// Shared body for the single-argument string transforms.
fn map_text(ctx: &EvalCtx, args: &[Expr], f: impl Fn(&str) -> String) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    text_result(ctx.eval_text(&args[0]).map(|s| f(s.as_str())))
}

/// Optional character-count argument (LEFT/RIGHT): defaults to 1, negative
/// is #VALUE!.
fn count_arg(ctx: &EvalCtx, e: Option<&Expr>) -> Result<usize, ErrorKind> {
    let n = match e {
        Some(x) => ctx.eval_number(x)?,
        None => 1.0,
    };
    if n < 0.0 {
        return Err(ErrorKind::Value);
    }
    Ok(n.trunc() as usize)
}

/// Optional start-position argument (FIND/SEARCH): defaults to 1, below 1 is
/// #VALUE!.
fn start_arg(ctx: &EvalCtx, e: Option<&Expr>) -> Result<usize, ErrorKind> {
    let n = match e {
        Some(x) => ctx.eval_number(x)?,
        None => 1.0,
    };
    if n < 1.0 {
        return Err(ErrorKind::Value);
    }
    Ok(n.trunc() as usize)
}

/// Flatten arguments into a dense list: range cells come out in row-major
/// order *including* blanks, which TEXTJOIN needs when ignore_empty is FALSE.
fn flatten(ctx: &EvalCtx, args: &[Expr]) -> Vec<Value> {
    let mut out = Vec::new();
    for a in args {
        let (grid, _) = ctx.eval_grid(a);
        for row in grid {
            out.extend(row);
        }
    }
    out
}

pub fn concat(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, usize::MAX) {
        return Value::Error(k);
    }
    let mut out = String::new();
    for g in gather(ctx, args) {
        if let Some(k) = g.value.as_error() {
            return Value::Error(k);
        }
        out.push_str(&g.value.display());
    }
    Value::Text(out)
}

/// CONCATENATE predates dynamic arrays and takes scalars only; a multi-cell
/// range is #VALUE!, which is exactly what scalar evaluation yields.
pub fn concatenate(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, usize::MAX) {
        return Value::Error(k);
    }
    let mut out = String::new();
    for a in args {
        match ctx.eval_text(a) {
            Ok(s) => out.push_str(&s),
            Err(k) => return Value::Error(k),
        }
    }
    Value::Text(out)
}

pub fn textjoin(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 3, usize::MAX) {
        return Value::Error(k);
    }
    text_result((|| -> Result<String, ErrorKind> {
        let delim = ctx.eval_text(&args[0])?;
        let ignore_empty = ctx.eval_bool(&args[1])?;
        let mut parts: Vec<String> = Vec::new();
        for v in flatten(ctx, &args[2..]) {
            if let Some(k) = v.as_error() {
                return Err(k);
            }
            let s = v.display();
            if ignore_empty && s.is_empty() {
                continue;
            }
            parts.push(s);
        }
        Ok(parts.join(&delim))
    })())
}

pub fn left(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 2) {
        return Value::Error(k);
    }
    text_result((|| -> Result<String, ErrorKind> {
        let s = ctx.eval_text(&args[0])?;
        let n = count_arg(ctx, args.get(1))?;
        Ok(s.chars().take(n).collect::<String>())
    })())
}

pub fn right(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 2) {
        return Value::Error(k);
    }
    text_result((|| -> Result<String, ErrorKind> {
        let s = ctx.eval_text(&args[0])?;
        let n = count_arg(ctx, args.get(1))?;
        let cs: Vec<char> = s.chars().collect();
        let n = n.min(cs.len());
        Ok(cs[cs.len() - n..].iter().collect::<String>())
    })())
}

pub fn mid(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 3, 3) {
        return Value::Error(k);
    }
    text_result((|| -> Result<String, ErrorKind> {
        let s = ctx.eval_text(&args[0])?;
        let start = ctx.eval_number(&args[1])?;
        let n = ctx.eval_number(&args[2])?;
        if start < 1.0 || n < 0.0 {
            return Err(ErrorKind::Value);
        }
        let start = start.trunc() as usize;
        let n = n.trunc() as usize;
        Ok(s.chars().skip(start - 1).take(n).collect::<String>())
    })())
}

pub fn len(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    num_result(ctx.eval_text(&args[0]).map(|s| s.chars().count() as f64))
}

pub fn trim(ctx: &EvalCtx, args: &[Expr]) -> Value {
    map_text(ctx, args, trim_spaces)
}

pub fn upper(ctx: &EvalCtx, args: &[Expr]) -> Value {
    map_text(ctx, args, str::to_uppercase)
}

pub fn lower(ctx: &EvalCtx, args: &[Expr]) -> Value {
    map_text(ctx, args, str::to_lowercase)
}

pub fn proper(ctx: &EvalCtx, args: &[Expr]) -> Value {
    map_text(ctx, args, proper_case)
}

pub fn substitute(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 3, 4) {
        return Value::Error(k);
    }
    text_result((|| -> Result<String, ErrorKind> {
        let text = ctx.eval_text(&args[0])?;
        let old = ctx.eval_text(&args[1])?;
        let new = ctx.eval_text(&args[2])?;
        let instance = match args.get(3) {
            Some(e) => {
                let n = ctx.eval_number(e)?;
                if n < 1.0 {
                    return Err(ErrorKind::Value);
                }
                Some(n.trunc() as usize)
            }
            None => None,
        };
        Ok(subst_text(&text, &old, &new, instance))
    })())
}

pub fn replace_fn(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 4, 4) {
        return Value::Error(k);
    }
    text_result((|| -> Result<String, ErrorKind> {
        let text = ctx.eval_text(&args[0])?;
        let start = ctx.eval_number(&args[1])?;
        let num = ctx.eval_number(&args[2])?;
        let new = ctx.eval_text(&args[3])?;
        if start < 1.0 || num < 0.0 {
            return Err(ErrorKind::Value);
        }
        Ok(replace_range(
            &text,
            start.trunc() as usize,
            num.trunc() as usize,
            &new,
        ))
    })())
}

/// FIND is case-sensitive and takes no wildcards.
pub fn find(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 3) {
        return Value::Error(k);
    }
    num_result((|| -> Result<f64, ErrorKind> {
        let needle = ctx.eval_text(&args[0])?;
        let hay = ctx.eval_text(&args[1])?;
        let start = start_arg(ctx, args.get(2))?;
        find_index(&needle, &hay, start)
            .map(|p| p as f64)
            .ok_or(ErrorKind::Value)
    })())
}

/// SEARCH is case-insensitive and honors `*` / `?` wildcards (`~` escapes).
pub fn search(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 3) {
        return Value::Error(k);
    }
    num_result((|| -> Result<f64, ErrorKind> {
        let pattern = ctx.eval_text(&args[0])?;
        let hay = ctx.eval_text(&args[1])?;
        let start = start_arg(ctx, args.get(2))?;
        search_index(&pattern, &hay, start)
            .map(|p| p as f64)
            .ok_or(ErrorKind::Value)
    })())
}

pub fn text(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 2) {
        return Value::Error(k);
    }
    let v = ctx.eval_scalar(&args[0]);
    if let Some(k) = v.as_error() {
        return Value::Error(k);
    }
    let code = match ctx.eval_text(&args[1]) {
        Ok(s) => s,
        Err(k) => return Value::Error(k),
    };
    text_result(numfmt::format_value(&v, &code))
}

pub fn value(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    num_result((|| -> Result<f64, ErrorKind> {
        let s = ctx.eval_text(&args[0])?;
        crate::eval::parse_number_text(&s)
            .or_else(|| crate::serial::parse_date_text(&s))
            .ok_or(ErrorKind::Value)
    })())
}

// ------------------------------------------------------------- pure helpers

/// Excel's TRIM: drop leading/trailing spaces and collapse internal runs to
/// one space. Only U+0020 counts, unlike Rust's `trim`.
fn trim_spaces(s: &str) -> String {
    s.split(' ')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A word starts after any non-alphabetic character, so "o'neil" becomes
/// "O'Neil" and "2nd" becomes "2Nd", matching Excel.
fn proper_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut word_start = true;
    for c in s.chars() {
        if word_start {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
        word_start = !c.is_alphabetic();
    }
    out
}

/// SUBSTITUTE is case-sensitive; an empty `old` leaves the text alone.
fn subst_text(text: &str, old: &str, new: &str, instance: Option<usize>) -> String {
    if old.is_empty() {
        return text.to_string();
    }
    let hay: Vec<char> = text.chars().collect();
    let needle: Vec<char> = old.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut seen = 0usize;
    while i < hay.len() {
        if i + needle.len() <= hay.len() && hay[i..i + needle.len()] == needle[..] {
            seen += 1;
            if instance.is_none_or(|k| k == seen) {
                out.push_str(new);
            } else {
                out.extend(&hay[i..i + needle.len()]);
            }
            i += needle.len();
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    out
}

/// REPLACE with a 1-based start; a start past the end appends.
fn replace_range(text: &str, start: usize, num: usize, new: &str) -> String {
    let cs: Vec<char> = text.chars().collect();
    let a = start.saturating_sub(1).min(cs.len());
    let b = a.saturating_add(num).min(cs.len());
    let mut out: String = cs[..a].iter().collect();
    out.push_str(new);
    out.extend(&cs[b..]);
    out
}

/// 1-based position of `needle` in `hay` at or after the 1-based `start`.
/// An empty needle matches at `start` (Excel allows start == len + 1).
fn find_index(needle: &str, hay: &str, start: usize) -> Option<usize> {
    let h: Vec<char> = hay.chars().collect();
    let n: Vec<char> = needle.chars().collect();
    let from = start.checked_sub(1)?;
    if from > h.len() {
        return None;
    }
    if n.is_empty() {
        return Some(start);
    }
    (from..h.len())
        .find(|&p| p + n.len() <= h.len() && h[p..p + n.len()] == n[..])
        .map(|p| p + 1)
}

#[derive(Debug, Clone, PartialEq)]
enum Pat {
    Star,
    Any,
    Ch(char),
}

fn parse_wildcard(pattern: &str) -> Vec<Pat> {
    let cs: Vec<char> = pattern.chars().collect();
    let mut out = Vec::with_capacity(cs.len());
    let mut i = 0;
    while i < cs.len() {
        match cs[i] {
            '~' => match cs.get(i + 1) {
                Some(c) if *c == '*' || *c == '?' || *c == '~' => {
                    out.push(Pat::Ch(*c));
                    i += 2;
                }
                _ => {
                    out.push(Pat::Ch('~'));
                    i += 1;
                }
            },
            '*' => {
                out.push(Pat::Star);
                i += 1;
            }
            '?' => {
                out.push(Pat::Any);
                i += 1;
            }
            c => {
                out.push(Pat::Ch(c));
                i += 1;
            }
        }
    }
    out
}

fn eq_ci(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// True when the pattern matches some prefix of `text` (SEARCH reports where
/// a match starts, not that it consumes the whole string).
fn wildcard_prefix(text: &[char], pat: &[Pat]) -> bool {
    match pat.first() {
        None => true,
        Some(Pat::Star) => (0..=text.len()).any(|k| wildcard_prefix(&text[k..], &pat[1..])),
        Some(Pat::Any) => !text.is_empty() && wildcard_prefix(&text[1..], &pat[1..]),
        Some(Pat::Ch(c)) => {
            !text.is_empty() && eq_ci(text[0], *c) && wildcard_prefix(&text[1..], &pat[1..])
        }
    }
}

fn search_index(pattern: &str, hay: &str, start: usize) -> Option<usize> {
    let h: Vec<char> = hay.chars().collect();
    let from = start.checked_sub(1)?;
    if from > h.len() {
        return None;
    }
    let pat = parse_wildcard(pattern);
    (from..=h.len())
        .find(|&p| wildcard_prefix(&h[p..], &pat))
        .map(|p| p + 1)
}

// ---------------------------------------------------------------------------
// Repetition, exact comparison, and the character/code pair
// ---------------------------------------------------------------------------

/// REPT(text, count): the text repeated. Excel caps a cell at 32767
/// characters and answers #VALUE! past it rather than building the string.
pub fn rept(ctx: &EvalCtx, args: &[Expr]) -> Value {
    const MAX_CELL_CHARS: f64 = 32_767.0;
    if let Err(k) = expect_args(args, 2, 2) {
        return Value::Error(k);
    }
    text_result((|| {
        let s = ctx.eval_text(&args[0])?;
        let n = ctx.eval_number(&args[1])?.trunc();
        if n < 0.0 {
            return Err(ErrorKind::Value);
        }
        if s.chars().count() as f64 * n > MAX_CELL_CHARS {
            return Err(ErrorKind::Value);
        }
        Ok(s.repeat(n as usize))
    })())
}

/// EXACT(a, b): the case-sensitive comparison, which `=` is not.
pub fn exact(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 2, 2) {
        return Value::Error(k);
    }
    match (ctx.eval_text(&args[0]), ctx.eval_text(&args[1])) {
        (Ok(a), Ok(b)) => Value::Bool(a == b),
        (Err(k), _) | (_, Err(k)) => Value::Error(k),
    }
}

/// CHAR(code): the character for a code point, 1..=255.
///
/// Excel's range is a byte because the function predates Unicode; UNICHAR is
/// the one that goes further, and we do not have it yet.
pub fn char_fn(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    text_result((|| {
        let n = ctx.eval_number(&args[0])?.trunc();
        if !(1.0..=255.0).contains(&n) {
            return Err(ErrorKind::Value);
        }
        char::from_u32(n as u32)
            .map(String::from)
            .ok_or(ErrorKind::Value)
    })())
}

/// CODE(text): the code point of the first character. Empty text is #VALUE!.
pub fn code(ctx: &EvalCtx, args: &[Expr]) -> Value {
    if let Err(k) = expect_args(args, 1, 1) {
        return Value::Error(k);
    }
    num_result((|| {
        let s = ctx.eval_text(&args[0])?;
        s.chars()
            .next()
            .map(|c| c as u32 as f64)
            .ok_or(ErrorKind::Value)
    })())
}

/// CLEAN(text): strip the non-printing characters a mainframe export leaves
/// behind. Excel removes the first 32 ASCII control codes and nothing else.
pub fn clean(ctx: &EvalCtx, args: &[Expr]) -> Value {
    map_text(ctx, args, |s| {
        s.chars().filter(|c| (*c as u32) >= 32).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_collapses_internal_runs() {
        assert_eq!(trim_spaces("  hello   world  "), "hello world");
        assert_eq!(trim_spaces("abc"), "abc");
        assert_eq!(trim_spaces("   "), "");
        assert_eq!(trim_spaces(""), "");
    }

    #[test]
    fn proper_capitalizes_after_non_letters() {
        assert_eq!(proper_case("hello world"), "Hello World");
        assert_eq!(proper_case("HELLO WORLD"), "Hello World");
        assert_eq!(proper_case("o'neil-smith"), "O'Neil-Smith");
        assert_eq!(proper_case("2nd place"), "2Nd Place");
        assert_eq!(proper_case("éclair test"), "Éclair Test");
    }

    #[test]
    fn substitute_all_or_nth() {
        assert_eq!(subst_text("a-b-c", "-", "+", None), "a+b+c");
        assert_eq!(subst_text("a-b-c", "-", "+", Some(2)), "a-b+c");
        assert_eq!(subst_text("a-b-c", "-", "+", Some(3)), "a-b-c");
        assert_eq!(subst_text("banana", "an", "AN", None), "bANANa");
        assert_eq!(subst_text("abc", "", "x", None), "abc");
        assert_eq!(subst_text("abc", "z", "x", None), "abc");
        // Case-sensitive.
        assert_eq!(subst_text("Abc abc", "abc", "x", None), "Abc x");
        // Removal.
        assert_eq!(subst_text("a b c", " ", "", None), "abc");
    }

    #[test]
    fn replace_is_one_based_over_chars() {
        assert_eq!(replace_range("abcdef", 2, 3, "XY"), "aXYef");
        assert_eq!(replace_range("abcdef", 1, 0, "X"), "Xabcdef");
        assert_eq!(replace_range("abc", 10, 2, "X"), "abcX");
        assert_eq!(replace_range("abc", 2, 99, "X"), "aX");
        assert_eq!(replace_range("héllo", 2, 1, "e"), "hello");
    }

    #[test]
    fn find_is_case_sensitive() {
        assert_eq!(find_index("b", "abcabc", 1), Some(2));
        assert_eq!(find_index("b", "abcabc", 3), Some(5));
        assert_eq!(find_index("B", "abcabc", 1), None);
        assert_eq!(find_index("z", "abc", 1), None);
        assert_eq!(find_index("", "abc", 1), Some(1));
        assert_eq!(find_index("", "abc", 4), Some(4));
        assert_eq!(find_index("", "abc", 5), None);
        assert_eq!(find_index("c", "abc", 4), None);
        // Characters, not bytes.
        assert_eq!(find_index("é", "aébc", 1), Some(2));
        assert_eq!(find_index("b", "aébc", 1), Some(3));
    }

    #[test]
    fn search_is_case_insensitive_with_wildcards() {
        assert_eq!(search_index("B", "abcabc", 1), Some(2));
        assert_eq!(search_index("b?c", "xxabc", 1), None);
        assert_eq!(search_index("a?c", "xxabc", 1), Some(3));
        assert_eq!(search_index("b*e", "abcde", 1), Some(2));
        assert_eq!(search_index("*", "abc", 1), Some(1));
        assert_eq!(search_index("", "abc", 2), Some(2));
        assert_eq!(search_index("z", "abc", 1), None);
        // ~ escapes a wildcard.
        assert_eq!(search_index("~?", "ab?c", 1), Some(3));
        assert_eq!(search_index("~*", "a*b", 1), Some(2));
        assert_eq!(search_index("?", "ab?c", 1), Some(1));
    }

    #[test]
    fn wildcard_matching_prefixes() {
        let text: Vec<char> = "abcde".chars().collect();
        assert!(wildcard_prefix(&text, &parse_wildcard("abc")));
        assert!(wildcard_prefix(&text, &parse_wildcard("a*d")));
        assert!(wildcard_prefix(&text, &parse_wildcard("")));
        assert!(!wildcard_prefix(&text, &parse_wildcard("b")));
        assert!(!wildcard_prefix(&text, &parse_wildcard("abcdef")));
    }
}
