# Gridline Event Schema

This document is the contract for everything Gridline captures. It is shipped
with the app and rendered by the in-product transparency page at
`/transparency`, so the vocabulary users read there is the vocabulary defined
here.

Two principles govern every entry below:

1. **Semantic, never raw.** Events describe intent — "pasted A2:A40 into
   B2:B40 with relative references adjusted" — never keystrokes, mouse
   coordinates, or screen content.
2. **Nothing without consent.** No event leaves the browser until a consent
   record exists, and capture can be paused at any moment from the status chip
   in the toolbar.

## Envelope

Every event is a JSON object with this shape (`schema_version` 1):

```json
{
  "schema_version": 1,
  "event_id": "01J8Z9XKQ4V7B2M3N5P6R7S8T9",
  "session_id": "01J8Z8W3H1K2L4M5N6P7Q8R9S0",
  "actor_id": "u_7f3a",
  "workbook_id": "wb_19c2",
  "seq": 4172,
  "ts_ms": 1767225600123,
  "action": "range.paste",
  "payload": {
    "source": "Sheet1!A2:A40",
    "target": "Sheet1!B2:B40",
    "mode": "formulas",
    "ref_adjust": "relative"
  },
  "context": {
    "sheet": "Sheet1",
    "selection": "B2:B40",
    "privacy_mode": "structural"
  },
  "client_version": "0.1.0"
}
```

| Field | Meaning |
| --- | --- |
| `schema_version` | Envelope version. Bumped only for breaking changes. |
| `event_id` | ULID. The server deduplicates on this, so delivery is at-least-once and safe to retry. |
| `session_id` | ULID for the capture session. Sessions close after 10 minutes of actor inactivity, decided server-side. |
| `actor_id` | Opaque per-user id. Not an email address or name. |
| `workbook_id` | Opaque workbook id. |
| `seq` | Monotonic per session; lets the server order events that arrive out of order. |
| `ts_ms` | Client wall clock, milliseconds since the Unix epoch. |
| `action` | One of the vocabulary entries below. |
| `payload` | Action-specific fields, documented per action. |
| `context` | Sheet, current selection, and the privacy mode in force when captured. Under `structural` the sheet name is replaced by its salted hash, exactly as payload sheet names are — it would otherwise be disclosed on every single event. A sheet name written *inside a formula* is not hashed, because the formula is recorded verbatim; see `PRIVACY.md`. |
| `client_version` | Version of the web app that produced the event. |

## Privacy modes

The mode is chosen at consent time and can be changed later in settings. It is
recorded on every event, so a log can always be read back knowing exactly what
it contains.

| Mode | What is captured |
| --- | --- |
| `full` | Payloads include entered values and formulas verbatim. |
| `structural` (default) | Formulas are kept verbatim, because their structure is the point. Literal cell **values** are replaced by `{ "hash": "<first 16 hex chars of a salted SHA-256>", "type": "number\|text\|bool", "len": 7 }`, salted per workbook. Structure, references, and the shape of every action are fully preserved. |
| `off` | Nothing is captured and nothing is transmitted. The status chip reads "capture off". |

Under `structural`, a cell edit of `12345` records the hash, the type
`number`, and the length `5` — enough to mine repetition, not enough to
recover the number. The salt is per workbook and never leaves the server.

The same rule applies wherever a name appears, including `context.sheet`.
Hashing a value in one field while sending it in clear in another would be
worse than not hashing it: the value leaks anyway, and the pair reveals that
value's hash under this workbook's salt, unpicking every other occurrence.

## Action vocabulary

Every variant of the engine's `Action` enum maps to exactly one action name.
The engine is the only thing that can mutate a workbook, so this list is
complete by construction.

### Cell and range editing

| Action | Payload | Notes |
| --- | --- | --- |
| `cell.edit` | `addr`, `input`, `prev_input` | `input` is the formula-bar text. Literal values are hashed under `structural`; formulas are kept verbatim. |
| `cell.clear` | `addr`, `prev_input` | |
| `range.copy` | `source` | Clipboard read; no state change. |
| `range.cut` | `source` | |
| `range.paste` | `source`, `target`, `mode` (`formulas` \| `values`), `ref_adjust` (`relative` \| `moved`), `cut` | A cut/paste also retargets references elsewhere that pointed into the moved block. |
| `fill.apply` | `source`, `target`, `filled` | Fill handle, Ctrl+D, Ctrl+R. |

### Structure

| Action | Payload | Notes |
| --- | --- | --- |
| `row.insert` | `at`, `count` | |
| `row.delete` | `at`, `count` | References to deleted rows become `#REF!`. |
| `col.insert` | `at`, `count` | |
| `col.delete` | `at`, `count` | |
| `row.resize` | `at`, `count`, `size`, `kind` | `size` is pixels, or absent when the run went back to the default; `kind` is `set` or `default`. Presentation, so nothing here is hashed. |
| `col.resize` | `at`, `count`, `size`, `kind` | As above, for column widths. |
| `sort.apply` | `range`, `keys` (column + direction), `has_header` | |
| `filter.apply` | `range`, `column`, `hidden` | Value filters hide rows; no cell values change. Allowed-value lists are hashed under `structural`. |
| `filter.clear` | `sheet` | |
| `sheet.add` | `name` | Sheet names are hashed under `structural`. |
| `sheet.rename` | `from`, `to` | |
| `sheet.delete` | `name` | |
| `name.define` | `name`, `refers_to` | A workbook-level defined name. The name is hashed under `structural`; where it points is structure, not content. |
| `name.delete` | `name` | |
| `panes.freeze` | `rows`, `cols`, `kind` | How many rows and columns are held still while the rest scrolls; `kind` is `freeze` or `unfreeze`. Layout, so nothing here is hashed. |
| `format.apply` | `range`, `cells`, `kind`, `attributes`, `patches` | Bold, italic, colours, borders, number format, alignment, merge/unmerge. `kind` is `style`, `clear`, `merge` or `unmerge`. `attributes` names the properties changed; `patches` carries their values. A colour or a format code describes presentation rather than content, so neither is hashed. |
| `find.replace` | `scope`, `range`, `find`, `replace`, `match_case`, `whole_cell` | Search and replacement terms are hashed under `structural`. |

### Files

| Action | Payload | Notes |
| --- | --- | --- |
| `file.new` | — | |
| `file.open` | `workbook_id` | |
| `file.import` | `format` (`xlsx` \| `csv`), `sheets`, `cells`, `warnings` | Warning *categories* only — never file contents or names under `structural`. |
| `file.export` | `format`, `sheets` | |
| `file.save` | `workbook_id` | |

### Navigation and history

| Action | Payload | Notes |
| --- | --- | --- |
| `nav.select` | `selection` | **Sampled.** Bursts are coalesced to at most 2 events per second, because raw selection changes are noise, not intent. |
| `undo` | `label` | |
| `redo` | `label` | |

### Automation and capture control

| Action | Payload | Notes |
| --- | --- | --- |
| `routine.run` | `routine_id`, `iterations`, `cells_changed` | Emitted for every action a replayed routine performs, tagged so mined behavior is never confused with human behavior. |
| `capture.pause` | — | |
| `capture.resume` | — | |
| `consent.granted` | `mode`, `consent_text_version` | |
| `consent.revoked` | — | Also stops any further capture immediately. |

## What is never captured

- Keystrokes, key timings, mouse positions, scroll offsets, or screenshots.
- Clipboard contents from outside Gridline.
- Anything at all while the mode is `off` or capture is paused.
- Data from any user who has not accepted the consent notice. Exports refuse
  to include an actor whose consent is currently `off` or revoked.

## Delivery

The client keeps a ring buffer and flushes every 5 seconds or 200 events,
whichever comes first, to `POST /v1/events`. Undelivered batches queue in
memory and IndexedDB so a closed laptop or a dropped connection does not lose
or duplicate events: delivery is at-least-once and the server deduplicates on
`event_id`. Capture never blocks or slows the grid.
