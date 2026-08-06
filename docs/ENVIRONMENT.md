# The agent-training environment

`crates/env` turns the spreadsheet into something a policy can be trained and
evaluated against. It wraps `engine::Engine` behind four deterministic
methods and adds the machinery around them that makes a recorded episode a
fact rather than a claim.

```
reset(snapshot_id)      put the world back exactly as it was
observe()               a bounded summary of what is there
step(Action)            take one action, report what it did
grade(TaskSpec)         decide whether the task was accomplished
```

The engine was already the right shape for this: every state change goes
through `Engine::apply(Action) -> Vec<Event>`, and replaying a log from an
empty workbook reproduces the exact final state. The environment wraps that
rather than reaching past it — `step` takes the same `Action` the toolbar
dispatches, so it inherits undo, replay and reference rewriting for free, and
there is no second door into the model.

## The invariants

**Two episodes reset to the same snapshot id are bit-identical.** A snapshot
is content-addressed — SHA-256 over canonically sorted JSON — and loading one
recalculates the workbook, so a stale cached value in a file written by an
older build cannot leak into a run.

**A trajectory replays.** Given its `initial_snapshot` and its actions,
running them again reproduces every per-step state hash. `gridline-env
validate` checks this over a whole dataset and is wired into `make ci`. A
trajectory that stops replaying is either corrupt or the engine moved
underneath it, and either way it must stop being training data before it
teaches something no longer true.

**The grader reads the workbook, never an observation.** If `observe`
summarizes something wrongly, the policy is misled and scores worse; the
score itself stays correct. Any other arrangement lets a summarization bug
silently inflate results.

**A generated variant is replayed and graded before it is kept.** The
perturbation generator is heuristic and is allowed to be. The gate is what
makes that safe, and rejections are reported rather than swallowed.

## What an observation contains

Sheet extents and formula counts; detected tables with their headers, column
types and a confidence; formulas grouped by R1C1 shape rather than listed per
cell; a dependency summary; the selection; visible errors; and what the last
step changed. Every list has a cap and every cap reports the real total
beside the capped list — a policy that cannot tell "these are all the errors"
from "these are the first 32" will believe it finished.

Nothing here is ground truth, and `TableView::confidence` exists because
"where is the table" has no correct answer on an arbitrary sheet.

## Checks a task can make

| check | asks |
| --- | --- |
| `cell_displays` | a cell shows exactly this string |
| `cell_number` | a cell holds this number, within a tolerance |
| `cell_formula` | a cell's formula has this *relative shape* |
| `range_filled` | every cell holds the first one's formula, shifted |
| `sums_match` | two ranges sum to the same thing — debits equal credits |
| `sum_equals` | a range sums to this |
| `no_errors` | nothing in the range is an error |
| `unchanged` | nothing in these ranges differs from the start |
| `sheets_exist` | these sheets are still here, under these names |
| `name_refers_to` | a defined name points here |

Shape comparison is what lets "put `=B2*C2` in D2 and fill down" be one check
rather than four hundred — and it is what catches the two ways of faking a
fill: pasted literals hold no formula, and `=B2*C2` repeated in every row
holds one but does not shift.

Every grade also reports `incidental_changes`: cells that changed and no
check asked about. A policy that completes more tasks while quietly rewriting
cells nobody asked about is worse than one that completes fewer, and average
reward cannot see that.

Without an explicit tolerance, numbers are compared by the engine's
fifteen-significant-digit rule rather than for exact equality. `0.1 + 0.2` is
`0.30000000000000004`, and a grader that failed that would teach a policy to
avoid correct answers.

## Perturbations

| perturbation | what it changes |
| --- | --- |
| `insert` | rows or columns — moves the table, or drops an irrelevant column into it |
| `rename_sheet` | the sheet name, in the workbook, the actions and the checks |
| `append_rows` | repeats the last row of data, and extends the demonstration's filled columns |
| `scale_literals` | multiplies the numeric inputs, so the right answer changes |

Each is expressed as engine actions plus a matching address remap. Inserting
two rows is `Action::RowInsert`, so every formula in the workbook is rewritten
by the same code copy, paste and fill go through; writing a second
implementation is how generated data comes to disagree with the product.

Recipes compose perturbations, and the combinations are where the value is: a
policy can memorise "the table starts at A1" or "the sheet is called Sheet1"
and still fail when both are false at once.

### Recorded limitations

* `append_rows` does not move anything that sat *below* the data. Telling "a
  footer under the table" from "an empty cell inside it" is the same guess
  table detection makes, and guessing wrong would silently relocate an
  output. A demonstration that writes a grand-total row is therefore rejected
  by the gate rather than mangled — which is why `make dataset` reports three
  rejections and should.
* A perturbation that changes the inputs changes the right answer, so
  value-based expectations (`cell_number`, `cell_displays`, `sum_equals`) are
  recomputed from the replayed demonstration. Those checks stop being
  independent evidence — the demonstration is being trusted, which is why only
  a *validated* one is ever augmented. The structural checks are what still
  have teeth, and a variant left with nothing but recomputed expectations is
  refused.
* `insert` does not know how to move a defined name's `refers_to` or a
  conditional rule's range. Those recipes are rejected by the gate rather
  than guessed at.

## The pipeline

```sh
make dataset          # regenerate ./dataset from ./corpus/env
make ci               # ...among other things, re-validates the committed one
```

or by hand:

```sh
gridline-env put      --store S corpus/env/ledger.start.jsonl
gridline-env record   --store S --tasks t.jsonl --actions a.jsonl --out demos.jsonl
gridline-env augment  --store S --tasks t.jsonl --dataset demos.jsonl \
                      --recipes corpus/env/recipes.json --out variants.jsonl
gridline-env validate --store S --dataset variants.jsonl
```

`gridline-env show --store S <snapshot-id>` prints an observation, and
`gridline-env grade --store S --tasks t.jsonl` grades a task from the shell —
run with no `--actions` it grades the untouched starting state, which is how a
task whose checks already pass before anything happens gets caught before it
reaches the corpus.

Snapshots live beside the trajectories and are referenced by hash, so a
trajectory never carries a workbook inline and thousands of variants of one
task share a handful of stored workbooks.
