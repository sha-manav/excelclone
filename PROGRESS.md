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
- [x] `gridline-miner export --consented-only --mode structural --out data/`
      writing `{pre_state_digest, context, action, post_state_digest}`, one
      JSONL file per session plus a manifest
- [x] Consent enforced by the query that reads the events: no consent row, a
      latest consent of `off`, or a revocation contributes nothing
- [x] Subprocess tests driving the built binary against a real database
- [x] Sheet add/rename/delete on an imported workbook: `xl/workbook.xml`, its
      relationships and `[Content_Types].xml` are spliced, never regenerated,
      and a package with no `xl/styles.xml` is given the default one at import
      so formatting always has somewhere to go
- [x] Seeded demo: `scripts/demo.sh` / `make demo` builds, seeds a database
      with scripted history (including an actor who declined), writes the
      worked workbook, mines it, exports the dataset and checks that the
      refusal held
- [x] Walked the whole demo in a real browser against the real server and the
      seeded database: open, routines panel, preview, run, one-undo, add and
      rename a sheet, save, reopen, transparency page. Three real bugs fixed
      (see DECISIONS.md)
- [x] `docs/DATASET.md`; README quick start and demo section; PRIVACY.md,
      EVENTS.md, the consent notice and the transparency page all corrected to
      say that a formula's text literals and sheet names are recorded in clear

## P0 — Parity harness (complete)
- [x] `crates/parity`: corpus loader, runner, scorer, `PARITY.md` generator
- [x] 126 cases across operators, aggregation, lookup, text, criteria, dates,
      errors and number handling, each citing where its expectation comes from
- [x] Round-trip fidelity measured part by part, not just "it opened"
- [x] `engine::functions::IMPLEMENTED`, with a test comparing it against the
      dispatcher so the coverage table cannot drift
- [x] `make parity`; CI regenerates the report and fails on a diff
- [x] The two Excel semantics pinned in M2 recorded as open questions with
      every candidate answer, excluded from the score
- [x] Five differences found and recorded; one of them — arithmetic overflow
      returning IEEE infinity instead of `#NUM!` — fixed on the spot, because
      `inf` is not a spreadsheet value and would have reached the dataset

## P1 — Closing the measured differences (in progress)
- [x] Number-format sections (`0;(0)`, literal and empty sections, the text
      section), with the stale test that pinned their absence corrected
- [x] Serial 60 reports Excel's 1900-02-29 through YEAR/MONTH/DAY, and
      `DATE(1900,2,29)` returns it; month arithmetic still refuses a date the
      calendar does not have
- [x] The scientific-notation case withdrawn as not well-founded — Excel's
      choice is column-width dependent and `display()` has no width
- [x] 17 cases for the functions no case pinned; 93.9% of implemented
      functions are now pinned, up from 71.2%
- [ ] Excel's 15-significant-digit final rounding (2 recorded differences)
- [ ] Excel's calendar for rollovers landing on the phantom day

## P2 — Function coverage (in progress)
- [x] 26 tier-2 functions: CEILING, FLOOR, MROUND, TRUNC, SIGN, EXP, LN, LOG,
      LOG10, GCD, LCM, MEDIAN, LARGE, SMALL, RANK, ISNA, ISERR, ISLOGICAL,
      IFNA, NA, TYPE, REPT, EXACT, CHAR, CODE, CLEAN
- [x] 52 cases pinning them, including the sign rules Microsoft's own page
      spends a paragraph on and the difference between competition and dense
      ranking
- [x] Coverage 48.5% → 91.9%; cell match 98.5%; 96.8% of implemented
      functions pinned
- [x] The reference family (ROW, COLUMN, ROWS, COLUMNS) — `EvalCtx` now
      carries the cell being evaluated, which nothing else had needed
- [x] SUMPRODUCT over plain ranges; a computed array argument
      (`(A1:A3>2)*1`) is a recorded difference, because element-wise operators
      are the same model change dynamic arrays need
- [x] TIME, HOUR, MINUTE, SECOND, DATEVALUE, EDATE, DAYS
- [x] The financial block (PMT, FV, PV, NPER, RATE, NPV, IRR), with the
      closed forms computed independently from the documented equation and
      the two iterative answers verified by substitution
- [x] MODE, STDEV, SUBTOTAL, XMATCH, LOOKUP, TEXTBEFORE, TEXTAFTER,
      NUMBERVALUE, ISREF
- [x] `COUNTUNIQUE` removed from the target list: it is a Google Sheets
      function, and a target naming functions Excel does not have makes the
      score unreachable for a reason that is nobody's fault
- [ ] `OFFSET`, `INDIRECT` and `TRANSPOSE` need functions that return
      *references*; today a function returns a `Value`, so `SUM(OFFSET(...))`
      could not work and a scalar-only version would fail on the common use
- [x] `TIMEVALUE`, `NETWORKDAYS`, `WORKDAY`, `YEARFRAC` (five day-count
      bases; actual/actual recorded as an open question, because it is the one
      convention whose exact definition is genuinely disputed)
- [ ] `AGGREGATE` and `LET`
- [ ] Dynamic arrays (UNIQUE, SORT, FILTER, SEQUENCE, TEXTSPLIT) need spilling
      first, which is a model change rather than a function

## P3 — The model changes (in progress)
- [x] Functions can return a reference: `functions::call_operand` is tried
      before the value path, so `SUM(OFFSET(A1,0,0,3,1))` works rather than
      only `OFFSET(A1,1,1)`
- [x] `OFFSET` and `INDIRECT`, volatile because the dependency graph is built
      from the references *written* in a formula and neither says where it
      points until it runs
- [x] The stale-read hazard is fixed, not just documented: a computed
      reference could be evaluated before the value it actually reads, so the
      engine now iterates (bounded) when any volatile cell computes its own
      references. Clock and random functions read nothing and cost no extra
      passes
- [x] Column widths and row heights live in the engine, not in React state:
      `Action::Resize` records them, undo takes them back, insert and delete
      shift them, and they are written to and read from `<cols>` and
      `<row ht=…>`. Row borders are draggable too, which they never were
- [x] The system clipboard: copy writes TSV and an HTML table, paste reads
      either, and text from outside lands as one batched, single-undo edit.
      An internal paste still carries formulas, decided by comparing the
      clipboard text with what we last put there
- [x] Dynamic arrays: operators are element-wise over anything wider than a
      cell, UNIQUE / SORT / SORTBY / FILTER / SEQUENCE / TRANSPOSE / TEXTSPLIT
      return blocks, and blocks spill onto the grid with `#SPILL!` when
      something is in the way
- [x] AGGREGATE and LET, which finishes the tier-2 target list

## Notes

- 446 Rust tests, 157 web unit tests, 51 Playwright end-to-end tests; full
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

- Three measured parity differences are recorded rather than fixed, and are
  in `PARITY.md`: the 1900 leap-year bug in month arithmetic, array literals
  (`{"a","b"}`) which the parser does not read, and a scalar function handed
  a range spilling one result per element the way Microsoft 365 does. The
  15-significant-digit rule that made `=0.1+0.2=0.3` answer FALSE was closed
  in P4b.

- A habit written with *relative* references to a fixed table is invisible to
  the miner, because the R1C1 shape differs in every row. That is faithful —
  the formulas really do mean different things — but it means a sheet with
  that latent bug in it also gets no suggestions.

- `docProps/app.xml` still lists the sheet names the file arrived with. Excel
  rewrites it on save and no reader validates it against `workbook.xml`, so a
  stale copy is cosmetic — but it is stale.
- A `<definedName>` pointing at a deleted sheet is left in `xl/workbook.xml`
  rather than rewritten to `#REF!`.
- Every structural operation triggers a full dependency rebuild and
  recalculation. Correct but O(all formulas); revisit under P5 performance.
- The grid ignores merged ranges when painting (the engine models them and
  they survive round trips). Wire into the renderer in M5.
- Auto-scroll while drag-selecting past the viewport edge is not implemented.

## Resume point
M0-M4 complete and green. Next: M5 — grid completeness (fill handle polish,
context menus, formatting toolbar, sort/filter UI, find & replace, and the
import-warnings drawer).
