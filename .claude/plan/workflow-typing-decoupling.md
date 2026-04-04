## Implementation Plan: Fix Bidirectional Coupling & Pervasive Any Typing

### Task Type
- [x] Backend

### Problem Statement

The planning workflow has two structural issues:

1. **Bidirectional coupling**: `phase_runner.py` imports 8+ private functions from `workflow.py` via deferred runtime imports (lines 79-88, 239-243, 374-378), while `workflow.py` imports `PhaseRunner` from `phase_runner.py`. This creates a tight bidirectional dependency that makes both modules harder to reason about independently.

2. **Pervasive `Any` typing**: `phase_def`, `store`, `run_named_agent_fn`, `phases`, and return values are all typed `Any` despite concrete types existing (`PhaseConfig` in `agent_core.loader.schema`, `PlanningStore` protocol in `_protocols.py`). This defeats type checking and IDE support.

### Technical Solution

**Decoupling**: Move the 9 functions that `PhaseRunner` imports back from `workflow.py` into shared modules (`_helpers.py` for utilities/filters, `_messages.py` for message builders and agent builders). This makes `workflow.py` purely orchestration and eliminates all backward imports.

**Typing**: Import `PhaseConfig` from `agent_core.loader.schema`, define a `RunNamedAgentFn` protocol in `_protocols.py`, and replace `Any` annotations throughout. Use `PlanningStore | None` for store parameters.

### Implementation Steps

#### Step 1: Extend `_protocols.py` with `RunNamedAgentFn`

Add a callable protocol for the agent runner function.

```python
# _protocols.py additions
from collections.abc import Awaitable

class RunNamedAgentFn(Protocol):
    """Callable that dispatches a message to a named agent and returns its result."""
    def __call__(
        self,
        agent_name: str,
        message: str,
        *,
        session_id: str,
        markdown: bool,
        options: dict[str, Any],
    ) -> Awaitable[Any]: ...
```

Files: `_protocols.py`
Risk: Low — additive only, no behavioral change.

#### Step 2: Move pure utilities from `workflow.py` to `_helpers.py`

Move these 4 functions (they have zero workflow-package imports):
- `_phase_runtime_tool_names` (lines 28-40)
- `_resolve_phase_summarize_after_seconds` (lines 43-47)
- `_remaining_phase_budget_seconds` (lines 50-58)
- `_phase_relevant_outputs` (lines 280-303)

Also move the module-level constants they use:
- `_PHASE_SUMMARIZE_AFTER_SECONDS` (line 24)
- `_BLOCKED_PHASE_RUNTIME_TOOL_NAMES` (line 25)

Files: `_helpers.py`, `workflow.py`
Risk: Low — pure moves, no logic changes.

#### Step 3: Create `_messages.py` for message builders and agent builders

Move these 5 functions from `workflow.py`:
- `_build_phase_runtime_message` (lines 122-165)
- `_build_phase_summarize_message` (lines 168-195)
- `_schema_declares_nested_fields` (lines 198-218) — private helper for posthook message
- `_build_posthook_runtime_message` (lines 221-277) — keeps deferred `from .phase_hooks import POSTHOOK_MESSAGE_ENRICHERS` inside function body to avoid circular with `phase_hooks.py`
- `_build_runtime_posthook_agent` (lines 306-327)
- `_build_phase_agent` (lines 359-394)

Import chain after move:
```
_helpers.py      ← no workflow package imports (clean leaf)
_messages.py     ← imports from _helpers (leaf), deferred import from phase_hooks
phase_hooks.py   ← imports from _helpers (leaf)
phase_runner.py  ← imports from _helpers + _messages (no workflow.py!)
workflow.py      ← imports from _helpers + _messages + phase_runner (unidirectional)
```

No circular dependency at import time.

Files: new `_messages.py`, `workflow.py`, `phase_runner.py`
Risk: Medium — largest move, needs careful verification that all imports resolve.

#### Step 4: Update `phase_runner.py` imports to use new modules

Replace all deferred `from .workflow import ...` blocks:

```python
# run() — was importing 8 functions from workflow.py
# Now:
from ._helpers import (
    _phase_relevant_outputs,
    _phase_runtime_tool_names,
    _remaining_phase_budget_seconds,
    _resolve_phase_summarize_after_seconds,
)
from ._messages import (
    _build_phase_agent,
    _build_phase_runtime_message,
    _build_posthook_runtime_message,
    _build_runtime_posthook_agent,
)
```

Convert deferred imports to top-level imports since the circular dependency is resolved.

Files: `phase_runner.py`
Risk: Low — import path changes only, verified by step 3's import chain.

#### Step 5: Update `workflow.py` imports

`workflow.py` now imports from `_helpers` and `_messages` instead of defining the functions locally. The functions it still calls:
- `_persist_planning_workflow_metadata` — stays in `workflow.py` (orchestration concern)
- `_phase_relevant_outputs` — import from `_helpers`
- `_build_phase_agent` — not called directly (PhaseRunner calls it)
- `_build_posthook_runtime_message` — not called directly

After the move, `workflow.py` only needs:
```python
from ._helpers import _phase_relevant_outputs  # if used directly
from .phase_runner import PhaseRunner  # already deferred
from .phase_hooks import OUTPUT_PERSISTERS  # already deferred
```

Files: `workflow.py`
Risk: Low — removing dead imports after function moves.

#### Step 6: Apply `PhaseConfig` typing across the workflow

Replace `phase_def: Any` with `phase_def: PhaseConfig` in:
- `workflow.py:_persist_planning_workflow_metadata` param `resolved_phases: list[PhaseConfig] | None`
- `workflow.py:_run_agent_phase` param `phase_def: PhaseConfig`
- `workflow.py:run_planning_workflow` param `phases: list[PhaseConfig] | None`
- `phase_runner.py:PhaseRunner.__init__` param `phase_def: PhaseConfig`
- `_helpers.py:_phase_relevant_outputs` param `phase_def: PhaseConfig | None`
- `_messages.py:_build_phase_agent` — no `phase_def` param (uses `agent_name` directly)

This eliminates ~15 `getattr(phase_def, ...)` calls. Direct attribute access (e.g., `phase_def.name`) is now safe since `PhaseConfig` is a dataclass.

Replace `getattr` patterns:
```python
# Before:
self.phase_name = getattr(phase_def, "name", "")
self.agent_name = getattr(phase_def, "agent", "")
self.timeout_s = getattr(phase_def, "timeout_s", None)
posthook = getattr(self.phase_def, "posthook", None)

# After:
self.phase_name = phase_def.name
self.agent_name = phase_def.agent
self.timeout_s = phase_def.timeout_s
posthook = self.phase_def.posthook
```

Files: `workflow.py`, `phase_runner.py`, `_helpers.py`
Risk: Medium — requires that all callers pass `PhaseConfig` instances. Verify via test suite.

#### Step 7: Apply `RunNamedAgentFn` and `PlanningStore` typing

Replace `run_named_agent_fn: Any` with `RunNamedAgentFn`:
- `workflow.py:run_planning_workflow` — `run_named_agent_fn: RunNamedAgentFn | None`
- `workflow.py:_run_agent_phase` — `run_named_agent_fn: RunNamedAgentFn`
- `phase_runner.py:PhaseRunner.__init__` — `run_named_agent_fn: RunNamedAgentFn`

Replace `store: Any` with `PlanningStore | None`:
- `workflow.py:_persist_planning_workflow_metadata` — `store: PlanningStore | None`
- `workflow.py:_run_agent_phase` — `store: PlanningStore | None`
- `workflow.py:run_planning_workflow` — `store: PlanningStore | None`
- `phase_runner.py:PhaseRunner.__init__` — `store: PlanningStore | None`
- `_plan_repository.py:extract_plan` — `store: PlanningStore | None`

Note: `hasattr` checks for optional store methods (e.g., `get_run_metadata`) remain — the protocol intentionally declares only `locked_metadata_update`.

Files: `workflow.py`, `phase_runner.py`, `_plan_repository.py`, `_protocols.py`
Risk: Low — additive type annotations, runtime behavior unchanged.

#### Step 8: Type return values and remaining `Any` parameters

- `run_planning_workflow` return: `-> dict[str, Any]` (always returns `phase_outputs`)
- `_run_agent_phase` return: `-> dict[str, Any]`
- `PhaseRunner.run` return: already `-> dict[str, Any]` (correct)
- `_build_phase_agent` return: keep `-> Any` (returns opaque agent object from `build_agent`)
- `_build_runtime_posthook_agent` return: keep `-> Any` (same reason)
- `PhaseRunner.__init__`: init `self._work_summary_run_id: str | None = None` explicitly

Files: `workflow.py`, `phase_runner.py`, `_messages.py`
Risk: Low — type annotation changes only.

#### Step 9: Update `__init__.py` exports

No changes needed — `__init__.py` only re-exports `extract_plan` and `run_planning_workflow`, which stay in their current modules.

#### Step 10: Run tests and verify

```bash
cd backend && python -m pytest tests/test_planning_workflow.py tests/test_planning_workflow_contracts.py tests/test_planning_workflow_e2e.py tests/test_planning_workflow_hierarchical_e2e.py tests/test_task_planner_payloads.py -v
```

Verify:
- All existing tests pass without modification
- No import errors at module load time
- `mypy` or `pyright` on the workflow package shows reduced `Any` usage

### Key Files

| File | Operation | Description |
|------|-----------|-------------|
| `backend/src/services/coordination/planning/workflow/_protocols.py` | Modify | Add `RunNamedAgentFn` protocol |
| `backend/src/services/coordination/planning/workflow/_helpers.py` | Modify | Receive 4 pure utility functions + 2 constants from workflow.py |
| `backend/src/services/coordination/planning/workflow/_messages.py` | **Create** | New module for 6 message/agent builder functions from workflow.py |
| `backend/src/services/coordination/planning/workflow/workflow.py` | Modify | Remove moved functions, update imports, apply types |
| `backend/src/services/coordination/planning/workflow/phase_runner.py` | Modify | Import from _helpers/_messages instead of workflow, apply types |
| `backend/src/services/coordination/planning/workflow/_plan_repository.py` | Modify | Apply `PlanningStore | None` type to `store` param |

### Risks and Mitigation

| Risk | Mitigation |
|------|------------|
| Deferred import in `_build_posthook_runtime_message` (phase_hooks) could break if moved | Keep the deferred import pattern in the function body; import chain analysis confirms no circular at module level |
| `PhaseConfig` typing breaks callers passing non-dataclass objects | All callers go through `SpecialistDefinition.from_dict` which produces proper `PhaseConfig` instances; verify with test suite |
| Tests import private functions directly from `workflow.py` | Check test imports and update to new module paths |
| `_build_phase_agent` / `_build_runtime_posthook_agent` have deferred imports from `agent_core` | Keep deferred pattern in new location; these avoid import-time coupling to the loader subsystem |

### Verification Checklist

- [ ] Zero `from .workflow import` in `phase_runner.py`
- [ ] Zero `Any` for `phase_def`, `store`, `run_named_agent_fn` params in workflow package
- [ ] All 5 test files pass
- [ ] `workflow.py` contains only orchestration (< 120 lines after move)
- [ ] No new circular imports (verify with `python -c "from services.coordination.planning.workflow import run_planning_workflow"`)
