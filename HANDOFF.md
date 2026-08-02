# Gridline — handoff

Continue building **Gridline** per the original spec (the full brief is the
source of truth; this file only records where the work stopped and what to
watch out for). Repo: <https://github.com/sha-manav/excelclone>.

Work the milestones in order under the same autonomy rules: decide, record the
decision in `DECISIONS.md`, keep `PROGRESS.md` current, commit at least once
per milestone, and never leave the repo red.

---

## What is already done

**M0–M4 are complete, committed, and green.** 189 Rust tests, 120 web unit
tests (vitest), 18 end-to-end tests (Playwright); `cargo clippy --workspace
--all-targets -- -D warnings` is clean.

| Milestone | State |
| --- | --- |
| M0 Scaffold | Cargo workspace (`engine`, `gridline-wasm`, `server`, `miner`), Vite/React/TS app, GitHub Actions CI, `make dev` |
| M1 Engine core | Model, Pratt parser with Excel precedence, evaluator, dependency graph, incremental recalc, cycle detection |
| M2 Engine complete | Full v1 function set (~90), reference rewriting, paste/fill/insert/delete/sort/filter/merge, undo/redo, xlsx+csv I/O with part preservation, golden workbooks |
| M3 Wasm + grid | wasm bindings, virtualized canvas grid, formula bar, sheet tabs, verified in a real browser |
| M4 Event spine | Envelope + redaction in Rust, client capture pipeline, consent flow, capture chip, transparency page, axum server (ingest/consent/export/sessions), determinism-replay suite |

### Architecture facts worth knowing before changing anything

- **Every mutation goes through `Engine::apply(Action) -> Vec<Event>`.** The UI
  never touches workbook state. This is what makes the event log trustworthy;
  do not add a side door.
- **Redaction lives in Rust** (`crates/engine/src/telemetry.rs`) and the client
  calls it through wasm (`describeAction`, `redactLabel`). Do not reimplement
  hashing in TypeScript — a second implementation is a second thing to get
  wrong, and it is the one that will leak.
- **The replay suite is the flagship test**
  (`crates/engine/tests/replay_determinism.rs`). If a change makes a workbook's
  values depend on *how* it was reached, that suite is supposed to fail. Do not
  weaken it to make something else pass.
- **Golden fixtures regenerate with `UPDATE_GOLDEN=1`.** Always read the diff
  before committing a regeneration; a silently re-blessed snapshot is exactly
  the failure these tests exist to catch.
- **Number display caps at 15 significant digits**, matching Excel, so
  `=0.1*3` renders `0.3`.
- Serials reproduce Excel's phantom 1900-02-29 so dates round-trip with real
  workbooks; serial 60 itself is `#NUM!`.

### Running it

```sh
make dev     # builds wasm on first run, seeds an admin token, starts API + UI
make ci      # fmt, clippy -D warnings, cargo test, vite build
cd apps/web && npx vitest run      # unit tests
cd apps/web && npx playwright test # end-to-end
```

Playwright uses the system Chrome locally (`channel: 'chrome'`) and downloads
its own Chromium in CI. If `npx playwright install chromium` stalls locally,
that is a known environment issue, not a project one.

---

## What is left

### M5 — Grid completeness (next)

- Fill handle polish, right-click context menus (insert/delete row/col, sort)
- Formatting toolbar: bold/italic, font and fill colour, borders
  (outline/all/none), number format (general, `#,##0.00`, %, date), alignment,
  merge/unmerge
- Sort and filter UI (the engine already supports both; `FilterSpec` is a
  checkbox-style allowed-value list)
- Find & replace
- Import-warnings drawer (the importer already returns structured warnings)
- Playwright happy-path suite green

Formatting needs a real decision: the engine has **no per-cell format model
yet**. Adding one touches `Cell`, the xlsx preservation path (which currently
carries the original `s` style index through untouched), and the
`format.apply` event payload. Design it before writing UI.

### M6 — Miner + routines

`crates/miner` is still a stub. Needs: event normalization to abstract tokens,
tandem-repeat loop detection, PrefixSpan mining (min support 3, max length 12,
gap tolerance 1), scoring by estimated minutes saved (discard < 2 min),
routine synthesis as JSON macros of typed engine `Action`s, dry-run sandbox
producing a diff, and the Routines panel with preview/run/dismiss. The server
already has a `routines` table and `GET /v1/routines` +
`POST /v1/routines/:id/feedback`.

### M7 — Dataset export + polish

`miner -- export --consented-only --mode structural --out data/` writing JSONL
`{pre_state_digest, context, action, post_state_digest}` grouped by session,
refusing any actor whose consent is `off` or revoked. Seeded demo workbook and
scripted demo data. Then verify the full §13 demo script end to end.

### Then the Parity Track (P0–P5)

Start with **P0**: build `crates/parity`, the differential harness, seed the
oracle corpus, and generate `PARITY.md` in CI. Do not add parity features
before the harness exists — the whole point is that parity is a measured
score, not a claim.

---

## Known gaps carried forward

1. **Adding, renaming or deleting a sheet in an *imported* workbook fails
   loudly on export** (`IoError::Unrepresentable`). Writing a new sheet part
   means rewriting `workbook.xml` and its relationships, which is not
   implemented. **This will break the M7 demo**, which imports a fixture — fix
   before the demo.
2. **Every structural operation triggers a full dependency rebuild and
   recalculation.** Correct but O(all formulas). Fine now; revisit under P5.
3. **The grid ignores merged ranges when painting.** The engine models them and
   they survive round-trips; only the renderer is missing. Natural M5 work.
4. **No auto-scroll while drag-selecting past the viewport edge.**
5. **Two Excel semantics are deliberately unresolved**, pinned for the P0
   oracle rather than guessed: whether `COUNTIF` should propagate errors found
   in its range (we propagate; Excel may ignore), and whether approximate
   lookup should replicate Excel's binary-search behaviour on unsorted data
   (we do a linear "last entry ≤ lookup" scan).
6. **`_xlfn.` handling covers the functions we implement**; new post-2007
   functions added later must be checked against the same prefix rule.
7. **CSV export dumps the used range** without padding to A1 and includes
   filter-hidden rows.

---

## Lessons from the bugs found so far

Five real bugs were caught by tests written to be adversarial rather than
confirmatory. Keep that stance:

- Property tests caught a **determinism bug**: cells that only *syntactically*
  referenced a cycle (an untaken `IF` branch) evaluated differently under full
  vs incremental recalculation.
- The golden round-trip caught **`_xlfn.` prefixes** making every modern
  real-world workbook import as `#NAME?`.
- Driving the real browser caught a **stale `requestAnimationFrame` handle**
  (grid never painted in dev) and **`apply()` called inside a `setState`
  updater** (every edit applied twice — would have duplicated every captured
  event).
- Running the real client against the real server caught an **`actor_id`
  mismatch** (every event rejected) and **`context.sheet` transmitted in
  clear** while payload sheet names were hashed.

The pattern: unit tests passed in all six cases. Integration and real
execution found them. When a subagent reports "all green", verify the claim
against the running system, and check that its tests assert the *right*
behaviour — one of them had encoded a bug as expected output.
