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

## P1 — Closing the measured differences (complete)
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

## P2 — Function coverage (complete)
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

## P3-P6 — The model changes
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

- 472 Rust tests, 167 web unit tests, 55 Playwright end-to-end tests; full
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

## E — Agent-training environment (in progress)

### E1 — Deterministic environment core (complete)
- [x] `crates/env`, depending on `engine` and depended on by nothing —
      training-loop concerns stay out of the spreadsheet library
- [x] Four methods: `reset(snapshot_id)`, `observe()`, `step(Action)`,
      `grade(TaskSpec)`, plus `checkpoint()` and `diff_from_start()`
- [x] Content-addressed snapshot store: SHA-256 over canonically sorted
      JSON, in memory or backed by a directory, deduplicating by id
- [x] `Sheet::cells` and `Sheet::formats` serialize keyed by A1, which is
      what made a `Workbook` expressible as JSON at all
- [x] `WorkbookObservation`: sheet extents, detected tables with headers and
      column types, formulas grouped by R1C1 shape, dependency summary,
      selection, visible errors, recent changes — each list capped, each cap
      reported alongside the real total
- [x] `StepResult` with the per-step state hash, the events, the cells
      touched including those that only recalculated, and whether the step
      budget is spent
- [x] `gridline-env put | show | grade`, building starting workbooks by
      replaying an action log

### E2 — Task specs and graders (complete)
- [x] Ten deterministic checks: exact display, number within tolerance,
      formula shape, range filled, sums match (debits equal credits), sum
      equals, no errors, forbidden ranges unchanged, sheets exist, defined
      name refers to
- [x] Shape comparison, so "fill this down" is one check and neither pasted
      literals nor an unshifted formula passes it
- [x] `incidental_changes` on every grade: the number that separates "did the
      task" from "did the task and nothing else"
- [x] Every failure names what was actually there
- [x] 47 tests, adversarial rather than confirmatory — two of them pin bugs
      the writing found: the `#REF!` collapse that made `CellFormula` accept
      any up-and-left formula, and the sheet-name mismatch that counted
      honest work as collateral damage

### C1–C4 — Corrections, evaluation and promotion (complete)
- [x] `Correction`: the instruction, the initial snapshot, the failed
      trajectory, the user's repair, the corrected state, and the exact
      divergence — computed from state hashes rather than from actions
- [x] Clean corrections distil into supervised examples; failed-versus-
      corrected pairs into preference data anchored at the diverging state,
      each carrying the grader's reason
- [x] `gridline-env distil`, reporting what it could not use
- [x] A content-versioned evaluation corpus read from immutable snapshots,
      scored on completion (overall and per origin), required outputs,
      invariants, forbidden-cell modifications, incidental cells, replans,
      refusals, router split and wall clock
- [x] A promotion rule that holds on any of several conditions — including a
      task that used to pass and now fails — and never on average reward
- [x] `gridline-agent evaluate` and `promote`; `make evaluate` runs both
- [x] `run_from` resumes at a checkpoint and reaches the same final workbook
      as an uninterrupted run, keeping the task's step budget
- [x] 671 Rust tests. Two things left deliberately unbuilt and documented in
      `docs/LOOP.md`: nothing in the product fires a correction capture, and
      nothing here trains anything

### A1–A5 — The hierarchical agent (complete)
- [x] `crates/agent`: a typed plan vocabulary with no code, no coordinates
      and no addresses — `LocateTable`, `CreateDerivedColumn`,
      `ApplyFormula`, `FillRange`, `FilterRows`, `ReconcileTotals`,
      `ExportWorkbook`
- [x] A compiler resolving each step against the workbook in front of it, by
      header, defined name, formula pattern and column type
- [x] An `ActionValidator` refusing writes outside the declared scope,
      unrequested structural changes, unresolvable references, literals
      replacing formulas, and changes over a configured blast radius
- [x] The loop: observe, propose, validate, rehearse on a clone, commit —
      with a checkpoint after every committed step, and refusals fed back to
      the planner in words it can act on
- [x] A `Planner` trait, a deterministic reference planner, and a `Router`
      that escalates on the first refusal
- [x] Successful plans persisted and clustered by shape into parameterized
      micro-policies, with a `MemoPlanner` that answers from them
- [x] `gridline-agent solve --memory plans.jsonl`; over the generated corpus
      the second pass scores the same 7/11 with zero planner calls and zero
      replans
- [x] 88 tests, including the control that a fixed-address macro fails every
      variant the agent solves. Two corpus bugs found by running the agent
      against it — see `docs/AGENT.md` and `DECISIONS.md`

### E3 — Trajectories and the JSONL dataset (complete)
- [x] `Trajectory`: instruction, initial snapshot by hash, ordered
      observations and actions, per-step state hashes, final workbook diff,
      termination reason and grader output
- [x] `Recorder` wraps the environment so no step can be taken unrecorded
- [x] `replay()` — the validation gate: every recorded state hash has to be
      reproduced, per step, and a refusal has to replay as a refusal
- [x] JSONL, one object per line, snapshots stored beside it by hash

### E4 — Augmentation with replay validation (complete)
- [x] Four perturbations — insert rows/columns (moving the table, or dropping
      an irrelevant column into it), rename a sheet, append rows of data,
      scale the numeric inputs — composable into named recipes
- [x] Each expressed as engine actions plus an address remap, so references
      are rewritten by the same code copy, paste and fill go through
- [x] The gate: replay the demonstration against the variant, grade it, keep
      it only if it passes; rejections reported with the grader's own reason
- [x] `gridline-env put | show | grade | record | augment | validate`
- [x] `corpus/env` + `scripts/dataset.sh` produce `dataset/`: 2 human
      demonstrations, 11 validated variants, 24 snapshots. Three recipes are
      rejected, correctly — see the recorded limitation in
      `docs/ENVIRONMENT.md`
- [x] `make ci` re-validates the committed dataset, so an engine change that
      invalidates it fails the build rather than going unnoticed

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
- A formula using a defined name is volatile, so it recalculates on every
  pass: the dependency graph is built from the references *written* in a
  formula, and a name is not one. Resolving names while building the graph is
  the fix; until then a workbook that leans on names recalculates fully.
- Every structural operation triggers a full dependency rebuild and
  recalculation. Correct but O(all formulas).
- **A conditional-formatting rule a file arrived with is preserved but not
  displayed.** Gridline's own rules work end to end — they colour the grid,
  follow the values, undo, and reach the file. Reading someone else's means
  parsing arbitrary `<dxf>` records as faithfully as `cellXfs`, and a
  half-read rule paints the wrong thing rather than nothing. The elements stay
  in the preserved bytes and come back untouched.
- **Only "greater than" has a control.** The engine takes comparisons, text,
  blank, duplicate and formula rules; the toolbar offers the one people reach
  for first. The rest need a rule editor.
- **Charts are preserved but cannot be authored.** A model, an authoring
  surface and a renderer, and the least like the rest of the codebase.
- Freezing more rows than fit in the window is allowed. The engine cannot see
  the viewport, and the button freezes above the cursor; putting the cursor
  near the bottom of a long sheet and pressing it leaves almost nothing to
  scroll. Excel refuses; this does not.
- **Pointing works with the mouse but not the arrow keys.** Clicking or
  dragging on the grid while a formula expects an operand writes the reference
  into it. Arrowing does not: in Excel the arrows pick the operand once a
  formula is mid-expression, and here they still move the caret. Building it
  means tracking a pointing cursor that is not the selection.
- **Only whole cells and ranges can be pointed at.** Clicking a column header
  mid-formula commits rather than writing `A:A`, because whole-column
  references are a reference shape the pointing code does not construct.

Two entries that stood here for several milestones were struck after checking
them rather than after fixing them: the grid *does* paint merged ranges (M5)
and drag-selection *does* auto-scroll past the viewport edge. A stale gap list
is worse than no gap list, because it is read as current.

## Resume point

M0-M7, the parity track P0-P7, the environment track E1-E4, the agent track
A1-A5 and the improvement loop C1-C4 are complete and green: `make ci` passes (fmt, clippy -D
warnings, 671 Rust tests, the parity report check, the dataset replay check,
`tsc -b`, the vite build, 203 web unit tests and 86 Playwright end-to-end
tests). `./scripts/demo.sh` runs the whole seeded scenario end to end and
`./scripts/dataset.sh` regenerates the training dataset.

Editing and selection were rebuilt to Excel's rules after the grid was
reported as sticky: a press on the grid commits the edit before the selection
moves, losing focus commits too (the formula bar excepted), and the arrow keys
commit-and-move when the edit started by typing while staying on the caret
when it started with F2 or a double-click. `apps/web/e2e/editing.spec.ts`
holds all eleven gestures; five of them failed before the change.

Formula authoring followed, for the same reason: writing one meant knowing
every function name by heart and typing every reference by hand. Pointing
turns a click or a drag on the grid into a reference when the formula is
mid-expression and leaves a finished one alone (`e2e/pointing.spec.ts`), and
the completion menu offers the engine's own function list with signatures
(`e2e/completion.spec.ts`). Both surfaces — the cell editor and the formula
bar — do both. The `###########` in the same report was a third bug: General
format is "as much precision as the column holds", and the renderer was
hashing anything the engine printed too wide instead of dropping decimals.

Parity stands at **99.1%** cell match over 320 settled cases, **100%**
function coverage of the tier-1 and tier-2 target list, and 100% round-trip
fidelity. Three differences are recorded rather than fixed and four questions
are open; all seven are in `PARITY.md` with what Gridline currently answers.

The environment resets, observes, steps and grades deterministically;
episodes are recorded as replayable trajectories; one validated demonstration
multiplies into variants that are each replayed and graded before being kept;
an agent plans over headers rather than addresses, is refused before it can
damage anything, and distils its own successes into policies that make the
next pass cheaper; corrections turn into supervised examples and preference
pairs; and a candidate is scored on eight dimensions and refused promotion on
any one of them. `docs/ENVIRONMENT.md`, `docs/AGENT.md` and `docs/LOOP.md`
describe all three, including what they deliberately do not do.

Next, in the order they are worth doing:

1. **A model planner behind the existing interface.** The routing, the
   confidence contract, the corrections format and the evaluation are built;
   what plugs into them is not. The two-step tasks the rule planner cannot
   phrase are the measurable gap it would close, and `make evaluate` is
   already the instrument that would show it closing.
2. **The correction capture triggers.** The record format and the
   distillation are done and tested end to end against the real agent; what
   is missing is the UI noticing that a user undid the agent, edited its
   preview, or repaired its output. Without them the loop has no input.
3. **Read the conditional-formatting rules a file arrives with**, which needs
   a `<dxf>` parser with the fidelity `cellXfs` already has, and a rule
   editor for the kinds the toolbar does not offer.
4. **Resolve defined names while building the dependency graph**, which takes
   the volatility cost off every formula that uses one.
5. **Incremental structural recalculation**, the other standing O(all
   formulas) cost.
6. **Charts.** A model, an authoring surface and a renderer; the biggest of
   the six and the least like the rest of the codebase.
