# Agent Mode System v1

## Overview

A **mode** is a typestate attached to a Task that bounds which tools an agent
may call and which terminal tools end the run. Modes encode commitment: by
entering a secondary mode, the agent declares an intent that is enforced at
the schema level rather than relied upon as a soft prompt instruction.

This system replaces ad-hoc prompt nudges (e.g. system-reminder injections
about "you should plan first") with a registry-driven state machine where:

1. Mode is a single field on the Task: `Task.mode`.
2. Each `(AgentRole, Mode)` pair maps to a fixed `(allowed_tools, terminals,
   entry_tool, briefing)` spec.
3. The tool dispatcher consults the spec on every tool call. Disallowed tools
   produce errored `tool_result` messages, which themselves serve as the
   in-band reminder.
4. Mode entry is a **one-way commitment**. Once a secondary mode is entered,
   the only way out is through that mode's terminal tool(s).

## Design Principles

| Principle | What it means |
|---|---|
| **Mode is data, not prompt** | The system prompt does not change across modes. Mode-specific framing is delivered as the `tool_result` of the entry tool — once — and lives in conversation history thereafter. |
| **One-shot briefing** | No per-turn re-injection of mode reminders. The entry tool's `tool_result` is the briefing; the tool gate's deny messages are the natural reminder when the agent strays. |
| **One-way secondary modes** | `direct → secondary` is a one-time transition. `secondary → direct` and `secondary → other_secondary` are rejected. Exit only via the mode's terminal. |
| **Terminal-tool commitment** | The tool name + schema is the commitment, not free-text declaration. Calling `submit_plan_handoff` *is* the act of handing off; there is no other way to express it. |
| **Schema-enforced articulation** | Where commitments need justification (e.g. partial coverage), the schema requires a non-empty prose field. The model cannot skip the explanation. |
| **Subagents cannot toggle parent mode** | Mode entry tools reject when invoked from a subagent context. Subagents have their own task and their own mode field. |

## Agent Definition

The agent definition is the source of truth for an agent's tool surface,
its supported modes, and the per-mode gating policy. There is **no** global
`MODE_REGISTRY` keyed on `(role, mode)` — each agent owns its own modes
inline. This collapses three things that used to be separate (a flat tool
list, a role label, and a mode registry lookup) into one data structure.

### Reformed shape

```python
class ModeDefinition(BaseModel):
    name: str                                   # e.g. "direct", "plan_for_handoff"
    is_default: bool = False                    # exactly one per agent
    allowed_tools: list[str] = []               # explicit per-mode allowlist
    terminals: list[str] = []                   # terminal tool name(s)
    entry_tool: str | None = None               # None for the default mode
    briefing: str | None = None                 # required iff entry_tool is set


class AgentDefinition(BaseModel):
    # --- identity ---
    name: str
    description: str

    # --- prompt & model ---
    system_prompt: str | None = None
    model: str | None = None
    tool_call_limit: int | None = None

    # --- lifecycle ---
    background: bool = False

    # --- typing ---
    role: str | None = None                     # freeform label (UI / logs)
    agent_type: AgentType = "agent"
    permissions: list[str] = []

    # --- mode-aware tool surface (REPLACES the old flat `tools: list[str]`) ---
    modes: list[ModeDefinition]

    # --- derived (computed_field) ---
    @computed_field
    def default_mode(self) -> ModeDefinition: ...        # the unique is_default=True
    @computed_field
    def modes_by_name(self) -> dict[str, ModeDefinition]: ...
```

### What changes vs. today

| Field | Before | After |
|---|---|---|
| `tools: list[str]` | Flat allowlist; the only tool gate. | **Removed.** Mode-scoped `allowed_tools` replace it. |
| Mode metadata | Lived in a global `MODE_REGISTRY[role, mode]` keyed by a freeform `role` label. | Lives inline on the agent as `modes: list[ModeDefinition]`. The `role` field stays as a freeform UI label only. |
| Entry tool / terminal binding | Looked up by role at dispatch time. | Resolved through `agent_def.modes_by_name[task.mode]`. |
| Default mode | Implicit. | Explicit: exactly one `ModeDefinition` per agent has `is_default=True`. |

### Validation rules (enforced by `AgentDefinition` validators)

1. `modes` is non-empty.
2. Exactly one mode has `is_default=True`. The default mode must have
   `entry_tool=None` and `briefing=None`.
3. Every non-default mode must have a non-empty `entry_tool` and a
   non-empty `briefing`.
4. `terminals` is non-empty for every mode (default modes always include
   `submit_task_completion`; secondary modes always include their own
   `submit_*`).
5. Mode names are unique within the agent.
6. `entry_tool` names are unique across the agent's modes.
7. Each mode's `allowed_tools`, `terminals`, and `entry_tool` names are
   verified against the global tool registry at agent-load time. Unknown tool
   names are a load-time error.

### Worked example: executor and evaluator

```python
EXECUTOR = AgentDefinition(
    name="executor",
    role="executor",
    agent_type="agent",
    modes=[
        ModeDefinition(
            name="direct",
            is_default=True,
            allowed_tools=[
                "read", "write", "bash",
                "enter_plan_for_handoff",
            ],
            terminals=["submit_task_completion"],
        ),
        ModeDefinition(
            name="plan_for_handoff",
            allowed_tools=[
                "read", "grep", "glob", "ls",
                "explore_subagent", "ask_user",
            ],
            terminals=["submit_plan_handoff"],
            entry_tool="enter_plan_for_handoff",
            briefing="...",                        # inline; see Briefing Mechanism
        ),
    ],
    # ... system_prompt, model, etc.
)

EVALUATOR = AgentDefinition(
    name="evaluator",
    role="evaluator",
    agent_type="agent",
    modes=[
        ModeDefinition(
            name="direct",
            is_default=True,
            allowed_tools=[
                "read", "write", "bash",
                "enter_prepare_continue_to_work",
            ],
            terminals=["submit_task_completion"],
        ),
        ModeDefinition(
            name="prepare_continue_to_work",
            allowed_tools=["read", "grep", "glob", "ls", "ask_user"],
            terminals=["submit_continue_work_handoff"],
            entry_tool="enter_prepare_continue_to_work",
            briefing="...",                        # inline; see Briefing Mechanism
        ),
    ],
)
```

**Default modes are explicit.** The executor in `direct` mode names its normal
working tools and the one secondary-mode entry tool it may call. Secondary
terminals are not included in `direct.allowed_tools`, so a terminal from an
unentered mode remains unavailable until the entry tool moves the task into
that mode.

**Symmetry note.** Each agent has exactly one default terminal
(`submit_task_completion`) and one secondary mode whose terminal is
schema-aligned with the mode's purpose. Adding a new agent or new mode is
a `ModeDefinition` literal, not a code path.

## Tool Registry

### Mode-entry tools (non-terminal, read-only)

| Tool | Effect | Failure modes |
|---|---|---|
| `enter_plan_for_handoff` | Sets `Task.mode = plan_for_handoff`. Returns the plan-handoff briefing as `tool_result`. | Rejects from subagent context. Rejects if `Task.mode` is already a secondary mode. Idempotent if already in `plan_for_handoff`. |
| `enter_prepare_continue_to_work` | Sets `Task.mode = prepare_continue_to_work`. Returns the iteration-feedback briefing as `tool_result`. | Same guards as above. |

### Terminal tools

| Tool | Owner mode | Effect |
|---|---|---|
| `submit_task_completion` | both `direct` modes | Marks task DONE with summary; propagates up the `closes_for` chain. |
| `submit_plan_handoff` | `plan_for_handoff` only | TaskCenter validates DAG, materializes child tasks, transitions parent to HANDOFF. Required `handoff_note` articulates coverage and risks. |
| `submit_continue_work_handoff` | `prepare_continue_to_work` only | Re-spawns the executor with the evaluator's gap analysis and feedback as input. |

### Authorization gate

**Where it lives.** The gate runs inside `tools/core/tool_execution.py`,
specifically in `execute_tool_call_streaming`, as a peer to the existing
`_consume_tool_budget_or_reject` check.

**Resolution.** At `QueryContext` construction the engine resolves the
active `ModeDefinition` directly from the agent definition:

```python
mode_def = agent_def.modes_by_name[task.mode]
context.active_mode = mode_def
```

The `ModeDefinition` is stashed on the context as `context.active_mode`.
The dispatcher does not call back into TaskCenter or the agent registry
mid-turn — everything it needs is on the context.

**Decision order**:

```
mode = context.active_mode
if mode is None:
    allow                                     # mode gating disabled (e.g. some subagents)

if tool_name in mode.terminals:
    allow
if tool_name in mode.allowed_tools:
    allow
deny
```

**Deny payload.** A structured `ToolResultBlock(is_error=True)`:

```
"`{tool_name}` not allowed in `{mode.name}` mode.
 Allowed terminals: {sorted(mode.terminals)}.
 Use read/search/explore tools or call a terminal."
```

**Budget interaction.** Not-allowed-in-mode rejections do **not** consume
tool-call budget. The gate runs after the budget check returns "allow" but
before the budget counter is incremented; on deny, the counter is rolled
back (or, equivalently, the gate runs first and the budget check only
fires on allowed calls). Rationale: model drift into an unavailable tool
should not silently shorten a run.

The deny message is the in-band reminder. It fires only on violation, so
context cost scales with drift, not with turn count.

## Workflow

### Executor

```
                          ┌──────────────────────┐
                          │ TaskCenter spawns    │
                          │ task.mode = "direct" │
                          └──────────┬───────────┘
                                     ▼
                          ┌──────────────────────┐
   read/edit/bash/ask ◄───┤  EXECUTOR: direct    ├───► enter_plan_for_handoff
   (loop)                 └──────────┬───────────┘                │
                                     │                            │
                                     │                            ▼
                                     │              ┌─────────────────────────┐
                                     │              │ task.mode set to        │
                                     │              │ plan_for_handoff        │
                                     │              │ tool_result = briefing  │
                                     │              └────────────┬────────────┘
                                     │                           │
                                     │                           ▼
                                     │              ┌────────────────────────┐
                                     │              │ EXECUTOR: plan mode    │
                                     │  read/search │ (one-way)              │── unavailable tool
                                     │  /ask (loop) ├──────────────────────┐ │   → deny message
                                     │              └─────────┬────────────┘ │   (loop back)
                                     │                        │              │
                                     ▼                        ▼              │
                       submit_task_completion        submit_plan_handoff     │
                                     │                        │              │
                                     ▼                        ▼              │
                                  DONE              ┌──────────────────┐     │
                                                    │ DAG valid?       │     │
                                                    └────┬─────┬───────┘     │
                                                         │     │             │
                                                       no      yes           │
                                                         │     │             │
                                                         ▼     ▼             │
                                            error tool_result  TaskCenter    │
                                            (loop) ◄───┐       materializes  │
                                                       │       children;     │
                                                       │       parent → HANDOFF
                                                       └───────────────────────┘
```

### Evaluator

```
                          ┌──────────────────────┐
                          │ TaskCenter spawns    │
                          │ task.mode = "direct" │
                          └──────────┬───────────┘
                                     ▼
                          ┌──────────────────────┐
   read/search/ask  ◄─────┤  EVALUATOR: direct   ├───► enter_prepare_continue_to_work
   (loop)                 └──────────┬───────────┘                │
                                     │                            │
                                     │                            ▼
                                     │              ┌─────────────────────────┐
                                     │              │ task.mode set to        │
                                     │              │ prepare_continue_to_work│
                                     │              │ tool_result = briefing  │
                                     │              └────────────┬────────────┘
                                     │                           │
                                     │                           ▼
                                     │              ┌────────────────────────┐
                                     │              │ EVALUATOR: prepare     │
                                     │  read/search │ mode (one-way)         │── unavailable tool
                                     │  /ask (loop) ├──────────────────────┐ │   → deny message
                                     │              └─────────┬────────────┘ │   (loop back)
                                     │                        │              │
                                     ▼                        ▼              │
                       submit_task_completion         submit_continue_work_handoff │
                                     │                        │              │
                                     ▼                        ▼              │
                            verdict applied;     executor re-spawned with    │
                            propagate up         feedback; parent stays      │
                                                 HANDOFF                     │
                                                                              │
                                                                              ┘
```

## Briefing Mechanism

Each entry tool returns a `tool_result` whose body is the full briefing for
that mode. The briefing covers:

- The mode's purpose and the commitment being made.
- The exhaustive list of allowed tools.
- The exhaustive list of terminals, with one-line guidance per terminal.
- Required input fields on the terminal(s) and what makes them well-formed.
- Explicit statement that the mode is one-way.

The briefing lives in conversation history. It is **not** re-injected per
turn. When the agent later attempts an unavailable tool, the gate's deny
message provides a focused reminder of just the relevant constraints.

This separates two concerns:

- **What is this mode for?** — answered once, at entry, in detail.
- **Why was my last call rejected?** — answered on demand, narrowly, when it
  matters.

## Invariants

1. `Task.mode` is the single source of truth for **which** mode is active.
   No mode-derived state lives anywhere else.
2. `AgentDefinition.modes` is the single source of truth for **what** each
   mode allows. There is no parallel global registry.
3. A task's mode can transition `default → secondary` exactly once per
   agent run. All other transitions are rejected by the entry tool.
4. The set of tools the dispatcher will dispatch on a given turn is
   determined by `agent_def.modes_by_name[task.mode]` resolved at
   `QueryContext` construction and stashed as `context.active_mode`.
   Whether the *model* sees an unavailable tool in its tool list (i.e.
   whether tool filtering also happens at `QueryContext` construction in
   addition to the dispatcher gate) is an engine concern; the dispatcher
   gate is the authoritative enforcement point either way.
5. The system prompt produced by `build_runtime_system_prompt` is identical
   across all modes of a given agent. Mode-specific text appears only in
   the entry tool's `tool_result` (the briefing).
6. Subagent contexts cannot enter or exit modes on the parent task. A
   subagent runs against its own `AgentDefinition` with its own modes,
   set at spawn time by the TaskCenter.

## Failure Modes

| Failure | Detection | Behavior |
|---|---|---|
| Disallowed tool call in any mode | tool gate | Tool returns `is_error=true` with a structured deny message; agent retries. |
| Plan validation fails on `submit_plan_handoff` | TaskCenter validation | Tool returns `is_error=true` with the validation error; agent retries within the same turn or next. Mode is preserved. |
| Turn budget exhausted in a secondary mode without terminal | engine's max-turns handler | Engine kills the run. TaskCenter decides recovery (e.g. re-spawn fresh planner with summarized history). Out of scope here. |
| Subagent invokes mode-entry tool | entry tool's pre-check | Tool returns `is_error=true`; subagent continues without flipping parent mode. |
| Cross-secondary transition attempt | entry tool's pre-check | Tool returns `is_error=true` listing the existing mode's allowed terminals. |

## Out of Scope

- **Engine-level continuation on turn-budget exhaustion.** If a secondary
  mode runs out of turns, the engine needs a recovery path (resummarize and
  re-spawn). The mode system itself does not dictate this policy.
- **Cross-agent mode coordination.** An evaluator's `prepare_continue_to_work`
  feedback influences the executor that gets re-spawned, but the two
  agents' modes are independent. No shared mode state.
- **Additional secondary modes.** The registry supports adding e.g.
  `clarify_for_ask_user` or `escalate_for_handoff_up` as future modes.
  This v1 scopes only `plan_for_handoff` and `prepare_continue_to_work`.

## Migration Notes (informational)

**`AgentDefinition.tools: list[str]` is removed.** Existing agents that
declared a flat tool list migrate by wrapping it in a single default-mode
`ModeDefinition`:

```python
# before
AgentDefinition(name="executor", tools=["read", "write", "bash", ...], ...)

# after
AgentDefinition(
    name="executor",
    modes=[
        ModeDefinition(
            name="direct",
            is_default=True,
            allowed_tools=["read", "write", "bash", ...],
            terminals=["submit_task_completion"],
        ),
    ],
    ...
)
```

The migration is mechanical and can be scripted. Agents with no
secondary-mode requirements only need the single default mode.

**Submission tools.** Existing `submit_full_plan_handoff` and
`submit_partial_plan_handoff` are consolidated into a single
`submit_plan_handoff` with a required `handoff_note` field. The note
articulates coverage and risks; the evaluator validates against
`acceptance_criteria` regardless of executor self-classification, so no
`coverage` enum is needed.

The current `iterate_for_continue_to_work` working name is replaced by
`prepare_continue_to_work` to reflect that the evaluator does not iterate
itself — it prepares feedback that the re-spawned executor will act on.

**Global `MODE_REGISTRY` is removed.** Mode metadata that was previously
keyed on `(role, mode)` now lives inline on each agent. The freeform
`AgentDefinition.role` field is retained for UI/log labeling only.
