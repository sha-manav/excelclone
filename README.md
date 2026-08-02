# Gridline

A web spreadsheet with a Rust calculation engine, built around a complete,
consent-gated, semantic log of everything you do — and a mining pipeline that
turns those logs into replayable automations.

Screen recorders and OS keyloggers capture spreadsheet work as pixels and raw
input: noisy, and hard to learn anything from. Gridline owns the application
layer instead, so every action arrives already structured and already
replayable. From that log it can detect the sequences you repeat and offer to
run them for you, and it can export high-fidelity demonstration data.

## Status

Work in progress. See `PROGRESS.md` for the current milestone and
`DECISIONS.md` for every non-obvious choice and its rationale.

## Quick start

Requires Rust (stable), Node 22+, and `wasm-pack`.

```sh
make dev      # starts the API server and the web app
```

Then open the URL Vite prints. On first run you will be asked to choose a
capture mode; nothing is recorded until you do.

Other targets:

```sh
make test     # cargo test --workspace
make lint     # cargo fmt --check + clippy -D warnings
make wasm     # build the engine for the browser
make ci       # everything CI runs
```

## Layout

```
crates/engine/   spreadsheet core — model, parser, evaluator, actions/events
crates/wasm/     wasm-bindgen bindings, consumed by the web app
crates/server/   axum + SQLite: auth, event ingest, consent, workbooks, routines
crates/miner/    log normalization, sequence mining, routine synthesis, export
apps/web/        React app: canvas grid, formula bar, consent UI, routines panel
fixtures/        golden workbooks and recorded event logs used by tests
docs/            ARCHITECTURE.md, EVENTS.md, PRIVACY.md
```

## How it works

Every mutation flows through one function:

```rust
Engine::apply(Action) -> Vec<Event>
```

The UI never touches workbook state directly. That single path is what makes
the log trustworthy: replaying it from an empty workbook reproduces the exact
final state, and there is a property-based test suite asserting exactly that.
It is the most important test in the repo, because everything else — undo,
routine replay, dataset export — depends on it holding.

Read `docs/ARCHITECTURE.md` for the full picture.

## Privacy

Capture is off until you turn it on, visible whenever it is on, and pausable
at any moment. The default mode records the *structure* of your work while
replacing the literal values you type with salted hashes. There are no
third-party analytics or telemetry SDKs in this project; data goes only to
your own server.

`docs/PRIVACY.md` explains this in plain terms and `docs/EVENTS.md` is the
machine-readable contract. The app renders the same vocabulary at
`/transparency`.

## Excel compatibility

The goal is drop-in behavioral compatibility, pursued as a measured score
rather than a claim. `crates/parity` runs a differential harness against an
oracle corpus and regenerates `PARITY.md` in CI: function coverage, cell-match
rate, and round-trip fidelity. Anything not yet implemented fails loudly
(`#NAME?`, an import warning) and round-trips losslessly — opening and saving
a file never destroys features Gridline does not yet understand.

No Microsoft UI assets, icons, or branding are used or reproduced.
