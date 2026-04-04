# Implementation Plan: Backend Code Quality Fixes

## Task Type
- [x] Backend

## Triage: Issues Already Resolved

**CRITICAL-1 & CRITICAL-2 (Hardcoded IP / DB credentials):** Already fixed. `config.py:19-21` now uses empty string defaults. No action needed.

---

## Technical Solution

Address all remaining HIGH/MEDIUM/LOW issues from the code review in priority order. Each step is atomic and independently testable.

---

## Implementation Steps

### Step 1: Add LRU eviction to `_PARALLEL_RESULT_SNAPSHOTS` (HIGH)

**File:** `backend/src/toolkits/coordination_toolkit/multi_agent_tools.py:18`

**Problem:** Module-level `dict[str, str]` grows unbounded — every coordination run appends a JSON snapshot, never evicted.

**Change:**
```python
# Before
_PARALLEL_RESULT_SNAPSHOTS: dict[str, str] = {}

# After
from collections import OrderedDict

_MAX_RESULT_SNAPSHOTS = 200

class _BoundedSnapshots(OrderedDict):
    """LRU dict that evicts oldest entries when capacity is reached."""
    def __setitem__(self, key, value):
        if key in self:
            self.move_to_end(key)
        super().__setitem__(key, value)
        while len(self) > _MAX_RESULT_SNAPSHOTS:
            self.popitem(last=False)

_PARALLEL_RESULT_SNAPSHOTS: dict[str, str] = _BoundedSnapshots()
```

- All existing call sites (`_PARALLEL_RESULT_SNAPSHOTS[key] = val` and `.get(key)`) remain compatible — no caller changes needed.

**Deliverable:** Bounded memory usage for result snapshots.

---

### Step 2: Cap fallback query in `list_coordination_plans` (HIGH)

**File:** `backend/src/api/coordination.py:197`

**Problem:** Fallback loads up to 10,000 rows into memory for in-Python aggregation.

**Change:**
```python
# Before
all_runs = store.list_coordination_plans(limit=10000)

# After
_FALLBACK_SUMMARY_LIMIT = 1000

all_runs = store.list_coordination_plans(limit=_FALLBACK_SUMMARY_LIMIT)
```

Also add a `logger.warning` when the fallback path is entered (line 193-194):
```python
except Exception:
    logger.warning("summarize_coordination_plans failed, using fallback aggregation", exc_info=True)
    stats = None
```

Same pattern for the second fallback at line ~286-289.

**Deliverable:** Bounded memory in fallback path + visible logging when fallback triggers.

---

### Step 3: Convert WebSocket connections from list to dict (HIGH)

**File:** `backend/src/api/coordination.py:914-999`

**Problem:** `_ws_connections` is a `list[tuple]`. `.remove()` is O(n), called inside `_ws_lock`.

**Change:**
```python
# Before
_ws_connections: list[tuple[WebSocket, set[str]]] = []

# After
_ws_connections: dict[int, tuple[WebSocket, set[str]]] = {}
_MAX_WS_CONNECTIONS = 500  # Also fixes MEDIUM: no connection limit

# In broadcast_coordination_event:
stale_ids: list[int] = []
for conn_id, (ws, subscribed_ids) in _ws_connections.items():
    if run_id in subscribed_ids:
        try:
            await ws.send_text(message)
        except Exception:
            stale_ids.append(conn_id)
for conn_id in stale_ids:
    _ws_connections.pop(conn_id, None)

# In coordination_websocket:
conn_id = id(ws)
async with _ws_lock:
    if len(_ws_connections) >= _MAX_WS_CONNECTIONS:
        await ws.close(code=1013, reason="Too many connections")
        return
    _ws_connections[conn_id] = entry

# In finally block:
async with _ws_lock:
    _ws_connections.pop(conn_id, None)
```

**Deliverable:** O(1) add/remove, connection limit enforced.

---

### Step 4: Add logging to silent `except Exception` blocks (HIGH/MEDIUM)

**Files (priority targets — 6 files):**
| File | Lines |
|------|-------|
| `services/execution/middleware/agno_streaming_tool_calls.py` | 78 |
| `services/execution/adapters/agno/event_helpers.py` | 38, 51, 58 |
| `services/ephemeral_agents/run_tracking.py` | 54 |
| `api/coordination.py` | 193, 198, 230, 286, 289 |
| `api/ephemeral_agents.py` | 119 |
| `services/execution/adapters/agno/__init__.py` | 80, 97, 115, 121 |

**Change pattern:**
```python
# Before
except Exception:
    pass

# After
except Exception:
    logger.debug("description of what failed", exc_info=True)
```

For `agno_streaming_tool_calls.py:78` specifically:
```python
except Exception:
    logger.debug("aclose failed during stream cleanup", exc_info=True)
```

**Deliverable:** All silent swallows become debuggable.

---

### Step 5: Extract helpers from `get_plan_detail` (MEDIUM)

**File:** `backend/src/api/coordination.py:500-706`

**Problem:** 200+ line endpoint with deep nesting and inline imports.

**Change:** Extract 3 helper functions in the same file (above the endpoint):

1. `_build_task_infos(store, plan_id, task_rows, task_expansions) -> list[TeamTaskInfo]` — lines 511-577
2. `_resolve_plan_context(meta, coordination_spec_context_service) -> tuple[str|None, str|None, str|None, str|None]` — lines 593-608 (returns global_context_path, spec_path, global_context_markdown, spec_markdown)
3. `_build_telemetry(tasks, workspace_contract_info, plan_id) -> CoordinationTelemetryInfo | None` — lines 624-657

The endpoint becomes ~40 lines of orchestration.

**Deliverable:** `get_plan_detail` reduced from 200+ to ~40 lines.

---

### Step 6: Thread-safe `_get_ephemeral_store()` (MEDIUM)

**File:** `backend/src/services/ephemeral_agents/run_tracking.py:46-56`

**Change:**
```python
import threading

_ephemeral_store_lock = threading.Lock()
_ephemeral_store: Any = None

def _get_ephemeral_store() -> "EphemeralAgentStore | None":
    global _ephemeral_store
    if _ephemeral_store is not None:
        return _ephemeral_store
    with _ephemeral_store_lock:
        if _ephemeral_store is None:  # double-checked locking
            try:
                from db.relational_db.ephemeral_agent_store import ephemeral_agent_store as store
                if store.is_available:
                    _ephemeral_store = store
            except Exception:
                logger.debug("Failed to initialize ephemeral store", exc_info=True)
    return _ephemeral_store
```

**Deliverable:** Race-free lazy initialization.

---

### Step 7: Lazy `execution_engine` singleton (MEDIUM)

**File:** `backend/src/services/coordination/engine/executor.py:658-661`

**Change:**
```python
# Before (module-level instantiation)
execution_engine = ExecutionEngine(...)

# After
_execution_engine: ExecutionEngine | None = None

def get_execution_engine() -> ExecutionEngine:
    global _execution_engine
    if _execution_engine is None:
        _execution_engine = ExecutionEngine(
            model_queue_limits=_parse_model_queue_limits(),
            default_model_concurrency=config.coordination_default_model_concurrency,
        )
    return _execution_engine

# Keep backward compat — but this is a breaking alias if anyone accesses
# attributes before first call. All importers should migrate to get_execution_engine().
```

Then update all `from services.coordination.engine.executor import execution_engine` call sites (grep shows ~8 locations in api/coordination.py and other files) to use `get_execution_engine()`.

**Deliverable:** Config read at first use, testable without monkeypatching import-time state.

---

### Step 8: Document `_REWRITE_WORKAROUND_TTL_S` (MEDIUM)

**File:** `backend/src/toolkits/daytona_toolkit/file_tools.py:26`

**Change:** Add a comment explaining the workaround:
```python
# Workaround: Daytona's create_file API may silently discard writes if the same
# path is written twice in rapid succession (observed as a race in the Daytona
# file sync layer). This TTL tracks recent create_file rejections so the tool
# can warn the agent instead of silently losing content.
# TODO: Remove once Daytona fixes the underlying file-sync race.
_REWRITE_WORKAROUND_TTL_S = 1800.0
```

**Deliverable:** Future maintainers understand the workaround's purpose.

---

### Step 9: Extract magic numbers to named constants (LOW)

**File:** `backend/src/api/coordination.py:109,129`

**Change:**
```python
_CONFLICT_WEIGHT = 10
_WRITE_WEIGHT = 3
_MAX_HOT_FILES = 20

# line 109
score = conflict_count * _CONFLICT_WEIGHT + write_count * _WRITE_WEIGHT

# line 129
return hot_files[:_MAX_HOT_FILES]
```

**Deliverable:** Self-documenting scoring formula.

---

### Step 10: Move `ephemeral_agent_models.py` into store package (LOW)

**Current:** `backend/src/db/relational_db/ephemeral_agent_models.py`
**Target:** `backend/src/db/relational_db/ephemeral_agent_store/models.py`

**Change:**
1. Move file
2. Update all imports (grep for `from db.relational_db.ephemeral_agent_models import`)
3. Optionally re-export from the old location for safety during transition

**Deliverable:** Consistent with `coordination_store/models.py` and `code_intelligence_store/models.py` patterns.

---

### Step 11 (Future / Incremental): Split large files

**Not in this PR** — these are structural refactors best done incrementally:

| File | Lines | Suggested Split |
|------|-------|-----------------|
| `api/coordination.py` | 999 | Split into `api/coordination_plans.py`, `api/coordination_tasks.py`, `api/coordination_ws.py` |
| `multi_agent_tools.py` | 933 | Separate parallel execution engine from tool definitions |
| `lsp/client.py` | 819 | Split protocol handling from client lifecycle |

These should be separate PRs to avoid merge conflict risk with active development.

---

## Key Files

| File | Operation | Description |
|------|-----------|-------------|
| `toolkits/coordination_toolkit/multi_agent_tools.py:18` | Modify | Add LRU eviction to snapshots dict |
| `api/coordination.py:109,129,190-230,286-289,500-706,914-999` | Modify | Cap fallback, extract helpers, fix WS, constants |
| `services/execution/middleware/agno_streaming_tool_calls.py:78` | Modify | Add debug logging |
| `services/execution/adapters/agno/event_helpers.py:38,51,58` | Modify | Add debug logging |
| `services/ephemeral_agents/run_tracking.py:46-56` | Modify | Thread-safe init |
| `services/coordination/engine/executor.py:658-661` | Modify | Lazy singleton |
| `toolkits/daytona_toolkit/file_tools.py:26` | Modify | Add workaround comment |
| `db/relational_db/ephemeral_agent_models.py` | Move | Into ephemeral_agent_store/models.py |

## Risks and Mitigation

| Risk | Mitigation |
|------|------------|
| `_BoundedSnapshots` evicts data a long-running session needs | 200 cap is generous; coordination runs are short-lived |
| Changing `execution_engine` to lazy breaks imports | Keep module-level `execution_engine` as property or update all call sites in same PR |
| Moving `ephemeral_agent_models.py` breaks imports | Grep all importers; small surface area (2-3 files) |
| WebSocket dict change breaks concurrent broadcast | Same lock semantics preserved; dict operations are atomic |

## Excluded from Plan

- **CRITICAL-1 & CRITICAL-2:** Already fixed in current codebase
- **Large file splits (Step 11):** Deferred to separate PRs — too high merge-conflict risk
- **Deferred import audit (MEDIUM):** Requires deep circular dependency analysis; low ROI for now
- **`print()` in runner.py (MEDIUM):** Runner is CLI-only benchmark tooling, low priority
