//! Formula AST.

use crate::addr::ParsedRef;
use crate::value::ErrorKind;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Concat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    pub fn symbol(&self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Pow => "^",
            BinOp::Concat => "&",
            BinOp::Eq => "=",
            BinOp::Ne => "<>",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
        }
    }
}

/// A cell reference in a formula, optionally sheet-qualified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellRef {
    pub sheet: Option<String>,
    pub r: ParsedRef,
}

/// Which axes a range names explicitly.
///
/// `A:C` and `1:5` are rectangles like any other — the difference is that one
/// axis was never written down, so it belongs to the sheet rather than to the
/// author. That matters in three places: printing the reference back (it has
/// to come out as `A:C`, not `A1:C1048576`), rewriting it when rows are
/// inserted (an axis nobody wrote cannot shift), and evaluating it (the span
/// is a million rows and densifying it would be absurd).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeSpan {
    /// `A1:B2` — both ends name a cell.
    #[default]
    Cells,
    /// `A:C` — every row of those columns.
    Cols,
    /// `1:5` — every column of those rows.
    Rows,
}

impl RangeSpan {
    /// True when the row span came from the sheet rather than the formula.
    pub fn open_rows(&self) -> bool {
        matches!(self, RangeSpan::Cols)
    }

    /// True when the column span came from the sheet rather than the formula.
    pub fn open_cols(&self) -> bool {
        matches!(self, RangeSpan::Rows)
    }

    /// Both ends written out — the ordinary case, and the one left out of the
    /// serialized form. See the note on `RangeRef::span`.
    pub fn is_cells(&self) -> bool {
        matches!(self, RangeSpan::Cells)
    }
}

/// A rectangular range reference in a formula.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeRef {
    pub sheet: Option<String>,
    pub start: ParsedRef,
    pub end: ParsedRef,
    /// Omitted from the serialized form when it is `Cells`.
    ///
    /// Snapshots are content-addressed over canonical JSON of the workbook,
    /// and the AST is in there — so writing `"span":"cells"` on every ordinary
    /// range would have changed the hash of every workbook that contains one,
    /// and every committed trajectory would have stopped replaying. The
    /// dataset check caught exactly that. Skipping the default keeps a
    /// workbook with no open ranges byte-identical to what it was before this
    /// field existed, which is what a refinement ought to be.
    #[serde(default, skip_serializing_if = "RangeSpan::is_cells")]
    pub span: RangeSpan,
}

impl RangeRef {
    /// `A1:B2`, both ends written out.
    pub fn cells(sheet: Option<String>, start: ParsedRef, end: ParsedRef) -> RangeRef {
        RangeRef {
            sheet,
            start,
            end,
            span: RangeSpan::Cells,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Expr {
    Number(f64),
    Text(String),
    Bool(bool),
    Error(ErrorKind),
    Cell(CellRef),
    Range(RangeRef),
    /// Uppercased function name + args.
    Func(String, Vec<Expr>),
    /// A bare identifier that is not a reference: a name LET bound, or — once
    /// defined names exist — one of those. Unbound, it evaluates to #NAME?,
    /// which is what an unknown identifier used to parse as directly.
    Name(String),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    /// Unary plus is kept so source can round-trip.
    Pos(Box<Expr>),
    Percent(Box<Expr>),
}

impl Expr {
    /// Render the AST back to formula text (canonical, without the leading '=').
    pub fn to_formula(&self) -> String {
        match self {
            Expr::Number(n) => crate::value::format_number_general(*n),
            Expr::Text(s) => format!("\"{}\"", s.replace('"', "\"\"")),
            Expr::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
            Expr::Error(e) => e.code().to_string(),
            Expr::Name(n) => n.clone(),
            Expr::Cell(c) => format_sheet_prefix(&c.sheet) + &c.r.to_a1(),
            Expr::Range(r) => {
                let (a, b) = match r.span {
                    RangeSpan::Cells => (r.start.to_a1(), r.end.to_a1()),
                    RangeSpan::Cols => (r.start.to_col(), r.end.to_col()),
                    RangeSpan::Rows => (r.start.to_row(), r.end.to_row()),
                };
                format!("{}{}:{}", format_sheet_prefix(&r.sheet), a, b)
            }
            Expr::Func(name, args) => {
                let args: Vec<String> = args.iter().map(|a| a.to_formula()).collect();
                format!("{}({})", name, args.join(","))
            }
            Expr::Binary(op, l, r) => {
                format!(
                    "{}{}{}",
                    paren_if(l, prec_of(op), false),
                    op.symbol(),
                    paren_if(r, prec_of(op), true)
                )
            }
            Expr::Neg(e) => format!("-{}", paren_if(e, 70, false)),
            Expr::Pos(e) => format!("+{}", paren_if(e, 70, false)),
            Expr::Percent(e) => format!("{}%", paren_if(e, 80, false)),
        }
    }

    /// Visit every cell/range reference in the expression.
    pub fn visit_refs<'a>(&'a self, f: &mut impl FnMut(RefVisit<'a>)) {
        match self {
            Expr::Cell(c) => f(RefVisit::Cell(c)),
            Expr::Range(r) => f(RefVisit::Range(r)),
            Expr::Func(_, args) => {
                for a in args {
                    a.visit_refs(f);
                }
            }
            Expr::Binary(_, l, r) => {
                l.visit_refs(f);
                r.visit_refs(f);
            }
            Expr::Neg(e) | Expr::Pos(e) | Expr::Percent(e) => e.visit_refs(f),
            _ => {}
        }
    }

    /// True if the expression calls a volatile function anywhere.
    pub fn is_volatile(&self) -> bool {
        match self {
            Expr::Func(name, args) => {
                // OFFSET and INDIRECT are volatile for a different reason from the
                // clock functions: the dependency graph is built from the
                // references written in the formula, and neither of these says
                // where it points until it runs. Recalculating them every pass
                // is how Excel solves the same problem.
                matches!(
                    name.as_str(),
                    "NOW" | "TODAY" | "RAND" | "RANDBETWEEN" | "OFFSET" | "INDIRECT"
                ) || args.iter().any(|a| a.is_volatile())
            }
            // A defined name points somewhere the dependency graph cannot
            // see: `visit_refs` walks the expression, and the expression says
            // `Total`, not `Sheet1!$A$1:$A$9`. Same problem OFFSET and
            // INDIRECT have and the same answer — recalculate it every pass —
            // at the same cost, which is why the resolution belongs in the
            // graph eventually and is recorded as a gap rather than hidden.
            Expr::Name(_) => true,
            Expr::Binary(_, l, r) => l.is_volatile() || r.is_volatile(),
            Expr::Neg(e) | Expr::Pos(e) | Expr::Percent(e) => e.is_volatile(),
            _ => false,
        }
    }

    /// True if the expression computes a reference — `OFFSET` or `INDIRECT`
    /// — rather than writing one down.
    ///
    /// Distinct from [`Expr::is_volatile`], which also covers the clock and
    /// random functions. Those recalculate every pass but read nothing, so
    /// their answer cannot be stale; these read cells the dependency graph
    /// never saw, which is a different problem and needs a different fix.
    pub fn has_dynamic_reference(&self) -> bool {
        match self {
            Expr::Func(name, args) => {
                matches!(name.as_str(), "OFFSET" | "INDIRECT")
                    || args.iter().any(|a| a.has_dynamic_reference())
            }
            // A name reads cells the graph never saw, so a pass can evaluate
            // it before the cells it names — the stale-read hazard OFFSET has.
            Expr::Name(_) => true,
            Expr::Binary(_, l, r) => l.has_dynamic_reference() || r.has_dynamic_reference(),
            Expr::Neg(e) | Expr::Pos(e) | Expr::Percent(e) => e.has_dynamic_reference(),
            _ => false,
        }
    }
}

pub enum RefVisit<'a> {
    Cell(&'a CellRef),
    Range(&'a RangeRef),
}

fn format_sheet_prefix(sheet: &Option<String>) -> String {
    match sheet {
        None => String::new(),
        Some(s) => {
            let needs_quote = !s
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
            if needs_quote {
                format!("'{}'!", s.replace('\'', "''"))
            } else {
                format!("{}!", s)
            }
        }
    }
}

/// Operator precedence (higher binds tighter). Matches Excel:
/// comparison < & < +- < */ < ^ < unary minus < %.
pub fn prec_of(op: &BinOp) -> u8 {
    match op {
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 10,
        BinOp::Concat => 20,
        BinOp::Add | BinOp::Sub => 30,
        BinOp::Mul | BinOp::Div => 40,
        BinOp::Pow => 50,
    }
}

fn expr_prec(e: &Expr) -> u8 {
    match e {
        Expr::Binary(op, _, _) => prec_of(op),
        Expr::Neg(_) | Expr::Pos(_) => 70,
        Expr::Percent(_) => 80,
        _ => 100,
    }
}

fn paren_if(e: &Expr, parent_prec: u8, is_right: bool) -> String {
    let p = expr_prec(e);
    // Left-associative operators: right child at equal precedence needs parens.
    if p < parent_prec || (is_right && p == parent_prec) {
        format!("({})", e.to_formula())
    } else {
        e.to_formula()
    }
}
