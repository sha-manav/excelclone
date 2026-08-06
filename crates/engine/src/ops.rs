//! Structural operations: paste, fill, insert/delete rows and columns,
//! sort, filter, merge. Each one records enough previous state for undo and
//! rewrites references according to the rules in `refs`.

use crate::addr::{CellAddr, RangeAddr};
use crate::engine::{ApplyError, Engine, FilterSpec, PasteMode, SortKey, UndoState};
use crate::format::{FormatId, FormatPatch};
use crate::model::{Cell, CellContent, SheetId};
use crate::refs::{self, Axis, MoveShift, StructuralShift};
use crate::value::Value;
use std::collections::{BTreeMap, HashMap};

/// One cell's previous format, for undo.
type FormatPatchRecord = (SheetId, CellAddr, Option<FormatId>);

impl Engine {
    /// Apply presentation patches across a range, returning the previous
    /// format of every cell touched.
    pub(crate) fn op_format(
        &mut self,
        sheet: SheetId,
        range: RangeAddr,
        patches: &[FormatPatch],
    ) -> Vec<FormatPatchRecord> {
        let mut undo = Vec::new();
        for addr in range.iter_cells() {
            if !addr.is_valid() {
                continue;
            }
            let before = self.wb.sheet(sheet).expect("sheet exists").format_id(addr);
            let mut f = self.wb.formats.resolve(before);
            for p in patches {
                p.apply_to(&mut f, range, addr.row, addr.col);
            }
            let after = self.wb.formats.intern(f);
            if after == before {
                continue;
            }
            let s = self.wb.sheet_mut(sheet).unwrap();
            match after {
                Some(id) => s.formats.insert(addr, id),
                None => s.formats.remove(&addr),
            };
            undo.push((sheet, addr, before));
        }
        undo
    }

    /// Move a block of formats from one place to another, mirroring what
    /// `op_paste` does to the contents. Returns the previous format of every
    /// destination (and, for a cut, every vacated source).
    fn move_formats(
        &mut self,
        src_sheet: SheetId,
        src: RangeAddr,
        dst_sheet: SheetId,
        dst: RangeAddr,
        tiles: (u32, u32),
        cut: bool,
    ) -> Vec<FormatPatchRecord> {
        let (tile_rows, tile_cols) = tiles;
        let src_formats: Vec<(CellAddr, Option<FormatId>)> = src
            .iter_cells()
            .map(|a| {
                (
                    a,
                    self.wb.sheet(src_sheet).expect("sheet exists").format_id(a),
                )
            })
            .collect();
        let mut prev = Vec::new();
        if cut {
            for (a, _) in &src_formats {
                let old = self.wb.sheet_mut(src_sheet).unwrap().formats.remove(a);
                prev.push((src_sheet, *a, old));
            }
        }
        for tr in 0..tile_rows {
            for tc in 0..tile_cols {
                for (sa, id) in &src_formats {
                    let da = CellAddr::new(
                        dst.start.row + tr * src.rows() + (sa.row - src.start.row),
                        dst.start.col + tc * src.cols() + (sa.col - src.start.col),
                    );
                    if !da.is_valid() {
                        continue;
                    }
                    let s = self.wb.sheet_mut(dst_sheet).unwrap();
                    let old = match id {
                        Some(i) => s.formats.insert(da, *i),
                        None => s.formats.remove(&da),
                    };
                    if old != *id {
                        prev.push((dst_sheet, da, old));
                    }
                }
            }
        }
        prev
    }

    /// Clear every cell in a range.
    pub(crate) fn op_range_clear(
        &mut self,
        sheet: SheetId,
        range: RangeAddr,
    ) -> Result<UndoState, ApplyError> {
        let mut prev = Vec::new();
        let addrs: Vec<CellAddr> = self
            .wb
            .sheet(sheet)
            .expect("sheet exists")
            .cells
            .keys()
            .filter(|a| range.contains(**a))
            .copied()
            .collect();
        for a in addrs {
            let old = self.wb.sheet_mut(sheet).unwrap().cells.remove(&a);
            prev.push((sheet, a, old));
        }
        Ok(UndoState::Cells(prev))
    }

    /// Copy or cut/paste a block, rewriting references as Excel does.
    pub(crate) fn op_paste(
        &mut self,
        src_sheet: SheetId,
        src: RangeAddr,
        dst_sheet: SheetId,
        dst: RangeAddr,
        mode: PasteMode,
        cut: bool,
    ) -> Result<UndoState, ApplyError> {
        // Snapshot the source block before any mutation: a cut into an
        // overlapping target would otherwise read cells it has overwritten.
        let src_cells: Vec<(CellAddr, Option<Cell>)> = src
            .iter_cells()
            .map(|a| {
                (
                    a,
                    self.wb
                        .sheet(src_sheet)
                        .expect("sheet exists")
                        .cells
                        .get(&a)
                        .cloned(),
                )
            })
            .collect();

        // A target larger than the source tiles, provided it is an exact
        // multiple in each axis (Excel behavior); otherwise paste once.
        let (tile_rows, tile_cols) = tiling(src, dst);
        let mut prev: Vec<(SheetId, CellAddr, Option<Cell>)> = Vec::new();

        if cut {
            for (a, _) in &src_cells {
                let old = self.wb.sheet_mut(src_sheet).unwrap().cells.remove(a);
                prev.push((src_sheet, *a, old));
            }
        }

        for tr in 0..tile_rows {
            for tc in 0..tile_cols {
                let anchor = CellAddr::new(
                    dst.start.row + tr * src.rows(),
                    dst.start.col + tc * src.cols(),
                );
                for (sa, cell) in &src_cells {
                    let dr = sa.row - src.start.row;
                    let dc = sa.col - src.start.col;
                    let da = CellAddr::new(anchor.row + dr, anchor.col + dc);
                    if !da.is_valid() {
                        continue;
                    }
                    let new_cell = cell
                        .as_ref()
                        .map(|c| transform_pasted(c, *sa, da, mode, cut));
                    let old = match &new_cell {
                        Some(nc) => self
                            .wb
                            .sheet_mut(dst_sheet)
                            .unwrap()
                            .cells
                            .insert(da, nc.clone()),
                        None => self.wb.sheet_mut(dst_sheet).unwrap().cells.remove(&da),
                    };
                    prev.push((dst_sheet, da, old));
                }
            }
        }

        // Cut also relocates every reference elsewhere that pointed into the
        // moved block.
        if cut {
            let mv = MoveShift {
                sheet: src_sheet,
                from: src,
                dr: dst.start.row as i64 - src.start.row as i64,
                dc: dst.start.col as i64 - src.start.col as i64,
            };
            let mut touched =
                self.rewrite_all_formulas(|e, cur, lookup| refs::moved(e, &mv, cur, lookup));
            prev.append(&mut touched);
        }

        // Formatting travels with a normal paste and a cut, but not with
        // paste-values: "paste values" means the numbers without the dressing.
        if matches!(mode, PasteMode::Values) {
            return Ok(UndoState::Cells(prev));
        }
        let formats =
            self.move_formats(src_sheet, src, dst_sheet, dst, (tile_rows, tile_cols), cut);
        Ok(UndoState::Compound(vec![
            UndoState::Cells(prev),
            UndoState::Formats(formats),
        ]))
    }

    /// Fill a source block across a target range, extending series.
    pub(crate) fn op_fill(
        &mut self,
        sheet: SheetId,
        src: RangeAddr,
        dst: RangeAddr,
    ) -> Result<UndoState, ApplyError> {
        let down = dst.end.row > src.end.row || dst.start.row < src.start.row;
        let mut prev = Vec::new();
        let mut prev_formats: Vec<FormatPatchRecord> = Vec::new();

        // Each line (column when filling down, row when filling right) is an
        // independent series seeded by the source cells on that line.
        let lines: Vec<u32> = if down {
            (src.start.col..=src.end.col).collect()
        } else {
            (src.start.row..=src.end.row).collect()
        };

        for line in lines {
            let seed_addrs: Vec<CellAddr> = if down {
                (src.start.row..=src.end.row)
                    .map(|r| CellAddr::new(r, line))
                    .collect()
            } else {
                (src.start.col..=src.end.col)
                    .map(|c| CellAddr::new(line, c))
                    .collect()
            };
            let seeds: Vec<Option<Cell>> = seed_addrs
                .iter()
                .map(|a| self.wb.sheet(sheet).unwrap().cells.get(a).cloned())
                .collect();
            let series = Series::detect(&seeds);

            // Target positions on this line, outside the source block,
            // ordered outward from the source so step counts are correct.
            let (before, after) = fill_targets(src, dst, line, down);
            for (step, addr) in after.into_iter().chain(before) {
                let (old, fmt) =
                    self.write_series_cell(sheet, &seeds, &seed_addrs, &series, step, addr);
                prev.push((sheet, addr, old));
                if let Some(f) = fmt {
                    prev_formats.push(f);
                }
            }
        }
        Ok(UndoState::Compound(vec![
            UndoState::Cells(prev),
            UndoState::Formats(prev_formats),
        ]))
    }

    /// Write one filled cell; `step` is signed distance from the source block
    /// (1, 2, 3... after the block; -1, -2... before it). Returns the cell
    /// that was there and, when the fill also changed the cell's formatting,
    /// the format it had.
    #[allow(clippy::type_complexity)]
    fn write_series_cell(
        &mut self,
        sheet: SheetId,
        seeds: &[Option<Cell>],
        seed_addrs: &[CellAddr],
        series: &Series,
        step: i64,
        addr: CellAddr,
    ) -> (Option<Cell>, Option<FormatPatchRecord>) {
        let n = seeds.len() as i64;
        // Which seed this position repeats, cycling through the block.
        let idx = if step > 0 {
            ((step - 1) % n) as usize
        } else {
            ((n + (step % n)) % n) as usize
        };
        let src_addr = seed_addrs[idx];
        let new_cell = seeds[idx].as_ref().map(|c| match &c.content {
            // Formulas always shift by the real distance moved.
            CellContent::Formula { ast, .. } => {
                let dr = addr.row as i64 - src_addr.row as i64;
                let dc = addr.col as i64 - src_addr.col as i64;
                let new_ast = refs::offset(ast, dr, dc);
                Cell {
                    content: CellContent::Formula {
                        src: new_ast.to_formula(),
                        ast: new_ast,
                        cached: Value::Empty,
                    },
                }
            }
            CellContent::Literal(v) => Cell::literal(series.extend(v, step, n)),
        });
        // A fill drags the seed's formatting along with its value, which is
        // what makes dragging a formatted total row down do the right thing.
        let seed_format = self.wb.sheet(sheet).unwrap().format_id(src_addr);
        let s = self.wb.sheet_mut(sheet).unwrap();
        let old_format = match seed_format {
            Some(id) => s.formats.insert(addr, id),
            None => s.formats.remove(&addr),
        };
        let format_record = (old_format != seed_format).then_some((sheet, addr, old_format));
        let old_cell = match new_cell {
            Some(nc) => self.wb.sheet_mut(sheet).unwrap().cells.insert(addr, nc),
            None => self.wb.sheet_mut(sheet).unwrap().cells.remove(&addr),
        };
        (old_cell, format_record)
    }

    /// Insert or delete rows/columns, remapping the sheet's cells and every
    /// reference in the workbook.
    pub(crate) fn op_shift(
        &mut self,
        sheet: SheetId,
        axis: Axis,
        at: u32,
        count: u32,
        insert: bool,
    ) -> Result<UndoState, ApplyError> {
        if count == 0 {
            return Ok(UndoState::Sheets(self.wb.sheets.clone()));
        }
        let before = UndoState::Sheets(self.wb.sheets.clone());
        let shift = StructuralShift {
            sheet,
            axis,
            at,
            count,
            insert,
        };

        // Move the cells themselves.
        let s = self.wb.sheet_mut(sheet).expect("sheet exists");
        let mut moved_cells: HashMap<CellAddr, Cell> = HashMap::new();
        for (addr, cell) in s.cells.drain() {
            let idx = match axis {
                Axis::Row => addr.row,
                Axis::Col => addr.col,
            };
            if let Some(new_idx) = shift.map_index(idx) {
                let new_addr = match axis {
                    Axis::Row => CellAddr::new(new_idx, addr.col),
                    Axis::Col => CellAddr::new(addr.row, new_idx),
                };
                if new_addr.is_valid() {
                    moved_cells.insert(new_addr, cell);
                }
            }
        }
        s.cells = moved_cells;

        // Formats move with the rows and columns they sit on. Cells shifted
        // off the end of the sheet drop their formatting the same way they
        // drop their contents.
        let mut moved_formats: BTreeMap<CellAddr, FormatId> = BTreeMap::new();
        for (addr, id) in std::mem::take(&mut s.formats) {
            let idx = match axis {
                Axis::Row => addr.row,
                Axis::Col => addr.col,
            };
            if let Some(new_idx) = shift.map_index(idx) {
                let new_addr = match axis {
                    Axis::Row => CellAddr::new(new_idx, addr.col),
                    Axis::Col => CellAddr::new(addr.row, new_idx),
                };
                if new_addr.is_valid() {
                    moved_formats.insert(new_addr, id);
                }
            }
        }
        s.formats = moved_formats;

        // A column's width belongs to the column, so it travels with it:
        // inserting in front of a widened column and finding the width left
        // behind on its neighbour is the sort of thing that makes a sheet
        // subtly wrong in a way nobody can point at.
        let bound = match axis {
            Axis::Row => crate::addr::MAX_ROWS,
            Axis::Col => crate::addr::MAX_COLS,
        };
        let sizes = match axis {
            Axis::Row => &mut s.row_heights,
            Axis::Col => &mut s.col_widths,
        };
        *sizes = std::mem::take(sizes)
            .into_iter()
            .filter_map(|(i, px)| Some((shift.map_index(i)?, px)))
            .filter(|(i, _)| *i < bound)
            .collect();

        // Merged regions move with their cells; fully-deleted ones vanish.
        s.merged = s
            .merged
            .iter()
            .filter_map(|m| {
                let (s0, e0) = match axis {
                    Axis::Row => (m.start.row, m.end.row),
                    Axis::Col => (m.start.col, m.end.col),
                };
                let (ns, ne) = shift.map_span(s0, e0)?;
                let mut start = m.start;
                let mut end = m.end;
                match axis {
                    Axis::Row => {
                        start.row = ns;
                        end.row = ne;
                    }
                    Axis::Col => {
                        start.col = ns;
                        end.col = ne;
                    }
                }
                Some(RangeAddr::new(start, end))
            })
            .collect();

        // Rewrite references workbook-wide.
        self.rewrite_all_formulas(|e, cur, lookup| refs::structural(e, &shift, cur, lookup));
        Ok(before)
    }

    /// Sort a range by one or more key columns.
    pub(crate) fn op_sort(
        &mut self,
        sheet: SheetId,
        range: RangeAddr,
        keys: &[SortKey],
        has_header: bool,
    ) -> Result<UndoState, ApplyError> {
        let first_row = if has_header {
            range.start.row + 1
        } else {
            range.start.row
        };
        if first_row > range.end.row {
            return Ok(UndoState::Cells(Vec::new()));
        }

        // Capture each row of the range as a block of cells.
        let mut rows: Vec<(u32, Vec<Option<Cell>>)> = (first_row..=range.end.row)
            .map(|r| {
                let cells = (range.start.col..=range.end.col)
                    .map(|c| {
                        self.wb
                            .sheet(sheet)
                            .unwrap()
                            .cells
                            .get(&CellAddr::new(r, c))
                            .cloned()
                    })
                    .collect();
                (r, cells)
            })
            .collect();

        // Formatting belongs to the row, not to the position: sorting a table
        // whose total row is bold must move the bold with it.
        let row_formats: HashMap<u32, Vec<Option<FormatId>>> = rows
            .iter()
            .map(|(r, _)| {
                (
                    *r,
                    (range.start.col..=range.end.col)
                        .map(|c| {
                            self.wb
                                .sheet(sheet)
                                .unwrap()
                                .format_id(CellAddr::new(*r, c))
                        })
                        .collect(),
                )
            })
            .collect();

        let sort_values: HashMap<u32, Vec<Value>> = rows
            .iter()
            .map(|(r, cells)| {
                (
                    *r,
                    cells
                        .iter()
                        .map(|c| {
                            c.as_ref()
                                .map(|c| c.value().clone())
                                .unwrap_or(Value::Empty)
                        })
                        .collect(),
                )
            })
            .collect();

        // Stable sort by the keys in order; ties keep the original order.
        rows.sort_by(|(ra, _), (rb, _)| {
            for k in keys {
                let ci = (k.column.saturating_sub(range.start.col)) as usize;
                let va = sort_values[ra].get(ci).unwrap_or(&Value::Empty);
                let vb = sort_values[rb].get(ci).unwrap_or(&Value::Empty);
                // Excel sorts blanks last regardless of direction.
                let ord = match (va.is_empty(), vb.is_empty()) {
                    (true, true) => std::cmp::Ordering::Equal,
                    (true, false) => std::cmp::Ordering::Greater,
                    (false, true) => std::cmp::Ordering::Less,
                    (false, false) => {
                        let o = crate::eval::compare_values(va, vb);
                        if k.ascending {
                            o
                        } else {
                            o.reverse()
                        }
                    }
                };
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            ra.cmp(rb)
        });

        // Write the rows back in their new order, shifting formula refs by
        // the distance each row travelled.
        let mut prev = Vec::new();
        let mut prev_formats: Vec<FormatPatchRecord> = Vec::new();
        for (new_i, (old_row, cells)) in rows.into_iter().enumerate() {
            let new_row = first_row + new_i as u32;
            let dr = new_row as i64 - old_row as i64;
            let formats = &row_formats[&old_row];
            for (ci, cell) in cells.into_iter().enumerate() {
                let addr = CellAddr::new(new_row, range.start.col + ci as u32);
                let want = formats.get(ci).copied().flatten();
                let s = self.wb.sheet_mut(sheet).unwrap();
                let had = match want {
                    Some(id) => s.formats.insert(addr, id),
                    None => s.formats.remove(&addr),
                };
                if had != want {
                    prev_formats.push((sheet, addr, had));
                }
                let new_cell = cell.map(|c| match &c.content {
                    CellContent::Formula { ast, .. } if dr != 0 => {
                        let new_ast = refs::offset(ast, dr, 0);
                        Cell {
                            content: CellContent::Formula {
                                src: new_ast.to_formula(),
                                ast: new_ast,
                                cached: Value::Empty,
                            },
                        }
                    }
                    _ => c,
                });
                let old = match new_cell {
                    Some(nc) => self.wb.sheet_mut(sheet).unwrap().cells.insert(addr, nc),
                    None => self.wb.sheet_mut(sheet).unwrap().cells.remove(&addr),
                };
                prev.push((sheet, addr, old));
            }
        }
        Ok(UndoState::Compound(vec![
            UndoState::Cells(prev),
            UndoState::Formats(prev_formats),
        ]))
    }

    /// Apply a value filter: rows whose key cell is not in the allowed set
    /// are hidden. Filtering is a view state; no cell values change.
    pub(crate) fn op_filter(
        &mut self,
        sheet: SheetId,
        spec: Option<FilterSpec>,
    ) -> Result<UndoState, ApplyError> {
        let before = UndoState::Sheets(self.wb.sheets.clone());
        let hidden = match &spec {
            None => Vec::new(),
            Some(f) => {
                let s = self.wb.sheet(sheet).expect("sheet exists");
                (f.range.start.row + 1..=f.range.end.row)
                    .filter(|r| {
                        let v = s.value(CellAddr::new(*r, f.column));
                        !f.allowed.contains(&v.display())
                    })
                    .collect()
            }
        };
        let s = self.wb.sheet_mut(sheet).expect("sheet exists");
        s.filter = spec;
        s.hidden_rows = hidden;
        Ok(before)
    }

    pub(crate) fn op_merge(
        &mut self,
        sheet: SheetId,
        range: RangeAddr,
        merge: bool,
    ) -> Result<UndoState, ApplyError> {
        let before = UndoState::Sheets(self.wb.sheets.clone());
        let s = self.wb.sheet_mut(sheet).expect("sheet exists");
        // Merging clears every cell but the anchor, as in Excel.
        s.merged.retain(|m| !ranges_overlap(*m, range));
        if merge {
            s.merged.push(range);
            s.merged.sort_by_key(|r| (r.start.row, r.start.col));
            let victims: Vec<CellAddr> = range.iter_cells().filter(|a| *a != range.start).collect();
            for a in victims {
                s.cells.remove(&a);
            }
        }
        Ok(before)
    }

    /// Apply a transform to every formula in the workbook, returning the
    /// previous state of each cell it changed (for undo).
    pub(crate) fn rewrite_all_formulas(
        &mut self,
        f: impl Fn(&crate::ast::Expr, SheetId, &dyn Fn(&str) -> Option<SheetId>) -> crate::ast::Expr,
    ) -> Vec<(SheetId, CellAddr, Option<Cell>)> {
        let names: Vec<(String, SheetId)> = self
            .wb
            .sheets
            .iter()
            .map(|s| (s.name.clone(), s.id))
            .collect();
        let lookup = move |n: &str| -> Option<SheetId> {
            names
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(n))
                .map(|(_, id)| *id)
        };

        let mut touched = Vec::new();
        for si in 0..self.wb.sheets.len() {
            let sid = self.wb.sheets[si].id;
            let formula_addrs: Vec<CellAddr> = self.wb.sheets[si]
                .cells
                .iter()
                .filter(|(_, c)| c.is_formula())
                .map(|(a, _)| *a)
                .collect();
            for addr in formula_addrs {
                let cell = self.wb.sheets[si].cells.get(&addr).unwrap();
                let CellContent::Formula { ast, cached, .. } = &cell.content else {
                    continue;
                };
                let new_ast = f(ast, sid, &lookup);
                if new_ast == *ast {
                    continue;
                }
                let old = cell.clone();
                let cached = cached.clone();
                self.wb.sheets[si].cells.insert(
                    addr,
                    Cell {
                        content: CellContent::Formula {
                            src: new_ast.to_formula(),
                            ast: new_ast,
                            cached,
                        },
                    },
                );
                touched.push((sid, addr, Some(old)));
            }
        }
        touched
    }
}

fn ranges_overlap(a: RangeAddr, b: RangeAddr) -> bool {
    a.start.row <= b.end.row
        && b.start.row <= a.end.row
        && a.start.col <= b.end.col
        && b.start.col <= a.end.col
}

/// How many copies of `src` fit in `dst`, when `dst` is an exact multiple.
fn tiling(src: RangeAddr, dst: RangeAddr) -> (u32, u32) {
    let rows = if dst.rows() >= src.rows() && dst.rows().is_multiple_of(src.rows()) {
        dst.rows() / src.rows()
    } else {
        1
    };
    let cols = if dst.cols() >= src.cols() && dst.cols().is_multiple_of(src.cols()) {
        dst.cols() / src.cols()
    } else {
        1
    };
    (rows, cols)
}

/// Positions to fill on one line, paired with their signed step distance
/// from the source block.
type FillTargets = Vec<(i64, CellAddr)>;

/// Fill positions on one line, split into those before and after the source
/// block.
fn fill_targets(
    src: RangeAddr,
    dst: RangeAddr,
    line: u32,
    down: bool,
) -> (FillTargets, FillTargets) {
    let mut before = Vec::new();
    let mut after = Vec::new();
    if down {
        for r in dst.start.row..=dst.end.row {
            if r >= src.start.row && r <= src.end.row {
                continue;
            }
            if r > src.end.row {
                after.push((((r - src.end.row) as i64), CellAddr::new(r, line)));
            } else {
                before.push((-((src.start.row - r) as i64), CellAddr::new(r, line)));
            }
        }
    } else {
        for c in dst.start.col..=dst.end.col {
            if c >= src.start.col && c <= src.end.col {
                continue;
            }
            if c > src.end.col {
                after.push((((c - src.end.col) as i64), CellAddr::new(line, c)));
            } else {
                before.push((-((src.start.col - c) as i64), CellAddr::new(line, c)));
            }
        }
    }
    (before, after)
}

/// The pattern a fill extends.
enum Series {
    /// Constant step between consecutive numeric seeds.
    Numeric { first: f64, step: f64 },
    /// Text with a trailing integer, e.g. "Item 7".
    TextCounter {
        prefix: String,
        first: i64,
        step: i64,
    },
    /// No detectable progression: repeat the seeds verbatim.
    Repeat,
}

impl Series {
    fn detect(seeds: &[Option<Cell>]) -> Series {
        let values: Vec<&Value> = seeds
            .iter()
            .filter_map(|c| c.as_ref())
            .filter(|c| !c.is_formula())
            .map(|c| c.value())
            .collect();
        if values.len() < 2 {
            // A single value is copied, not extrapolated (Excel needs two
            // cells to establish a step for plain numbers).
            if let Some(Value::Text(t)) = values.first() {
                if let Some((prefix, n)) = split_trailing_int(t) {
                    return Series::TextCounter {
                        prefix,
                        first: n,
                        step: 1,
                    };
                }
            }
            return Series::Repeat;
        }
        let nums: Option<Vec<f64>> = values
            .iter()
            .map(|v| match v {
                Value::Number(n) => Some(*n),
                _ => None,
            })
            .collect();
        if let Some(ns) = nums {
            let step = ns[1] - ns[0];
            let consistent = ns.windows(2).all(|w| (w[1] - w[0] - step).abs() < 1e-9);
            if consistent {
                return Series::Numeric { first: ns[0], step };
            }
            return Series::Repeat;
        }
        let texts: Option<Vec<(String, i64)>> = values
            .iter()
            .map(|v| match v {
                Value::Text(t) => split_trailing_int(t),
                _ => None,
            })
            .collect();
        if let Some(parts) = texts {
            if parts.windows(2).all(|w| w[0].0 == w[1].0) {
                let step = parts[1].1 - parts[0].1;
                let consistent = parts.windows(2).all(|w| w[1].1 - w[0].1 == step);
                if consistent {
                    return Series::TextCounter {
                        prefix: parts[0].0.clone(),
                        first: parts[0].1,
                        step,
                    };
                }
            }
        }
        Series::Repeat
    }

    /// The value at signed distance `step` from the source block, where `n`
    /// is the seed count. Seeds occupy ordinals 0..n-1, so the first cell
    /// after the block is ordinal n and the first cell before it is -1.
    fn extend(&self, seed: &Value, step: i64, n: i64) -> Value {
        let ordinal = if step > 0 { n + step - 1 } else { step };
        match self {
            Series::Repeat => seed.clone(),
            Series::Numeric { first, step: s } => Value::Number(first + s * ordinal as f64),
            Series::TextCounter {
                prefix,
                first,
                step: s,
            } => Value::Text(format!("{}{}", prefix, first + s * ordinal)),
        }
    }
}

/// Split "Item 7" into ("Item ", 7). None when there is no trailing integer.
fn split_trailing_int(s: &str) -> Option<(String, i64)> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = chars.len();
    while i > 0 && chars[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i == chars.len() {
        return None;
    }
    let prefix: String = chars[..i].iter().collect();
    let digits: String = chars[i..].iter().collect();
    digits.parse().ok().map(|n| (prefix, n))
}

/// Build the cell that lands at `dst` when `src` is pasted there.
fn transform_pasted(cell: &Cell, src: CellAddr, dst: CellAddr, mode: PasteMode, cut: bool) -> Cell {
    match (&cell.content, mode) {
        // Paste-values freezes the computed result.
        (_, PasteMode::Values) => Cell::literal(cell.value().clone()),
        (CellContent::Literal(v), _) => Cell::literal(v.clone()),
        (CellContent::Formula { ast, cached, .. }, _) => {
            // A cut moves formulas verbatim; a copy shifts relative refs.
            let new_ast = if cut {
                ast.clone()
            } else {
                refs::offset(
                    ast,
                    dst.row as i64 - src.row as i64,
                    dst.col as i64 - src.col as i64,
                )
            };
            Cell {
                content: CellContent::Formula {
                    src: new_ast.to_formula(),
                    ast: new_ast,
                    cached: cached.clone(),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_int_split() {
        assert_eq!(split_trailing_int("Item 7"), Some(("Item ".into(), 7)));
        assert_eq!(split_trailing_int("Q1"), Some(("Q".into(), 1)));
        assert_eq!(split_trailing_int("plain"), None);
        assert_eq!(split_trailing_int("12"), Some(("".into(), 12)));
    }

    #[test]
    fn tiling_multiples() {
        let src = RangeAddr::parse_a1("A1:A2").unwrap();
        assert_eq!(tiling(src, RangeAddr::parse_a1("B1:B6").unwrap()), (3, 1));
        // Not an exact multiple: paste once.
        assert_eq!(tiling(src, RangeAddr::parse_a1("B1:B5").unwrap()), (1, 1));
    }

    #[test]
    fn numeric_series_positions() {
        let s = Series::Numeric {
            first: 10.0,
            step: 5.0,
        };
        // Two seeds (10, 15); the next cell down is the third element.
        assert_eq!(s.extend(&Value::Number(0.0), 1, 2), Value::Number(20.0));
        assert_eq!(s.extend(&Value::Number(0.0), 2, 2), Value::Number(25.0));
        // Filling upward continues the series backwards.
        assert_eq!(s.extend(&Value::Number(0.0), -1, 2), Value::Number(5.0));
        assert_eq!(s.extend(&Value::Number(0.0), -2, 2), Value::Number(0.0));
    }
}
