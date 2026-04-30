# Task Center Harness Migration - Phase Index

This migration is split into sequential implementation documents. Read and
implement them in order; each phase leaves a runnable intermediate state for
the next phase to build on.

## Phase documents

1. [Phase 00 - Target architecture](task-center-harness-migration/phase-00-target-architecture.md)
2. [Phase 01 - Graph and attempt model](task-center-harness-migration/phase-01-graph-and-attempt-model.md)
3. [Phase 02 - Local orchestrator lifecycle](task-center-harness-migration/phase-02-local-orchestrator-lifecycle.md)
4. [Phase 03 - Agent roles and tool gates](task-center-harness-migration/phase-03-agent-roles-and-tool-gates.md)
5. [Phase 04 - Vertical graph spawning and bubble-up](task-center-harness-migration/phase-04-vertical-spawning-and-bubble-up.md)
6. [Phase 05 - End-to-end workflows and cutover](task-center-harness-migration/phase-05-workflows-and-cutover.md)

## Implementation order

The dependency order is intentional:

1. Establish the target mental model before changing code.
2. Add durable graph and attempt state before orchestration uses it.
3. Move lifecycle decisions into local graph orchestrators.
4. Enforce terminal-tool policy against the new state model.
5. Add child-graph spawning and close-report routing.
6. Validate complete workflows, migrate callers, and remove obsolete paths.

## Scope

The migration reshapes the harness around two progression axes:

- Vertical: nested `HarnessGraph`s for delegation and partial-plan
  continuation.
- Horizontal: graph-local `Attempt`s for retry within a single scoped goal.

The detailed context-composition system is intentionally out of scope except
where these phase documents name the boundary.
