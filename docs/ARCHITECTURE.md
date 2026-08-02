# Gridline Architecture

## Overview

Gridline is a web spreadsheet with a Rust calculation engine whose defining feature is a
complete, consent-gated, semantic event log of user actions, plus a mining pipeline that
turns logs into replayable automations ("routines") and exportable demonstration data.

```
apps/web (React/TS, canvas grid)
   │  Action                    ▲ Events (render)
   ▼                            │
crates/wasm (wasm-bindgen bridge)
   │
crates/engine  Engine::apply(Action) -> Vec<Event>   ← single mutation path
   │
   ├── event log  ──►  client capture ──► POST /v1/events ──► crates/server (axum+SQLite)
   │                                                              │
   └── replay(log) == final state (flagship invariant)            ▼
                                                          crates/miner (normalize,
                                                          mine, synthesize routines)
```

## Principles

1. **Event-sourced core.** The UI never mutates state directly; every mutation flows
   through `Engine::apply(Action) -> Vec<Event>`. Replaying the log from an empty
   workbook reproduces the exact final state (tested, incl. property-based).
2. **Consent-first capture.** Nothing leaves the client before consent; capture state is
   always visible; pause always works. Privacy modes: `full`, `structural` (default),
   `off`.
3. **Semantic events.** Events describe intent, never raw input coordinates.
4. **Boring technology.** Pinned, well-maintained crates; no third-party telemetry SDKs.
5. **Excel compatibility is measured, not asserted** — see the Parity Track and
   `crates/parity` (post-M7).

## Crates

- `crates/engine` — pure library, no I/O. Workbook model (sparse cell store, dependency
  graph, incremental recalc), formula parser/evaluator, actions/events, structural ops
  with reference rewriting, undo/redo as inverse actions, xlsx/csv de/serialization
  logic.
- `crates/wasm` (`gridline-wasm`) — wasm-bindgen bindings; ships to `apps/web` as an npm
  package built by wasm-pack.
- `crates/server` — axum + tokio + sqlx/SQLite. Token auth, batch event ingest
  (idempotent on `event_id`), consent records, workbook save/load, routines storage,
  server-side sessionization (10 min inactivity).
- `crates/miner` — CLI + server-callable library: normalize events to abstract tokens,
  detect tandem repeats and frequent sequences (PrefixSpan), score by estimated minutes
  saved, synthesize routines (JSON macros of typed engine Actions), export JSONL
  demonstration datasets (consent-enforced).

## Event flow

Client ring buffer → batch flush (5 s / 200 events) → `POST /v1/events` with offline
IndexedDB queue and at-least-once delivery; the server dedupes on `event_id`. See
`docs/EVENTS.md` for the envelope and action vocabulary.

## Testing strategy

- Determinism/replay suite (fixtures + proptest) — the most important tests in the repo.
- Per-function engine tests with Excel-verified expected values.
- Golden workbook tests (`fixtures/*.xlsx` → computed-value snapshots).
- Miner planted-pattern tests; server ingest/consent tests; Playwright E2E.
