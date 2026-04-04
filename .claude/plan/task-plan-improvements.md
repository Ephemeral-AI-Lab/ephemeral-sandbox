# Implementation Plan: Task Planning System Improvements

## Overview

Three targeted improvements to the coordination planning system: (1) extract the 360-line `_run_agent_phase` function into a testable `PhaseRunner` class, (2) replace ~60 raw string status literals across the engine with `TaskStatus`/`RunStatus` enum members, and (3) make phase input dependencies declarative via the JSON config instead of hard-coded Python.

## Correct Ordering

**Phase 1 (enums) → Phase 2 (PhaseRunner) → Phase 3 (declarative config)**

- Phase 1 is lowest-risk, highest-breadth. Doing it first means PhaseRunner extraction starts from cleaner code.
- Phase 2 is the largest structural change. The resulting `PhaseRunner` class becomes the natural home for Phase 3's config-driven logic.
- Phase 3 touches both `PhaseRunner` and the JSON schema. Doing it last means the class structure is stable.

---

## Phase 1: Replace String Literals with TaskStatus/RunStatus Enums

**Complexity: Low | Risk: Low | ~15 files, ~60 sites**

Since `TaskStatus` and `RunStatus` are `StrEnum`, every member compares equal to its string value. Every change is a drop-in replacement with zero behavioral risk.

### Step 1.1: Engine graph module
**File: `backend/src/services/coordination/engine/graph.py`**
- Add `from services.coordination.core.models import TaskStatus`
- Line 24: `"pending"` → `TaskStatus.PENDING`
- Line 27: `"completed"` → `TaskStatus.COMPLETED`, `"skipped"` → `TaskStatus.SKIPPED`

### Step 1.2: Engine dispatch module
**File: `backend/src/services/coordination/engine/dispatch.py`**
- Add `TaskStatus` to existing import
- Replace ~10 raw status strings (lines 250, 259, 262, 275, 277, 448, 454, 513)

### Step 1.3: Engine lifecycle module
**File: `backend/src/services/coordination/engine/lifecycle.py`**
- Add `TaskStatus, RunStatus` to existing import
- Replace ~12 raw status strings across finalization, cancellation, and blocking logic

### Step 1.4: Engine worker_hooks module
**File: `backend/src/services/coordination/engine/worker_hooks.py`**
- Replace lines 97, 99, 122, 126, 264

### Step 1.5: Expansion modules
**Files:**
- `backend/src/services/coordination/engine/expansion/expansion.py` (~8 sites)
- `backend/src/services/coordination/engine/expansion/state.py` (lines 67, 70)

### Step 1.6: Remaining peripheral files
- `context_parts/summary.py` (lines 55-58)
- `context_parts/artifacts.py` (line 60)
- `infrastructure/domain_context.py` (lines 153, 259, 262)
- `infrastructure/store.py` (lines 104, 169, 218, 461, 465, 470, 520)
- `engine/executor.py` (lines 362, 438-439, 480, 547)
- `engine/dependency_blocking.py` (line 78)
- `query/coordination_query.py` (lines 584, 586, 606)
- `adapters/benchmark/sweevo_adapter/orchestration.py` (lines 52, 128, 146, 150, 176, 226)
- `infrastructure/audit.py` (lines 63, 78, 92)

### Step 1.7: Update model defaults
**File: `backend/src/services/coordination/core/models.py`**
- `TeamTask.status` default: `"pending"` → `TaskStatus.PENDING`
- `CoordinationPlan.status` default: `"planning"` → `RunStatus.PLANNING`

### Step 1.8: Add enum regression test
**File: `backend/tests/test_coordination_enums.py`** (new)
- Grep-based test scanning `services/coordination/` for raw status string patterns, asserting zero matches

---

## Phase 2: Extract `_run_agent_phase` into PhaseRunner Class

**Complexity: Medium-High | Risk: Medium | 2 files modified, 1 new file**

### Step 2.1: Create PhaseRunner class
**File: `backend/src/services/coordination/planning/workflow/phase_runner.py`** (new)
- Constructor captures shared state: `phase_def`, `phase_outputs`, `run_id`, `goal`, `project_context`, `store`, `run_named_agent_fn`, `team_id`, `team_agent_names`
- `async def run(self) -> dict[str, Any]` — top-level orchestrator
- `async def _run_work_step(self) -> Any` — lines 606-720 (run agent, timeout/summarize, recover partial)
- `async def _run_posthook_step(self, phase_work_response, submit_tool, tool_result) -> tuple[Any, Any]` — lines 783-863
- `def _try_direct_explore_posthook(self, submit_tool, tool_result) -> tuple[bool, Any, Any]` — lines 751-776

### Step 2.2: Move helper functions into phase_runner.py
- `_recover_phase_work_material` (lines 420-446)
- `_recover_run_response_text` (lines 389-402)
- `_parallel_snapshot_for_session` (lines 449-461)
- `_normalize_explore_output_from_parallel_snapshot` (lines 464-523)
- `_extract_phase_work_response` (lines 380-386)

### Step 2.3: Replace `_run_agent_phase` with thin delegation
**File: `backend/src/services/coordination/planning/workflow/workflow.py`**
```python
async def _run_agent_phase(...) -> dict[str, Any]:
    from .phase_runner import PhaseRunner
    runner = PhaseRunner(
        phase_def=phase_def, phase_outputs=phase_outputs,
        run_id=run_id, goal=goal, project_context=project_context,
        store=store, run_named_agent_fn=run_named_agent_fn,
        team_id=team_id, team_agent_names=team_agent_names,
    )
    return await runner.run()
```

### Step 2.4: Add PhaseRunner unit tests
**File: `backend/tests/test_phase_runner.py`** (new)
- `_run_work_step` with immediate return → verify correct extraction
- `_run_work_step` with timeout → verify summarize handoff + partial recovery
- `_try_direct_explore_posthook` with mock snapshot → verify deterministic submission
- `_run_posthook_step` timeout with output already set → verify recovery
- `_run_posthook_step` timeout with output None → verify RuntimeError

### Step 2.5: Verify existing tests pass
- `test_planning_workflow.py`, `test_planning_workflow_helpers.py`, `test_planning_workflow_contracts.py`, `test_planning_workflow_e2e.py`

---

## Phase 3: Declarative Phase Input Dependency Config

**Complexity: Medium | Risk: Medium | 3 files modified, 1 JSON modified**

### Step 3.1: Define `InputDepConfig` schema
**File: `backend/src/agent_core/loader/schema.py`**
```python
@dataclass
class InputDepConfig:
    phase: str
    keys: list[str] | None = None
```
Add `input_deps: list[InputDepConfig]` to `PhaseConfig` with `default_factory=list`.

### Step 3.2: Add `input_deps` to task_planner.json
**File: `.super-cocoa-agents/task_planner/task_planner.json`**
```json
"explore":     { "input_deps": [{"phase": "analyze"}] }
"synthesize":  { "input_deps": [{"phase": "explore"}] }
"plan_tasks":  { "input_deps": [{"phase": "synthesize", "keys": ["report_count", "success_count", ...]}] }
```

### Step 3.3: Implement config-driven `_phase_relevant_outputs`
**File: `backend/src/services/coordination/planning/workflow/phase_runner.py`**
- Read `input_deps` from `PhaseConfig`
- If `keys` specified, filter predecessor output to only those keys
- Fallback to legacy hard-coded behavior when `input_deps` is absent (backward compat)

### Step 3.4: Add tests
**File: `backend/tests/test_planning_workflow_helpers.py`** (extend)
- Config-driven filtering with `keys`
- Empty `input_deps` fallback to legacy behavior
- `PhaseConfig.from_dict` round-trip for `input_deps`

### Step 3.5: Remove legacy fallback (future PR)
- After production validation, remove the if/elif chain entirely

---

## Risks and Mitigations

| Risk | Mitigation |
|------|-----------|
| PhaseRunner introduces subtle behavioral difference | Write as line-for-line transliteration first, refactor into methods only after tests pass |
| Enum replacement breaks DB serialization | `StrEnum` serializes as string by default; add targeted test confirming `json.dumps(TaskStatus.PENDING) == '"pending"'` |
| Missing `input_deps` in production JSON | Fallback path preserves old hard-coded behavior; log warning when falling back |
| New `input_deps` field breaks existing `from_dict` | Field has `default_factory=list`, existing callers unaffected |

## Success Criteria

- [ ] Zero raw string status comparisons in `services/coordination/` (enforced by regression test)
- [ ] `_run_agent_phase` in `workflow.py` is ≤10 lines (thin delegation)
- [ ] `PhaseRunner` methods each have dedicated unit tests covering timeout/recovery paths
- [ ] `_phase_relevant_outputs` reads `input_deps` from `PhaseConfig` when present
- [ ] `task_planner.json` declares `input_deps` for all four phases
- [ ] All existing tests pass with zero modifications
- [ ] Adding a new phase requires only JSON config, not Python changes
