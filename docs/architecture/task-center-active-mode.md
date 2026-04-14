# TaskCenter Active Mode — Automatic Note Generation via External Trigger

**Status:** IMPLEMENTED  
**Date:** 2026-04-14  
**Branch:** `codex/pydantic-benchmark-loop`  
**Author:** Architecture session  
**Depends on:** Dynamic Replanning Blocker Protocol (dynamic-replanning-blocker-protocol.md), External Trigger Module (external_trigger/)

---

## Overview

The existing TaskCenter is a passive store — it holds notes and serves them. The existing edit-based nudge in query.py injects a SystemReminderBlock hoping the agent will call post_note. The agent may ignore it.

In active mode, the TaskCenter takes ownership of its own content quality. It tracks agent activity, detects when an agent has been silent too long, and spawns an external_trigger agent to produce a note on the agent's behalf. The agent is never interrupted. The note is guaranteed via `tool_choice="any"` with retry.

---

## Passive vs Active

    TaskCenter (existing — passive)
        post(note)              agents push notes in
        read(filters)           agents pull notes out
        context_for(task)       auto-inject into agent context

    TaskCenter (new — active)
        all of the above, PLUS:
        on_edit(task_id, file_path)     track edit activity
        on_posthook(task_id)            track posthook activity
        tick(task_id)                   track turn activity
        check(task_id, executor)        spawn external_trigger agent if thresholds crossed

    The active methods are called by the executor (outside the query loop).
    The TaskCenter decides when to spawn, and uses the external_trigger module
    (run_external_trigger → runner.run()) to execute the snapshot.

---

## Two Triggers, Two Counters

The TaskCenter maintains per-task counters:

    Trigger 1: Edit Progress
        Counter: edits since last post_note or auto-generated note
        Threshold: 5
        Resets on: post_note() call by agent, or auto-generated note
        External trigger prompt focus: "what files were edited and why"

    Trigger 2: Turn Checkpoint
        Counter: turns since last posthook call
        Threshold: 10
        Resets on: ANY posthook call (`post_note`, `request_replan`),
                   or auto-generated note
        External trigger prompt focus: "overall status, findings, and blockers"

    Why the reset conditions differ:

        Edit counter resets on post_note ONLY:
            Agent is actively editing. Only a progress note satisfies the need
            to document what changed.

        Turn counter resets on ANY posthook call:
            The point is "has the agent communicated at all?"
            Any posthook call means the agent is not silent.
            Only truly silent agents trigger a checkpoint.

---

## Architecture Diagram

    +------------------------------------------------------+
    |                    Executor                           |
    |                                                      |
    |  Agent (query loop) *** UNTOUCHED ***                |
    |  +----------------------------------------------+    |
    |  |  display_messages, tool calls, LLM turns     |    |
    |  +----------------------------------------------+    |
    |          |                                            |
    |          | tool result events (observed by executor)  |
    |          |                                            |
    |  Executor event observation (outside the loop):      |
    |          |                                            |
    |          +--- edit tool? ---> task_center.on_edit()   |
    |          +--- posthook?  ---> task_center.on_posthook()|
    |          +--- any event  ---> task_center.tick()      |
    |          |                                            |
    |          +--- task_center.check(task_id, snapshot=..., api_client=...)  |
    |                    |                                  |
    |                    | threshold crossed?               |
    |                    |                                  |
    |              +-----v-----+                            |
    |              | External  |  spawn ephemeral agent     |
    |              | Trigger   |  snapshot + PostNoteTool   |
    |              | Agent     |  tool_choice="any", retry  |
    |              +-----+-----+  guaranteed tool call      |
    |                    |                                  |
    |                    v                                  |
    |              TaskCenter.post(note)                    |
    |              (posts under original task's ID)         |
    |                                                      |
    +------------------------------------------------------+

    Data flow: Executor observes -> TaskCenter decides -> external_trigger executes
               -> note posted back to TaskCenter

    Agent: never interrupted, never aware
    query.py: completely untouched

---

## TaskCenter.check — The Decision Point

    task_center.check(task_id, *, snapshot=..., api_client=..., model=...)
          |
          v
    should_checkpoint(task_id) — look up per-task counters
          |
          +--- edits_since_note >= 5?
          |       |
          |      YES ---> spawn external_trigger agent
          |               prompt: EDIT_CHECKPOINT_PROMPT
          |               uses snapshot + api_client
          |               reset edit counter
          |               return True
          |
          +--- turns_since_posthook >= 10?
          |       |
          |      YES ---> spawn external_trigger agent
          |               prompt: TURN_CHECKPOINT_PROMPT
          |               uses snapshot + api_client
          |               reset turn counter
          |               return True
          |
          +--- neither threshold crossed
                    return False

    Fallback: when no api_client is available, check() generates
    a factual counter-based note (e.g. "Auto-checkpoint (7 edits): a.py, b.py")
    without an LLM call.

---

## External Trigger Prompts

    Edit trigger prompt:
        "Based on this agent's work so far, write a progress note
         for the Task Center.
         Focus on: what files were edited and why.
         Include file paths and specific changes made.
         Keep under 300 words."

    Turn trigger prompt:
        "Based on this agent's work so far, write a progress note
         for the Task Center.
         Include:
         1. What the agent has accomplished
         2. Current status (working / stuck / nearly done)
         3. Whether the agent appears blocked by code that another
            task broke (include the file path and error if so)
         Keep under 300 words."

    The turn prompt explicitly asks about blockers. This ensures
    that even if the agent does not recognize a systemic issue, the
    The external trigger agent surfaces it. The replanner sees it via read_sibling_notes.

---

## Note Attribution

    The auto-generated note is posted with:
        task_id         original task's ID
        agent_name      original agent's name + " (auto)"
        scope_paths     original task's scope_paths
        timestamp       current time

    To siblings and the replanner, it reads like a note from the
    original agent. The "(auto)" suffix distinguishes it from
    agent-authored notes for auditing.

---

## TaskCenter Class — Updated Definition

    TaskCenter (updated)

        Existing (passive)
            post(note: Note)
            read(*, authors, scope_paths, since, limit)
            context_for(task: Task, *, max_context_bytes)
                No external callbacks needed — TaskCenter owns the DAG.

        Per-task activity tracking (active)
            _activity_counters          dict mapping task_id to counter dict

            on_edit(task_id, file_path)
                Increment edit counter for task_id.
                Append file_path to files list (deduplicated).

            on_posthook(task_id)
                Reset turn counter for task_id to 0.

            tick(task_id)
                Increment turn counter for task_id.

            on_note_posted(note: Note)
                Reset both counters for the note's task_id to 0.
                Called internally by post().
                Ignores system/checkpoint agent notes (only resets
                for agent-authored or auto-generated notes).

        Checkpoint spawning (active)
            should_checkpoint(task_id) returns str or None
                Check thresholds. Returns "edit", "turn", or None.

            check(task_id, *, snapshot, api_client, model) returns bool
                If thresholds crossed:
                  With api_client: spawn external_trigger agent via
                  run_checkpoint_note() for rich LLM-generated note.
                  Without api_client: generate factual counter-based note.
                Posts result back via self.post().
                Returns True if a checkpoint was spawned.

        Sibling note query (for replanner)
            read_sibling_notes(parent_id, *, keyword, scope_paths)
                Resolve subtree task IDs via _sibling_subtree_ids (internal).
                Read all notes from those tasks.
                Apply optional filters.
                Return formatted notes string.

    Activity counters (per-task, stored as dict)
        edits                       int (edits since last note)
        turns                       int (turns since last posthook)
        files_edited                list of str

---

## Conversation Snapshot Mechanism

The external trigger agent needs a read-only snapshot of the running agent's conversation. The Conductor maintains these snapshots via `register_snapshot(task_id, snapshot)` called by the executor after each tool result. This requires a lightweight extension:

    The QueryRunner (run_query_loop) maintains display_messages internally.
    To expose a snapshot without modifying query.py internals:

    Option: Conversation observer callback.

    run_query_loop accepts an optional on_turn callback:
        on_turn(display_messages: list) -> None

    The executor provides this callback. On each turn, the callback
    receives a reference to display_messages. The executor stores
    the latest reference (not a copy — the list is append-only so
    a reference is safe for read-only snapshot).

    When TaskCenter.check or the Conductor needs a snapshot:
        snapshot = list(executor._latest_messages)  # shallow copy at read time

    This adds one optional parameter to run_query_loop's signature.
    The query loop body gains one line: calling the callback at the
    top of each turn (alongside ScopeChangeBuffer flush).

    This is the ONLY touch to the query loop — a single callback invocation.
    No message injection. No display_messages mutation.

---

## Integration with External Trigger Module

    TaskCenter uses the external_trigger module, not the other way around.

    TaskCenter.check()
          |
          | threshold crossed
          v
    Call run_checkpoint_note() from external_trigger.tc_note:
        task_id         = the tracked task's ID
        agent_run_id    = original agent's run ID
        messages        = executor conversation snapshot (read-only copy)
        prompt          = edit prompt or turn prompt (depending on trigger)
        max_tokens      = 500
        model           = cheaper model if configured (e.g. Haiku)
        api_client      = from team_run
          |
          v
    run_external_trigger() spawns ephemeral agent identity
          |
          v
    runner.run() with [PostNoteTool], tool_choice="any", retry until success
          |
          v
    RunResult(tool_name="post_note", validated=PostNoteInput(...))
          |
          v
    TaskCenter.post(Note(
        task_id     = original task's ID,
        agent_name  = original agent + " (auto)",
        content     = result.note_summary,
        scope_paths = original task's scope_paths,
    ))

    The external_trigger module is a dependency of TaskCenter.
    The Conductor also uses external_trigger (for pause assessment).
    Both are consumers of the same runner.run() loop.

---

## Dependency Diagram

    +-------------------+         +---------------------+
    |    Conductor      |         |    TaskCenter        |
    |                   |         |    (active mode)     |
    |  uses:            |         |                      |
    |  assess_pause()   |         |  uses:               |
    |  (pause_assess-   |         |  run_checkpoint_note |
    |   ment.py)        |         |  (tc_note.py)        |
    +--------+----------+         +----------+-----------+
             |                               |
             +---------------+---------------+
                             |
                             v
                 +-----------------------+
                 | external_trigger/     |
                 |                       |
                 |   runner.run()        |  shared LLM loop
                 |   run_external_       |  tool_choice="any"
                 |     trigger()         |  retry until success
                 |   RunResult           |
                 +-----------------------+
                             |
                             v
                 +-----------------------+
                 | tools/external_       |
                 |   trigger/            |
                 |   PauseVerdictTool    |
                 +-----------------------+
                 | tools/context/        |
                 |   PostNoteTool        |
                 |   (multi-type:        |
                 |    external_trigger   |
                 |    + post_run)        |
                 +-----------------------+

    external_trigger module is standalone.
    TaskCenter and Conductor are independent consumers.
    Executor also uses runner.run() for post-run submission.

---

## Migration from Existing Nudge

The existing edit-based nudge in query.py (lines 536-559) and _track_edit_for_note_nudge in daytona tools is replaced entirely. The TaskCenter's active mode replaces the inline counter logic. The metadata keys edits_since_last_note, files_edited_since_last_note, and _note_nudge_at_edit are no longer needed.

    Before: 24 lines of inline counter logic in query.py
            + 16 lines of _track_edit_for_note_nudge in daytona tools
            + 4 lines of counter reset in PostNoteTool
            Agent may ignore the nudge. query.py is touched.

    After:  TaskCenter active mode (activity tracking + external_trigger spawning)
            Executor calls on_edit/on_posthook/tick/check (outside query loop)
            Note is guaranteed. query.py is untouched.

---

## Interaction with the Blocker Protocol

    TaskCenter auto-generates notes periodically for all running agents
          |
          v
    Turn trigger prompt asks about blockers
          |
          v
    Auto-generated note surfaces blocker evidence early:
    "Blocked by dask/compatibility.py — ImportError on parse"
          |
          v
    Replanner calls read_sibling_notes() (via TaskCenter)
    Sees: 3 siblings all report the same ImportError
          |
          v
    Replanner: declare_blocker (high confidence, multiple signals)

    Without active mode: agents fail silently. Replanner sees
    one failure reason from the task that called request_replan.
    With active mode: replanner sees notes from ALL siblings,
    even the ones that never explicitly posted.
