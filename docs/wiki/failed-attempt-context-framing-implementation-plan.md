---
title: "Failed Attempt Context Framing Implementation Plan"
tags: ["task-center", "context-engine", "planner", "retry", "failed-attempt", "implementation-plan"]
created: 2026-05-14T00:00:00.000Z
updated: 2026-05-14T00:00:00.000Z
sources: ["role-context-ecommerce-example.md", "role-context-next-phase-report.md"]
links: ["role-context-ecommerce-example.md", "role-context-next-phase-report.md", "context-engine-recipes.md", "role-planner.md"]
category: architecture
confidence: high
schemaVersion: 1
---

# Failed Attempt Context Framing Implementation Plan

## Goal

Replace the retry planner's failed-attempt projection with a smoother
post-attempt frame:

```md
# Prior Failed Attempts

## Attempt 1

### Accepted Plan

Plan type: full | partial | unsubmitted

Specification:
<attempt.task_specification or "(not submitted)">

### Generator Outcomes

Status summary:
- gen-a: done
- gen-b: failed
- gen-c: blocked by gen-b

#### gen-a

<latest generator summary>

#### gen-b

<latest generator summary>

### Evaluator Judgment

Evaluation criteria:
- <accepted criterion>

Evaluator summary:
<latest evaluator summary>
```

## Rendering Rules

1. Rename the group heading from `# Failed Attempts` to
   `# Prior Failed Attempts`.
2. Rename each failed attempt body from raw field names to three framed sections:
   `Accepted Plan`, `Generator Outcomes`, and `Evaluator Judgment`.
3. Render `Plan type` and `Specification` in `Accepted Plan`.
4. Drop `continuation_goal` from the retry projection by default. Retry planning
   should focus on accepted scope and failed outcomes, not deferred future work.
5. Render every planned generator task in `Status summary`, including status and
   blocker information for blocked tasks.
6. Render detailed generator subsections only for tasks with useful stored
   summaries. Blocked tasks with only `blocked_by` metadata stay in the status
   summary and do not get a `####` subsection.
7. Hide `Evaluator Judgment` when one or more generator tasks failed or were
   blocked, because the evaluator did not judge a coherent generator result.
8. Show `Evaluator Judgment` only when an evaluator task exists and generator
   outcomes are all successful. The section includes accepted evaluation criteria
   and the evaluator's latest summary.
9. Do not render a separate failure-reason section. The failure should be visible
   through generator statuses or evaluator judgment.

## Patch Scope

This implementation changes the context-engine recipe and renderer defaults, not
the attempt lifecycle. The attempt still closes once with
`generator_failed`, `evaluator_failed`, `planner_failed`, or `startup_failed`;
the retry planner simply receives a clearer projection of the closed attempt.

## Implementation Tasks

1. Update `task_center.context_engine.recipes.attempt_landscape` to build the new
   Markdown body.
2. Add helper logic for generator task status, blocker extraction, and useful
   summary detection.
3. Change failed-attempt group metadata and renderer defaults to
   `# Prior Failed Attempts`.
4. Update focused context-engine tests for:
   - plan-type/specification framing,
   - blocked task status without a detail subsection,
   - hidden evaluator section on generator failure,
   - evaluator criteria and evaluator summary on evaluator failure,
   - all failed attempts still rendering in sequence order.
5. Update the role-context e-commerce example and next-phase report so the docs
   match the new retry planner context.

## Acceptance Criteria

- Retry planner context contains `# Prior Failed Attempts`.
- Each failed attempt renders `### Accepted Plan` and
  `### Generator Outcomes`.
- Status summary includes task status and `blocked by <task>` when available.
- Blocked tasks do not get synthetic detail sections.
- Evaluator judgment is absent for generator-failed attempts.
- Evaluator judgment is present for evaluator-failed attempts and includes
  accepted criteria plus the evaluator summary.
- No `fail_reason:` section is rendered in failed-attempt context.
