//! What a policy is allowed to see.
//!
//! The constraint that shapes this file: an observation is produced on *every
//! step*, and a workbook is large. Serializing the whole thing would make the
//! trajectory bigger than the dataset and the context window the limiting
//! factor on task length. So an observation is a summary with a fixed budget:
//! sheet extents, the tables it could find with their headers and column
//! types, formulas grouped by *pattern* rather than listed per cell, a
//! dependency summary, the selection, the visible errors, and what changed
//! since the last step.
//!
//! Two things follow from that, and both are deliberate:
//!
//! * **Nothing here is the ground truth.** A grader reads the workbook, never
//!   the observation. If the summary is wrong the policy is misled, but the
//!   score is still right — which is the only arrangement in which a
//!   summarization bug shows up as a lower score rather than a wrong one.
//! * **Detection is heuristic and says so.** `TableView::confidence` exists
//!   because "where is the table" has no correct answer on an arbitrary
//!   sheet, and a policy that cannot tell a guess from a fact will trust the
//!   guess.

use std::collections::BTreeMap;

use engine::{CellAddr, Engine, RangeAddr, SheetId, Value};
use serde::{Deserialize, Serialize};

/// The most cells any one observation will look at per sheet.
///
/// Type inference and error scanning walk cells; a sheet with a million of
/// them would make `observe()` the slowest thing in the loop. Sampling is
/// honest about itself — `TableView::sampled` says when a column's type came
/// from a sample rather than a census.
const SCAN_BUDGET: usize = 20_000;

/// How many rows of a column are examined to decide its type.
const TYPE_SAMPLE: usize = 200;

/// How many changed cells an observation will name before it summarises.
const CHANGE_BUDGET: usize = 64;

/// How many erroring cells an observation will list.
///
/// A budget rather than "all of them" because errors cluster: one broken
/// formula filled down a column is one mistake and ten thousand `#REF!`s, and
/// an observation that listed all of them would be larger than the workbook
/// it summarises. The first few, plus the true count, is the whole signal.
const ERROR_BUDGET: usize = 32;

/// How many distinct formula shapes an observation will list. Grouping
/// usually collapses a sheet to a handful; a sheet of a thousand unrelated
/// formulas is a sheet nobody is going to fix in one step anyway.
const GROUP_BUDGET: usize = 64;

/// The broad kind of a value, which is what a policy reasons about — the
/// difference between a number column and a text one decides which formula
/// makes sense, and the exact values do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellType {
    Empty,
    Number,
    Text,
    Bool,
    Error,
    /// A number carrying a date format: the same bits as `Number`, but a
    /// policy that treats it as an amount will produce nonsense.
    Date,
    /// More than one kind in the same column, which is usually a sign the
    /// range is wrong rather than that the data is mixed.
    Mixed,
}

impl CellType {
    fn of(value: &Value, number_format: Option<&str>) -> CellType {
        match value {
            Value::Empty => CellType::Empty,
            Value::Text(_) => CellType::Text,
            Value::Bool(_) => CellType::Bool,
            Value::Error(_) => CellType::Error,
            Value::Number(_) => {
                if number_format.is_some_and(is_date_format) {
                    CellType::Date
                } else {
                    CellType::Number
                }
            }
        }
    }

    fn merge(self, other: CellType) -> CellType {
        match (self, other) {
            (a, b) if a == b => a,
            (CellType::Empty, b) => b,
            (a, CellType::Empty) => a,
            // A stray error in a number column is still a number column with
            // a broken cell in it, and saying "mixed" would hide that.
            (CellType::Error, b) | (b, CellType::Error) => b,
            _ => CellType::Mixed,
        }
    }
}

/// Whether a number format code makes its cell a date.
///
/// Deliberately crude: `y`, `d` or `m` outside a literal is what every date
/// code has and no currency code does. Reading the code properly means the
/// format parser, and being wrong here costs a mislabelled column rather than
/// a wrong answer.
fn is_date_format(code: &str) -> bool {
    let mut in_literal = false;
    for ch in code.chars() {
        match ch {
            '"' => in_literal = !in_literal,
            'y' | 'Y' | 'd' | 'D' if !in_literal => return true,
            _ => {}
        }
    }
    false
}

/// One column of a detected table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnView {
    /// The header text, trimmed. Empty when the column has no header.
    pub header: String,
    /// Absolute column index, so an action can be aimed without re-deriving it.
    pub index: u32,
    pub cell_type: CellType,
    /// How many of the body cells hold anything.
    pub populated: u32,
    /// True when every populated body cell is a formula — the signal that a
    /// column is derived and probably should not be overwritten with literals.
    pub all_formulas: bool,
    /// One formula from the column, as written, when it has any. A single
    /// example rather than every row: they are almost always the same shape,
    /// and the shape is the information.
    pub formula_example: Option<String>,
}

/// A rectangular region that looks like a table: a header row and a body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableView {
    pub sheet: String,
    /// The whole table including its header row, in A1.
    pub range: String,
    /// The body alone, which is what a fill or a formula usually targets.
    pub body_range: String,
    pub header_row: u32,
    pub columns: Vec<ColumnView>,
    pub row_count: u32,
    /// 0.0 to 1.0. A table found by a header row of text over a body of
    /// numbers scores high; a lone block of numbers with no header scores
    /// low and should be treated as a guess.
    pub confidence: f32,
    /// True when column types came from a sample rather than every row.
    pub sampled: bool,
}

/// Per-sheet summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SheetView {
    pub name: String,
    /// Populated extent in A1, or None for an empty sheet.
    pub used_range: Option<String>,
    pub cell_count: u32,
    pub formula_count: u32,
    pub frozen: (u32, u32),
    pub merged: Vec<String>,
    pub hidden_rows: u32,
    pub conditional_rules: Vec<String>,
}

/// A group of formulas that share a shape, with one example and a count.
///
/// A filled-down column is one entry here rather than four hundred. That is
/// the single biggest reason an observation stays small, and it is also the
/// more useful shape: "column D is `=B*C` for 400 rows" is what a policy
/// needs, and the four hundred addresses are not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormulaGroup {
    pub sheet: String,
    /// The formula with its references made relative to the writing cell, so
    /// a filled column collapses to one entry.
    pub shape: String,
    /// One address that has it, as an anchor.
    pub example_at: String,
    /// The formula exactly as written at `example_at`.
    pub example: String,
    pub count: u32,
}

/// A cell holding an error, with what it says.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorView {
    pub sheet: String,
    pub addr: String,
    pub code: String,
    /// The formula that produced it, when there is one. An error with no
    /// formula behind it was typed, which is a different problem.
    pub formula: Option<String>,
}

/// How tangled the sheet is, without listing the graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DependencySummary {
    pub formula_cells: u32,
    /// Formulas nothing else reads: the outputs.
    pub leaf_formulas: u32,
    /// The longest chain of formulas reading formulas, bounded by the walk.
    pub max_depth: u32,
    /// Cells with the most dependents, which is where a change does damage.
    pub most_depended_on: Vec<(String, u32)>,
}

/// What changed in the last step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeSummary {
    /// Up to `CHANGE_BUDGET` addresses, in reading order.
    pub cells: Vec<String>,
    /// The real number, which may be larger than `cells.len()`.
    pub total: u32,
    pub truncated: bool,
}

/// Everything a policy sees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkbookObservation {
    pub sheets: Vec<SheetView>,
    pub active_sheet: String,
    /// The current selection in A1, on the active sheet.
    pub selection: String,
    pub tables: Vec<TableView>,
    /// The largest `GROUP_BUDGET` shapes, biggest first.
    pub formula_groups: Vec<FormulaGroup>,
    /// How many distinct shapes there are, which may exceed
    /// `formula_groups.len()`. A policy that cannot tell "these are all the
    /// formulas" from "these are the top 64" will confidently miss the rest.
    pub formula_group_total: u32,
    pub defined_names: Vec<(String, String)>,
    /// Up to `ERROR_BUDGET` erroring cells, in reading order.
    pub errors: Vec<ErrorView>,
    /// The real number of erroring cells.
    pub error_total: u32,
    pub dependencies: DependencySummary,
    pub recent_changes: ChangeSummary,
    /// The hash of the workbook this describes, so an observation can be
    /// checked against the state it claims to be of.
    pub state_hash: String,
}

/// Build an observation. `changed` is what the last step touched.
pub fn observe(
    engine: &Engine,
    active_sheet: &str,
    selection: &str,
    changed: &[(String, CellAddr)],
    state_hash: String,
) -> WorkbookObservation {
    let sheets: Vec<SheetView> = engine.wb.sheets.iter().map(sheet_view).collect();
    let tables = engine
        .wb
        .sheets
        .iter()
        .flat_map(|s| detect_tables(engine, s))
        .collect();
    let (formula_groups, formula_group_total) = group_formulas(engine);
    let (errors, error_total) = collect_errors(engine);
    let dependencies = summarize_dependencies(engine);

    let mut cells: Vec<String> = changed
        .iter()
        .take(CHANGE_BUDGET)
        .map(|(sheet, addr)| format!("{sheet}!{}", addr.to_a1()))
        .collect();
    cells.sort();
    let recent_changes = ChangeSummary {
        total: changed.len() as u32,
        truncated: changed.len() > CHANGE_BUDGET,
        cells,
    };

    WorkbookObservation {
        active_sheet: active_sheet.to_string(),
        selection: selection.to_string(),
        sheets,
        tables,
        formula_groups,
        formula_group_total,
        defined_names: engine
            .wb
            .names
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        errors,
        error_total,
        dependencies,
        recent_changes,
        state_hash,
    }
}

fn sheet_view(s: &engine::Sheet) -> SheetView {
    SheetView {
        name: s.name.clone(),
        used_range: s.used_range().map(|r| r.to_a1()),
        cell_count: s.cells.len() as u32,
        formula_count: s.cells.values().filter(|c| c.is_formula()).count() as u32,
        frozen: (s.frozen_rows, s.frozen_cols),
        merged: s.merged.iter().map(|m| m.to_a1()).collect(),
        hidden_rows: s.hidden_rows.len() as u32,
        conditional_rules: s.conditional.iter().map(|r| r.summary()).collect(),
    }
}

/// Find the tables on a sheet.
///
/// The rule: a row of mostly-text cells with populated rows under it is a
/// header. Columns run to the first fully empty column, rows to the first
/// fully empty row. It finds the one-table-per-sheet layout that most real
/// spreadsheets are, and it reports how sure it is rather than pretending.
fn detect_tables(engine: &Engine, s: &engine::Sheet) -> Vec<TableView> {
    let Some(used) = s.used_range() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let sampled = (used.cell_count() as usize) > SCAN_BUDGET;

    // The header is the first row in the used range with at least two text
    // cells and something populated below it. Anything more clever needs to
    // be wrong less often than this, which is a high bar.
    let mut header_row = None;
    for row in used.start.row..=used.end.row.min(used.start.row + 50) {
        let texts = (used.start.col..=used.end.col)
            .filter(|c| matches!(s.value(CellAddr::new(row, *c)), Value::Text(t) if !t.trim().is_empty()))
            .count();
        let below = (used.start.col..=used.end.col)
            .filter(|c| !s.value(CellAddr::new(row + 1, *c)).is_empty())
            .count();
        if texts >= 2 && below > 0 {
            header_row = Some(row);
            break;
        }
    }
    let Some(header_row) = header_row else {
        return out;
    };

    // Columns: from the header's first populated cell to the first gap.
    let first_col = (used.start.col..=used.end.col)
        .find(|c| !s.value(CellAddr::new(header_row, *c)).is_empty());
    let Some(first_col) = first_col else {
        return out;
    };
    let mut last_col = first_col;
    for c in first_col..=used.end.col {
        if s.value(CellAddr::new(header_row, c)).is_empty() {
            break;
        }
        last_col = c;
    }

    // Rows: down to the last row with anything in the table's columns.
    let mut last_row = header_row;
    for r in (header_row + 1)..=used.end.row {
        let populated = (first_col..=last_col).any(|c| !s.value(CellAddr::new(r, c)).is_empty());
        if populated {
            last_row = r;
        }
    }
    if last_row == header_row {
        return out;
    }

    let columns: Vec<ColumnView> = (first_col..=last_col)
        .map(|c| column_view(engine, s, c, header_row, last_row))
        .collect();

    // Two things make a detection a guess rather than a fact.
    //
    // A column whose values are not consistently one type usually means the
    // range is wrong rather than that the data really is mixed — and a short
    // body means a header row is barely distinguishable from a first row of
    // data, because two rows of anything look like a header and a value.
    //
    // What is deliberately *not* scored is whether every column has a header:
    // the column run stops at the first empty header cell by construction, so
    // that test can never fail and a branch for it would be dead code
    // pretending to be a safeguard. (It was, until a test that needed a
    // genuinely low-confidence table could not produce one.)
    let typed = columns
        .iter()
        .filter(|c| !matches!(c.cell_type, CellType::Empty | CellType::Mixed))
        .count();
    let type_score = if columns.is_empty() {
        0.0
    } else {
        typed as f32 / columns.len() as f32
    };
    let depth_score = match last_row - header_row {
        0 => 0.0,
        1 => 0.6,
        2 => 0.8,
        _ => 1.0,
    };
    let confidence = 0.35 + 0.5 * type_score + 0.15 * depth_score;

    out.push(TableView {
        sheet: s.name.clone(),
        range: RangeAddr::new(
            CellAddr::new(header_row, first_col),
            CellAddr::new(last_row, last_col),
        )
        .to_a1(),
        body_range: RangeAddr::new(
            CellAddr::new(header_row + 1, first_col),
            CellAddr::new(last_row, last_col),
        )
        .to_a1(),
        header_row,
        row_count: last_row - header_row,
        columns,
        confidence,
        sampled,
    });
    out
}

fn column_view(
    engine: &Engine,
    s: &engine::Sheet,
    col: u32,
    header_row: u32,
    last_row: u32,
) -> ColumnView {
    let header = match s.value(CellAddr::new(header_row, col)) {
        Value::Text(t) => t.trim().to_string(),
        other if !other.is_empty() => other.display(),
        _ => String::new(),
    };
    let body = (header_row + 1)..=last_row;
    let step = ((body.clone().count() / TYPE_SAMPLE).max(1)) as u32;

    let mut cell_type = CellType::Empty;
    let mut populated = 0u32;
    let mut formulas = 0u32;
    let mut example = None;
    for (i, r) in body.clone().enumerate() {
        let addr = CellAddr::new(r, col);
        let value = s.value(addr);
        if !value.is_empty() {
            populated += 1;
        }
        if let Some(cell) = s.cells.get(&addr) {
            if cell.is_formula() {
                formulas += 1;
                if example.is_none() {
                    example = Some(cell.input());
                }
            }
        }
        // Every cell is counted, but only a sample is typed: counting is
        // cheap and the count is what "is this column full" depends on.
        if (i as u32).is_multiple_of(step) {
            let fmt = engine.wb.formats.resolve(s.format_id(addr));
            cell_type = cell_type.merge(CellType::of(&value, fmt.number_format.as_deref()));
        }
    }

    ColumnView {
        header,
        index: col,
        cell_type,
        populated,
        all_formulas: populated > 0 && formulas == populated,
        formula_example: example,
    }
}

/// Formulas grouped by their relative shape.
fn group_formulas(engine: &Engine) -> (Vec<FormulaGroup>, u32) {
    let mut groups: BTreeMap<(String, String), (String, String, u32)> = BTreeMap::new();
    for s in &engine.wb.sheets {
        let mut addrs: Vec<CellAddr> = s
            .cells
            .iter()
            .filter(|(_, c)| c.is_formula())
            .map(|(a, _)| *a)
            .collect();
        addrs.sort();
        for addr in addrs {
            let Some(cell) = s.cells.get(&addr) else {
                continue;
            };
            let input = cell.input();
            let shape = relative_shape(&input, addr);
            let entry = groups
                .entry((s.name.clone(), shape))
                .or_insert_with(|| (addr.to_a1(), input.clone(), 0));
            entry.2 += 1;
        }
    }
    let total = groups.len() as u32;
    let mut out: Vec<FormulaGroup> = groups
        .into_iter()
        .map(|((sheet, shape), (at, example, count))| FormulaGroup {
            sheet,
            shape,
            example_at: at,
            example,
            count,
        })
        .collect();
    // Biggest groups first, so truncation drops the one-offs rather than
    // whatever happened to sort last. Ties break on sheet then shape, which
    // keeps the order a function of the workbook and nothing else.
    out.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then(a.sheet.cmp(&b.sheet))
            .then(a.shape.cmp(&b.shape))
    });
    out.truncate(GROUP_BUDGET);
    (out, total)
}

/// A formula with its references rewritten relative to the cell holding it,
/// so a filled column collapses to one shape.
///
/// R1C1 rather than a shifted A1 formula, and the difference is not cosmetic.
/// Shifting `=B2*C2` from D2 to the origin asks for a cell three columns left
/// of column A, which does not exist, so the engine's `refs::offset` — quite
/// correctly, for its own purpose — returns `#REF!`. Every formula that reads
/// up and to the left then normalizes to the same string, and `=B2*C2` stops
/// being distinguishable from `=Z9+Q1`. That is a bad grouping in an
/// observation and a *wrong answer* in a grader, which is where this function
/// is also used.
///
/// `R[0]C[-2]` has no such floor. Absolute references keep their absolute
/// spelling (`$A$1` is `R1C1`), which is right: a filled column referring to
/// `$A$1` really does have one shape.
pub fn relative_shape(input: &str, at: CellAddr) -> String {
    let body = input.strip_prefix('=').unwrap_or(input);
    match engine::parser::parse_formula(body) {
        Ok(ast) => format!("={}", to_r1c1(&ast, at).to_formula()),
        // A formula the parser cannot read groups under its own text, which
        // over-counts groups rather than merging two different formulas.
        Err(_) => input.to_string(),
    }
}

/// Rewrite every reference in an expression to its R1C1 spelling relative to
/// `at`, as an `Expr::Name` — which renders verbatim, so the engine's own
/// precedence and parenthesization rules produce the final text and this
/// function does not have to reimplement them.
fn to_r1c1(e: &engine::ast::Expr, at: CellAddr) -> engine::ast::Expr {
    use engine::ast::Expr;
    match e {
        Expr::Cell(c) => Expr::Name(format!("{}{}", sheet_prefix(&c.sheet), r1c1(&c.r, at))),
        Expr::Range(r) => Expr::Name(format!(
            "{}{}:{}",
            sheet_prefix(&r.sheet),
            r1c1(&r.start, at),
            r1c1(&r.end, at)
        )),
        Expr::Func(name, args) => {
            Expr::Func(name.clone(), args.iter().map(|a| to_r1c1(a, at)).collect())
        }
        Expr::Binary(op, l, r) => {
            Expr::Binary(*op, Box::new(to_r1c1(l, at)), Box::new(to_r1c1(r, at)))
        }
        Expr::Neg(x) => Expr::Neg(Box::new(to_r1c1(x, at))),
        Expr::Pos(x) => Expr::Pos(Box::new(to_r1c1(x, at))),
        Expr::Percent(x) => Expr::Percent(Box::new(to_r1c1(x, at))),
        other => other.clone(),
    }
}

fn r1c1(r: &engine::addr::ParsedRef, at: CellAddr) -> String {
    let row = if r.abs_row {
        format!("R{}", r.row + 1)
    } else {
        format!("R[{}]", r.row as i64 - at.row as i64)
    };
    let col = if r.abs_col {
        format!("C{}", r.col + 1)
    } else {
        format!("C[{}]", r.col as i64 - at.col as i64)
    };
    format!("{row}{col}")
}

/// A sheet qualifier for a shape key. Unquoted even when the name needs
/// quoting in a formula: this string is a grouping key, never parsed back.
fn sheet_prefix(sheet: &Option<String>) -> String {
    match sheet {
        Some(s) => format!("{s}!"),
        None => String::new(),
    }
}

fn collect_errors(engine: &Engine) -> (Vec<ErrorView>, u32) {
    let mut out = Vec::new();
    for s in &engine.wb.sheets {
        let mut addrs: Vec<CellAddr> = s.cells.keys().copied().collect();
        addrs.sort();
        for addr in addrs {
            let Value::Error(k) = s.value(addr) else {
                continue;
            };
            let cell = s.cells.get(&addr);
            out.push(ErrorView {
                sheet: s.name.clone(),
                addr: addr.to_a1(),
                code: k.code().to_string(),
                formula: cell.filter(|c| c.is_formula()).map(|c| c.input()),
            });
        }
        // A spilled error has no cell but is still on screen.
        for (addr, (_, v)) in &s.spill {
            if let Value::Error(k) = v {
                out.push(ErrorView {
                    sheet: s.name.clone(),
                    addr: addr.to_a1(),
                    code: k.code().to_string(),
                    formula: None,
                });
            }
        }
    }
    let total = out.len() as u32;
    out.truncate(ERROR_BUDGET);
    (out, total)
}

fn summarize_dependencies(engine: &Engine) -> DependencySummary {
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    let mut formula_cells = 0u32;
    for s in &engine.wb.sheets {
        for (addr, cell) in &s.cells {
            if !cell.is_formula() {
                continue;
            }
            formula_cells += 1;
            let key = engine::CellKey {
                sheet: s.id,
                addr: *addr,
            };
            let dependents = engine.dependents_of(key);
            if !dependents.is_empty() {
                counts.insert(
                    format!("{}!{}", s.name, addr.to_a1()),
                    dependents.len() as u32,
                );
            }
        }
    }
    let leaf_formulas = formula_cells - counts.len() as u32;
    let mut ranked: Vec<(String, u32)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.truncate(5);

    DependencySummary {
        formula_cells,
        leaf_formulas,
        max_depth: chain_depth(engine),
        most_depended_on: ranked,
    }
}

/// The longest chain of formulas reading formulas, bounded so a pathological
/// sheet cannot make `observe` the slow step.
fn chain_depth(engine: &Engine) -> u32 {
    const LIMIT: u32 = 32;
    let mut depth = 0;
    let mut frontier: Vec<engine::CellKey> = Vec::new();
    for s in &engine.wb.sheets {
        for (addr, cell) in &s.cells {
            if cell.is_formula() {
                frontier.push(engine::CellKey {
                    sheet: s.id,
                    addr: *addr,
                });
            }
        }
    }
    // Peel leaves off repeatedly; the number of rounds is the depth.
    let mut remaining: std::collections::HashSet<engine::CellKey> =
        frontier.iter().copied().collect();
    while !remaining.is_empty() && depth < LIMIT {
        let leaves: Vec<engine::CellKey> = remaining
            .iter()
            .copied()
            .filter(|k| {
                engine
                    .dependents_of(*k)
                    .iter()
                    .all(|d| !remaining.contains(d))
            })
            .collect();
        if leaves.is_empty() {
            break;
        }
        for k in leaves {
            remaining.remove(&k);
        }
        depth += 1;
    }
    depth
}

/// The sheet id for a name, for callers that hold one.
pub fn sheet_id(engine: &Engine, name: &str) -> Option<SheetId> {
    engine.wb.sheet_id_by_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::Action;

    fn build(cells: &[(&str, &str, &str)]) -> Engine {
        let mut e = Engine::new();
        for (sheet, a1, input) in cells {
            if !e.wb.sheets.iter().any(|s| s.name == *sheet) {
                e.apply(&Action::SheetAdd {
                    name: (*sheet).into(),
                })
                .unwrap();
            }
            e.apply(&Action::CellEdit {
                sheet: (*sheet).into(),
                addr: CellAddr::parse_a1(a1).unwrap(),
                input: (*input).into(),
            })
            .unwrap();
        }
        e
    }

    fn look(e: &Engine) -> WorkbookObservation {
        observe(e, "Sheet1", "A1", &[], "hash".into())
    }

    /// A small ledger: header row, three data rows, one derived column.
    fn ledger() -> Engine {
        build(&[
            ("Sheet1", "A1", "Item"),
            ("Sheet1", "B1", "Qty"),
            ("Sheet1", "C1", "Price"),
            ("Sheet1", "D1", "Total"),
            ("Sheet1", "A2", "Bolt"),
            ("Sheet1", "B2", "4"),
            ("Sheet1", "C2", "2.5"),
            ("Sheet1", "D2", "=B2*C2"),
            ("Sheet1", "A3", "Nut"),
            ("Sheet1", "B3", "9"),
            ("Sheet1", "C3", "0.5"),
            ("Sheet1", "D3", "=B3*C3"),
            ("Sheet1", "A4", "Washer"),
            ("Sheet1", "B4", "2"),
            ("Sheet1", "C4", "1.25"),
            ("Sheet1", "D4", "=B4*C4"),
        ])
    }

    #[test]
    fn a_table_is_found_with_its_header_body_and_column_types() {
        let obs = look(&ledger());
        let t = &obs.tables[0];
        assert_eq!(t.range, "A1:D4");
        assert_eq!(t.body_range, "A2:D4", "the body must exclude the header");
        assert_eq!(t.row_count, 3);
        let headers: Vec<&str> = t.columns.iter().map(|c| c.header.as_str()).collect();
        assert_eq!(headers, ["Item", "Qty", "Price", "Total"]);
        assert_eq!(t.columns[0].cell_type, CellType::Text);
        assert_eq!(t.columns[1].cell_type, CellType::Number);
        assert!(
            t.confidence > 0.9,
            "a clean table should not read as a guess"
        );
    }

    #[test]
    fn a_derived_column_is_flagged_as_all_formulas() {
        // The point of the flag: a policy about to write literals into a
        // computed column should be able to see that it is computed.
        let obs = look(&ledger());
        let t = &obs.tables[0];
        assert!(!t.columns[1].all_formulas, "Qty is typed in");
        assert!(t.columns[3].all_formulas, "Total is computed");
        assert_eq!(t.columns[3].formula_example.as_deref(), Some("=B2*C2"));
    }

    #[test]
    fn a_half_filled_derived_column_is_not_flagged_as_all_formulas() {
        // The interesting case: three rows, two formulas. Reporting
        // `all_formulas` here would tell a policy the column is finished
        // when the whole task may be to finish it.
        let mut e = ledger();
        e.apply(&Action::CellEdit {
            sheet: "Sheet1".into(),
            addr: CellAddr::parse_a1("D4").unwrap(),
            input: "5".into(),
        })
        .unwrap();
        let obs = look(&e);
        assert!(!obs.tables[0].columns[3].all_formulas);
    }

    #[test]
    fn a_filled_column_collapses_to_one_formula_group() {
        // The whole reason an observation stays small.
        let obs = look(&ledger());
        let group = obs
            .formula_groups
            .iter()
            .find(|g| g.count == 3)
            .expect("three identically shaped formulas should be one group");
        assert_eq!(group.example, "=B2*C2");
        assert_eq!(group.example_at, "D2");
        assert_eq!(obs.formula_group_total, 1);
    }

    #[test]
    fn formulas_that_only_look_alike_are_not_merged() {
        // =B2*C2 at D2 and =B3*C3 at D3 share a shape. =B2*C2 at D2 and
        // =B2*C2 at D3 do not — the second reads a different cell relative
        // to itself, and merging them would lose exactly the information
        // that makes a group actionable.
        let e = build(&[
            ("Sheet1", "A1", "H"),
            ("Sheet1", "B1", "H2"),
            ("Sheet1", "A2", "1"),
            ("Sheet1", "B2", "2"),
            ("Sheet1", "D2", "=A2+B2"),
            ("Sheet1", "D3", "=A2+B2"),
        ]);
        let obs = look(&e);
        assert_eq!(
            obs.formula_group_total, 2,
            "two absolute-identical formulas at different cells are two shapes: {:?}",
            obs.formula_groups
        );
    }

    #[test]
    fn errors_are_reported_with_the_formula_that_caused_them() {
        let e = build(&[
            ("Sheet1", "A1", "H"),
            ("Sheet1", "B1", "H2"),
            ("Sheet1", "A2", "1"),
            ("Sheet1", "B2", "=1/0"),
        ]);
        let obs = look(&e);
        assert_eq!(obs.error_total, 1);
        assert_eq!(obs.errors[0].addr, "B2");
        assert_eq!(obs.errors[0].code, "#DIV/0!");
        assert_eq!(obs.errors[0].formula.as_deref(), Some("=1/0"));
    }

    #[test]
    fn a_column_of_errors_is_counted_in_full_but_listed_in_part() {
        // One mistake, a hundred error cells. The list is capped; the count
        // must not be, or a policy reads "32 errors" and thinks it fixed
        // everything after fixing 32.
        let mut e = Engine::new();
        for row in 1..=100 {
            e.apply(&Action::CellEdit {
                sheet: "Sheet1".into(),
                addr: CellAddr::parse_a1(&format!("A{row}")).unwrap(),
                input: "=1/0".into(),
            })
            .unwrap();
        }
        let obs = look(&e);
        assert_eq!(obs.error_total, 100);
        assert_eq!(obs.errors.len(), ERROR_BUDGET);
    }

    #[test]
    fn an_empty_workbook_observes_without_inventing_a_table() {
        let obs = look(&Engine::new());
        assert!(obs.tables.is_empty());
        assert!(obs.sheets[0].used_range.is_none());
        assert_eq!(obs.dependencies.formula_cells, 0);
        assert_eq!(obs.dependencies.max_depth, 0);
    }

    #[test]
    fn a_block_of_numbers_with_no_header_is_reported_as_no_table_at_all() {
        // Better to find nothing than to name a header row that is data. A
        // policy that writes into row 1 because it was called a header has
        // destroyed a value.
        //
        // This test used to assert "no table is reported *confidently*",
        // which passed for the wrong reason — the list was empty, so the
        // `all` was vacuous and it would have passed no matter what
        // confidence said.
        let e = build(&[
            ("Sheet1", "A1", "1"),
            ("Sheet1", "B1", "2"),
            ("Sheet1", "A2", "3"),
            ("Sheet1", "B2", "4"),
        ]);
        assert!(look(&e).tables.is_empty());
    }

    #[test]
    fn a_table_of_mixed_columns_reports_itself_as_a_guess() {
        // Where confidence earns its keep: there *is* a header row, so the
        // table is found, but no column has a consistent type — which usually
        // means the detected range is wrong rather than that the data is
        // genuinely mixed. A caller that acts on this without noticing is
        // writing into something it has not understood.
        let e = build(&[
            ("Sheet1", "A1", "One"),
            ("Sheet1", "B1", "Two"),
            ("Sheet1", "A2", "1"),
            ("Sheet1", "B2", "text"),
            ("Sheet1", "A3", "more text"),
            ("Sheet1", "B3", "2"),
        ]);
        let obs = look(&e);
        assert_eq!(obs.tables.len(), 1);
        assert!(
            obs.tables[0].confidence < 0.5,
            "a table of mixed columns read as confident: {}",
            obs.tables[0].confidence
        );
    }

    #[test]
    fn a_two_row_table_is_less_certain_than_a_long_one() {
        // Two rows of anything look like a header and a value.
        let short = build(&[
            ("Sheet1", "A1", "Item"),
            ("Sheet1", "B1", "Qty"),
            ("Sheet1", "A2", "Bolt"),
            ("Sheet1", "B2", "4"),
        ]);
        let long = ledger();
        assert!(look(&short).tables[0].confidence < look(&long).tables[0].confidence);
    }

    #[test]
    fn dependency_depth_counts_chained_formulas_not_chained_cells() {
        let e = build(&[
            ("Sheet1", "A1", "H"),
            ("Sheet1", "B1", "H2"),
            ("Sheet1", "A2", "1"),
            ("Sheet1", "A3", "=A2+1"),
            ("Sheet1", "A4", "=A3+1"),
            ("Sheet1", "A5", "=A4+1"),
        ]);
        let obs = look(&e);
        assert_eq!(obs.dependencies.formula_cells, 3);
        assert_eq!(obs.dependencies.max_depth, 3);
        assert_eq!(
            obs.dependencies.leaf_formulas, 1,
            "only A5 is read by nobody"
        );
        assert_eq!(obs.dependencies.most_depended_on[0].0, "Sheet1!A3");
    }

    #[test]
    fn a_reference_cycle_does_not_hang_the_observation() {
        // Peeling leaves finds none when everything depends on something.
        // The loop has to notice and stop rather than spin.
        let e = build(&[("Sheet1", "A1", "=B1+1"), ("Sheet1", "B1", "=A1+1")]);
        let obs = look(&e);
        assert_eq!(obs.dependencies.formula_cells, 2);
        assert_eq!(obs.dependencies.max_depth, 0, "nothing could be peeled");
    }

    #[test]
    fn every_sheet_is_described_even_when_only_one_is_active() {
        let e = build(&[
            ("Sheet1", "A1", "1"),
            ("Data", "A1", "Name"),
            ("Data", "B1", "Value"),
            ("Data", "A2", "x"),
            ("Data", "B2", "2"),
        ]);
        let obs = look(&e);
        let names: Vec<&str> = obs.sheets.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Sheet1", "Data"]);
        assert_eq!(obs.active_sheet, "Sheet1");
        assert!(obs.tables.iter().any(|t| t.sheet == "Data"));
    }

    #[test]
    fn an_observation_round_trips_through_json() {
        // It is going to be sent to a model and stored in a trajectory; a
        // field that cannot survive serialization is not in the dataset.
        let obs = look(&ledger());
        let text = serde_json::to_string(&obs).unwrap();
        let back: WorkbookObservation = serde_json::from_str(&text).unwrap();
        assert_eq!(obs, back);
    }

    #[test]
    fn observing_the_same_workbook_twice_gives_the_same_answer() {
        // Any HashMap iteration leaking into the output would show up here.
        let e = ledger();
        assert_eq!(
            serde_json::to_string(&look(&e)).unwrap(),
            serde_json::to_string(&look(&e)).unwrap()
        );
    }
}
