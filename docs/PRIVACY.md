# Privacy

Gridline records what you do in the app so it can find repetitive work and
offer to automate it. This document explains exactly what that means, in
plain terms. The machine-readable contract is `EVENTS.md`; this is the same
information written for a person deciding whether to say yes.

## The short version

- Capture is **off until you turn it on.** Nothing is recorded or sent before
  you accept the consent notice.
- The default mode, **structural**, records the *shape* of your work and
  replaces the literal values you type with salted hashes.
- Capture state is **always visible** in the toolbar, and clicking it pauses
  capture instantly.
- Data goes **only to the Gridline server**. There are no third-party
  analytics or telemetry SDKs in this application, of any kind.
- You can **revoke consent** at any time. Revoking stops capture immediately
  and excludes you from every dataset export from that moment on.

## What is recorded

Gridline records *actions*, not input. An action is a thing you did to the
workbook: edited a cell, pasted a range, sorted a table, inserted rows. Each
one is recorded as structured data — which cells, what kind of operation,
when.

Gridline does **not** record keystrokes, key timings, mouse movement, scroll
position, screenshots, or the contents of your screen. There is no screen
recorder and no keylogger. The application records what it was asked to do,
because it is the thing being asked.

Selection changes (clicking around) are sampled rather than recorded in full:
bursts are coalesced to at most two events per second, because moving a cursor
is not intent.

## The three modes

You choose a mode when you accept, and can change it later in settings.

### `full`

Formulas and the values you type are recorded verbatim. Choose this only for
workbooks whose contents are not sensitive.

### `structural` — the default

Formulas are recorded verbatim, because a formula's structure is the entire
point of finding repeated work. **Verbatim means verbatim**: a formula carries
the text inside it, so `=IF(F7<=0,"settled","overdue")` records the words
`settled` and `overdue`, and `=VLOOKUP(B7,Rates!$A$1:$B$3,2,FALSE)` records the
sheet name `Rates`. A sheet name is hashed everywhere else — in the event's
context and in every payload field — but a reference inside a formula is part
of the formula. If a sheet name or a phrase in a formula is sensitive, the mode
to choose is `off`.

The mining pipeline does not see any of it: formulas are reduced to
position-free shapes with their text literals blanked and their sheet
qualifiers anonymised before a pattern is looked for. But the *log* holds the
formula as typed, and so does an exported dataset.

Literal values are **not** recorded. In their place Gridline stores:

- a salted SHA-256 hash, truncated to 16 hex characters,
- the type (`number`, `text`, or `bool`),
- the length.

So typing `48250` into a cell records that a 5-digit number was entered, and a
hash that matches other cells containing the same number — enough to notice
you copied the same value twice, not enough to recover the value. The salt is
per workbook, is generated on the server, and never leaves it, so hashes
cannot be compared across workbooks or attacked with a precomputed table.

### `off`

Nothing is captured and nothing is transmitted. The status chip reads
"capture off".

## What the data is used for

Two things, both stated up front:

1. **Finding routines.** A mining pass looks for action sequences you repeat —
   the same three steps on forty rows — and offers a one-click automation.
   You always see a preview of exactly which cells a routine would change
   before it runs, and you can dismiss any suggestion.
2. **Demonstration datasets.** Exported logs are intended as training data for
   future models: sequences of state and action, described in `DATASET.md`.
   Under `structural` capture the values in an exported dataset are *not
   yours* — the literals were never recorded, and the replay uses
   placeholders derived from their hashes. Every record says so.

Export refuses to include any user whose consent is currently `off` or has
been revoked. This is enforced in the exporter, not by convention, and it is
covered by a test.

## Control

| Control | Where | Effect |
| --- | --- | --- |
| Capture chip | Toolbar, always visible | Shows ● capturing, ‖ paused, or ○ off. Click to pause or resume immediately. |
| Mode | Settings | Switch between `full`, `structural`, and `off` at any time. |
| Transparency page | `/transparency` | Renders the live action vocabulary and your current mode, generated from the same document the code follows. |
| Revoke | Settings | Stops capture and excludes you from all future exports. |

## Retention and access

Events are stored in Gridline's own SQLite database on Gridline's own server.
The event log is append-only, so a routine's provenance can always be traced.
Export requires an admin token; ordinary user tokens cannot read anyone's
events, including their own account's raw log, through the export endpoint.

## Where this is enforced

Claims in this document map to code and tests:

- Consent gating on ingest — the server rejects events from a user whose mode
  is `off` (server tests).
- Export consent filtering — `miner export --consented-only` excludes actors
  with no consent, with `off`, and with a revocation, by a `WHERE` clause
  rather than a filter over the results (miner unit tests, plus a subprocess
  test that drives the built binary against a real database).
- Pause actually stops network traffic — the end-to-end suite asserts no
  requests to `/v1/events` are made while capture is paused.
- No third-party telemetry — the web app's dependency manifest contains no
  analytics SDK, and the only network destination in the client is the
  Gridline API.
