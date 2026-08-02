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
