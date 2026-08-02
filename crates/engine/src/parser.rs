//! Formula lexer + Pratt parser with Excel operator precedence.
//!
//! Grammar notes:
//! - Function names are case-insensitive and uppercased in the AST.
//! - An identifier followed by `(` is a function call; otherwise we try to
//!   read it as a cell reference. Unknown bare identifiers parse as a
//!   `#NAME?` error node (named ranges are not in v1).
//! - `^` is left-associative (Excel: 2^3^2 = 64).
//! - Unary minus binds tighter than `^` (Excel: -2^2 = 4).
//! - `%` is a postfix operator.

use crate::addr::ParsedRef;
use crate::ast::{BinOp, CellRef, Expr, RangeRef};
use crate::value::ErrorKind;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    /// Bare identifier or A1-style token (function name, TRUE/FALSE, or ref).
    Ident(String),
    /// Quoted sheet name (already unescaped), always followed by `!` in valid input.
    QuotedSheet(String),
    ErrorLit(ErrorKind),
    LParen,
    RParen,
    Comma,
    Colon,
    Bang,
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    Amp,
    Percent,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ParseError {
    #[error("unexpected character '{0}' at offset {1}")]
    UnexpectedChar(char, usize),
    #[error("unexpected end of formula")]
    UnexpectedEnd,
    #[error("unexpected token at offset {0}")]
    UnexpectedToken(usize),
    #[error("unterminated string literal")]
    UnterminatedString,
    #[error("invalid reference '{0}'")]
    BadRef(String),
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Lexer { src, pos: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn tokenize(mut self) -> Result<Vec<(Tok, usize)>, ParseError> {
        let mut out = Vec::new();
        loop {
            let rest = self.rest();
            let Some(c) = rest.chars().next() else {
                break;
            };
            let start = self.pos;
            if c.is_whitespace() {
                self.pos += c.len_utf8();
                continue;
            }
            let tok = match c {
                '(' => {
                    self.pos += 1;
                    Tok::LParen
                }
                ')' => {
                    self.pos += 1;
                    Tok::RParen
                }
                ',' => {
                    self.pos += 1;
                    Tok::Comma
                }
                ':' => {
                    self.pos += 1;
                    Tok::Colon
                }
                '!' => {
                    self.pos += 1;
                    Tok::Bang
                }
                '+' => {
                    self.pos += 1;
                    Tok::Plus
                }
                '-' => {
                    self.pos += 1;
                    Tok::Minus
                }
                '*' => {
                    self.pos += 1;
                    Tok::Star
                }
                '/' => {
                    self.pos += 1;
                    Tok::Slash
                }
                '^' => {
                    self.pos += 1;
                    Tok::Caret
                }
                '&' => {
                    self.pos += 1;
                    Tok::Amp
                }
                '%' => {
                    self.pos += 1;
                    Tok::Percent
                }
                '=' => {
                    self.pos += 1;
                    Tok::Eq
                }
                '<' => {
                    if rest.starts_with("<>") {
                        self.pos += 2;
                        Tok::Ne
                    } else if rest.starts_with("<=") {
                        self.pos += 2;
                        Tok::Le
                    } else {
                        self.pos += 1;
                        Tok::Lt
                    }
                }
                '>' => {
                    if rest.starts_with(">=") {
                        self.pos += 2;
                        Tok::Ge
                    } else {
                        self.pos += 1;
                        Tok::Gt
                    }
                }
                '"' => self.lex_string()?,
                '\'' => self.lex_quoted_sheet()?,
                '#' => self.lex_error_literal()?,
                c if c.is_ascii_digit() || c == '.' => self.lex_number()?,
                c if c.is_alphabetic() || c == '$' || c == '_' => self.lex_ident(),
                other => return Err(ParseError::UnexpectedChar(other, start)),
            };
            out.push((tok, start));
        }
        Ok(out)
    }

    fn lex_string(&mut self) -> Result<Tok, ParseError> {
        // Skip opening quote; "" inside is an escaped quote.
        self.pos += 1;
        let mut s = String::new();
        loop {
            let rest = self.rest();
            match rest.chars().next() {
                None => return Err(ParseError::UnterminatedString),
                Some('"') => {
                    if rest[1..].starts_with('"') {
                        s.push('"');
                        self.pos += 2;
                    } else {
                        self.pos += 1;
                        return Ok(Tok::Str(s));
                    }
                }
                Some(c) => {
                    s.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    fn lex_quoted_sheet(&mut self) -> Result<Tok, ParseError> {
        self.pos += 1;
        let mut s = String::new();
        loop {
            let rest = self.rest();
            match rest.chars().next() {
                None => return Err(ParseError::UnterminatedString),
                Some('\'') => {
                    if rest[1..].starts_with('\'') {
                        s.push('\'');
                        self.pos += 2;
                    } else {
                        self.pos += 1;
                        return Ok(Tok::QuotedSheet(s));
                    }
                }
                Some(c) => {
                    s.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    fn lex_error_literal(&mut self) -> Result<Tok, ParseError> {
        // Longest-match against the known error codes.
        const CODES: [&str; 7] = [
            "#DIV/0!", "#VALUE!", "#REF!", "#NAME?", "#N/A", "#NUM!", "#CIRC!",
        ];
        let rest = self.rest();
        for code in CODES {
            if rest.len() >= code.len() && rest[..code.len()].eq_ignore_ascii_case(code) {
                self.pos += code.len();
                return Ok(Tok::ErrorLit(ErrorKind::from_code(code).unwrap()));
            }
        }
        Err(ParseError::UnexpectedChar('#', self.pos))
    }

    fn lex_number(&mut self) -> Result<Tok, ParseError> {
        let rest = self.rest();
        let bytes = rest.as_bytes();
        let mut i = 0;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
        // Exponent: only if followed by digits (else "E" starts an identifier/ref).
        if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
            let mut j = i + 1;
            if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
                j += 1;
            }
            if j < bytes.len() && bytes[j].is_ascii_digit() {
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                i = j;
            }
        }
        let text = &rest[..i];
        let n: f64 = text
            .parse()
            .map_err(|_| ParseError::UnexpectedChar('.', self.pos))?;
        self.pos += i;
        Ok(Tok::Num(n))
    }

    fn lex_ident(&mut self) -> Tok {
        let rest = self.rest();
        let mut end = 0;
        for c in rest.chars() {
            if c.is_alphanumeric() || c == '$' || c == '_' || c == '.' {
                end += c.len_utf8();
            } else {
                break;
            }
        }
        let text = &rest[..end];
        self.pos += end;
        Tok::Ident(text.to_string())
    }
}

pub struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
}

/// Uppercase a function name and strip the xlsx storage prefixes.
///
/// The file format stores functions introduced after Excel 2007 with an
/// `_xlfn.` prefix (and worksheet-scoped ones with a further `_xlws.`), which
/// Excel hides from the user. Without stripping them, every real workbook
/// using TEXTJOIN, XLOOKUP, IFS or CONCAT would import as `#NAME?`.
pub fn normalize_func_name(raw: &str) -> String {
    let upper = raw.to_uppercase();
    let stripped = upper.strip_prefix("_XLFN.").unwrap_or(&upper);
    stripped
        .strip_prefix("_XLWS.")
        .unwrap_or(stripped)
        .to_string()
}

/// Parse formula body text (without the leading '=').
pub fn parse_formula(src: &str) -> Result<Expr, ParseError> {
    let toks = Lexer::new(src).tokenize()?;
    let mut p = Parser { toks, pos: 0 };
    let e = p.parse_expr(0)?;
    if p.pos != p.toks.len() {
        return Err(ParseError::UnexpectedToken(p.toks[p.pos].1));
    }
    Ok(e)
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|(t, _)| t)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).map(|(t, _)| t.clone());
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn expect(&mut self, t: Tok) -> Result<(), ParseError> {
        match self.next() {
            Some(got) if got == t => Ok(()),
            Some(_) => Err(ParseError::UnexpectedToken(self.toks[self.pos - 1].1)),
            None => Err(ParseError::UnexpectedEnd),
        }
    }

    fn parse_expr(&mut self, min_prec: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Eq) => BinOp::Eq,
                Some(Tok::Ne) => BinOp::Ne,
                Some(Tok::Lt) => BinOp::Lt,
                Some(Tok::Le) => BinOp::Le,
                Some(Tok::Gt) => BinOp::Gt,
                Some(Tok::Ge) => BinOp::Ge,
                Some(Tok::Amp) => BinOp::Concat,
                Some(Tok::Plus) => BinOp::Add,
                Some(Tok::Minus) => BinOp::Sub,
                Some(Tok::Star) => BinOp::Mul,
                Some(Tok::Slash) => BinOp::Div,
                Some(Tok::Caret) => BinOp::Pow,
                _ => break,
            };
            let prec = crate::ast::prec_of(&op);
            if prec < min_prec {
                break;
            }
            self.next();
            // All Excel binary operators are left-associative.
            let rhs = self.parse_expr(prec + 1)?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        match self.peek() {
            Some(Tok::Minus) => {
                self.next();
                let e = self.parse_unary()?;
                Ok(Expr::Neg(Box::new(e)))
            }
            Some(Tok::Plus) => {
                self.next();
                let e = self.parse_unary()?;
                Ok(Expr::Pos(Box::new(e)))
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.parse_primary()?;
        while self.peek() == Some(&Tok::Percent) {
            self.next();
            e = Expr::Percent(Box::new(e));
        }
        Ok(e)
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let at = self.toks.get(self.pos).map(|(_, p)| *p).unwrap_or(0);
        match self.next() {
            None => Err(ParseError::UnexpectedEnd),
            Some(Tok::Num(n)) => Ok(Expr::Number(n)),
            Some(Tok::Str(s)) => Ok(Expr::Text(s)),
            Some(Tok::ErrorLit(e)) => Ok(Expr::Error(e)),
            Some(Tok::LParen) => {
                let e = self.parse_expr(0)?;
                self.expect(Tok::RParen)?;
                Ok(e)
            }
            Some(Tok::QuotedSheet(name)) => {
                self.expect(Tok::Bang)?;
                self.parse_ref_after_sheet(Some(name))
            }
            Some(Tok::Ident(id)) => {
                // Sheet-qualified reference: Ident '!' ref
                if self.peek() == Some(&Tok::Bang) {
                    self.next();
                    return self.parse_ref_after_sheet(Some(id));
                }
                // Function call: Ident '('
                if self.peek() == Some(&Tok::LParen) {
                    self.next();
                    let mut args = Vec::new();
                    if self.peek() != Some(&Tok::RParen) {
                        loop {
                            // Empty argument (e.g. IF(A1,,2) or SUM(1,)) parses as 0.
                            if self.peek() == Some(&Tok::Comma) || self.peek() == Some(&Tok::RParen)
                            {
                                args.push(Expr::Number(0.0));
                            } else {
                                args.push(self.parse_expr(0)?);
                            }
                            if self.peek() == Some(&Tok::Comma) {
                                self.next();
                                continue;
                            }
                            break;
                        }
                    }
                    self.expect(Tok::RParen)?;
                    return Ok(Expr::Func(normalize_func_name(&id), args));
                }
                let upper = id.to_uppercase();
                if upper == "TRUE" {
                    return Ok(Expr::Bool(true));
                }
                if upper == "FALSE" {
                    return Ok(Expr::Bool(false));
                }
                self.finish_ref_or_range(None, &id, at)
            }
            Some(_) => Err(ParseError::UnexpectedToken(at)),
        }
    }

    fn parse_ref_after_sheet(&mut self, sheet: Option<String>) -> Result<Expr, ParseError> {
        let at = self.toks.get(self.pos).map(|(_, p)| *p).unwrap_or(0);
        match self.next() {
            Some(Tok::Ident(id)) => self.finish_ref_or_range(sheet, &id, at),
            _ => Err(ParseError::UnexpectedToken(at)),
        }
    }

    fn finish_ref_or_range(
        &mut self,
        sheet: Option<String>,
        first: &str,
        at: usize,
    ) -> Result<Expr, ParseError> {
        let Some(start) = ParsedRef::parse(first) else {
            // Unknown bare identifier: evaluates to #NAME? (no named ranges in v1).
            let _ = at;
            return Ok(Expr::Error(ErrorKind::Name));
        };
        if self.peek() == Some(&Tok::Colon) {
            self.next();
            let at2 = self.toks.get(self.pos).map(|(_, p)| *p).unwrap_or(0);
            match self.next() {
                Some(Tok::Ident(id2)) => {
                    let end = ParsedRef::parse(&id2)
                        .ok_or_else(|| ParseError::BadRef(id2.to_string()))?;
                    Ok(Expr::Range(RangeRef { sheet, start, end }))
                }
                _ => Err(ParseError::UnexpectedToken(at2)),
            }
        } else {
            Ok(Expr::Cell(CellRef { sheet, r: start }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Expr::*;

    fn p(s: &str) -> Expr {
        parse_formula(s).unwrap_or_else(|e| panic!("parse {s}: {e}"))
    }

    #[test]
    fn precedence_arith() {
        // 1+2*3 => 1+(2*3)
        assert_eq!(p("1+2*3").to_formula(), "1+2*3");
        assert_eq!(p("(1+2)*3").to_formula(), "(1+2)*3");
        // ^ left-assoc: 2^3^2 = (2^3)^2
        match p("2^3^2") {
            Binary(crate::ast::BinOp::Pow, l, _) => {
                assert!(matches!(*l, Binary(crate::ast::BinOp::Pow, _, _)))
            }
            other => panic!("{other:?}"),
        }
        // unary minus binds tighter than ^: -2^2 => (-2)^2
        match p("-2^2") {
            Binary(crate::ast::BinOp::Pow, l, _) => assert!(matches!(*l, Neg(_))),
            other => panic!("{other:?}"),
        }
        // comparison lowest: 1+2=3 => (1+2)=3
        match p("1+2=3") {
            Binary(crate::ast::BinOp::Eq, _, _) => {}
            other => panic!("{other:?}"),
        }
        // concat below arithmetic: "a"&1+2 => "a"&(1+2)
        match p("\"a\"&1+2") {
            Binary(crate::ast::BinOp::Concat, _, r) => {
                assert!(matches!(*r, Binary(crate::ast::BinOp::Add, _, _)))
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn percent_postfix() {
        assert_eq!(p("50%").to_formula(), "50%");
        match p("50%%") {
            Percent(inner) => assert!(matches!(*inner, Percent(_))),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn refs_and_ranges() {
        match p("A1") {
            Cell(c) => {
                assert_eq!(c.r.to_a1(), "A1");
                assert_eq!(c.sheet, None);
            }
            other => panic!("{other:?}"),
        }
        match p("$B$2:C3") {
            Range(r) => {
                assert_eq!(r.start.to_a1(), "$B$2");
                assert_eq!(r.end.to_a1(), "C3");
            }
            other => panic!("{other:?}"),
        }
        match p("Sheet2!A1") {
            Cell(c) => assert_eq!(c.sheet.as_deref(), Some("Sheet2")),
            other => panic!("{other:?}"),
        }
        match p("'My Sheet'!A1:B2") {
            Range(r) => assert_eq!(r.sheet.as_deref(), Some("My Sheet")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn functions() {
        match p("sum(A1:A3,2)") {
            Func(name, args) => {
                assert_eq!(name, "SUM");
                assert_eq!(args.len(), 2);
            }
            other => panic!("{other:?}"),
        }
        // Function-name-vs-ref ambiguity: LOG10 is a valid address, but with
        // '(' it must be a function.
        match p("LOG10(100)") {
            Func(name, _) => assert_eq!(name, "LOG10"),
            other => panic!("{other:?}"),
        }
        match p("LOG10") {
            Cell(c) => assert_eq!(c.r.to_a1(), "LOG10"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn literals() {
        assert_eq!(p("\"a\"\"b\""), Text("a\"b".into()));
        assert_eq!(p("TRUE"), Bool(true));
        assert_eq!(p("false"), Bool(false));
        assert_eq!(p("#N/A"), Error(crate::value::ErrorKind::NA));
        assert_eq!(p("1.5e2"), Number(150.0));
        assert_eq!(p(".5"), Number(0.5));
    }

    #[test]
    fn xlsx_future_function_prefixes_are_stripped() {
        // xlsx stores post-2007 functions prefixed; Excel hides that.
        for (src, want) in [
            ("_xlfn.TEXTJOIN(\",\",TRUE,A1:A2)", "TEXTJOIN"),
            ("_xlfn.XLOOKUP(1,A1:A2,B1:B2)", "XLOOKUP"),
            ("_xlfn.IFS(A1,1)", "IFS"),
            ("_xlfn._xlws.FILTER(A1:A2,B1:B2)", "FILTER"),
        ] {
            match p(src) {
                Func(name, _) => assert_eq!(name, want, "for {src}"),
                other => panic!("{src} parsed as {other:?}"),
            }
        }
    }

    #[test]
    fn unknown_name_is_name_error() {
        assert_eq!(p("FOO_BAR"), Error(crate::value::ErrorKind::Name));
    }

    #[test]
    fn errors() {
        assert!(parse_formula("1+").is_err());
        assert!(parse_formula("(1").is_err());
        assert!(parse_formula("\"abc").is_err());
        assert!(parse_formula("1 2").is_err());
    }

    #[test]
    fn round_trip_canonical() {
        for f in [
            "1+2*3",
            "(1+2)*3",
            "-A1^2",
            "SUM(A1:B9,3)",
            "IF(A1>2,\"yes\",\"no\")",
            "Sheet2!A1+'My Sheet'!B2",
            "50%",
            "\"a\"&\"b\"",
        ] {
            let ast = p(f);
            let rendered = ast.to_formula();
            let reparsed = p(&rendered);
            assert_eq!(ast, reparsed, "round trip {f} -> {rendered}");
        }
    }
}
