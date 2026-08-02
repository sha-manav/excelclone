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

## M1 — Engine core
- [ ] Cell/sheet/workbook data model (sparse store, merged-range map)
- [ ] Formula parser (Pratt, Excel precedence, A1 refs, ranges, cross-sheet)
- [ ] Evaluator with error semantics + empty-cell coercion
- [ ] Dependency graph, incremental recalc, cycle detection (#CIRC!)
- [ ] ~25 math + logic functions with Excel-verified tests

## M2 — Engine complete
- [ ] Full v1 function list (lookup, conditional agg, text, date/time)
- [ ] Copy/cut/paste/fill with ref rewriting; insert/delete rows/cols
- [ ] Sort/filter; undo/redo as inverse actions
- [ ] xlsx/csv import/export + preservation rule; golden workbook tests

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

## Resume point
M0 complete. Next: M1 engine core (start with `crates/engine/src/model.rs`).
