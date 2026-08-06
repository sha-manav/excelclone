# Gridline — handoff

Continue building **Gridline** per the original spec (the full brief is the
source of truth; this file only records where the work stopped and what to
watch out for). Repo: <https://github.com/sha-manav/excelclone>.

Work the milestones in order under the same autonomy rules: decide, record the
decision in `DECISIONS.md`, keep `PROGRESS.md` current, commit at least once
per milestone, and never leave the repo red.

---

## What is already done

**M0–M7 and P0–P2 are complete.** 412 Rust tests, 140 web unit tests
(vitest), 44 end-to-end tests (Playwright); `cargo clippy --workspace
--all-targets -- -D warnings` is clean and `PARITY.md` is regenerated and
checked in CI.

Current parity score: **98.5% cell match** (262 of 266 settled cases),
**91.9% function coverage** (124 of 135), **100% round-trip fidelity**, with
4 recorded differences and 4 open questions.

| Milestone | State |
| --- | --- |
| M0 Scaffold | Cargo workspace (`engine`, `gridline-wasm`, `server`, `miner`), Vite/React/TS app, GitHub Actions CI, `make dev` |
| M1 Engine core | Model, Pratt parser with Excel precedence, evaluator, dependency graph, incremental recalc, cycle detection |
| M2 Engine complete | Full v1 function set (~90), reference rewriting, paste/fill/insert/delete/sort/filter/merge, undo/redo, xlsx+csv I/O with part preservation, golden workbooks |
| M3 Wasm + grid | wasm bindings, virtualized canvas grid, formula bar, sheet tabs, verified in a real browser |
| M4 Event spine | Envelope + redaction in Rust, client capture pipeline, consent flow, capture chip, transparency page, axum server (ingest/consent/export/sessions), determinism-replay suite |
| M5 Grid completeness | Per-cell format model through the xlsx round trip, find & replace, formatting toolbar, context menu, sort/filter UI, merges painted, import-notes drawer, file open/save, autoscroll, autofit |
| M6 Miner + routines | Normalization, loop + PrefixSpan mining, scoring, routine synthesis, dry-run sandbox, CLI with `--db` write-back, Routines panel with live preview / Run / Dismiss, planted-pattern and browser acceptance suites |
| M7 Dataset + demo | `miner export --consented-only`, consent enforced by the query; sheet add/rename/delete on imported packages; `make demo` seeds a database, mines it, exports a dataset and checks the refusal held; the whole demo walked in a real browser |
| P0 Parity harness | `crates/parity`: corpus, runner, scorer, `PARITY.md` generated and CI-checked. Found five differences on its first pass |
| P1 Close differences | Number-format sections, Excel's serial 60, a withdrawn case that could not be well-founded |
| P2 Function coverage | 58 functions added across maths, statistics, errors, text, references, finance and dates; coverage 48.5% → 91.9% |

### Architecture facts worth knowing before changing anything

- **Every mutation goes through `Engine::apply(Action) -> Vec<Event>`.** The UI
  never touches workbook state, and neither will a routine — running one
  replays typed `Action`s through the same path. Do not add a side door. The
  one deliberate exception is documented: xlsx import writes `Sheet::formats`
  directly, because a file's existing formatting is initial state rather than
  something the user did in this session.
- **Redaction lives in Rust** (`crates/engine/src/telemetry.rs`) and the client
  calls it through wasm. Do not reimplement hashing in TypeScript.
- **The replay suite is the flagship test**
  (`crates/engine/tests/replay_determinism.rs`). If a change makes a workbook's
  values depend on *how* it was reached, that suite is supposed to fail. Do not
  weaken it to make something else pass.
- **Golden fixtures regenerate with `UPDATE_GOLDEN=1`.** Always read the diff
  before committing a regeneration. Twice now that diff has shown a new test
  quietly destroying older coverage — a `RangeClear` that erased the float
  formatting case, and a find/replace that broke the `IFERROR` case. Both were
  fixed by moving the new case, not by blessing the snapshot.
- **Formats live beside cells, not inside them** (`Sheet::formats`, interned in
  a workbook-level `FormatTable`). A format can exist with no value; that is
  deliberate and several tests depend on it.
- **`Routine` and its dry-run sandbox live in the engine**
  (`crates/engine/src/routine.rs`), because the miner discovers routines, the
  server stores them and the client runs them. The miner keeps only the
  discovery half. Running a routine hands its shifted actions back to the
  client, which pushes them through the same `applyBatch` as any other
  gesture — there is no second execution path.
- **`Engine::apply_batch` coalesces the undo entries** its actions push into
  one, so a five-step routine is one Ctrl+Z. Applying in a loop does not.
- **Number display caps at 15 significant digits**, matching Excel, so
  `=0.1*3` renders `0.3`.
- Serials reproduce Excel's phantom 1900-02-29 so dates round-trip with real
  workbooks; serial 60 itself is `#NUM!`.
- **`window.__gridline__.stateSnapshot()`** exists in dev builds only, for the
  end-to-end suite. It is read-only by design.

### Running it

```sh
make demo     # seed a database, mine it, export a dataset, check the refusal
make parity   # measure Excel parity and regenerate PARITY.md
```


```sh
make dev     # builds wasm on first run, seeds an admin token, starts API + UI
make ci      # fmt, clippy -D warnings, cargo test, vite build
cd apps/web && npx vitest run      # unit tests
cd apps/web && npx playwright test # end-to-end
cargo run -p miner --bin gridline-miner -- mine --in events.jsonl
cargo run -p miner --bin gridline-miner -- mine --in events.jsonl --db gridline.db
```

**Two environment notes, both worked around rather than papered over:**

- `wasm-pack build` fails at the `wasm-opt` step when the sandbox cannot reach
  GitHub. Add `--no-opt` for local builds; the Makefile is left alone, since CI
  should optimize.
- Playwright's bundled Chromium build number may not match the one installed.
  Set `GRIDLINE_CHROMIUM=/opt/pw-browsers/chromium` to point at the binary that
  is already there; `playwright.config.ts` reads it.

---

## What is left

### The next thing is a model change, and it unlocks most of the rest

**Reference-returning functions.** `functions::call` returns a `Value`, so a
function can never hand back a range. That single fact is what blocks
`OFFSET`, `INDIRECT` and `TRANSPOSE`, and it is the same infrastructure
dynamic arrays need. The shape of the change:

- add `functions::call_operand(ctx, name, args) -> Option<Operand>`, returning
  `Some` only for the functions that produce references;
- in `EvalCtx::eval_operand`, try it first for `Expr::Func` and fall back to
  `Operand::Scalar(call(...))`.

That part is small — maybe thirty lines. **The dependency graph is the hard
part**: it is built from the AST's static references, so `OFFSET(A1,5,0)`
depends on a cell the graph cannot see, and `INDIRECT` is fully dynamic. Excel
solves this by making both volatile, and there is already a `volatile` set in
`Engine` used for `RAND`/`NOW`/`TODAY`. Volatility handles *recalculation* but
not *ordering*: a volatile cell can still read a formula cell that has not been
recomputed yet in the same pass. Decide that deliberately, and check it against
`replay_determinism` and the incremental-vs-full property test, both of which
should catch a stale read.

Do not ship a scalar-only `OFFSET`. It would handle `OFFSET(A1,1,1)`, fail on
`SUM(OFFSET(A1,0,0,3,1))` — the common use — and the parity report would count
it as implemented, which is worse than the gap.

**Then spilling**, which needs the same operand plumbing plus a model for a
cell owning a region: `UNIQUE`, `SORT`, `SORTBY`, `FILTER`, `SEQUENCE`,
`TEXTSPLIT`, `TRANSPOSE`, and the computed-array argument that `SUMPRODUCT`
currently refuses (`SUMPRODUCT((A1:A3>2)*1)` is a recorded difference).

### Smaller, self-contained work

- `AGGREGATE` (19 functions × 7 ignore-options) and `LET` (needs name binding
  in the evaluator).
- Excel's **15-significant-digit final rounding** — two recorded differences.
  The mechanism appears to be the subtraction inside a comparison rather than a
  per-cell rounding, which is why it is recorded rather than guessed at; get
  that wrong and you trade one wrong answer for a subtler one.
- **P3–P5** of the parity track as the original spec defines them.

## Known gaps carried forward

1. **A function cannot return a reference**, which blocks `OFFSET`,
   `INDIRECT`, `TRANSPOSE` and dynamic arrays. See "What is left" above; this
   is the one item worth doing before anything else on the parity track.
2. **`docProps/app.xml` still lists the sheet names an imported file arrived
   with**, and a `<definedName>` pointing at a deleted sheet is left alone
   rather than rewritten to `#REF!`. Excel rewrites app.xml on save and no
   reader validates it, so both are cosmetic — but they are stale.
3. **A habit written with *relative* references to a fixed table is invisible
   to the miner**, because the R1C1 shape differs in every row. That is
   faithful — the formulas really do mean different things — but a sheet with
   that latent bug in it also gets no suggestions.
4. **Four measured parity differences are recorded rather than fixed**, and
   are listed in `PARITY.md` with what Gridline answers: Excel's
   15-significant-digit final rounding (two cases), `EOMONTH` from the phantom
   1900-02-29, and `SUMPRODUCT` over a computed array.
5. **Every structural operation triggers a full dependency rebuild and
   recalculation.** Correct but O(all formulas). Fine now; revisit under P5.
6. **No keyboard navigation *into* a merged range.** Merges are painted, and
   click and drag selection handle them, but arrow keys still walk the covered
   cells.
7. **The import-notes drawer has no end-to-end test with notes in it** — only
   the negative case (a clean file shows no badge). Producing a warning needs a
   fixture using a feature we do not model, and fixtures are generated rather
   than committed. The drawer is presentational; the warnings themselves are
   covered in Rust.
8. **Two Excel semantics are deliberately unresolved**, pinned for the P0
   oracle rather than guessed: whether `COUNTIF` should propagate errors found
   in its range (we propagate; Excel may ignore), and whether approximate
   lookup should replicate Excel's binary-search behaviour on unsorted data
   (we do a linear "last entry ≤ lookup" scan).
9. **`_xlfn.` handling covers the functions we implement**; new post-2007
   functions added later must be checked against the same prefix rule.
10. **CSV export dumps the used range** without padding to A1 and includes
   filter-hidden rows. It also ignores number formats, where Excel exports the
   formatted text.
11. **Case-insensitive find/replace is ASCII-only.** Full Unicode folding
   changes byte lengths and would corrupt the text around a match; "Match case"
   is exact for any script.
12. **Formatting is per cell, capped at 200k cells per action.** There are no
   row or column format defaults, which is what xlsx uses for "bold this whole
   column". Raising the cap properly means adding them.
13. **The miner cannot rebuild a paste, a filter or a replacement** from the
    log: the log records their shape but not the clipboard, the allowed values
    or the search terms. Those steps are skipped during synthesis, so a routine
    mined from a gesture containing them is shorter than the gesture was. If
    this matters, the fix is in what the *capture* records, not in the miner.
14. **Nothing runs the miner automatically.** `gridline-miner mine --db …` has
    to be invoked by hand or by cron; there is no scheduler and `make dev` does
    not wire one up. The M7 demo script will need a step that runs it.
15. **A partial routine runs its known steps and leaves the rest to the user.**
    The panel names the cells it cannot fill, but there is no flow for typing
    them into the routine — you run it and then fill them in yourself.

---

## Lessons from the bugs found so far

Every real bug in this project so far has been found by a test written to be
adversarial rather than confirmatory, and almost all of them passed the unit
tests first. Keep that stance.

- Property tests caught a **determinism bug**: cells that only *syntactically*
  referenced a cycle (an untaken `IF` branch) evaluated differently under full
  vs incremental recalculation.
- The golden round-trip caught **`_xlfn.` prefixes** making every modern
  real-world workbook import as `#NAME?`.
- Driving the real browser caught a **stale `requestAnimationFrame` handle**
  (grid never painted in dev), **`apply()` called inside a `setState` updater**
  (every edit applied twice), a **blob URL revoked synchronously** after the
  download click (so Save silently did nothing), and a **stale sheet name**
  painting one frame after a rename and throwing where nothing could catch it.
- Running the real client against the real server caught an **`actor_id`
  mismatch** (every event rejected) and **`context.sheet` transmitted in
  clear** while payload sheet names were hashed.
- M5's end-to-end suite caught that **xlsx export had never worked in the
  browser** — `SystemTime::now()` traps on wasm32 — a bug latent since M2
  because nothing in the UI called export until then. No native test could have
  found it.
- M6's planted-pattern tests caught a **privacy leak in a fallback path** (the
  unparseable-formula branch passed raw text through, and whole-column
  references make that branch the common one), a **loop miner that crossed
  session boundaries** and so claimed three sittings were one loop, and a
  **scoring model that charged the review cost per repetition**, hiding genuine
  habits below the threshold.
- The routines panel's browser suite caught that **`applyBatch` pushed one undo
  entry per action**, so the panel's own promise — "runs in one undo step" —
  was false, and rejecting a five-step routine took five Ctrl+Z. Latent because
  nothing had used `applyBatch` until routines did.

- **P0's harness found five differences on its first serious pass**, none of
  which any existing test had — including arithmetic overflow returning the
  IEEE infinity instead of `#NUM!` from every operator except `^`. `inf` would
  have travelled from the grid into the state snapshot, the event log and the
  exported dataset. That is the argument for building the measurement before
  the features.
- **P2's date block reproduced a lesson from earlier the same day.** XMATCH's
  wildcard mode was made to share COUNTIF's matcher, because two
  implementations of `*` and `?` would eventually disagree. An hour later
  `NETWORKDAYS` counted weekends with its own `serial mod 7` — a day out,
  because Excel's serial line contains a phantom 1900-02-29 — and answered
  Sunday for "one working day after Friday". The harness caught it. Ask the
  existing code; do not write the second implementation.
- **Walking the demo in a browser found three bugs no test covered**:
  `VITE_DEV_TOKEN` had never been read by anything despite `dev.sh` claiming
  otherwise since M4, so every `make dev` session 401'd; the workbook id is
  client-minted, so seeded routines could never appear in the panel; and the
  preview told users to type four values that were already on screen.
- **Writing a document against real output found a documentation bug.**
  `docs/DATASET.md` was checked against an actual exported record, which showed
  `Rates!$A$1:$B$3` in clear beside a hashed `context.sheet` — `structural`
  hashes sheet names everywhere *except* inside formulas, which the privacy
  docs implied it did not. The behaviour was right; the description was not.

Two habits worth keeping specifically:

- **When a test passes on the first run, try to make it fail.** Several tests
  here were verified by temporarily breaking the code they cover; two turned
  out to be asserting nothing — an autofit check that passed without autofit
  implemented, and a tautological `!x || x`.
- **When a suite or a subagent reports "all green", check the claim against the
  running system.** The cautionary example is a `tsc --noEmit` that passed
  cleanly against an empty `node_modules`.
- **A test can encode a limitation as though it were behaviour.** Implementing
  number-format sections broke a test asserting that `0.00;(0.00)` formatted
  `5` as `5` — the fallback written down as an expectation. Two others named a
  function as their stand-in for "not implemented" and broke when it was
  implemented, failing for a reason unrelated to what they tested.
- **A case that cannot be well-founded should be withdrawn, not kept because it
  is already written.** One parity case asserted Excel shows `1E+20` for a
  large number; Excel's General format switches to scientific when the number
  does not fit *the column*, and `Value::display()` has no width. It was
  manufacturing a difference out of a question the corpus cannot ask.
