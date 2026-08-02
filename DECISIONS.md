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

## M4 — Event spine

- **The event envelope and privacy redaction live in `crates/engine/src/telemetry.rs`, not in the server or the client** — the client redacts by calling this same code compiled to wasm. A privacy guarantee implemented twice is a guarantee that will eventually be implemented differently, and the divergent copy is the one that leaks. It also keeps the engine I/O-free: these are pure data transforms.
- **A test asserts the code's action vocabulary appears in `docs/EVENTS.md`** — the transparency page renders that document, so an action captured but not documented would be undisclosed capture. That fails the build rather than shipping.
- **Salt and text are separated by a `0x1f` byte before hashing** — otherwise salt `"ab"` + value `"c"` would collide with salt `"a"` + value `"bc"`.
- **Sheet names, filter values and search terms are hashed under `structural`, not just cell values** — a sheet named "Payroll Q3" or a filter on an email address is user content by any reasonable reading of the promise in `docs/PRIVACY.md`.
- **Formulas are kept verbatim in `structural` mode** — their structure is the entire point of mining, and they describe shape rather than content. This is stated plainly in the consent copy rather than buried.
- **The server rejects an envelope claiming `full` privacy mode when the actor's consent is `structural`** — beyond the spec, but without it a buggy or forged client could smuggle verbatim values in under a structural grant, contradicting what the user was promised.
- **Sessions are derived server-side from `ts_ms` gaps rather than trusting the client's `session_id`** — the client's id is retained alongside so disagreements (reload, crash recovery, a tab left open over lunch) are visible rather than silent.
- **`ts_ms` is the client wall clock, deliberately** — a batch flushed hours later still sessionizes where the work actually happened. `received_at` is used only for export `since` filtering. Clock skew can therefore fabricate or hide a split; noted as a known limit.
- **Ingest is partially accepting: good events in a mixed batch are stored** and bad ones are reported with reasons. An all-or-nothing batch would let one malformed event discard a whole session's work.
- **Rejected actions stay in the replay log** — replay must reject exactly the same actions the live session did, so a log records attempts, not just successes.
- **`format_number_general` caps at 15 significant digits** — Excel's General format does, which is why `=0.1*3` shows `0.3` there. Printing the shortest round-trip float instead exposed IEEE noise on nearly every decimal a user sees.
- **`context.sheet` is redacted under `structural`, not just payload sheet names** — the context rides on every envelope, so leaving it in clear would have disclosed the sheet name on every event *and* handed an observer a matched hash/plaintext pair, unpicking every other hash made with that workbook's salt. Redacting a value in one field while sending it in clear in another is worse than not redacting it at all. Caught by an end-to-end test that renames a sheet to a sensitive string and asserts it never appears in transmitted bodies.
- **Redaction fails closed** — if the redactor throws, the client transmits an empty sheet name rather than the raw one.
- **The `actor_id` is stamped at flush time, not at capture time** — the server's identity arrives asynchronously, and anything buffered before it lands would otherwise carry the client's placeholder and be rejected as a mismatched actor. Since rejected events are retried unchanged, those events would have been lost permanently.

## M5 — Grid completeness

**Formatting model**

- **Formats live in a second sparse map on the sheet (`Sheet::formats`), not in `Cell`** — a format has to be able to exist without a value, because formatting a blank column before typing into it is an ordinary gesture. Putting the format inside `Cell` would mean inventing a cell to hold it, which then appears in the used range, in CSV export, and in every count of populated cells. It also keeps the evaluator, dependency graph and parser entirely unaware of presentation.
- **Formats are interned in a workbook-level append-only table (`FormatTable`)** — the same shape xlsx uses with `cellXfs`; a bold column costs one `CellFormat` and N `u32`s. Append-only means an id recorded for undo still resolves after any number of intervening changes, which is what lets `UndoState::Formats` record ids rather than whole formats. Interning is a linear scan because real workbooks have tens of distinct formats, and a `Vec` stays trivially serializable and deterministically ordered.
- **`Sheet::used_range` stays blind to formatting; `Sheet::painted_range` unions cells, formats and merges** — the used range is the *data* extent, which is what CSV export and whole-sheet ranges mean. A bold empty column is not data, but the grid still has to draw it.
- **The unit of change is a `FormatPatch` naming one attribute, and an action carries a list of them** — "make this bold" must not clear the fill colour set a moment ago. An `Option<Option<T>>` field would have had to distinguish "clear it" from "leave it alone" by JSON `null` versus a missing key, which serde does not do without a custom deserializer; `{"set":"fill_color","value":null}` is unambiguous.
- **The state snapshot records resolved formats, not format ids** — an id is an artefact of the order formats happened to be interned, which differs between two paths to the same workbook. That is exactly the difference the replay suite must not see.
- **Border presets are a range gesture, not a cell attribute** — `Outline` gives a cell different edges depending on where in the range it sits, so the preset is resolved per cell at apply time and only the resulting edges are stored.
- **Colours are normalized to `#rrggbb` at apply time** — otherwise `#FFF`, `#ffffff` and `FFFFFF` would intern as three distinct formats that render identically.
- **A formatting action is capped at `MAX_FORMAT_CELLS` (200k) and fails loudly above it** — we model formatting per cell, not per row/column as xlsx does, so "bold this whole column" would otherwise materialise a million map entries. Lifting the cap properly means row and column format defaults, which v1 does not have.
- **Formatting travels with contents through paste, cut, fill, sort and insert/delete, but not through paste-values** — "paste values" means the numbers without the dressing. Sorting a range narrower than the formatted region therefore leaves the outer columns' borders behind at their positions while the inner ones move with their rows; that is what Excel does too.
- **`Delete` clears contents and leaves formatting; `FormatClear` is a separate action** — Excel's semantics, and the reason "Clear Formats" is its own menu item.
- **Formatting never triggers a recalculation** — a number format changes how a value reads, not what it is.

**Find & replace**

- **`FindReplace` is one engine action, not N `CellEdit`s** — it undoes as a single step, emits one `find.replace` event for the miner, and cannot half-apply.
- **Matching and replacement both operate on the cell's formula-bar text, never its computed value** — there is no way to write a replacement back into a formula's result, so a search that matched results would either refuse to replace or destroy the formula that produced them. Excel's default "Look in: Formulas" behaves the same way.
- **Case-insensitive matching is ASCII-only** — full Unicode case folding changes byte lengths (`İ` lowercases to two chars), so an offset found in a folded haystack does not point at the same place in the original, and splicing a replacement at it corrupts the surrounding text. `to_ascii_lowercase` is length-preserving. Non-ASCII users get exact matching with "Match case" rather than silently mangled cells.
- **A replacement that would not parse as a formula leaves that cell alone** rather than failing the whole operation part-way through.
