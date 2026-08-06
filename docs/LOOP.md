# The improvement loop

The end state this is aimed at: employees generate validated demonstrations,
the system learns from those and from their corrections, policies are
evaluated against reproducible tasks, and only demonstrably safer and more
capable versions are deployed.

Four pieces, three of them built.

## 1. Corrections

The most valuable signal a working system produces is not its successes —
those are cheap to generate. It is the moment somebody undoes what the agent
did and does it differently. That moment contains the instruction, the state
the agent was in when it went wrong, what it did there, and what should have
been done instead.

A `Correction` pairs two trajectories over the same starting state and
locates where they part company:

| signal | means |
| --- | --- |
| `preview_edited` | changed the proposal before it ran — the cheapest correction and the most informative, because it is about the *plan* |
| `undone` | undid the agent's work and redid it |
| `output_repaired` | fixed the cells the agent wrote, in place |
| `rejected` | threw the result away |

The divergence is computed from **state hashes, not actions** — two different
action sequences can reach the same workbook, and the point worth learning
from is the first place the *states* differ. When the user repaired on top of
the agent's output rather than redoing it, the agent's whole run counts as
agreed and the record says so, rather than inferring it from a number.

From a correction, two datasets:

* **Supervised examples** — the repair, and only when the repair passes the
  grader. A user who undid the agent and then did something equally wrong is
  not a teacher.
* **Preference pairs** — the agent's actions as rejected, the user's as
  preferred, anchored *at the diverging state*. A preference over the part
  both sides agreed on is a preference about nothing. Each pair carries the
  grader's reason; an unauditable preference is how a dataset acquires
  somebody's bad afternoon as a training signal.

```sh
gridline-env distil --corrections c.jsonl \
  --out-supervised supervised.jsonl --out-preferences preferences.jsonl
```

It reports what it could not use rather than dropping it quietly: a capture
pipeline that silently discards most of what it sees looks exactly like one
that is working.

**Not built:** nothing in the product fires these captures. The record
format, the divergence computation and the distillation are done and tested
end to end against the real agent; the hooks in the UI that would notice a
user undoing the agent are not.

## 2. Evaluation

A corpus is versioned by the hash of its contents, not by a number somebody
has to remember to bump — because a version that stops being bumped lets two
incomparable scorecards compare fine.

Everything runs from immutable snapshots. The store an evaluation reads is
opened read-only: every committed step checkpoints, so a scoring run against
a directory-backed corpus would leave a trail of new files in it, and a
corpus that changes when you measure against it is not a corpus.

What is scored:

* task completion, **and completion per origin** — a policy that only wins on
  generated variants has learned the generator, not the job, and the
  aggregate hides that completely
* required outputs, check by check
* invariants — sums that must match, ranges that must stay clean
* forbidden-cell modifications, counted apart from ordinary failures because
  they are a different kind of wrong
* runs that touched *any* cell nobody asked about, alongside the total cell
  count: one run scribbling over four hundred cells and four hundred runs
  each moving one are different problems
* replans, refusals, and how many proposals each side of the router answered
* wall clock, recorded and deliberately excluded from promotion

```sh
gridline-agent evaluate --store dataset/snapshots \
  --tasks dataset/variant-tasks.jsonl --policy rules --out runs/rules.json
```

## 3. Promotion

Not a comparison of aggregate scores. Each of these holds the release on its
own, and every reason is reported — blocked for three reasons and fixed for
one is still blocked, and finding that out one round at a time is how a week
goes.

* the scorecards came from different corpus versions
* more runs touched cells nobody asked about
* more incidental cells, forbidden-cell violations, or invariant failures
* fewer tasks solved
* **any task that used to pass now fails** — the regression an aggregate
  cannot show: one task lost, two won, net positive, somebody's Monday broken
* any origin's pass count went down

And if nothing improved on any dimension, it is held too: a release nobody
can point at a reason for is a release nobody can roll back with a reason
either.

```sh
gridline-agent promote --incumbent runs/rules.json --candidate runs/memo.json
```

`make evaluate` runs the whole thing. On the corpus in this repository it
prints:

```
rules against corpus e10a3cdc3e987927   7/11 tasks, 15 planner calls, 4 replans
memo  against corpus e10a3cdc3e987927   7/11 tasks,  0 planner calls, 0 replans
PROMOTE memo over rules
  0 planner call(s), down from 15
  0 replan(s), down from 4
```

## 4. Checkpoint and resume

Every committed plan step is followed by a content-addressed checkpoint.
`run_from(env, planner, task, config, Some(&checkpoint))` picks up there.

Two things a resumed run does not inherit, on purpose:

* **The plan.** The planner is asked afresh against the state it finds. A
  plan is intent formed from an observation, and the observation has moved
  on; replaying the rest of a stale plan is the fixed-coordinate macro replay
  this whole design avoids.
* **The trajectory.** The resumed run records its own from the checkpoint.
  Stitching two recordings together would produce state hashes that are real
  and an action list that never happened in one sitting, and it would not
  replay.

A test asserts a resumed run reaches the same final workbook hash as an
uninterrupted one, and another asserts it keeps the task's step budget —
which it did not, at first, and the only thing that noticed was the corpus
score three commits later.

## What is left

* **A model planner.** The interface, the routing, the confidence contract,
  the corrections format and the evaluation are built; what plugs into them
  is not.
* **The capture triggers.** See above.
* **Training.** This produces supervised examples and preference pairs in a
  documented format. Nothing here trains anything.
* **A human-intervention count.** The scorecard has a slot for it in spirit —
  replans and refusals are the automated analogue — but a real count needs
  the capture triggers first.
