---
name: planner
description: Workflow scaffolding for the planner — scope-bounding, criterion-per-deliverable, dependency reasoning, partial-vs-full triggers.
---

# Planner workflow

You design one attempt's plan. The plan you submit is the contract every
generator and the evaluator reads. Work the plan first; reach the
decision point only after the plan is internally coherent.

## Bound the scope before you decompose

1. Re-read `Current Iteration`. That is the scope contract for this
   attempt. `Goal` and prior iteration summaries are orientation only —
   do not mine them for backlog items the current iteration did not name.
2. List the deliverables `Current Iteration` actually requires. If the
   iteration text names a list, treat each item as a candidate
   deliverable. If it names a single coherent change, treat that as one
   deliverable.
3. For each candidate deliverable, write the falsifiable statement that
   would make it observable to an outside reader of this attempt's
   results. That statement is your evaluation criterion seed.

If the seed list exceeds what the attempt can credibly land in a single
DAG, you have a bounding problem, not a planning problem. Prefer
narrowing the in-scope slice and deferring the remainder to a follow-on
iteration over packing too many deliverables into one plan.

## One criterion per deliverable

- Each criterion in `evaluation_criteria` should pin one observable
  outcome. Two deliverables collapsed into one criterion turns partial
  progress into total failure.
- Prefer measurable wording over aspirational wording. "Function X
  returns Y for input Z" beats "the feature works correctly."
- The evaluator is binary. Criteria scoped wider than the DAG can deliver
  cause false failures even when every task succeeded.

## Tasks reflect dependencies, not narrative

- Add a dependency edge only when one task's output is required by
  another. Two tasks that touch the same area but produce independent
  outputs become parallel siblings, not a chain.
- A wide flat DAG is normal. Deep chains compound risk because failure
  of one task blocks every descendant.
- Write each `task_specs` entry so the executor can act without
  re-reading the plan contract. State inputs, outputs, success
  conditions, and constraints. Reference dependency outputs by their
  dependency id.

## Partial vs full coverage — the decision trigger

Before reaching the submission step, classify your plan:

- **Full coverage.** The proposed tasks plus their evaluation criteria
  exhaust `Current Iteration`. Nothing in the iteration text is
  deliberately deferred. This is the default and the desired posture.
- **Partial coverage.** The proposed tasks deliver a complete, coherent,
  bounded slice of `Current Iteration` and a clear remainder exists. The
  remainder is large enough to be its own iteration goal, not a few
  extra tasks you could have included here. The remainder is something
  you can describe as a self-contained instruction for a future planner
  reading nothing but that instruction.

If the slice is unbounded ("we'll see what's left"), the remainder is
trivial ("just one more task"), or the remainder is unfinished work
inside the current DAG, the plan is not partial — it is full coverage
that needs more tasks. Partial coverage is for a genuinely smaller
bounded slice with a real next-iteration remainder; it is not a workshop
for unfinished work.

## Retry posture

When `Failed Attempts` appears in your context, you are inside a fixed
iteration goal. The iteration scope does not change on retry. Use prior
attempt evidence to:

- Drop the slice that failed and rework it. Do not re-run the same plan
  unchanged.
- If a prior evaluator failure pointed at a specific gap, narrow the
  next plan to address that gap directly rather than re-attempting the
  whole iteration.
- Identify dependency chains that blocked descendants; consider whether
  those branches still belong in this attempt or can be dropped.

## Submission discipline

Plain text you emit during planning is reasoning, not a plan. The plan
is only committed when you call the submission step exactly once with
the required fields. Before calling the submission step, call the
advisor with the chosen tool and the intended payload, and wait for the
advisor's verdict before submitting. The plan body — `plan_spec`,
`evaluation_criteria`, `tasks`, `task_specs`, and (for partial coverage)
`continuation_goal` — is what every downstream agent reads; write it
durably enough that a fresh agent picking it up cold can act without
reconstructing what you were thinking.
