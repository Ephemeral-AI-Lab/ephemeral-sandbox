# Task Center Harness Migration - Phase Index

This migration is split into sequential implementation documents. Read and
implement them in order; each phase leaves a runnable intermediate state for
the next phase to build on.

## Phase documents

1. [Phase 00 - Target architecture](task-center-harness-migration/phase-00-target-architecture.md)
2. [Phase 01 - Complex task request and harness graph model](task-center-harness-migration/phase-01-graph-and-attempt-model.md)
3. [Phase 02 - Harness graph orchestrator lifecycle](task-center-harness-migration/phase-02-harness-graph-orchestrator-lifecycle.md)
4. [Phase 03 - Agent roles and tool gates](task-center-harness-migration/phase-03-agent-roles-and-tool-gates.md)
5. [Phase 04 - Complex task spawning and handoff](task-center-harness-migration/phase-04-complex-task-spawning-and-handoff.md)
6. [Phase 05 - End-to-end workflows and cutover](task-center-harness-migration/phase-05-workflows-and-cutover.md)
7. [Phase 06 - Context engine](task-center-harness-migration/phase-06-context-engine.md)

## Overview documents

- [Complex task segmentation and harness graph workflow](task-center-harness-migration/complex-task-workflow-overview.md)

## Implementation order

The dependency order is intentional:

1. Establish the target mental model before changing code.
2. Add durable complex-task, segment, and harness-graph state plus
   `ComplexTaskRequestHandler` and `TaskSegmentManager` before graph execution
   uses it.
3. Move planner/generator/evaluator lifecycle decisions into
   `HarnessGraphOrchestrator`.
4. Enforce role and terminal-tool policy against the new state model.
5. Add complex-task request spawning and final report delivery.
6. Validate complete workflows, migrate callers, and remove obsolete paths.
7. Add role-specific context composition, durable summaries, and close-report
   payloads on top of the migrated lifecycle model.

## Scope

The migration reshapes the harness around two context axes:

- Request origin: a `ComplexTaskRequest` is created when an executor calls
  `request_complex_task_solution(goal)`.
- Segment retry policy: a single `TaskSegment` owns retry budget for the
  request. `HarnessGraph`s are planner-produced DAG executions inside that
  segment. A failed graph returns to `TaskSegmentManager`, which decides whether
  to spend retry budget by launching another `HarnessGraph` in the same segment.

The detailed context-composition system is specified separately in Phase 06.
