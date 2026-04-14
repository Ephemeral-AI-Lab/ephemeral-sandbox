# Task Center Redesign — Key Changes Summary

**Date:** 2026-04-14  
**Source documents:**
- [TaskCenter + DAG Unification](task-center-dag-unification.md)
- [Dynamic Replanning Blocker Protocol](dynamic-replanning-blocker-protocol.md)
- [TaskCenter Active Mode](task-center-active-mode.md)

---

## What changes

### 1. TaskCenter becomes the single owner of task lifecycle

**Before:** Three components split task management — TaskCenter (notes), Dispatcher (orchestration), DispatcherStore (SQL persistence). Every consumer bridges the gap. context_for needs a task_lookup callback to reach the DAG. read_sibling_notes needs a dispatcher_store to resolve subtrees. The executor mediates between all three on every event.

**After:** TaskCenter absorbs both Dispatcher and DispatcherStore. It owns task records, dependencies, status transitions, plan insertion, cascade operations, notes, and context building. Consumers call one API.

**Key signature changes:**
- `context_for(task, *, max_context_bytes)` — no more task_lookup or file_change_store callbacks
- `read_sibling_notes(parent_id, *, keyword, scope_paths)` — no more dispatcher_store parameter
- `complete_task(task_id, result)` — no run_id parameter (stored internally)
- `pause_running_task` / `resume_paused_tasks` / `cancel_paused_tasks` — renamed from block/unblock

**Deleted:** `dispatcher.py`, `dispatcher_store.py`

**New:** `dispatch_queue.py` — thin component (~60 lines) with only `pop_ready` (no separate `claim`), extracted for SQL atomicity (FOR UPDATE SKIP LOCKED). `mark_running` moved to TaskCenter.

### 2. Blocker-aware pause/fix/resume replaces independent retries

**Before:** When a completed task breaks shared code, every sibling fails independently. Each retries, each fails again, each triggers a separate replan. No mechanism detects "these failures share a root cause."

**After:** Three roles with strict separation:
- **Developer** — reports failure via `request_replan`. No blocker awareness.
- **Replanner** — assesses and decides. Three actions only: `add_tasks`, `declare_blocker`, `cancel_and_redraft`.
- **Conductor** — executes blocker mechanics. Zero LLM calls. Deterministic and fully testable.

**Blocker lifecycle:** declare -> assess running siblings+descendants via external_trigger agent -> pause YES verdicts -> spawn resolver fix task -> fix completes -> resume paused agents with checkpoint rehydration.

**DispatchQueue integration:** `pop_ready` accepts a `blocker_guard` callable from the Conductor. During an active blocker, `guard_pop_ready` allows only the resolver fix task to dispatch; all other READY candidates are skipped without mutating their state.

### 3. TaskCenter active mode replaces passive nudges

**Before:** An edit-based nudge in query.py injects a SystemReminderBlock hoping the agent calls post_note. The agent may ignore it. 24 lines of inline counter logic in query.py + 16 lines in daytona tools.

**After:** TaskCenter owns content quality. It tracks agent activity and spawns external_trigger agents to produce notes on agents' behalf. The agent is never interrupted. The note is guaranteed.

**Two triggers:**
- **Edit counter** — threshold 5 edits since last note. Resets on post_note or auto note. External trigger prompt: EDIT_CHECKPOINT_PROMPT ("what files were edited and why").
- **Turn counter** — threshold 10 turns since last posthook. Resets on any posthook call or auto note. External trigger prompt: TURN_CHECKPOINT_PROMPT ("status, findings, and blockers").

**check() signature:** `check(task_id, *, snapshot, api_client, model)` — receives a conversation snapshot and api_client directly, not an executor reference. Falls back to factual counter-based note when no api_client is available.

**Critical design point:** The turn prompt explicitly asks about blockers. This ensures that even silent agents surface systemic failures. The replanner sees these via read_sibling_notes — enabling early blocker detection across the full sibling set.

**Note attribution:** auto-generated notes use `agent_name + " (auto)"` suffix, posted under the original task's ID and scope_paths. System/checkpoint notes do not reset activity counters.

**query.py impact:** All posthook logic removed from query loop. Post-run submission handled by executor via `external_trigger.runner`. Conversation snapshots maintained by Conductor via `register_snapshot()`. No message injection, no display_messages mutation.

---

## How the three designs connect

```
TaskCenter (unified)
    owns task lifecycle, notes, context, planning
        |
        +-- Active Mode (auto-notes)
        |       monitors edit/turn activity
        |       spawns external_trigger agents for silent agents
        |       surfaces blockers early
        |
        +-- Blocker Protocol
                replanner reads sibling notes (including auto-notes)
                declares blocker when systemic pattern detected
                Conductor pauses/resumes via TaskCenter
                fix task resolves root cause once

DispatchQueue (thin)
    pop_ready only (mark_running moved to TaskCenter)
    accepts blocker_guard from Conductor
    guard returns True=allow, False=skip (no mutation)

External Trigger Module (external_trigger/)
    runner.run()            shared LLM loop (tool_choice="any", retry until success)
    run_external_trigger()  agent identity wrapper for mid-run triggers
    Used by: Conductor (pause assessment), TaskCenter (checkpoint notes)
    Also by: Executor (post-run submission via runner.run() directly)
```

Active mode feeds the blocker protocol: auto-generated notes surface shared failures that agents failed to report. The replanner sees the pattern via read_sibling_notes and declares a blocker. The Conductor executes mechanically through TaskCenter. One fix resolves the root cause for all paused siblings.

### 4. Tool type classification and unified tool execution

**New:** `ToolType = Literal["normal", "post_run", "external_trigger"]` on `BaseTool`. Each tool can have multiple types (e.g., `post_note` is both `external_trigger` and `post_run`).

**External trigger** (mid-run): Conductor/TaskCenter spawn ephemeral agents via `run_external_trigger()` which calls `runner.run()` with constrained tools and a frozen conversation snapshot. The assessed agent is never interrupted.

**Post-run** (after query loop): Executor calls `runner.run()` directly with post_run tools (`post_note`, `submit_plan`, `request_replan`, etc.) after the query loop returns. All posthook logic removed from query.py (~100 lines).

Both phases use the same `runner.run()` — `tool_choice="any"` with Pydantic validation retry up to `max_turns` (default 10). Raises `RuntimeError` if exhausted.

---

## Executor before/after

**Before (5 references, 6 calls per task):**
```
task = dispatcher.store.pop_ready(run_id)
dispatcher.mark_running(task.id, agent_run_id)
ctx = task_center.context_for(task, file_change_store, task_lookup, max_bytes)
result = run_agent(ctx)
dispatcher.complete(task.id, result)
task_center.post(completion_note)
```

**After (2 components, 4-step lifecycle per task):**
```
rec = dispatch_queue.pop_ready(run_id, conductor.guard_pop_ready)
task = task_center.mark_running(task.id, agent_run_id)
ctx = task_center.context_for(task)
run_agent(ctx)                              # query loop
result = executor._run_post_run(task, defn, ctx)  # posthook extraction
executor._dispatch(task, result)            # routes to complete/fail/retry/replan/blocker
    → task_center.complete_task(task.id, result)
```

---

## Files impact

| Action | Files |
|--------|-------|
| Modified | task_center.py, executor.py, conductor.py, team_run.py, context_builder.py, query.py (posthook removed), tools/core/base.py (ToolType), tools/posthook/toolkit.py (tool_types + DeclareBlockerTool), tools/context/toolkit.py (PostNoteTool multi-type) |
| New | dispatch_queue.py (~60 lines), persistence/task_store.py (SQL delegated from TaskCenter), external_trigger/ (runner, agent, pause_assessment, tc_note), tools/external_trigger/ (PauseVerdictTool) |
| Deleted | dispatcher.py (~640 lines), dispatcher_store.py (~840 lines), ephemeral_task/ (replaced by external_trigger/) |
| Untouched | models.py, task_record.py, schema.sql |
