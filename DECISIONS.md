# Decisions

One-line rationale for every non-obvious choice.

- **Repo root is `/Users/manavshah/excelclone` (existing empty dir), not a nested `gridline/`** — the working directory was created for this project; nesting would add noise.
- **Rust installed via Homebrew `rustup` (keg-only) + stable toolchain** — no toolchain existed on the machine; rustup is required for wasm32 target management.
- **Version pinning via committed `Cargo.lock` + `package-lock.json` rather than `=x.y.z` in manifests** — lockfiles pin exact versions for every build (incl. transitive deps) while keeping manifests readable; CI uses the lockfiles.
- **Wasm crate named `gridline-wasm` (dir `crates/wasm`)** — crate name `wasm` would collide conceptually with the ecosystem; dir layout follows the spec.
- **`serde-wasm-bindgen` for JS↔Rust value passing** — boring, maintained, avoids JSON string round-trips where possible.
- **CI on `dtolnay/rust-toolchain` + `Swatinem/rust-cache`** — the standard boring GitHub Actions setup for Rust workspaces.
- **getrandom wasm_js backend cfg in `.cargo/config.toml` + target-specific getrandom dep in gridline-wasm** — ulid→rand→getrandom 0.3 requires an explicit JS entropy backend on wasm32-unknown-unknown.

## M1 — Engine core

- **`SheetId` (stable u32) as the internal sheet key; names resolved at the edges** — renames and reorders must not invalidate the dependency graph or cell keys.
- **Sheet names compare case-insensitively (Excel behavior)** — `Data` and `data` are the same sheet; `SheetAdd` rejects a case-variant duplicate.
- **Sheet rename rewrites formula text; sheet delete replaces refs with `#REF!`** — matches Excel; failing loudly beats silently dangling refs.
- **Unknown bare identifiers parse to a `#NAME?` literal node rather than a parse error** — named ranges arrive in P1; until then the cell must still accept the input and show `#NAME?` like Excel, not reject the edit.
- **`^` is left-associative and unary minus binds tighter than `^`** — matches Excel: `2^3^2 = 64`, `-2^2 = 4` (differs from most programming languages).
- **A multi-cell range in scalar context is `#VALUE!`; no implicit intersection** — implicit intersection (`@`) is P2 work; loud failure until then.
- **Aggregates distinguish references from direct arguments** — `=AVERAGE(A1)` with text in A1 is `#DIV/0!` (text behind a ref is ignored) while `=AVERAGE("x")` is `#VALUE!`; single-cell refs follow the range rule, matching Excel.
- **Cycles are detected structurally during recalc (Kahn's algorithm; unprocessed nodes are cyclic) and marked `#CIRC!`** — no iterative-calculation mode in v1; recalc always terminates.
- **A formula whose watched range covers its own cell (`A5 = SUM(A1:A10)`) is a cycle** — matches Excel.
- **Recalc order is deterministic (keys sorted at every tie-break)** — required for the replay invariant; two runs of the same log must produce identical results.
- **`now_ms` is injected into the engine rather than read from the system clock** — `NOW`/`TODAY` must be replayable from the event log.
- **Excel's 1900 leap-year quirk is NOT reproduced** — dates use a plain proleptic serial system; documented per spec §5. Revisit in the Parity Track if the oracle corpus demands it.
- **`ROUND` snaps within 1e-9 before rounding half-away-from-zero** — Excel rounds the *decimal* the user typed, so `ROUND(2.675,2)` is 2.68 even though the binary double is just below 2.675.

## M2 — Engine complete

**Date system**
- **Excel's phantom 1900-02-29 IS reproduced** (serials ≥ 61 use a 1899-12-30 epoch) — the spec permits skipping it, but every real workbook's dates would then be off by one against Excel. Serial 60 itself has no calendar date and converts to `#NUM!` rather than a fake 1900-02-29.

**Structural operations**
- **Undo stores recorded previous state, not inverse actions** — cell-level ops record the cells they touched; ops that relocate cells wholesale (insert/delete row/col, sort, merge, sheet ops) record the affected sheets. Restoring returns the replaced state, so undo and redo are the same operation run in opposite directions, and both flow through `apply()` and emit events. A synthesized inverse *action* cannot express `#REF!` damage faithfully; recorded state can.
- **A copy tiles into a target that is an exact multiple of the source, else pastes once at the anchor** — matches Excel's common cases without implementing its full paste-shape dialog.
- **Cut/paste moves formulas verbatim and retargets references elsewhere that pointed into the moved block** — matches Excel. A range reference follows only when it lies *entirely* inside the moved block; partial overlaps are left alone, as in Excel.
- **Fill treats a single numeric seed as a copy, two or more as a linear series** (constant step, verified consistent). Excel uses a least-squares trend for longer selections; we use the constant step and fall back to repeating the block when no consistent progression exists.
- **Fill has no date awareness yet** — a lone date cell copies rather than incrementing by a day, because number formats (which is how Excel knows a number is a date) arrive in M5. Revisit then.
- **Sort shifts moved rows' relative formula references by the distance travelled** and places blanks last in both directions, matching Excel.
- **Filters are view state** (`Sheet::hidden_rows`), computed from a checkbox-style allowed-value set; no cell values change and no recalculation is triggered.
- **Committing an empty cell edit clears the cell** rather than storing an empty string, so `ISBLANK`/`COUNTBLANK` match Excel.

**Function semantics decided while implementing**
- **Lookups return `0` for a blank result cell** (`VLOOKUP`/`HLOOKUP`/`INDEX`/`XLOOKUP`), matching Excel; `CHOOSE` passes `Empty` through since it evaluates an expression rather than reading a grid.
- **`XLOOKUP` supports exact modes only** (`match_mode` 0 and 2); approximate and binary-search modes return `#VALUE!` until P2.
- **`INDEX` with a 0 index is honoured only when that dimension is a single row/column**; a whole-row/column array result needs the dynamic-array model (P2), so anything else is `#VALUE!`.
- **Approximate lookup is a linear "last entry ≤ lookup" scan, not a binary search** — degrades predictably on unsorted data instead of reproducing Excel's binary-search surprises. Pinned for the oracle corpus.
- **Criteria comparisons are type-restricted**: `">100"` matches only numbers, `"apple"` only text; `"<>"` is the one inclusive operator. Without this, Excel's cross-type ordering (text > number) would make `">100"` match `"zebra"`.
- **`RAND`/`RANDBETWEEN` derive from the injected clock via a pure SplitMix64 mixer, not system entropy** — the engine must stay replayable and I/O-free. Consequence: two `RAND()` calls with identical arguments in one recalc return the same value.
- **`COUNTIF` and friends propagate errors found anywhere in a scanned range**, where Excel ignores them. Pinned as an open oracle question for P0 rather than guessed at.
- **`numfmt` fails soft**: an unrecognised format code renders as General instead of erroring, because it only affects display.

## M2 — xlsx / csv I/O

- **Preservation is implemented by patching the original zip, not by regenerating it** — on import every part is retained verbatim; on export only each modelled sheet's `<sheetData>` (and `<mergeCells>`) is replaced, leaving `<cols>`, `<conditionalFormatting>`, `<dataValidations>`, `<pageMargins>`, tab colours, charts, pivot caches and VBA byte-identical. Per-cell style indices (`s=`) captured at import are written back, so formatting we do not model still survives.
- **Inline strings (`t="inlineStr"`) are used when rewriting sheet data** — writing shared strings would mean rewriting `sharedStrings.xml`, which is exactly the kind of shared part the preservation rule exists to protect.
- **New workbooks export via `rust_xlsxwriter`; imported ones via the zip-patch path** — the fixed choice in §4 covers the from-scratch case, and the preservation rule cannot be expressed through it.
- **Adding a sheet to an imported workbook fails loudly on export** rather than silently dropping it, because writing a new sheet part also means rewriting `workbook.xml` and its relationships. Known gap; the error names the sheet.
- **`_xlfn.` / `_xlws.` function prefixes are stripped at parse time** — xlsx stores every post-2007 function that way (`_xlfn.TEXTJOIN`, `_xlfn.XLOOKUP`, `_xlfn.IFS`, `_xlfn.CONCAT`) and Excel hides it. Without this, importing any modern real-world workbook produced `#NAME?`. Caught by the golden-workbook round trip, not by a unit test.
- **A formula whose source contains `_xlfn.` is re-rendered from its AST for display**, so the formula bar shows `=TEXTJOIN(...)` like Excel; all other formula text is stored exactly as written.
- **A formula our parser rejects imports as literal text plus a warning** — never silently dropped, and the original text survives the round trip.
