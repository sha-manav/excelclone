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

## M4 — Event spine (complete)
- [x] Event envelope and privacy redaction in the engine, so the client
      redacts through the same Rust the tests cover
- [x] Client capture: ring buffer, 5s/200-event batching, IndexedDB offline
      queue degrading to memory, nav.select sampling, never blocks the UI
- [x] Consent modal, privacy modes, capture chip, transparency page
- [x] axum server: idempotent + consent-gated ingest, admin-only JSONL export,
      server-derived sessionization, SHA-256-only tokens
- [x] Determinism/replay suite: fixture logs, resumable replay, deterministic
      event streams, full-recalc-after-every-action property
- [x] Verified end to end against the running server: consent gating,
      dedupe, hashed values and sheet names, formulas preserved

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

- 189 Rust tests, 120 web unit tests, 18 Playwright end-to-end tests; full
  CI gate (fmt, clippy -D warnings, tests, wasm build, vite build, e2e)
  passes locally.
- The property suite caught a real determinism bug: cells that only
  *syntactically* referenced a cycle (an untaken `IF` branch) were marked
  `#CIRC!` by a full recalculation but evaluated correctly by an incremental
  one. Cycle membership is now decided by strongly connected component
  (iterative Tarjan), so both recalculation paths agree.
- The golden workbook round trip caught a second real bug: xlsx stores
  post-2007 functions as `_xlfn.TEXTJOIN` etc., so every modern real-world
  workbook would have imported as `#NAME?`. Prefixes are now stripped at
  parse time.
- Driving the real browser caught two more bugs that no unit test would have:
  a stale `requestAnimationFrame` handle left by StrictMode's mount/unmount
  cycle meant the grid never painted at all in dev, and `commitEdit` called
  `apply()` inside a `setState` updater, so React's double-invocation applied
  every cell edit twice. The second would have duplicated every captured
  event in M4.
- Running the real client against the real server caught two more that both
  sides' own suites had passed: the client minted its own `actor_id` while
  the server authenticated a different one, so every event was rejected as a
  mismatched actor; and `context.sheet` was transmitted in clear while
  payload sheet names were hashed, leaking the name on every event and
  exposing a matched hash/plaintext pair for the workbook salt. Identity is
  now stamped at flush time (so events buffered before the server answers are
  not permanently mis-attributed) and context labels redact through the same
  Rust path.

## Known gaps carried forward

- Adding a sheet to an imported workbook fails loudly on export (writing a new
  sheet part means rewriting `workbook.xml` and its relationships). Fix before
  the M7 demo, which imports a fixture and may add sheets.
- Every structural operation triggers a full dependency rebuild and
  recalculation. Correct but O(all formulas); revisit under P5 performance.
- The grid ignores merged ranges when painting (the engine models them and
  they survive round trips). Wire into the renderer in M5.
- Auto-scroll while drag-selecting past the viewport edge is not implemented.

## Resume point
M0-M4 complete and green. Next: M5 — grid completeness (fill handle polish,
context menus, formatting toolbar, sort/filter UI, find & replace, and the
import-warnings drawer).
