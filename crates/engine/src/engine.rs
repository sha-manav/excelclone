//! The Engine: the single mutation path `apply(Action) -> Vec<Event>` plus
//! incremental recalculation.

use crate::addr::CellAddr;
use crate::ast::{Expr, RefVisit};
use crate::deps::DepGraph;
use crate::eval::EvalCtx;
use crate::model::{Cell, CellContent, CellKey, SheetId, Workbook};
use crate::parser::parse_formula;
use crate::value::{ErrorKind, Value};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Semantic actions. Every state mutation flows through `Engine::apply`.
/// The serialized action log is the source of truth: replaying it from an
/// empty workbook reproduces the exact final state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Set a cell from user input ("=SUM(A1:A3)", "42", "hello", "TRUE").
    CellEdit {
        sheet: String,
        addr: CellAddr,
        input: String,
    },
    CellClear {
        sheet: String,
        addr: CellAddr,
    },
    SheetAdd {
        name: String,
    },
    SheetRename {
        from: String,
        to: String,
    },
    SheetDelete {
        name: String,
    },
}

/// What happened as a result of an action. Carries enough context (previous
/// state) for undo to be synthesized as an inverse action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    CellEdited {
        sheet: String,
        addr: CellAddr,
        input: String,
        prev_input: Option<String>,
    },
    CellCleared {
        sheet: String,
        addr: CellAddr,
        prev_input: Option<String>,
    },
    SheetAdded {
        name: String,
    },
    SheetRenamed {
        from: String,
        to: String,
    },
    SheetDeleted {
        name: String,
    },
    /// Cells whose computed value changed due to recalculation (derived
    /// state; informational for the UI, not required for replay).
    Recalced {
        cells: Vec<(String, CellAddr)>,
    },
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ApplyError {
    #[error("unknown sheet '{0}'")]
    UnknownSheet(String),
    #[error("invalid cell address")]
    BadAddr,
    #[error("formula parse error: {0}")]
    Formula(#[from] crate::parser::ParseError),
    #[error("sheet name '{0}' already exists")]
    DuplicateSheet(String),
    #[error("cannot delete the last sheet")]
    LastSheet,
}

#[derive(Debug, Clone, Default)]
pub struct Engine {
    pub wb: Workbook,
    deps: DepGraph,
    volatile: HashSet<CellKey>,
    /// Injected clock for NOW/TODAY so evaluation is replayable; the shell
    /// updates this from event timestamps.
    pub now_ms: i64,
}

impl Engine {
    pub fn new() -> Self {
        Engine {
            wb: Workbook::new(),
            deps: DepGraph::default(),
            volatile: HashSet::new(),
            now_ms: 0,
        }
    }

    pub fn apply(&mut self, action: &Action) -> Result<Vec<Event>, ApplyError> {
        match action {
            Action::CellEdit { sheet, addr, input } => self.cell_edit(sheet, *addr, input),
            Action::CellClear { sheet, addr } => self.cell_clear(sheet, *addr),
            Action::SheetAdd { name } => self.sheet_add(name),
            Action::SheetRename { from, to } => self.sheet_rename(from, to),
            Action::SheetDelete { name } => self.sheet_delete(name),
        }
    }

    fn sheet_id(&self, name: &str) -> Result<SheetId, ApplyError> {
        self.wb
            .sheet_id_by_name(name)
            .ok_or_else(|| ApplyError::UnknownSheet(name.to_string()))
    }

    fn cell_edit(
        &mut self,
        sheet: &str,
        addr: CellAddr,
        input: &str,
    ) -> Result<Vec<Event>, ApplyError> {
        if !addr.is_valid() {
            return Err(ApplyError::BadAddr);
        }
        let sid = self.sheet_id(sheet)?;
        let key = CellKey { sheet: sid, addr };
        let cell = build_cell(input)?;
        let prev_input = self.prev_input(key);

        // Maintain the dependency graph for the new content.
        match &cell.content {
            CellContent::Formula { ast, .. } => {
                let (exact, ranges) = self.precedents_of_ast(sid, ast);
                self.deps.set_precedents(key, exact, ranges);
                if ast.is_volatile() {
                    self.volatile.insert(key);
                } else {
                    self.volatile.remove(&key);
                }
            }
            CellContent::Literal(_) => {
                self.deps.clear(key);
                self.volatile.remove(&key);
            }
        }
        self.wb
            .sheet_mut(sid)
            .expect("sheet exists")
            .cells
            .insert(addr, cell);

        let recalced = self.recalc(vec![key]);
        let mut events = vec![Event::CellEdited {
            sheet: self.wb.sheet(sid).unwrap().name.clone(),
            addr,
            input: input.to_string(),
            prev_input,
        }];
        if !recalced.is_empty() {
            events.push(Event::Recalced {
                cells: self.keys_to_names(&recalced),
            });
        }
        Ok(events)
    }

    fn cell_clear(&mut self, sheet: &str, addr: CellAddr) -> Result<Vec<Event>, ApplyError> {
        let sid = self.sheet_id(sheet)?;
        let key = CellKey { sheet: sid, addr };
        let prev_input = self.prev_input(key);
        self.deps.clear(key);
        self.volatile.remove(&key);
        self.wb
            .sheet_mut(sid)
            .expect("sheet exists")
            .cells
            .remove(&addr);
        let recalced = self.recalc(vec![key]);
        let mut events = vec![Event::CellCleared {
            sheet: self.wb.sheet(sid).unwrap().name.clone(),
            addr,
            prev_input,
        }];
        if !recalced.is_empty() {
            events.push(Event::Recalced {
                cells: self.keys_to_names(&recalced),
            });
        }
        Ok(events)
    }

    fn sheet_add(&mut self, name: &str) -> Result<Vec<Event>, ApplyError> {
        if self.wb.sheet_by_name(name).is_some() {
            return Err(ApplyError::DuplicateSheet(name.to_string()));
        }
        self.wb.add_sheet(name);
        // A new sheet can satisfy previously-broken cross-sheet refs.
        self.rebuild_deps_and_recalc_all();
        Ok(vec![Event::SheetAdded {
            name: name.to_string(),
        }])
    }

    fn sheet_rename(&mut self, from: &str, to: &str) -> Result<Vec<Event>, ApplyError> {
        let sid = self.sheet_id(from)?;
        if !from.eq_ignore_ascii_case(to) && self.wb.sheet_by_name(to).is_some() {
            return Err(ApplyError::DuplicateSheet(to.to_string()));
        }
        // Excel rewrites formulas on rename; we do the same so formula text
        // stays consistent with sheet names.
        self.rewrite_sheet_refs(from, Some(to));
        self.wb.sheet_mut(sid).unwrap().name = to.to_string();
        self.rebuild_deps_and_recalc_all();
        Ok(vec![Event::SheetRenamed {
            from: from.to_string(),
            to: to.to_string(),
        }])
    }

    fn sheet_delete(&mut self, name: &str) -> Result<Vec<Event>, ApplyError> {
        let sid = self.sheet_id(name)?;
        if self.wb.sheets.len() == 1 {
            return Err(ApplyError::LastSheet);
        }
        // Refs into the deleted sheet become #REF! (loud failure).
        self.rewrite_sheet_refs(name, None);
        self.wb.sheets.retain(|s| s.id != sid);
        self.volatile.retain(|k| k.sheet != sid);
        self.rebuild_deps_and_recalc_all();
        Ok(vec![Event::SheetDeleted {
            name: name.to_string(),
        }])
    }

    /// Rewrite formulas referencing sheet `from`: rename to `to`, or replace
    /// the ref with #REF! when `to` is None (sheet deleted).
    fn rewrite_sheet_refs(&mut self, from: &str, to: Option<&str>) {
        for si in 0..self.wb.sheets.len() {
            let addrs: Vec<CellAddr> = self.wb.sheets[si]
                .cells
                .iter()
                .filter(|(_, c)| c.is_formula())
                .map(|(a, _)| *a)
                .collect();
            for addr in addrs {
                let cell = self.wb.sheets[si].cells.get(&addr).unwrap();
                let CellContent::Formula { ast, cached, .. } = &cell.content else {
                    continue;
                };
                let mut changed = false;
                let new_ast = rewrite_sheet_in_expr(ast, from, to, &mut changed);
                if changed {
                    let cached = cached.clone();
                    let src = new_ast.to_formula();
                    self.wb.sheets[si].cells.insert(
                        addr,
                        Cell {
                            content: CellContent::Formula {
                                src,
                                ast: new_ast,
                                cached,
                            },
                        },
                    );
                }
            }
        }
    }

    fn prev_input(&self, key: CellKey) -> Option<String> {
        self.wb
            .sheet(key.sheet)
            .and_then(|s| s.cells.get(&key.addr))
            .map(|c| c.input())
    }

    fn keys_to_names(&self, keys: &[CellKey]) -> Vec<(String, CellAddr)> {
        keys.iter()
            .filter_map(|k| self.wb.sheet(k.sheet).map(|s| (s.name.clone(), k.addr)))
            .collect()
    }

    /// Resolve AST refs to concrete precedent keys for the dependency graph.
    fn precedents_of_ast(
        &self,
        current: SheetId,
        ast: &Expr,
    ) -> (Vec<CellKey>, Vec<(SheetId, crate::addr::RangeAddr)>) {
        let mut exact = Vec::new();
        let mut ranges = Vec::new();
        ast.visit_refs(&mut |r| match r {
            RefVisit::Cell(c) => {
                let sid = match &c.sheet {
                    None => Some(current),
                    Some(n) => self.wb.sheet_id_by_name(n),
                };
                if let Some(sid) = sid {
                    exact.push(CellKey {
                        sheet: sid,
                        addr: c.r.addr(),
                    });
                }
            }
            RefVisit::Range(rr) => {
                let sid = match &rr.sheet {
                    None => Some(current),
                    Some(n) => self.wb.sheet_id_by_name(n),
                };
                if let Some(sid) = sid {
                    ranges.push((
                        sid,
                        crate::addr::RangeAddr::new(rr.start.addr(), rr.end.addr()),
                    ));
                }
            }
        });
        (exact, ranges)
    }

    fn rebuild_deps_and_recalc_all(&mut self) {
        self.deps.reset();
        self.volatile.clear();
        let mut formula_keys = Vec::new();
        for s in &self.wb.sheets {
            for (addr, cell) in &s.cells {
                if let CellContent::Formula { ast, .. } = &cell.content {
                    formula_keys.push((
                        CellKey {
                            sheet: s.id,
                            addr: *addr,
                        },
                        ast.clone(),
                    ));
                }
            }
        }
        formula_keys.sort_by_key(|(k, _)| *k);
        for (key, ast) in &formula_keys {
            let (exact, ranges) = self.precedents_of_ast(key.sheet, ast);
            self.deps.set_precedents(*key, exact, ranges);
            if ast.is_volatile() {
                self.volatile.insert(*key);
            }
        }
        self.recalc(formula_keys.into_iter().map(|(k, _)| k).collect());
    }

    /// Incremental recalculation from seed cells: mark the transitive
    /// dependent closure dirty (plus volatile cells), evaluate in
    /// topological order, mark cycles #CIRC!. Returns cells whose computed
    /// value changed.
    pub fn recalc(&mut self, seeds: Vec<CellKey>) -> Vec<CellKey> {
        // 1. Dirty closure.
        let mut dirty: HashSet<CellKey> = HashSet::new();
        let mut queue: Vec<CellKey> = Vec::new();
        for s in seeds.into_iter().chain(self.volatile.iter().copied()) {
            if dirty.insert(s) {
                queue.push(s);
            }
        }
        while let Some(k) = queue.pop() {
            for d in self.deps.dependents_of(k) {
                if dirty.insert(d) {
                    queue.push(d);
                }
            }
        }

        // 2. Restrict to formula cells; build the intra-dirty subgraph.
        let mut dirty_formulas: Vec<CellKey> = dirty
            .iter()
            .copied()
            .filter(|k| {
                self.wb
                    .sheet(k.sheet)
                    .and_then(|s| s.cells.get(&k.addr))
                    .map(|c| c.is_formula())
                    .unwrap_or(false)
            })
            .collect();
        dirty_formulas.sort();
        let dirty_set: HashSet<CellKey> = dirty_formulas.iter().copied().collect();

        let mut indeg: HashMap<CellKey, usize> = dirty_formulas.iter().map(|k| (*k, 0)).collect();
        let mut edges: HashMap<CellKey, Vec<CellKey>> = HashMap::new(); // precedent -> dependents
        for &d in &dirty_formulas {
            if let Some((exact, ranges)) = self.deps.precedents_of(d) {
                let mut precs: HashSet<CellKey> = HashSet::new();
                for p in exact {
                    if *p != d && dirty_set.contains(p) {
                        precs.insert(*p);
                    }
                }
                for (sid, range) in ranges {
                    for &x in &dirty_formulas {
                        if x != d && x.sheet == *sid && range.contains(x.addr) {
                            precs.insert(x);
                        }
                    }
                }
                // Self-references (A1 = A1+1) are cycles; keep the self edge
                // so the cell never enters the ready queue.
                let self_ref = exact.contains(&d)
                    || ranges
                        .iter()
                        .any(|(sid, r)| *sid == d.sheet && r.contains(d.addr));
                if self_ref {
                    edges.entry(d).or_default().push(d);
                    *indeg.get_mut(&d).unwrap() += 1;
                }
                for p in precs {
                    edges.entry(p).or_default().push(d);
                    *indeg.get_mut(&d).unwrap() += 1;
                }
            }
        }

        // 3. Kahn's algorithm in deterministic order.
        let mut ready: Vec<CellKey> = dirty_formulas
            .iter()
            .copied()
            .filter(|k| indeg[k] == 0)
            .collect();
        ready.sort();
        ready.reverse(); // pop from the end -> ascending order
        let mut changed: Vec<CellKey> = Vec::new();
        let mut processed: HashSet<CellKey> = HashSet::new();
        while let Some(k) = ready.pop() {
            processed.insert(k);
            if self.eval_and_store(k) {
                changed.push(k);
            }
            if let Some(deps) = edges.get(&k) {
                let mut newly: Vec<CellKey> = Vec::new();
                for d in deps.clone() {
                    let e = indeg.get_mut(&d).unwrap();
                    *e -= 1;
                    if *e == 0 {
                        newly.push(d);
                    }
                }
                newly.sort();
                for n in newly.into_iter().rev() {
                    ready.push(n);
                }
            }
        }

        // 4. Anything unprocessed is part of (or downstream inside) a cycle.
        for &k in &dirty_formulas {
            if !processed.contains(&k) && self.store_value(k, Value::Error(ErrorKind::Circ)) {
                changed.push(k);
            }
        }
        changed.sort();
        changed
    }

    /// Evaluate one formula cell and store the result; true if it changed.
    fn eval_and_store(&mut self, key: CellKey) -> bool {
        let Some(sheet) = self.wb.sheet(key.sheet) else {
            return false;
        };
        let Some(cell) = sheet.cells.get(&key.addr) else {
            return false;
        };
        let CellContent::Formula { ast, .. } = &cell.content else {
            return false;
        };
        let ctx = EvalCtx {
            wb: &self.wb,
            sheet: key.sheet,
            now_ms: self.now_ms,
        };
        let ast = ast.clone();
        let v = ctx.eval_scalar(&ast);
        self.store_value(key, v)
    }

    fn store_value(&mut self, key: CellKey, v: Value) -> bool {
        let Some(sheet) = self.wb.sheet_mut(key.sheet) else {
            return false;
        };
        let Some(cell) = sheet.cells.get_mut(&key.addr) else {
            return false;
        };
        let CellContent::Formula { cached, .. } = &mut cell.content else {
            return false;
        };
        if *cached == v {
            false
        } else {
            *cached = v;
            true
        }
    }

    /// Convenience accessors used by tests and the UI layer.
    pub fn value_at(&self, sheet: &str, a1: &str) -> Value {
        let Some(s) = self.wb.sheet_by_name(sheet) else {
            return Value::Error(ErrorKind::Ref);
        };
        let Some(addr) = CellAddr::parse_a1(a1) else {
            return Value::Error(ErrorKind::Ref);
        };
        s.value(addr)
    }
}

/// Parse raw user input into a cell (formula, number, bool, error, or text).
fn build_cell(input: &str) -> Result<Cell, ApplyError> {
    if let Some(body) = input.strip_prefix('=') {
        let ast = parse_formula(body)?;
        return Ok(Cell {
            content: CellContent::Formula {
                src: body.to_string(),
                ast,
                cached: Value::Empty,
            },
        });
    }
    // Leading apostrophe forces text.
    if let Some(text) = input.strip_prefix('\'') {
        return Ok(Cell::literal(Value::Text(text.to_string())));
    }
    if let Some(n) = crate::eval::parse_number_text(input) {
        return Ok(Cell::literal(Value::Number(n)));
    }
    if input.eq_ignore_ascii_case("TRUE") {
        return Ok(Cell::literal(Value::Bool(true)));
    }
    if input.eq_ignore_ascii_case("FALSE") {
        return Ok(Cell::literal(Value::Bool(false)));
    }
    if let Some(e) = ErrorKind::from_code(input) {
        return Ok(Cell::literal(Value::Error(e)));
    }
    Ok(Cell::literal(Value::Text(input.to_string())))
}

/// Replace or strip references to a sheet name across an expression.
fn rewrite_sheet_in_expr(e: &Expr, from: &str, to: Option<&str>, changed: &mut bool) -> Expr {
    match e {
        Expr::Cell(c) if sheet_matches(&c.sheet, from) => {
            *changed = true;
            match to {
                Some(t) => Expr::Cell(crate::ast::CellRef {
                    sheet: Some(t.to_string()),
                    r: c.r,
                }),
                None => Expr::Error(ErrorKind::Ref),
            }
        }
        Expr::Range(r) if sheet_matches(&r.sheet, from) => {
            *changed = true;
            match to {
                Some(t) => Expr::Range(crate::ast::RangeRef {
                    sheet: Some(t.to_string()),
                    start: r.start,
                    end: r.end,
                }),
                None => Expr::Error(ErrorKind::Ref),
            }
        }
        Expr::Func(name, args) => Expr::Func(
            name.clone(),
            args.iter()
                .map(|a| rewrite_sheet_in_expr(a, from, to, changed))
                .collect(),
        ),
        Expr::Binary(op, l, r) => Expr::Binary(
            *op,
            Box::new(rewrite_sheet_in_expr(l, from, to, changed)),
            Box::new(rewrite_sheet_in_expr(r, from, to, changed)),
        ),
        Expr::Neg(x) => Expr::Neg(Box::new(rewrite_sheet_in_expr(x, from, to, changed))),
        Expr::Pos(x) => Expr::Pos(Box::new(rewrite_sheet_in_expr(x, from, to, changed))),
        Expr::Percent(x) => Expr::Percent(Box::new(rewrite_sheet_in_expr(x, from, to, changed))),
        other => other.clone(),
    }
}

fn sheet_matches(sheet: &Option<String>, name: &str) -> bool {
    sheet
        .as_deref()
        .map(|s| s.eq_ignore_ascii_case(name))
        .unwrap_or(false)
}
