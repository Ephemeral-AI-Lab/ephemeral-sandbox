# Non-Root Context Reuse

Use this reference only on child planning turns or prompts with `## Scoped Expansion`.

## Workflow

1. Must reuse inherited briefings, artifacts, and known owner boundaries before fresh exploration.
2. Must reuse inherited scout refs and shared briefings before consulting Atlas or opening a new scout.
3. Must spend at most one live confirmation step on the one unresolved owner when siblings are already mapped.
4. Must emit direct lanes for already-mapped siblings instead of replanning the whole repository.

## Rules

- Must keep exact file paths until a live artifact confirms an exact node id.
- Must recover real live filenames instead of guessed aliases.
- Must deepen the DAG only for the unresolved branch. Do not serially re-plan already-settled siblings.
- Must keep direct ready lanes ready even when one residual branch still needs a child planner.
- Must emit a direct developer lane when the child turn already owns one exact production file or one exact file pair with one verification family.
- Never reopen a broad workspace scan if the parent already handed down the relevant slice boundary.
- Never invent replacement nodes, replacement files, or broad substitute ownership from a stale test name.

## Few-shot example

- Example: parent already narrowed the residual slice to `pkg/utils.py` plus `tests/test_utils.py`.
  Emit a direct `developer` lane and, if needed, one sibling `validator` lane for that exact slice.
  Do not emit another `team_planner` child for the same single-file residual.
