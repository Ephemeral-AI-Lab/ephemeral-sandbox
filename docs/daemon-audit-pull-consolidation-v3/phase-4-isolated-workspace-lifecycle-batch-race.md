# Phase 4 — Isolated-Workspace Lifecycle Batch Race

> **Status:** ✅ **Code-complete (2026-05-26).** Engine batch policy +
> daemon per-agent quiesce primitive shipped. See
> [`phase-4-implementation-report.md`](phase-4-implementation-report.md)
> for file-level deltas, AC coverage, and the two deferred items
> (FU#A integration matrix, FU#B mock-agent E2E retry).
>
> **Historical status:** Plan approved via `/ralpan` consensus (Planner → Architect → Critic, 2 iterations to APPROVE).
>
> **Scope:** Close P1 concurrency hole where `Intent.LIFECYCLE` tools
> (`enter_isolated_workspace`, `exit_isolated_workspace`) co-batched with
> ordinary foreground tools race the workspace routing decision, allowing
> private-intent writes to leak into the shared OCC workspace.
>
> **Topical relationship to Phases 1-3:** Independent. Phases 1-3 close the
> daemon audit pull consolidation (V3 is code-complete per
> [`README.md`](README.md) §Phase progress). This phase closes an unrelated
> sandbox concurrency invariant that surfaced after V3 landed. Filed under
> the same folder because the affected files
> (`backend/src/sandbox/isolated_workspace/_control_plane/workspace_handle_lifecycle.py`,
> `backend/src/sandbox/daemon/workspace_tool_dispatch.py`,
> `backend/src/sandbox/daemon/rpc/dispatcher.py`) are the same sandbox
> control plane the V3 audit emitters wire into, and the same lane-1
> "isolated-workspace exit safety" driver (V3 README §Decision Drivers #1)
> applies here at root rather than at instrumentation.
>
> **Goal:** Two-layer enforcement of the architecture-promised invariant
> (`docs/architecture/tools/isolated-workspace.html:166` — lifecycle tools
> change routing state for **later** tool calls): engine-side batch policy
> (Option A) + daemon-side per-agent quiesce primitive (Option C). Both
> ship in the same PR.
>
> **Author:** `/ralpan` consensus, 2026-05-26.

## Quick index

| Priority | ID | Closes |
|---|---|---|
| P1 | [E1](#e1) | Engine dispatches `[exit_isolated_workspace, write_file]` concurrently → write routes to shared OCC after exit removes the handle |
| P1 | [E2](#e2) | Engine dispatches `[enter_isolated_workspace, write_file]` concurrently → first write of isolated session leaks to shared OCC before handle becomes visible |
| P1 | [D1](#d1) | Daemon `_active_isolated_pipeline_for` does a lockless `get_handle` probe — direct daemon RPC callers bypass any engine-side fix |
| P1 | [D2](#d2) | Exit removes maps under `_map_lock`, then runs `_teardown` outside the lock — in-flight foreground RPCs are not drained |
| P2 | [D3](#d3) | Plugin gate (`dispatcher.py:_check_plugin_block`) uses the same lockless probe — same race against enter/exit |
| P2 | [O1](#o1) | Observability: rejection counter + audit event for lifecycle batch rejections |
| P3 | [G1](#g1) | CI lint guard: `dispatch_workspace_tool_call` must have at most one caller outside test files |
| P3 | [G2](#g2) | Doc refresh: `docs/architecture/tools/isolated-workspace.html:166` replaces "later" hedge with explicit two-layer enforcement |

Each item below is self-contained: file path, current behavior, what
needs to land, and how to verify.

---

## Architectural Decision Record

**Decision:** Ship Option A (engine-side lifecycle-batch policy) AND
Option C (daemon-side per-agent dispatch state with explicit quiesce
primitive) in the SAME PR.

**Drivers (priority order):**
1. **Correctness of isolation invariant.** Order-B race silently
   publishes private writes to the shared workspace, breaking the
   discard-boundary invariant at
   `docs/architecture/tools/isolated-workspace.html:140-143`.
2. **Coverage of all entry paths.** Engine batching, direct daemon RPC,
   plugin gate. Option A alone leaves daemon racy; Option C alone gives
   the model no actionable signal.
3. **Minimum diff** conditional on (1) and (2). Reuse existing rejection
   envelope (`_record_tool_batch_rejection` at `dispatch.py:261`) for
   engine; introduce a small dataclass + per-agent state map for daemon.

**Alternatives considered:**

| Option | Verdict | Why |
|---|---|---|
| **A alone** (engine batch policy) | Rejected | Doesn't cover direct daemon-RPC callers or plugin gate. Self-admits residual gap. |
| **B** (engine partition + ordered passes) | Rejected | Picks an implicit ordering (LIFECYCLE-first or LIFECYCLE-last). Either choice fools realistic prompts: `[exit, write]` LIFECYCLE-first writes to shared OCC after exit; `[enter, write]` LIFECYCLE-last writes to shared OCC before enter. Rejection is the only safe choice. |
| **C alone** (daemon lock) | Rejected | Closes invariant universally but gives the model no actionable signal. Agents silently observe non-deterministic semantics. Defense in depth requires both. |
| **Decorator-level policy** (`tools/_framework/core/decorator.py:66-70`) | Rejected | Decorator validates per-tool intent; batching is a property of dispatch. Wrong layer. |
| **A primary + C as follow-up** (initial draft) | Rejected by Architect | Known-incomplete merge; self-violates Principle 3 (defense in depth). |

**Why chosen (A + C together):**
- A fixes the dominant entry point and gives the model an actionable
  error.
- C closes the invariant for every RPC path (workspace dispatch, plugin
  gate, future paths) via an explicit quiesce primitive that survives
  independent of A.
- The two cover disjoint failure modes (engine = same-turn batching;
  daemon = direct RPC / different agents / cross-turn in-flight).
- Both honor all five RALPLAN-DR principles in one ship.

**Consequences:**
- **Net positive:** isolation invariant becomes enforceable; routing-
  state ordering becomes a property of the system rather than a per-
  prompt hope.
- **Cost:** ~80-120 LOC across engine + daemon + tests; new per-agent
  state map; one new CI lint guard.
- **Backwards compat:** existing prompts that batched lifecycle with
  siblings now receive `is_error=True` on siblings (the lifecycle still
  executes). Error text is model-actionable — expected agent loops will
  retry the rejected siblings in the next batch.
- **Future:** if a third call site for `dispatch_workspace_tool_call`
  appears, the CI lint (G1) blocks it pending review.

**Follow-ups (out of scope for this plan):**
- AC11 perf benchmark — tracked but `[observability, non-blocking]`. Tripwire,
  not a CI gate.
- Architecture page refresh (G2) — replace "later" hedge at
  `isolated-workspace.html:166` with explicit two-layer enforcement.

---

## RALPLAN-DR Summary

### Principles
1. **Routing state must be totally ordered with respect to tool calls
   that observe it.** Lifecycle effects are not commutative with
   workspace I/O.
2. **Isolation is a security/data-integrity boundary**, not a perf
   concern — correctness > batching throughput.
3. **Defense in depth.** Engine batch policy + daemon quiesce primitive.
4. **Match existing patterns where appropriate; diverge where semantics
   differ.** Terminal-tool rejection is the right precedent but lifecycle
   is `semantically valid concurrent batch`, not `contract violation`,
   so the lifecycle call itself dispatches.
5. **Fail loudly with explicit, actionable guidance.** Sibling rejection
   text names the lifecycle tool and instructs retry.

### Decision Drivers (top 3)
1. Correctness of isolation invariant (Order-B leaks private writes to
   shared OCC).
2. Coverage of all entry paths (engine + daemon RPC + plugin gate).
3. Minimum diff conditional on (1) and (2).

### Viable options summary
- **A. Engine batch-rejection (lifecycle solo) — adopted as one half.**
- **B. Engine partition + ordered passes — rejected** (silent reordering).
- **C. Daemon per-agent dispatch lock + quiesce — adopted as other half.**
- **D. Decorator-level policy — rejected** (wrong layer).

### Pre-mortem (6 scenarios — deliberate mode)

1. **Existing prompts batch lifecycle with siblings → ship breaks them.**
   Likelihood: low (no current prompts known to batch lifecycle).
   Blast radius: agent loop re-issues siblings on next turn. Mitigation:
   AC1 error text is model-actionable; lifecycle still executes so the
   agent's intent (enter/exit) advances; pre-merge grep of
   `task_center_runner/tests` and `docs/architecture` for examples.

2. **Per-agent lock deadlocks under nested calls.** Likelihood: low
   (enumerated audit). Mitigation by enumeration, not by review:
   `dispatch_workspace_tool_call` has one caller
   (`backend/src/sandbox/daemon/builtin_operations.py:72`);
   `_check_plugin_block` has one caller
   (`backend/src/sandbox/daemon/rpc/dispatcher.py:84`, lifecycle ops
   bypass via prefix filter). CI guard (G1 / AC10) prevents regression.
   Lock acquisition order asserted in test mode (AC9).

3. **Plugin LIFECYCLE in mock harness** at
   `backend/src/task_center_runner/agent/mock/plugin_workspace_probe.py:749`:
   test-only scaffolding under `task_center_runner/agent/mock/` that
   exercises the `op_registry.py:120-122` rejection. No prod plugin
   emitter carries LIFECYCLE. Documented in `op_registry.py` docstring;
   no code change needed.

4. **Drain timeout under heavy load.** Long-running shell holds inflight
   slot; default `grace_s=5.0` (from `ExitIsolatedWorkspaceInput`) may be
   insufficient. Mitigation: exit returns `exit_drain_timeout`; agent
   can retry with larger grace or pre-cancel via background-task path.
   Document recommended `grace_s` in `exit_isolated_workspace` tool
   docstring.

5. **Dict-state leak on drain-timeout.** Retained
   `_AGENT_DISPATCH_STATES[agent_id]` is reused by subsequent dispatches
   and exit retries. Cleanup happens on first successful exit. If the
   agent never retries exit, state is GC'd with the daemon process; no
   persistent leak.

6. **Enter-succeeds → no-dispatch → exit drain.** State is created
   **lazily on first dispatch**, not on enter. If exit runs before any
   dispatch, `_STATES_DICT_LOCK` lookup returns missing → fast-path exit
   with no drain needed. `inflight` is decremented in a `finally` block
   in the dispatch path, so failed/cancelled dispatches still release
   the counter and unblock pending exit drains.

### Expanded test plan
- **Unit (engine):** AC1, AC2, AC3, AC4 — see [E1](#e1) for full matrix.
- **Unit (daemon):** AC7, AC8, AC9 using `_TestableAgentDispatchState`.
- **Integration:** AC5 matrix; assert shared OCC manifest+root_hash
  byte-identical pre/post for each row.
- **E2E:** real agent loop emits batched-lifecycle prompt → engine
  returns error → model retries → both batches succeed.
- **Lint/CI:** G1 grep guard.
- **Observability:** AC6 counters/audits asserted in unit + integration
  tests.
- **Perf (non-blocking):** AC11 benchmark.

---

## Verified Root Cause

**The architecture promises but does not enforce sequential ordering.**

`docs/architecture/tools/isolated-workspace.html:166` states:
> "From the engine perspective, the lifecycle tools are regular tools
> with `Intent.LIFECYCLE`. They change the routing state that **later**
> sandbox tool calls observe; they do not themselves complete a
> TaskCenter attempt."

The word "later" is the load-bearing invariant. The engine's
parallel-foreground dispatcher silently breaks it.

### Race anchors (all verified in current source)

**Engine dispatch — no ordering for foreground tools:**
- `backend/src/engine/tool_call/dispatch.py:265-308` —
  `_dispatch_deferred_tool_calls` partitions tool calls only by
  `background` vs `foreground`. `Intent.LIFECYCLE` is not a partition
  axis.
- `backend/src/engine/tool_call/dispatch.py:352` —
  `_dispatch_many_foreground_tools` creates concurrent `asyncio` tasks.
- `backend/src/engine/tool_call/dispatch.py:401-404` —
  `tasks = [asyncio.create_task(run_foreground_tool(tool_call)) for tool_call in foreground_tool_calls]`.
  No per-agent serialization, no `Intent.LIFECYCLE` ordering.

**Daemon routing — lockless probe:**
- `backend/src/sandbox/daemon/workspace_tool_dispatch.py:30-37` —
  ```python
  def _active_isolated_pipeline_for(agent_id: str) -> Any | None:
      isolated_pipeline = get_active_pipeline()
      if (
          isolated_pipeline is not None
          and isolated_pipeline.get_handle(agent_id) is not None
      ):
          return isolated_pipeline
      return None
  ```
  `get_handle` only takes the map lock momentarily; the routing decision
  is otherwise lockless.

**Exit mutates maps then teardown outside the lock:**
- `backend/src/sandbox/isolated_workspace/_control_plane/workspace_handle_lifecycle.py:206-227` —
  ```python
  async with self._map_lock:
      ...
      del self._by_agent[agent_id]
      del self._handles[handle_id]
  ...
  await self._teardown(handle, grace_s=effective_grace_s, timer=timer)
  ```
  The map mutation is atomic with `_map_lock`, but `_teardown` runs
  outside the lock — and in-flight foreground RPCs holding `pipeline`
  references from the lockless probe (before the map mutation) are not
  drained.

**Plugin gate uses the same lockless probe:**
- `backend/src/sandbox/daemon/rpc/dispatcher.py:214-233` —
  `_check_plugin_block` calls `iws.get_handle(agent_id)` with no
  coordination against enter/exit. The plugin-op gate decision is
  equally racy.

**Decorator validates intent but not batching policy:**
- `backend/src/tools/_framework/core/decorator.py:66-70` — `@tool`
  validates `intent in {READ_ONLY, WRITE_ALLOWED, LIFECYCLE}`. No
  batching constraint.

### Failure modes

For `[exit_isolated_workspace, write_file]` co-batched:

- **Order A.** `write_file` probes first → routes to
  `isolated_pipeline.run_tool_call` → mid-RPC, `exit`'s `_teardown` kills
  the holder pid / unlinks scratch → spurious failures, partial state,
  scratch retained outside the lease window.
- **Order B (worse).** `exit` mutates the maps first → `write_file`
  probes after → routing returns `None` → write routes to **shared
  OCC** via `_dispatch_layer_stack_file_request`
  (`workspace_tool_dispatch.py:78-91`). The write that the model intended
  for private scratch is **published to the shared workspace**, violating
  the discard-boundary invariant at
  `docs/architecture/tools/isolated-workspace.html:140-143`.

For `[enter_isolated_workspace, write_file]` co-batched: more dangerous
still. The **first write of an isolated session** can leak to shared OCC
before the handle becomes visible. The model believed it had just
entered isolated mode; the write lands in shared.

### Production blast radius

- **Production `Intent.LIFECYCLE` tools: exactly 2.**
  - `backend/src/tools/isolated_workspace/enter_isolated_workspace/definition.py:30`
  - `backend/src/tools/isolated_workspace/exit_isolated_workspace/definition.py:30`
  - The mock at
    `backend/src/task_center_runner/agent/mock/plugin_workspace_probe.py:749`
    is test-only and exercises the rejection path at
    `backend/src/sandbox/ephemeral_workspace/plugin/op_registry.py:120-122`.
- **`dispatch_workspace_tool_call` callers: exactly 1.**
  - `backend/src/sandbox/daemon/builtin_operations.py:72` is the daemon
    RPC handler. No tool body re-enters dispatch.
- **`_check_plugin_block` callers: exactly 1.**
  - `backend/src/sandbox/daemon/rpc/dispatcher.py:84`. The filter
    `op_name.startswith("api.plugin.") or op_name.startswith("plugin.")`
    excludes lifecycle ops (`api.isolated_workspace.{enter,exit}` at
    `dispatcher.py:255-282,379-380`). No recursive re-entry.

---

## Design

### E1 / E2 — Option A: Engine lifecycle-batch policy

Add a check in `_dispatch_deferred_tool_calls` (`dispatch.py`)
immediately after `_record_tool_batch_rejection` (line 261). For batches
containing ≥1 `Intent.LIFECYCLE` tool:

- **>1 LIFECYCLE in batch:** all LIFECYCLE calls rejected.
  - Error text: `"Multiple lifecycle tools in one batch; engine cannot
    choose ordering. Resubmit each lifecycle call in its own batch."`
  - Result: `is_error=True` `ToolResultBlock` for every LIFECYCLE call.
- **=1 LIFECYCLE + ≥1 sibling:** **siblings rejected; lifecycle
  dispatches normally.**
  - Sibling error text: `"{lifecycle_tool} changes workspace routing;
    sibling tools ({names}) were rejected to avoid ordering ambiguity.
    The lifecycle call executed. Resubmit the rejected tools in the next
    batch."`
  - Result: `is_error=True` `ToolResultBlock` for each sibling;
    LIFECYCLE call enters `_dispatch_single_foreground_tool`.

Reuse the rejection envelope from `_record_tool_batch_rejection`,
adapted for **partial rejection** (lifecycle call still dispatches).

**Why divergence from terminal-tool precedent is justified:**
Terminal-tool rejection (`dispatch.py:153-172`) rejects the terminal +
siblings because terminal+sibling co-batch is a model contract
violation (terminals must be alone). Lifecycle+sibling is **not** a
contract violation — it is a valid sequence the engine cannot order
safely. Rejecting the lifecycle call would force the agent into a retry
loop where enter/exit never lands. Lifecycle must dispatch; only
siblings are rejected.

### D1 / D2 / D3 — Option C: Daemon per-agent dispatch state

Introduce `AgentDispatchState` per agent in
`backend/src/sandbox/daemon/workspace_tool_dispatch.py`.

```python
@dataclass
class AgentDispatchState:
    entry_lock: asyncio.Lock           # short-held: protects map mutation + counter
    inflight: int = 0                  # dispatches currently running
    inflight_zero: asyncio.Event       # set when inflight == 0
    exit_pending: bool = False         # new dispatches reject while True

_AGENT_DISPATCH_STATES: dict[str, AgentDispatchState] = {}
_STATES_DICT_LOCK = asyncio.Lock()     # only held for dict insert/remove
```

**Lazy creation:** `_AGENT_DISPATCH_STATES[agent_id]` is created on
first **dispatch** (not on enter). Consequence: if enter fails before
any dispatch, no state exists; nothing to clean up. If enter succeeds
but the agent immediately exits without dispatch, exit's
`_STATES_DICT_LOCK` lookup returns missing; exit fast-paths with no
drain (inflight implicitly zero).

#### Dispatch path (`dispatch_workspace_tool_call`)

1. `async with _STATES_DICT_LOCK`: get-or-create state for `agent_id`.
2. `async with state.entry_lock`:
   - If `state.exit_pending`: raise
     `LifecycleError("lifecycle_in_progress", "exit_isolated_workspace
     is draining; retry after exit completes")`.
   - `pipeline = _active_isolated_pipeline_for(agent_id)`.
   - `state.inflight += 1`; `state.inflight_zero.clear()`.
3. Release `entry_lock`. Run dispatch unlocked (the RPC body executes
   without holding any lock).
4. **In `finally`** (covers failed/cancelled dispatches):
   `async with state.entry_lock`: `state.inflight -= 1`; if zero,
   `state.inflight_zero.set()`.

#### Exit path (`workspace_handle_lifecycle.py:206`)

1. `async with _STATES_DICT_LOCK`: get state for `agent_id`. If missing
   (enter succeeded but no dispatch ever happened): fast-path exit, no
   drain.
2. `async with state.entry_lock`: `state.exit_pending = True`;
   `inflight_snapshot = state.inflight`.
3. If `inflight_snapshot > 0`:
   `await asyncio.wait_for(state.inflight_zero.wait(), timeout=grace_s)`.
   - On timeout: return
     `ExitIsolatedWorkspaceResult(success=False,
     error=LifecycleError("exit_drain_timeout",
     details={"inflight": N, "grace_s": G}))`; **maps untouched**;
     `exit_pending` reset to False so retry can proceed.
4. `async with state.entry_lock`: re-acquire (drain done). Inner
   `async with self._map_lock`: mutate `_by_agent` / `_handles`. Release
   `_map_lock`. Release `entry_lock`.
5. Run `_teardown` **outside** all locks.
6. `async with _STATES_DICT_LOCK`: delete
   `_AGENT_DISPATCH_STATES[agent_id]` (cleanup).

#### Plugin gate (`dispatcher.py:214-233`)

Wrap the `iws.get_handle(agent_id)` probe in
`async with state.entry_lock`. If `state.exit_pending`, return the
existing `forbidden_in_isolated_workspace` error kind. Otherwise capture
the probe result, increment inflight, release lock, proceed. The plugin
op then runs unlocked; on completion, decrement inflight in `finally`
as in the dispatch path.

### Lock acquisition order (AC9)

**If both `entry_lock` (per-agent) and `_map_lock` (process-wide) are
held by the same task, `entry_lock` must be acquired first.**

- Enter's existing `_map_lock`-solo acquisitions at
  `workspace_handle_lifecycle.py:49-63` and `:100-102` are **unchanged**.
  Enter never holds `entry_lock`.
- The exit drain prelude is the only new code path that holds both. It
  acquires `entry_lock` outer, `_map_lock` inner.
- `_assert_lock_order_in_test_mode` instruments `entry_lock.__aenter__`
  and `_map_lock.__aenter__` to maintain a **per-task acquisition stack
  of `(lock_name, monotonic_ts)` tuples**. Inner-lock acquisition asserts
  the outer is present in the same task's stack with an earlier
  timestamp.

### Cancellation semantics on exit drain timeout

- Exit returns failure with `kind="exit_drain_timeout"`,
  `details={"inflight": N, "grace_s": G}`.
- Maps and `_teardown` are NOT executed.
- `exit_pending` is reset to False so future exit attempts can re-try.
- The handle remains live; the agent can retry exit later.
- `_AGENT_DISPATCH_STATES[agent_id]` is retained (cleanup deferred to
  the first successful exit).

---

## Acceptance Criteria

| # | Criterion | Verified by |
|---|---|---|
| AC1 | Batch with 1 LIFECYCLE + ≥1 non-LIFECYCLE → siblings get `is_error=True` ToolResultBlocks with the AC1 message; **lifecycle dispatches normally**. | `test_tool_call_dispatch_lifecycle_siblings_rejected_lifecycle_executes` asserts `tool_results[lifecycle_idx].is_error is False` and `tool_results[sibling_idx].is_error is True`. |
| AC2 | Batch with >1 LIFECYCLE → all LIFECYCLE calls rejected. | `test_tool_call_dispatch_multiple_lifecycle_rejected`. |
| AC3 | Solo `[enter_isolated_workspace]` / `[exit_isolated_workspace]` continue to succeed end-to-end. | `test_tool_call_dispatch_solo_lifecycle_succeeds`. |
| AC4 | Non-LIFECYCLE batches continue to parallelize. | `test_tool_call_dispatch_parallel_non_lifecycle_unchanged`. |
| AC5 | Integration matrix: `[exit, write_file]`, `[enter, write_file]`, `[exit, plugin_op]`, `[enter, plugin_op]`, `[exit, shell]`. For each: shared OCC manifest+root_hash **byte-identical pre/post**. | `test_isolated_workspace_lifecycle_batch_shared_occ_untouched`. |
| AC6 | Counter `engine.tool_dispatch.lifecycle_batch_rejected{lifecycle_tool, sibling_count_bucket}` (cardinality-safe labels; `agent_id` is a structured-log dimension only) + audit event via `lifecycle_operation`. | `test_lifecycle_batch_rejection_emits_counter_and_audit`. |
| AC7 | Deterministic test using `_TestableAgentDispatchState` with `wait_until_acquired` / `proceed` `asyncio.Event`s. Asserts: (a) exit waits for in-flight dispatch; (b) post-exit dispatch routes to shared OCC; (c) timeout path returns `exit_drain_timeout` with maps intact. | `test_agent_dispatch_state_serializes_exit_against_inflight_dispatch`. |
| AC8 | Drain primitive explicitly testable: (a) inflight=0 → exit fast-paths; (b) inflight=N → exit blocks until N→0 or timeout; (c) timeout → exit fails cleanly, retry succeeds after drain. | `test_exit_drain_inflight_zero_fast_path`, `test_exit_drain_waits_for_inflight`, `test_exit_drain_timeout_then_retry_succeeds`. |
| AC9 | Lock ordering assertion: per-task `(lock_name, monotonic_ts)` stack; inner-lock acquisition asserts outer is present with earlier timestamp. Rule: **if both locks are held by the same task, `entry_lock` is outer.** | `test_lock_order_entry_outer_map_inner_assertion`. |
| AC10 | CI lint `backend/tools/lint_dispatch_callsites.py` fails on any new caller of `dispatch_workspace_tool_call` outside `backend/src/sandbox/daemon/`, AND asserts `_check_plugin_block` has exactly one caller. Wired into `make lint`. | `test_lint_dispatch_callsites_baseline_passes`, `test_lint_dispatch_callsites_extra_caller_fails`. |
| AC11 | **[observability, non-blocking]** Perf benchmark `tests/perf/test_workspace_dispatch_lock_overhead.py` asserts p99 dispatch entry overhead < 100µs under N=32 concurrent dispatches per agent. Failure produces a warning artifact, **not** a CI-red. | Manual review of warning artifact. |

---

## Implementation steps

Each step is a single PR-grain commit. Order is mandatory because
later steps depend on earlier ones.

1. **Engine — Option A.** Add lifecycle-batch rejection in
   `_dispatch_deferred_tool_calls` after `_record_tool_batch_rejection`
   (`backend/src/engine/tool_call/dispatch.py:261`). Compute via
   `tool_def.intent == Intent.LIFECYCLE`. Partial-rejection envelope:
   lifecycle dispatches; siblings get `is_error=True`. Lands AC1–AC4.

2. **Daemon — `AgentDispatchState` scaffolding.** Introduce
   `AgentDispatchState`, `_AGENT_DISPATCH_STATES`, `_STATES_DICT_LOCK`
   in `backend/src/sandbox/daemon/workspace_tool_dispatch.py`. No
   behavioral change yet — only the dataclass + globals. Test scaffolding
   `_TestableAgentDispatchState` introduced.

3. **Daemon dispatch path.** Wrap `_active_isolated_pipeline_for + dispatch`
   in entry_lock + inflight counter as per §Design. The `finally` block
   decrements inflight regardless of success/failure/cancellation.
   Lands D1.

4. **Daemon exit path.** Drain via `inflight_zero` event with `grace_s`
   timeout in
   `backend/src/sandbox/isolated_workspace/_control_plane/workspace_handle_lifecycle.py:206`.
   Inner `_map_lock` preserved. Cleanup state from
   `_AGENT_DISPATCH_STATES` on success. Lands D2 + AC7 + AC8.

5. **Plugin gate.** Same entry_lock acquisition + `exit_pending` check
   in `backend/src/sandbox/daemon/rpc/dispatcher.py:214-233`. Lands D3.

6. **Lock order assertion.** Implement
   `_assert_lock_order_in_test_mode` with per-task acquisition stack.
   Wire into both `entry_lock.__aenter__` and `_map_lock.__aenter__`
   when `EOS_TEST_MODE=true`. Lands AC9.

7. **CI lint guard.** `backend/tools/lint_dispatch_callsites.py` —
   greps for `dispatch_workspace_tool_call` outside
   `backend/src/sandbox/daemon/` and `_check_plugin_block` callers
   outside `dispatcher.py`. Wired into `make lint`. Lands AC10 / G1.

8. **Observability.** Counter
   `engine.tool_dispatch.lifecycle_batch_rejected{lifecycle_tool, sibling_count_bucket}`
   + audit event via `lifecycle_operation`. Lands AC6 / O1.

9. **Architecture doc refresh.** Update
   `docs/architecture/tools/isolated-workspace.html` §taskcenter-workflow
   (line 166): replace the "later" hedge with explicit two-layer
   enforcement language (engine policy + daemon quiesce primitive).
   Refresh `data-last-reviewed-commit` metadata. Lands G2.

10. **Perf tripwire.** `tests/perf/test_workspace_dispatch_lock_overhead.py`
    p99 < 100µs under N=32 concurrent dispatches per agent. Warning
    artifact on failure; not a CI gate. Lands AC11.

---

## Test plan

### Unit (engine) — `backend/tests/unit_test/test_engine/test_tool_call_dispatch*`

- `test_tool_call_dispatch_lifecycle_siblings_rejected_lifecycle_executes`
  — covers AC1.
- `test_tool_call_dispatch_multiple_lifecycle_rejected` — covers AC2.
- `test_tool_call_dispatch_solo_lifecycle_succeeds` — covers AC3.
- `test_tool_call_dispatch_parallel_non_lifecycle_unchanged` — covers
  AC4.
- `test_lifecycle_batch_rejection_emits_counter_and_audit` — covers
  AC6.

### Unit (daemon) — `backend/tests/unit_test/test_sandbox/test_workspace_tool_dispatch*`

- `test_agent_dispatch_state_serializes_exit_against_inflight_dispatch`
  — covers AC7 using `_TestableAgentDispatchState`.
- `test_exit_drain_inflight_zero_fast_path` — covers AC8a.
- `test_exit_drain_waits_for_inflight` — covers AC8b.
- `test_exit_drain_timeout_then_retry_succeeds` — covers AC8c.
- `test_lock_order_entry_outer_map_inner_assertion` — covers AC9.
- `test_plugin_gate_exit_pending_returns_forbidden` — covers D3.

### Integration — `backend/tests/unit_test/test_sandbox/test_isolated_workspace_lifecycle_batch.py`

- `test_isolated_workspace_lifecycle_batch_shared_occ_untouched` —
  covers AC5 matrix. For each of `[exit, write_file]`,
  `[enter, write_file]`, `[exit, plugin_op]`, `[enter, plugin_op]`,
  `[exit, shell]`:
  - Snapshot `services.manager.read_active_manifest()` root_hash before.
  - Submit the batch via the engine dispatch path.
  - Assert sibling result is `is_error=True`.
  - Assert manifest root_hash unchanged.
  - Assert lifecycle result is correct (enter succeeded / exit drained).

### E2E — `backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace`

- `test_batched_lifecycle_prompt_retry_succeeds` — model emits batched
  lifecycle prompt → engine returns error → mock model retries with
  separate batches → both succeed.

### Lint/CI — `backend/tools/lint_dispatch_callsites.py`

- `test_lint_dispatch_callsites_baseline_passes` — covers AC10 baseline.
- `test_lint_dispatch_callsites_extra_caller_fails` — introduces a
  synthetic extra caller in a tmp dir and asserts the lint fails.

### Perf (non-blocking) — `tests/perf/test_workspace_dispatch_lock_overhead.py`

- `test_dispatch_entry_overhead_p99_under_concurrent_load` — N=32
  concurrent dispatches per agent; assert p99 entry overhead < 100µs;
  failure produces warning artifact.

---

## Detailed item ledger

<a id="e1"></a>
### E1 — Engine batches `[exit_isolated_workspace, write_file]` concurrently → write to shared OCC after exit

| What | Where |
|---|---|
| Race today | `backend/src/engine/tool_call/dispatch.py:401-404` creates concurrent `asyncio` tasks for both calls. The write's `_active_isolated_pipeline_for` probe (`backend/src/sandbox/daemon/workspace_tool_dispatch.py:30-37`) returns `None` after exit's map mutation completes → write routes to `_dispatch_layer_stack_file_request` (`workspace_tool_dispatch.py:78-91`) → shared OCC. |
| What to land | Implementation step 1 (Option A engine batch policy): reject siblings, dispatch lifecycle. AC1 covers exact behavior. |
| Verify | `test_tool_call_dispatch_lifecycle_siblings_rejected_lifecycle_executes` asserts the sibling result has `is_error=True` with the AC1 message; lifecycle result has `is_error=False`. Integration test `test_isolated_workspace_lifecycle_batch_shared_occ_untouched[exit_write_file]` asserts shared OCC root_hash unchanged. |

<a id="e2"></a>
### E2 — Engine batches `[enter_isolated_workspace, write_file]` concurrently → first write of isolated session leaks to shared OCC

| What | Where |
|---|---|
| Race today | Symmetric to E1. The write's probe runs before enter completes the map mutation → returns `None` → routes to shared OCC. The model believed it had just entered isolated mode; the write lands in shared. This is **more dangerous than E1** because the leaked write is by definition the first write of an intended-isolated session — the model has no reason to suspect leakage. |
| What to land | Same as E1 (Option A engine batch policy handles both enter and exit symmetrically). |
| Verify | `test_isolated_workspace_lifecycle_batch_shared_occ_untouched[enter_write_file]`. |

<a id="d1"></a>
### D1 — Daemon `_active_isolated_pipeline_for` lockless probe bypassed by direct RPC

| What | Where |
|---|---|
| Race today | `backend/src/sandbox/daemon/workspace_tool_dispatch.py:30-37` reads `pipeline.get_handle(agent_id)` without holding any per-agent lock; routes accordingly. Any caller of `dispatch_workspace_tool_call` that arrives during exit's `_teardown` window (or before enter's map mutation completes) sees inconsistent routing. Option A protects only the engine entry path. |
| What to land | Implementation step 2 + step 3 (Option C scaffolding + dispatch wrap). Per-agent `entry_lock` acquired around `_active_isolated_pipeline_for + _dispatch_via_workspace_pipeline`. |
| Verify | `test_agent_dispatch_state_serializes_exit_against_inflight_dispatch` (AC7); enumeration check confirms `dispatch_workspace_tool_call` has exactly one caller (`backend/src/sandbox/daemon/builtin_operations.py:72`). |

<a id="d2"></a>
### D2 — Exit removes maps under `_map_lock`, runs `_teardown` outside the lock; in-flight foreground RPCs not drained

| What | Where |
|---|---|
| Race today | `backend/src/sandbox/isolated_workspace/_control_plane/workspace_handle_lifecycle.py:212-227`: `async with self._map_lock` → `del self._by_agent[agent_id]` → release `_map_lock` → `await self._teardown(handle, …)`. Any in-flight foreground RPC that captured a `pipeline` reference before the map mutation continues running while `_teardown` tears down the scratch / kills the holder pid. `_cancel_by_agent` (`backend/src/sandbox/host/isolated_workspace_lifecycle.py:111-115`) drains **background** tasks only; foreground in-flight RPCs are not drained. |
| What to land | Implementation step 4 (Option C exit drain): exit acquires `entry_lock`, sets `exit_pending`, awaits `inflight_zero` with `grace_s` timeout, then mutates maps + tears down. Lock acquisition order asserted (AC9). |
| Verify | `test_exit_drain_waits_for_inflight` (AC8b), `test_exit_drain_timeout_then_retry_succeeds` (AC8c). |

<a id="d3"></a>
### D3 — Plugin gate uses the same lockless `get_handle` probe

| What | Where |
|---|---|
| Race today | `backend/src/sandbox/daemon/rpc/dispatcher.py:214-233` (`_check_plugin_block`) probes `iws.get_handle(agent_id)` without coordination. Order-A: plugin op probes first, observes handle, proceeds inside isolated pipeline while teardown races; Order-B: exit removes handle first, plugin op probes after, returns `None` from gate → plugin op proceeds to shared workspace. |
| What to land | Implementation step 5: wrap the gate probe in `async with state.entry_lock`. If `state.exit_pending`, return existing `forbidden_in_isolated_workspace` error kind. Otherwise capture probe result, increment inflight, release lock, proceed. |
| Verify | `test_plugin_gate_exit_pending_returns_forbidden`; integration `test_isolated_workspace_lifecycle_batch_shared_occ_untouched[exit_plugin_op]` and `[enter_plugin_op]`. Enumeration confirms `_check_plugin_block` has exactly one caller (`dispatcher.py:84`) and lifecycle ops bypass the gate via prefix filter. |

<a id="o1"></a>
### O1 — Observability: rejection counter + audit event

| What | Where |
|---|---|
| Stub today | No counter or audit signal exists when the engine rejects a batch — neither for the existing terminal-tool path nor the new lifecycle-batch path. Operational visibility of rejection volume requires a counter. |
| What to land | Implementation step 8: emit `engine.tool_dispatch.lifecycle_batch_rejected{lifecycle_tool, sibling_count_bucket}` from the rejection site. `agent_id` is a structured-log dimension only (high cardinality). Emit an audit event via `lifecycle_operation` (`backend/src/sandbox/audit/lifecycle.py`) so the rejection appears in trace bundles alongside enter/exit events. |
| Verify | `test_lifecycle_batch_rejection_emits_counter_and_audit` (AC6). |

<a id="g1"></a>
### G1 — CI lint guard for dispatch callers

| What | Where |
|---|---|
| Stub today | No CI guard prevents a new caller of `dispatch_workspace_tool_call` from appearing outside the daemon RPC layer. Pre-mortem #2's "review nesting" mitigation needs a mechanical enforcement to survive future refactors. |
| What to land | Implementation step 7: `backend/tools/lint_dispatch_callsites.py` greps for `dispatch_workspace_tool_call` callers outside `backend/src/sandbox/daemon/` and for `_check_plugin_block` callers outside `dispatcher.py`. Wired into `make lint`. |
| Verify | `test_lint_dispatch_callsites_baseline_passes`, `test_lint_dispatch_callsites_extra_caller_fails` (AC10). |

<a id="g2"></a>
### G2 — Architecture doc refresh

| What | Where |
|---|---|
| Stub today | `docs/architecture/tools/isolated-workspace.html:166` says lifecycle tools change routing for "later" tool calls. "Later" is not enforced anywhere. Future readers will assume engine ordering is sufficient. |
| What to land | Implementation step 9: replace the "later" hedge with explicit two-layer enforcement language (engine batch policy at `dispatch.py` + daemon per-agent quiesce primitive at `workspace_tool_dispatch.py`). Refresh `data-last-reviewed-commit` metadata to point at the Phase 4 merge commit. |
| Verify | `git log -p docs/architecture/tools/isolated-workspace.html` shows the wording change at line 166 and the new metadata commit hash. |

---

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| **Lock-induced perf regression** on hot file-I/O paths. | Lock is per `agent_id`; multi-agent workloads unaffected. Single-agent: one `asyncio.Lock` acquire per call (~µs). AC11 measures p99 < 100µs as a tripwire. |
| **Lock granularity wrong** — long shell holds inflight slot, blocks exit on default `grace_s=5.0`. | `entry_lock` is narrowly held (counter + map mutation only); RPC body runs unlocked. Drain via separate `inflight_zero` event. On drain timeout, exit returns `exit_drain_timeout` and agent can retry with larger grace or pre-cancel via background path. |
| **Plugin-gate lock acquisition order** vs other plugin-side locks. | Acquire `entry_lock` outermost in all paths; documented in lock-introduction commit; enforced by AC9 assertion. |
| **Engine rejection lands but a future caller bypasses it.** | Defense in depth via Option C closes the daemon path; AC10 CI guard blocks new direct callers of `dispatch_workspace_tool_call`. |
| **Dict-state leak on drain-timeout retry storm.** | Cleanup deferred to first successful exit. Retained state is reused by subsequent dispatches/exit retries; no per-exit allocation cost. GC'd with daemon process on permanent failure. |
| **Existing prompts already batch lifecycle with siblings.** | AC1 error text is model-actionable; lifecycle still executes so the agent's intent advances; pre-merge grep of `task_center_runner/tests` and `docs/architecture` for examples; counter (AC6) surfaces production volume during gradual rollout. |

---

## Consensus trail (for audit)

| Stage | Verdict | Output |
|---|---|---|
| v1 Planner draft | — | A as primary, C as follow-up |
| Architect iter-1 | ENDORSE-WITH-CHANGES | promote C into same PR; soften AC1 (lifecycle executes); add `[enter, write]` and plugin variants to AC5 |
| v2 Planner revise | — | A + C same PR; AC1 partial rejection; expanded AC5 matrix |
| Critic iter-1 | ITERATE | 5 required changes (AC8/granularity contradiction; `_map_lock` interaction; AC7 determinism; enumerated audit + CI guard; dict teardown contract) |
| v3 Planner revise | — | explicit `inflight_zero` quiesce primitive; OUTER/INNER lock order; `_TestableAgentDispatchState` deterministic test; single-caller enumeration; lazy-on-dispatch state lifecycle |
| Architect iter-2 | ENDORSE-WITH-CHANGES | reword AC9 ("if both are held"); extend enumeration to `_check_plugin_block` callers; document lazy-on-dispatch explicitly |
| v3-final Planner revise | — | all 3 Architect deltas + 3 Critic polish items folded |
| Critic iter-2 | **APPROVE** | 3 optional non-blocking polish items |

---

## References

- `backend/src/engine/tool_call/dispatch.py:153-172` — existing terminal-tool rejection precedent that AC1 mirrors with partial-rejection divergence.
- `backend/src/engine/tool_call/dispatch.py:259-309` — `_dispatch_deferred_tool_calls`, the Option A patch site.
- `backend/src/engine/tool_call/dispatch.py:352-416` — `_dispatch_many_foreground_tools`, the concurrent-task dispatcher that triggers the race.
- `backend/src/sandbox/daemon/workspace_tool_dispatch.py:30-37` — the lockless probe.
- `backend/src/sandbox/daemon/workspace_tool_dispatch.py:53-75` — `dispatch_workspace_tool_call`, the Option C patch site.
- `backend/src/sandbox/daemon/builtin_operations.py:72` — the sole caller of `dispatch_workspace_tool_call`.
- `backend/src/sandbox/isolated_workspace/_control_plane/workspace_handle_lifecycle.py:206-227` — exit's map mutation + teardown sequence.
- `backend/src/sandbox/isolated_workspace/_control_plane/workspace_handle_lifecycle.py:49-102` — enter's `_map_lock` acquisitions (unchanged by AC9 reword).
- `backend/src/sandbox/daemon/rpc/dispatcher.py:84` — sole caller of `_check_plugin_block`.
- `backend/src/sandbox/daemon/rpc/dispatcher.py:214-233` — `_check_plugin_block`, plugin gate patch site (D3).
- `backend/src/sandbox/daemon/rpc/dispatcher.py:255-282,379-380` — lifecycle op handlers; bypass plugin gate via prefix filter.
- `backend/src/sandbox/host/isolated_workspace_lifecycle.py:48-60` — enter's background-task in-flight gate.
- `backend/src/sandbox/host/isolated_workspace_lifecycle.py:111-115` — exit's background-task drain (foreground RPCs not covered; D2).
- `backend/src/tools/isolated_workspace/enter_isolated_workspace/definition.py:30` — `Intent.LIFECYCLE` registration.
- `backend/src/tools/isolated_workspace/exit_isolated_workspace/definition.py:30` — `Intent.LIFECYCLE` registration.
- `backend/src/tools/_framework/core/decorator.py:66-70` — intent validation (not the right place for batching policy).
- `backend/src/sandbox/ephemeral_workspace/plugin/op_registry.py:120-122` — plugin LIFECYCLE rejection at registration.
- `backend/src/task_center_runner/agent/mock/plugin_workspace_probe.py:749` — mock test scaffolding (not a production emitter).
- `docs/architecture/tools/isolated-workspace.html:140-143` — discard-boundary invariant.
- `docs/architecture/tools/isolated-workspace.html:166` — "later" invariant claim (refresh target, G2).

*End of Phase 4 plan. Implementation lands in a single PR per the
implementation-step ordering above.*
