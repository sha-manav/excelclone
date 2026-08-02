# Gridline — handoff

Continue building **Gridline** per the original spec (the full brief is the
source of truth; this file only records where the work stopped and what to
watch out for). Repo: <https://github.com/sha-manav/excelclone>.

Work the milestones in order under the same autonomy rules: decide, record the
decision in `DECISIONS.md`, keep `PROGRESS.md` current, commit at least once
per milestone, and never leave the repo red.

---

## What is already done

**M0–M5 are complete. M6 is complete except for the panel.** 325 Rust tests,
140 web unit tests (vitest), 35 end-to-end tests (Playwright); `cargo clippy
--workspace --all-targets -- -D warnings` is clean.

| Milestone | State |
| --- | --- |
| M0 Scaffold | Cargo workspace (`engine`, `gridline-wasm`, `server`, `miner`), Vite/React/TS app, GitHub Actions CI, `make dev` |
| M1 Engine core | Model, Pratt parser with Excel precedence, evaluator, dependency graph, incremental recalc, cycle detection |
| M2 Engine complete | Full v1 function set (~90), reference rewriting, paste/fill/insert/delete/sort/filter/merge, undo/redo, xlsx+csv I/O with part preservation, golden workbooks |
| M3 Wasm + grid | wasm bindings, virtualized canvas grid, formula bar, sheet tabs, verified in a real browser |
| M4 Event spine | Envelope + redaction in Rust, client capture pipeline, consent flow, capture chip, transparency page, axum server (ingest/consent/export/sessions), determinism-replay suite |
| M5 Grid completeness | Per-cell format model through the xlsx round trip, find & replace, formatting toolbar, context menu, sort/filter UI, merges painted, import-notes drawer, file open/save, autoscroll, autofit |
| M6 Miner | Normalization, loop + PrefixSpan mining, scoring, routine synthesis, dry-run sandbox, CLI, planted-pattern acceptance suite. **The panel and the server write-back are not done.** |

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
- **Number display caps at 15 significant digits**, matching Excel, so
  `=0.1*3` renders `0.3`.
- Serials reproduce Excel's phantom 1900-02-29 so dates round-trip with real
  workbooks; serial 60 itself is `#NUM!`.
- **`window.__gridline__.stateSnapshot()`** exists in dev builds only, for the
  end-to-end suite. It is read-only by design.

### Running it

```sh
make dev     # builds wasm on first run, seeds an admin token, starts API + UI
make ci      # fmt, clippy -D warnings, cargo test, vite build
cd apps/web && npx vitest run      # unit tests
cd apps/web && npx playwright test # end-to-end
cargo run -p miner --bin gridline-miner -- mine --in events.jsonl
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

### M6 — finish the routines loop (next, and small)

The miner produces `Routine` values and previews them; nothing shows them to a
user yet. Three pieces:

1. **Write routines into the server.** The `routines` table and
   `GET /v1/routines` + `POST /v1/routines/:id/feedback` already exist and are
   tested. The miner needs a `--db <path>` (or an admin `POST /v1/routines`)
   that upserts on `Routine::id`, which is stable across mining runs precisely
   so re-mining updates rather than duplicating.
2. **Expose the dry run through wasm.** `miner::routine::dry_run` takes an
   `&Engine`; the wasm crate holds one. The bridge needs
   `previewRoutine(bodyJson, sheet, row, col) -> DryRun` and
   `runRoutine(bodyJson, sheet, row, col)`. Running must go through
   `Engine::apply` so the actions are captured, and should emit `routine.run`
   as a shell action alongside them (already in the vocabulary and in
   `docs/EVENTS.md`).
   *Note:* `crates/wasm` does not depend on `miner` today. Either add the
   dependency or move `Routine`/`dry_run` into the engine. The former is
   quicker; the latter is arguably where they belong, since the engine already
   owns telemetry for the same reason.
3. **The Routines panel.** List proposals ranked by
   `estimated_minutes_saved`, each with its summary, a preview of the cell
   diff at the current selection, and Run / Dismiss. Dismiss posts feedback. A
   routine with a non-empty `requires` must say plainly which values it cannot
   supply rather than making a partial change silently — the miner already
   reports them as offsets and kinds.

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
   before the demo. Formatting a package that has no `xl/styles.xml` fails the
   same way and for the same reason.
2. **Every structural operation triggers a full dependency rebuild and
   recalculation.** Correct but O(all formulas). Fine now; revisit under P5.
3. **No keyboard navigation *into* a merged range.** Merges are painted, and
   click and drag selection handle them, but arrow keys still walk the covered
   cells.
4. **The import-notes drawer has no end-to-end test with notes in it** — only
   the negative case (a clean file shows no badge). Producing a warning needs a
   fixture using a feature we do not model, and fixtures are generated rather
   than committed. The drawer is presentational; the warnings themselves are
   covered in Rust.
5. **Two Excel semantics are deliberately unresolved**, pinned for the P0
   oracle rather than guessed: whether `COUNTIF` should propagate errors found
   in its range (we propagate; Excel may ignore), and whether approximate
   lookup should replicate Excel's binary-search behaviour on unsorted data
   (we do a linear "last entry ≤ lookup" scan).
6. **`_xlfn.` handling covers the functions we implement**; new post-2007
   functions added later must be checked against the same prefix rule.
7. **CSV export dumps the used range** without padding to A1 and includes
   filter-hidden rows. It also ignores number formats, where Excel exports the
   formatted text.
8. **Case-insensitive find/replace is ASCII-only.** Full Unicode folding
   changes byte lengths and would corrupt the text around a match; "Match case"
   is exact for any script.
9. **Formatting is per cell, capped at 200k cells per action.** There are no
   row or column format defaults, which is what xlsx uses for "bold this whole
   column". Raising the cap properly means adding them.
10. **The miner cannot rebuild a paste, a filter or a replacement** from the
    log: the log records their shape but not the clipboard, the allowed values
    or the search terms. Those steps are skipped during synthesis, so a routine
    mined from a gesture containing them is shorter than the gesture was. If
    this matters, the fix is in what the *capture* records, not in the miner.

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

Two habits worth keeping specifically:

- **When a test passes on the first run, try to make it fail.** Several tests
  here were verified by temporarily breaking the code they cover; two turned
  out to be asserting nothing — an autofit check that passed without autofit
  implemented, and a tautological `!x || x`.
- **When a suite or a subagent reports "all green", check the claim against the
  running system.** The cautionary example from this session is a
  `tsc --noEmit` that passed cleanly against an empty `node_modules`.
