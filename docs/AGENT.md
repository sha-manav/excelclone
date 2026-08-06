# The agent

`crates/agent` sits on top of the environment. A **planner** decides *what* to
do; a **compiler** decides *where*. The planner sees a summarised observation
and returns typed steps naming headers and patterns; the compiler resolves
those against the workbook actually in front of it and emits
`engine::Action` values.

Splitting them is not tidiness. It buys three things:

* **A plan can be read before it runs.** `CreateDerivedColumn { header:
  "Total", formula: "={Qty}*{Price}" }` says what it will touch. A string of
  code does not.
* **A plan survives the sheet moving.** Nothing in it is an address, so
  inserting a row above the table does not invalidate it.
* **A failure is attributable** to one half or the other, and they are
  separately testable.

The cost, stated plainly: a planner can only say what `plan::Step` can
express. Anything else has to be added there first, deliberately, with a
compiler and a validator to match. That is the trade — a narrower agent that
can be reasoned about, over a general one that cannot.

## The vocabulary

| step | means |
| --- | --- |
| `locate_table` | find the table with these headers; everything after is about it |
| `create_derived_column` | add a column computed from others, over the body rows |
| `apply_formula` | one formula in one place — a grand total, a reconciliation |
| `fill_range` | continue a formula somebody already started |
| `filter_rows` | hide the rows that do not match a predicate |
| `reconcile_totals` | assert two columns agree, optionally recording the variance |
| `export_workbook` | say the task is done |

There is deliberately no `RunCode`, no `Eval`, and no `ClickAt`.

Columns are named by header, by defined name, or as "the next free one" —
never by index. Formulas are templates over headers: `={Qty}*{Price}`
compiles to `=B2*C2` on one sheet and `=E7*G7` on another, and the plan is
the same plan. A placeholder means *this row's cell* inside the table and
*the whole column* outside it, which is the difference between "each row
times its price" and "the total of the column".

`export_workbook` compiles to no actions. It exists so that finishing is
something the planner *declares* rather than something inferred from it
running out of ideas — which is the difference between `Termination::Done`
and `Termination::BudgetExhausted`, and they mean different things.

## The loop

Four layers, each catching something the others cannot:

1. **Compile.** A step naming a column that is not there fails here, before
   anything is touched. The error goes back as an instruction — "there is no
   column headed \"Quantity\"; the ones here are \"Item\", \"Qty\",
   \"Price\"" — not as a diagnostic code.
2. **Validate.** Writes outside the declared scope, unrequested structural
   changes, formulas with unresolvable references, literals replacing
   formulas, and changes over the cell limit all fail here. Still untouched.
3. **Rehearse on a clone.** The actions run against a copy and the result is
   inspected. This is what catches what static checks cannot: a step that
   changes nothing, a formula that parses and validates and evaluates to
   `#VALUE!`, an effect landing outside the declared range by a route the
   action list did not show.
4. **Commit.** Only now does the real environment see it, through
   `Env::step`, so the trajectory records it and the whole episode replays.

A failure at any layer becomes *feedback* and the planner is asked again —
that is the difference between replanning and retrying. Steps after a failed
one are abandoned rather than run anyway: they were written assuming it
succeeded, and running them is how an agent digs a hole.

Every committed step is followed by a checkpoint — a content-addressed
snapshot id — so a long task resumes at a step boundary.

## The validator

| refusal | catches |
| --- | --- |
| `outside_scope` | did the job *and* touched something else |
| `not_permitted` | inserted, deleted or renamed something the task did not ask for |
| `invalid_reference` | a `#REF!`, an unknown sheet, a cell reading itself |
| `overwrites_formula` | computed the answer and pasted the number |
| `too_many_cells` | more blast radius than the task allows |

None of these is a correctness check — the grader does that. What the
validator catches is the more dangerous class: work that is *plausible* and
destroys something.

The cell limit counts recalculation. Editing one input that four hundred
formulas read is a four-hundred-cell change, and an agent allowed to make it
because "it only wrote one cell" has been measured by the wrong number.

`allow_sheet_changes` is separate from `allow_structural` on purpose:
deleting a sheet is the most destructive thing in the vocabulary and nothing
should enable it by accident.

## Planners, and choosing between them

`Planner` takes an instruction plus an observation and returns a `Plan`. It
gets no `Engine`, no filesystem, and no way to return anything else — so "the
policy was misled by a bad summary" is a scoring problem rather than a safety
one.

Two implementations ship:

* **`RulePlanner`** — deterministic, free, and honestly limited: it
  recognises a handful of phrasings over a detected table. It exists so the
  interface, the compiler, the validator and the loop can be exercised end to
  end without a model, and so a model planner has something to be compared
  against. When the instruction names an operator but no columns this table
  has — "the quantity multiplied by the price" where the headers are `Qty`
  and `Price` — it infers the operands from the column types and *says it is
  guessing*, at a confidence below any sensible routing threshold. It refuses
  to guess the order of a subtraction, because `{a}-{b}` and `{b}-{a}` are
  different answers.
* **`MemoPlanner`** — answers from distilled micro-policies (below).

`Router` puts a cheap planner in front of an expensive one. It takes the
cheap answer only when its confidence clears a threshold, and — the rule that
matters more — **the moment anything has been refused, the expensive planner
takes over.** A cheap policy allowed to retry after a refusal proposes
variations of the same mistake until the budget is gone, looks busy, and
produces nothing.

## Micro-policies

A successful plan is remembered only if it passed *and* changed nothing
outside what the task asked about. Promoting a careless success turns one
accident into a policy that is careless every time.

Plans are clustered by shape — `create_derived_column:={0}*{1} ->
export_workbook` is one habit whether the columns were Qty and Price or Hours
and Rate — and each cluster becomes a `MicroPolicy`: a representative plan,
plus per placeholder the header names that slot has been bound to and its
column type.

Instantiating one against a new workbook binds the slots. Binding by a name
seen before is nearly a fact (confidence 0.9); binding by "it is the only
numeric column left" is a guess (0.5), which a router sends somewhere better.
A policy that could not tell those apart would be a fast way to be wrong.

```sh
gridline-agent solve --store dataset/snapshots \
  --tasks dataset/variant-tasks.jsonl --memory plans.jsonl
```

Run it twice. On the corpus in this repository the first pass scores 7/11
using 15 planner calls and 4 replans; the second scores 7/11 using **zero**
planner calls and zero replans, every proposal answered by one distilled
policy with support 7. Cheaper at the same score is the whole point; cheaper
and *worse* would be a regression with a budget line, and there is a test
asserting it does not happen.

## What it does not do

* **The rule planner does not solve the two-step tasks.** `ledger-grand-total`
  — fill a column, then total it underneath — needs a plan the rule planner
  has no phrasing for, and it fails all four variants of it. That gap is the
  shape of what a model planner is for, and it is left visible rather than
  papered over by teaching the rule planner one more sentence.
* **A micro-policy can declare a task done that it only partly solved.**
  Nothing in the loop catches this, because nothing was refused — the plan
  was valid, just incomplete. Only the grader notices, after the fact. That
  is what an evaluation corpus is for and it is not yet mitigated in the
  loop.
* **Table detection is a heuristic** and the loop refuses to act on one below
  a confidence threshold. It will still occasionally be confidently wrong
  about where a table ends.
* **There is no model planner here.** The interface, the routing, the
  confidence contract and the evaluation are built; what plugs into them is
  not.
