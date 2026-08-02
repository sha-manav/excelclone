# The demonstration dataset

What `gridline-miner export` produces, what each field means, and — the part
that matters most — what the numbers in it are and are not.

```sh
gridline-miner export --consented-only --mode structural --out data/
```

## The shape

One directory, one JSONL file per session, plus a manifest naming exactly the
files that belong to the dataset:

```
data/
  0000-01HQ8Z9J5K2M3N4P5Q6R7S8T9V.jsonl
  0001-01HQC4Y7X1W0V9U8T7S6R5Q4P3.jsonl
  manifest.json
```

Each line is one action, with the state before it and the state after:

```json
{
  "pre_state_digest": "cd7cfbd0…",
  "context": {
    "session_id": "01HQ8Z9J5K2M3N4P5Q6R7S8T9V",
    "workbook_id": "wb_demo_dues_ledger",
    "seq": 41,
    "sheet": "a3f1c2…",
    "selection": "E19",
    "privacy_mode": "structural",
    "values_synthetic": true
  },
  "action": {
    "action": "cell_edit",
    "sheet": "a3f1c2…",
    "addr": { "row": 18, "col": 4 },
    "input": "=VLOOKUP(B19,Rates!$A$1:$B$3,2,FALSE)"
  },
  "post_state_digest": "21c63cc2…"
}
```

| Field | Meaning |
| --- | --- |
| `pre_state_digest` | SHA-256 of the workbook state before the action |
| `context.session_id` | The sitting this action belongs to |
| `context.seq` | Position within that sitting |
| `context.sheet` | The sheet the action landed on, hashed under `structural` |
| `context.selection` | Where the cursor was |
| `context.privacy_mode` | What was recorded at capture time |
| `context.values_synthetic` | Whether the literals in `action` are stand-ins |
| `action` | A typed engine `Action`, the same enum `Engine::apply` takes |
| `post_state_digest` | SHA-256 of the state after it |

Each record's `post_state_digest` is the next record's `pre_state_digest`, so a
session is a chain. A digest is over the deterministic state snapshot the
replay test suite compares, which means two records with the same digest
describe the same workbook state, by the same definition the rest of the
project uses.

Actions the engine refused are counted in the run report but produce no
record: their pre and post digests would be identical, and a reader would take
that to mean the action does nothing rather than that it was refused.

## What the values are

**Not the user's.** Under `structural` capture — the default — a literal the
user typed was never recorded; the log holds a salted hash of it. The replay
that produces these digests therefore runs against a *synthetic workbook with
the same shape*: every redacted literal becomes a placeholder derived
deterministically from its hash.

That preserves exactly one property of the original data, and it is the one
the dataset needs:

- Two cells that held the same value still hold the same value.
- Two cells that held different values still hold different values.
- A number is still a number, text is still text, a boolean is still a
  boolean.

So a lookup finds its match, a conditional aggregation counts the right rows,
and a formula's dependency structure is intact. What you cannot do is read a
placeholder as a fact about the person who typed it. `values_synthetic` is on
every record so nothing downstream has to guess.

Formulas are verbatim in every capture mode. Their structure is the entire
point of the exercise. Note what that includes: the text literals inside a
formula and the sheet names in its references travel with it. `=IF(F7<=0,
"settled","overdue")` carries both words, and `Rates!$A$1:$B$3` carries the
sheet name — even though the same sheet name is hashed in `context.sheet`.
`PRIVACY.md` says so where a user will read it.

## Consent

The events are selected by a `WHERE` clause, not by a filter applied to the
results — the same predicate the server's own export uses. An actor
contributes nothing if:

- they have no consent record at all, or
- their most recent consent is `off`, or
- their most recent consent has been revoked.

Revocation is retroactive: the events stay in the log so a routine's
provenance can still be traced, but nothing leaves the building with them.

`--consented-only` is **mandatory** for a database export rather than the
default. An operator who has to type it cannot later say they did not know the
export was filtered, and a flag that must be typed cannot be quietly dropped
from a copied script. It is *refused* with `--in`, because a JSONL log carries
no consent records and accepting the flag there would let a script claim a
check that never ran.

`--mode structural` narrows further, to actors whose recorded consent is
exactly that.

## Reproducibility

The same database exports to the same bytes. `scripts/demo.sh` builds a
complete example from nothing — a seeded history, a mined routine, a dataset,
and a check that the actor who declined contributed none of it.

## Where this is enforced

- `crates/miner/src/dataset.rs` — records, digests, placeholders (unit tests)
- `crates/miner/src/store.rs` — the consent query (unit tests against the
  server's own migration)
- `crates/miner/tests/dataset_export.rs` — the built binary, driven as a
  subprocess against a real database, including the refusals
