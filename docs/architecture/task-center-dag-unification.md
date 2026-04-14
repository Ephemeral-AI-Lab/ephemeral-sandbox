# TaskCenter + DAG Unification

**Status:** IMPLEMENTED  
**Date:** 2026-04-14  
**Branch:** `codex/pydantic-benchmark-loop`  
**Author:** Architecture session  
**Prerequisite for:** Dynamic Replanning Blocker Protocol (dynamic-replanning-blocker-protocol.md)  
**Depends on:** Plan A Team Coordination Redesign (plan-a-team-coordination-redesign.md)

---

## Table of Contents

1. [Problem Statement](#1-problem-statement)
2. [Design Goals](#2-design-goals)
3. [Current Architecture — The Seam](#3-current-architecture--the-seam)
4. [Unified Architecture](#4-unified-architecture)
5. [TaskCenter — Unified API](#5-taskcenter--unified-api)
6. [DispatchQueue — Extracted API](#6-dispatchqueue--extracted-api)
7. [Persistence Strategy](#7-persistence-strategy)
8. [Migration Map — What Moves Where](#8-migration-map--what-moves-where)
9. [Executor Simplification](#9-executor-simplification)
10. [Impact on Blocker Protocol](#10-impact-on-blocker-protocol)
11. [Migration Path](#11-migration-path)
12. [Files Changed](#12-files-changed)
13. [Implementation Phases](#13-implementation-phases)

---

## 1. Problem Statement

The current system splits task management across two components with no clear boundary:

    TaskCenter       knows about notes (what agents said)
    Dispatcher       knows about tasks (what agents do)

Every consumer bridges the gap. context_for walks the DAG to build note context. read_sibling_notes queries the DAG to find note authors. The Conductor pauses tasks via the Dispatcher and posts notes via the TaskCenter. The executor calls both on every task lifecycle event.

This split is artificial. Notes and tasks are two views of the same thing — a task's full lifecycle. The split forces:

    - context_for to accept a task_lookup callback (to reach the DAG)
    - read_sibling_notes to accept a dispatcher_store (to resolve subtree)
    - The Conductor to hold references to both
    - The executor to mediate between them on every event
    - Every new feature (blocker protocol, active mode) to wire both

---

## 2. Design Goals

    DG-1    Single source of truth for tasks
            One component owns task structure, state, and context.
            Consumers call one API, not two.

    DG-2    Dispatch queue stays atomic
            pop_ready with FOR UPDATE SKIP LOCKED is a proven pattern.
            It stays as its own thin component with SQL atomicity.

    DG-3    Minimal persistence change
            Tasks stay in PostgreSQL (existing schema, existing atomic ops).
            Notes stay in-memory (existing behavior, fast reads).
            No new tables. No migration of notes to SQL.

    DG-4    Dispatcher class absorbed
            The Dispatcher class disappears. Its methods become
            TaskCenter methods or DispatchQueue methods.
            No wrapper, no delegation — direct ownership.

    DG-5    query.py simplified
            The DAG unification is a backend restructuring.
            Posthook logic removed from query loop (moved to executor
            post-run via external_trigger.runner).

---

## 3. Current Architecture — The Seam

### Component Responsibilities

    TaskCenter (team/task_center.py)
        In-memory. ~250 lines.

        post(note)                  append note to log
        read(filters)               query notes by author/scope/keyword
        context_for(task)           build context string for agent
            walks deps              (needs task_lookup callback)
            walks parent chain      (needs task_lookup callback)
            reads file changes      (needs file_change_store)

    Dispatcher (team/runtime/dispatcher.py)
        Orchestrator. ~640 lines.

        complete(task_id, result)   handle completion, plan expansion
        retry_work_item(task_id)    retry a task
        request_replan(task_id)     trigger replan
        apply_replan(task_id, plan) apply replan result
        sibling_stats(parent_id)    sibling status counts
        refresh_graph()             reload task graph

    DispatcherStore (team/runtime/dispatcher_store.py)
        PostgreSQL persistence. ~840 lines.

        pop_ready(run_id)           atomic task claiming
        mark_running(task_id)       set RUNNING
        mark_done(task_id)          dec pending_dep_count, promote
        fail_task(task_id)          fail + cascade
        retry_task(task_id)         reset to READY
        insert_plan(specs)          insert child tasks
        cascade_cancel_recursive()  recursive CTE cancel
        maybe_promote_expanded()    parent promotion
        request_replan()            fail + insert replanner
        cancel_by_ids()             cancel specific tasks
        get_adjacency()             {id: deps} for cycle detection
        get_statuses()              {id: status}
        sibling_stats()             status counts for siblings
        get_all_tasks()             full task list
        get_task()                  single task lookup

### The Cross-References

    context_for needs:
        task.deps           → from DispatcherStore (task_lookup callback)
        parent chain        → from DispatcherStore (task_lookup callback)
        file changes        → from FileChangeStore
        notes               → from TaskCenter (self)

    read_sibling_notes needs:
        subtree task IDs    → from DispatcherStore (get_subtree_task_ids)
        notes               → from TaskCenter (self)

    Conductor needs:
        pause tasks         → from Dispatcher (block_task)
        post notes          → from TaskCenter (post)
        resume tasks        → from Dispatcher (unblock_tasks)
        check siblings      → from DispatcherStore (sibling_stats)

    Executor needs:
        pop task            → from DispatcherStore (pop_ready)
        build context       → from TaskCenter + Dispatcher
        complete task       → from Dispatcher (complete)
        post note           → from TaskCenter (post)

Every consumer holds references to both components and orchestrates
between them. The boundary is a liability, not an asset.

### Diagram — Current

    Executor
    ├──────────► Dispatcher ──────► DispatcherStore ──► PostgreSQL
    │            (orchestrator)     (task persistence)   (tasks table)
    │
    ├──────────► TaskCenter
    │            (in-memory notes)
    │
    ├──────────► Conductor
    │            ├──► Dispatcher (pause/resume)
    │            └──► TaskCenter (post notes)
    │
    └──────────► FileChangeStore

    Four components. Every operation crosses 2-3 of them.

---

## 4. Unified Architecture

### Diagram — After

    Executor
    ├──────────► TaskCenter ─────────────────────► PostgreSQL
    │            (unified: structure + state       (tasks table)
    │             + context + notes + planning)
    │            ├── task records
    │            ├── notes (in-memory)
    │            ├── deps + parent chain
    │            ├── status transitions
    │            ├── plan insertion
    │            ├── cascade operations
    │            ├── sibling/subtree queries
    │            └── active mode (auto-notes)
    │
    ├──────────► DispatchQueue ──────────────────► PostgreSQL
    │            (thin: pop_ready + claim)          (same tasks table)
    │            Two methods. ~50 lines.
    │
    ├──────────► Conductor
    │            └──► TaskCenter (pause/resume/notes — one reference)
    │
    └──────────► FileChangeStore

    Two components for tasks (down from three).
    Every task operation goes through TaskCenter.
    DispatchQueue only reads + atomically claims.

### Separation of Concerns

    TaskCenter                          DispatchQueue
    "what tasks exist, what they       "what to execute next"
     know, and what state they're in"

    Owns:                               Owns:
    - task records                      - atomic claiming
    - dependencies                        (FOR UPDATE SKIP LOCKED)
    - parent/child relationships        - priority ordering
    - status transitions                  (depth, created_at)
    - pending_dep_count
    - notes (in-memory log)             Reads from:
    - context building                  - TaskCenter's task state
    - plan insertion/replan               (same PostgreSQL table)
    - cascade operations
    - promotion logic                   Writes:
    - sibling/subtree queries           - status = 'running' only
    - active mode (auto-notes)            (the claiming operation)
    - blocker operations

    The line is clear:
    TaskCenter manages task lifecycle.
    DispatchQueue manages execution scheduling.

---

## 5. TaskCenter — Unified API

### Class Definition

    TaskCenter

        Constructor
            session_factory     async_sessionmaker (PostgreSQL, for task persistence)
            team_run_id         str (owning TeamRun ID — stored internally, so
                                methods below do not accept a run_id parameter)
            budgets             BudgetConfig
            budget_state        BudgetState
            goal                str (optional)
            user_request        str (optional)
            file_change_store   FileChangeStore or None
            max_checkpoints     int (default 10)
            event_store         TeamRunStore or None
            checkpoint_store    Any or None

        Internal delegation:
            self._store = TaskStore(session_factory, team_run_id)
            SQL persistence lives in TaskStore; TaskCenter delegates to it.

        --- STRUCTURE (from DispatcherStore, delegated to TaskStore) ---

        insert_plan(specs, parent_id, parent_depth, parent_root_id)
            Insert child tasks atomically. Catch-up pass for
            already-done deps. Sets parent_id, root_id, depth.

        get_task(task_id) returns Task or None
            Fetch a single task by ID.

        get_all_tasks() returns list of TaskRecord
            Fetch all tasks for the run. Used by checkpoints and metrics.

        get_adjacency() returns dict of id to list of deps
            Lightweight: just {id: deps} for cycle detection.

        get_siblings_and_descendants(initiating_task_id) returns list of TaskRecord
            All siblings of the given task plus their entire subtrees.
            Used by Conductor for blocker assessment scope.

        _sibling_subtree_ids(parent_id) returns list of str  [private]
            All task IDs under a parent. Used by read_sibling_notes.

        --- STATE (from Dispatcher + DispatcherStore) ---

        mark_running(task_id, agent_run_id) returns Task
            Set status to RUNNING. Assign agent_run_id. Charge budget.

        complete_task(task_id, result) returns list of Task
            Mark DONE. Decrement pending_dep_count for dependents.
            Promote expanded parents. Handle plan expansion if
            result contains a submitted_plan or submitted_replan.

        fail(task_id, reason)
            Mark FAILED. Cascade cancel dependents (based on
            cascade_policy).

        retry_task(task_id, request: RetryRequest)
            If retries remaining: reset to READY, increment retry_count.
            If exhausted: mark FAILED, cascade.

        pause_running_task(task_id, blocker_id, checkpoint, verdict) returns bool
            Set status to PAUSED. Store blocker_id, checkpoint, verdict.
            Only RUNNING tasks can be paused.

        resume_paused_tasks(blocker_id) returns int
            Bulk transition PAUSED → READY for all tasks paused by this blocker.

        cancel_paused_tasks(blocker_id) returns int
            Cancel all tasks paused by this blocker (on fix failure).

        cancel_by_ids(task_ids, reason) returns int
            Cancel specific tasks by ID.

        cascade_cancel_recursive(root_task_id) returns list of str
            Recursive CTE cancel of all dependents.

        cancel_all_pending() returns int
        cancel_all_running(reason) returns int

        sibling_stats(parent_id) returns dict
            Status counts for all siblings (same parent).

        --- PLANNING (from Dispatcher) ---

        request_replan(task_id, request: ReplanRequest) returns Task
            Mark task FAILED. Insert replanner task.
            Siblings are NOT cancelled (replanner decides).
            Returns the replanner Task.

        apply_replan(replan_task_id, add_tasks, cancel_ids, depth, parent_id, root_id)
            Validate new tasks (cycle detection, agent resolution).
            Cancel specified tasks. Insert new tasks.

        --- CONTEXT (existing TaskCenter) ---

        post(note)
            Append note to in-memory log.
            Call on_note_posted for activity tracking.

        read(authors, scope_paths, since, limit) returns list of Note
            Query notes with optional filters.

        context_for(task, max_context_bytes) returns str
            Build context string for a task.
            Priority order: retry context, task description, self-notes,
            dep notes, file changes, parent chain.
            Now calls get_task internally — no callback needed.

        read_sibling_notes(parent_id, keyword, scope_paths) returns list of Note
            Resolve subtree via get_subtree_task_ids.
            Read all notes from those tasks.
            No external dispatcher_store parameter needed.

        --- ACTIVE MODE (new, see task-center-active-mode.md) ---

        on_edit(task_id, file_path)
        on_posthook(task_id)
        tick(task_id)
        on_note_posted(note: Note)
            Reset counters for the note's task_id.
            Ignores system/checkpoint notes.
        should_checkpoint(task_id) returns str or None
            Check thresholds. Returns "edit" or "turn" or None.
        check(task_id, *, snapshot, api_client, model) returns bool
            Spawn external_trigger agent if thresholds crossed.
            Falls back to factual counter-based note when no api_client.

        --- QUERIES ---

        get_statuses(run_id) returns dict of id to status
        cancel_all_pending(run_id) returns int
        cancel_all_running(run_id, reason) returns int

### What context_for Gains

    BEFORE:
        context_for(task, file_change_store, task_lookup, max_context_bytes)
            task_lookup is a callback to reach the DAG (Dispatcher).
            file_change_store is passed in.
            Must be wired by the executor each time.

    AFTER:
        context_for(task, max_context_bytes)
            TaskCenter owns the DAG. No callback needed.
            file_change_store is set at construction.
            Two parameters instead of four.

### What read_sibling_notes Gains

    BEFORE:
        read_sibling_notes(parent_id, dispatcher_store, keyword, scope_paths)
            Needs dispatcher_store to resolve subtree IDs.
            Crosses the component boundary.

    AFTER:
        read_sibling_notes(parent_id, keyword, scope_paths)
            TaskCenter owns get_subtree_task_ids.
            No external reference needed.

---

## 6. DispatchQueue — Extracted API

### Class Definition

    DispatchQueue

        Constructor
            session_factory     async_sessionmaker (same PostgreSQL connection)

        pop_ready(run_id, blocker_guard) returns TaskRecord or None
            Atomically claim the next READY task.
            Uses FOR UPDATE SKIP LOCKED for concurrent safety.
            Calls blocker_guard(candidate) before returning.
            Guard returns True to ALLOW dispatch, False to SKIP.
            Skipped candidates are not mutated — just passed over.

            Implementation: SELECTs up to 32 READY candidates with
            SKIP LOCKED, iterates until one passes the guard, then
            UPDATEs that single row to 'running'.

    One method. Same SQL. Same atomicity guarantees.
    The only difference: it is no longer bundled with 800 lines
    of task lifecycle management.

    Note: the former mark_running / claim step is now handled by
    TaskCenter.mark_running(task_id, agent_run_id) which assigns
    the agent_run_id after pop_ready returns.

### Blocker Guard Integration

    DispatchQueue does NOT know about blockers.
    It accepts a guard function from the Conductor:

    Executor calls:
        task = dispatch_queue.pop_ready(
            run_id,
            blocker_guard=conductor.guard_pop_ready
        )

    The guard is a callable:
        guard(task) returns True if task is ALLOWED to dispatch
        guard(task) returns False if task should be SKIPPED

    Conductor.guard_pop_ready: allows only resolver fix tasks
    while any blocker is active. All other candidates are skipped
    (their status stays READY — no mutation).

    This keeps DispatchQueue free of blocker knowledge.
    The Conductor owns the guard logic.

---

## 7. Persistence Strategy

### Dual Backend (Option A)

    PostgreSQL                          In-Memory
    ├── task records                    ├── notes (append-only log)
    │   id, status, deps,              │   fast reads for context_for
    │   parent_id, scope_paths,        │   no durability needed
    │   pending_dep_count,             │   (notes live within a TeamRun)
    │   blocker_ids, paused_from,      │
    │   pause_checkpoint,              ├── activity counters
    │   pause_verdict                  │   (edits_since_note, turns_since_posthook)
    │                                  │   (per-task, reset on note/posthook)
    ├── dispatch queue                 │
    │   pop_ready (FOR UPDATE          ├── active blockers
    │   SKIP LOCKED)                   │   (Conductor's in-memory set)
    │                                  │
    └── adjacency / cycle detection    └── latest conversation snapshots
        (read-only queries)                (references from on_turn callback)

    Tasks need PostgreSQL for:
        atomic claiming (FOR UPDATE SKIP LOCKED)
        crash recovery (WAL)
        concurrent executor safety (row-level locks)

    Notes do NOT need PostgreSQL:
        no concurrent writes (single TeamRun event loop)
        no crash recovery needed (notes are ephemeral within a run)
        read frequency is high (context_for on every dispatch)

    The TaskCenter wraps both backends. Consumers see one API.

---

## 8. Migration Map — What Moves Where

### From DispatcherStore to TaskCenter

    DispatcherStore method             TaskCenter method
    ────────────────────               ─────────────────
    mark_done                          _mark_done (private, called by complete_task)
    fail_task                          fail (public) / _fail_task_sql (private)
    retry_task                         retry_task (accepts RetryRequest)
    insert_plan                        insert_plan (delegates to TaskStore)
    request_replan                     request_replan (accepts ReplanRequest)
    cancel_by_ids                      cancel_by_ids
    cascade_cancel_recursive           cascade_cancel_recursive
    maybe_promote_expanded_parent      _maybe_promote_expanded_parent (private)
    get_task                           get_task
    get_all_tasks                      get_all_tasks (returns TaskRecord list)
    get_adjacency                      get_adjacency
    get_statuses                       get_statuses
    sibling_stats                      sibling_stats
    get_subtree_task_ids               _sibling_subtree_ids (private)
    get_siblings_and_descendants       get_siblings_and_descendants (new)
    cancel_all_pending                 cancel_all_pending
    cancel_all_running                 cancel_all_running
    block_task / block_task_with_chk   pause_running_task (renamed)
    unblock_tasks                      resume_paused_tasks (renamed)
    (new)                              cancel_paused_tasks (for fix failure)

### From DispatcherStore to DispatchQueue

    DispatcherStore method             DispatchQueue / TaskCenter method
    ────────────────────               ────────────────────────────────
    pop_ready                          DispatchQueue.pop_ready (with blocker_guard)
    mark_running                       TaskCenter.mark_running (assigns agent_run_id)

### From Dispatcher to TaskCenter

    Dispatcher method                  TaskCenter method
    ─────────────────                  ─────────────────
    complete                           complete_task
    retry_work_item                    retry_task
    request_replan                     request_replan
    apply_replan                       apply_replan
    sibling_stats                      sibling_stats
    refresh_graph                      (removed — no in-memory graph cache)

### Absorbed (Dispatcher class removed)

    Dispatcher._emit (event emission)  moved to TaskCenter or TeamRun
    Dispatcher._charge_tasks (budget)  moved to TaskCenter
    Dispatcher.new_id                  moved to TaskCenter
    BudgetState tracking               moved to TaskCenter

---

## 9. Executor Simplification

### Before (three components, five references)

    Executor holds:
        self.team_run.dispatcher        (Dispatcher)
        self.team_run.dispatcher.store  (DispatcherStore, via Dispatcher)
        self.team_run.task_center       (TaskCenter)
        self.team_run.conductor         (Conductor)
        self.team_run.file_change_store (FileChangeStore)

    Task lifecycle:
        task = dispatcher.store.pop_ready(run_id)
        dispatcher.mark_running(task.id, agent_run_id)
        ctx = build_query_context(defn, team_run, task)
            internally calls: task_center.context_for(task, ..., task_lookup=...)
        result = run_agent(ctx)
        dispatcher.complete(task.id, result)
            internally calls: store.mark_done / store.insert_plan / ...
        task_center.post(completion_note)
        conductor.on_task_completed(task, result)

### After (two components, three references)

    Executor holds:
        self.team_run.task_center       (TaskCenter — unified)
        self.team_run.dispatch_queue    (DispatchQueue — thin)
        self.team_run.conductor         (Conductor)

    Task lifecycle:
        rec = dispatch_queue.pop_ready(run_id, conductor.guard_pop_ready)
        task = task_center.mark_running(task.id, agent_run_id)
        ctx = task_center.context_for(task)
        run_agent(ctx)                              # query loop
        result = _run_post_run(task, defn, ctx)     # post-run tool phase
        _dispatch(task, result)                     # routes to complete/fail/retry/replan/blocker
            task_center.complete_task(task.id, result)
                internally: mark_done, dec deps, promote parent,
                handle plan expansion

    Four calls instead of six. One component for all task operations.
    Post-run phase uses runner.run() with posthook tools when no
    in-loop submission was captured in metadata.

### Diagram — Executor Event Flow

    Executor
          |
          | pop_ready
          v
    DispatchQueue -----(reads)-----> PostgreSQL (tasks table)
          |
          | task returned
          v
    TaskCenter.context_for(task)
          |
          | context string (deps, notes, file changes, parent chain)
          v
    run_agent(context)
          |
          | result
          v
    TaskCenter.complete_task(task_id, result)
          |
          +--- mark_done (PostgreSQL)
          +--- decrement pending_dep_count (PostgreSQL)
          +--- promote_parent if all children done (PostgreSQL)
          +--- post completion note (in-memory)
          +--- notify Conductor (if fix task or blocker-related)
          +--- emit event (for UI/metrics)

    One call to TaskCenter handles everything.
    The executor does not orchestrate between components.

---

## 10. Impact on Blocker Protocol

### What Gets Simpler

    BEFORE (blocker protocol doc, current design):
        Conductor holds references to:
            dispatcher          (for pause/resume/status)
            task_center         (for notes)
            blocker_store       (for blocker records)
            _executor_registry  (for conversation snapshots)

        Conductor.pause_all calls:
            dispatcher.store.block_task (for non-running)
            dispatcher.store.tasks_with_scope_overlap (for candidates)
            task_center.post (for blocker notes)

        Conductor.resume_all calls:
            dispatcher.store.unblock_tasks
            task_center.post (for resume notes)

    AFTER (unified):
        Conductor holds references to:
            _team_run           (for task_center, api_client, cancel_agent_run)
            _blocker_store      (optional, for durable persistence)
            _executor_snapshots (dict: task_id → display_messages snapshot)

        Conductor._assess_running calls:
            task_center.get_siblings_and_descendants (for candidates)
            assess_pause (external_trigger) per RUNNING candidate
            task_center.pause_running_task (for YES verdicts)
            team_run.cancel_agent_run (terminate the asyncio task)

        Conductor._resume_paused calls:
            task_center.resume_paused_tasks(blocker_id)
            (all one component)

### Replanner Integration

    BEFORE:
        Replanner calls add_tasks
            → executor calls dispatcher.apply_replan
            → dispatcher.store.insert_plan
            → dispatcher.store.cancel_by_ids

        Replanner calls declare_blocker
            → executor calls conductor.create_blocker
            → conductor calls dispatcher.store.block_task
            → conductor calls task_center.post

    AFTER:
        Replanner calls add_tasks
            → executor calls task_center.apply_replan

        Replanner calls declare_blocker
            → executor calls conductor.create_blocker
            → conductor calls task_center.block_task
            → conductor calls task_center.post
            (all one component)

---

## 11. Migration Path

### Strategy — Absorb, Don't Rewrite

The migration is an absorption, not a rewrite. The SQL in DispatcherStore is proven and unchanged. The methods move to TaskCenter with the same implementation. The Dispatcher's orchestration logic merges into TaskCenter's methods.

    Step 1: Move SQL methods from DispatcherStore to TaskCenter
            (copy the SQL, same session_factory, same queries)

    Step 2: Move orchestration from Dispatcher to TaskCenter
            (complete, retry_work_item, request_replan, apply_replan
             become TaskCenter methods that call the moved SQL)

    Step 3: Extract pop_ready + mark_running into DispatchQueue
            (~50 lines, same SQL)

    Step 4: Update Executor to call TaskCenter + DispatchQueue
            instead of Dispatcher + DispatcherStore + TaskCenter

    Step 5: Update Conductor to reference TaskCenter only

    Step 6: Delete Dispatcher class and DispatcherStore class
            (all methods have been moved)

    Step 7: Update imports across the codebase

### What Does NOT Change

    PostgreSQL schema          same tasks table, same columns
    SQL queries                same queries, same atomicity
    TaskCenter note API        same post/read/context_for interface
    Query loop (query.py)      posthook logic removed, post-run via executor
    Agent tools                posthook tools tagged tool_types={"post_run"}
    external_trigger/          replaces ephemeral_task/ (shared runner)

---

## 12. Files Changed

    MODIFIED
        team/task_center.py
            Absorbs all DispatcherStore methods (SQL persistence)
            Absorbs all Dispatcher orchestration methods
            + session_factory in constructor (for PostgreSQL access)
            + file_change_store in constructor
            context_for: remove task_lookup parameter
            read_sibling_notes: remove dispatcher_store parameter

        team/runtime/executor.py
            Replace dispatcher + dispatcher.store + task_center references
            with task_center + dispatch_queue
            Simplify _run_one / _dispatch to call TaskCenter only

        team/runtime/conductor.py
            Replace dispatcher reference with task_center
            Remove dispatcher_store reference

        team/runtime/team_run.py
            Remove Dispatcher instantiation
            Add DispatchQueue instantiation
            Wire TaskCenter with session_factory

        team/runtime/context_builder.py
            Remove dispatcher reference
            Build context via TaskCenter only

    NEW
        team/runtime/dispatch_queue.py
            DispatchQueue class (~50 lines)
            pop_ready + claim, extracted from DispatcherStore

    DELETED
        team/runtime/dispatcher.py
            Absorbed into TaskCenter

        team/runtime/dispatcher_store.py
            SQL methods absorbed into TaskCenter
            pop_ready + mark_running extracted to DispatchQueue

    NOT TOUCHED
        team/models.py                  (Task, TaskSpec, Plan, etc.)
        team/persistence/task_record.py (ORM model)
        team/persistence/schema.sql     (PostgreSQL schema)

    MODIFIED (tool type system + post-run)
        engine/core/query.py            (posthook logic removed)
        tools/core/base.py              (ToolType, tool_types on BaseTool)
        tools/posthook/toolkit.py       (tool_types={"post_run"})
        tools/context/toolkit.py        (PostNoteTool multi-type)

    NEW
        external_trigger/               (replaces ephemeral_task/)
        tools/external_trigger/         (PauseVerdictTool)

    DELETED
        ephemeral_task/                 (replaced by external_trigger/)

---

## 13. Implementation Phases

Two phases. Phase 1 has no dependencies. Phase 2 depends on Phase 1.

### Dependency Graph

    PHASE 1 (absorb + extract)
    +----------------------------------+     +---------------------------+
    | Phase 1A                         |     | Phase 1B                  |
    | Absorb DispatcherStore           |     | Extract DispatchQueue     |
    | into TaskCenter                  |     |                           |
    |                                  |     | pop_ready + claim         |
    | Move all SQL methods.            |     | into dispatch_queue.py    |
    | TaskCenter gains session_factory.|     |                           |
    | Existing tests pass with new     |     | ~50 lines.               |
    | method locations.                |     | Same SQL, same tests.    |
    |                                  |     |                           |
    | deps: none                       |     | deps: none                |
    +----------------------------------+     +---------------------------+

    PHASE 2 (rewire + delete)
    +------------------------------------------------------------------+
    | Phase 2                                                          |
    | Absorb Dispatcher into TaskCenter                                |
    | Rewire Executor, Conductor, context_builder                      |
    | Delete Dispatcher + DispatcherStore                              |
    |                                                                  |
    | deps: Phase 1A, Phase 1B                                        |
    +------------------------------------------------------------------+

### Phase 1A — Absorb DispatcherStore into TaskCenter

    Status: [x] DONE
    Deps: none
    Parallel with: Phase 1B

    Deliverables:
        [x] TaskCenter gains session_factory parameter
        [x] SQL methods delegated to TaskStore (extracted persistence layer)
        [x] TaskCenter delegates to self._store = TaskStore(...)
        [x] context_for: no task_lookup callback (uses internal get_task)
        [x] read_sibling_notes: uses internal _sibling_subtree_ids
        [x] Method names updated: pause_running_task, resume_paused_tasks,
            cancel_paused_tasks (renamed from block/unblock)

### Phase 1B — Extract DispatchQueue

    Status: [x] DONE
    Deps: none
    Parallel with: Phase 1A

    Deliverables:
        [x] team/runtime/dispatch_queue.py — new file
        [x] DispatchQueue class with:
            - pop_ready(run_id, blocker_guard) — same SQL as current
            (mark_running moved to TaskCenter instead of DispatchQueue)
        [x] blocker_guard parameter (callable, provided by Conductor)
        [x] Tests: pop_ready returns READY task, skips blocked,
            respects SKIP LOCKED, calls blocker_guard

### Phase 2 — Rewire and Delete

    Status: [x] DONE
    Deps: Phase 1A, Phase 1B

    Deliverables:
        [x] Absorb Dispatcher orchestration into TaskCenter:
            - complete_task (plan expansion, event emission)
            - retry_task, request_replan
            - Budget tracking (BudgetState, _charge_tasks)
            - Event emission (_emit)
        [x] Rewire Executor:
            - Uses task_center + dispatch_queue
            - _run_one → mark_running → run → _run_post_run → _dispatch
        [x] Rewire Conductor:
            - References _team_run (accesses task_center through it)
        [x] Rewire TeamRun:
            - DispatchQueue instantiated
            - No Dispatcher
        [x] Delete team/runtime/dispatcher.py
        [x] Delete team/runtime/dispatcher_store.py
        [x] All imports updated

### Parallelism Map

    Time ---->

    Week 1:     Phase 1A                Phase 1B
                Absorb Store            Extract Queue
                (SQL methods move)      (~50 lines new file)
                    |                       |
                    |                       |
    Week 2:     Phase 2
                Rewire + Delete
                (needs both 1A and 1B)

    Two developers can work in parallel on Phase 1.
    Phase 2 is integration work — one developer.
