---
title: "Role: Generator (Executor + Verifier)"
tags: ["task-center", "generator", "executor", "verifier", "role", "context-recipe", "dag", "submission", "see-also"]
created: 2026-05-13T00:00:00.000Z
updated: 2026-05-13T00:00:00.000Z
sources: []
links: ["task-center-pipeline.md", "context-engine-recipes.md", "role-planner.md", "role-evaluator.md", "sandbox-subsystem.md"]
category: architecture
confidence: high
schemaVersion: 1
---

# Role: Generator (Executor + Verifier)

A "generator" is any DAG leaf inside one Attempt. The harness recognizes one structural role (`HarnessTaskRole.GENERATOR`) split into two _profile roles_ — `executor` and `verifier` — that share the same context recipe but expose different tool palettes and different terminal contracts. Generators are the only roles in the TaskCenter that mutate sandbox state.

## Identity in one sentence

> The generator is the **worker** of one DAG node: an executor produces artifacts, a verifier checks them; both write a single durable task summary that downstream tasks, the evaluator, and the retry planner inherit.

## Place in the pipeline

```
Attempt
  ├── planner          (designs DAG and rubric)
  ├── generator × N    ← (you are here — DAG of executors/verifiers)
  └── evaluator        (judges the result)
```

Each generator task carries deterministic id `{attempt_id}:gen:{local_id}` (`task_center/task/ids.py`). Dispatched by `AttemptDispatcher._dispatch_generating` (`task_center/attempt/dispatcher.py:94`) when all of its `needs[]` are `DONE`.

The entry executor (the top-level agent that owns the user request) is _also_ a generator role, but with `task_center_attempt_id=None`. It is not part of an Attempt DAG — it lives outside the Mission/Episode/Attempt tree and dispatches missions via `request_mission_solution`.

## Two profiles, one recipe

| | Executor (`agents/profile/main/generator_executor.md`) | Verifier (`agents/profile/main/generator_verifier.md`) |
|---|---|---|
| `role` (in profile frontmatter) | `executor` | `verifier` |
| `allowed_tools` | `read_file, write_file, edit_file, shell, run_subagent, ask_advisor` | `read_file, shell, ask_resolver` |
| Terminals | `submit_execution_success`, `submit_execution_failure`, `request_mission_solution` | `submit_verification_success`, `submit_verification_failure` |
| `context_recipe` | `generator_v1` | `generator_v1` (same recipe) |
| Notification triggers | `request_mission_after_edit` | `resolver_limit` |
| Editorial stance | Build the artifact | Inspect; if broken, delegate the fix via `ask_resolver`; re-check |

Profile-level separation is enforced by each `AgentDefinition.terminals` whitelist — the executor profile lists only executor terminals, the verifier profile lists only verifier terminals — so a verifier-launched task cannot reach an executor terminal at all. There is no runtime role gate; the structural role on the task row is set by the dispatcher and consumed by `resolve_attempt_submission_context`.

The recipe is identical because the _information_ each profile needs is identical: the DAG framing, the dependency summaries, and the local task spec. The _operation_ differs because the tool palette and the success contract differ.

## Lifecycle

| Event | Effect |
|---|---|
| Planner submission | Dispatcher creates generator task rows `PENDING`. |
| Dependencies all `DONE` | `_dispatch_generating` flips row to `RUNNING`, composes `generator_v1`, launches agent (`dispatcher.py:147`). |
| Agent calls `submit_execution_success` / `submit_verification_success` | `apply_generator_submission(outcome="success")` → task `DONE`; dispatcher checks quiescence and either launches more ready siblings or spawns the evaluator. |
| Agent calls `submit_execution_failure` / `submit_verification_failure` | Task `FAILED`; `block_failed_descendants` walks the DAG and marks every transitive PENDING descendant as `BLOCKED`. Attempt closes `FAILED/generator_failed`. |
| Agent calls `request_mission_solution` | Task parks in `WAITING_COMPLEX_TASK`; child Mission starts. Calling agent run ends here. Child Mission close-report drives task to `DONE`/`FAILED`. |
| Agent run ends without terminal | Launcher synthesises `apply_generator_submission(outcome="failure")` with `fail_reason="generator_failed"`. |

## Responsibilities

What an executor MUST do:

1. **Read the assigned task** — `planned_task_spec` block (REQUIRED, last position). This is the contract.
2. **Use the attempt plan** (`task_specification` block, HIGH) only as framing. Don't re-implement other tasks; sibling DAG nodes are someone else's job.
3. **Use dependency summaries** for inputs. The DAG's edges encode information flow.
4. **Decide scope fit before editing.** If the task is too broad, call `request_mission_solution` _before_ any `write_file`/`edit_file`/`shell` use. Once edits begin, the `request_mission_after_edit` notification reminder fires to steer the agent toward finishing through its own success/failure terminal instead.
5. **Write a self-contained summary on submission.** It is the only durable record of this task — siblings, the evaluator, and a future retry planner will read it without the conversation history.
6. **Exit via exactly one terminal.**

What a verifier MUST do:

1. **Inspect via read-only tools** (`read_file`, read-only `shell`).
2. **If unresolved issues need fixes**, dispatch via `ask_resolver`. The `resolver_limit` notification reminder fires at 4 unresolved calls; the verifier is expected to switch to `submit_verification_failure` rather than wave through a success the resolver loop never closed.
3. **Re-check after resolver completes** — verification is the read-modify-verify loop encoded across two agents.
4. **Exit via success only when the output passes.** Don't carry forward unresolved issues; submit failure and let the attempt retry or the evaluator catch it.

What both MUST NOT do:

- **Mutate state outside this task's scope.** A generator owns its node; siblings own theirs.
- **Re-derive the episode goal.** The episode goal is not in the context. The task spec is the contract.
- **Re-judge other tasks' work.** Dependency summaries are inputs, not litigation material.

## Context recipe — `generator_v1`

Source: `task_center/context_engine/recipes/generator.py:32-89`. Required scope: `{mission_id, attempt_id, task_id}`.

**Block order (rendered top-to-bottom):**

| Position | Block kind | Priority | When present |
|---|---|---|---|
| 1 | `task_specification` | HIGH | `attempt.task_specification` non-empty (always, post-planning) |
| 2..N | `dependency_summary` (under `# Dependency Results`) | MEDIUM | One per resolved `task.needs[]` entry; rendered via `latest_summary_text` |
| last | `planned_task_spec` | **REQUIRED** | Always (the task's own `task_input`, derived from the planner's `task_specs[local_id]`) |

**Why this order:** The local task spec is the concrete obligation. Placing it last anchors the agent's reading on what it must do, not on what the broader attempt is about.

**What is deliberately absent from the generator's view:**

- **No mission goal, no episode goal, no prior episode results.** The generator never sees the broader contract. This is information minimization: re-exposing the wider goal invites scope creep (a generator that knows the mission goal might "improve" beyond their task and break invariants the planner relied on).
- **No `partial_plan_boundary` or `continuation_goal`.** Partial-plan boundaries are evaluator and retry-planner context. A generator should not reason about future episode scope while executing its node.
- **No evaluation criteria.** The criteria are the evaluator's business. A generator that sees them risks teaching to the test in the wrong direction.
- **No failed-attempt landscape.** Retry history is the planner's responsibility, not the worker's.
- **No sibling task specs.** Each generator sees only its own `planned_task_spec`. Siblings communicate only via summaries on completed dependency edges.

**Dependency summary policy:** `latest_summary_text(dep.summaries)` (`_summaries.py:14`) returns only the **last** summary entry — preferring `"summary"`, falling back to `"outcome"`, then `"(empty)"`. If a dependency was retried across attempts and ultimately succeeded, the dependent sees only the final summary. The summary list itself is not exposed.

**Implication:** Write task summaries for a reader who has none of the conversation context. The summary _is_ the handoff.

## Submission contracts

### Executor

#### `submit_execution_success(summary, artifacts)`

`tools/submission/main_agent/generator/executor/submit_execution_success.py:18`. `summary` is non-blank prose; `artifacts: list[str]` is an open-ended list of identifiers the executor wants downstream readers to know about. Calls `submission_context.submit_executor_success` which routes through `apply_generator_submission(outcome="success", payload={"artifacts": ...})`.

#### `submit_execution_failure(summary, reason, details)`

Same flow with `outcome="failure"`. The `reason` field is a short cause-of-failure tag; `details` lists specifics. Failure cascades: the dispatcher's `block_failed_descendants` walks the DAG (`dispatcher.py:76`) and marks every transitive PENDING descendant as `BLOCKED`. The attempt closes `FAILED/generator_failed`.

#### `request_mission_solution(goal)`

`tools/submission/main_agent/generator/request_mission_solution.py:41`. Spawns a child Mission. Gates:

`request_mission_solution` has no pre-hook. Verifier-vs-executor restriction is enforced by the profile's `terminals` whitelist (only the executor profile lists `request_mission_solution`). Structural role checks (caller is a live generator in an open attempt) happen inside `resolve_attempt_submission_context`, which raises `AttemptSubmissionContextError`. After the first edit, the `request_mission_after_edit` notification reminder nudges the agent toward its own success/failure terminal — the call itself is not blocked.

After acceptance, the parent task parks in `WAITING_COMPLEX_TASK` (`MissionStarter.start`, `mission/starter.py:53`); the agent run terminates. When the child Mission closes, `MissionCloseReportRouter.deliver` routes the close report back into the parent attempt's orchestrator, which transitions the parked task to `DONE` (mission succeeded) or `FAILED` (mission failed), and `dispatch_ready_work()` re-evaluates the DAG.

### Verifier

#### `submit_verification_success(summary, checks)`

`tools/submission/main_agent/generator/verifier/submit_verification_success.py:26`. Calls `apply_generator_submission(outcome="success", payload={"generator_role": "verifier", "checks": ...})`. There is no hook gate on this terminal; resolver-loop saturation is signalled by the `resolver_limit` notification reminder, not a blocking gate.

#### `submit_verification_failure(summary, unresolved_issues)`

Same routing with `outcome="failure"`. Failure cascades the same way as executor failure.

## Constraints in summary

Submission terminals have no pre-hooks. Structural role and attempt-open checks are enforced by `resolve_attempt_submission_context`, which fails the tool with `AttemptSubmissionContextError`. Profile-vs-terminal separation (executor cannot call verifier terminals, and vice versa) is enforced by each profile's `AgentDefinition.terminals` whitelist. After-edit and resolver-loop pressure are delivered as notification reminders (`request_mission_after_edit`, `resolver_limit`), not blocking gates.

## DAG behavior

**Ready set:** `ready_pending_generator_ids` (`generator_dag.py:72`) filters `PENDING` tasks whose every `need` is `DONE`.

**Concurrency:** All ready siblings launch in the same dispatcher tick. The launcher schedules asyncio tasks; concurrency is bounded by the launcher's semaphore, not the dispatcher.

**Failure propagation:** `blocked_descendant_ids` (`generator_dag.py:90`) is BFS over the dependents graph. Any PENDING descendant of a FAILED task is marked `BLOCKED`. Non-pending descendants of a failed task are an _invariant violation_ — `generator_dag.py:124` raises `TaskCenterInvariantViolation` rather than tolerate it. The invariant is hard, not best-effort: a non-PENDING dependent of a failing task indicates a race or replan ordering bug, not transient state.

**Quiescence:** Once `all_generators_quiescent` is true (every task in a terminal status), the dispatcher either closes the attempt as `FAILED/generator_failed` (any FAILED/BLOCKED present) or spawns the evaluator (`all_generators_done`).

**Retry granularity is the attempt, not the task.** A failed generator does not retry in place. The whole attempt closes; a new attempt spawns a new planner that re-derives the DAG, possibly differently. This is deliberate — single-task retry would leak fail-context across the attempt boundary and complicate the planner's failure-landscape projection.

## Key insights

**1. Same context, different operations.** Executor and verifier both receive `generator_v1` because the _information_ they need is the same: framing, dependencies, local task spec. The _difference_ is tool palette and submission contract. This factoring means the recipe never has to model "what kind of generator am I" — the agent profile does.

**2. The generator is the most context-starved role.** No mission goal. No episode goal. No criteria. No retry history. The block ordering ends on `planned_task_spec` so the prompt closes on the concrete obligation. Anything broader is either framing (`task_specification`) or input (dependency summaries). This minimization is the design's defense against scope creep.

**3. Summaries are the only durable handoff.** Three downstream consumers read a generator's task summary:

- **Sibling/downstream generators** see it via `dependency_summary` blocks in their `generator_v1` packet (latest only).
- **The evaluator** sees it via `completed_task_summary` blocks in `evaluator_v1` (latest only).
- **A retry-attempt's planner** sees an aggregated `prior_episode_summary` if the episode closed and a continuation episode opened.

None of these readers see the conversation, the diffs, the tool calls, or the artifacts directly. The summary _is_ the truth.

**4. `request_mission_solution` is gated before edits.** A generator that has begun editing has committed to direct execution — the sandbox is already modified. Allowing recursive delegation after that point would create a state-management problem: the child Mission's "fresh" precondition is no longer true. The gate enforces an early-or-not-at-all decision.

**5. Verifier ↔ resolver is read-modify-verify across agents.** The verifier owns inspection but not edits. When verification finds an issue, `ask_resolver` dispatches a helper agent (a `subagent` profile) to make the fix; the verifier then re-runs its checks. The `resolver_limit` notification reminder nudges the verifier toward `submit_verification_failure` once the loop has run several times without resolution — but the decision is the verifier's, not a hook's.

**6. Failure cascades, retry doesn't.** A single FAILED generator cascades into BLOCKED descendants and closes the entire attempt. The next attempt (if budget remains) starts from scratch: new planner, fresh DAG, no in-flight state from the failed attempt. The only thing that crosses the attempt boundary is the _failure landscape projection_ in the next planner's prompt.

**7. The entry executor is structurally a generator.** It uses `entry_executor_v1` (a separate recipe with just an `entry_request` block) and has no attempt id, but its `HarnessTaskRole` is `GENERATOR`. It can `request_mission_solution` to delegate. Most user-facing interactions begin as one entry executor that either solves the request directly or dispatches a Mission.

**8. Two failure terminals exist because nuance matters downstream.** `execution_failure(reason, details)` and `verification_failure(unresolved_issues)` carry different shaped payloads. The executor's failure carries causation; the verifier's failure carries an issue list. Both flow into `apply_generator_submission(outcome="failure")` but the persisted payload differs — useful for log analysis and for retry planners reasoning about what kind of failure to address.

## Context building workflow

This section traces — end-to-end — how a generator's `task_input` string is built. The generator's recipe is the **narrowest** of the three roles: it sees the local task, its immediate dependency outputs, and the attempt-wide plan as framing. Nothing else.

### The seven-stage pipeline

```
┌─────────────────────────────────────────────────────────────────────────┐
│  AttemptDispatcher._dispatch_generating  (attempt/dispatcher.py:94)     │
│      ready = ready_pending_generator_ids(task_records)                  │
│        # all needs[] are DONE, status PENDING                           │
│      for task in ready:                                                 │
│          task.status = RUNNING                                          │
│          composer.compose(                                              │
│              recipe_id="generator_v1",                                  │
│              scope=ContextScope(mission_id, attempt_id, task_id))       │
└──────────────────────────────────┬──────────────────────────────────────┘
                                   │
                                   ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  ContextComposer.compose                                                │
│  (task_center/agent_launch/composer.py)                                 │
└──────────────────────────────────┬──────────────────────────────────────┘
                                   │  engine.build(recipe_id, scope)
                                   ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  ContextEngine.build  (engine.py:60)                                    │
│      recipe = RecipeRegistry.get("generator_v1")                        │
│      scope.assert_fields({mission_id, attempt_id, task_id})             │
└──────────────────────────────────┬──────────────────────────────────────┘
                                   │
                                   ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  _generator_v1_build  (recipes/generator.py:32)                         │
│      attempt = attempt_store.get(attempt_id)                            │
│      task    = task_store.get_task(task_id)                             │
│                                                                         │
│      blocks  = []                                                       │
│      if attempt.task_specification:                                     │
│          blocks += [ task_specification block HIGH ]                    │
│                                                                         │
│      for dep_id in task["needs"]:                                       │
│          dep = task_store.get_task(dep_id)                              │
│          if dep is None: continue                                       │
│          blocks += [ dependency_summary block MEDIUM ]                  │
│                                                                         │
│      blocks += [ planned_task_spec block REQUIRED ]    ← always last    │
│                                                                         │
│      return ContextPacket(target_role="generator", blocks=blocks, ...)  │
└──────────────────────────────────┬──────────────────────────────────────┘
                                   │  ContextPacket
                                   ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  MarkdownPromptRenderer.render  (renderer.py:125)                       │
│      group_heading="# Dependency Results" groups all dependency_summary │
│      blocks under one heading.                                          │
│      planned_task_spec heading defaults to "# Assigned Task".           │
└──────────────────────────────────┬──────────────────────────────────────┘
                                   │  task_input: str
                                   ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  EphemeralAttemptAgentLauncher                                          │
│      profile = executor or verifier  (set by the task row's agent_name) │
│      → launches the corresponding agent profile against the task_input  │
└─────────────────────────────────────────────────────────────────────────┘
```

The recipe is **scope-narrowest** of the three role recipes. The generator's scope demands `{mission_id, attempt_id, task_id}` — three fields — but the recipe itself never consults the mission or episode stores. Effectively, `mission_id` is carried in scope for canonical-refs purposes; the recipe reads only `attempt_store` (once) and `task_store` (1 + N times, where N is the dependency count).

### The four cases the generator sees

The dimensions are dependency count and attempt-plan presence:

| Case | task.needs | attempt.task_specification | Builders invoked | Block kinds |
|---|---|---|---|---|
| **A — root leaf, normal attempt** | `[]` | non-empty | spec + (no deps) + planned_task_spec | `task_specification`, `planned_task_spec` |
| **B — mid-DAG, normal attempt** | `[d1, d2]` | non-empty | spec + 2 dep summaries + planned_task_spec | `task_specification`, `dependency_summary`×2, `planned_task_spec` |
| **C — degenerate: empty plan** | any | empty | spec block elided; deps + planned_task_spec only | `dependency_summary`×N, `planned_task_spec` |
| **D — orphan dep id** | `[d1, d_missing]` | non-empty | `d_missing` silently skipped (`generator.py:99`) | as B with the missing dep omitted |

### Walk-through: Case A — a root leaf

A planner submits a 2-task DAG: `gen-fetch` (no deps) and `gen-process` (`needs=["gen-fetch"]`). Dispatcher launches `gen-fetch` first.

```
Stores read for gen-fetch:
  attempt_store.get(attempt_id)        → Attempt(
                                            task_specification="Build pipeline that fetches...",
                                            ...)
  task_store.get_task("gen-fetch")     → {
                                            id: "gen-fetch",
                                            needs: [],
                                            task_input: "Implement fetch_remote_csv...",
                                            agent_name: "executor.default",
                                            ...
                                         }

Builder decisions:
  attempt.task_specification truthy
    → append task_specification block HIGH

  task.needs == []
    → _dependency_summary_blocks returns []   (no iterations)

  always append planned_task_spec block REQUIRED  (last position)
    text = str(task["task_input"])

Final block sequence:
  [0] task_specification    HIGH      heading="# Attempt Plan"
  [1] planned_task_spec     REQUIRED  heading="# Assigned Task"
```

Rendered `task_input` (Case A):

```
# Attempt Plan

Build a pipeline that fetches the daily transactions CSV from the
warehouse SFTP, validates schema, and inserts new rows into the
`transactions` table. Deliver: a fetch component, a processor component,
and a smoke test that runs the full path against a fixture file.

# Assigned Task

Implement fetch_remote_csv(host, path, dest_dir) in services/sftp_fetch.py:
- connect via paramiko using credentials in env (SFTP_USER, SFTP_KEY_PATH)
- download `path` into `dest_dir/{basename}.csv`
- raise SftpFetchError on any IO failure
- return the absolute destination path
Do not modify any other module. Unit tests for this function only.
```

The prompt ends on the **concrete obligation**. Nothing the agent needs to do for *this* task is below the `# Assigned Task` heading.

### Walk-through: Case B — a mid-DAG node

Now the dispatcher launches `gen-process` after `gen-fetch` completes:

```
gen-fetch's submission wrote a summary list onto its task row:
  task_store.get_task("gen-fetch")["summaries"] =
    [{"summary": "Downloaded /daily/2026-05-13.csv to /tmp/2026-05-13.csv (12,491 rows). No schema drift detected."}]

Stores read for gen-process:
  attempt_store.get(attempt_id)            → Attempt(task_specification="...", ...)
  task_store.get_task("gen-process")       → {
                                                id: "gen-process",
                                                needs: ["gen-fetch"],
                                                task_input: "Implement process_csv...",
                                                ...
                                             }
  task_store.get_task("gen-fetch")          → {... summaries: [...] }   (read inside _dependency_summary_blocks)

Builder decisions:
  attempt.task_specification truthy
    → task_specification block HIGH

  task.needs == ["gen-fetch"]
    iterate:
      dep = task_store.get_task("gen-fetch")
      dep is not None → emit dependency_summary block MEDIUM
        text = latest_summary_text(dep["summaries"])
             = "Downloaded /daily/2026-05-13.csv to /tmp/..."
        metadata.group_heading = "# Dependency Results"
        metadata.subheading    = "gen-fetch"

  always append planned_task_spec REQUIRED

Final block sequence:
  [0] task_specification     HIGH      heading="# Attempt Plan"
  [1] dependency_summary     MEDIUM    group=# Dependency Results, subheading=gen-fetch
  [2] planned_task_spec      REQUIRED  heading="# Assigned Task"
```

Rendered `task_input` (Case B):

```
# Attempt Plan

Build a pipeline that fetches the daily transactions CSV from the
warehouse SFTP, validates schema, and inserts new rows into the
`transactions` table. Deliver: a fetch component, a processor component,
and a smoke test that runs the full path against a fixture file.

# Dependency Results

## gen-fetch

Downloaded /daily/2026-05-13.csv to /tmp/2026-05-13.csv (12,491 rows).
No schema drift detected.

# Assigned Task

Implement process_csv(path) in services/transactions_processor.py:
- read CSV from `path`, validate header against TRANSACTION_COLUMNS
- skip rows whose `external_id` already exists in `transactions`
- bulk-insert remaining rows in batches of 500
- return ProcessResult(inserted, skipped, errors)
Use the file produced by the gen-fetch dependency (see Dependency Results
above).
```

Critically: `gen-process` does **not** see `gen-fetch`'s `task_input`, agent role, or the conversation that produced the summary. It sees one paragraph of prose. The summary *is* the contract.

### Walk-through: Case D — orphaned dependency

If a dep id in `needs` does not resolve in `task_store` (e.g., a transient store inconsistency or a deleted row), the recipe silently skips that block:

```
task.needs = ["gen-fetch", "gen-validate"]
loop:
  task_store.get_task("gen-fetch")     → {...}              → emit block
  task_store.get_task("gen-validate")  → None               → continue (skip)

Final blocks: just the gen-fetch summary; gen-validate is invisible.
```

This is a **silent degradation** by design (`generator.py:99-100`). The dispatcher's invariants (`generator_dag.py`) guarantee `needs` references are valid at DAG-construction time; a runtime miss is treated as recoverable rather than as a hard error. The trade-off: a generator may proceed under-informed if upstream state corrupts. The downstream verifier or evaluator is expected to catch the resulting defect.

### Where each piece of information comes from

```
                                  ┌── attempt.task_specification ─► task_specification block
                                  │                                    (HIGH, framing)
attempt_store ──────────►─────────┤
                                  │   (attempt is read but only the task_specification field
                                  │    feeds the prompt; episode/criteria/generator_ids never
                                  │    reach the generator's view)
                                  │
                                  ┌── task["task_input"] ──────────► planned_task_spec block
                                  │                                    (REQUIRED, last)
task_store    ──────────►─────────┤
                                  │   for dep_id in task["needs"]:
                                  │     dep = task_store.get_task(dep_id)
                                  │     if dep is not None:
                                  │       latest_summary_text(dep["summaries"])
                                  │                                  ► dependency_summary block
                                  │                                    (MEDIUM, group="# Dependency Results")

mission_store  ──── (not read by generator_v1) ────
episode_store  ──── (not read by generator_v1) ────
```

Compared to the planner and evaluator provenance maps, the generator's surface is dramatically narrower:

- **No mission or episode store reads.** The generator's contract is its task, full stop.
- **No `evaluation_criteria` exposure.** Even though `attempt.evaluation_criteria` is in the attempt row the recipe already loaded, the recipe does not surface it. This is deliberate scope minimization.
- **No `generator_task_ids` walk.** The generator sees only its own `needs[]` — a subset of the DAG. Siblings are invisible.

### The dependency summary contract

The same `latest_summary_text` helper used by the evaluator (`_summaries.py:14`) feeds dependency summaries:

```python
latest_summary_text([
  {"summary": "First update — wrote scaffolding"},
  {"summary": "Final update — completed implementation, 27 tests green"}
])
→ "Final update — completed implementation, 27 tests green"
```

Implications:

- **Only the last summary is exposed.** If a generator wrote intermediate progress summaries before its terminal one, those are unread. Write your terminal summary as if it is the only one.
- **Latest wins; field priority is `summary > outcome > "(empty)"`.** Executors emit `"summary"`; some fallback paths emit `"outcome"`.
- **No artifacts list, no tool-call trace, no diff.** Even if the executor returned `artifacts=["routes/import.py"]`, that list is persisted on the task row but **not surfaced** to dependents. The generator that needs to consume an artifact must learn its path from the dependency's prose summary.

### Where this recipe differs from the others — at a glance

```
┌────────────────────────┬───────────────┬───────────────┬───────────────┐
│ Block kind             │  planner_v1   │  evaluator_v1 │  generator_v1 │
├────────────────────────┼───────────────┼───────────────┼───────────────┤
│ mission_goal           │   ✓ (seq>1)   │   ✓ (seq>1)   │       ✗       │
│ episode_goal           │       ✓       │       ✓       │       ✗       │
│ prior_episode_*        │   ✓ (seq>1)   │   ✓ (seq>1)   │       ✗       │
│ failed_attempt_*       │       ✓       │       ✗       │       ✗       │
│ task_specification     │       ✗       │     ✓ REQ     │     ✓ HIGH    │
│ completed_task_summary │       ✗       │     ✓ HIGH    │       ✗       │
│ dependency_summary     │       ✗       │       ✗       │     ✓ MED     │
│ evaluation_criteria    │       ✗       │     ✓ REQ     │       ✗       │
│ planned_task_spec      │       ✗       │       ✗       │     ✓ REQ     │
└────────────────────────┴───────────────┴───────────────┴───────────────┘
```

Reading down each column tells you exactly what that role can know without going to a tool. The generator's column is the shortest by design.

### Closing-block discipline

Each recipe places its highest-priority _operational_ block last so the agent's reading ends on what to do:

| Recipe | Closes on |
|---|---|
| `planner_v1` | `failed_attempt_landscape` (HIGH) — the retry evidence |
| `evaluator_v1` | `evaluation_criteria` (REQUIRED) — the verdict basis |
| `generator_v1` | `planned_task_spec` (REQUIRED) — the concrete obligation |

This ordering is not what `_compress` operates on — priority drives only compression, not order (`renderer.py` docstring: _"Priority is a compression policy only; it is not a presentation-order policy."_). The order is fixed by the recipe builder's `blocks.append(...)` sequence.

### Failure shapes inside the recipe

| Where it fails | Trigger | Effect |
|---|---|---|
| `ContextEngine.build` | scope missing `mission_id`/`attempt_id`/`task_id` | `AssertionError` from `scope.assert_fields` → launcher exception → task fails with `fail_reason="agent_launch_failed"`; cascade to descendants. |
| `attempt_store.get` returns None | attempt row deleted/missing | `ContextEngineError("Attempt ... not found")` — same effect as above. |
| `task_store.get_task` returns None for the scope task | task row deleted/missing | `ContextEngineError("TaskCenterTask ... not found")`. |
| `task_store.get_task` returns None for a dep id | upstream task row gone | **silently skipped** — dependent proceeds without that dep's summary. |
| `task["task_input"]` falsy | planner submitted empty `task_specs[id]` | `planned_task_spec` block emitted with `text=""`. Pydantic's `_non_blank_required_text` validator on `ContextBlock` (`packet.py:73`) **raises ValidationError** because the priority is REQUIRED. The planner schema rejects blank task_specs upstream, so this should be unreachable in practice. |

## Failure modes

| Mode | Cause | Effect |
|---|---|---|
| Profile mismatch | Verifier agent calling executor terminal (or vice versa) | Each profile's `terminals` whitelist excludes the other side's terminals, so the call is never dispatched. |
| Edit-then-delegate | Edit tool used before `request_mission_solution` | `request_mission_after_edit` notification reminder fires; the generator is expected to finish through success/failure. The call is not hard-blocked. |
| Resolver saturation | ≥4 unresolved resolver calls before verifier success | `resolver_limit` notification reminder fires; verifier is expected to submit failure with remaining issues. |
| Submission failure | Agent submits explicit failure | Cascade BLOCKED descendants; close attempt `FAILED/generator_failed`. |
| Launcher exception | Agent crash, sandbox error | `_launch_ready_generator` exception path marks task `FAILED` with `fail_reason="agent_launch_failed"`; cascade. |
| Unfinished agent run | Run ends with no terminal | Launcher synthesises `apply_generator_submission(outcome="failure")`. |
| Submission to closed attempt | Race between submission and attempt close | `resolve_attempt_submission_context` raises `AttemptSubmissionContextError` once the attempt is closed. |

## See also

- [[role-planner]] — produces the `task_specs[id]` each generator consumes.
- [[role-evaluator]] — reads the summaries each generator writes.
- [[task-center-pipeline]] — the dispatcher and DAG state machine.
- [[context-engine-recipes]] — `generator_v1` block builders and renderer.
- [[sandbox-subsystem]] — where executor edits actually land.
