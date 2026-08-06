//! Turning a plan into actions, against the workbook actually in front of it.
//!
//! This is the half of the agent that knows where things are. The planner
//! says "add a Total column that is Qty times Price"; this decides that Qty
//! is column B, Price is column C, the body runs from row 2 to row 6, and the
//! new column goes in D — and emits the `engine::Action` values that do it.
//!
//! Two rules govern everything here.
//!
//! **Resolution fails loudly.** A header that is not there, a placeholder
//! that matches no column, a fill with nothing to copy: each is a
//! `CompileError` naming what it looked for and what it found. The
//! alternative — guessing the closest column — produces an agent that
//! confidently does the wrong thing, which is worse than one that stops.
//!
//! **Nothing is compiled from an address the planner supplied.** The one
//! exception is `ApplyFormula`'s `at`, which is a location the plan really
//! does have to name (a grand total goes *somewhere*), and even that is
//! resolved against the located table's sheet rather than assumed.

use engine::{Action, CellAddr, Engine, FilterSpec, RangeAddr, Value};
use env::observe::{TableView, WorkbookObservation};
use serde::{Deserialize, Serialize};

use crate::plan::{Aggregate, ColumnRef, FormulaTemplate, Predicate, RowRange, Step};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CompileError {
    #[error("no table has been located yet; the plan must start with locate_table")]
    NoSubject,
    #[error("no table found with the headers {0:?}")]
    NoSuchTable(Vec<String>),
    #[error("no column headed {looked_for:?}; this table has {available:?}")]
    NoSuchColumn {
        looked_for: String,
        available: Vec<String>,
    },
    #[error("no defined name {0}")]
    NoSuchName(String),
    #[error("{0} is not an address")]
    NotAnAddress(String),
    #[error("{0} has no formula to fill from")]
    NothingToFill(String),
    #[error("the table has no body rows")]
    EmptyTable,
    #[error("rows {first}..{last} are not inside the table")]
    RowsOutsideTable { first: u32, last: u32 },
}

/// The table a plan is currently working on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subject {
    pub sheet: String,
    pub header_row: u32,
    pub first_row: u32,
    pub last_row: u32,
    pub first_col: u32,
    pub last_col: u32,
    /// Headers in column order, as written.
    pub headers: Vec<String>,
    /// How confident table detection was. Carried so the loop can refuse to
    /// act on a guess without saying so.
    pub confidence: f32,
}

impl Subject {
    fn from(view: &TableView) -> Option<Subject> {
        let first = view.columns.first()?;
        let last = view.columns.last()?;
        Some(Subject {
            sheet: view.sheet.clone(),
            header_row: view.header_row,
            first_row: view.header_row + 1,
            last_row: view.header_row + view.row_count,
            first_col: first.index,
            last_col: last.index,
            headers: view.columns.iter().map(|c| c.header.clone()).collect(),
            confidence: view.confidence,
        })
    }

    /// The absolute index of the column with this header.
    pub fn column_of(&self, header: &str) -> Option<u32> {
        let want = normalize(header);
        self.headers
            .iter()
            .position(|h| normalize(h) == want)
            .map(|i| self.first_col + i as u32)
    }

    pub fn body(&self, col: u32) -> RangeAddr {
        RangeAddr::new(
            CellAddr::new(self.first_row, col),
            CellAddr::new(self.last_row, col),
        )
    }

    pub fn whole(&self) -> RangeAddr {
        RangeAddr::new(
            CellAddr::new(self.header_row, self.first_col),
            CellAddr::new(self.last_row, self.last_col),
        )
    }
}

/// Header matching: case-insensitive, whitespace-collapsed, punctuation-free.
///
/// `"Unit Price"`, `"unit price"` and `"Unit  Price:"` are the same column.
/// Deliberately not fuzzy beyond that — matching `"Price"` to `"Unit Price"`
/// would make a two-price table resolve to whichever came first, and being
/// wrong about which column is the price is exactly the kind of quiet error
/// this whole design is arranged to avoid.
pub fn normalize(header: &str) -> String {
    header
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// What compiling one step produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Compiled {
    pub actions: Vec<Action>,
    /// Everything the step intends to touch. The validator compares what the
    /// actions actually did against this, so a step that names a narrow scope
    /// and then writes outside it is caught rather than trusted.
    pub scope: Vec<(String, RangeAddr)>,
    /// A new subject, when the step changed it.
    pub subject: Option<Subject>,
}

impl Compiled {
    fn nothing() -> Compiled {
        Compiled {
            actions: Vec::new(),
            scope: Vec::new(),
            subject: None,
        }
    }
}

/// Compile one step.
pub fn compile(
    step: &Step,
    engine: &Engine,
    observation: &WorkbookObservation,
    subject: Option<&Subject>,
) -> Result<Compiled, CompileError> {
    match step {
        Step::LocateTable { sheet, must_have } => locate(observation, sheet.as_deref(), must_have),
        Step::CreateDerivedColumn {
            header,
            at,
            formula,
            rows,
        } => derived_column(engine, need(subject)?, header, at, formula, rows),
        Step::ApplyFormula { at, formula } => apply_formula(engine, need(subject)?, at, formula),
        Step::FillRange { column, rows } => fill(engine, need(subject)?, column, rows),
        Step::FilterRows { column, predicate } => filter(engine, need(subject)?, column, predicate),
        Step::ReconcileTotals {
            left,
            right,
            variance_at,
            aggregate_with,
        } => reconcile(
            engine,
            need(subject)?,
            left,
            right,
            variance_at.as_deref(),
            aggregate_with.unwrap_or(Aggregate::Sum),
        ),
        // Saying "done" is not doing anything. The step exists so that
        // finishing is something the planner declares rather than something
        // inferred from it running out of ideas.
        Step::ExportWorkbook { .. } => Ok(Compiled::nothing()),
    }
}

fn need(subject: Option<&Subject>) -> Result<&Subject, CompileError> {
    subject.ok_or(CompileError::NoSubject)
}

fn locate(
    observation: &WorkbookObservation,
    sheet: Option<&str>,
    must_have: &[String],
) -> Result<Compiled, CompileError> {
    let want: Vec<String> = must_have.iter().map(|h| normalize(h)).collect();
    let found = observation
        .tables
        .iter()
        .filter(|t| sheet.is_none_or(|s| t.sheet == s))
        .find(|t| {
            want.iter().all(|w| {
                t.columns
                    .iter()
                    .any(|c| !c.header.is_empty() && normalize(&c.header) == *w)
            })
        })
        .ok_or_else(|| CompileError::NoSuchTable(must_have.to_vec()))?;

    Ok(Compiled {
        actions: Vec::new(),
        scope: Vec::new(),
        subject: Some(Subject::from(found).ok_or(CompileError::EmptyTable)?),
    })
}

/// Which absolute column a `ColumnRef` means.
fn resolve_column(
    engine: &Engine,
    subject: &Subject,
    column: &ColumnRef,
) -> Result<u32, CompileError> {
    match column {
        ColumnRef::Header { text } => {
            subject
                .column_of(text)
                .ok_or_else(|| CompileError::NoSuchColumn {
                    looked_for: text.clone(),
                    available: subject.headers.clone(),
                })
        }
        ColumnRef::Name { name } => {
            let refers_to = engine
                .wb
                .names
                .get(&name.to_ascii_uppercase())
                .ok_or_else(|| CompileError::NoSuchName(name.clone()))?;
            let rest = refers_to
                .split_once('!')
                .map_or(refers_to.as_str(), |x| x.1);
            let range = RangeAddr::parse_a1(rest)
                .or_else(|| CellAddr::parse_a1(rest).map(RangeAddr::single))
                .ok_or_else(|| CompileError::NotAnAddress(refers_to.clone()))?;
            Ok(range.start.col)
        }
        ColumnRef::NextFree => Ok(subject.last_col + 1),
    }
}

fn rows_of(subject: &Subject, rows: &RowRange) -> Result<(u32, u32), CompileError> {
    match rows {
        RowRange::TableBody => {
            if subject.last_row < subject.first_row {
                return Err(CompileError::EmptyTable);
            }
            Ok((subject.first_row, subject.last_row))
        }
        RowRange::Rows { first, last } => {
            // 1-based in the plan, because that is what a person writing one
            // would mean by "rows 2 to 6".
            let (f, l) = (first.saturating_sub(1), last.saturating_sub(1));
            if f > l || f < subject.header_row {
                return Err(CompileError::RowsOutsideTable {
                    first: *first,
                    last: *last,
                });
            }
            Ok((f, l))
        }
    }
}

/// Substitute `{Header}` placeholders with addresses.
///
/// `row` decides what a placeholder *means*: given a row, it is that row's
/// cell in the named column (`=B4*C4`); without one, it is the column's whole
/// body (`=SUM(D2:D6)`). That is the difference between a formula living
/// inside the table and one summarising it, and both are things plans need to
/// say.
fn substitute(
    subject: &Subject,
    template: &FormulaTemplate,
    row: Option<u32>,
) -> Result<String, CompileError> {
    let mut out = String::new();
    let mut rest = template.0.as_str();
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            // An unterminated brace is not a placeholder and is not valid
            // formula syntax either, so it will be refused by the parser
            // rather than silently doing something.
            out.push_str(&rest[open..]);
            return Ok(out);
        };
        let header = after[..close].trim();
        let col = subject
            .column_of(header)
            .ok_or_else(|| CompileError::NoSuchColumn {
                looked_for: header.to_string(),
                available: subject.headers.clone(),
            })?;
        out.push_str(&match row {
            Some(r) => CellAddr::new(r, col).to_a1(),
            None => subject.body(col).to_a1(),
        });
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn derived_column(
    engine: &Engine,
    subject: &Subject,
    header: &str,
    at: &ColumnRef,
    formula: &FormulaTemplate,
    rows: &RowRange,
) -> Result<Compiled, CompileError> {
    let col = resolve_column(engine, subject, at)?;
    let (first, last) = rows_of(subject, rows)?;

    let mut actions = vec![Action::CellEdit {
        sheet: subject.sheet.clone(),
        addr: CellAddr::new(subject.header_row, col),
        input: header.to_string(),
    }];
    for row in first..=last {
        actions.push(Action::CellEdit {
            sheet: subject.sheet.clone(),
            addr: CellAddr::new(row, col),
            input: substitute(subject, formula, Some(row))?,
        });
    }

    Ok(Compiled {
        actions,
        scope: vec![(
            subject.sheet.clone(),
            RangeAddr::new(
                CellAddr::new(subject.header_row, col),
                CellAddr::new(last, col),
            ),
        )],
        subject: Some(Subject {
            // The table just got a column wider, and the next step has to see
            // that. Recomputing it from a fresh observation would also work,
            // and would cost a full scan per step.
            last_col: subject.last_col.max(col),
            headers: {
                let mut h = subject.headers.clone();
                let slot = (col - subject.first_col) as usize;
                if slot < h.len() {
                    h[slot] = header.to_string();
                } else {
                    h.resize(slot, String::new());
                    h.push(header.to_string());
                }
                h
            },
            ..subject.clone()
        }),
    })
}

fn apply_formula(
    engine: &Engine,
    subject: &Subject,
    at: &str,
    formula: &FormulaTemplate,
) -> Result<Compiled, CompileError> {
    let (sheet, addr) = split_address(subject, at)?;
    let _ = engine;
    // Inside the table's rows, a placeholder means this row's cell; outside
    // them it means the whole column. Both readings are what somebody would
    // mean by the same words in those two places.
    let row = (addr.row >= subject.first_row && addr.row <= subject.last_row).then_some(addr.row);
    let input = substitute(subject, formula, row)?;
    Ok(Compiled {
        actions: vec![Action::CellEdit {
            sheet: sheet.clone(),
            addr,
            input,
        }],
        scope: vec![(sheet, RangeAddr::single(addr))],
        subject: None,
    })
}

fn fill(
    engine: &Engine,
    subject: &Subject,
    column: &ColumnRef,
    rows: &RowRange,
) -> Result<Compiled, CompileError> {
    let col = resolve_column(engine, subject, column)?;
    let (first, last) = rows_of(subject, rows)?;
    let source = CellAddr::new(first, col);

    // Fill copies what is there. If nothing is, the plan is wrong about the
    // state of the sheet and should be told so rather than filling blanks
    // over the column.
    let has_formula = engine
        .wb
        .sheet_by_name(&subject.sheet)
        .and_then(|s| s.cells.get(&source))
        .is_some_and(|c| c.is_formula());
    if !has_formula {
        return Err(CompileError::NothingToFill(format!(
            "{}!{}",
            subject.sheet,
            source.to_a1()
        )));
    }
    if last <= first {
        // One row is already filled by definition; emitting a fill over it
        // would be a no-op that the validator would then have to explain.
        return Ok(Compiled::nothing());
    }

    let target = RangeAddr::new(source, CellAddr::new(last, col));
    Ok(Compiled {
        actions: vec![Action::FillApply {
            sheet: subject.sheet.clone(),
            source: RangeAddr::single(source),
            target,
        }],
        scope: vec![(subject.sheet.clone(), target)],
        subject: None,
    })
}

/// Compile a predicate into the engine's checkbox filter by evaluating it
/// against the values actually in the column.
///
/// The engine models a filter as the set of display strings that stay
/// visible, which is what the UI produces; a plan says "greater than 100".
/// Resolving one into the other here is exactly the planner/compiler split
/// working — the plan states intent, the compiler answers it against the data
/// in front of it.
fn filter(
    engine: &Engine,
    subject: &Subject,
    column: &ColumnRef,
    predicate: &Predicate,
) -> Result<Compiled, CompileError> {
    let col = resolve_column(engine, subject, column)?;
    let sheet = engine
        .wb
        .sheet_by_name(&subject.sheet)
        .ok_or_else(|| CompileError::NotAnAddress(subject.sheet.clone()))?;

    let mut allowed: Vec<String> = Vec::new();
    for row in subject.first_row..=subject.last_row {
        let value = sheet.value(CellAddr::new(row, col));
        if !matches(&value, predicate) {
            continue;
        }
        let shown = value.display();
        if !allowed.contains(&shown) {
            allowed.push(shown);
        }
    }

    let range = subject.whole();
    Ok(Compiled {
        actions: vec![Action::FilterApply {
            sheet: subject.sheet.clone(),
            spec: FilterSpec {
                range,
                column: col,
                allowed,
            },
        }],
        // A filter hides rows; it changes no cell. The scope is the table so
        // the validator can say the step stayed inside it.
        scope: vec![(subject.sheet.clone(), range)],
        subject: None,
    })
}

fn matches(value: &Value, predicate: &Predicate) -> bool {
    match predicate {
        Predicate::Equals { value: want } => value.display() == *want,
        Predicate::NotEquals { value: want } => value.display() != *want,
        Predicate::GreaterThan { value: n } => matches!(value, Value::Number(v) if v > n),
        Predicate::LessThan { value: n } => matches!(value, Value::Number(v) if v < n),
        Predicate::Contains { text } => value
            .display()
            .to_lowercase()
            .contains(&text.to_lowercase()),
        Predicate::IsBlank => value.is_empty(),
        Predicate::IsNotBlank => !value.is_empty(),
    }
}

fn reconcile(
    engine: &Engine,
    subject: &Subject,
    left: &ColumnRef,
    right: &ColumnRef,
    variance_at: Option<&str>,
    aggregate: Aggregate,
) -> Result<Compiled, CompileError> {
    let l = resolve_column(engine, subject, left)?;
    let r = resolve_column(engine, subject, right)?;
    let Some(at) = variance_at else {
        // "Check that these agree" with nowhere to write the answer is a
        // grader's job, not an editor's. Compiling to nothing is right; the
        // step still belongs in the plan because it says what is being
        // relied on.
        return Ok(Compiled::nothing());
    };
    let (sheet, addr) = split_address(subject, at)?;
    let f = aggregate.function();
    let input = format!(
        "={f}({})-{f}({})",
        subject.body(l).to_a1(),
        subject.body(r).to_a1()
    );
    Ok(Compiled {
        actions: vec![Action::CellEdit {
            sheet: sheet.clone(),
            addr,
            input,
        }],
        scope: vec![(sheet, RangeAddr::single(addr))],
        subject: None,
    })
}

/// `Sheet!A1`, or a bare `A1` on the subject's sheet.
fn split_address(subject: &Subject, text: &str) -> Result<(String, CellAddr), CompileError> {
    let (sheet, rest) = match text.split_once('!') {
        Some((s, rest)) => (s.trim_matches('\'').to_string(), rest),
        None => (subject.sheet.clone(), text),
    };
    let addr =
        CellAddr::parse_a1(rest).ok_or_else(|| CompileError::NotAnAddress(text.to_string()))?;
    Ok((sheet, addr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::FormulaTemplate;

    fn edit(sheet: &str, a1: &str, input: &str) -> Action {
        Action::CellEdit {
            sheet: sheet.into(),
            addr: CellAddr::parse_a1(a1).unwrap(),
            input: input.into(),
        }
    }

    fn ledger() -> Engine {
        let mut e = Engine::new();
        for (a1, input) in [
            ("A1", "Item"),
            ("B1", "Qty"),
            ("C1", "Unit Price"),
            ("A2", "Bolt"),
            ("B2", "4"),
            ("C2", "2.5"),
            ("A3", "Nut"),
            ("B3", "9"),
            ("C3", "0.5"),
            ("A4", "Washer"),
            ("B4", "2"),
            ("C4", "1.25"),
        ] {
            e.apply(&edit("Sheet1", a1, input)).unwrap();
        }
        e
    }

    fn look(e: &Engine) -> WorkbookObservation {
        env::observe::observe(e, "Sheet1", "A1", &[], "hash".into())
    }

    fn located(e: &Engine) -> Subject {
        let obs = look(e);
        compile(
            &Step::LocateTable {
                sheet: None,
                must_have: vec!["Qty".into()],
            },
            e,
            &obs,
            None,
        )
        .unwrap()
        .subject
        .unwrap()
    }

    #[test]
    fn a_table_is_located_by_its_headers_not_its_address() {
        let s = located(&ledger());
        assert_eq!(s.sheet, "Sheet1");
        assert_eq!(s.header_row, 0);
        assert_eq!((s.first_row, s.last_row), (1, 3));
        assert_eq!(s.column_of("Qty"), Some(1));
        assert_eq!(s.column_of("unit  price"), Some(2), "matching is loose");
        assert_eq!(s.column_of("Price"), None, "but not that loose");
    }

    #[test]
    fn the_same_plan_compiles_differently_when_the_table_moved() {
        // The property the whole planner/compiler split exists for. If this
        // test ever passes by producing the same addresses, the compiler has
        // stopped resolving and started replaying.
        let step = Step::CreateDerivedColumn {
            header: "Total".into(),
            at: ColumnRef::NextFree,
            formula: FormulaTemplate::new("={Qty}*{Unit Price}"),
            rows: RowRange::TableBody,
        };

        let here = ledger();
        let a = compile(&step, &here, &look(&here), Some(&located(&here))).unwrap();

        let mut moved = ledger();
        moved
            .apply(&Action::RowInsert {
                sheet: "Sheet1".into(),
                at: 0,
                count: 3,
            })
            .unwrap();
        moved
            .apply(&Action::ColInsert {
                sheet: "Sheet1".into(),
                at: 0,
                count: 2,
            })
            .unwrap();
        let b = compile(&step, &moved, &look(&moved), Some(&located(&moved))).unwrap();

        assert_eq!(a.actions[1], edit("Sheet1", "D2", "=B2*C2"));
        assert_eq!(b.actions[1], edit("Sheet1", "F5", "=D5*E5"));
    }

    #[test]
    fn a_derived_column_writes_its_header_and_one_formula_per_body_row() {
        let e = ledger();
        let c = compile(
            &Step::CreateDerivedColumn {
                header: "Total".into(),
                at: ColumnRef::NextFree,
                formula: FormulaTemplate::new("={Qty}*{Unit Price}"),
                rows: RowRange::TableBody,
            },
            &e,
            &look(&e),
            Some(&located(&e)),
        )
        .unwrap();
        assert_eq!(c.actions.len(), 4, "a header and three rows");
        assert_eq!(c.actions[0], edit("Sheet1", "D1", "Total"));
        assert_eq!(c.actions[3], edit("Sheet1", "D4", "=B4*C4"));
        assert_eq!(
            c.scope,
            vec![("Sheet1".into(), RangeAddr::parse_a1("D1:D4").unwrap())]
        );
    }

    #[test]
    fn the_new_column_is_visible_to_the_next_step() {
        // Without this, a plan cannot total the column it just created —
        // which is the second half of almost every real task.
        let e = ledger();
        let first = compile(
            &Step::CreateDerivedColumn {
                header: "Total".into(),
                at: ColumnRef::NextFree,
                formula: FormulaTemplate::new("={Qty}*{Unit Price}"),
                rows: RowRange::TableBody,
            },
            &e,
            &look(&e),
            Some(&located(&e)),
        )
        .unwrap();
        let subject = first.subject.unwrap();
        assert_eq!(subject.column_of("Total"), Some(3));

        let second = compile(
            &Step::ApplyFormula {
                at: "D5".into(),
                formula: FormulaTemplate::new("=SUM({Total})"),
            },
            &e,
            &look(&e),
            Some(&subject),
        )
        .unwrap();
        assert_eq!(second.actions[0], edit("Sheet1", "D5", "=SUM(D2:D4)"));
    }

    #[test]
    fn a_placeholder_means_this_row_inside_the_table_and_the_column_outside_it() {
        // The rule that lets one syntax cover both "each row times its price"
        // and "the total of the column".
        let e = ledger();
        let s = located(&e);
        let inside = compile(
            &Step::ApplyFormula {
                at: "D3".into(),
                formula: FormulaTemplate::new("={Qty}*2"),
            },
            &e,
            &look(&e),
            Some(&s),
        )
        .unwrap();
        assert_eq!(inside.actions[0], edit("Sheet1", "D3", "=B3*2"));

        let outside = compile(
            &Step::ApplyFormula {
                at: "D9".into(),
                formula: FormulaTemplate::new("=SUM({Qty})"),
            },
            &e,
            &look(&e),
            Some(&s),
        )
        .unwrap();
        assert_eq!(outside.actions[0], edit("Sheet1", "D9", "=SUM(B2:B4)"));
    }

    #[test]
    fn a_header_that_is_not_there_is_an_error_naming_the_ones_that_are() {
        // Not the closest match. Being wrong about which column is the price
        // is the exact failure this design exists to make impossible.
        let e = ledger();
        let err = compile(
            &Step::CreateDerivedColumn {
                header: "Total".into(),
                at: ColumnRef::NextFree,
                formula: FormulaTemplate::new("={Quantity}*{Unit Price}"),
                rows: RowRange::TableBody,
            },
            &e,
            &look(&e),
            Some(&located(&e)),
        )
        .unwrap_err();
        match err {
            CompileError::NoSuchColumn {
                looked_for,
                available,
            } => {
                assert_eq!(looked_for, "Quantity");
                assert!(available.contains(&"Qty".to_string()));
            }
            other => panic!("wrong error: {other}"),
        }
    }

    #[test]
    fn a_step_before_the_table_is_located_is_refused() {
        let e = ledger();
        assert_eq!(
            compile(
                &Step::FillRange {
                    column: ColumnRef::NextFree,
                    rows: RowRange::TableBody
                },
                &e,
                &look(&e),
                None
            )
            .unwrap_err(),
            CompileError::NoSubject
        );
    }

    #[test]
    fn locating_a_table_that_is_not_there_fails_rather_than_picking_one() {
        let e = ledger();
        assert!(matches!(
            compile(
                &Step::LocateTable {
                    sheet: None,
                    must_have: vec!["Debit".into(), "Credit".into()]
                },
                &e,
                &look(&e),
                None
            ),
            Err(CompileError::NoSuchTable(_))
        ));
    }

    #[test]
    fn filling_a_column_with_nothing_in_it_is_refused() {
        // Filling blanks over a column looks like work and destroys data.
        let e = ledger();
        assert!(matches!(
            compile(
                &Step::FillRange {
                    column: ColumnRef::Header {
                        text: "Item".into()
                    },
                    rows: RowRange::TableBody,
                },
                &e,
                &look(&e),
                Some(&located(&e))
            ),
            Err(CompileError::NothingToFill(_))
        ));
    }

    #[test]
    fn filling_continues_a_formula_somebody_already_wrote() {
        let mut e = ledger();
        e.apply(&edit("Sheet1", "D1", "Total")).unwrap();
        e.apply(&edit("Sheet1", "D2", "=B2*C2")).unwrap();
        let s = located(&e);
        let c = compile(
            &Step::FillRange {
                column: ColumnRef::Header {
                    text: "Total".into(),
                },
                rows: RowRange::TableBody,
            },
            &e,
            &look(&e),
            Some(&s),
        )
        .unwrap();
        assert_eq!(
            c.actions,
            vec![Action::FillApply {
                sheet: "Sheet1".into(),
                source: RangeAddr::parse_a1("D2:D2").unwrap(),
                target: RangeAddr::parse_a1("D2:D4").unwrap(),
            }]
        );
    }

    #[test]
    fn a_filter_predicate_is_resolved_against_the_values_in_the_column() {
        let e = ledger();
        let c = compile(
            &Step::FilterRows {
                column: ColumnRef::Header { text: "Qty".into() },
                predicate: Predicate::GreaterThan { value: 3.0 },
            },
            &e,
            &look(&e),
            Some(&located(&e)),
        )
        .unwrap();
        let Action::FilterApply { spec, .. } = &c.actions[0] else {
            panic!("expected a filter");
        };
        assert_eq!(spec.column, 1);
        assert_eq!(spec.allowed, vec!["4".to_string(), "9".to_string()]);
    }

    #[test]
    fn reconciling_without_a_place_to_write_compiles_to_nothing() {
        // "These two must agree" is a claim for the grader, not an edit.
        let mut e = ledger();
        e.apply(&edit("Sheet1", "D1", "Credit")).unwrap();
        let s = located(&e);
        let c = compile(
            &Step::ReconcileTotals {
                left: ColumnRef::Header { text: "Qty".into() },
                right: ColumnRef::Header {
                    text: "Credit".into(),
                },
                variance_at: None,
                aggregate_with: None,
            },
            &e,
            &look(&e),
            Some(&s),
        )
        .unwrap();
        assert!(c.actions.is_empty());
    }

    #[test]
    fn reconciling_with_a_place_to_write_produces_a_difference_of_sums() {
        let mut e = ledger();
        e.apply(&edit("Sheet1", "D1", "Credit")).unwrap();
        e.apply(&edit("Sheet1", "D2", "1")).unwrap();
        let s = located(&e);
        let c = compile(
            &Step::ReconcileTotals {
                left: ColumnRef::Header { text: "Qty".into() },
                right: ColumnRef::Header {
                    text: "Credit".into(),
                },
                variance_at: Some("F1".into()),
                aggregate_with: Some(Aggregate::Sum),
            },
            &e,
            &look(&e),
            Some(&s),
        )
        .unwrap();
        assert_eq!(c.actions[0], edit("Sheet1", "F1", "=SUM(B2:B4)-SUM(D2:D4)"));
    }

    #[test]
    fn saying_the_work_is_done_edits_nothing() {
        let e = ledger();
        let c = compile(
            &Step::ExportWorkbook { path: None },
            &e,
            &look(&e),
            Some(&located(&e)),
        )
        .unwrap();
        assert!(c.actions.is_empty());
        assert!(c.scope.is_empty());
    }

    #[test]
    fn every_step_declares_the_scope_it_intends_to_touch() {
        // What the validator checks against. A step with no declared scope
        // can write anywhere without anyone noticing.
        let e = ledger();
        let c = compile(
            &Step::CreateDerivedColumn {
                header: "Total".into(),
                at: ColumnRef::NextFree,
                formula: FormulaTemplate::new("={Qty}*{Unit Price}"),
                rows: RowRange::TableBody,
            },
            &e,
            &look(&e),
            Some(&located(&e)),
        )
        .unwrap();
        for action in &c.actions {
            let Action::CellEdit { sheet, addr, .. } = action else {
                continue;
            };
            assert!(
                c.scope.iter().any(|(s, r)| s == sheet && r.contains(*addr)),
                "{addr:?} is outside the declared scope"
            );
        }
    }
}
