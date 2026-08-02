# Gridline Progress

Living checklist. Updated every session.

## M0 — Scaffold
- [x] Cargo workspace with `engine`, `wasm` (gridline-wasm), `server`, `miner`
- [x] Vite + React + TS app in `apps/web` renders "hello grid"
- [x] Rust toolchain installed (rustc 1.97.1 stable, wasm32-unknown-unknown target)
- [x] CI pipeline (`.github/workflows/ci.yml`): fmt, clippy -D warnings, test, wasm build, vite build
- [x] Makefile (`make dev`, `make ci`), scripts/dev.sh
- [x] PROGRESS.md, DECISIONS.md, docs/ARCHITECTURE.md
- [x] First commit

## M1 — Engine core (complete)
- [x] Cell/sheet/workbook data model (sparse store, merged-range map)
- [x] Formula parser (Pratt, Excel precedence, A1 refs, ranges, cross-sheet)
- [x] Evaluator with error semantics + empty-cell coercion
- [x] Dependency graph, incremental recalc, cycle detection (#CIRC!)
- [x] ~25 math + logic functions with Excel-verified tests

## M2 — Engine complete (complete)
- [x] Full v1 function list — math, logic, lookup, conditional aggregation,
      text (+ a common-codes number-format engine), date/time
- [x] Reference rewriting: offset (copy/fill), structural (insert/delete),
      moved (cut/paste), each with Excel semantics and unit tests
- [x] Copy/cut/paste (formulas, values, tiling), fill with series detection
- [x] Insert/delete rows and columns with workbook-wide ref remapping
- [x] Sort (multi-key, blanks last), value filters, merge/unmerge
- [x] Undo/redo through the same apply() path, recorded state not inverse actions
- [x] Property tests: undo identity, insert/delete inverses, replay determinism,
      incremental vs full recalc agreement (500 cases each)
- [x] xlsx import (calamine) and export, CSV import/export
- [x] Preservation rule: original zip parts retained verbatim, only
      `<sheetData>`/`<mergeCells>` patched, per-cell style indices kept
- [x] Import warnings for charts, pivots, VBA, conditional formatting,
      data validation, tables, comments, external links
- [x] Golden workbook tests: fixtures regenerate, import, recalc, and match
      stored snapshots; two-pass round trip is lossless

## M3 — Wasm + Grid MVP
- [ ] wasm-bindgen API over engine; npm package consumed by web app
- [ ] Virtualized canvas grid: selection, editing, formula bar, sheet tabs
- [ ] Live recalc; 60fps scroll on 50k-cell fixture

## M4 — Event spine
- [ ] Action→Event pipeline; client capture (batch, offline queue)
- [ ] Consent modal, privacy modes, capture status chip
- [ ] Server ingest (idempotent, consent-gated)
- [ ] Determinism/replay tests incl. proptest

## M5 — Grid completeness
- [ ] Fill handle, context menus, formatting toolbar, sort/filter UI
- [ ] Find & replace, import-warnings drawer, transparency page
- [ ] Playwright happy-path suite

## M6 — Miner + routines
- [ ] Normalization, loop detection, PrefixSpan, scoring
- [ ] Routine synthesis + storage; panel with dry-run diff + run
- [ ] Planted-pattern tests, E2E routine flow

## M7 — Dataset export + polish
- [ ] miner export JSONL (consent-enforced)
- [ ] Demo workbook + scripted data; docs complete; README

## Notes

- 149 engine tests green; full CI gate (fmt, clippy -D warnings, tests,
  wasm build, vite build) passes locally.
- The property suite caught a real determinism bug: cells that only
  *syntactically* referenced a cycle (an untaken `IF` branch) were marked
  `#CIRC!` by a full recalculation but evaluated correctly by an incremental
  one. Cycle membership is now decided by strongly connected component
  (iterative Tarjan), so both recalculation paths agree.
- The golden workbook round trip caught a second real bug: xlsx stores
  post-2007 functions as `_xlfn.TEXTJOIN` etc., so every modern real-world
  workbook would have imported as `#NAME?`. Prefixes are now stripped at
  parse time.

## Known gaps carried forward

- Adding a sheet to an imported workbook fails loudly on export (writing a new
  sheet part means rewriting `workbook.xml` and its relationships). Fix before
  the M7 demo, which imports a fixture and may add sheets.
- Every structural operation triggers a full dependency rebuild and
  recalculation. Correct but O(all formulas); revisit under P5 performance.

## Resume point
M0-M2 complete and green. Next: M3 — wasm-bindgen API over the engine, then
the virtualized canvas grid in `apps/web`.
