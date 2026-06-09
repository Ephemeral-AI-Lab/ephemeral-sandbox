---
name: planner
description: Workflow scaffolding for planner work-item plans: scope bounding, direct dependency reasoning, and deferred-goal discipline.
---

# Planner Workflow

You design one attempt's worker plan. The plan is a DAG of `work_items`; each work item has an id, a worker-capable `agent_name`, an executable `work_spec`, and optional direct `needs`.

## Bound The Scope

1. Re-read the current iteration goal.
2. Identify the deliverables this attempt can complete.
3. Convert each deliverable into one or more self-contained work items.

Use `deferred_goal_for_next_iteration` only when concrete current-iteration goal items are intentionally left for a later iteration.

## Work Items

- Each `work_spec` must be actionable without rereading the whole plan.
- Add a `needs` edge only when one work item requires another work item's outcome.
- A worker receives only direct dependency outcomes, not transitive ancestors.
- Independent items should be parallel siblings.
- The graph must be acyclic.

## Submission

Plain text is reasoning, not a plan. Commit the plan only by calling `submit_plan_outcome` once after advisor approval.
