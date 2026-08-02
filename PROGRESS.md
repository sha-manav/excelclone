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

## M3 — Wasm + Grid MVP (complete)
- [x] wasm-bindgen API over engine; npm package consumed by web app
- [x] Virtualized canvas grid: selection, editing, formula bar, sheet tabs
- [x] Live recalc; 60fps scroll on 50k-cell fixture

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

## M5 — Grid completeness (complete)
- [x] Per-cell format model: bold, italic, font/fill colour, borders, number
      format, alignment — interned beside the cells, not inside them, so a
      format can exist without a value
- [x] Formatting travels with contents through paste, cut, fill, sort and
      insert/delete; Delete clears contents and leaves formatting standing
- [x] xlsx round trip: styles parsed on import, original `s` indices written
      back for untouched cells, new `<xf>` records appended for changed ones
- [x] Find & replace as one engine action (one undo step, one mined gesture),
      matching formula-bar text rather than computed results
- [x] Formatting toolbar, right-click menu, multi-key sort dialog, checkbox
      filter menu, find panel, import-notes drawer, Open / Save xlsx / Save csv
- [x] Merged ranges painted as one block; clicking a covered cell selects the
      block; drag-selection expands over merges
- [x] Fill-handle double-click follows the neighbouring run; column autofit
- [x] Drag autoscroll past the viewport edge
- [x] Playwright happy-path suite (17 new tests, canvas pixels *and* engine
      state)

## M6 — Miner + routines (complete)
- [x] Normalization to abstract tokens (R1C1 formula shapes, no literals)
- [x] Tandem-repeat loop detection, scoped to a single session
- [x] PrefixSpan (min support 3, max length 12, gap tolerance 1)
- [x] Scoring in estimated minutes saved, discarding under 2
- [x] Routine synthesis into a macro of typed engine `Action`s, with
      redacted values reported as requirements rather than guessed
- [x] Dry-run sandbox: clone the engine, apply, diff — including downstream
      recalculation
- [x] `gridline-miner mine --in events.jsonl`
- [x] Planted-pattern acceptance tests, positive and negative
- [x] `gridline-miner mine --db <path>` upserts into the server's `routines`
      table, per (actor, workbook), preserving any verdict the user has given
      and pruning proposals the log no longer supports
- [x] `Routine` and the dry-run sandbox moved into the engine, so the miner,
      the server and the client share one definition
- [x] Routines panel: ranked proposals, a live diff against the current
      selection, Run and Dismiss, partial routines naming what they cannot fill
- [x] End-to-end routine flow in the browser

## M7 — Dataset export + polish
- [ ] miner export JSONL (consent-enforced)
- [ ] Demo workbook + scripted data; docs complete; README

## Notes

- 341 Rust tests, 140 web unit tests, 44 Playwright end-to-end tests; full
  CI gate (fmt, clippy -D warnings, tests, wasm build, vite build, e2e)
  passes locally.
- M5 found a bug that had been latent since M2: **xlsx export had never
  worked in the browser**. `rust_xlsxwriter` stamps every workbook with the
  current time, and `SystemTime::now()` traps on `wasm32-unknown-unknown`,
  so each export panicked with `RuntimeError: unreachable`. Nothing had
  called export from the UI until this milestone, and no native test can
  reach the wasm target — the guard is now an end-to-end test that saves a
  workbook in a real browser and opens it again.
- Two more real-browser finds in the same session: revoking the blob URL
  synchronously after clicking the download anchor cancelled the download
  before a byte was read, and renaming a sheet left the grid painting one
  frame against a name the engine no longer had, throwing inside a
  `requestAnimationFrame` callback where nothing could catch it. All three
  passed every unit test.
- A fourth, quieter one: import replays a file through `apply()`, so a
  freshly opened workbook arrived with one undo entry per imported cell and
  the first Ctrl+Z un-typed a cell the user never typed.
- M6's planted-pattern tests earned their keep the same way. Three bugs the
  unit tests were happy with: the unparseable-formula fallback leaked sheet
  names and string literals into tokens (and whole-column references, which
  v1 does not parse, are the common case); tandem repeats ran across session
  boundaries, so three sittings became one "loop repeated 3 times in a row";
  and review cost was charged per repetition, which made a genuine twelve-row
  habit score below the threshold and vanish.
- Driving the routines panel in the browser found two more. `applyBatch`
  pushed one undo entry per action, so the panel's own promise — "runs in one
  undo step" — was false and rejecting a five-step routine took five Ctrl+Z;
  nothing had used `applyBatch` until routines did. And the dry run compared
  cell values only, so a routine that bolds a header row previewed as
  "nothing would change" and Run was disabled on a routine that worked.
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
