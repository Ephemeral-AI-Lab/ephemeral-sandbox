# Refactor: TaskCenter → a Task-based agentic framework (`task/` + `workflow/`)

Status: draft (v2 — reshaped per design review)
Date: 2026-06-01
Builds on: `docs/plans/delegate_workflow_BACKGROUND_IMPLEMENTATION_PLAN.md`

## Framing: Task is the unifying agent interface

The system becomes a **standard agentic framework** — user input → agent loop →
terminal outcome → result — with one deliberate primitive: **every agent runs
against a `Task`.**

```
user input
  → create root Task        (request.root_task_id; root_task=True, workflow_id=None)
  → run root agent loop      (prompt = task.instruction; tools incl. delegate_workflow)
  → submit_root_outcome      (terminal) → request result → user
```

The explicit root Task is the thing "other frameworks don't have," and it earns
its place by giving **one contract for every agent**: *an agent is something that
runs against a Task.* That single shape covers the root agent and every
workflow-internal agent (planner / generator / reducer) identically — they differ
only in their **position columns** and their **terminal**.

`delegate_workflow` is an *optional, non-terminal* tool any agent (root or
generator) can call to spin up more Tasks under a delegated workflow. The old
`Workflow → Iteration → Attempt → planner → generators → reducers` decomposition
machinery is **kept** — it is simply *launched by an agent* now, never wrapping
the root.

**No "root workflow" vs "child workflow" distinction.** Today there are two kinds
of workflow, told apart by their parent: the *root workflow* (parent =
`<run_id>:root`, close → `run_close_handler` → finish the run) and a *child
workflow* (parent = a generator task, close → `apply_child_workflow_outcome` →
write onto the parent). That split exists **only because the old design wrapped
the top-level request in a synthetic workflow**. Here the root is not a workflow
at all (it's a bare agent), so every `Workflow` is uniformly "one an agent
launched": it has a `parent_task_id` (the launching Task — root agent *or* a
generator) and resolves through a **single** close path that delivers its
outcome back to that launching agent. The `_route_close` root-vs-child branch,
`run_close_handler`, and `apply_child_workflow_outcome`-onto-parent all collapse
into one uniform "deliver to the launcher" path.

## Pinned decisions

1. **No `TaskCenter`, no `run`.** The `task_center` package and the `runs` table
   are removed. The execution container is the **`request`**; tasks thread by
   **`request_id`** (renamed from `task_center_run_id` everywhere — on disk too,
   no `task_center_` prefix kept). `request` gains `root_task_id` + `status`.
2. **Task is first-class and carries its position as explicit columns.** Root is
   identified by `workflow_id IS NULL` (+ `request.root_task_id`), *not* by a
   `<run_id>:root` id string. `workflow_id` / `iteration_id` / `attempt_id` are
   optional columns. This replaces id-string parsing
   (`attempt_id_from_task_id`, `role_from_task_id`, `list_tasks_for_attempt`
   prefix-`LIKE`) with plain column reads.
3. **Two top-level packages.** `backend/src/task/` (the first-class unit: spawn a
   task, run its agent; entry, root agent, `submit_root_outcome`) and
   `backend/src/workflow/` (delegated decomposition launched by an agent;
   Iteration→Attempt kept; ContextEngine; planner/generator/reducer).
4. **Terminals:** `submit_root_outcome` (root → finishes the request → user
   result), `submit_generator_outcome` / `submit_reducer_outcome` (workflow
   tasks). `submit_root_outcome` is a near-clone of `submit_generator_outcome`.
5. **`delegate_workflow` is non-terminal** (`is_terminal_tool=False`): optional,
   returns a workflow handle, agent keeps running. Launchable by root *and*
   generator agents.
6. **`submit_workflow_handoff` is deleted** — the terminal-handoff model is gone;
   `delegate_workflow` replaces it. The `DisallowNestedWorkflowHandoff` pre-hook
   and its notification trigger are deleted too.
7. **ContextEngine is wired to workflow agents only.** When a workflow agent
   launches, it uses the **existing** launcher (`EphemeralAttemptAgentLauncher` +
   `AgentLaunchFactory` + composer), which appends initial messages built from
   the ContextEngine **context** packet + **task guidance**. The **root agent
   does not use the ContextEngine** — its initial message is just
   `task.instruction` (the user request).
8. **The planner authors `Task`s directly.** `PlannedGeneratorTask` and
   `PlannedReducerTask` are removed (the planner emits `Task` rows with `role`,
   `instruction`, `needs`). `GeneratorSubmission` / `ReducerSubmission` stay
   distinct for now.

## The Task (the unifying DTO)

```python
# ONE role enum (was: AgentRole + the internal TaskCenterTaskRole). ROOT added once.
class AgentRole(StrEnum):
    ROOT = "root"; PLANNER = "planner"; GENERATOR = "generator"
    REDUCER = "reducer"; HELPER = "helper"; SUBAGENT = "subagent"
# Task-bearing subset = {ROOT, PLANNER, GENERATOR, REDUCER}.
# HELPER/SUBAGENT are ephemeral agents that run WITHOUT a persisted Task (run_subagent / ask_advisor).

@dataclass(frozen=True, slots=True)
class Task:
    id: str
    request_id: str                  # threads the execution (run collapsed into request)
    role: AgentRole                  # validated ∈ {ROOT, PLANNER, GENERATOR, REDUCER}
    instruction: str                 # the assigned instruction (was context_message)
    status: TaskStatus               # pending | running | done | failed | blocked
    # --- position (all optional) ---
    workflow_id: str | None = None   # ★ root discriminator: NULL ⇔ root; set ⇔ in a delegated workflow
    iteration_id: str | None = None
    attempt_id: str | None = None
    # --- payload ---
    agent_name: str | None = None
    needs: tuple[str, ...] = ()
    outcomes: tuple[Any, ...] = ()
    terminal_tool_result: dict | None = None
```

| Task kind | `role` | `workflow_id` | `iteration_id` | `attempt_id` | `instruction` is… |
|---|---|---|---|---|---|
| **root** | `ROOT` | `None` | `None` | `None` | the user request |
| **planner** | `PLANNER` | set | set | set | the workflow goal |
| **generator** | `GENERATOR` | set | set | set | the planner's task spec |
| **reducer** | `REDUCER` | set | set | set | the planner's reducer prompt |

> **Two simplifications the final review forced:**
> - **One role enum.** `AgentRole` is the superset; `Task.role` is its
>   task-bearing subset. Deletes the separate `TaskCenterTaskRole`/`TaskRole` and
>   the dual-`ROOT` addition — `ROOT` is added to `AgentRole` *once*.
> - **`root_task` flag dropped.** The sole root discriminator is `workflow_id IS
>   NULL` (every non-root Task is minted with a `workflow_id`; the root is the only
>   `NULL`), with `request.root_task_id` as the forward pointer. Invariant to
>   enforce in the Task-spawn path: no non-root Task is ever created without a
>   `workflow_id`.

`WAITING_WORKFLOW` is **gone** from `TaskStatus` — a delegating agent stays
`RUNNING` (delegate_workflow is background; the agent waits via the
delivery-before-terminal gate, not a status flip).

## Simplified flow (before → after)

### Before (today): everything is a workflow
```
prompt → TaskCenterEntry → create request/run/sandbox
       → RunController seeds <run_id>:root (synthetic, NO agent)
       → WorkflowStarter.start → root Workflow→Iteration→Attempt
       → flip <run_id>:root RUNNING→WAITING_WORKFLOW
       → Attempt launches PLANNER → DAG of generators+reducers → quiescence
       ... root Workflow closes → on_root_workflow_closed → finish_run
```

### After: agent-first, Task-unified
```
prompt → RequestEntry (task/)
       → create request (+ sandbox);  root_task_id = mint Task(role=root, root_task=True,
                                       workflow_id=None, instruction=prompt, status=RUNNING)
       → task/ runs the root agent loop directly (prompt = task.instruction)
                                       — NO ContextEngine, NO composer
       root agent tools: edit/shell/…, delegate_workflow (non-terminal),
                         check_workflow_status, cancel_workflow, submit_root_outcome (terminal)

       delegate_workflow(goal)                         ← optional; creates Workflow rows
         → workflow/ : Workflow→Iteration→Attempt; planner authors generator/reducer Tasks
           (workflow_id/iteration_id/attempt_id set); ContextEngine builds THEIR packets
         → reducer = exit gate; result delivered back to the launching agent (background)

       submit_root_outcome(status, outcome)            ← terminal
         → task_store.set_task_status(root_task, DONE/FAILED, outcome)
         → request finished (status + result) → USER reads the request result
```

## New class / field detail

### Terminals (`tools/submission/…`)
- **New `submit_root_outcome`** — near-clone of `submit_generator_outcome`; input
  `{status, outcome}`. Handler writes the **root Task** DONE/FAILED + outcome and
  **finishes the request** (status + result) via the task/request store. No
  attempt, no orchestrator. Its outcome **is** the user-facing request result.
- `submit_generator_outcome` / `submit_reducer_outcome` — unchanged in shape;
  route to the `AttemptOrchestrator` (workflow agents only).
- **`submit_workflow_handoff` deleted** (tool dir, `_names` constant, terminals
  registry, factory registration, executor prose, the `DisallowNestedWorkflowHandoff`
  pre-hook + `nested_workflow_handoff_disabled` / `request_workflow_after_edit`
  notification triggers).
- **`delegate_workflow`** — `is_terminal_tool=False`; returns a workflow handle;
  available to root + generator agents (per the delegate_workflow plan).

### Planner authors Tasks (`submissions.py` / `outcomes.py` shrink)
| Removed | Replaced by |
|---|---|
| `PlannedGeneratorTask` | `Task {role=generator, agent_name, needs, instruction}` |
| `PlannedReducerTask` | `Task {role=reducer, needs, instruction}` |
| `PlannerSubmission.generators/reducers` (tuples of the above) | the planner terminal **creates Task rows** + carries `{kind: completes\|defers, deferred_goal}` |
| `PlannerTaskOutcome` / `PlannedTaskRef` | planner outcome = the child Task ids it created; reads go to the Task rows |

Kept: the planner *role* + planner agent; the `plan_dag` structural gate (≥1
reducer + reachability), now validating the **created Task rows** via their
`role` + `needs` columns. A thin tool-input parse resolves the planner's local
`needs` ids → real Task ids at creation (no persistent `Planned*` class).

### Identity / persistence (run → request)
- **`request`** table gains `root_task_id` + `status` (+ `finished_at`); the
  `task_center_runs` table and `task_center_run_id` are **removed**.
- **`task`** table: FK `request_id` (was `task_center_run_id`); **add** columns
  `root_task` (bool), `workflow_id`, `iteration_id`, `attempt_id` (nullable);
  **rename** `context_message` → `instruction`.
- `workflow` / `iteration` / `attempt` tables: `task_center_run_id` → `request_id`.
- This **is** a schema change (decision 1 reverses the earlier "keep on-disk
  names"). `task_center_run_id` is also a serialized dict key + a metadata name
  crossing into `sandbox/…/op_context.py`, `tools/_framework/core/runtime.py`,
  `tools/sandbox/_lib/tool_context.py`, `engine/audit/stream.py`, and
  `task_center_runner` — all rename to `request_id`.

### Launch paths (decision 7)
```
root agent (task/)         initial message = task.instruction
                           → run_ephemeral_agent      (no composer, no ContextEngine)
workflow agent (workflow/) EphemeralAttemptAgentLauncher + AgentLaunchFactory + composer
                           initial messages = ContextEngine context + task guidance
                           → run_ephemeral_agent      (UNCHANGED)
```

## Resulting folder structure

`task/` is a **pure leaf** — only the Task primitive. Entry + run-control are
**not** in `task/`; they are the composition root (`runtime/`) that imports both
`task/` and `workflow/`. There is **no** `root_agent.py` — the root run is just
`engine.run_ephemeral_agent`, invoked by the composition. Strictly one-way:

```text
task/  ◄────────  workflow/  ◄────────  runtime/   (composition: entry + root run + delegation wiring)
(Task primitive)  (decomposition)        imports task/ + workflow/ + engine
```

```text
backend/src/task/                      # PURE LEAF — the Task primitive only (imports nothing upward)
├── __init__.py
├── task.py                            # ★ Task DTO + TaskRole + TaskStatus
├── outcomes.py                        # generic per-task outcome record (workflow projections stay in workflow/)
└── store.py                           # spawn / transition / fetch a Task

backend/src/workflow/                  # delegated decomposition; imports task/ for the Task type
├── __init__.py                        # lazy __getattr__ facade (keeps the db.stores cycle broken)
├── starter.py · lifecycle.py · submissions.py
├── _core/   state.py (Workflow/Iteration/Attempt) · outcomes(projections) · primitives · persistence · invariants · audit · workflow_depth
├── context_engine/                    # context for the workflow ONLY
├── agent_launch/                      # composer, entry_messages (workflow-agent launch)
├── iteration/   attempt_coordinator
└── attempt/   orchestrator · run_stage · plan_dag · launch   (EphemeralAttemptAgentLauncher stays HERE)

backend/src/runtime/                   # COMPOSITION ROOT (was task_center/entry + run_controller)
├── entry.py                           # ← entry/bootstrap.py : create request → mint root Task → run root agent (engine.run_ephemeral_agent)
├── run_controller.py                  # ← run_controller.py : finish_request on submit_root_outcome (no synthetic bootstrap)
└── sandbox_provisioning.py            # ← entry/sandbox_provisioning.py

backend/src/tools/workflow/            # delegate_workflow · check_workflow_status · cancel_workflow (non-terminal)
backend/src/tools/submission/root/     # submit_root_outcome (new) — resolver checks workflow_id IS NULL, finishes request
backend/src/agents/profile/main/root.md  # root agent profile (role: root, terminals: submit_root_outcome)
backend/src/db/models/{request.py,task.py,workflow.py,iteration.py,attempt.py}  # task_center_runs DROPPED
```

**Why this is clean + acyclic.** `workflow/` needs `TaskRole`/`TaskStatus`/`Task`
→ they live in `task/` (a pure leaf importing nothing upward). `runtime/` is the
composition root: it creates the request, mints the root `Task`, runs the root
agent via `engine.run_ephemeral_agent`, wires the workflow runtime
(`AttemptDeps`, launcher, registries, composer) for later `delegate_workflow`
calls, and finishes the request on `submit_root_outcome`. Module DAG:
`task ← workflow ← runtime` — strictly one-way, no facade gymnastics. The
role-aware `EphemeralAttemptAgentLauncher` stays in `workflow/`; the root never
uses it.

## Agent schema (Task vs AgentDefinition vs AgentRun)

"Agent" is **two** existing schemas, both kept — the Task just gains a real
`AgentRun` for the root.

**`AgentDefinition`** (`agents/definition/model.py`, Pydantic; the *static
profile*, loaded from a profile `.md`):
```python
class AgentDefinition(BaseModel):
    name: str; description: str
    system_prompt: str | None = None
    model: str | None = None
    tool_call_limit: int                 # per-run tool-dispatch cap
    role: AgentRole = GENERATOR          # planner|generator|reducer|helper|subagent  (+ ROOT — see below)
    agent_type: AgentType = AGENT        # agent | subagent
    allowed_tools: list[str] = []
    terminals: list[str]                 # non-empty
    notification_triggers: list[str] = []
    skill: Path | None = None
    context_recipe: str | None = None    # ← root profile leaves this None (no ContextEngine)
```

**`AgentRunRecord`** (`db/models/agent_run.py`; the *execution*, **1:1 with a
Task**):
```python
class AgentRunRecord(Base):              # __tablename__ = "agent_runs"
    id: str
    task_id: str                         # FK → tasks.id, UNIQUE  (one agent_run per Task)
    agent_name: str
    initial_messages: JSON               # ★ NEW — the resolved seed the run launched with
                                         #   always [system, …role-specific…]; system = AgentDefinition.system_prompt
    message_history: JSON | None         # the evolving conversation (grows during the run)
    terminal_tool_result: JSON | None
    token_count: int = 0
    error: str | None
    created_at; finished_at
```

**`initial_messages` is per-run and always starts with the system message.** The
*static* system prompt lives on `AgentDefinition.system_prompt`; at launch each
path resolves the runtime seed and persists it here (distinct from the growing
`message_history`). Composition by agent role:

| agent | `initial_messages` |
|---|---|
| **root** | `[system, user_prompt, skill?]` |
| **workflow** (planner/generator/reducer) | `[system, context, task_guidance, skill?]` |
| **subagent** (explorer) | `[system, prompt]` |
| **advisor** (helper) | `[system, parent_transcript, review_request]` — `parent_transcript` = the caller's forwarded history (`ask_helper/_lib/_transcript.build_parent_transcript`); `review_request` = the tool/catalog + the pending terminal submission under review |

`skill?` is present only when `AgentDefinition.skill` is declared (the row-4 skill
+ `terminal_tool_selection` body).

### Unified `AgentEntryMessages`

The current `AgentEntryMessages` is **workflow-shaped** — named fields
`context` / `task_guidance` / `skill` that mean nothing for a root agent, a
subagent, or the advisor. Unify it by dropping the named segments: the class is
just **a system message + an ordered tuple of seed messages**. Every role differs
only in *what fills the seed*, which is the builder's job — so one class serves
all four.

```python
# agents/entry_messages.py  (shared leaf: depends only on Message + AgentDefinition)
@dataclass(frozen=True, slots=True)
class AgentEntryMessages:
    """The resolved launch seed for ANY agent."""
    agent_def: AgentDefinition
    system: str                          # = rendered AgentDefinition.system_prompt
    seed: tuple[Message, ...]            # role-specific user messages, in order (skill appended when declared)

    def to_messages(self) -> list[Message]:
        return [Message.from_system_text(self.system), *self.seed]   # system ALWAYS first
```

One class, four builders — each returns the *same* `AgentEntryMessages`; the
launcher just calls `.to_messages()` and persists it to `AgentRunRecord.initial_messages`:

| builder (lives where its inputs live) | produces `seed =` |
|---|---|
| `build_root_entry(def, user_prompt)` — `runtime/` | `(user_prompt, skill?)` |
| `AgentEntryComposer.compose(def, scope)` — `workflow/` (today's composer, repacked) | `(context, task_guidance, skill?)` |
| `build_subagent_entry(def, prompt)` — `tools/subagent/` | `(prompt,)` |
| `build_advisor_entry(def, transcript, review)` — `tools/ask_helper/` | `(parent_transcript, review_request)` |

Why this is the right unification:
- **The shared shape is genuinely just an ordered message list.** The four roles
  draw from heterogeneous sources (request prompt / ContextEngine / caller prompt
  / caller transcript), so a *common builder* would only fan back out — but a
  *common output type* (`AgentEntryMessages`) is exactly what the launcher +
  `AgentRunRecord` want. Keep one class, keep four builders.
- **`system` is structural, not a seed entry** — encoding the "system always
  first" invariant in `to_messages()` instead of trusting every builder to
  prepend it.
- **`skill` stops being a named field** — it's a universal-optional, so each
  builder appends the skill message to `seed` when `agent_def.skill` is set
  (still rendered by `build_skill_message`). The advisor/subagent simply never
  declare one.
- **Home:** move `AgentEntryMessages` out of `workflow/agent_launch` to
  `agents/entry_messages.py` (next to `AgentDefinition`) — a shared leaf importing
  only `Message` + `AgentDefinition`, so `runtime/`, `workflow/`, and `tools/` all
  import it without a cycle. The workflow-specific `AgentEntryComposer` /
  ContextEngine stay in `workflow/` and just *produce* this shared type.

Relationship in the new model:
```
Task ──.agent_name──▶ AgentDefinition (which agent: prompt/tools/terminals/model/role)
  │                        │ runs via engine.run_ephemeral_agent
  └──────── 1:1 ──────────▶ AgentRunRecord (the execution: messages, tokens, terminal result, error)
```
- Add **`ROOT` to the single `AgentRole`** (the unified enum); `root.md` declares
  `role: root`, `terminals: [submit_root_outcome]`, `context_recipe: null`.
- `agent_runs` gains **exactly one column — `initial_messages`** (the only
  `agent_runs` schema change; include it in the no-Alembic rebuild watchpoint).
  `initial_messages` is the launch-time seed (written at `create`);
  `message_history` is the finish-time transcript (written at `finish`) — not a
  subset at t0; both justified by write-lifecycle + crash durability, not redundancy.
- The root `Task` now produces a **real `AgentRunRecord`** (today the synthetic
  root has none) — otherwise reusing the existing shape. Behavior flip: root now
  has message history / tokens / event stream; verify audit/node-id consumers.
- `agent_runs.task_id` FK retargets `task_center_tasks.id` → the renamed `tasks`
  table; the `Mapped["TaskCenterTaskRecord"]` forward-ref string updates.

## Resulting Workflow / Iteration / Attempt schemas

These are **kept** (only `delegate_workflow` creates them; the root never does).
Two forced changes: (1) `task_center_run_id → request_id`; (2)
`Workflow.parent_task_id` becomes **non-null** (`str | None → str`,
`WorkflowRecord.parent_task_id nullable=False`) — the only null-parent producer
was the synthetic root workflow, now deleted. The `Iteration`/`Attempt` DTO field
names below (`iteration_goal`, `deferred_goal_for_next_iteration`) are reproduced
verbatim from `state.py`; the stores already map them onto the unchanged DB
columns `goal` / `deferred_goal`, so those columns are untouched. The
child-id-list fields (`iteration_ids` / `attempt_ids` /
`generator_task_ids` / `reducer_task_ids`) are **kept** (your call — see note above).

```python
# workflow/_core/state.py
class WorkflowStatus(StrEnum):  OPEN; SUCCEEDED; FAILED; CANCELLED

@dataclass(frozen=True, slots=True)
class Workflow:                          # workflows table; created ONLY by delegate_workflow
    id: str
    request_id: str                      # ← was task_center_run_id (FK → requests)
    parent_task_id: str                  # the launching Task (root agent OR a generator) — durable back-link
    workflow_goal: str
    status: WorkflowStatus
    iteration_ids: tuple[str, ...]       # kept (ordered)
    outcomes: str | None                 # ★ now PERSISTED on close (was derived via workflow_outcomes())
    created_at; updated_at; closed_at

class IterationStatus(StrEnum):          OPEN; SUCCEEDED; FAILED; CANCELLED
class IterationCreationReason(StrEnum):  INITIAL; DEFERRED_GOAL_CONTINUATION

@dataclass(frozen=True, slots=True)
class Iteration:                         # iterations table; UNIQUE(workflow_id, sequence_no)
    id: str
    workflow_id: str                     # FK → workflows
    sequence_no: int
    creation_reason: IterationCreationReason
    iteration_goal: str
    attempt_budget: int
    status: IterationStatus
    attempt_ids: tuple[str, ...]         # kept (ordered)
    deferred_goal_for_next_iteration: str | None
    outcomes: str | None                 # persisted projection (closing attempt's evidence); None while open
    created_at; updated_at; closed_at

class AttemptStage(StrEnum):       PLAN; RUN; CLOSED
class AttemptStatus(StrEnum):      RUNNING; PASSED; FAILED
class AttemptFailReason(StrEnum):  TASK_FAILED; STARTUP_FAILED

@dataclass(frozen=True, slots=True)
class Attempt:                           # attempts table; UNIQUE(iteration_id, attempt_sequence_no)
    id: str
    iteration_id: str                    # FK → iterations
    workflow_id: str                     # ★ denormalized owning workflow (source for stamping Task.workflow_id)
    attempt_sequence_no: int
    stage: AttemptStage                  # PLAN → RUN → CLOSED
    status: AttemptStatus                # PASSED iff every plan task DONE; FAILED if any failed/blocked
    planner_task_id: str | None
    generator_task_ids: tuple[str, ...]  # kept — read directly by the RUN-stage scheduler
    reducer_task_ids: tuple[str, ...]    # kept — reducer is the exit gate
    deferred_goal_for_next_iteration: str | None
    fail_reason: AttemptFailReason | None
    outcomes: tuple[ExecutionTaskOutcome, ...]
    created_at; updated_at; closed_at
```

**Child-id-list fields are KEPT** (`Attempt.generator_task_ids` /
`reducer_task_ids`, `Iteration.attempt_ids`, `Workflow.iteration_ids`). Although
the explicit `Task.{workflow_id,iteration_id,attempt_id}` columns make them
*derivable*, they stay: the RUN-stage scheduler (`run_stage.py`) reads
`attempt.generator_task_ids/reducer_task_ids` directly, and the ordered tuples
are the cheap path for the scheduler + `latest_attempt_id`/`attempt_count`. This
is an accepted denormalization — membership is now writable two ways (the id-list
tuples *and* the `Task.attempt_id` column), and the orchestrator/coordinator keep
their existing `set_generator_task_ids`/`set_reducer_task_ids` bookkeeping. The
new `Task` position columns serve the **root discriminator + position/audit**,
not a scheduler rewrite.

## Resulting DB schema (tables, columns, FKs)

`task_center` prefix dropped from table names; `task_center_runs` dropped;
`task_center_run_id → request_id`. No Alembic — `db/engine.py` rebuilds from
`Base.metadata` (drop-unmodelled + `create_all`), so the rename/drop is a rebuild.
Legend: ★ new · ⊖ dropped · ⟳ changed. Cross-row links that are **plain indexed
columns, app-enforced (NOT declared FKs)** — matching today's `parent_task_id` —
are marked *(soft)*; declared FKs say `FK→…`.

```text
requests   (⟳ was task_center_requests)
  id                PK String(36)
  cwd               String(1024)
  sandbox_id        String(128) NULL
  request_prompt    Text
  root_task_id      String(96) NULL  ★  (soft → tasks.id; set after the root Task is minted — avoids the request↔task insert cycle)
  status            String(32) = 'running'   ★ (migrated from runs)
  created_at; updated_at; finished_at NULL  ★ (finished_at from runs; runs.started_at folds into created_at)

⊖ task_center_runs   — DROPPED ENTIRELY (status/finished_at migrated to requests; 1 run-per-request, so nothing orphaned)

tasks   (⟳ was task_center_tasks)
  id                PK String(96)
  request_id        String(36)  FK→requests.id  ON DELETE CASCADE, indexed   ⟳ (was task_center_run_id → runs)
  role              String(32)        {root|planner|generator|reducer}
  instruction       Text              ⟳ (was context_message)
  status            String(32)        {pending|running|done|failed|blocked}   ⟳ (waiting_workflow removed)
  workflow_id       String(36) NULL, indexed   ★  (soft → workflows.id; NULL ⇔ ROOT — the discriminator)
  iteration_id      String(36) NULL, indexed   ★  (soft → iterations.id)
  attempt_id        String(96) NULL, indexed   ★  (soft → attempts.id)
  agent_name        String(128) NULL
  needs             JSON = []
  outcomes          JSON = []
  terminal_tool_result  JSON NULL
  created_at; updated_at
  ⊖ child_workflow_id   — DROPPED (back-link is workflows.parent_task_id, one-directional)
  rel: agent_run 1:1

agent_runs
  id                PK String(36)
  task_id           String(96)  FK→tasks.id  ON DELETE CASCADE, UNIQUE, indexed   (1:1 with a Task)
  agent_name        String(128)
  initial_messages  JSON         ★  (launch seed; [system, …role-specific…])
  message_history   JSON NULL
  terminal_tool_result  JSON NULL
  token_count       Integer = 0
  error             Text NULL
  created_at; finished_at NULL

workflows   (created ONLY by delegate_workflow)
  id                PK String(36)
  request_id        String(36)  FK→requests.id  ON DELETE CASCADE, indexed   ⟳ (was task_center_run_id → runs)
  parent_task_id    String(96) NOT NULL, indexed   ⟳ (was NULL; soft → tasks.id; the launching Task)
  goal              Text
  status            String            {open|succeeded|failed|cancelled}
  iteration_ids     JSON              (kept)
  outcomes          Text NULL         ★ (persisted projection, set on close; was derived)
  created_at; updated_at; closed_at NULL

iterations
  id                PK String(36)
  workflow_id       String(36)  FK→workflows.id  ON DELETE CASCADE, indexed
  sequence_no       Integer
  creation_reason   String            {initial|deferred_goal_continuation}
  goal              Text
  attempt_budget    Integer
  status            String            {open|succeeded|failed|cancelled}
  attempt_ids       JSON              (kept)
  deferred_goal     Text NULL
  outcomes          Text NULL
  created_at; updated_at; closed_at NULL
  UNIQUE(workflow_id, sequence_no)

attempts
  id                PK String(36)
  iteration_id      String(36)  FK→iterations.id  ON DELETE CASCADE, indexed
  workflow_id       String(36), indexed   ★  (soft → workflows.id; denormalized owning workflow)
  attempt_sequence_no  Integer
  stage             String            {plan|run|closed}
  status            String            {running|passed|failed}
  planner_task_id   String(96) NULL
  generator_task_ids  JSON            (kept — scheduler reads directly)
  reducer_task_ids    JSON            (kept)
  deferred_goal     Text NULL
  fail_reason       String NULL       {task_failed|startup_failed}
  outcomes          JSON
  created_at; updated_at; closed_at NULL
  UNIQUE(iteration_id, attempt_sequence_no)
```

FK / ownership graph after the collapse:
```
requests ──FK──< tasks            (tasks.request_id, CASCADE)
requests ──FK──< workflows        (workflows.request_id, CASCADE)
                  └──FK──< iterations ──FK──< attempts
tasks    ──FK(1:1)── agent_runs   (agent_runs.task_id UNIQUE, CASCADE)
requests ··soft··> tasks          (root_task_id, forward pointer)
tasks    ··soft··> workflows/iterations/attempts   (workflow_id/iteration_id/attempt_id — NULL ⇔ root)
workflows ··soft··> tasks         (parent_task_id — the launching Task)
```
Note the `requests.root_task_id ↔ tasks.request_id` reference is cyclic, so
`root_task_id` is a soft nullable column set right after the root Task is minted
(not a hard FK) — same pattern the codebase already uses for `parent_task_id`.

## Phased plan

### Phase 1 — Task DTO + collapse run into request (schema + model)
- New `task/task.py`: `Task` with `request_id`, `role`, `instruction`,
  position columns (`workflow_id`, `iteration_id`, `attempt_id`).
- DB: drop `task_center_runs`; `request` gains `root_task_id` + `status`; `task`
  FK → `request_id`, add position columns, rename `context_message` → `instruction`.
- Rename `task_center_run_id` → `request_id` across src + tests + the wire
  consumers + `task_center_runner` + `db/models/__init__.py` + `db/stores/__init__.py`
  + the `Mapped["…Record"]` forward-ref strings.
- Replace id-string routing with column reads (`workflow_id IS NULL`, `WHERE attempt_id=`,
  `task.role`).
- Verify: `uv run pytest -q backend/tests/unit_test/test_task_center/test_persistence/`
  (renamed) + a request-lifecycle test.

### Phase 2 — Root agent + `submit_root_outcome` + entry
- `runtime/entry.py` (composition, NOT `task/`): create request → mint root Task
  (`role=root`, `root_task=True`, `instruction=prompt`, RUNNING) → run root agent
  via `engine.run_ephemeral_agent` (no ContextEngine, no `root_agent.py`).
- `root.md` profile (terminals: `submit_root_outcome` only).
- New `submit_root_outcome` tool + handler: write root Task + finish request.
- User result: the consumer reads the **request** (status + root Task outcome)
  instead of the run.
- Delete `RunController.start_root_run` synthetic-bootstrap + `on_root_workflow_closed`.
- Verify: root prompt → real agent; `submit_root_outcome(success)` ⇒ request
  `done` with result; failed ⇒ `failed`; never-submitted ⇒ `failed`.

### Phase 3 — Delete handoff; wire delegate_workflow as non-terminal
- Delete `submit_workflow_handoff` + `DisallowNestedWorkflowHandoff` +
  `nested_workflow_handoff_disabled` / `request_workflow_after_edit`.
- `delegate_workflow` (non-terminal) launches a workflow from a Task (root or
  generator); on close the result is delivered to the launching agent
  (background, per the delegate_workflow plan). Workflows are uniformly
  task-launched — delete the root-workflow / attempt-less-parent special-casing:
  `WorkflowStarter._mark_parent_waiting`/`_restore_or_fail_parent` root branches,
  `WorkflowLifecycle._route_close` None-branch, `run_close_handler` /
  `_no_root_close_handler`.
- Verify: `delegate_workflow` returns a handle; parent stays RUNNING; reducer is
  the exit gate; `plan_dag` ≥1-reducer unchanged.

### Phase 4 — Planner authors Tasks (remove Planned*)
- Remove `PlannedGeneratorTask` / `PlannedReducerTask`; planner terminal creates
  generator/reducer `Task` rows + `{kind, deferred_goal}`.
- `plan_dag` validates the created Task rows (role + needs columns).
- Collapse `PlannerTaskOutcome` / `PlannedTaskRef` to task-id references.
- Verify: `test_lifecycle` plan-submission + `plan_dag` tests.

### Phase 5 — Package split + facades + docs
- Split `task_center` → `task/` + `workflow/` per the tree above; one lazy
  `__init__` facade each (zero eager imports — verified cycle-safe).
- Update all external importers + `task_center_runner` + `pyproject.toml` ruff pin.
- Rename `backend/tests/unit_test/test_task_center/` and `docs/architecture/task_center/`;
  regenerate `search-index.js`.
- **Rewrite the CLAUDE.md invariant** (it currently hard-codes "TaskCenter is the
  persisted control plane … every workflow incl. the root is generator-spawned
  via a synthetic `<run_id>:root` bootstrap"). New text: Task is the unifying
  agent interface; the root request runs a first-class root agent; workflow is an
  optional delegated capability; planner/reducer exist only inside a workflow.

## What stays unchanged
- `engine/query/loop.py`, `engine/agent/lifecycle.py` (`run_ephemeral_agent`),
  terminal-tool enforcement, `tool_call/dispatch.py` — attempt-agnostic already.
- `workflow/attempt/` internals: `AttemptOrchestrator`, `AttemptStageAdvancer`,
  `plan_dag` (≥1 reducer + reachability), `IterationAttemptCoordinator`.
- `Workflow → Iteration → Attempt` DTOs + the recursive `Outcome` algebra.
- The workflow-agent launch path (composer + ContextEngine) — unchanged.

## Risks & watchpoints
- **`task_center_run_id` → `request_id` is now a real schema + wire rename** (no
  keep-verbatim; ~527 occurrences / 46 src files). It is a serialized dict key
  (`_serialize_task`) and a metadata name across sandbox / tools / audit / runner
  — change them together; verify nothing reads the old key.
- **NAME COLLISION — `request_id` is partially occupied.** A *separate* field
  `task_center_request_id` (13 occ) already exists and holds the **workflow id**
  (`AgentLaunch` sets `task_center_request_id = launch.workflow_id`), feeding
  `AuditNode.request_id` and `PipelineReport.request_id`. The rename must scope to
  the **exact token `task_center_run_id`** and must **not** touch
  `task_center_request_id` or the existing `request_id` sink fields — conflating
  them corrupts audit correlation. Decision: rename the metadata *key* that
  producers/consumers read (`runtime.py`, `engine/audit/stream.py`,
  `tool_context.py`, `op_context.py`, `SandboxCaller`), but leave
  `AuditNode`/`PipelineReport` field names alone for now (they sit next to an
  already-taken `request_id`). Also exclude `agent_run_store` /
  `run_tracker`'s `create_run`/`finish_run` — those are the `agent_runs` row, a
  different store.
- **`finish_run`/`get_run` → `finish_request`/`get_request`.** No `finish_request`
  exists today; add it (request-level status + `finished_at`). The only business
  callers are `RunController`; the runner reads `get_run().status` →
  `PipelineReport` — that read path is preserved, only *who writes the finish*
  moves to the `submit_root_outcome` handler.
- **No Alembic in the repo.** Schema is rebuilt via `db/engine.py`
  (`Base.metadata.create_all` + a drop-unmodelled pass). Verify the drop pass
  removes `task_center_runs` and rebuilds `task_center_tasks`/`workflows` with the
  new `request_id` FK + the added `workflow_id`/position columns; existing dev DBs
  need that rebuild path to run.
- **The root task is now a *real* agent.** It produces an `agent_run` row
  (`Task`↔`agent_run` one-to-one), audit/node-id graph entries
  (`task_center_runner/audit/recorder.py`, `node_id.py`), and an event stream.
  Verify nothing assumes "the root has no agent_run" / "every request has exactly
  one root workflow."
- **Adding position columns to Task** must keep `needs` wiring intact (planner
  local-id → real-id resolution at creation).
- **Two writers of the request result** (root `submit_root_outcome`; root agent
  exhaustion ⇒ failed) must both be double-finish-guarded.
- **Module cycle:** keep `task/` free of `workflow/` imports; route delegation
  through the `tools/workflow/` tool, not a package import.
- **Sequencing with delegate_workflow:** deleting `submit_workflow_handoff` and
  the `_route_close` None-branch must land with delegate_workflow's
  background-delivery, so no agent can create a workflow whose close has no
  handler.

## Success criteria
- A request runs a first-class root agent against a root `Task`
  (`workflow_id=None`); no `runs` table, no synthetic bootstrap.
- **Grep gate (no root/child workflow concept survives):** after migration,
  `rg "root_workflow|child_workflow"` returns **0**. Requires deleting
  `on_root_workflow_closed` + `run_close_handler` + `apply_child_workflow_outcome`;
  renaming `start_child_workflow`→`start_delegated_workflow`/`start_background_child`
  and `cancel_child_workflow`→`cancel_workflow`; dropping the `Task.child_workflow_id`
  column (the durable back-link is `Workflow.parent_task_id`, one-directional);
  and renaming `start_root_run` (no "run" — folds into the `runtime/` request
  entry). What remains is the legit delegated construct: `Workflow`,
  `Workflow.parent_task_id`, `workflow_id`, `delegate_workflow`,
  `check_workflow_status`, `cancel_workflow`.
- `submit_root_outcome` finishes the request; the user reads the request result.
- `delegate_workflow` is non-terminal, optional, usable by root + generators, and
  is the only creator of `Workflow` rows.
- `submit_workflow_handoff` and the `Planned*` DTOs are gone.
- Routing reads Task columns (`workflow_id`, `role`, `attempt_id`), not id strings.
- `backend/src/task` and `backend/src/workflow` import cleanly (no cycle, no
  `task_center` references).
- CLAUDE.md describes the Task-unified, agent-first model.
```