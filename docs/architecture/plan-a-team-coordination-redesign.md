# Plan A: Team Coordination Redesign — Task Center Architecture

**Status:** IMPLEMENTED — Updated 2026-04-13 to reflect final implementation decisions and benchmark validation  
**Date:** 2026-04-12 (doc updated 2026-04-13)  
**Branch:** `codex/pydantic-benchmark-loop`  
**Author:** Architecture session  

---

## Table of Contents

1. [Design Goals & Constraints](#1-design-goals--constraints)
2. [Diagnosis: What the Current System Over-Engineers](#2-diagnosis-what-the-current-system-over-engineers)
3. [Architecture Overview](#3-architecture-overview)
4. [Data Model](#4-data-model)
5. [Task Center (Shared Context Log)](#5-task-center-shared-context-log)
6. [Plan & Execution](#6-plan--execution)
7. [Submission & PostAgentHook](#7-submission--postagenthook)
8. [Context Sharing & Inheritance](#8-context-sharing--inheritance)
9. [OCC, Code Intelligence & Exploration Cache](#9-occ-code-intelligence--exploration-cache)
10. [Toolkit Assignment](#10-toolkit-assignment)
11. [Task-Agnostic Flows](#11-task-agnostic-flows)
12. [Migration Phases](#12-migration-phases)
13. [Deletion Inventory](#13-deletion-inventory)
14. [PostgreSQL Infrastructure](#14-postgresql-infrastructure)

---

## 1. Design Goals & Constraints

### Hard Constraints

| # | Constraint |
|---|-----------|
| HC-1 | Planner's role is planning only. All agents adopt the ephemeral principle (no state between invocations). |
| HC-2 | Must integrate with existing OCC (Arbiter, Ledger) and Code Intelligence (LSP, SymbolIndex, CI Service). |
| HC-3 | PostAgentHook must always run successfully after agent completion. Submission is guaranteed. |
| HC-4 | `query.py` loop logic stays untouched (minimal 2-line gate in `_has_submission()` only). |

### Design Goals

| # | Goal | How This Plan Achieves It |
|---|------|--------------------------|
| G-1 | Simplest possible design | Replace 7-layer indirection with 2: agent calls tool → executor reads metadata |
| G-2 | High-speed changing codebase | Arbiter + Ledger detect real-time file contention; Task Center notes are append-only, never stale by design |
| G-3 | High parallelism | Append-only Task Center needs no read locks; Arbiter serializes only overlapping file edits, not agent coordination |
| G-4 | Perfect context sharing | Two read filters (deps, parent chain) + Ledger-based file change awareness. No tiered dedup. |
| G-5 | Task agnostic | Same dispatcher, same executor, same Task Center for greenfield, bugfix, feature, rebuild |
| G-6 | High-granularity decomposition | Planner submits fine-grained TaskSpecs. Nested planners create sub-plans. Budget limits prevent explosion. |
| G-7 | Subagent swarm output | Subagent results flow into Task Center as notes. Parent reads them via tag filter. No artifact store indirection. |
| G-8 | PostgreSQL as coordination kernel | One mature database replaces work queue (SKIP LOCKED), event bus (LISTEN/NOTIFY), lock manager (advisory locks), search index (GIN + FTS), and crash recovery (WAL). No custom coordination infrastructure. |

---

## 2. Diagnosis: What the Current System Over-Engineers

```
CURRENT: 7 layers between "planner decides" and "agent does it"

  Planner output
    → Posthook LLM (submit_plan_agent)        ← DELETED: extra LLM call
    → SubmitPlanTool validation                ← SIMPLIFIED: single pass
    → Phase A validation                       ← MERGED into single pass
    → Plan.from_dict deserialization           ← SIMPLIFIED: TaskSpec
    → Phase B validation (dispatcher-time)     ← DELETED: redundant
    → Task creation + Briefing attachment  ← SIMPLIFIED: no Briefing type
    → 3-tier briefing rendering + dedup        ← REPLACED: Task Center

PLAN A: 2 layers

  Planner calls submit_plan() tool
    → Single-pass validation + TaskSpec creation
    → Executor reads Task Center for context
```

### Components Removed vs Kept

| Component | Verdict | Rationale |
|-----------|---------|-----------|
| `Briefing` dataclass | DELETE | Replaced by `Note` in Task Center |
| `DependencyArtifact` dataclass | DELETE | Deps read from Task Center directly |
| `InMemoryArtifactStore` | DELETE | Task Center stores prose, not binary artifacts |
| 3-tier briefing renderer | DELETE | Single `task_center.context_for()` replaces 180 lines |
| `canonical_scope` + coherence tokens (briefing layer) | DELETE | Tags + scope_paths replace canonical scopes |
| `scout_briefings.py` (pressure, freshness, auto-promotion) | DELETE | Was briefing-layer concept, not OCC |
| Atlas service + store + model + freshness | DELETE | Optional `TaskCenterCache` for cross-run reuse |
| 5 posthook agent definitions | DELETE | Submission tools go to work agents directly |
| `agent_posthook.py` (execute_with_posthook) | DELETE | Deterministic `_posthook()` in executor |
| Phase A + Phase B validation split | MERGE | Single validation pass |
| Plan normalization (name inference) | SIMPLIFY | Leaner roster resolution |
| `Arbiter` (per-file OCC) | **KEEP** | Core write coordination |
| `Ledger` (edit audit log) | **KEEP** | Core edit history |
| `scope_packets.py` (contention snapshots) | **DELETE** | Arbiter catches conflicts at edit time; agents don't need pre-flight contention reports |
| `coordination.py` (scope helpers) | **DELETE** | `task.scope_paths` is read directly; no packet building needed |
| `CIToolkit` (LSP, grep, glob) | **KEEP** | Core code intelligence |
| `DaytonaToolkit` (sandbox ops) | **KEEP** | Core file I/O + codeact |
| `SubagentToolkit` (run_subagent) | **KEEP** | Planner exploration |
| `query.py` loop | **KEEP** | 2-line gate in `_has_submission()` only |
| `_has_submission()` mechanism | **KEEP + gate** | `posthook_enabled` flag |
| `Dispatcher` DAG + ready queue | **SIMPLIFY** | Replaced by PG-backed `SKIP LOCKED` queue (Section 14.6). No in-memory DAG state. |
| `Executor` pop-ready loop | **SIMPLIFY** | Remove `execute_with_posthook`, add `_posthook()` |

---

## 3. Architecture Overview

```
┌────────────────────────────────────────────────────────────────────┐
│ TeamRun                                                            │
│                                                                    │
│  ┌────────────┐   ┌──────────────┐   ┌──────────────────────────┐ │
│  │ PGDispatcher │   │ Task Center  │   │ CI Service               │ │
│  │ SKIP LOCKED  │   │ append-only  │   │  ├─ Arbiter (file OCC)   │ │
│  │ + budgets    │   │ note log     │   │  ├─ Ledger (edit audit)  │ │
│  └──────┬───────┘   └──────┬───────┘   │  ├─ SymbolIndex          │ │
│         │                  │           │  └─ LSP client            │ │
│    pop_ready()        read / post      └────────────┬─────────────┘ │
│         │                 │                        │               │
│  ┌──────▼─────────────────▼────────────────────────▼─────────┐    │
│  │ Executor  (N concurrent workers)                           │    │
│  │                                                            │    │
│  │  1. Pop ready Task                                     │    │
│  │  2. Build agent context:                                   │    │
│  │       task.task              ← planner's prose instruction   │    │
│  │       task_center.context_for(task)  ← deps + parent + ledger  │    │
│  │  3. Spawn EphemeralAgent with role-scoped toolkits         │    │
│  │  4. query.py loop runs (UNTOUCHED)                         │    │
│  │       agent uses code_intel, sandbox tools (EXISTING)      │    │
│  │       agent posts notes to Task Center (INTERMEDIATE)      │    │
│  │       agent calls submit_summary() or submit_plan() (TERMINAL) │    │
│  │       _has_submission() gate → exits early if submitted    │    │
│  │  5. _posthook() runs (DETERMINISTIC, always)               │    │
│  │  6. Dispatch result to PGDispatcher                                 │    │
│  └────────────────────────────────────────────────────────────┘    │
└────────────────────────────────────────────────────────────────────┘
```

### Rationale

| Design choice | Why it suits dynamic/parallel/swarm workloads |
|---------------|-----------------------------------------------|
| Append-only Task Center | Reads never lock. N agents post concurrently. No contention on the knowledge layer. |
| Arbiter for file-level OCC | Contention is serialized only at the file level, not at the context level. Two agents editing different files never block each other. |
| Single executor → runner call | One LLM invocation per task, not two (no posthook agent). Halves LLM cost for every team agent. |
| Arbiter + Ledger at tool level | OCC catches file conflicts at edit time — no pre-flight contention report needed. Agents just work; the Arbiter serializes overlapping writes automatically. |
| Tags on notes | Subagent swarm output tagged by scope. Consumers filter by tag, not by artifact ID. Scales to hundreds of notes without dedup machinery. |

---

## 4. Data Model

### 4.1 Note (replaces Briefing + DependencyArtifact)

The `Note` type is the single context primitive in the Task Center. Each note has an id, the task and agent that wrote it, plain-text content of any length and format, a wall-clock timestamp, an optional list of `scope_paths` for scope-filtered reads, and an optional `parent_note_id` for threading subagent notes back to their parent.

**Why `scope_paths` on Note:** The PostgreSQL schema stores `scope_ltree` on `task_notes`, and tools like `post_note(content, scope_paths?)`, `search_context(scope_paths?)`, and `ExplorationCache.check()` all rely on per-note scope metadata. Without `scope_paths` on the in-memory `Note`, the in-memory TaskCenter cannot reproduce the same scope-filtered reads that PostgreSQL provides, causing same-run context sharing to diverge from search/cache behavior. The field defaults to empty (unscoped notes are visible to all queries).

**Rationale:** One type replaces the former `Briefing`, `DependencyArtifact`, and `InMemoryArtifactStore`. LLMs parse prose better than JSON schemas. Agents post what they know; consumers read what's relevant.

### 4.2 TaskSpec (replaces WorkItemSpec)

`TaskSpec` is what the planner submits for each item in a plan. It has a local reference `id`, a plain-text `task` instruction (which is the briefing), an `agent` field accepting an exact name or role hint, a `deps` list of task IDs this depends on, `scope_paths` for file/directory hints used by OCC and note scoping, and a `cascade_policy` ("cancel" | "retry_first" | "continue") controlling dependent behavior on failure.

**Comparison with current `WorkItemSpec`:**

| Field | Current `WorkItemSpec` | New `TaskSpec` | Change |
|-------|----------------------|----------------|--------|
| `agent_name` | str | `agent` (str) | Renamed, accepts role hints |
| `payload` | dict (structured) | `task` (str, prose) | **Schema → prose** |
| `local_id` | str | `id` (str) | Renamed |
| `deps` | list[str] | list[str] | Same |
| `notes` | str | Absorbed into `task` | Merged |
| `timeout_seconds` | float | Removed | Executor default |
| `kind` | TaskKind | Removed | Inferred from agent role |
| `briefings` | list[Briefing] | Removed | `task` field IS the briefing |
| — | — | `scope_paths` (new) | OCC integration |
| — | — | `cascade_policy` (new) | Controls dependent behavior on failure |

**Rationale:** `task` replaces both `payload` and `briefings`. The planner writes one prose description instead of populating a JSON schema + attaching separate Briefing objects. `scope_paths` feeds both the OCC layer (Arbiter) and note scoping (PostgreSQL ltree + GiST index).

### 4.3 Task (simplified)

`Task` is the runtime representation of a dispatched work item. It carries the fields needed for execution and DAG tracking: `id`, `team_run_id`, `agent_name`, `status` (pending | ready | running | done | failed | cancelled), `task` (plain-text instruction), `deps`, `scope_paths`, a derived `scope_ltree` list for PostgreSQL queries, `cascade_policy`, `parent_id`, `root_id`, `depth`, `pending_dep_count` (decremented on dep completion; 0 = ready), `retry_count`, `max_retries`, `agent_run_id`, and timestamp fields (`created_at`, `started_at`, `finished_at`, `failure_reason`).

**Removed fields:** `kind`, `payload`, `briefings`, `dep_artifacts`, `artifact_ref`, `local_id`, `timeout_seconds`, `replan_source_id`.

**Rationale for each removal:**

| Removed field | Why safe to remove |
|---------------|-------------------|
| `kind` | Inferred from agent role: planner → expandable, everything else → atomic |
| `payload` | Replaced by `task` (prose) + `scope_paths` (OCC) |
| `briefings` | Replaced by `task` field + Task Center notes |
| `dep_artifacts` | Deps read from Task Center at context-build time, not snapshot at promotion time |
| `artifact_ref` | No artifact store. Agent output is a Note in Task Center. |
| `local_id` | Merged into `id` lifecycle (local during plan, resolved by dispatcher) |
| `timeout_seconds` | Use executor-level default. Agent `tool_call_limit` is the real budget. |
| `replan_source_id` | Replanner reads failure context from Task Center, not from a back-pointer |

### 4.4 Plan (simplified)

`Plan` contains a list of `TaskSpec` items and an optional `rationale` string.

### 4.5 ReplanPlan (simplified)

`ReplanPlan` has two lists: `add_tasks` (new `TaskSpec` items to insert) and `cancel_ids` (existing task IDs to cancel).

### 4.6 BudgetConfig (simplified)

`BudgetConfig` enforces runtime limits: `max_tasks` (50), `max_depth` (4), `max_plan_size` (50), `max_retries_per_item` (2), `max_replans_per_run` (5), `max_note_bytes` (100,000 — per-note size cap), and `max_total_note_bytes` (5,000,000 — aggregate cap).

**Removed:** `max_artifact_bytes`, `max_total_artifact_bytes`, `max_briefing_bytes`, `max_shared_briefings`, `max_reviewers_per_plan`, `require_reviewer_for_plan_size`.

**Rationale:** Artifact and briefing limits are irrelevant (no artifacts, no briefings). Reviewer constraints were plan-validation rules, not budget — they move into the single validation pass.

---

## 5. Task Center (Shared Context Log)

### 5.1 Data Structure

`TaskCenter` is an in-memory append-only log shared by all executors in a `TeamRun`. It holds a list of `Note` objects and exposes three primary operations: `post(note)` appends a note; `read(authors?, scope_paths?, since?, limit?)` returns filtered notes; and `context_for(task, file_change_store?, task_lookup?, max_context_bytes?)` builds the prioritized context string delivered to each agent at task start (implementation detailed in Section 8.2). It also stores the run's `goal` and `user_request` strings for reference.

**Implementation decision — in-memory notes, no NoteStore:** The original design specified a `NoteStore` for PG-backed note persistence with an in-memory fallback. The implementation chose in-memory only because all executors in a `TeamRun` share the same `TaskCenter` instance — cross-executor visibility is guaranteed without PostgreSQL. This eliminates the dual-path complexity (`_store_backed()`, `NoteStore`, `TaskNoteRecord` conversion) with no loss of functionality in single-process deployments. If multi-process scaling requires cross-process note visibility, a `NoteStore` can be reintroduced at that time.

**Implementation note — FileChangeStore replaces Arbiter parameter:** The original design specified an `Arbiter` object for file change awareness. The implementation passes `file_change_store` instead — the durable `FileChangeStore` (sync SQLAlchemy, backed by the `file_changes` table) provides cross-process visibility and crash recovery, while the Arbiter's in-memory ring buffer is limited to the current process. The `task_lookup` callable replaces the raw `pool` parameter for parent chain walks — cleaner than inline SQL.

### 5.2 Why Append-Only

| Property | Benefit for dynamic/parallel workloads |
|----------|---------------------------------------|
| No locks | list.append() is GIL-atomic. No read locks on append-only list. Zero contention on any path. |
| No mutation | A note posted at t=1 is still valid at t=100. No invalidation tracking needed. |
| No dedup | LLMs naturally handle overlapping prose. The 3-tier dedup machinery (canonical scopes, seen_scopes, seen_refs) is deleted. |
| Monotonic timestamps | `since` filter gives "what's new since I last looked" for free. |
| Simple checkpoint | `snapshot()` = `list(self._notes)`. No artifact store serialization. |
| Monotonic knowledge | Agents that start later see strictly more context. Knowledge never decreases — a formal property that append-only + immutable notes guarantee by construction. |

---

## 6. Plan & Execution

### 6.1 Planner Flow

```
┌─────────────────────────────────────────────────────┐
│ Planner (ephemeral, planning-only)                   │
│                                                      │
│ Toolkits:                                            │
│   code_intelligence (READ-ONLY)                      │
│   subagent (spawn explorer for pre-plan recon)       │
│   task_center_read (read notes)                      │
│   submission (submit_plan ONLY)                      │
│                                                      │
│ CANNOT: write files, run shell, call submit_summary() │
│                                                      │
│ Flow:                                                │
│   1. Read Task Center for existing context           │
│   2. Optionally spawn explorer subagents              │
│   3. Read explorer findings from Task Center         │
│   4. Decompose work into TaskSpecs                   │
│   5. Call submit_plan(tasks=[...], rationale="...")   │
│      → tool writes to metadata["submitted_output"]   │
│      → _has_submission() gate → loop exits           │
└─────────────────────────────────────────────────────┘
```

**Rationale (HC-1):** Planner has no `sandbox_operations` toolkit. It literally cannot `write_file()`, `edit_file()`, `codeact()`, or `shell()`. The only terminal tool in its toolkit is `submit_plan()`. If it doesn't call it, the posthook fails the task.

### 6.2 Single-Pass Plan Validation

Merges current Phase A + Phase B into one pass, run inside `SubmitPlanTool.execute()`:

```
SubmitPlanTool.execute(arguments, context)
  │
  ├── 1. Structural checks
  │     - Plan non-empty (unless sub-planner)
  │     - Items ≤ max_plan_size
  │     - ID uniqueness
  │     - Dep refs valid (within plan or known external IDs)
  │     - No cycles (iterative DFS)
  │
  ├── 2. Agent resolution
  │     - Exact name match → use it
  │     - Role hint → resolve via roster
  │     - Unknown → error
  │
  ├── 3. Kind inference
  │     - Agent role == "planner" → expandable
  │     - All others → atomic
  │
  ├── 4. Budget check
  │     - tasks_used + len(tasks) ≤ max_tasks
  │     - max_depth not exceeded
  │
  ├── 5. Note size check
  │     - Each task.task ≤ max_note_bytes
  │
  └── 6. Write to metadata
        context.metadata["submitted_output"] = Plan(tasks, rationale)
```

**Rationale:** One pass instead of two. Phase B was re-running Phase A checks with graph context — but `known_external_dep_ids` is already available in metadata (the executor pre-populates it). No reason to split.

### 6.3 Executor Flow

```
Executor.run_forever()
  │
  WHILE not cancelled:
  │
  ├── task_id = dispatcher.pop_ready()
  │
  ├── _run_one(task_id):
  │   │
  │   ├── 1. (task already RUNNING — pop_ready set status atomically)
  │   │
  │   ├── 2. Build context
  │   │     query_ctx.tool_metadata["posthook_enabled"] = True
  │   │     query_ctx.user_message = task_center.context_for(task)
  │   │
  │   ├── 3. await self.runner(defn, query_ctx)
  │   │     # query.py loop runs. UNTOUCHED.
  │   │     # Agent works, posts notes, calls terminal tool.
  │   │
  │   ├── 4. _posthook(query_ctx, defn)         ← ALWAYS RUNS
  │   │     │
  │   │     ├── metadata["submitted_output"] exists?
  │   │     │     YES → return it
  │   │     │     NO  → role-aware fallback
  │   │     │           planner  → FAIL
  │   │     │           worker   → auto-extract summary
  │   │     │
  │   │     └── Returns: Plan | Summary | Retry | Replan | Failure
  │   │
  │   └── 5. _dispatch(task_id, result)
  │         │
  │         ├── Plan     → validate + insert TaskSpecs into DAG
  │         ├── Summary  → mark DONE, post summary note to Task Center
  │         ├── Retry    → reset PENDING, increment retry_count
  │         ├── Replan   → fail item, spawn replanner
  │         └── Failure  → mark FAILED, cascade-cancel dependents
  │
  └── Loop
```

### 6.4 Retry & Replan

**Retry** — agent decides, deterministic routing:

```
Developer encounters transient failure
  → calls request_retry(reason="sandbox timeout")
  → tool writes RetryRequest to metadata["submitted_output"]
  → _has_submission() → loop exits
  → _posthook reads RetryRequest
  → dispatcher: if retry_count < max_retries → reset PENDING
                 else → FAIL + cascade
  → reason posted to Task Center (available on retry)
```

**Replan** — agent decides, replanner decomposes:

```
Developer realizes task is mis-scoped
  → calls request_replan(reason="auth is 3 services, need separate tasks")
  → tool writes ReplanRequest to metadata["submitted_output"]
  → _posthook reads ReplanRequest
  → dispatcher: fail task, spawn replanner at same depth/parent
  → Replanner reads Task Center:
      - failed agent's reason + suggestion
      - other siblings' results (if any)
  → Replanner calls submit_replan(add_tasks=[...], cancel_ids=[...])
  → dispatcher: atomically cancel old + insert new + promote ready
```

**Rationale:** The work agent has the most context about what went wrong. A separate decision posthook LLM (current `decision_submit_retry`) re-processes the same information with less context. Giving the decision to the work agent is both simpler and more accurate.

### 6.5 Cascade Policy

When a task fails, its dependents' behavior is controlled by `cascade_policy` on the **dependent** TaskSpec:

| Policy | Behavior | Use case |
|--------|----------|----------|
| `cancel` (default) | Cancel all dependents immediately | Strict dependency chains where downstream is meaningless without upstream |
| `retry_first` | Retry the failed task up to `max_retries` before cascading | Transient failures (network, sandbox timeout) where retry is cheap |
| `continue` | Mark dep as failed but let dependent start anyway, with failure context injected | Best-effort tasks where partial results are useful (e.g., tests can run even if one module failed to build) |

When a task fails, the dispatcher checks each dependent's `cascade_policy`: `cancel` cancels the dependent immediately; `retry_first` retries the failed task up to `max_retries` before cancelling dependents; `continue` posts a warning note to the Task Center and lets the dependent proceed with the failure context available.

### 6.6 External-Change-Triggered Replanning

Agent-initiated `request_replan()` handles cases where the agent discovers its task is mis-scoped. But in a fast-changing codebase, external changes (another team's commit, CI pipeline update) can invalidate the planner's decomposition before tasks execute.

**Detection:** Before starting each task, the executor queries the `FileChangeStore` for external changes in the task's scope since plan creation. "External" means edits not made by any agent run belonging to this team run (identified by `agent_run_id`).

**Response:** If scope is invalidated, the executor injects a warning note into the task's context rather than auto-replanning (which could cause cascading replans). The agent sees the warning and can call `request_replan()` if the changes are incompatible:

```
## Warning: scope changes detected since plan creation
The following files in your scope were modified externally:
- src/auth/session.py (by external commit, 45s ago)
Review these changes before proceeding. Call request_replan()
if your task is no longer valid.
```

This keeps the agent in control of the replan decision while ensuring it has the information to make it.

---

## 7. Submission & PostAgentHook

### 7.1 Submission Tools (Terminal)

Each role gets exactly the terminal tools it needs:

| Tool | Who calls it | What it writes to metadata |
|------|-------------|--------------------------|
| `submit_plan(tasks, rationale)` | planner, replanner | `Plan` |
| `submit_summary(summary)` | developer, reviewer | `SubmittedSummary` |
| `request_retry(reason)` | developer, reviewer | `RetryRequest` |
| `request_replan(reason, suggestion?)` | developer, reviewer | `ReplanRequest` |
| `submit_replan(add_tasks, cancel_ids)` | replanner | `ReplanPlan` |

All submission tools:
1. Validate arguments (structural, not LLM)
2. Write to `context.metadata["submitted_output"]`
3. Post summary note to Task Center (for siblings to read)
4. Return `ToolResult` (the loop continues to the `_has_submission()` check, which exits)

### 7.2 `_has_submission()` Gate

The only change to `query.py` is a two-line gate in `_has_submission()`: if `posthook_enabled` is not set in the metadata extras, the function returns `False` immediately (preserving current behavior for standalone agents). When it is set, the function checks for any `submitted_*` key with a non-null value — the signal that a terminal tool was called.

**Behavior by agent type:**

| Agent type | `posthook_enabled` | `_has_submission()` behavior |
|------------|-------------------|------------------------------|
| Standalone (no team) | Not set | Always returns `False`. Loop never exits for this reason. |
| Team agent (work) | `True` | Checks for `submitted_*` keys. Exits early when agent submits. |
| Subagent (via run_subagent) | `True` | Same as team agent. |

### 7.3 PostAgentHook — Deterministic Guarantee

```
                  await self.runner(defn, ctx)
                             │
                     runner returns (any reason)
                             │
                             ▼
                  ┌─────────────────────┐
                  │  _posthook(ctx, defn) │
                  │  ALWAYS RUNS          │
                  │  DETERMINISTIC         │
                  │  NO LLM               │
                  │                       │
                  │  metadata[            │
                  │   "submitted_output"] │
                  │       │               │
                  │    ┌──▼──┐            │
                  │    │found│            │
                  │    └┬───┬┘            │
                  │    YES  NO            │
                  │     │    │            │
                  │     ▼    ▼            │
                  │  return ┌──────────┐  │
                  │    it   │ defn.role │  │
                  │         └──┬────┬──┘  │
                  │        planner worker │
                  │            │     │    │
                  │            ▼     ▼    │
                  │         FAIL  extract │
                  │               last    │
                  │               message │
                  │               as      │
                  │               Summary │
                  └─────────────────────┘
                             │
                    Submission (guaranteed)
                             │
                             ▼
                    _dispatch(task_id, result)
```

**Why runner exit reason doesn't matter:**

| Loop ended because | `submitted_output` in metadata? | `_posthook` result |
|----|----|----|
| Agent called `submit_summary()` → `_has_submission()` exit | YES | Returns the submission |
| Agent called `submit_plan()` → same | YES | Returns the plan |
| Agent called `request_retry()` → same | YES | Returns retry request |
| Model returned stop (no tool calls) | NO | Role-aware fallback |
| `tool_call_limit` exhausted | NO | Role-aware fallback |
| Runner exception | N/A | Executor catches, calls `dispatcher.fail()` |

**Rationale:** The posthook is a function call in the executor, not a conditional event handler. It is structurally guaranteed to run. It produces a result in every case. Current system has TWO points of LLM failure (work agent + posthook agent). This design has ONE (work agent) with a deterministic backstop.

### 7.4 What Gets Deleted

The posthook agent infrastructure is deleted entirely: the `agent_posthook.py` module with its `execute_with_posthook` entry point, all five posthook agent `.md` definitions (submit plan, submit summary, submit replan, decision retry, decision replan), the legacy posthook toolkit classes, and the `PosthookConfig` field on `AgentDefinition`. The executor's `_run_one()` is rewritten to call the runner directly and then invoke a small deterministic `_posthook()` function. All five submission tools are consolidated into a single toolkit file, keeping the same file path but deleting the per-tool source files and the old toolkit class file.

---

## 8. Context Sharing & Inheritance

### 8.1 Design Principle: Two Filters + Ledger

Agents need three kinds of context. Each comes from the right source:

| Need | Source | Mechanism |
|------|--------|-----------|
| What upstream produced | Task Center | Dep filter: notes from dependency tasks |
| What changed in my files | Arbiter | `arbiter.changes_since()`: actual file edits in scope (Arbiter owns the edit ring buffer) |
| Why this task exists | Task Center | Parent chain: walk parent_id up to root |

No sibling tag filtering. No dedup machinery. No canonical scopes.

**Rationale:** Sibling awareness via notes is advisory and degrades under parallelism (siblings post after context is built). File-level awareness via Ledger is ground truth and always current. Agents need to know what **changed in the codebase**, not what siblings **wrote about** the codebase.

### 8.2 Context Rendering

`context_for()` builds the agent's context string using a fixed priority order within a byte budget. Priority 1 (never trimmed) is the task instruction and scope paths. Priority 2 is dep notes from the Task Center, deduplicated to the latest note per dependency (direct deps only — not transitive, to avoid context explosion). Priority 3 is recent file changes in scope from the `FileChangeStore` (durable, cross-process visible). Priority 4 is the parent chain (walked via the `task_lookup` callable), trimmed if budget is exhausted. All sections are joined and returned as a single string.

**Implementation note — FileChangeStore replaces Arbiter parameter:** The original design specified an `Arbiter` object for file change awareness. The implementation passes `file_change_store` instead — the durable `FileChangeStore` (backed by the `file_changes` table) provides cross-process visibility and crash recovery. The `task_lookup` callable replaces a raw pool parameter for parent chain walks.

#### Design note: dep note dedup (latest-per-dep)

A progressive disclosure layer (`_dep_note_index` returning lightweight `(id, agent, summary, timestamp)` tuples, then selectively loading full content) was considered but rejected as marginal. The current design already handles the expensive case:

- **Direct deps only** — `_dep_notes` fetches immediate dependencies, not transitive. The note set is bounded by plan fan-out, which is typically small.
- **Byte-budget truncation** — `_render_notes_truncated` degrades gracefully when dep notes exceed the budget.
- **In-memory reads** — no I/O cost to filter; a two-phase fetch saves nothing in the single-process case.

The one scenario that *can* bloat is a single dependency posting many incremental notes (e.g., a long-running explorer logging progress). The fix is to dedup to the **latest note per dep task** (last entry per `task_id` wins). This targets the actual problem without adding an abstraction layer. A two-phase index would only become worthwhile if `TaskCenter` moves to a persistent store where fetching full content has real I/O cost.

### 8.2.1 Automatic Context Freshness Warning

`context_for()` builds a snapshot at task start that can go stale during long-running tasks. LISTEN/NOTIFY (Section 14.7) handles file-level changes, but dep notes or new sibling completions are invisible after context is built. Two mechanisms close this gap:

1. **`context_changed_since` tool** — registered in the `context` toolkit (available to all roles). Agents call this before committing large changes. Implemented in `tools/context/toolkit.py` as `ContextChangedSinceTool`, which delegates to `tools/context/freshness.py:check_freshness()`.

2. **Automatic freshness gate on submission** — the submission tools (`submit_summary`, `submit_plan`, `submit_replan` in `tools/posthook/toolkit.py`) call `_check_context_freshness()` before accepting a submission. If context is stale and the agent hasn't called `context_changed_since()` first, the submission is rejected with an error asking the agent to refresh.

This dual approach ensures agents are both informed (pull via tool) and gated (push via submission rejection).

### 8.3 Context Priority & Overflow

Fixed priority order (hardcoded, not configurable — add configurability only if evidence shows different orderings improve outcomes).

| Priority | Section | Source | Trim policy |
|----------|---------|--------|-------------|
| 1 (never trimmed) | Your task | `task.task` + `task.scope_paths` | Never |
| 2 | Dep notes | Task Center (dep filter) | Keep most recent, trim oldest |
| 3 | File changes | Ledger (scope filter) | Keep most recent N changes |
| 4 | Parent chain | Task Center (parent walk) | Keep root rationale, trim middle |

**Rationale for priority order:**
- Agent must know WHAT to do (task) -- always
- Agent must know WHAT upstream produced (deps) -- structural dependency
- Agent must know WHAT changed in its files (ledger) -- collision avoidance
- Agent should know WHY it exists (parent) -- strategic, nice-to-have

### 8.4 Detailed Flow: Dependency Context (Parent to Child)

```
Root planner submits:
  Task P: "Implement user API"  (planner, expandable)
  P posts rationale note: "Decomposing into schema + endpoints + tests"

Sub-planner P runs, submits:
  Task A: "Create user schema"    parent=P
  Task B: "Implement endpoints"   parent=P, deps=[A]

When A starts, context_for(A) builds:

  ## Your task
  Create user schema
  Scope: src/db/

  ## Parent context
  ### planner (P)
  Decomposing into schema + endpoints + tests

When A finishes and B starts, context_for(B) builds:

  ## Your task
  Implement endpoints
  Scope: src/api/

  ## Context from dependencies
  ### developer (A)
  Created migration: users table with id, email, name, created_at.
  File: src/db/migrations/001_users.py

  ## Recent changes in your scope
  (none -- A edited src/db/, B's scope is src/api/)

  ## Parent context
  ### planner (P)
  Decomposing into schema + endpoints + tests
```

### 8.5 Detailed Flow: Parallel Agents with Ledger Awareness

```
Planner submits:
  Task A: "Fix auth timeout"      scope_paths=["src/auth"]  deps=[]
  Task B: "Fix auth retry logic"  scope_paths=["src/auth"]  deps=[]
  Task C: "Verify auth module"    scope_paths=["src/auth"]  deps=[A, B]

Timeline:
  t=1  A starts, B starts (parallel, no deps)
       Both see empty Ledger (no prior changes)
  t=2  A edits session.py -> Ledger.record("src/auth/session.py", agent_A)
  t=3  B calls scope_changed_since(["src/auth"], since=t1)
       -> Returns: session.py edited by agent_A 1s ago
       B now knows A touched session.py, avoids conflicting edit
  t=4  A finishes (done), B finishes (done)
  t=5  C starts (both deps satisfied):
       context_for(C) includes:
         Dep notes: A's summary + B's summary (from Task Center)
         Ledger: session.py edited by A, middleware.py edited by B
         C has full picture to verify both changes
```

**Key difference from old design:** B discovers A's work through the **Ledger** (actual file changes), not through sibling notes (agent prose). The Ledger is ground truth -- it records what actually happened to files, not what an agent chose to write about.

### 8.6 Detailed Flow: Subagent Swarm Output

```
Planner spawns 3 explorer subagents via run_subagent():

  Explorer-1: "Read src/auth/"  -> posts Note(scope_paths=["src/auth"], content="...")
  Explorer-2: "Read src/api/"   -> posts Note(scope_paths=["src/api"], content="...")
  Explorer-3: "Read src/db/"    -> posts Note(scope_paths=["src/db"], content="...")

All three run concurrently. All post to Task Center.

Planner reads Task Center after explorers complete:
  task_center.read(scope_paths=["src/auth", "src/api", "src/db"])
  -> gets all three explorers' findings
  -> decomposes work based on full picture

Later, Developer assigned to "Fix auth timeout" (scope_paths=["src/auth"]):
  context_for(developer_task) includes:
    Dep notes: explorer's note about src/auth/ (if explorer is a dep)
    Or: planner included explorer findings in the task description
```

**Rationale:** Subagent output flows through the same Task Center as everything else. No special artifact store, no structured contracts. Explorers post prose. The planner reads and synthesizes. Consumers get context through deps, not broadcast.

### 8.7 Comparison with Current 3-Tier System

| Aspect | Current (3-tier briefings) | Plan A (Task Center + Ledger) |
|--------|--------------------------|-------------------------------|
| Parent to child | `task.briefings` (Tier 3, explicit) | Parent chain filter on Task Center |
| Dep to consumer | `task.dep_artifacts` (Tier 2, snapshot at PENDING->READY) | Dep filter on Task Center (read at context-build time) |
| Sibling awareness | `project_context.shared_briefings` (Tier 1, canonical_scope) | Ledger: actual file changes in scope |
| Dedup mechanism | 3-tier priority + `seen_scopes` + `seen_refs` + `_claim()` | Dedupe by `note.id` (trivial) |
| Freshness | `scout_artifact_invalidated()`, coherence tokens, pressure scoring | Notes are immutable. Ledger is ground truth. |
| Overflow | `max_briefing_bytes` per-item truncation | Priority-based budget with per-section trim |
| Code size | ~1,420 lines across 9 files | ~250 lines in 1 file |

### 8.8 Dynamic Environment Awareness (Consolidated View)

A fast-changing codebase means the world can change while an agent is working. Four mechanisms handle this at four timescales:

| Timescale | Mechanism | How it works |
|-----------|-----------|-------------|
| **Pre-start** | `_check_scope_validity()` (Section 6.6) | Executor checks if files in the task's scope changed externally since the plan was created. If so, injects a warning note — agent decides whether to `request_replan()`. |
| **At start** | `context_for()` (Section 8.2) | Builds a snapshot including recent file changes from `arbiter.changes_since(task.created_at)`. Agent starts with full picture of what changed since it was planned. |
| **Mid-task (pull)** | `context_changed_since()` tool (Section 8.2.1) | Agent calls this before committing large changes. Returns new dep notes, sibling completions, and scope file changes since task started. |
| **Mid-task (push)** | `LISTEN/NOTIFY` (Section 14.7) | Real-time `SystemReminderBlock` injected into agent conversation when another agent edits files in its scope. Buffered per-executor and flushed at the top of each query loop turn with replacement semantics (no accumulation). |
| **At edit time** | Arbiter OCC (Section 9.3) | Hard backstop. Content-hash token validation catches stale edits with zero false negatives. Agent gets error, re-reads file, retries. |

**Concrete example — file edited out from under an agent:**

```
t=0  Agent A reads src/auth/session.py (hash=abc)
     Arbiter issues token(session.py, hash=abc, agent_A)
t=1  Agent B edits session.py → hash changes to def
     Ledger records the edit
t=2  Agent A tries to edit session.py
     Arbiter.validate_token(token, session.py, hash=def) → FAIL (abc ≠ def)
     Agent A gets error: "File changed since you read it"
     Agent A re-reads session.py (sees B's changes)
     Agent A re-issues token with new hash, edits successfully
```

No agent coordination needed. The Arbiter serializes at the file level, and token validation ensures no edit is ever applied against stale content.

---

## 9. OCC, Code Intelligence & Exploration Cache

### 9.1 Design Principle: Two Layers, Not Three

The current system has three coordination layers: briefings (knowledge), scope packets (contention awareness), and Arbiter (file locks). But agents are ephemeral workers — they cannot wait, queue, or reschedule based on a contention report. The dispatcher handles sequencing via deps. The Arbiter catches actual conflicts at edit time. Scope packets are a pre-flight contention report for an agent that cannot act on it. **Delete them.**

```
              +-------------------------+
              | KNOWLEDGE LAYER          |
              | (Task Center)            |
              |                          |
              | "What do agents know?"   |
              |                          |
              | Explorer found X.        |
              | Developer fixed Y.       |
              | Validator failed Z.      |
              +------------+-------------+
                           |
                answers "what to do"
                           |
              +------------v-------------+
              | EXECUTION LAYER          |
              | (Arbiter + Ledger)       |
              |                          |
              | "Serialize file edits"   |
              |                          |
              | Token -> Lock -> Edit    |
              | -> Validate -> Record    |
              +--------------------------+
```

**Rationale:** Two layers, two concerns, zero coupling:
- Task Center handles knowledge divergence (agents learn at different rates)
- Arbiter handles conflict prevention (serializes overlapping file writes)
- No middle layer needed. Deps handle sequencing. Arbiter handles collisions.

### 9.2 What Stays, What Goes

Within `code_intelligence`, the entire `editing/` layer (Arbiter, Ledger, patcher, merge, time_machine) is kept untouched. The `routing/` layer keeps the CI service, query router, and backend protocol, but deletes `scope_packets.py` (agents cannot act on contention reports). The `analysis/` and `lsp/` layers are kept untouched. The entire `atlas/` directory is deleted and replaced by `ExplorationMemory`. In `tools/daytona_toolkit/`, the `coordination.py` module is deleted — `task.scope_paths` is read directly wherever scope information is needed.

### 9.3 OCC During Agent Execution

The Arbiter and Ledger operate inside tool execution, not at the context level:

```
Agent calls edit_file("src/auth/session.py", ...)
  |
  +-- DaytonaToolkit.edit_tool:
  |     1. Arbiter.issue_token(session.py, hash, agent_id)
  |     2. Arbiter.acquire_file_lock(session.py)
  |     3. Apply edit
  |     4. Arbiter.validate_token(token_id, session.py, new_hash)
  |     5. Arbiter.release_file_lock(session.py)
  |     6. Ledger.record(session.py, agent_id, "edit")
  |     7. Arbiter.record_edit(session.py, agent_id)
  |
  +-- If another agent edited session.py since token was issued:
        -> validate_token fails -> content hash mismatch
        -> Agent gets error -> can read fresh content and retry
```

This is completely independent of the Task Center. The Arbiter is per-file, real-time, and operates inside the tool call. No change needed.

**File-level write serialization with region-level OCC:** The file-level `threading.Lock` (`acquire_file_lock`) serializes the physical write I/O — only one agent writes at a time. However, the OCC layer is smarter than pure file-level: when a token's content hash mismatches (another agent edited the file since the token was issued), `_resolve_pending_write()` in `service.py` performs **line-range-based merge** via `detect_edit_window()` + `merge_non_overlapping_edit()`. If the specific target lines are unchanged in the current file, the edit succeeds despite the hash mismatch. Only overlapping-range edits are rejected.

| Scenario | Result |
|----------|--------|
| Agent A edits lines 1-10, Agent B edits lines 50-60 | **Both succeed** — non-overlapping merge |
| Agent A edits lines 1-10, Agent B edits lines 5-15 | B rejected — overlapping range, must re-read and retry |
| No other agent touched the file | Token hash matches, edit succeeds immediately |

This gives effectively region-level OCC with file-level write serialization — maximizing parallel throughput while keeping the lock implementation simple. The planner further mitigates contention by assigning disjoint `scope_paths` (Section 9.7's `QueryEditHistoryTool` predicts hotspots at decomposition time).

**Rationale for no scope packets in agent prompts:** The Arbiter catches conflicts at edit time with zero false negatives. Deps already prevent agents from starting before their predecessors finish. Showing a contention report to an ephemeral agent that cannot reschedule itself adds prompt noise without actionable benefit. If we later find agents need contention awareness, it can be added as a lazy tool (`check_contention(paths)`) rather than eager prompt injection.

### 9.4 Explorer: Prose-Based Code Understanding

**What explorer solves:** N developers need to understand the same code area. Without explorer, each reads the same files independently — Nx the tool calls, Nx the LLM input tokens. One explorer serves all N consumers via Task Center.

```
WITHOUT explorer:                     WITH explorer:

Dev A: read_file x 15 --+            Explorer: read_file x 15 -> Note
Dev B: read_file x 15   +-- 45       Dev A: read note    --+
Dev C: read_file x 15 --+            Dev B: read note      +-- 15 + 3 reads
                                      Dev C: read note    --+
```

**Plan A makes explorer MORE useful than current.** Current explorer must produce `{target_paths, files, entry_points, scope_coverage, gaps}`. In Plan A, explorer writes prose — it can note things that don't fit a JSON schema:

```
Current explorer output (constrained by contract):
  {"files": [...], "entry_points": [...], "scope_coverage": 0.8}

Plan A explorer output (free prose):
  "The auth module has 3 files. session.py has a complex state machine
   (lines 40-120) -- careful, there's a known race condition noted in a
   TODO at line 87. middleware.py is straightforward except the bare
   except at line 87 which swallows TimeoutError. tokens.py is clean."
```

The second is more useful to a developer. LLMs consume prose better than JSON schemas.

### 9.5 Exploration Cache (Replaces Atlas)

**What Atlas solves:** Don't re-explore unchanged code across runs. The current Atlas achieves this with ~400 lines across 6 files. The actual mechanism is a content-addressed cache. Everything else is overhead.

**Exploration Cache** — `ExplorationMemory` exposes two operations: `check(scope_paths, sandbox)` hashes the files in the given paths and returns cached notes if the hash matches a prior run, or `None` if re-exploration is needed; and `save(scope_paths, notes, sandbox)` stores notes keyed by a hash of the scope paths plus the current file content hash. The cache key is a SHA-256 digest of sorted scope paths and content hash.

Content hash IS the freshness check. No subsystem model, no auto-promotion, no coherence tokens, no complex persistence model.

### 9.6 Planner's Exploration Flow

The planner gets one tool — `check_exploration_memory` — to check the cache before spawning explorers. On a cache hit, it loads the cached notes into the Task Center and returns `{"status": "cached"}`. On a miss, it returns `{"status": "needs_exploration"}` and the planner spawns an explorer subagent.

The flow:

```
Planner starts
  |
  +-- For each scope it wants to understand:
  |     |
  |     +-- check_exploration_memory(["src/auth/"])
  |     |     |
  |     |     +-- CACHED -> notes loaded into Task Center
  |     |     |              skip explorer (save ~15 tool calls)
  |     |     |
  |     |     +-- NEEDS_EXPLORATION -> spawn explorer subagent
  |     |           Explorer posts findings -> notes auto-saved to cache
  |     |
  |     +-- Read Task Center -> has findings either way
  |
  +-- submit_plan(tasks=[...])
```

**Cache behavior in a fast-changing codebase:** Content hash won't match if files changed since last exploration. Returns `None`. Explorer re-explores. The cache is self-invalidating with zero staleness tracking.

**Cache is optional and zero-cost when unused:** If `ExplorationCache` is not wired up, `check_exploration_cache` always returns `needs_exploration`. Explorer runs every time. No harm.

### 9.7 Planner Conflict Prediction

**What it solves:** Scope packets gave agents pre-flight contention reports they couldn't act on (agents can't reschedule). But the **planner** can act — it can restructure decomposition to avoid overlapping scopes. The Ledger's historical edit data, stored in PostgreSQL, gives the planner cross-run intelligence about which files are contentious.

The `query_edit_history` tool accepts a list of paths and queries `FileChangeStore.contention_hotspots()` to return the most-edited files with agent and edit counts across prior runs.

**Usage in planner prompt:**

```
Before decomposing, check edit history for contention hotspots:
  query_edit_history(paths=["src/payment/"])
  -> shared/utils.py: 3 agents, 7 edits (historical)

Planner action: assign shared/utils.py to a single task
or sequence it explicitly before parallel work.
```

**Why this is the right layer for contention awareness:**

| Layer | Can restructure work? | Has edit history? | Result |
|---|---|---|---|
| Agent (worker) | No — ephemeral, can't reschedule | No — sees only its task | Scope packets were here (wrong layer, deleted) |
| Planner | **Yes** — chooses decomposition | **Yes** — via PostgreSQL | Conflict prediction is here (right layer) |

### 9.8 Atlas to Exploration Cache Comparison

| Current Atlas (6 files, ~400 lines) | Exploration Cache (~60 lines) |
|------|------|
| `atlas/service.py` -- lookup_subsystems, persist_scout_brief | `ExplorationMemory.check/save` |
| `atlas/store.py` -- SQL persistence, chunk storage | Simple key-value store |
| `atlas/model.py` -- ORM model | Not needed (key-value) |
| `atlas/persistence.py` -- durable storage | Built into cache |
| `atlas/freshness.py` -- reuse status, staleness checks | Content hash comparison (3 lines) |
| `atlas/identity.py` -- project_key_for() | Scope paths are the identity |
| `tools/atlas/lookup.py` -- planner-facing tool | `CheckExplorationMemoryTool` (~20 lines) |

---

## 10. Toolkit Assignment

### 10.1 Agent Types

Plan A reduces `agent_type` from three values to two. Posthook agents are deleted entirely (Section 7.4):

| Type | Description | Dispatching | Can spawn subagents? |
|------|-------------|-------------|---------------------|
| `"agent"` | Regular team-mode agents (planner, developer, reviewer, replanner) | Dispatched as tasks through PGDispatcher → Executor | Planner/replanner: yes (explorer only). Others: no. |
| `"subagent"` | Focused worker subagents (explorer) | Spawned inline via `run_subagent()` tool | No |

**Deleted:** `"posthook"` — all 5 posthook agents are replaced by the deterministic `_posthook()` function (Section 7.3).

### 10.2 Per-Role Toolkit Matrix (Dispatched Roles)

Explorer is a subagent spawned via `run_subagent()`, not a dispatched task. Its toolkits come from its agent definition (Section 10.3), not from `toolkits_for_role()`. This matrix covers only dispatched roles:

| Toolkit | Planner | Developer | Reviewer | Replanner |
|---------|:-------:|:---------:|:--------:|:---------:|
| `code_intelligence` | read (blocked: ci_read_file) | full | full | read (blocked: ci_read_file) |
| `sandbox_operations` | -- | full | full | -- |
| `subagent` | spawn (explorer only) | -- | -- | -- |
| `context` | read-only (blocked: post_note) | full | full | read-only (blocked: post_note) |
| `submission` | submit_plan | done, retry, replan | done, retry, replan | submit_replan |

**Implementation note — toolkit consolidation:** The original design specified 5 new toolkits (`task_center_read`, `task_center_write`, `exploration_memory`, `edit_history`, `search`). The implementation consolidates to 2:
- `context` — single unified toolkit merging task_center + search + exploration_memory. Contains `PostNoteTool`, `ReadNotesTool` (with `keyword` param absorbing `search_context`), `ContextChangedSinceTool` (absorbing `scope_changed_since`), and `CheckExplorationMemoryTool`. Role-based read/write restrictions are enforced via `blocked_tools` in agent definitions (e.g., planners block `post_note`) rather than separate toolkit classes.
- `submission` — unchanged from design.

The `memory` toolkit was absorbed into `context`. `query_edit_history` is backed by `FileChangeStore.contention_hotspots()` and is wrapped as a tool in `tools/ci_toolkit/query_tools.py`. No other capability was removed — tools are bundled into fewer registration names for simplicity.

### 10.3 Explorer (Subagent)

Explorer is the only subagent type. It is NOT dispatched through the executor — it runs inline within the planner's turn via `run_subagent()`.

**Flow:**
1. Planner/replanner calls `run_subagent(agent_name="explorer", prompt="...")`
2. Explorer spawns as a background task within the caller's turn
3. Explorer reads code via `code_intelligence` (read-only) and posts notes to Task Center
4. Explorer returns its result via the `run_subagent` return envelope (not via `submit_summary()`)
5. Planner reads explorer's findings from Task Center

**Caller restriction (preserved from current system):** Only `planner` and `replanner` can call `run_subagent()`, and they can ONLY spawn `explorer`. This is enforced by `SCOUT_ONLY_CALLERS` policy, not by toolkit assignment.

**Explorer's toolkits** (defined in agent definition, not in `toolkits_for_role()`):

| Toolkit | Access |
|---------|--------|
| `code_intelligence` | read-only |
| `context_read` | yes |
| `context_write` | yes (posts exploration findings) |

Explorer has NO `submission` toolkit — it does not call `submit_summary()`, `request_retry()`, or any terminal tool. Its result is captured by the `run_subagent` infrastructure and returned to the caller.

### 10.4 New Toolkits (2)

**`context` toolkit** (replaces `context_inheritance`, `context_sharing`, `team_context`, `search`, `atlas`):

Single unified toolkit. Role-based read/write restrictions are enforced via `blocked_tools` in agent definitions (e.g., planners and replanners block `post_note`) rather than separate read/write classes.

| Tool | Blocked for | Description |
|------|:----------:|------------|
| `read_notes(authors?, scope_paths?, keyword?, limit?)` | — | Read/search notes with optional keyword filter (absorbs former `search_context`). |
| `context_changed_since()` | — | Check if context is stale: scope changes, dep notes, sibling completions since task started. Absorbs former `scope_changed_since`. |
| `post_note(content, scope_paths?)` | planner, replanner | Post a note to Task Center. Inherits task scope_paths by default. |
| `check_exploration_memory(paths)` | — | Check if scope was recently explored. Returns `cached` or `needs_exploration`. |

**Note:** `query_edit_history` is backed by `FileChangeStore.contention_hotspots()` and is implemented in `tools/ci_toolkit/query_tools.py`.

**`submission` toolkit** (replaces 5 posthook toolkit classes):

| Tool | Available to | Description |
|------|-------------|-------------|
| `submit_plan(tasks, rationale)` | planner | Submit plan. Terminal. |
| `submit_summary(summary)` | developer, reviewer | Signal completion. Terminal. |
| `request_retry(reason)` | developer, reviewer | Request retry. Terminal. |
| `request_replan(reason, suggestion?)` | developer, reviewer | Request replan. Terminal. |
| `submit_replan(add_tasks, cancel_ids)` | replanner | Submit replan. Terminal. |

### 10.5 Toolkit Factory Changes

The toolkit factory removes the old registrations for `context_inheritance`, `context_sharing`, `team_context`, `atlas`, and the five posthook toolkit classes. Two new registrations replace them: `submission` (the unified submission toolkit) and `context` (the unified context toolkit). The `sandbox_operations`, `code_intelligence`, and `subagent` registrations are unchanged.

### 10.6 Role Resolution

Toolkit assignment is handled in agent definitions and the context builder rather than a standalone function. The effective mapping: planner gets `code_intelligence` (read-only), `subagent`, `context` (read-only, `post_note` blocked), and `submission`; developer and reviewer get `sandbox_operations`, `code_intelligence` (full), `context` (full), and `submission`; replanner gets `code_intelligence` (read-only), `context` (read-only, `post_note` blocked), and `submission`.

### 10.7 Complete Tool Inventory

**10 new tools in 3 toolkits**, replacing ~15+ tools/toolkits from the old system:

| # | Tool | Toolkit | Available to | Terminal? | Description |
|---|------|---------|-------------|:---------:|-------------|
| 1 | `read_notes` | `context_read` | all roles | no | Read/search Task Center notes by author, scope, keyword |
| 2 | `context_changed_since` | `context_read` | all roles | no | Check staleness: scope changes + dep notes + sibling completions |
| 3 | `post_note` | `context_write` | developer, reviewer, explorer | no | Post note to Task Center with optional scope |
| 4 | `check_exploration_memory` | `memory` | planner | no | Check cross-run exploration cache; returns `cached` or `needs_exploration` |
| 5 | `query_edit_history` | `memory` | planner | no | Query cross-run edit patterns to predict scope conflicts |
| 6 | `submit_plan` | `submission` | planner | **yes** | Submit plan of TaskSpecs |
| 7 | `submit_summary` | `submission` | developer, reviewer | **yes** | Signal task completion with summary |
| 8 | `request_retry` | `submission` | developer, reviewer | **yes** | Request task retry with reason |
| 9 | `request_replan` | `submission` | developer, reviewer | **yes** | Request replan with reason and optional suggestion |
| 10 | `submit_replan` | `submission` | replanner | **yes** | Submit replan (add/cancel tasks) |

**What these replace:**

| Old tool/toolkit | New equivalent |
|-----------------|----------------|
| `share_briefing` | `post_note` |
| `inspect_inherited_context` | `read_notes` |
| `atlas/lookup` | `check_exploration_memory` |
| `scope_packets` tools | Deleted (Arbiter handles at edit time) |
| `coordination.py` helpers | Deleted (`task.scope_paths` read directly) |
| 5 posthook toolkit classes | `submission` toolkit (5 tools, no LLM) |
| `search_context` (standalone) | `read_notes` with `keyword` parameter |
| `scope_changed_since` (standalone) | `context_changed_since` (unified check) |

---

## 11. Task-Agnostic Flows

### 11.1 Greenfield: Empty Project, Build From Scratch

```
User: "Build a REST API for user management"

Planner reads Task Center: empty (no prior context)
Planner does NOT spawn explorers (nothing to explore)
Planner calls submit_plan:

  TaskSpec(id="schema",  task="Create user table migration with id, email, name, created_at",
           agent="developer", scope_paths=["src/db/"])
  TaskSpec(id="api",     task="Implement CRUD endpoints: GET/POST/PUT/DELETE /users",
           agent="developer", deps=["schema"], scope_paths=["src/api/"])
  TaskSpec(id="test",    task="Write integration tests for user API",
           agent="reviewer", deps=["api"], scope_paths=["tests/"])
```

**Why it works:** No explorer, no atlas lookup, no "subsystem discovery". Planner goes straight to work decomposition. Same dispatcher, same executor, same Task Center.

### 11.2 Existing Project: Bug Fix

```
User: "Fix the login timeout bug"

Planner checks exploration cache: miss (first time seeing src/auth/)
Planner spawns explorer subagent:
  run_subagent(agent_name="explorer", prompt="Read src/auth/. Find timeout handling.")

Explorer posts to Task Center:
  Note(content="auth/session.py:42 has 30s timeout. middleware.py:87 has bare
   except that swallows TimeoutError.", scope_paths=["src/auth"])

Planner reads Task Center: sees explorer's finding
Planner calls submit_plan:

  TaskSpec(id="fix", task="Fix: middleware.py:87 bare except should re-raise
   TimeoutError after logging. session.py timeout is correct at 30s.",
           agent="developer", scope_paths=["src/auth/middleware.py"])
  TaskSpec(id="verify", task="Run auth test suite. Verify timeout propagates.",
           agent="reviewer", deps=["fix"], scope_paths=["tests/test_auth.py"])
```

**Why it works:** Planner optionally spawns explorer. Explorer posts prose (no JSON contract). Planner reads prose and plans accordingly. Explorer findings cached for next run.

### 11.3 Existing Project: Feature Implementation

```
User: "Add OAuth2 support to the auth module"

Planner checks exploration cache:
  src/auth/  -> CACHED (explored 3 min ago, files unchanged)
  src/api/routes/ -> NEEDS_EXPLORATION

Planner spawns 1 explorer (not 2 -- cache saved one):
  Explorer: "Read src/api/routes/ for existing endpoint patterns"

Planner reads Task Center (has both cached + fresh findings), decomposes:

  TaskSpec(id="model",    task="Add OAuth2Provider model and token storage",
           agent="developer", scope_paths=["src/auth/models.py"])
  TaskSpec(id="flow",     task="Implement OAuth2 authorization code flow",
           agent="developer", deps=["model"],
           scope_paths=["src/auth/oauth2.py"])
  TaskSpec(id="endpoint", task="Add /auth/oauth2/callback and /auth/oauth2/authorize",
           agent="developer", deps=["flow"],
           scope_paths=["src/api/routes/auth.py"])
  TaskSpec(id="verify",   task="Test OAuth2 flow end-to-end",
           agent="reviewer", deps=["endpoint"],
           scope_paths=["tests/test_oauth2.py"])
```

### 11.4 High-Parallelism Decomposition

```
User: "Refactor the payment module into microservice-ready components"

Planner spawns explorers for all payment submodules:
  Explorer-1: "Read src/payment/billing/"
  Explorer-2: "Read src/payment/invoicing/"
  Explorer-3: "Read src/payment/gateway/"

After explorers report, planner decomposes with maximum parallelism:

  TaskSpec(id="billing",   task="Extract billing into standalone module...",
           agent="developer",
           scope_paths=["src/payment/billing/"])
  TaskSpec(id="invoicing", task="Extract invoicing into standalone module...",
           agent="developer",
           scope_paths=["src/payment/invoicing/"])
  TaskSpec(id="gateway",   task="Extract gateway into standalone module...",
           agent="developer",
           scope_paths=["src/payment/gateway/"])
  TaskSpec(id="verify",    task="Run full payment test suite...",
           agent="reviewer", deps=["billing", "invoicing", "gateway"],
           scope_paths=["tests/test_payment/"])
```

**Why parallelism is safe:**
1. **Non-overlapping scope_paths** -- Planner assigned disjoint scopes. No file-level contention.
2. **Ledger-based awareness** -- if billing developer edits a shared file, the Ledger records it. Other developers see it via `scope_changed_since()` or in their next `context_for()` call.
3. **Arbiter as backstop** -- if scopes unexpectedly collide, the Arbiter catches it at edit time with token validation. Agent re-reads and retries. Rare but recoverable.

**What happens if scopes collide:**

```
billing developer discovers it needs to edit src/payment/shared/utils.py
  -> Arbiter.issue_token("shared/utils.py", hash, billing_agent)
  -> billing developer edits shared/utils.py
  -> Ledger.record("shared/utils.py", billing_agent)

invoicing developer also needs to edit src/payment/shared/utils.py
  -> Arbiter.issue_token("shared/utils.py", hash, invoicing_agent)
  -> hash MATCHES current content (billing edit committed)
      OR hash MISMATCHES (billing edit in progress)
  -> If mismatch: edit tool returns error, agent re-reads file
  -> Arbiter.acquire_file_lock() serializes the write
```

OCC handles the collision at file level. No coordination needed in the Task Center or plan.

---

## 12. Migration Phases

All six phases are complete.

**Phase 1 — Task Center + Submission Tools + Exploration Cache:** New code only, no deletions. The in-memory `TaskCenter` was introduced, all five submission tools were consolidated into a single toolkit file, the unified `context` toolkit was created (including `PostNoteTool`, `ReadNotesTool`, `ContextChangedSinceTool`, `CheckExplorationMemoryTool`, and `ExplorationMemory`), the freshness detection helper was added, and the `query_edit_history` tool was wired to `FileChangeStore`. Prerequisite: PostgreSQL schema migration to create the `file_changes` and `tasks` tables with ltree extension.

**Phase 2 — Query Engine Gate:** A two-line gate was added to `_has_submission()` to check `posthook_enabled` before inspecting submitted output keys. No other changes to the query engine.

**Phase 3 — Executor Rewrite:** The executor's `_run_one()` was rewritten to call the runner directly and then invoke a deterministic `_posthook()` function. The context builder was updated to use `task_center.context_for()`.

**Phase 4 — Data Model Migration:** The data model was updated with the new `TaskSpec`, simplified `Task` and `Plan`, and `ReplanPlan`. Single-pass plan validation replaced the old Phase A + B split. The dispatcher and its store were rewritten to use PostgreSQL `SKIP LOCKED` (Section 14.6).

**Phase 5 — Deletion:** All replaced components were removed — the posthook agent module, the briefings and scout-briefings layers, the artifact store, the Atlas directory, scope packets, the coordination helper, the per-tool posthook source files, the old team-context tools, and the five posthook agent definitions.

**Phase 6 — Agent Definition Cleanup:** The `PosthookConfig` field was removed from `AgentDefinition`. All built-in agent definitions were updated to use the new toolkit names. Playbooks were simplified to remove contract references. All affected tests were updated.

---

## 13. Deletion Inventory

The following components were deleted entirely as part of this redesign:

The posthook agent infrastructure (`agent_posthook.py` and five posthook agent definition files) was replaced by the deterministic `_posthook()` function in the executor. The briefing layer (`briefings.py`, `scout_briefings.py`, `canonicalize.py`) and the artifact store were replaced by the append-only Task Center. The entire Atlas directory (service, store, model, persistence, freshness, identity) was replaced by `ExplorationMemory`. The scope packets module and the coordination helper were deleted outright — the Arbiter handles file conflicts at edit time, and `task.scope_paths` is read directly. The per-tool posthook source files and the legacy toolkit class file were consolidated into a single submission toolkit file. The old team-context tools for sharing and inspecting briefings were replaced by `PostNoteTool` and `ReadNotesTool` in the context toolkit. In aggregate, the deletions far outweigh the new additions, resulting in a substantially smaller and simpler coordination layer.

---

## 14. PostgreSQL Infrastructure

### 14.1 Design Principle: PostgreSQL as Coordination Kernel

Most multi-agent frameworks build custom coordination infrastructure — in-memory message queues, shared state objects, custom lock managers. This design uses PostgreSQL as the universal coordination substrate:

| Coordination need | Custom approach (typical) | PostgreSQL primitive |
|---|---|---|
| Work queue | In-memory priority queue + lock | `FOR UPDATE SKIP LOCKED` |
| Event bus | Redis pub/sub, custom callbacks | `LISTEN / NOTIFY` |
| Distributed file locking | In-memory lock manager | Advisory locks |
| Knowledge search | Vector DB, custom index | GIN + full-text search |
| Time-range queries | Custom ring buffer | BRIN index |
| Set/hierarchy membership | Custom tag matching | `ltree` + GiST |
| Crash recovery | Checkpoint + replay | WAL (built-in) |

One database replaces an entire microservice-style coordination stack. Every component in this plan — dispatcher, Ledger, Task Center, Arbiter, ExplorationMemory — reads and writes PostgreSQL. The in-memory layer is a performance cache, not a source of truth.

**Trade-off acknowledged:** PostgreSQL becomes a single point of failure — if it's down, all coordination primitives fail simultaneously. This is accepted because: (1) a single managed PG instance is operationally simpler than 7 independent subsystems, (2) PG has mature HA solutions (streaming replication, patroni) that protect all primitives at once, and (3) the in-memory cache allows agents already running to complete their current task even during a brief PG outage.

### 14.2 Async Engine & Store Pattern

The existing codebase uses synchronous SQLAlchemy. The team coordination layer adds an async engine alongside it — the executor already uses `asyncio`, and `LISTEN/NOTIFY` fundamentally requires async connections. The async engine uses `asyncpg` as the driver and sizes its connection pool to `max_agents + 5` (one connection per agent plus headroom for the dispatcher, cache, and health checks). A single dedicated connection outside the pool is reserved for `LISTEN/NOTIFY` (Section 14.7).

Each domain gets a Store class, following the existing codebase pattern. Standard CRUD operations use the SQLAlchemy ORM; PostgreSQL-specific operations (`SKIP LOCKED`, ltree operators, full-text search, advisory locks, `LISTEN/NOTIFY`) use raw `text()` queries.

> **Implementation note — NoteStore not implemented.** Notes are in-memory (see Section 5.1). A `NoteStore` backed by the `task_notes` table is retained as a reference design for future multi-process scaling. Currently, `read_notes(keyword=...)` filters in-memory notes instead of querying PostgreSQL full-text search.

`FileChangeStore` follows the same store pattern, persisting each file edit to the `file_changes` table via ORM insert and querying scope changes via ltree descendant matching (`path_ltree <@ ANY(scopes)`) with a timestamp filter.

**What uses ORM vs `text()`:**

| Query type | Approach | Why |
|---|---|---|
| Note INSERT/SELECT by ID/deps | **ORM** (`db.add`, `select().where()`) | Standard CRUD, no PG-specific features |
| Task INSERT (plan insertion) | **ORM** (`db.add_all()`) | Bulk insert of model objects |
| Task SELECT by status | **ORM** (`select().where(status == 'ready')`) | Standard filter |
| `pop_ready()` SKIP LOCKED | **`text()`** | Atomic UPDATE+subquery+SKIP LOCKED has no ORM equivalent |
| `mark_done()` conditional UPDATE | **`text()`** | Arithmetic decrement + conditional promotion |
| FTS search (`tsvector`) | **`text()`** | PG-specific full-text operators |
| Scope queries (`ltree <@`) | **`text()`** | PG-specific hierarchical containment |
| `LISTEN/NOTIFY` | **raw asyncpg connection** | No SQLAlchemy abstraction exists |
| Advisory locks | **`text()`** | `SELECT pg_advisory_lock()` |

**LISTEN/NOTIFY access:** SQLAlchemy async exposes the underlying asyncpg connection for PG-specific features:

```python
async with engine.connect() as conn:
    raw = await conn.get_raw_connection()
    asyncpg_conn = raw.driver_connection
    await asyncpg_conn.add_listener(channel, callback)
```

**Alternative for large swarms:** Instead of one LISTEN connection per worker, use a single shared listener connection with in-process fan-out to workers. This caps LISTEN connections at 1 regardless of concurrency. Trade-off: adds ~20 lines of fan-out code.

### 14.3 PostgreSQL-Primary Persistence

PostgreSQL is the single source of truth for all team coordination state. There is no in-memory shadow store. Every write goes to PG (awaited), every read queries PG. This eliminates an entire class of bugs around write ordering, crash windows, and cross-process visibility.

```
Agent calls post_note() or submit_summary()
  |
  +-- await INSERT INTO task_notes (...)
       Durable. Searchable (tsvector). Scope-indexed (ltree).
       Visible to all processes immediately on return.

Agent calls edit_file()
  |
  +-- await INSERT INTO file_changes (...)
       Durable. Queryable via scope_changed_since tool.
       Visible to all processes immediately on return.
```

**Why not dual-write:** An earlier revision of this plan proposed dual-write (PG + in-memory list). This was rejected because:

1. **Write ordering bugs** — PG-first-then-in-memory creates a crash window where PG has the note but the in-memory list doesn't. In-memory-first creates the opposite. Either way, the two stores can disagree.
2. **Hydration complexity** — multi-process deployments require a `hydrate()` call before every `context_for()` to sync the in-memory list from PG. This is easy to forget and hard to test.
3. **Marginal latency benefit** — `context_for()` runs once per task start, not in a hot loop. A PG round-trip (~1ms on localhost, ~5ms networked) is negligible compared to the LLM call that follows (~seconds).
4. **Consistency by construction** — with one store, `search_context` and `context_for` always agree on note visibility. No ordering invariants to maintain.

**Trade-off acknowledged:** Every `read()` and `context_for()` call hits PG. For single-process deployments on localhost this adds ~1ms per query — unmeasurable against LLM latency. If profiling later shows PG reads as a bottleneck (unlikely), a read-through cache can be added as a transparent layer without changing the write path or the consistency model.

### 14.4 Schema

Four tables are created, all requiring the `ltree` extension:

**`task_notes`** — Task Center backing store (currently deferred; notes are in-memory). Partitioned by `team_run_id`. Columns: `id` (UUID), `team_run_id`, `task_id`, `agent_name`, `content` (text), `scope_ltree` (ltree array), `created_at`. Indexes: B-tree on `task_id`, GiST on `scope_ltree`, BRIN on `created_at`, GIN on `tsvector(content)` for full-text search.

**`file_changes`** — Ledger backing store. Partitioned by `team_run_id`. Columns: `id` (bigserial), `team_run_id`, `file_path`, `path_ltree` (scalar ltree), `agent_id`, `edit_type`, `old_hash`, `new_hash`, `created_at`. Indexes: GiST on `path_ltree`, BRIN on `created_at`.

**`tasks`** — Dispatcher work queue. Partitioned by `team_run_id`. Columns mirror the `Task` data model (Section 4.3): `id`, `team_run_id`, `agent_name`, `status`, `task`, `deps` (text array), `scope_paths` (text array), `scope_ltree` (ltree array), `cascade_policy`, `parent_id`, `root_id`, `depth`, `pending_dep_count`, `retry_count`, `max_retries`, `agent_run_id`, and timestamp fields. Indexes: B-tree on `(team_run_id, status)` for work queue pops, B-tree on `(team_run_id, depth, created_at)` for ordered dispatch.

**`exploration_memory`** — Cross-run exploration cache. Not partitioned (shared across runs). Columns: `cache_key` (primary key), `scope_paths`, `content_hash`, `notes` (JSONB), `created_at`, `accessed_at`.

Partition lifecycle is managed by `TeamRun` setup and teardown: `create_partitions(run_id)` creates one partition per table for the run, and `drop_partitions(run_id)` drops them instantly on completion with no vacuum needed. `run_id` is validated against `[a-zA-Z0-9_\-]` before use in partition names.

### 14.5 Index Strategy

| Query pattern | SQL | Index type | Why |
|---|---|---|---|
| Dep notes | `WHERE task_id = ANY($1)` | B-tree on `task_id` | O(log n) exact match |
| Scope hierarchy | `WHERE $1::ltree @> ANY(scope_ltree)` | GiST on `ltree[]` | Hierarchical containment — scalar `@>` (ancestor-of) against each element of the ltree array. `'src.auth' @> 'src.auth.sessionDpy'` is `TRUE`. |
| Changes in scope | `WHERE path_ltree <@ ANY($1::ltree[])` | GiST on `ltree` | Descendant match — scalar `<@` (descendant-of) against each query scope. Finds all files under a directory. |
| Changes since | `WHERE created_at > $1` | BRIN on timestamp | O(pages) for append-only data. Tiny index (KBs). |
| Full-text search | `to_tsvector('english', content) @@ plainto_tsquery($1)` | GIN on tsvector | Built-in PostgreSQL FTS |
| Cache lookup | `WHERE cache_key = $1` | Primary key | O(1) |
| Ready tasks | `WHERE status = 'ready' AND pending_dep_count = 0 FOR UPDATE SKIP LOCKED` | B-tree on `(team_run_id, status)` | Work queue pop — `pending_dep_count = 0` avoids correlated subquery on deps |

**`path_to_ltree()` specification:**

```python
import re

_LTREE_UNSAFE = re.compile(r'[^a-zA-Z0-9_]')

def _escape_char(ch: str) -> str:
    """Escape a non-alphanumeric character to a reversible representation.
    Dots → 'D', hyphens → 'H', others → 'X' + 2-digit hex ordinal.
    This prevents collisions: 'my-module' → 'myHmodule',
    'my_module' → 'my_module' (unchanged). Distinct inputs always
    produce distinct ltree labels."""
    if ch == '.':
        return 'D'
    if ch == '-':
        return 'H'
    return f'X{ord(ch):02x}'

def path_to_ltree(path: str) -> str:
    """Convert a file path to an ltree label path.

    Rules:
      1. Strip leading/trailing slashes.
      2. Split on '/'.
      3. For each path component, replace unsafe characters using
         reversible escaping (_escape_char). This avoids collisions:
         'my-module' and 'my_module' map to different labels.
      4. ltree labels must be [a-zA-Z0-9_], max 256 chars.
      5. Drop empty labels.

    Examples:
      "src/auth/"                → "src.auth"
      "src/auth/session.py"      → "src.auth.sessionDpy"
      "src/auth/__init__.py"     → "src.auth.__init__Dpy"
      "src/payment/utils.v2.py"  → "src.payment.utilsDv2Dpy"
      "src/my-module/foo.py"     → "src.myHmodule.fooDpy"
      "src/my_module/foo.py"     → "src.my_module.fooDpy"
      "/leading/slash"           → "leading.slash"

    Collision safety: 'my-module' → 'myHmodule' vs 'my_module' →
    'my_module'. Distinct paths always produce distinct ltree values.
    """
    parts = path.strip('/').split('/')
    labels = []
    for part in parts:
        label = _LTREE_UNSAFE.sub(lambda m: _escape_char(m.group()), part)
        if label:
            labels.append(label)
    return '.'.join(labels)
```

**Why `ltree` over `TEXT[]` with `&&`:** The `&&` (overlap) operator only checks if two arrays share an element. It cannot match `src/auth/` against `src/auth/session.py` — those are different strings. `ltree` handles hierarchical containment natively: `'src.auth' @> 'src.auth.sessionDpy'` is `TRUE`. This makes all scope queries correct by construction.

**Correct ltree operator usage:** The `@>` (ancestor-of) and `<@` (descendant-of) operators are defined on scalar `ltree` values, not on `ltree[]` arrays. When columns are `ltree[]` (like `scope_ltree`), use `ANY()` to unwrap: `s <@ ANY($1::ltree[])` checks if scalar `s` is a descendant of any element in the query array. For columns that are scalar `ltree` (like `path_ltree`), `path_ltree <@ ANY($1::ltree[])` works directly. Never use `ltree[] @> ltree[]` — that's the array containment operator, not the ltree hierarchy operator.

**Why BRIN on timestamps:** Ideal for append-only tables where timestamps are naturally ordered. BRIN indexes are tiny (KBs, not MBs) and scan only the physical pages that contain matching timestamps. Perfect for `changes_since()` queries.

### 14.6 Dispatcher: PostgreSQL-Backed Work Queue

The dispatcher's `pop_ready()` becomes a single PostgreSQL query using `FOR UPDATE SKIP LOCKED` — a purpose-built work queue primitive:

```python
class PGDispatcher:
    """Dispatcher backed by PostgreSQL. No in-memory DAG state.
    Uses async_sessionmaker from the team engine (Section 14.2).
    ORM for standard CRUD, text() for PG-specific atomic operations."""

    def __init__(self, session_factory: async_sessionmaker):
        self._sf = session_factory

    async def pop_ready(self, run_id: str) -> TaskRecord | None:
        """Atomically claim the next ready task. Lock-free under concurrency.
        Uses text() — the atomic UPDATE+subquery+SKIP LOCKED pattern
        has no ORM equivalent."""
        async with self._sf() as db:
            row = (await db.execute(text("""
                UPDATE tasks SET status = 'running', started_at = NOW()
                WHERE id = (
                    SELECT t.id FROM tasks t
                    WHERE t.team_run_id = :run_id
                      AND t.status = 'ready'
                      AND t.pending_dep_count = 0
                    ORDER BY t.depth, t.created_at
                    LIMIT 1
                    FOR UPDATE SKIP LOCKED
                )
                RETURNING *
            """), {"run_id": run_id})).fetchone()
            await db.commit()
            return TaskRecord.from_row(row) if row else None

    async def mark_done(self, task_id: str, run_id: str) -> list[str]:
        """Mark task done, decrement pending_dep_count on dependents,
        and promote any that reach zero. Uses text() — conditional
        arithmetic UPDATE has no ORM equivalent."""
        async with self._sf() as db:
            await db.execute(text(
                "UPDATE tasks SET status = 'done', finished_at = NOW() "
                "WHERE id = :task_id AND team_run_id = :run_id"),
                {"task_id": task_id, "run_id": run_id})
            # Decrement pending_dep_count for all tasks that depend
            # on the completed task, and promote those that hit zero.
            promoted = (await db.execute(text("""
                UPDATE tasks t
                SET pending_dep_count = pending_dep_count - 1,
                    status = CASE
                        WHEN pending_dep_count - 1 = 0 THEN 'ready'
                        ELSE status
                    END
                WHERE t.team_run_id = :run_id
                  AND t.status = 'pending'
                  AND :task_id = ANY(t.deps)
                  AND t.pending_dep_count > 0
                RETURNING CASE
                    WHEN pending_dep_count = 0 THEN t.id
                    ELSE NULL
                END AS promoted_id
            """), {"run_id": run_id, "task_id": task_id})).fetchall()
            await db.commit()
            return [r.promoted_id for r in promoted
                    if r.promoted_id is not None]

    async def insert_plan(self, run_id: str, tasks: list[TaskSpec],
                          parent_id: str | None = None,
                          parent_depth: int = 0,
                          parent_root_id: str | None = None) -> None:
        """Insert plan tasks atomically via ORM bulk insert.
        Roots start as 'ready', others as 'pending'.

        After insertion, a text() catch-up pass decrements
        pending_dep_count for any deps that are already done
        (handles external deps from prior plans whose mark_done()
        already fired)."""
        async with self._sf() as db:
            records = []
            for spec in tasks:
                status = "ready" if not spec.deps else "pending"
                root_id = parent_root_id if parent_id else spec.id
                records.append(TaskRecord(
                    id=spec.id, team_run_id=run_id,
                    agent_name=spec.agent, status=status,
                    task=spec.task, deps=spec.deps,
                    scope_paths=spec.scope_paths,
                    scope_ltree=[path_to_ltree(p) for p in spec.scope_paths],
                    parent_id=parent_id, root_id=root_id,
                    depth=(parent_depth + 1) if parent_id else 0,
                    pending_dep_count=len(spec.deps),
                ))
            db.add_all(records)
            await db.flush()  # IDs visible for catch-up query

            # Catch-up: decrement pending_dep_count for deps already done.
            # Uses text() — conditional arithmetic UPDATE with CTE.
            await db.execute(text("""
                WITH already_done AS (
                    SELECT id FROM tasks
                    WHERE team_run_id = :run_id AND status = 'done'
                )
                UPDATE tasks t
                SET pending_dep_count = pending_dep_count - (
                        SELECT COUNT(*) FROM already_done ad
                        WHERE ad.id = ANY(t.deps)
                    ),
                    status = CASE
                        WHEN pending_dep_count - (
                            SELECT COUNT(*) FROM already_done ad
                            WHERE ad.id = ANY(t.deps)
                        ) = 0 THEN 'ready'
                        ELSE status
                    END
                WHERE t.team_run_id = :run_id
                  AND t.status = 'pending'
                  AND t.deps && (SELECT array_agg(id) FROM already_done)
            """), {"run_id": run_id})
            await db.commit()
```

**What this replaces:**

| Concern | In-memory dispatcher | PG dispatcher |
|---|---|---|
| Ready queue | `collections.deque` + manual promotion | `WHERE status = 'ready' FOR UPDATE SKIP LOCKED` |
| Concurrency | `asyncio.Lock` around pop | `SKIP LOCKED` — no application lock |
| Crash recovery | Lost — must rebuild from checkpoint | Free — tasks in 'running' at crash time get retried |
| DAG traversal | Manual walk + parent pointers | `pending_dep_count` column, decremented atomically on completion |
| State consistency | Single-process only | Multi-process safe |

### 14.7 Real-Time Scope Awareness: LISTEN/NOTIFY

> **Status: IMPLEMENTED** — Updated 2026-04-13. Uses a buffered flush design where notifications are held in a per-executor `ScopeChangeBuffer` and flushed at the top of each query loop turn, eliminating the need for a timer-based flush loop.

Agents discover concurrent file changes via push, not poll. When the Arbiter records an edit to `file_changes`, a PostgreSQL trigger notifies all listeners:

```sql
CREATE OR REPLACE FUNCTION notify_scope_change() RETURNS trigger AS $$
BEGIN
    PERFORM pg_notify(
        'scope_change_' || NEW.team_run_id,
        json_build_object(
            'file_path', NEW.file_path,
            'agent_id', NEW.agent_id,
            'agent_run_id', COALESCE(NEW.agent_run_id, ''),
            'edit_type', NEW.edit_type
        )::text
    );
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_scope_change
    AFTER INSERT ON file_changes
    FOR EACH ROW EXECUTE FUNCTION notify_scope_change();
```

#### Architecture: Three Components

```
Arbiter.record_edit()
  → INSERT INTO file_changes
    → trg_scope_change (PG trigger)
      → pg_notify('scope_change_{run_id}', payload)
        │
        │ pushed over dedicated PG connection
        ▼
ScopeChangeListener (1 per TeamRun, 1 PG connection)
  → _route_notification(payload)
    → filter: skip own agent_run_id
    → filter: file_path must match subscriber's scope_paths
    → buffer.buffer(change) into per-executor ScopeChangeBuffer
        │
        │ held until top of next query loop turn
        ▼
Query loop top-of-turn flush
  → scope_buffer.flush_into(display_messages)
    → ONE SystemReminderBlock (replaces previous)
    → compact_for_api() picks it up → LLM sees it
```

#### ScopeChangeListener

One shared instance per `TeamRun`. Uses a single dedicated async connection outside the pool for `LISTEN`. Routes notifications to per-executor `ScopeChangeBuffer` instances based on scope and agent identity filtering.

```python
class ScopeChangeListener:
    """Single shared LISTEN connection with in-process fan-out."""

    def __init__(self, engine: AsyncEngine, run_id: str):
        self._engine = engine
        self._channel = f"scope_change_{run_id}"
        self._subscribers: dict[str, _Subscription] = {}  # agent_run_id → sub

    async def start(self) -> None:
        # Dedicated connection outside pool for LISTEN
        self._conn = await self._engine.connect()
        raw = await self._conn.get_raw_connection()
        # LISTEN via psycopg, poll via notifies() async generator
        await raw.cursor().execute(f"LISTEN {self._channel}")
        self._listen_task = asyncio.create_task(self._poll_loop(raw.dbapi_connection))

    def _route_notification(self, payload: str) -> None:
        change = json.loads(payload)
        for sub in self._subscribers.values():
            if change["agent_run_id"] == sub.agent_run_id:
                continue  # don't notify about own edits
            if any(change["file_path"].startswith(p.rstrip("/"))
                   for p in sub.scope_paths):
                sub.buffer.buffer(change)

    def subscribe(self, agent_run_id, scope_paths, buffer): ...
    def unsubscribe(self, agent_run_id): ...
    async def stop(self) -> None: ...
```

**Files:** `team/runtime/scope_change_listener.py` (~50 lines)

#### ScopeChangeBuffer

Per-executor notification buffer with replacement semantics. Buffers scope change notifications and flushes them as a single `SystemReminderBlock` at the top of each query loop turn. Only one active notification exists in `display_messages` at a time — previous notifications are marked with `category="scope_change_superseded"` so the compactor can drop them.

```python
class ScopeChangeBuffer:
    """Per-executor buffer. Flushed at top of each query loop turn."""

    def buffer(self, change: dict) -> None:
        """Called by ScopeChangeListener. Deduplicates by file_path."""
        self._pending[change["file_path"]] = change

    def flush_into(self, display_messages: list) -> bool:
        """Flush ONE SystemReminderBlock, replacing previous notification."""
        if not self._pending:
            return False
        changes = list(self._pending.values())
        self._pending.clear()
        # Mark previous notification as superseded
        if self._last_notification_idx is not None:
            old = display_messages[self._last_notification_idx]
            old.content[0].category = "scope_change_superseded"
        self._last_notification_idx = len(display_messages)
        display_messages.append(ConversationMessage(role="user", content=[
            SystemReminderBlock(category="scope_change", text=...)]))
        return True
```

**Files:** `team/runtime/scope_change_buffer.py` (~30 lines)

#### Lifecycle Wiring

```
TeamRun.start()
  → _start_scope_listener()
    → ScopeChangeListener(get_team_engine(), self.id).start()

Executor._run_one(task_id)
  → _subscribe_scope_listener(task, agent_run_id, ctx)
    → ScopeChangeBuffer() created
    → listener.subscribe(agent_run_id, scope_paths, buffer)
    → buffer injected into ctx.tool_metadata.extras["scope_change_buffer"]
  → await self.runner(defn, ctx)
    # Query loop: each turn calls scope_buffer.flush_into(display_messages)
  → finally: _unsubscribe_scope_listener(agent_run_id)

TeamRun.wait() / TeamRun.cancel()
  → _stop_scope_listener()
    → listener.stop()
```

#### Why Buffered Flush, Not Timer-Based

The original design (pre-implementation) specified a 5-second timer-based flush loop. The implementation uses the query loop's natural turn boundary instead:

| Aspect | Timer flush (original) | Turn-boundary flush (implemented) |
|---|---|---|
| Timing | Fixed 5s interval via `asyncio.sleep` | Top of each query loop turn |
| Extra coroutine | Yes — `_flush_loop()` task | No — flush is a sync call in the loop |
| Thread safety | Callback appends to list read by loop | Buffer in callback, flush synchronously in loop |
| Accumulation | Append-only, grows forever | Replace previous, compactor drops superseded |
| Lifecycle | Timer must be started/cancelled | No lifecycle — buffer exists, loop calls flush |

The query loop rebuilds `api_messages` from `display_messages` via `compact_for_api()` at the start of every turn. This means anything appended to `display_messages` between turns is automatically picked up. The turn boundary IS the natural flush point — no timer needed.

#### Backpressure

Three filters stack to keep notification volume low:

1. **Scope filter** — agent only sees NOTIFYs for files matching its `scope_paths`. With disjoint scopes (the expected case), most agents see zero notifications.
2. **Self-filter** — `agent_run_id` check excludes the agent's own edits.
3. **File-path dedup** — `ScopeChangeBuffer._pending` deduplicates by file_path (latest wins). Multiple edits to the same file produce one notification line.
4. **Replacement semantics** — `flush_into()` replaces the previous `SystemReminderBlock` instead of accumulating. The agent sees at most one scope notification at any time, containing only changes since the last flush.

#### Graceful Degradation

If `get_team_engine()` returns `None` (no PG configured) or the LISTEN connection fails, `_start_scope_listener` logs a debug message and returns. The `scope_listener` attribute remains `None`. The executor's `_subscribe_scope_listener` checks `is_running` and skips subscription. All pull-based mechanisms (`_inject_scope_warnings`, `context_changed_since`) continue to work unchanged.

#### What This Solves

`context_for()` builds a snapshot at task start that goes stale during long-running tasks. `LISTEN/NOTIFY` closes this gap with push-based, real-time warnings about concurrent edits in the agent's scope. The notification arrives one turn late (buffered during tool execution, visible on the next LLM call), which is acceptable because:

1. The LLM cannot act on information mid-stream — it's committed to its current tool call
2. The Arbiter catches actual file conflicts at edit time regardless of notification timing
3. One turn of latency is negligible compared to the information value of the notification

### 14.8 Agent Search Tools

Two tools available to all roles. `search_context` is absorbed into `read_notes` with a `keyword` parameter (in-memory filtering, no PG FTS). `scope_changed_since` delegates to `FileChangeStore`:

> **Implementation note:** The original design specified `SearchContextTool` delegating to `NoteStore.search_fts()` for PostgreSQL full-text search. Since notes are in-memory, search is implemented as keyword filtering inside `ReadNotesTool` in `tools/context/toolkit.py`. The PG-backed FTS design below is retained as reference for multi-process scaling.

```python
# REFERENCE DESIGN — not currently used (notes are in-memory)
class SearchContextTool(BaseTool):
    """Search notes by keyword and/or scope. Delegates to NoteStore."""
    name = "search_context"

    async def execute(self, arguments, context):
        query = arguments.get("query")
        scope = arguments.get("scope_paths")
        limit = arguments.get("limit", 10)
        run_id = context.metadata["team_run_id"]
        note_store: NoteStore = context.metadata["note_store"]

        ltree_scopes = [path_to_ltree(p) for p in scope] if scope else None
        rows = await note_store.search_fts(run_id, query, ltree_scopes, limit)
        return [{"task_id": r.task_id, "agent": r.agent_name,
                 "summary": r.content[:500], "scope": r.scope_ltree}
                for r in rows]


class ScopeChangedSinceTool(BaseTool):
    """Check what files changed in scope since a timestamp.
    Delegates to FileChangeStore."""
    name = "scope_changed_since"

    async def execute(self, arguments, context):
        paths = arguments["paths"]
        since = arguments["since"]
        run_id = context.metadata["team_run_id"]
        fc_store: FileChangeStore = context.metadata["file_change_store"]

        ltree_scopes = [path_to_ltree(p) for p in paths]
        rows = await fc_store.changes_in_scope(run_id, ltree_scopes, since)

        if not rows:
            return {"changed": False}
        return {"changed": True, "files": [
            {"path": r.file_path, "agent": r.agent_id,
             "type": r.edit_type,
             "seconds_ago": int(time.time() - r.created_at.timestamp())}
            for r in rows
        ]}
```

### 14.9 How Agents Use Search

```
Developer starts working on src/auth/middleware.py
  |
  +-- context_for() provides (from in-memory, fast):
  |     task description + scope
  |     dep notes (explorer findings)
  |     ledger changes in scope
  |     parent chain rationale
  |
  +-- LISTEN/NOTIFY provides (push, real-time):
  |     "Warning: src/auth/session.py edited by agent_A 2s ago"
  |     Agent re-reads file before editing
  |
  +-- Mid-run, agent wants more context:
  |     search_context(query="timeout handling", scope_paths=["src/auth/"])
  |     -> PostgreSQL full-text search, returns relevant notes from this run
  |
  +-- Before committing a large multi-file change:
  |     scope_changed_since(paths=["src/auth/"], since=task_started_at)
  |     -> PostgreSQL ledger query, returns file changes since task started
  |
  +-- Edit-time conflict:
        Arbiter handles it inside the tool (in-memory or advisory lock)
```

### 14.10 Cache/Search Consistency

> **Status: N/A.** Both `TaskCenter` and `ExplorationMemory` are in-memory (no PG-backed `NoteStore`). Since both live in the same process, cached exploration notes inserted into `TaskCenter._notes` are immediately visible to all reads including `read_notes(keyword=...)`. There are no separate PG partitions to synchronize.
>
> If `NoteStore` is reintroduced for multi-process scaling, the batch-insert fix described below would become relevant.

~~When `ExplorationMemory.check()` returns cached notes from a previous run, those notes exist in in-memory `TaskCenter._notes` but NOT in the current run's `task_notes` partition. Agents using `search_context` won't find cached exploration data.~~

~~**Fix:** On cache hit, batch-insert cached notes via NoteStore.~~

### 14.11 Advisory Locks for Multi-Process Arbiter (DEFERRED)

> **Status: Not yet implemented.** The current Arbiter uses in-process `threading.Lock` for per-file locking, which is correct for single-process deployments. Advisory locks are needed only for multi-process horizontal scaling, which is not the current deployment model.

The current Arbiter uses in-memory `asyncio.Lock` for per-file locking. For multi-process executor deployments (horizontal scaling), PostgreSQL advisory locks provide distributed file-level locking with zero additional infrastructure:

```python
class PGArbiter:
    """Arbiter with PostgreSQL advisory locks. Multi-process safe.
    Uses SQLAlchemy text() — advisory locks are PG-specific."""

    def __init__(self, session_factory: async_sessionmaker):
        self._sf = session_factory

    async def acquire_file_lock(self, file_path: str) -> None:
        lock_key = self._path_to_lock_key(file_path)
        async with self._sf() as db:
            await db.execute(text("SELECT pg_advisory_lock(:key)"),
                             {"key": lock_key})

    async def release_file_lock(self, file_path: str) -> None:
        lock_key = self._path_to_lock_key(file_path)
        async with self._sf() as db:
            await db.execute(text("SELECT pg_advisory_unlock(:key)"),
                             {"key": lock_key})

    def _path_to_lock_key(self, path: str) -> int:
        """Stable hash of file path to PG advisory lock key (int8)."""
        return int(hashlib.sha256(path.encode()).hexdigest()[:15], 16)
```

**When to use:** Optional. Single-process deployments keep the in-memory `asyncio.Lock` (faster, simpler). Switch to advisory locks when running multiple executor processes against the same sandbox.

### 14.12 Updated Toolkit Matrix (Dispatched Roles)

Explorer is a subagent (see Section 10.3) — its toolkits are defined in its agent definition, not here.

| Toolkit | Planner | Developer | Reviewer | Replanner |
|---------|:-------:|:---------:|:--------:|:---------:|
| `code_intelligence` | read (blocked: ci_read_file) | full | full | read (blocked: ci_read_file) |
| `sandbox_operations` | -- | full | full | -- |
| `subagent` | spawn (explorer only) | -- | -- | -- |
| `context` | read-only (blocked: post_note) | full | full | read-only (blocked: post_note) |
| `submission` | submit_plan | done, retry, replan | done, retry, replan | submit_replan |

See Section 10.4 for toolkit consolidation rationale (2 toolkits instead of 5).

---

*End of document.*
