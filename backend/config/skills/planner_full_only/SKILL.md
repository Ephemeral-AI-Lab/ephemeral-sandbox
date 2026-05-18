---
name: planner_full_only
description: Workflow scaffolding for the depth-restricted planner — partial coverage is unavailable; bound the iteration slice and plan full coverage.
---

# Planner workflow (full-coverage only)

You design one attempt's plan inside a goal whose ancestry has already
spent its partial-coverage budget. The downstream submission step does
not include a partial-coverage option. Your only path is to plan an
attempt whose tasks fully cover `Current Iteration`. The workflow that
drives you to the decision point is the same as the unrestricted
planner; the one degree of freedom you lose is the ability to defer
remainder work to a follow-on iteration.

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

If the seed list exceeds what the attempt can credibly land in one DAG,
**narrow the slice inside `Current Iteration`'s bounds** and plan full
coverage of the narrowed slice. You do not control later iterations and
cannot defer remainder work here. Narrow `plan_spec` and
`evaluation_criteria` to a slice the planned DAG can satisfy; do not
write criteria the tasks cannot deliver.

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
`evaluation_criteria`, `tasks`, and `task_specs` — is what every
downstream agent reads; write it durably enough that a fresh agent
picking it up cold can act without reconstructing what you were thinking.
