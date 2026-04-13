# Dynamic Replanning: Blocker-Aware Pause/Fix/Resume Protocol

**Status:** PROPOSED  
**Date:** 2026-04-14  
**Branch:** `codex/pydantic-benchmark-loop`  
**Author:** Architecture session  
**Depends on:** Plan A Team Coordination Redesign (plan-a-team-coordination-redesign.md)  
**Prerequisite:** TaskCenter + DAG Unification (task-center-dag-unification.md)  
**Companion:** TaskCenter Active Mode (task-center-active-mode.md)

---

## Table of Contents

1. [Problem Statement](#1-problem-statement)
2. [Design Goals](#2-design-goals)
3. [Architecture Overview](#3-architecture-overview)
4. [Task Status — Extended State Machine](#4-task-status--extended-state-machine)
5. [Data Model](#5-data-model)
6. [Replanner Decision Tree](#6-replanner-decision-tree)
7. [Blocker Lifecycle](#7-blocker-lifecycle)
8. [EphemeralTask Module](#8-ephemeraltask-module)
9. [Conductor](#9-conductor)
10. [Toolkit Changes](#10-toolkit-changes)
11. [Dispatcher Changes](#11-dispatcher-changes)
12. [Resume Protocol](#12-resume-protocol)
13. [Task Center Integration](#13-task-center-integration)
14. [Scope and Boundaries](#14-scope-and-boundaries)
15. [Budget and Safety](#15-budget-and-safety)
16. [Walkthrough — The compatibility.py Scenario](#16-walkthrough--the-compatibilitypy-scenario)
17. [Files Changed](#17-files-changed)
18. [Implementation Phases](#18-implementation-phases)
19. [Tradeoffs and Scores](#19-tradeoffs-and-scores)

---

## 1. Problem Statement

When a completed task breaks a shared dependency mid-run, sibling tasks that depend on that shared code fail independently. The current system handles each failure in isolation — each task retries, each retry fails again, each exhaustion triggers a separate replan. There is no mechanism to detect "these failures share a root cause" and coordinate a single fix before resuming.

### The Scenario

A planner decomposes a dask bug-fix run into 32 HDF tasks and a compatibility.py refactor task. The planner does NOT declare dependencies between them (a planning gap). The refactor task completes first and breaks `dask/compatibility.py` by replacing `_EMSCRIPTEN` with a `__getattr__` mechanism that removes the `parse` import. All 32 HDF tasks then fail with the same `ImportError` because they all import `compatibility.py` transitively.

Without coordination: 32 independent failures, 32 retries, 32 re-failures, multiple replanners racing to fix the same file.

With this protocol: the replanner detects the systemic pattern, declares a blocker, pauses all siblings, fixes the root cause once, and resumes everyone.

---

## 2. Design Goals

| # | Goal | How |
|---|------|-----|
| G-1 | Developer stays simple | Developer has 2 tools: submit_summary and request_replan. No blocker awareness. |
| G-2 | Single decision point | Replanner owns all failure recovery decisions. Three clear actions, zero overlap. |
| G-3 | Conductor is deterministic | Conductor executes blocker mechanics. No LLM calls. Fully testable. |
| G-4 | query.py untouched | Blocker protocol operates outside the query loop. No injection, no halt directives, no buffer flushes. |
| G-5 | Sibling scoped | Blocker affects siblings and their children only. No cross-subtree coordination. Simple and predictable. |
| G-6 | Zero impact on unaffected agents | EphemeralTask-based assessment means agents that are not affected never see the blocker notification. |
| G-7 | Non-running tasks untouched | Only RUNNING agents are assessed and potentially paused. READY/PENDING/FAILED tasks keep their status — the pop_ready guard prevents dispatch during fix, but never mutates task state. |

---

## 3. Architecture Overview

### Role Responsibility Map

    Developer       "I failed"                      reports failure
         |                                          (request_replan)
         v
    Replanner       "Here's what to do"             assesses and decides
         |                                          (add_tasks / declare_blocker /
         |                                           cancel_and_redraft)
         v
    Conductor       "Executing blocker protocol"    mechanical execution
         |                                          (pause, assess, terminate,
         |                                           fix, resume)
         v
    Resolver        "Repairing root cause"          dedicated resolver role
         |                                          (scoped to broken files,
         |                                           submit_fix / abandon_fix)
         v
    Conductor       "Post-fix coordination"         resume assessed agents,
         |                                          lift pop_ready guard,
         |                                          spawn replanner for initiator
         v
    Resumed Agents  "Continuing from checkpoint"    resume from EphemeralTask state

### System-Level Flow

    Developer fails
          |
          | request_replan()
          v
    Dispatcher: mark task FAILED, spawn replanner
                (siblings UNTOUCHED — no auto-cancel)
          |
          v
    Replanner reads context: failure reason, sibling statuses,
    sibling notes, plan health, children statuses
          |
          v
    Replanner decides:
          |
          +---> add_tasks -----------> Dispatcher inserts new tasks
          |                            siblings untouched
          |
          +---> declare_blocker -----> Conductor executes:
          |                            assess RUNNING agents only
          |                            (non-running tasks untouched)
          |                            guard pop_ready during fix
          |                            spawn resolver (dedicated role)
          |                            on fix: resume assessed agents,
          |                              lift guard, spawn replanner
          |                              for initiator
          |                            on fix fail: mark team run FAILED
          |
          +---> cancel_and_redraft --> Dispatcher cancels all
                                       siblings + children,
                                       inserts new plan

---

## 4. Task Status — Extended State Machine

### Status Values

    PENDING     waiting for dependencies to complete
    READY       dependencies satisfied, eligible for dispatch
    RUNNING     actively being executed by an agent
    EXPANDED    planner submitted child tasks, waiting for them
    PAUSED      stopped by Conductor (RUNNING agents only),       <-- NEW
                waiting for blocker fix
    DONE        completed successfully
    FAILED      execution failed
    CANCELLED   cancelled by cascade or replan

### Terminal vs Non-Terminal

    Non-terminal:  PENDING, READY, RUNNING, EXPANDED, PAUSED
    Terminal:      DONE, FAILED, CANCELLED

PAUSED is non-terminal. This is the critical property: a parent with a PAUSED child stays EXPANDED. The maybe_promote_expanded_parent function will not promote the parent to DONE while any child is PAUSED. No ancestor chain reopening is needed in the common case.

Only RUNNING tasks can transition to PAUSED (via PauseAssessmentTask YES verdict). Non-running tasks (READY, PENDING, FAILED) are never touched by the blocker protocol — the pop_ready guard prevents dispatch during an active blocker, but their status remains unchanged.

### Transition Diagram

                   deps met
    PENDING -----------------------> READY
       |                               |
       |                               | pop_ready
       |                               v
       |                            RUNNING
       |                               |
       |                          +----+--------+--------+
       |                          |    |        |        |
       |                          v    v        v        v
       |                        DONE  FAILED  EXPANDED  (agent submits plan)
       |                                                    |
       |          Conductor                                 | children
       |          Authority                                 | complete
       |                                                    v
       |                                                  DONE
       |
       |                          blocker
       |                          created
       |                             |
       |                    PAUSED <--- RUNNING (via PauseAssessmentTask+terminate)
       |                       |
       |                       | blocker resolved
       |                       v
       |                    READY  (re-enters normal flow, new agent run
       |                            from pause_checkpoint)
       |
       |   Non-running tasks (READY, PENDING, FAILED):
       |     Status UNCHANGED during blocker.
       |     Pop_ready guard blocks dispatch while blocker active.
       |     Guard lifts on blocker resolution — normal dispatch resumes.

---

## 5. Data Model

### Blocker

    Blocker
        id                  str                 unique identifier
        team_run_id         str                 which run this blocker belongs to
        status              BlockerStatus       ASSESSING | FIXING | RESOLVED | FAILED
        reason              str                 human-readable description of the problem
        root_cause_paths    list of str         the broken files (fix target)
        blast_radius        list of str         broader scope of affected consumers
        fix_task_id         str or None         the task assigned to fix the root cause
        declared_by         str or None         the replanner task that declared this
        initiating_task_id  str                 the failed task that triggered the blocker
        fix_summary         str or None         filled when fix completes
        pending_assessments  int                 PauseAssessmentTasks still awaiting response
        created_at          float               timestamp
        resolved_at         float or None       timestamp

### BlockerStatus

    ASSESSING       spawning PauseAssessmentTasks for RUNNING agents, waiting for verdicts
    FIXING          all assessments resolved, resolver task is running
    RESOLVED        fix complete, assessed agents resumed, replanner spawned for initiator
    FAILED          resolver could not fix, team run marked FAILED

### TaskRecord Additions

    blocker_id          str or None         which blocker paused this task
    pause_checkpoint    blob or None        PauseAssessmentTask's display_messages for resume
    pause_verdict       str or None         PauseAssessmentTask's YES reason

    Only tasks paused from RUNNING have these fields populated.
    Non-running tasks (READY, PENDING, FAILED) are never touched by the
    blocker protocol — their status remains unchanged.

### Resume Behavior

    Only one resume path exists: PAUSED → READY (from formerly-RUNNING tasks).

    Resume: new agent run starting from pause_checkpoint
    Context: PauseAssessmentTask's full conversation + resume message appended
    The resumed agent sees everything the original did, plus why it was paused,
    plus what was fixed.

    Non-running tasks are not resumed — they were never paused.
    READY tasks dispatch normally once the pop_ready guard lifts.
    PENDING tasks continue waiting for deps.
    The initiating FAILED task is handled by a post-fix replanner spawn
    (see Conductor.on_fix_complete).

---

## 6. Replanner Decision Tree

### Context Available to the Replanner

The replanner is spawned after a developer calls request_replan. It has access to:

    - The failed task's error message and scope
    - All sibling tasks and their statuses (via sibling_stats)
    - Notes from completed and failed siblings (via TaskCenter)
    - Plan health signals (failure rate, retry counts)
    - Children task statuses for expanded siblings
    - The original plan structure

### Three Actions

    Replanner spawns after a task fails
          |
          v
    Read context: failure reason, sibling statuses,
    notes, plan health, children
          |
          v
    "What went wrong?"
          |
          +--- "Missing work. Plan had a gap."
          |     Some tasks are fine, we just need more.
          |     Or a task just needs another attempt with adjustments.
          |
          |     ---> add_tasks
          |          Add new tasks alongside existing siblings.
          |          Can include retried versions of failed tasks
          |          with adjusted descriptions, deps, or scope.
          |          Siblings continue running. No interruption.
          |
          +--- "Shared dependency broken. Not a plan error."
          |     A completed sibling broke something others depend on.
          |     Multiple siblings will hit the same error.
          |
          |     ---> declare_blocker
          |          Pause all siblings and their children.
          |          Conductor spawns a fix task.
          |          Everything resumes after the fix.
          |
          +--- "Plan was fundamentally wrong."
                Wrong decomposition, wrong ordering, wrong approach.
                Need to start over.

                ---> cancel_and_redraft
                     Cancel all siblings and their children.
                     Submit a completely new plan.

### add_tasks Absorbs request_retry

The old request_retry tool is removed. Retry logic is absorbed into the replanner's add_tasks action. When a task just needs another attempt, the replanner creates a new task with the same goal plus failure context. This is strictly more powerful than a blind retry because the replanner can:

    - Adjust the task description based on the failure
    - Add dependencies the original was missing
    - Change the assigned agent
    - Modify the scope
    - Include diagnostic context from the failure

Each attempt is a new task with a clean record. The original stays FAILED with its history preserved.

### Replanner Prompt Guidance

    "You are a replanner. A task has failed. Read the failure context,
     sibling statuses, and plan health, then call exactly ONE action:

     add_tasks — the plan is fine, just needs more work or a retry.
     declare_blocker — a shared dependency is broken, pause siblings.
     cancel_and_redraft — the plan was wrong, cancel and start over."

---

## 7. Blocker Lifecycle

### State Machine

                        replanner calls
                        declare_blocker()
                               |
                               v
                        +------------+
                        | ASSESSING  |
                        |            |
                        | spawn PauseAssessmentTasks for RUNNING agents
                        | non-running tasks UNTOUCHED (stay as-is)
                        | activate pop_ready guard against blast_radius
                        +-----+------+
                              |
                              | all assessments resolved (YES/NO/timeout)
                              | all terminated agents saved
                              v
                        +-----------+
                        |  FIXING   |
                        |           |
                        | resolver task dispatched at depth=0
                        | resolver (dedicated role) repairs root cause
                        +-----+-----+
                             / \
                       DONE /   \ FAILED (after 1 retry)
                           /     \
                    +----------+  +-----------+
                    | RESOLVED |  |  FAILED   |
                    |          |  |           |
                    | resume   |  | cancel    |
                    | assessed |  | all PAUSED|
                    | agents   |  | tasks     |
                    | lift     |  | mark team |
                    | guard    |  | run as    |
                    | spawn    |  | FAILED    |
                    | replanner|  |           |
                    | for      |  |           |
                    | initiator|  |           |
                    +----------+  +-----------+

### Pause Phase

    Blocker created by replanner
          |
          v
    +-----------------------------------------------------------+
    |                 ASSESSMENT PHASE                           |
    |                                                           |
    |  Non-running tasks (READY / PENDING / FAILED):            |
    |    UNTOUCHED. Status remains as-is.                       |
    |    Pop_ready guard prevents READY tasks from dispatching  |
    |    while the blocker is active.                           |
    |                                                           |
    |  Running tasks (ALL of them in blocker scope):            |
    |    Spawn PauseAssessmentTask per agent (parallel).        |
    |    Let the EphemeralTask decide YES or NO.                |
    |    YES: terminate original, save conversation, PAUSED.    |
    |    NO: discard, original continues unaware.               |
    |    TIMEOUT (30s): treat as YES.                           |
    |                                                           |
    |  No tiers. No auto-halt. No scope classification.         |
    |  One rule: running = EphemeralTask decides.               |
    |  Non-running = never touched.                             |
    |                                                           |
    +-----------------------------------------------------------+
          |
          | all assessments resolved
          v
    FIXING phase begins

### Pop-Ready Guard

While any blocker is active, the dispatcher's pop_ready must not dispatch tasks whose scope overlaps with the blocker's blast_radius. This prevents new tasks (whose deps just completed) from running into broken code during the fix phase.

    pop_ready()
          |
          v
    Select next READY task with pending_dep_count = 0
          |
          v
    Any active blocker's blast_radius overlaps this task's scope?
          |
         YES ---> skip candidate (leave READY), try next candidate
          |
         NO ----> dispatch normally (READY to RUNNING)

    The guard NEVER changes task status. It only skips candidates.
    When the blocker resolves and the guard lifts, skipped tasks
    become eligible for dispatch on the next pop_ready cycle.

### Dedup and Merge

When a second blocker declaration targets the same root cause (same paths or same fingerprint), the Conductor merges into the existing blocker rather than creating a new one:

    declare_blocker(paths, reason)
          |
          v
    Search active blockers:
      scope overlap match?
          |
     +----+----+
     |         |
    match    no match
     |         |
     v         v
    MERGE    CREATE
    expand   new blocker
    paths    assess_running
    (union)  spawn_resolver
    re-run
    pause
    for new
    scope

On merge, assess_running re-runs with expanded paths. Tasks already PAUSED are skipped. Only newly-in-scope RUNNING agents get assessed. Non-running tasks remain untouched — the expanded blast_radius automatically extends the pop_ready guard.

---

## 8. EphemeralTask Module

An EphemeralTask is a single-shot LLM call that reads an agent's conversation snapshot, answers one question, and terminates. It is not a full agent run — no tools, no loop, no state. It is the lightest possible unit of work in the system.

EphemeralTask is a shared module used by both the Conductor (for pause assessment) and the TaskCenter active mode (for progress reporting). It lives in its own file and provides the common mechanism that both consumers use.

### Core Principle

    The original agent NEVER sees the EphemeralTask.
    The EphemeralTask sees everything the original saw PLUS one question.
    The original is never interrupted (unless PauseAssessmentTask says YES).

### EphemeralTask Base

    EphemeralTask
        A single LLM call with a focused purpose.

        Fields
            task_id             str             the original task this is observing
            agent_run_id        str             the original agent run
            snapshot            list of messages display_messages at snapshot time
            prompt              str             the question to answer
            system_prompt       str             system prompt for the LLM call
            max_tokens          int             output cap (default 500)
            model               str or None     optional cheaper model override
            timeout_seconds     int             max wait time (default 30)

        Execution
            run() returns EphemeralTaskResult
                Builds messages = snapshot + one user message (prompt).
                Single LLM call. No tools. No loop.
                Returns the response text.
                On timeout: returns EphemeralTaskResult with timed_out=True.

    EphemeralTaskResult
        text                str             the LLM response
        timed_out           bool            whether the call timed out
        elapsed_seconds     float           how long the call took

### Two Concrete Types

    PauseAssessmentTask (extends EphemeralTask concept)
        Purpose: decide if a running agent is affected by a blocker
        Triggered by: Conductor
        Prompt: "Based on your work, does your task depend on {broken_files}?"
        Output: parsed into PauseVerdict (YES/NO/TIMEOUT + reason)
        Effect: YES → terminate original, save conversation as resume point
                NO → discard, original unaware
                TIMEOUT → treat as YES (conservative)

    CheckpointTask (extends EphemeralTask concept)
        Purpose: produce a progress note on behalf of a running agent
        Triggered by: TaskCenter active mode
        Prompt: "Summarize progress. Report blockers." (varies by trigger)
        Output: note text posted to TaskCenter
        Effect: note posted under original task's ID, original unaware

### PauseAssessmentTask — Blocker Impact Assessment

#### Fork Diagram

    Time --------------------------->

    Agent-A          | tool 1 | tool 2 | tool 3 | tool 4 | tool 5 |
    (original)       | read   | edit   | test   | read   | edit   |
                     |        |        |   ^    |        |   X    |
                     |        |        |   |    |        |   terminated
                     |        |        |   |    |        |   (asyncio.cancel)
                     |        |        | snapshot         |
                     |        |        |   |    |        |
    PauseAssessment  |        |        |   +-->+--------+|
    (1 LLM call)     |        |        |       | sees:  ||
                     |        |        |       | tool1-3||
                     |        |        |       | +      ||
                     |        |        |       |blocker ||
                     |        |        |       |question||
                     |        |        |       |answers:||
                     |        |        |       |"YES"   ||
                     |        |        |       +---+----+|
                     |        |        |           |
                     |     (fix happens here)      |
                     |        |        |           |
    Resumed agent    |        |        |           +-->+----------------+
    (new run)        |        |        |               | sees:         |
                     |        |        |               | tool1-3       |
                     |        |        |               | + blocker Q   |
                     |        |        |               | + "YES" answer|
                     |        |        |               | + resume msg  |
                     |        |        |               | continues...  |
                     |        |        |               +----------------+

The original did tool calls 4 and 5 after the snapshot. Those are lost. The resumed agent starts from the EphemeralTask's conversation (snapshot at tool 3 plus blocker question plus YES answer). This is correct — tool calls 4 and 5 happened against broken code and their results are unreliable.

#### Assessment Says NO

    Agent-B          | tool 1 | tool 2 | tool 3 | tool 4 | tool 5 | DONE
    (original)       | read   | edit   | test   | read   | edit   | submit
                     |        |        |   ^    |        |        |
                     |        |        | snapshot         |        |
                     |        |        |   |    |        |        |
    PauseAssessment  |        |        |   +-->+--------+|        |
                     |        |        |       |"NO: my ||        |
                     |        |        |       | task is||        |
                     |        |        |       | bag/   ||        |
                     |        |        |       | only"  ||        |
                     |        |        |       +--------+|        |
                     |        |        |         |       |        |
                     |        |        |      discard    |        |
                     |        |        |                  |        |
                     Agent-B finished normally.                    |
                     Never saw the EphemeralTask. Zero impact.    |

#### PauseAssessmentTask Input

The EphemeralTask's input is the original agent's full display_messages plus one new user message:

    Input:
        system_prompt: same as original agent
        messages:
            [all display_messages from original agent — every tool call,
             every tool result, everything the agent has seen and done]
            +
            one new user message:
                "BLOCKER CHECK
                 A shared dependency has been reported broken.
                 Broken files: dask/compatibility.py
                 Problem: __getattr__ replaced _EMSCRIPTEN, broke parse import
                 Reporter: replanner assessing sibling failures

                 Based on your work so far in this conversation,
                 does your task depend on any of these files?
                 Answer exactly one of:
                   YES: (reason)
                   NO: (reason)"

        max_tokens: 200
        tools: none

    Output (example):
        "YES: I imported dask.compatibility in tool call 1 to access the
         parse function. My HDF reader depends on it for version string parsing."

The EphemeralTask has full context — it saw every tool call the original agent made, every file read, every import. It knows exactly whether it touched the broken dependency.

#### PauseVerdict

    PauseVerdict
        task_id         str                 which task was assessed
        answer          YES | NO | TIMEOUT  the decision
        reason          str                 the reasoning
        conversation    list of messages    the full conversation (snapshot + Q + A)
                                            saved as resume checkpoint if YES

#### Timeout

    EphemeralTask spawned
          |
          +--- LLM responds within 30 seconds ---> normal YES/NO path
          |
          +--- LLM takes more than 30 seconds ---> timeout
                    |
                    v
               Treat as YES (conservative)
               Terminate original, mark PAUSED
               pause_verdict = "TIMEOUT: assumed affected"

#### Termination — External, No query.py Changes

The executor manages the agent run as an asyncio task. The Conductor terminates by cancelling the asyncio task. This is standard asyncio cancellation. The query loop catches CancelledError and cleans up. No modification to query.py required.

#### Safety Net

If a PauseAssessmentTask says NO but the original agent later fails with the same error fingerprint as the blocker, the Conductor's on_task_failed catches it and auto-pauses the task. The assessment was wrong, and the system self-corrects.

### CheckpointTask — Progress Reporter

#### Fork Diagram

    Agent-A          | tool 1 | tool 2 | tool 3 | tool 4 | tool 5 | tool 6 |
    (original)       | read   | edit   | edit   | edit   | edit   | edit   |
                     |        |        |        |        |   ^    |        |
                     |        |        |        |        | snapshot        |
                     |        |        |        |        |   |    |        |
    CheckpointTask   |        |        |        |        |   +-->+------+ |
    (1 LLM call)     |        |        |        |        |       |summa-| |
                     |        |        |        |        |       |rize  | |
                     |        |        |        |        |       |prog- | |
                     |        |        |        |        |       |ress  | |
                     |        |        |        |        |       +--+---+ |
                     |        |        |        |        |          |     |
                     |        |        |        |        |     note posted|
                     |        |        |        |        |     to TC      |
                     |        |        |        |        |                |
                     Agent-A continues working. Never saw the checkpoint.|
                     Other agents see the note in TaskCenter.            |

#### Two Trigger Variants

    EDIT_CHECKPOINT (triggered after 5 edits without post_note)
        Prompt:
            "Based on this agent's work so far, write a progress note.
             Focus on: what files were edited and why.
             Include file paths and specific changes.
             Keep under 300 words."

    TURN_CHECKPOINT (triggered after 10 turns without any posthook call)
        Prompt:
            "Based on this agent's work so far, write a progress note.
             Include:
             1. What the agent has accomplished
             2. Current status (working / stuck / nearly done)
             3. Whether the agent appears blocked by code another
                task broke (include file path and error if so)
             Keep under 300 words."

        The turn checkpoint explicitly asks about blockers. This feeds
        the replanner's decision when it calls read_sibling_notes.

#### Note Attribution

    The checkpoint note is posted with:
        task_id         original task's ID
        agent_name      original agent's name + " (checkpoint)"
        scope_paths     original task's scope_paths
        timestamp       current time

    To siblings and the replanner, it looks like the original agent
    posted a note. The "(checkpoint)" suffix distinguishes it from
    agent-authored notes for auditing purposes.

### Module Structure

    ephemeral_task/ (NEW)

    Contains:
        EphemeralTask           base dataclass + run() method
        EphemeralTaskResult         result dataclass
        PauseAssessmentTask     blocker assessment (used by Conductor)
        PauseVerdict            parsed YES/NO result
        CheckpointTask          progress reporting (used by TaskCenter active mode)

    Consumers:
        Conductor               imports PauseAssessmentTask, PauseVerdict
        TaskCenter active mode     imports CheckpointTask
        Executor                provides display_messages snapshots to both

    The module is standalone — no dependency on query.py, Conductor,
    or TaskCenter active mode. Those are consumers, not dependencies.
    The EphemeralTask only needs an API client to make the LLM call.

### EphemeralTask Uses — Complete Map

    Trigger              EphemeralTask Type       Output             Effect

    Blocker declared     PauseAssessmentTask      YES/NO verdict     terminate if YES
    (Conductor)                                                      none if NO

    5 edits              CheckpointTask           progress note      none on original
    (TaskCenter active mode)                         posted to TC

    10 turns             CheckpointTask           progress note      none on original
    (TaskCenter active mode)                         + blocker check
                                                  posted to TC

---

## 9. Conductor

### What the Conductor Is

The Conductor is a deterministic, non-LLM system actor within the TeamRun. It executes blocker mechanics: pause, assess, terminate, fix, resume. It makes no judgment calls. All judgment comes from the replanner (which declares the blocker) and PauseAssessmentTasks (which answer YES/NO).

### What the Conductor Is Not

The Conductor is not an LLM agent. It never calls a model. It never reasons about code, imports, or blast radius. This is critical for speed (sub-second blocker response), reliability (deterministic behavior), cost (no LLM calls per failure), and authority (system-level powers guarded by deterministic predicates).

The Conductor spawns resolver tasks (a dedicated role, not a normal developer) for fixing root causes, and spawns a post-fix replanner for the blocker-initiating task.

### Class Definition

    Conductor
        Constructor
            team_run            reference to the owning TeamRun
            dispatcher          reference to the Dispatcher
            task_center         reference to the TaskCenter
            blocker_store       persistence for Blocker records
            _executor_registry  dict mapping task_id to Executor
            _active_blockers    dict mapping blocker_id to Blocker

        Executor Registration
            register_executor(task_id, executor)
                Track which executor is running which task.
                Called by executor at task start.

            unregister_executor(task_id)
                Remove tracking when task completes.

        Detection
            on_task_failed(task, failure_reason)
                Called after every task failure.
                Checks if failure matches an active blocker's fingerprint.
                If match: add task to blocker's affected set (skip normal replan).
                If no match and 2+ correlated failures: trigger request_replan
                on one of them (replanner will assess).

        Blocker Lifecycle
            create_blocker(replanner_verdict)
                Called when replanner invokes declare_blocker.
                Creates Blocker record from replanner's assessment
                (including initiating_task_id from the failed task).
                Activates pop_ready guard, then calls assess_running.

            assess_running(blocker)
                Spawns one PauseAssessmentTask per RUNNING executor in
                blocker scope via asyncio.gather. Every running agent is
                assessed by an EphemeralTask — no tier classification,
                no auto-halt. Each assessment is a single LLM call.
                Non-running tasks (READY/PENDING/FAILED) are NEVER touched.

            _run_pause_assessment(executor, blocker) returns PauseVerdict
                Snapshots display_messages from executor.
                Single LLM call: no tools, max_tokens 200, timeout 30 seconds.
                Parses YES/NO/TIMEOUT from response.

            _on_pause_yes(executor, blocker, assessment_conversation, reason)
                Saves assessment_conversation as pause_checkpoint on the task.
                Cancels executor's asyncio task (external termination).
                Marks task PAUSED.
                Decrements pending_assessments.

            _on_pause_no(executor, blocker, reason)
                Logs dismissal and reason.
                Discards assessment result.
                Original agent continues unaware.
                Decrements pending_assessments.

            spawn_resolver(blocker)
                Creates a resolver task at depth=0 (top level, no parent).
                Resolver is a dedicated role (not a normal developer) with
                its own posthook tools: submit_fix and abandon_fix.
                Resolver is scoped to root_cause_paths.
                Resolver context includes: blocker reason, root cause paths,
                all failure reasons from assessed tasks, replanner's suggestion.

            on_fix_complete(blocker, fix_summary)
                Called when resolver calls submit_fix.
                Stores fix_summary on blocker.
                Calls resume_assessed(blocker, fix_summary).
                Lifts pop_ready guard (blocker no longer active).
                Spawns a replanner for blocker.initiating_task_id —
                the replanner reads the fix_summary and original failure,
                then calls add_tasks with an appropriate retry/adjusted task.
                Blocker status set to RESOLVED.

            on_fix_failed(blocker)
                If resolver has retries: retry.
                If retries exhausted:
                    Cancel all PAUSED tasks (assessed agents that were paused).
                    Mark team run as FAILED.
                    failure_reason = blocker.reason + "resolver could not fix"
                    Blocker status set to FAILED.

            resume_assessed(blocker, fix_summary)
                Transitions all PAUSED tasks with this blocker_id to READY.
                For each: new agent run from pause_checkpoint with resume
                message appended. Only formerly-RUNNING tasks are in this set.
                Non-running tasks were never paused — they resume naturally
                when the pop_ready guard lifts.

        Guards
            guard_pop_ready(task) returns bool
                Called by pop_ready before dispatching.
                Returns True if any active blocker's blast_radius
                overlaps the task's scope_paths.
                If True: task is skipped (stays READY, not dispatched).
                Task status is NEVER changed by this guard.

            match_fingerprint(task, failure_reason) returns bool
                Checks if a failure reason matches an active blocker's
                error fingerprint. Used by on_task_failed for the safety net.

        Intercept
            intercept_retry_replan(task_id) returns bool
                When a developer calls request_replan while an active blocker
                covers its scope: pauses the task instead.
                Returns "Blocker active, your task is being handled."

---

## 10. Toolkit Changes

### Developer / Reviewer Posthook — Before and After

    BEFORE (3 tools):
        submit_summary     "I'm done, here's what I did"
        request_retry      "I failed, same task again"
        request_replan     "I failed, need a different approach"

    AFTER (2 tools):
        submit_summary     "I'm done, here's what I did"       (unchanged)
        request_replan     "I failed"                           (unchanged interface)

    REMOVED:
        request_retry      absorbed into replanner's add_tasks

The developer no longer distinguishes between "retry" and "replan." It just reports failure. The replanner decides what to do.

### Planner Posthook — Unchanged

    submit_plan            "Here's the task decomposition"

### Replanner Posthook — Before and After

    BEFORE (1 tool, overloaded):
        submit_replan      add_tasks + cancel_ids bundled together
                           called after siblings already auto-cancelled

    AFTER (3 tools, clear intent):
        add_tasks           add new tasks alongside existing siblings
                            can include retried versions of failed tasks
                            siblings continue running, no interruption

        declare_blocker     pause siblings + their children
                            triggers Conductor to spawn fix task
                            everything resumes after fix

        cancel_and_redraft  cancel all siblings + their children
                            submit a completely new plan

    REMOVED:
        submit_replan       replaced by the three tools above

### declare_blocker Tool Definition

    declare_blocker
        Parameters:
            root_cause_paths    list of str     the broken files (fix target)
            blast_radius        list of str     broader scope of consumers
            reason              str             why this is systemic
            suggestion          str or None     how to fix (optional)

        Available to: replanner role only

        Returns: confirmation that blocker was created

        Side effect: Conductor creates Blocker and begins pause protocol

### add_tasks Tool Definition

    add_tasks
        Parameters:
            tasks               list of TaskSpec     new tasks to insert

        Available to: replanner role only

        Returns: confirmation with count of inserted tasks

        Side effect: Dispatcher inserts tasks as siblings of the failed task.
        Existing siblings are untouched.

### cancel_and_redraft Tool Definition

    cancel_and_redraft
        Parameters:
            tasks               list of TaskSpec     the new plan

        Available to: replanner role only

        Returns: confirmation with cancel count and insert count

        Side effect: Dispatcher cancels all pending/ready/expanded siblings
        and their dependents, then inserts the new tasks.

### Resolver Posthook — New Role

    RESOLVER ROLE (NEW):
        submit_fix         "Root cause repaired, here's what I did"
        abandon_fix        "Cannot fix, here's why"

    The resolver is a dedicated role, NOT a normal developer.
    It does NOT have submit_summary, request_replan, or any developer tools.
    Its only job is to fix the root cause files and signal completion.

    submit_fix signals success to Conductor.on_fix_complete.
    abandon_fix signals failure to Conductor.on_fix_failed.

### submit_fix Tool Definition

    submit_fix
        Parameters:
            fix_summary         str             what was fixed and how

        Available to: resolver role only

        Returns: confirmation

        Side effect: Conductor receives on_fix_complete, resumes assessed
        agents, lifts pop_ready guard, spawns replanner for initiator.

### abandon_fix Tool Definition

    abandon_fix
        Parameters:
            reason              str             why the fix could not be applied

        Available to: resolver role only

        Returns: confirmation

        Side effect: Conductor receives on_fix_failed. If retries remain,
        retry. If exhausted, cancel PAUSED tasks and mark team run FAILED.

### Infrastructure Retry — Preserved at Executor Level

Agent-level request_retry is removed, but infrastructure failures (OOM, timeout, network errors) still auto-retry at the executor level. These are transient failures that do not need replanning:

    Executor catches exception
          |
          +--- Infrastructure failure? (worker_exception, runner_exception)
          |       |
          |       YES --> auto-retry at executor level
          |               uses existing retry_count / max_retries
          |               no replanner spawned
          |
          +--- Agent failure? (agent called request_replan or submitted error)
                  |
                  --> spawn replanner
                      replanner decides: add_tasks / blocker / nuke

---

## 11. Dispatcher Changes

### request_replan — Remove Auto-Cancel

The current request_replan in dispatcher_store.py auto-cancels all pending/ready/expanded siblings and cascade-cancels their dependents BEFORE the replanner runs. This is too aggressive — it destroys work before anyone assesses whether that work should be destroyed.

    CURRENT request_replan flow:
        1. Mark failing task FAILED
        2. Cancel pending/ready/expanded siblings        <-- destructive
        3. Cascade cancel dependents of cancelled        <-- destructive
        4. Collect done siblings as deps for replanner
        5. Insert replanner task

    PROPOSED request_replan flow:
        1. Mark failing task FAILED
        2. Insert replanner task (siblings UNTOUCHED)    <-- replanner decides

The replanner now sees live siblings. It can assess their state, read their notes, and decide: add alongside them, pause them, or cancel them.

### New DispatcherStore Methods

    pause_running_task(task_id, blocker_id, pause_checkpoint, pause_verdict)
        UPDATE tasks SET status = 'paused', blocker_id = blocker_id,
            pause_checkpoint = checkpoint, pause_verdict = verdict
        WHERE id = task_id AND status = 'running'
        Used only for RUNNING tasks after PauseAssessmentTask says YES.
        Non-running tasks are never paused.

    resume_paused_tasks(run_id, blocker_id) returns int
        UPDATE tasks SET status = 'ready', blocker_id = NULL
        WHERE team_run_id = run_id AND blocker_id = blocker_id
            AND status = 'paused'
        Returns count of resumed tasks. All resumed tasks were
        formerly RUNNING — they re-enter as READY with pause_checkpoint
        for conversation restoration.

    cancel_paused_tasks(run_id, blocker_id) returns int
        UPDATE tasks SET status = 'cancelled', blocker_id = NULL
        WHERE team_run_id = run_id AND blocker_id = blocker_id
            AND status = 'paused'
        Used when resolver fails and team run is marked FAILED.

### pop_ready Modification

    pop_ready gains a guard that checks active blockers before dispatching:

    SELECT t.id FROM tasks t
    WHERE t.team_run_id = run_id
      AND t.status = 'ready'
      AND t.pending_dep_count = 0
      AND NOT EXISTS (
          SELECT 1 FROM blockers b
          WHERE b.team_run_id = run_id
            AND b.status IN ('active', 'fixing')
            AND t.scope_paths overlaps b.blast_radius
      )
    ORDER BY t.depth, t.created_at
    LIMIT 1
    FOR UPDATE SKIP LOCKED

Tasks that overlap with an active blocker's blast_radius are skipped (not dispatched, not status-changed). This prevents new tasks from running into broken code during the fix phase. When the blocker resolves and the guard lifts, these tasks become eligible for dispatch on the next pop_ready cycle.

---

## 12. Resume Protocol

### Resume Message

When a formerly-paused task dispatches, the executor injects a resume message as Priority 0 context (never trimmed). This is handled in task_center.context_for, not in query.py.

    Resume message structure:

        RESUME — Blocker Resolved

        Your task was paused because a shared dependency was broken.

        Blocker: (reason from Blocker record)
        Broken files: (root_cause_paths)
        Reported by: (replanner that declared the blocker)
        Fix applied: (fix_summary from resolver)

        Why you were paused (your own assessment):
        (pause_verdict — the PauseAssessmentTask's YES reason)

        Your progress before pause:
        (pause_checkpoint summary)

        Continue from where you left off.
        Re-read any files from the affected scope that you previously read.

### Resume — From PauseAssessmentTask Checkpoint

Only formerly-RUNNING tasks are paused and resumed. They resume from the PauseAssessmentTask's conversation, not from scratch:

    1. Load pause_checkpoint (the PauseAssessmentTask's display_messages)
    2. Append the resume message as a new user message
    3. Start a new agent run with these messages as the conversation history
    4. The query loop runs normally from there

The resumed agent sees:
    - Everything the original did before the snapshot (all tool calls)
    - The blocker question (injected into the PauseAssessmentTask)
    - Its own YES answer (the assessment's reasoning about why it was affected)
    - The resume message (what was fixed, continue working)

The agent naturally continues from the snapshot point.

### Non-Running Tasks — Not Paused, Not Resumed

Non-running tasks (READY, PENDING, FAILED) are never paused by the blocker protocol. Their status remains unchanged throughout the blocker lifecycle.

    READY tasks: blocked from dispatch by pop_ready guard while blocker
    is active. Once the blocker resolves and the guard lifts, they dispatch
    normally on the next pop_ready cycle. No resume message needed — these
    tasks start fresh and the fix is already applied to the codebase.

    PENDING tasks: continue waiting for dependencies. Unaffected.

    The initiating FAILED task: handled by a post-fix replanner spawn.
    The Conductor spawns a replanner scoped to the initiating task after
    the resolver completes. The replanner reads the fix_summary and the
    original failure, then calls add_tasks with a retry or adjusted task.

---

## 13. Task Center Integration

The Task Center is the shared context backbone of the coordination system. Two additions strengthen it for the replanner and improve note discipline across all agents.

### 13.1 read_sibling_notes — Replanner's Source of Truth

The existing read_notes tool filters by author task IDs or scope_paths. The replanner needs neither — it needs "everything my siblings and their children have posted." Today it would have to manually enumerate sibling IDs, query each one's children, and pass them all as authors. That is fragile and slow.

A new tool, read_sibling_notes, resolves the sibling subtree automatically from the replanner's own parent_id and returns all notes in one call.

#### How It Works

    Replanner is inserted at parent_id = P (same parent as the failed task)

    read_sibling_notes()
          |
          v
    Resolve from replanner's metadata:
        parent_id = P
          |
          v
    Query all tasks where parent_id = P (siblings)
    + all tasks whose root traces to any sibling (children, recursively)
          |
          v
    Collect task IDs: {sibling_1, sibling_2, ..., child_1a, child_1b, ...}
          |
          v
    TaskCenter.read(authors = collected IDs)
          |
          v
    Return notes grouped by task, most recent first

    Each note includes:
        agent_name      who posted it
        task_id         which task posted it
        content         the note body
        scope_paths     which files it relates to
        timestamp       when it was posted

#### What the Replanner Sees

    read_sibling_notes() returns:

    --- task hdf-01 (developer) [scope: dask/dataframe/io/hdf.py] ---
    "Edited _read_hdf to use new parse API. Tests 1-15 passing.
     Hit ImportError on dask.compatibility.parse — file was changed
     by fix-compat task. This is a shared dependency issue."

    --- task hdf-02 (developer) [scope: dask/dataframe/io/hdf.py] ---
    "ImportError: cannot import name 'parse' from dask.compatibility.
     Same error as hdf-01. Root cause is in compatibility.py, not my scope."

    --- task fix-compat (developer) [scope: dask/compatibility.py] ---
    "Replaced _EMSCRIPTEN with __getattr__ mechanism. Removed direct
     parse import in favor of lazy attribute lookup."

    --- task array-01 (developer) [scope: dask/array/overlap.py] ---
    "Overlap computation rewritten. 3 of 5 tests passing.
     Remaining 2 need rechunk fix."

The replanner reads this and immediately sees: fix-compat broke a shared dependency, hdf-01 and hdf-02 both report the same ImportError, and array-01 is working on something unrelated. This is the evidence it needs to call declare_blocker versus add_tasks.

#### Tool Definition

    read_sibling_notes
        Parameters:
            keyword         str or None     optional keyword filter on note content
            scope_paths     list of str or None     optional additional scope filter
            include_children    bool, default True      include notes from children
                                                        of siblings, not just siblings

        Available to: replanner role

        Returns: all notes from sibling tasks and their descendants,
                 grouped by task, most recent first

        Implementation: resolves parent_id from replanner's metadata,
        queries dispatcher for sibling + descendant task IDs,
        passes them as authors to TaskCenter.read

#### Why Not Just read_notes

read_notes requires the caller to know which task IDs or scope paths to filter on. The replanner does not know sibling IDs upfront — they are in the dispatcher, not in the replanner's context. read_sibling_notes bridges the gap by doing the lookup automatically.

This also keeps read_notes simple and general-purpose. The sibling resolution logic lives in the new tool, not buried inside read_notes as a special case.

#### Data Flow

    Replanner calls read_sibling_notes()
          |
          v
    Tool resolves replanner's parent_id from context.metadata
          |
          v
    Tool queries DispatcherStore.get_subtree_task_ids(run_id, parent_id)
          |
          |   returns: {sibling_1, sibling_2, child_1a, child_2a, ...}
          v
    Tool calls TaskCenter.read(authors = subtree_ids)
          |
          v
    Tool applies optional keyword / scope_paths filter
          |
          v
    Returns formatted notes to replanner

    DispatcherStore.get_subtree_task_ids is a new query method:

        WITH RECURSIVE subtree AS (
            SELECT id FROM tasks
            WHERE team_run_id = run_id AND parent_id = parent_id
          UNION ALL
            SELECT t.id FROM tasks t
            JOIN subtree s ON t.parent_id = s.id
            WHERE t.team_run_id = run_id
        )
        SELECT id FROM subtree

### 13.2 TaskCenter Active Mode

See separate document: [task-center-active-mode.md](task-center-active-mode.md)

The TaskCenter gains an active mode where it tracks agent activity (edits, turns, posthook calls) and spawns EphemeralTasks to auto-generate progress notes when agents are silent too long. This is an independent feature that complements the blocker protocol by ensuring the replanner has rich context from all siblings when it calls read_sibling_notes.

Key relationship to the blocker protocol: the turn-trigger EphemeralTask prompt explicitly asks about blockers. Auto-generated notes surface blocker evidence early, giving the replanner higher-confidence signals for declare_blocker decisions.

### 13.3 Conversation Snapshot Mechanism

The EphemeralTask (both PauseAssessmentTask and auto-note generation) needs a read-only snapshot of the running agent's conversation. The current Executor delegates to a QueryRunner callable and does not retain conversation state. This requires a lightweight extension.

#### Design

The query loop (run_query_loop) maintains display_messages internally. To expose a snapshot without modifying query.py internals, the loop accepts an optional callback:

    run_query_loop signature gains one optional parameter:

        on_turn: Callable[[list[ConversationMessage]], None] or None

    At the top of each turn (alongside ScopeChangeBuffer flush),
    the loop calls on_turn(display_messages) if provided.

    The executor provides this callback:

        def _on_turn(self, messages):
            self._latest_messages = messages

    When the Conductor or TaskCenter needs a snapshot:

        snapshot = list(executor._latest_messages)

    This is a shallow copy of the append-only list. Safe because
    display_messages is never mutated in place — only appended to.

#### Why This Is Minimal

    query.py changes: one optional parameter + one callback invocation
    No message injection. No display_messages mutation.
    The callback is fire-and-forget — no return value, no blocking.
    If on_turn is None (non-team mode), the line is skipped entirely.

#### Diagram

    query loop (inside query.py):
          |
          | top of each turn
          v
    if on_turn is not None:
        on_turn(display_messages)     # one line, fire-and-forget
          |
          v
    (rest of the turn proceeds normally)


    executor (outside query.py):
          |
          | _on_turn callback stores reference
          v
    self._latest_messages = messages
          |
          v
    Conductor or TaskCenter reads snapshot when needed:
        snapshot = list(self._latest_messages)

### 13.4 Pop-Ready Blocker Guard — Python-Side Filtering

The proposed SQL using PostgreSQL array overlap (&&) cannot perform path-prefix matching. blast_radius=["dask/"] will not match scope_paths=["dask/dataframe/io/hdf.py"] via array overlap because they are not exact element matches. The guard must use Python-side filtering with the existing scope_paths_overlap function.

#### Design

The Conductor maintains an in-memory set of active blockers. The pop_ready guard is a Python-side check AFTER the SQL query returns a candidate, not a SQL-level filter.

    pop_ready flow (revised):

    1. SQL: SELECT next READY task with pending_dep_count = 0
       (existing query, unchanged)

    2. Python: Conductor.guard_pop_ready(candidate) -> bool
       Checks candidate.scope_paths against all active blockers'
       blast_radius using scope_paths_overlap (prefix matching).

    3. If blocked:
       Skip the candidate (stays READY, not dispatched).
       pop_ready retries with next candidate.

    4. If not blocked:
       Return candidate for dispatch.

    Conductor.guard_pop_ready(task):
        for blocker in self._active_blockers.values():
            for task_path in task.scope_paths:
                for blast_path in blocker.blast_radius:
                    if scope_paths_overlap(task_path, blast_path):
                        return True  # blocked
        return False  # clear

#### Why Python-Side, Not SQL

    scope_paths_overlap does prefix matching:
        "dask/" overlaps "dask/dataframe/io/hdf.py"     (prefix)
        "dask/dataframe" overlaps "dask/dataframe/io/"   (prefix)
        "pandas/" does NOT overlap "dask/compatibility.py"

    PostgreSQL array overlap (&&) checks exact element matches:
        ARRAY['dask/'] && ARRAY['dask/dataframe/io/hdf.py']  -> FALSE

    The existing scope_paths_overlap function in team/_path_utils.py
    already handles all the edge cases (trailing slashes, prefix
    containment, bidirectional checks). Reusing it in Python is
    simpler and more correct than reimplementing in SQL.

#### Performance

    The guard runs once per pop_ready call. pop_ready is called
    when an executor needs work — typically a few times per second
    at most. The guard iterates over active blockers (max 3) and
    scope_paths (typically 1-3 per task). This is O(blockers * paths)
    per pop_ready call — negligible.

### 13.5 Blocker Persistence

The Blocker is stored in-memory on the Conductor, NOT in PostgreSQL. This is a deliberate choice:

    In-memory:
        Fast (no SQL round-trip for guard_pop_ready)
        Simple (no migration, no table, no ORM)
        Blocker lifetime = TeamRun lifetime (appropriate scope)

    Risk: if the process crashes mid-blocker, PAUSED tasks stay PAUSED.
    Mitigation: on TeamRun restart, scan for tasks with status=PAUSED
    and blocker_id set. If no active blocker matches, cancel them
    and mark team run as FAILED. This is a recovery path, not a normal path.

### 13.6 Concurrent Blockers on the Same Task

A task has a single blocker_id field. If two blockers want to pause the same task, use a list:

    TaskRecord.blocker_ids    list of str (ARRAY in PostgreSQL)

    A task is unblocked only when ALL its blocker_ids are resolved.
    resume_paused_tasks removes one blocker_id from the list. When the
    list is empty, the task transitions back to its resume status.

    This handles the edge case where blast_radius of two independent
    blockers overlaps on the same task.

### 13.7 Correlation Engine

The Conductor monitors task failures for patterns. This is a lightweight mechanism, not a full feature:

    on_task_failed(task, failure_reason):
        fingerprint = normalize_fingerprint(failure_reason)
        self._recent_failures.append((task.id, fingerprint, time.time()))

        # Prune old entries (older than 5 minutes)
        cutoff = time.time() - 300
        self._recent_failures = [f for f in self._recent_failures if f[2] > cutoff]

        # Check for cluster
        matching = [f for f in self._recent_failures if f[1] == fingerprint]
        if len(matching) >= 2:
            # Trigger request_replan on the latest failure
            # The replanner will see the pattern and may declare_blocker
            await self.dispatcher.request_replan(task.id, ...)

    normalize_fingerprint(reason):
        Strip UUIDs, line numbers, file paths, timestamps.
        Return SHA-256 prefix of the normalized string.

    This is Phase D (Conductor) implementation, not a separate phase.

### 13.8 Database Migration

The following schema changes require a migration:

    ALTER TABLE tasks ADD COLUMN blocker_ids TEXT[] DEFAULT '{}';
    ALTER TABLE tasks ADD COLUMN pause_checkpoint BYTEA;
    ALTER TABLE tasks ADD COLUMN pause_verdict TEXT;

    No paused_from column — only RUNNING tasks can be paused.
    No new tables. Blocker is in-memory (Section 13.5).
    Migration is part of Phase B (PAUSED Status + Blocker Model).

### 13.3 DispatcherStore Addition

    get_subtree_task_ids(run_id, parent_id) returns set of str
        Recursive CTE that returns all task IDs under a given parent,
        including the parent's direct children and all their descendants.
        Used by read_sibling_notes to resolve the sibling subtree.

---

## 14. Scope and Boundaries

### Blocker Scope — Siblings and Their Children Only

A blocker declared by a replanner affects only the siblings of the failed task and their children. It does not affect tasks in other subtrees.

    Parent (EXPANDED)
    +-- task-A (DONE — broke shared dep)
    +-- task-B (FAILED — hit broken dep, triggered replan)
    +-- task-C (READY)                    <-- sibling, in scope
    +-- task-D (RUNNING)                  <-- sibling, in scope
    |   +-- task-D1 (READY)              <-- child of sibling, in scope
    |   +-- task-D2 (RUNNING)            <-- child of sibling, in scope
    +-- task-E (EXPANDED)                 <-- sibling, in scope
    |   +-- task-E1 (DONE)
    |   +-- task-E2 (PENDING)            <-- child of sibling, in scope
    |
    +-- [replanner task inserted here, same parent]

    Everything outside this parent is NOT in scope.

### Cross-Subtree Blockers

If the same broken dependency affects tasks in a different subtree (different parent), those tasks fail independently and trigger their own request_replan. Their replanner makes its own assessment and may also declare_blocker. This results in two independent blockers, each scoped to their own subtree.

This is slightly redundant (two fix tasks for the same file) but correct and dramatically simpler than cross-subtree coordination. The second fix task sees the file already repaired and completes immediately.

### Blast Radius vs Root Cause Paths

    root_cause_paths    The specific broken files. Used by the fix task to know
                        what to repair. Included in PauseAssessmentTask prompts
                        so the EphemeralTask can identify direct impact.

    blast_radius        The broader scope of files that might be affected.
                        Declared by the replanner based on its assessment of
                        the codebase structure. Used by the pause phase
                        and the pop_ready guard.

    Example:
        root_cause_paths = ["dask/compatibility.py"]
        blast_radius = ["dask/"]

        The fix task repairs dask/compatibility.py.
        All tasks with scope under dask/ are paused.
        Tasks under pandas/ or numpy/ are unaffected.

---

## 15. Budget and Safety

### Budget Guards

    max_active_blockers         3           prevent blocker storms
    assessment_timeout          30 sec      single LLM call should be fast
    assessment_max_tokens       200         just need YES/NO + reason
    fix_max_retries             1           if fix fails twice, abandon
    assessment_gather_timeout   5 min       don't wait forever for all assessments
    max_paused_ratio            60%         don't pause the entire run

### Blocker Failure Policy

    Resolver task completes
          |
          +--- submit_fix ----> blocker RESOLVED
          |                     resume assessed agents with fix context
          |                     lift pop_ready guard
          |                     spawn replanner for initiating task
          |
          +--- abandon_fix --> retry resolver (max 1 retry)
                            |
                            +--- submit_fix ----> blocker RESOLVED (as above)
                            |
                            +--- abandon_fix --> blocker FAILED
                                                 cancel all PAUSED tasks
                                                 mark team run as FAILED
                                                 failure_reason includes
                                                 blocker.reason

### Correlation Engine

The Conductor monitors task failures for patterns. When 2 or more tasks fail within 5 minutes with the same normalized error fingerprint, the Conductor triggers request_replan on one of them. The replanner then has the evidence to decide whether to declare_blocker.

Fingerprint normalization strips variable parts (UUIDs, line numbers, file paths, timestamps) to expose the structural error pattern. Two failures with the same normalized fingerprint are likely the same root cause.

---

## 16. Walkthrough — The compatibility.py Scenario

### Initial State

    Root Planner (EXPANDED)
    +-- plan-A (EXPANDED)
        +-- fix-compat       scope=[dask/compatibility.py]     DONE
        +-- hdf-01           scope=[dask/dataframe/io/hdf.py]  RUNNING
        +-- hdf-02           scope=[dask/dataframe/io/hdf.py]  FAILED
        +-- hdf-03           scope=[dask/dataframe/io/hdf.py]  READY
        +-- hdf-04 .. hdf-32 scope=[dask/dataframe/io/...]     READY

fix-compat completed and broke dask/compatibility.py. hdf-02 failed with ImportError. hdf-01 is still running. hdf-03 through hdf-32 are waiting.

### Step 1 — Developer Reports Failure

hdf-02 calls request_replan("ImportError: cannot import parse from dask.compatibility").

Dispatcher marks hdf-02 FAILED. Inserts replanner task under plan-A. Siblings are NOT cancelled (new behavior).

### Step 2 — Replanner Assesses

Replanner spawns and reads context:
- hdf-02 failed: ImportError on dask.compatibility
- fix-compat DONE: recently modified dask/compatibility.py
- hdf-01 RUNNING: same scope area
- hdf-03 to hdf-32 READY: all in dask/dataframe scope
- Plan health: 1 failure out of 2 started

Replanner judges: fix-compat broke a shared dependency. This is systemic. All dask tasks will hit the same error.

Replanner calls declare_blocker with root_cause_paths=["dask/compatibility.py"], blast_radius=["dask/"], reason="fix-compat introduced broken __getattr__ that removes parse import", suggestion="revert __getattr__, restore direct imports".

### Step 3 — Conductor Assesses

Conductor creates Blocker with initiating_task_id=hdf-02. Begins assess_running:

    Non-running tasks (UNTOUCHED):
        hdf-03 to hdf-32: stay READY. Pop_ready guard blocks dispatch.
        hdf-02: stays FAILED. Will be handled by post-fix replanner.

    PauseAssessmentTask (running agents):
        hdf-01 is RUNNING with scope dask/dataframe/io/hdf.py.
        Conductor spawns PauseAssessmentTask for hdf-01.

        Assessment sees hdf-01's full conversation.
        Assessment sees: "BLOCKER CHECK: dask/compatibility.py is broken."
        Assessment answers: "YES: I imported dask.compatibility in my
        first tool call to read the parse function."

        Conductor terminates hdf-01's executor.
        Saves assessment conversation as pause_checkpoint.
        hdf-01: RUNNING to PAUSED.

### Step 4 — State After Assessment

    Root Planner (EXPANDED)
    +-- plan-A (EXPANDED)
        +-- fix-compat       DONE
        +-- hdf-01           PAUSED (has pause_checkpoint)
        +-- hdf-02           FAILED (untouched, initiating task)
        +-- hdf-03 .. hdf-32 READY  (untouched, blocked by pop_ready guard)
        +-- replanner        DONE
    
    resolver-node (READY, depth=0, parent=None)   <-- spawned by Conductor

plan-A stays EXPANDED because hdf-01 is PAUSED (non-terminal).

### Step 5 — Fix

resolver-node dispatches. A resolver agent scoped to dask/compatibility.py reads the file, reverts the __getattr__ mechanism, restores the direct parse import. Calls submit_fix. DONE.

Conductor receives on_fix_complete. fix_summary = "restored direct parse import in compatibility.py".

### Step 6 — Resume + Post-Fix Replanner

Conductor executes on_fix_complete:

    1. Resume assessed agents:
        hdf-01 (PAUSED):
            New agent run started from pause_checkpoint.
            Assessment conversation included: tool calls 1-3, blocker question,
            "YES: I imported dask.compatibility..." answer.
            Resume message appended: "Fix applied: restored direct parse import.
            Continue from where you left off."

    2. Lift pop_ready guard:
        hdf-03 to hdf-32 (READY, were blocked by guard):
            Now eligible for dispatch. Pop_ready picks them up normally.
            No resume message needed — they start fresh, fix already in codebase.

    3. Spawn replanner for initiating task (hdf-02):
        Replanner reads: hdf-02's failure reason, fix_summary, current state.
        Replanner calls add_tasks with a retry task for hdf-02's goal,
        including fix context in the new task description.

### Step 7 — Completion

hdf-01 resumes and completes. hdf-03 to hdf-32 dispatch and complete. The replanner's retry task for hdf-02 dispatches and completes. plan-A's children all reach DONE. plan-A promotes to DONE via maybe_promote_expanded_parent.

### Cost Comparison

    Without blocker protocol:
        32 failures + 32 retries + 32 re-failures + multiple replanners
        Total: 64+ task executions

    With blocker protocol:
        1 failure (hdf-02) + 1 replan + 1 resolver + 1 post-fix replanner
        + 1 resume (hdf-01) + 31 normal dispatches (hdf-03..32) + 1 retry (hdf-02)
        Total: 37 task executions
        Saved: 27+ wasted executions

---

## 17. Files Changed

    MODIFIED
        team/models.py
            + PAUSED in TaskStatus
            + Blocker dataclass
            + PauseVerdict dataclass
            + BlockerStatus enum

        team/runtime/dispatcher_store.py
            request_replan: remove auto-cancel of siblings
            + pause_running_task (RUNNING → PAUSED with checkpoint)
            + resume_paused_tasks (PAUSED → READY for assessed agents)
            + cancel_paused_tasks (PAUSED → CANCELLED on resolver failure)
            + get_subtree_task_ids (recursive CTE for sibling note resolution)
            pop_ready: add blocker blast_radius guard (skip, not pause)

        team/runtime/executor.py
            + on_turn callback for conversation snapshot (see Section 13.3)
            handle asyncio.CancelledError from external termination
            support resume from pause_checkpoint (new agent run from saved messages)
            call TaskCenter.on_edit / on_posthook / tick / check (outside query loop)
            register/unregister with Conductor

        team/task_center.py
            + Active mode: on_edit, on_posthook, tick, on_note_posted
              (see task-center-active-mode.md for full spec)
            + check (spawn EphemeralTask when thresholds crossed)
            + read_sibling_notes (subtree note query for replanner)
            + ActivityCounters per-task internal state
            context_for: add resume message injection at Priority 0

        tools/posthook/toolkit.py
            Remove: RequestRetryTool, SubmitReplanTool
            Add: AddTasksTool, DeclareBlockerTool, CancelAndRedraftTool
            Add: SubmitFixTool, AbandonFixTool (resolver role)
            PosthookTools.from_context: update role-to-tools mapping
            Add resolver role → {SubmitFixTool, AbandonFixTool}

        tools/context/toolkit.py
            PostNoteTool.execute: call task_center.on_note_posted

        team/runtime/team_run.py
            Wire Conductor into lifecycle

    NEW
        ephemeral_task/
            EphemeralTask base (single LLM call mechanism)
            EphemeralTaskResult dataclass
            PauseAssessmentTask (used by Conductor for blocker impact assessment)
            PauseVerdict (parsed YES/NO result)

        team/runtime/conductor.py
            Conductor class (all methods defined in Section 9)

        docs/architecture/task-center-active-mode.md
            Full spec for TaskCenter active mode (split from this doc)

    MINIMAL TOUCH
        engine/core/query.py
            + one optional on_turn callback parameter to run_query_loop
            + one callback invocation at top of each turn
            Remove inline edit-based note nudge logic (lines 536-559)
            No message injection. No display_messages mutation.

        tools/daytona_toolkit/tools.py
            _track_edit_for_note_nudge: redirect to task_center.on_edit

        tools/core/runtime.py
            Remove edits_since_last_note, files_edited_since_last_note,
            _note_nudge_at_edit from MERGED_RUNTIME_METADATA_KEYS

    NOT TOUCHED
        engine/core/notifications.py

    REMOVED (from developer toolkit)
        RequestRetryTool                    absorbed into replanner's add_tasks

---

## 18. Implementation Phases

Six phases. Phases within the same tier have no dependencies on each other and can be implemented in parallel. Phases in a later tier depend on at least one phase from an earlier tier.

### Dependency Graph

    TIER 0 (foundations — no dependencies, all parallel)
    +------------------+     +------------------+     +------------------+
    | Phase A          |     | Phase B          |     | Phase C          |
    | EphemeralTask    |     | PAUSED Status    |     | Dispatcher       |
    | Module           |     | + Blocker Model  |     | request_replan   |
    |                  |     |                  |     | remove auto-     |
    | ephemeral_task/  |     | team/models.py   |     | cancel           |
    |                  |     |                  |     |                  |
    | deps: none       |     | deps: none       |     | deps: none       |
    +------------------+     +------------------+     +------------------+

    TIER 1 (core protocol — depends on Tier 0)
    +------------------+     +------------------+     +------------------+
    | Phase D          |     | Phase E          |     | Phase F          |
    | Conductor        |     | Replanner        |     | TaskCenter       |
    |                  |     | Toolkit          |     | Active Mode      |
    | conductor.py     |     |                  |     |                  |
    | pause/fix/resume |     | add_tasks        |     | on_edit/tick/    |
    | PauseAssessment  |     | declare_blocker  |     | check + auto    |
    |                  |     | cancel_and_redraft|    | note generation |
    |                  |     | remove old tools |     | read_sibling_   |
    |                  |     |                  |     | notes            |
    | deps: A, B       |     | deps: C          |     | deps: A          |
    +------------------+     +------------------+     +------------------+

### Phase A — EphemeralTask Module

    Status: [ ]
    Deps: none
    Parallel with: B, C

    Deliverables:
        [ ] ephemeral_task/__init__.py
        [ ] EphemeralTask base class
            - Fields: task_id, snapshot, prompt, system_prompt,
              max_tokens, model, timeout_seconds
            - run() method: single LLM call, no tools, returns EphemeralTaskResult
        [ ] EphemeralTaskResult dataclass
            - Fields: text, timed_out, elapsed_seconds
        [ ] PauseAssessmentTask (extends EphemeralTask concept)
            - Builds blocker check prompt
            - Parses YES/NO/TIMEOUT from response
        [ ] PauseVerdict dataclass
            - Fields: task_id, answer (YES/NO/TIMEOUT), reason, conversation

    Tests:
        [ ] EphemeralTask.run returns result with mock API client
        [ ] EphemeralTask.run returns timed_out=True on timeout
        [ ] PauseAssessmentTask parses YES correctly
        [ ] PauseAssessmentTask parses NO correctly
        [ ] PauseAssessmentTask returns TIMEOUT on slow response

### Phase B — PAUSED Status + Blocker Model

    Status: [ ]
    Deps: none
    Parallel with: A, C

    Deliverables:
        [ ] Add PAUSED to TaskStatus enum in team/models.py
        [ ] Verify PAUSED is NOT in TERMINAL_STATUSES frozenset
        [ ] Blocker dataclass in team/models.py
            - Fields: id, team_run_id, status, reason, root_cause_paths,
              blast_radius, fix_task_id, declared_by, initiating_task_id,
              fix_summary, pending_assessments, created_at, resolved_at
        [ ] BlockerStatus enum: ASSESSING, FIXING, RESOLVED, FAILED
        [ ] TaskRecord additions: blocker_id, pause_checkpoint, pause_verdict
            (no paused_from — only RUNNING tasks can be paused)
        [ ] DispatcherStore new methods:
            - pause_running_task(task_id, blocker_id, checkpoint, verdict)
            - resume_paused_tasks(run_id, blocker_id) returns count
            - cancel_paused_tasks(run_id, blocker_id) returns count
            - get_subtree_task_ids(run_id, parent_id) returns set of str
        [ ] pop_ready: add blocker blast_radius guard (skip, not pause)

    Tests:
        [ ] PAUSED task does not trigger maybe_promote_expanded_parent
        [ ] pause_running_task only accepts RUNNING tasks
        [ ] resume_paused_tasks transitions PAUSED to READY
        [ ] cancel_paused_tasks transitions PAUSED to CANCELLED
        [ ] pop_ready skips (not pauses) tasks overlapping active blocker blast_radius
        [ ] get_subtree_task_ids returns full recursive subtree

### Phase C — Dispatcher request_replan Change

    Status: [ ]
    Deps: none
    Parallel with: A, B

    Deliverables:
        [ ] dispatcher_store.py request_replan:
            remove step 2 (cancel pending/ready/expanded siblings)
            remove step 3 (cascade cancel dependents)
            keep step 1 (mark task FAILED) and step 5 (insert replanner)
        [ ] Replanner now sees live siblings when it runs

    Tests:
        [ ] request_replan no longer cancels siblings
        [ ] request_replan still marks the failing task FAILED
        [ ] request_replan still inserts replanner task
        [ ] Siblings remain in their original status after request_replan

### Phase D — Conductor

    Status: [ ]
    Deps: Phase A (EphemeralTask), Phase B (PAUSED + Blocker model)
    Parallel with: E, F

    Deliverables:
        [ ] team/runtime/conductor.py — new file
        [ ] Conductor class with:
            - register_executor / unregister_executor
            - create_blocker (from replanner's declare_blocker verdict)
            - assess_running (spawn PauseAssessmentTasks for RUNNING agents only;
              non-running tasks are NEVER touched)
            - _spawn_pause_assessments (asyncio.gather, parallel)
            - _run_pause_assessment (uses PauseAssessmentTask from Phase A)
            - _on_pause_yes (terminate executor, save checkpoint, mark PAUSED)
            - _on_pause_no (discard, log)
            - spawn_resolver (create resolver task at depth=0, dedicated role)
            - on_fix_complete: resume assessed agents, lift pop_ready guard,
              spawn replanner for initiating_task_id
            - on_fix_failed: cancel PAUSED tasks, mark team run FAILED
            - resume_assessed (PAUSED → READY with checkpoint restore)
            - guard_pop_ready (skip candidates, never change status)
            - match_fingerprint (safety net)
            - intercept_retry_replan
        [ ] Wire Conductor into TeamRun lifecycle (team_run.py)
        [ ] Executor changes:
            - expose display_messages snapshot
            - handle asyncio.CancelledError from external termination
            - support resume from pause_checkpoint
            - register/unregister with Conductor

    Tests:
        [ ] assess_running spawns PauseAssessmentTask ONLY for RUNNING agents
        [ ] assess_running does NOT touch READY/PENDING/FAILED tasks
        [ ] YES verdict terminates executor and saves checkpoint
        [ ] NO verdict discards assessment, original continues
        [ ] TIMEOUT treated as YES
        [ ] spawn_resolver creates resolver-role task at depth=0
        [ ] on_fix_complete resumes assessed agents + lifts guard + spawns replanner
        [ ] on_fix_failed cancels PAUSED tasks and marks team run FAILED
        [ ] resume from checkpoint starts new agent run with saved messages
        [ ] guard_pop_ready skips (not pauses) blocked candidates
        [ ] fingerprint safety net catches missed tasks
        [ ] post-fix replanner is scoped to initiating_task_id

### Phase E — Replanner Toolkit

    Status: [ ]
    Deps: Phase C (request_replan no longer auto-cancels)
    Parallel with: D, F

    Deliverables:
        [ ] tools/posthook/toolkit.py changes:
            - Remove: RequestRetryTool
            - Remove: SubmitReplanTool
            - Add: AddTasksTool
                params: tasks (list of TaskSpec)
                inserts tasks as siblings, no cancellation
            - Add: DeclareBlockerTool
                params: root_cause_paths, blast_radius, reason, suggestion
                triggers Conductor.create_blocker
            - Add: CancelAndRedraftTool
                params: tasks (list of TaskSpec)
                cancels all siblings + children, inserts new plan
            - PosthookTools.from_context: update replanner role mapping
            - Add: SubmitFixTool (resolver role)
                params: fix_summary
                triggers Conductor.on_fix_complete
            - Add: AbandonFixTool (resolver role)
                params: reason
                triggers Conductor.on_fix_failed
            - PosthookTools.from_context: add resolver role mapping
        [ ] Remove RequestRetryTool from developer/reviewer tool list
        [ ] Update replanner playbook SKILL.md with decision guidance
        [ ] Update developer playbook SKILL.md to remove request_retry references

    Tests:
        [ ] AddTasksTool inserts tasks without cancelling siblings
        [ ] DeclareBlockerTool triggers Conductor
        [ ] CancelAndRedraftTool cancels siblings then inserts new tasks
        [ ] Developer posthook only has submit_summary + request_replan
        [ ] Replanner posthook has add_tasks + declare_blocker + cancel_and_redraft
        [ ] Resolver posthook has submit_fix + abandon_fix (nothing else)
        [ ] SubmitFixTool triggers Conductor.on_fix_complete
        [ ] AbandonFixTool triggers Conductor.on_fix_failed

### Phase F — TaskCenter Active Mode

    Status: [ ]
    Deps: Phase A (EphemeralTask)
    Parallel with: D, E

    Deliverables:
        [ ] team/task_center.py additions:
            - ActivityCounters per-task internal state
            - _counters dict mapping task_id to ActivityCounters
            - on_edit(task_id, file_path)
            - on_posthook(task_id)
            - tick(task_id)
            - on_note_posted(task_id) — reset counters, called by post()
            - check(task_id, executor) — spawn EphemeralTask if threshold crossed
            - read_sibling_notes(parent_id, dispatcher_store, keyword, scope_paths)
        [ ] Executor changes:
            - call task_center.on_edit when edit tool completes
            - call task_center.on_posthook when posthook tool completes
            - call task_center.tick after each tool result
            - call task_center.check after tick
        [ ] context_for: add resume message injection at Priority 0
        [ ] Remove existing nudge:
            - Remove query.py lines 536-559 (inline edit nudge)
            - Remove _track_edit_for_note_nudge from daytona tools
            - Remove edits_since_last_note, files_edited_since_last_note,
              _note_nudge_at_edit from MERGED_RUNTIME_METADATA_KEYS
            - Remove manual counter resets from PostNoteTool.execute

    Tests:
        [ ] on_edit increments edit counter
        [ ] on_posthook resets turn counter
        [ ] tick increments turn counter
        [ ] check spawns EphemeralTask after 5 edits
        [ ] check spawns EphemeralTask after 10 turns without posthook
        [ ] Auto-generated note posted with "(auto)" suffix
        [ ] post() calls on_note_posted to reset counters
        [ ] read_sibling_notes resolves subtree and returns notes
        [ ] Existing edit nudge logic removed from query.py
        [ ] Resume message injected at Priority 0 for formerly-paused tasks

### Parallelism Map

    Time ---->

    Week 1:     Phase A          Phase B          Phase C
                EphemeralTask    PAUSED + Blocker  Dispatcher change
                (standalone)     (models + store)  (remove auto-cancel)
                    |                |                 |
                    |                |                 |
    Week 2:     Phase D          Phase E          Phase F
                Conductor        Replanner tools   TaskCenter active
                (needs A, B)     (needs C)         (needs A)
                    |                |                 |
                    |                |                 |
    Week 3:     Integration testing across D + E + F
                End-to-end walkthrough of the compatibility.py scenario

    3 developers can work in parallel:
        Dev 1: Phase A then Phase D (EphemeralTask → Conductor)
        Dev 2: Phase B then Phase E (Models → Replanner toolkit)
        Dev 3: Phase C then Phase F (Dispatcher → TaskCenter active)

    Each developer owns a vertical slice from foundation to feature.
    No cross-developer blocking within a tier.

---

## 19. Tradeoffs and Scores

### Scoring

    Dimension                   Score       Notes
    
    Simplicity                  9/10        2 dev tools, 3 replanner tools, 1 new
                                            status, 1 new actor. Sibling scope kills
                                            cross-tree complexity entirely.

    Coherence with existing     8/10        Builds on replan mechanism, same parent
                                            scope, same replanner role. One behavior
                                            change: request_replan stops auto-cancel.
                                            This is an improvement, not a compromise.

    Role clarity                9/10        Developer: "I failed." Replanner: assess
                                            and pick 1 of 3. Conductor: execute.
                                            Zero overlap. Zero new roles.

    Completeness                7/10        Solves the scenario fully within a subtree.
                                            Cross-subtree handled independently.
                                            Deliberate tradeoff for simplicity.

    Blast radius of changes     8/10        1 tool removed, 1 flow changed, 3 tools
                                            added replacing 1. query.py untouched.
                                            Surgical additions, not a rewrite.

    Correctness                 8/10        PAUSED non-terminal keeps parents safe.
                                            Pause checkpoint is clean resume point.
                                            Fingerprint safety net catches missed tasks.
                                            Clear fallback when fix fails.

    Overall                     8/10

### What Costs the 2 Points

Every failure now spawns a replanner. The old request_retry was a zero-cost status reset. The new design trades one extra LLM call per failure for better decisions. This is buying judgment with compute.

Cross-subtree blockers are handled independently. Same broken file in two subtrees means two fix tasks. The second fix task sees the file already repaired and completes immediately. Slightly redundant but correct.

The replanner must correctly triage. If it calls add_tasks when it should declare_blocker, the retried task fails again, triggering another replan. Self-correcting but burns one extra cycle.

### What Earns the 8

Developer is trivially simple. Single decision point for all failure recovery. Conductor is fully testable and deterministic. PauseAssessmentTask is zero-impact when the answer is NO. query.py is completely untouched. PAUSED status solves parent-promotion invariant cleanly. Scoped to siblings means no ancestor reopening in the common case. Maps onto existing models. Three replanner tools have zero semantic overlap.
