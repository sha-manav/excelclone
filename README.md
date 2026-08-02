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
make demo     # seed a worked workbook and history, mine it, export the dataset
make test     # cargo test --workspace
make lint     # cargo fmt --check + clippy -D warnings
make wasm     # build the engine for the browser
make ci       # everything CI runs
```

## The demo

`make demo` builds the whole story from an empty directory: a dues ledger with
three sittings of scripted work in it, a database holding the event log that
work produced, the routine mined out of it, and a demonstration dataset — plus
a second actor who declined capture, whose events are in the log and in none of
the output. It prints the browser steps when it finishes.

## Layout

```
crates/engine/   spreadsheet core — model, parser, evaluator, actions/events
crates/wasm/     wasm-bindgen bindings, consumed by the web app
crates/server/   axum + SQLite: auth, event ingest, consent, workbooks, routines
crates/miner/    log normalization, sequence mining, routine synthesis, export
crates/parity/   the Excel comparison harness and its scorecard
apps/web/        React app: canvas grid, formula bar, consent UI, routines panel
fixtures/        golden workbooks and recorded event logs used by tests
parity/          the case corpus and the target function list
docs/            ARCHITECTURE.md, EVENTS.md, PRIVACY.md, DATASET.md
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

`docs/PRIVACY.md` explains this in plain terms, `docs/EVENTS.md` is the
machine-readable contract, and `docs/DATASET.md` describes what an exported
dataset contains — including the fact that under the default mode its values
are placeholders, not yours. The app renders the same vocabulary at
`/transparency`.

## Excel compatibility

The goal is drop-in behavioral compatibility, pursued as a measured score
rather than a claim. `crates/parity` runs the engine against a corpus of cases
and regenerates [`PARITY.md`](PARITY.md) in CI: function coverage, cell-match
rate, and round-trip fidelity. Anything not yet implemented fails loudly
(`#NAME?`, an import warning) and round-trips losslessly — opening and saving
a file never destroys features Gridline does not yet understand.

Every case in the corpus cites where its expectation comes from, and cases
nobody here can settle are excluded from the score and listed as open
questions instead. Excel does not run on the machines this is developed on, so
no expectation was recorded by running it; `PARITY.md` says so at the top,
because a score whose provenance is vague is worse than no score.

```sh
make parity   # measure and regenerate PARITY.md
```

No Microsoft UI assets, icons, or branding are used or reproduced.
