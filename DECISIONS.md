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

**xlsx formatting**

- **Import parses `xl/styles.xml` into our model *and* keeps every cell's original `s` index; export writes that original index back for any cell whose format did not change** — so opening and saving a workbook without touching its formatting leaves the style sheet byte-identical, including every attribute we do not model (typefaces, theme colours, vertical alignment, diagonal borders). A test asserts exactly that, because "we preserve formatting" is the kind of claim that quietly stops being true.
- **A changed cell gets a *newly appended* `<xf>`, never a rewritten one** — appending keeps every existing index pointing at the same record, so parts of the package we never parsed stay correct. Rewriting in place would silently repaint every other cell that shared the style.
- **A synthesized font copies the original font's `sz`, `name`, `family`, `scheme` and `u` children verbatim** — bolding an 8pt Garamond cell must produce bold 8pt Garamond, not bold 11pt Calibri.
- **Theme and indexed colours read as "no colour" rather than being resolved** — they resolve through `xl/theme/` and a legacy palette we do not parse, and guessing black would be wrong in a dark theme. Because an unchanged cell keeps its original index, they still round-trip perfectly; they simply do not appear in the model.
- **A style sheet we cannot parse is a warning, not a fatal error** — the cells still import, they just arrive unformatted, and the preserved indices keep the round trip lossless.
- **Formatting a workbook whose package has no `xl/styles.xml` fails loudly** — adding a new part means touching `[Content_Types].xml` and the workbook relationships, the same gap that blocks adding a sheet to an imported workbook.
- **Import writes `Sheet::formats` directly instead of replaying `FormatApply` actions** — the file's existing formatting is initial state, not something the user did in this session. The importer already builds its `Engine` directly for the same reason.
- **`Engine::clear_history()` is called at the end of every import** — import replays a file cell by cell through `apply`, which is what keeps the importer honest, but it also meant a freshly opened workbook arrived with one undo entry per imported cell and the user's first Ctrl+Z un-typed a cell they never typed.
- **Splice edits are ordered by `(start, end)`, not `start`** — when a zero-length insertion shares an offset with a replacement, sorting on `start` alone leaves their order to sort stability, and the replacement overwrites the text the insertion just placed. That bug produced a corrupt style sheet on the one path that inserts a missing `<numFmts>`.

**Grid and UI**

- **The viewport sends per-cell format *indices* plus a small palette, not per-cell objects** — a formatted block is overwhelmingly repetitive; a bold header row is one palette entry and N small integers rather than N copies of the same object crossing the wasm boundary every frame.
- **Number formatting is applied in Rust, inside `viewport`** — `numfmt` already implements Excel's format codes for `TEXT`, and a second implementation in TypeScript would drift. The grid draws text; deciding what the text says stays with the engine.
- **`SheetInfo` reports both `used_*` (data extent) and `painted_*` (everything drawable)** — Ctrl+Down should stop at the last value, but the scrollable area has to include a bold empty column.
- **Clicking anywhere in a merged block selects the whole block, and dragging a selection expands to contain any merge it touches** — otherwise the cursor can land on a cell the user cannot see, and a partially-selected merge makes every subsequent operation ambiguous. Expansion iterates to a fixed point, because absorbing one merge can bring the selection into contact with another.
- **A merged block is painted by covering it and re-stroking its outline, after the gridlines** — cheaper and simpler than teaching the gridline pass to skip interior segments, and it keeps the single-pass gridline path intact.
- **Drag autoscroll ramps with distance past the edge** — a fixed step makes selecting a long range either unbearably slow or impossible to stop on the right row.
- **`GRIDLINE_CHROMIUM` overrides the Playwright browser** — some sandboxes ship a Chromium whose build number does not match the one Playwright expects and cannot reach the download host; pointing at the binary that is already there beats not running the suite.
- **`window.__gridline__` exposes `stateSnapshot` and nothing else, in dev builds only** — the grid is a canvas, so the end-to-end suite has no DOM to assert formatting against. It is deliberately read-only: a test-only route into `apply` would be exactly the side door the single-mutation-path invariant exists to forbid.
- **`rust_xlsxwriter`'s `wasm` feature is enabled for the wasm32 target only** — the crate stamps each workbook with the current time, and `SystemTime::now()` traps on `wasm32-unknown-unknown`. Every xlsx export from the browser panicked with `RuntimeError: unreachable`; the bug had been latent since M2 because nothing in the UI called export until M5. No native test can catch a regression, so the guard is the end-to-end test that saves a workbook in a real browser and opens it again.

## M6 — Miner

- **Mining runs on abstract tokens, never on raw events** — `=SUM(B2:D2)` in E2 and `=SUM(B3:D3)` in E3 are one habit repeated, and a tokenizer that kept the addresses would never notice. References are normalized to R1C1 offsets from the writing cell, which is exactly what collapses a filled-down column into one repeated token.
- **Literal values never enter a token, even under `full` capture** — the habit is "typed a number here"; the number is what we promised not to mine. A hashed literal and a verbatim one produce the same token, so mining behaves identically in both capture modes.
- **The unparseable-formula fallback scrubs strings and sheet qualifiers** rather than passing the raw text through. Whole-column references (`A:B`) do not parse in v1, so that fallback is the path a real workbook takes — and it would otherwise have been the leak the careful path exists to prevent.
- **Two miners, not one.** Tandem repeats catch a loop the user ran by hand in one sitting and need no cross-session support; PrefixSpan catches a habit spread across sessions and needs three. Neither finds the other's answers.
- **A tandem repeat may never span a session boundary** — three sittings that each start the same way concatenate into what looks like a loop, and calling it one both overstates the evidence and puts "repeated 3 times in a row" in front of a user for whom it never happened. Caught by a planted-pattern test, not by the unit tests.
- **Only primitive periods are reported** — `a b a b a b a b` has a period of 4 as well as 2, and proposing the longer one would offer a routine that does the work twice.
- **PrefixSpan counts support by session, not by occurrence** — one busy afternoon is a loop, and letting a single session mint support would make every busy afternoon look like a habit.
- **Only maximal patterns are proposed** — offering both `a b` and `a b c` is offering half a routine alongside the whole one.
- **Review cost is charged once per routine, not once per repetition** — the first version charged it per occurrence, which priced in re-reading the same preview twelve times and made a genuine twelve-row habit score below the threshold and vanish from the panel.
- **Scoring is backward-looking**: "you have already spent this long", which a user can check against their own memory, rather than a projection of future savings, which they cannot.
- **A routine is synthesized from a real occurrence, not from the tokens** — tokens are deliberately lossy, so building from them would mean inventing the details back. The most recent occurrence is the template, on the grounds that the user's latest way of doing something is most likely to still be right.
- **A routine keeps the coordinates it was recorded at, plus its anchor, and is shifted once when it runs** — normalizing the actions to the origin first looks tidier and is quietly wrong: `=SUM(B5:D5)` written in E5 points three columns left, so moving it to A1 walks off the grid and the reference collapses to `#REF!` before it can be moved back.
- **What cannot be rebuilt is stated, not guessed** — a redacted literal becomes a `Requirement` the routine reports and does not perform, and a paste is skipped entirely because the log records its shape but not the clipboard. A routine that pasted the wrong block would be far worse than one that does not paste.
- **A routine runs through `Engine::apply` like everything else** — no second execution path, so it can do nothing the user could not have done by hand, and every action it takes is captured like any other.
- **The dry run clones the engine rather than applying and undoing** — undo is itself engine behaviour, and a preview that leaned on undo being correct could not show the user a bug in undo. The diff includes downstream recalculation, which is most of the point.

**Routines: where the pieces live**

- **`Routine`, its shift logic and the dry-run sandbox live in the engine, not the miner** — the miner discovers routines, the server stores them and the client runs them, so three things need the definition and it must be one definition. The same reasoning that put telemetry in the engine. What stays in the miner is discovery: `synthesize`, `reconstruct` and the summary, which are all about reading a mined pattern back into actions.
- **The miner writes the `routines` table directly** rather than posting to an endpoint. The server's own doc comment already said this is the arrangement ("the miner writes rows here; the server only serves them back and records feedback"), and an HTTP path would need an admin credential and a second copy of the schema. The miner keeps a copy of the table definition for its in-memory tests, and a test compares that copy against the migration so the two cannot drift silently.
- **Re-mining updates a proposal; it never duplicates one** — the routine id is derived from the pattern's token shapes, so a nightly run over a growing log refreshes support and estimate in place.
- **A verdict the user has given is never overwritten.** A dismissed routine stays dismissed no matter how strong the evidence gets; asking again every morning is how a good feature becomes an irritating one. The *body* is still refreshed, so an accepted routine keeps working as the log grows.
- **Proposals the log no longer supports are pruned; answered ones are not.** A panel that only grows stops being read, but a dismissed row is the user's answer rather than our suggestion, and deleting it would resurrect the suggestion on the next run.
- **Mining is per (actor, workbook)** — a habit belongs to the person who has it, and pooling two people's logs would manufacture support neither of them earned.
- **The wasm bridge hands back a routine's actions rather than running them.** `routineActions` returns the shifted `Action`s and the client pushes them through the same `applyBatch` every other gesture uses. Running them inside the engine would be a second execution path — exactly the side door the single-mutation rule exists to forbid — and the capture pipeline would have to be taught what a routine is instead of just seeing the actions.
- **A routine is run as one batch**, so it lands in one undo step: a suggestion the user has to press Ctrl+Z five times to reject is not a suggestion.
- **Every proposal previews before Run is available**, against the current selection, computed by the same engine the run will use. A routine that would change nothing here says so and disables Run; one the engine would refuse shows the refusal before the click, not after.
- **A partial routine names the cells it cannot fill** and runs the rest. "The log only has a hash of that number" is a fact about what we chose not to record, and hiding it would make the routine look broken instead of honest.
- **A server the panel cannot reach is a line of text in the panel, not an error toast** — no suggestions is not a broken spreadsheet.
- **The dry run diffs formatting as well as values.** The first version compared cell values only, so a routine that bolds a header row previewed as "nothing would change" and the panel disabled Run on a routine that worked perfectly well. Format changes are reported separately and described in words, because "B2 becomes bold" and "B2 becomes 47" are different enough that one column would read as noise.
- **`Engine::apply_batch` coalesces the undo entries its actions push into one.** Applying in a loop pushes one per action, which made the panel's own promise — "runs in one undo step" — false, and meant rejecting a five-step routine took five Ctrl+Z. Caught by the browser suite; nothing had used `applyBatch` until routines did.

## M7 — Demonstration dataset

- **A record is `{pre_state_digest, context, action, post_state_digest}` and never the state itself.** A digest identifies a state without disclosing it, which is what lets the dataset say "from here the user did this, reaching there" while carrying none of their data. The digest is SHA-256 of the same `state_snapshot` the replay suite compares, so "same digest" means exactly what a passing replay test means.
- **Redacted literals replay as placeholders derived from their hash, and the record says so.** Under `structural` capture the real values were never recorded, so there is nothing to replay; deriving the stand-in from the hash preserves the one property the dataset actually needs — equal values stay equal, different ones stay different — so a lookup finds its match and a conditional aggregation counts the right rows. `values_synthetic` is on every record, because a consumer that mistook these for the user's numbers would be drawing conclusions from noise.
- **A placeholder keeps its original's type.** A number stays a number and text stays text (apostrophe-forced, so a placeholder that looks like a number or a formula still lands as text), because `=A1+A2` over two text placeholders is `#VALUE!` and the record would be worthless.
- **Each session replays from an empty workbook.** A session is the unit a demonstration is read from; carrying state across one would make every record after the first depend on work the reader cannot see. It also means two sessions that did the same thing produce the same digests, which is how the dataset is checked.
- **An action the engine refuses is counted, not recorded.** Its pre and post digests would be identical, and a record of that teaches a reader the action is a no-op rather than that it was refused.
- **Consent is a `WHERE` clause, not a filter over the results** — the same predicate the server's own export uses. A filter can be forgotten; a join cannot. Latest consent wins, `off` and revoked are excluded, and an actor with no consent row at all contributes nothing.
- **`--consented-only` is mandatory for a database export rather than the default.** An operator who has to type it cannot later say they did not know the export was filtered, and a flag that must be typed cannot be quietly dropped from a copied script. It is *refused* with `--in`, because a JSONL log carries no consent records and accepting the flag there would let a script claim a check that never ran.
- **The miner's test database is the server's migration, run verbatim.** The earlier arrangement — a hand-copied `routines` table plus a drift test — could not cover the consent query, which is only worth anything against the real `consents` table. `include_str!` on the migration was already crossing that boundary anyway.
- **One JSONL file per session, named `NNNN-<session-id>.jsonl`, plus a `manifest.json` naming exactly the files that belong to the dataset.** Session ids arrive from a client, so the filename is sanitized to alphanumerics and truncated: an id of `../../etc/passwd` must land inside the output directory as a mangled name. The numeric prefix makes collisions between two ids that sanitize alike impossible. Stray `.jsonl` files from an earlier run are reported rather than deleted — they would read as part of the dataset to anyone globbing the directory, but removing a file the operator put there is not ours to do.

**Sheets in an imported package**

- **Worksheet parts are tracked by `SheetId`, never by name.** Matching on the name makes a rename indistinguishable from deleting one sheet and adding another, and the renamed sheet would silently lose everything its part held that we do not model: its columns, its conditional formatting, its drawings. Sheet ids are monotonic and restored by undo, so the mapping survives an undone delete.
- **The package is spliced, never regenerated.** `xl/workbook.xml`, `xl/_rels/workbook.xml.rels` and `[Content_Types].xml` are edited by byte range: a rename rewrites one attribute, an addition inserts one element before each closing tag, a deletion removes exactly the elements that named the departed part. When the sheet list is unchanged, none of the three is written at all — which is the only way to keep the promise that opening and saving a file changes nothing it did not have to.
- **A new sheet's part is generated empty and then patched by the same writer as every other sheet**, so there is one code path for writing a worksheet rather than two that can disagree.
- **New relationship ids clear every existing one, not just the worksheet ones.** `rId2` is often a VBA project or a theme; allocating the next free *worksheet* id would collide with it.
- **A new part's filename is the first free `sheetN.xml`, which need not match its `sheetId`.** The two are unrelated in the format, and pretending otherwise breaks on any file whose author deleted a sheet before us.
- **Deleting a sheet drops `xl/calcChain.xml`; renaming one does not.** The chain is keyed by sheet index, so adding or removing a sheet renumbers it and Excel offers to repair the file. It is a cache: dropping it costs a recalculation on open. A rename renumbers nothing, so throwing the cache away would be a cost paid for no reason.
- **A package with no `xl/styles.xml` is given the default one at import, not at export.** Formatting then always has somewhere to go, and every later stage has one case to handle instead of two. The default records matter: index 0 of each collection must be the real default, because a cell with no `s` attribute means `s="0"` — appending into empty collections would make the first format anyone applies the default for the whole workbook.
- **Verified against an independent reader.** The round-trip tests use our own importer, which would happily read back a file only we can parse; the add/rename/delete output was additionally opened with `openpyxl` to confirm a third-party reader agrees about the sheet list and the formulas.

**The demo**

- **The demo is a script, not a rehearsal.** `scripts/demo.sh` seeds a database, mines it, exports the dataset and checks the consent refusal, all from an empty directory. Every piece goes through the real path: the events are built by `engine::telemetry::describe`, the same function the browser calls through wasm; the rows land in the schema the migrations declare; the consent test is the export's own query. A seeder that fabricated payloads would demonstrate a system that does not exist.
- **The seed includes an actor who declined.** The export refusing to carry them is the claim worth making, and it can only be *shown* if there is something it could have taken. `scripts/demo.sh` greps the output for their sessions and fails if it finds any.
- **The seeder writes the worked workbook, not just the log.** A log claiming twelve rows were added and a file containing five is a demo that contradicts itself. The workbook is exported through the preserved package, so what the presenter opens is the fixture with the scripted work in it.
- **The rate card is referenced absolutely (`Rates!$A$1:$B$3`).** It was relative, which is a bug waiting in a real ledger — fill the row down and the lookup walks off the table — and it also made every row of the ledger a *different shape* to the miner, so the habit was invisible. The fix is in the sheet, not in the normalizer: a relative reference genuinely does mean something different in every row.
- **The summary block moved below the member table and reads a range with room in it.** A total that stops at the fifth row goes quietly wrong on the sixth, and a ledger the demo appends to needs somewhere to append.

**What running the miner on real data changed**

- **A loop explains the steps it covers; nothing else may count them as evidence.** Twelve identical rows typed over three sittings produced **155 proposals**: every subsequence straddling a row boundary reached support, and the ones that hit `max_length` looked maximal only because the search stopped there. Masking the steps a tandem repeat covers takes it to one. The masked steps are skipped, not deleted — they still occupy their positions, so the work either side of a loop does not become spuriously adjacent.
- **Identical loops from different sittings merge into one proposal.** The detector works per session, so a habit run every morning arrived as three identical patterns; proposing each separately offered the same routine three times and understated what it saved.
- **A merged loop does not say "in a row".** Twelve repetitions across three mornings is not twelve consecutive ones, and the summary now distinguishes them. The same reasoning that stopped tandem repeats spanning sessions in the first place.

**What walking the demo in a browser changed**

- **`VITE_DEV_TOKEN` now actually does something.** `scripts/dev.sh` had been printing "the web app reads it from VITE_DEV_TOKEN, so no manual step is needed" since M4, and nothing read it. There is no sign-in screen, so every API call in a `make dev` session came back 401 unless you opened the console and wrote to `localStorage` by hand. The app adopts it at start-up, only in a dev build, and only when nothing is already stored — a token typed in by hand still wins.
- **`VITE_DEV_WORKBOOK_ID` exists for the demo.** The workbook id is minted by the client and kept in `localStorage`, so routines mined for a seeded workbook could never appear in a panel that had invented its own id. Same guards as the token.
- **A requirement that is already filled in is not reported as missing.** The preview said "you will still need to type them" about four cells the user could see on screen, filled in. `DryRun` now reports `unmet` — the subset of `requires` whose target is actually empty — alongside `requires`, and the panel says the values are already in place when they are. Found by looking at a screenshot of the panel, not by a test.

**The demo's own conventions**

- **The ledger labels every column it uses and formats the rows it will grow into.** Six unlabelled columns under a header band is what an unfinished sheet looks like, and a currency band that stops at the last filled row makes the next row someone types look like a different sheet.

**Saying plainly what "formulas are verbatim" costs**

- **A formula's text literals and sheet names are recorded in clear, and the documents now say so where a user reads them.** `structural` hashes every literal value and every sheet name — except the ones written *inside a formula*, because the formula is kept as typed. Reading `PRIVACY.md` and `EVENTS.md` together, a careful user would have concluded sheet names are never disclosed; a dataset record showing `=VLOOKUP(B7,Rates!$A$1:$B$3,2,FALSE)` beside a hashed `context.sheet` says otherwise. Found while checking the new `DATASET.md` against a real exported record, which is the point of writing documentation against output rather than against intent.
- **The disclosure was documented, not redacted.** Blanking string literals inside formulas would break the dataset — a replay of `=IF(F7<=0,"","")` computes something the user never saw — and the mining pipeline already reduces formulas to shapes with the text blanked and the sheet qualifier anonymised before it looks for a pattern. What was wrong was the description, not the behaviour. The consent notice, the transparency page, `PRIVACY.md`, `EVENTS.md` and `DATASET.md` now all say the same thing, and the honest advice for a sensitive sheet name is `off`.
