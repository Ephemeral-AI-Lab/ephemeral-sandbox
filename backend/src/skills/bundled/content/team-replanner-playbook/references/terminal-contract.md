# Replanner Terminal Contract

Load this reference before drafting any non-empty `submit_replan(...)` payload or any payload with `cancel_ids`.

## Call Shape

```ts
submit_replan({ new_tasks: NewTaskSpec[], cancel_ids: string[] })
```

```ts
type NewTaskSpec = {
  id: string;
  description: string;
  name: "developer" | "validator"; // team_planner is accepted by the runtime but the replanner owns synthesis — do not spawn one
  spec: string;
  deps: string[];
  scope_paths: string[];
};
```

Top-level input has only `new_tasks` and `cancel_ids`. New task objects have only `id`, `description`, `name`, `spec`, `deps`, and `scope_paths`.

Never include `output`, `summary`, `background`, `parent_id`, `new_sibling_tasks`, `new_children_tasks`, `expected_projection`, or prose outside the terminal call.

## Field Rules

| Field | Rule |
| --- | --- |
| `id` | Unique lower-kebab id in this payload. Local deps reference this exact string. |
| `description` | Short non-blank corrective outcome label. |
| `name` | Use `developer` or terminal `validator`. Never use `scout` or `team_replanner`. `team_planner` is accepted by the schema but the replanner owns synthesis — do not spawn one. |
| `spec` | Must contain `1. Goal:`, `2. Task Details:`, `3. Acceptance Criteria:` in order, each on its own line with body text after the colon. |
| `deps` | Prefer local payload ids. Existing ids require fresh graph proof that they are schedulable and not downstream of this replanner or the failed task. Validators depend on local payload ids. |
| `scope_paths` | Non-empty repo-relative production paths. Verification-only tests stay in `spec` unless tests are explicitly the owned bug surface. |

`cancel_ids` may include only stale non-terminal direct siblings of this replanner. Never include the failed task id, the original `request_replan` task, this replanner id, terminal tasks, or nested descendants. Cancel the stale sibling root only; cascade handles descendants and dependents.

Replacement tasks may include a sibling's scope only when that sibling id appears in `cancel_ids`.

## Validator Guidance

Validator tasks are optional. Add one only when a distinct verification lane is useful and no preserved downstream validator already covers the repair surface. A validator must depend on at least one upstream local repair id; a terminal validator should cover the terminal repair leaves it verifies.

## Spec Contents

`2. Task Details:` should name:

- failure classification
- root cause mechanism or unresolved trace gap
- exact production scope
- sibling/cancel handling
- dependency context
- uncertainty and evidence source

`3. Acceptance Criteria:` should name concrete verification commands or pytest ids and require reporting command output, exit codes, changed behavior, and residual risk.

## Examples

### Empty Replan

```json
{
  "new_tasks": [],
  "cancel_ids": []
}
```

### Direct Scope Expansion

```json
{
  "new_tasks": [
    {
      "id": "repair-config-path",
      "description": "Repair config loader path",
      "name": "developer",
      "spec": "1. Goal: Repair the config regression in the production loader path identified by the failed task.\n2. Task Details: Classification: scope_expansion. The failed task proved the original assigned file was not the source of the wrong value; the root cause mechanism is the config lookup branch in pkg/config.py. Own pkg/config.py, run ci_diagnostics(file_path=\"pkg/config.py\") first, preserve the named failing test evidence in the summary, and do not edit benchmark tests. Verification test paths appear in acceptance only; scope_paths stays on the production file.\n3. Acceptance Criteria: Run uv run pytest tests/test_config.py -q and the focused failing test id from the failed summary; report commands, exit codes, and whether the config lookup branch now matches the expected production behavior.",
      "deps": [],
      "scope_paths": ["pkg/config.py"]
    }
  ],
  "cancel_ids": []
}
```

### Cancel Stale Sibling

```json
{
  "new_tasks": [
    {
      "id": "repair-shared-auth-path",
      "description": "Repair shared auth path after stale sibling cancellation",
      "name": "developer",
      "spec": "1. Goal: Replace stale auth work with the production path proven by the failed task.\n2. Task Details: Classification: wrong_owner_or_role. Cancel sibling dev-auth-wrapper because it is non-terminal, shares this replanner's parent, and is still working from the invalid wrapper assumption. Own backend/src/auth/session.py; run ci_diagnostics(file_path=\"backend/src/auth/session.py\") first; keep cancelled sibling scope out of all uncancelled work.\n3. Acceptance Criteria: Run uv run pytest backend/tests/test_auth/test_session.py -q and report command output, exit codes, and any residual risk.",
      "deps": [],
      "scope_paths": ["backend/src/auth/session.py"]
    }
  ],
  "cancel_ids": ["dev-auth-wrapper"]
}
```

### Diagnostic Repair With Validator

```json
{
  "new_tasks": [
    {
      "id": "repair-index-state",
      "description": "Repair index state mutation",
      "name": "developer",
      "spec": "1. Goal: Repair the state mutation confirmed by diagnostic scouts.\n2. Task Details: Classification: unresolved_blocker resolved by diagnostics. Scout notes for backend/src/index/state.py confirmed the failing cluster reaches the stale mutation path in apply_index_update. Own backend/src/index/state.py, run ci_diagnostics(file_path=\"backend/src/index/state.py\") first, and preserve the exact failing ids from the failed task summary.\n3. Acceptance Criteria: Run uv run pytest backend/tests/test_index/test_state.py -q and report commands, exit codes, changed behavior, and residual risk.",
      "deps": [],
      "scope_paths": ["backend/src/index/state.py"]
    },
    {
      "id": "val-index-recovery",
      "description": "Validate index recovery repairs",
      "name": "validator",
      "spec": "1. Goal: Verify the corrective index repair after diagnostic child work finishes.\n2. Task Details: Validate backend/src/index/state.py after repair-index-state. This is the terminal validator for the local replan payload; downstream validators already rewired to the replanner should not be duplicated.\n3. Acceptance Criteria: Run uv run pytest backend/tests/test_index -q; all pass or failures identify the owning repair scope with command, exit code, and failing assertion.",
      "deps": ["repair-index-state"],
      "scope_paths": ["backend/src/index/state.py"]
    }
  ],
  "cancel_ids": []
}
```

## Final Checklist

- Top-level input has only `new_tasks` and `cancel_ids`.
- Every task has only `id`, `description`, `name`, `spec`, `deps`, and `scope_paths`.
- Every id is unique.
- Every local dep names another task in this payload.
- Existing deps, if used, are freshly proven schedulable and not downstream of this replanner or the failed task.
- Every task has non-empty repo-relative production `scope_paths`.
- Every spec uses `1. Goal:`, `2. Task Details:`, `3. Acceptance Criteria:`.
- `cancel_ids` contains only stale non-terminal direct siblings.
- No benchmark tests are scoped unless the prompt explicitly owns a test-only bug.
- The final assistant action is the `submit_replan(...)` tool call, not prose.
