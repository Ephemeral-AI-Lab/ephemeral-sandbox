# Agents, Teams, and Customization

## Overview

EphemeralOS agents are configurable runtime units defined through Markdown files under `backend/config`. Teams orchestrate multiple agents in coordinated workflows with persistent task queues, scoped coordination notes, and replanning. This document describes the config surface, loading pipeline, runtime persistence model, and team run lifecycle.

---

## 1. Customization Surface

Agent, team, and skill definitions are customized by editing checked-in config files under `backend/config`.

```
┌─────────────────────────────────┐
│  backend/config/agents/*.md     │
│  backend/config/teams/*.md      │
│  backend/config/skills/*        │
└────────────────┬────────────────┘
                 │ YAML frontmatter + Markdown body
                 ▼
        ┌────────────────────────┐
        │ Agent/Team/Skill models│
        └────────┬───────────────┘
                 │ register_all / loaders
                 ▼
        ┌────────────────────────┐
        │ In-memory registries   │
        └────────┬───────────────┘
                 │ lookup at runtime
                 ▼
        ┌────────────────────────┐
        │ Runtime definitions    │
        └────────────────────────┘
```

### Markdown Format

Agent definitions are written as Markdown files with YAML frontmatter:

```
---
name: developer
description: "Team-mode developer: reads, writes, and edits code."
role: developer
model: inherit
tool_call_limit: 100
tools: ["daytona_shell", "ci_query_symbol", "ci_diagnostics"]
terminal_tools: ["submit_task_success", "request_replan"]
---
# System Prompt (body of the file)
Execute one bounded coding task...
```

**Frontmatter fields:**
- `name`, `description` (required)
- `system_prompt` (optional; overridden by body text)
- `model`, `effort`, `tool_call_limit`
- `tools`, `skills`, `terminal_tools`
- `background`, `role`, `agent_type` (agent | subagent)
- `source` (builtin | user | plugin), capability flags

`terminal_tools` is the runtime source of truth for tools that may end the
agent's loop. For team-mode agents it also determines which submission tools
remain visible after tool registration.

### API Surface

The API can list, fetch, and validate definitions. Mutating definition endpoints are read-only because config files are the source of truth.

```
GET /api/agents
GET /api/agents/{name}
POST /api/agents/validate
GET /api/skills
GET /api/skills/{name}
```

---

## 2. Loader → Registry → Runtime Flow

Three distinct layers orchestrate the transition from config files to runtime execution:

```
┌──────────────────────────────────────────────────────────┐
│                    Config Files                         │
│  ┌────────────────────────┐  ┌──────────────────────┐   │
│  │  backend/config/agents │  │ backend/config/teams │   │
│  └───────────┬────────────┘  └──────────┬───────────┘   │
└──────────────┼───────────────────────────┼──────────────┘
               │ _parse_frontmatter        │ _parse_frontmatter
               ▼                           ▼
┌─────────────────────────────────────────────────────────┐
│  Load Phase                                             │
│  ┌──────────────────────────────┐  ┌──────────────────┐ │
│  │  load_agents_dir             │  │  load_teams_dir  │ │
│  │  Parse YAML frontmatter      │  │  Parse roster    │ │
│  └──────────────┬───────────────┘  └────────┬─────────┘ │
└─────────────────┼────────────────────────────┼──────────┘
                  │ AgentDefinition
                  ▼
┌─────────────────────────────────────────────────────────┐
│  Registry Phase                                         │
│  ┌──────────────────────────────────────────────────┐  │
│  │  AgentRegistry  ─  in-memory map  _DEFINITIONS   │  │
│  └───────────────────────┬──────────────────────────┘  │
└───────────────────────────┼─────────────────────────────┘
                            │ lookup
          ┌─────────────────┼──────────────────┐
          ▼                 ▼                  ▼
┌──────────────────┐ ┌──────────────────┐ ┌──────────────────┐
│  get_definition  │ │ list_definitions │ │    get_role      │
│  Lookup by name  │ │  Filter by src   │ │  Team dispatch   │
└──────────────────┘ └──────────────────┘ └──────────────────┘
```

**Key components:**

- **`AgentLoader`** (`backend/src/agents/loader.py`): Parses Markdown frontmatter via `_parse_frontmatter()` and calls `load_agents_dir()` to scan `backend/config/agents`.

- **`AgentRegistry`** (`backend/src/agents/registry.py`): Single in-memory `_DEFINITIONS` dict holding registered `AgentDefinition` objects from config files. Provides lookup functions: `get_definition(name)`, `find_by_role(role)`, `has_role(agent_name, role)`.

---

## 3. Persistence Model

### Runtime Storage Diagram

```
┌─────────────────────────────────────┐
│             AGENT_RUNS              │
│─────────────────────────────────────│
│  id              PK  string         │
│  agent_name          string         │
│  session_id      FK  string         │
│  parent_task_id      string         │
│  status              string         │
│  created_at          timestamp      │
│  finished_at         timestamp      │
└──────────────────┬──────────────────┘
                   │ assigned_to
                   ▼
┌─────────────────────────────────────┐
│               TASKS                 │
│─────────────────────────────────────│
│  id, team_run_id PK                 │
│  agent_name                         │
│  status                             │
│  spec                               │
│  deps, scope_paths                  │
│  parent_id, root_id, depth          │
│  agent_run_id                       │
└─────────┬───────────────────────────┘
          │ parent-child
          ▼
     ┌────────────┐
     │  (TASKS)   │
     │  sub-tasks │
     └────────────┘
```

**Key tables:**

- Agent, team, and skill definitions are not SQL-backed. They are loaded from `backend/config` into in-memory registries.

- **`team_runs`** (durable): Team execution instances. Tracks `team_definition_id`, `session_id`, `status` (pending | running | succeeded | failed), replan count.

- **`tasks`** (partitioned by `team_run_id`): Task queue for a single team run. Fields: `status` (pending | ready | running | expanded | expanded_awaiting_summary | request_replan | done | failed | cancelled), `agent_name` (assigned worker), `deps` (task IDs), `parent_id` (parent task for expansion), `depth`, `agent_run_id` (link to agent execution), `fired_by_task_id` (for replanner tasks, points to original task).

- **`agent_runs`** (durable): Every individual agent invocation (ephemeral or team). Links task to agent execution via `parent_task_id`.

### Ephemeral vs. Durable State

| Layer | Ephemeral | Durable |
|-------|-----------|---------|
| **Agent Definition** | In-memory registry (`_DEFINITIONS` dict) | `backend/config/agents/*.md` |
| **Team Definition** | In-memory registry | `backend/config/teams/*.md` |
| **Skill Definition** | In-memory registry | `backend/config/skills/*` |
| **Run Execution** | Task graph snapshot | `team_runs`, `tasks`, `agent_runs` rows |
| **Task Notes** | Task Center in-memory cache | Note records in `team_runs` (persisted asynchronously) |

---

## 4. Editing Definitions

Definition changes go through checked-in config files:

```
backend/config/agents/*.md
backend/config/teams/*.md
backend/config/skills/*/SKILL.md
```

**Steps:**

1. Edit the Markdown config file.
2. Restart the runtime.
3. `register_all()` and the skill loaders rebuild in-memory registries from `backend/config`.

---

## 5. Team Run Lifecycle Wiring

Sequence showing a team run from start through task dispatch to completion, integrating loader, registry, and persistence:

```
  User      /api/teams/runs   TeamLoader       TaskStore        Runner        PostgreSQL
             Start Run       +AgentRegistry   Persistence    Dispatch Loop
    │              │               │               │               │               │               │
    │─POST /teams/─▶               │               │               │               │               │
    │  runs         │               │               │               │               │               │
    │  {team_name,  │               │               │               │               │               │
    │   session_id} │               │               │               │               │               │
    │              │──load_team()──▶               │               │               │               │
    │              │               │─get_team_definition(team_name)│               │               │
    │              │               │─resolve_roster()              │               │               │
    │              │               │  agent names → AgentDefinition│               │               │
    │              │               │─validate_agents()             │               │               │
    │              │◀──TeamRun─────│               │               │               │               │
    │              │───────────────────────────────────────────────────────────────────INSERT INTO──▶
    │              │               │               │               │               │  team_runs    │
    │              │───────────────────────────────────────────────create(team_run_id)             │
    │              │               │               │               │  Init task    │               │
    │              │               │               │               │  graph        │               │
    │              │───────────────────────────────restore()───────│               │               │
    │              │               │               │  Load active  │               │               │
    │              │               │               │  task graph   │               │               │
    │              │───────────────────────────────────────────────────────────────start_run()     │
    │              │               │               │               │               │               │
    │              │               │               │               │               │─spawn_ephemeral│
    │              │               │               │               │               │  (planner)    │
    │              │               │               │               │◀──insert_tasks(plan.tasks)────│
    │              │               │               │               │               │──INSERT INTO──▶│
    │              │               │               │               │               │  tasks        │
    │              │               │               │               │               │               │
    │              │               │               │     ┌─────────────────────────────────────┐  │
    │              │               │               │     │  loop until team_run.status terminal │  │
    │              │               │               │     │                                     │  │
    │              │               │               │     │  ──get_ready_tasks()──▶             │  │
    │              │               │               │     │    query status='ready'             │  │
    │              │               │               │     │  ◀──ready task list────             │  │
    │              │               │               │     │  ──dispatch_task(agent, task)       │  │
    │              │               │               │     │  ──spawn_ephemeral_agent(agent)     │  │
    │              │               │               │     │  ──UPDATE tasks SET status='running'──▶│
    │              │               │               │     │                                     │  │
    │              │               │               │     │  [if task succeeds]                 │  │
    │              │               │               │     │  ──register_snapshot(task_id, msgs)─▶  │
    │              │               │               │     │  ──UPDATE tasks SET status='done'─────▶│
    │              │               │               │     │                                     │  │
    │              │               │               │     │  [if replan triggered]              │  │
    │              │               │               │     │  ──spawn_ephemeral_agent(replanner) │  │
    │              │               │               │     │  ──update_plan_tasks(add, cancel)──▶│  │
    │              │               │               │     │  ──INSERT (new) + UPDATE (cancel)──────▶│
    │              │               │               │     └─────────────────────────────────────┘  │
    │              │               │               │               │               │               │
    │              │               │               │               │               │─UPDATE team_runs▶
    │              │               │               │               │               │  status=      │
    │              │               │               │               │               │  'succeeded'  │
    │◀─200 OK───────               │               │               │               │               │
    │  TeamRunResponse             │               │               │               │               │

  NOTE: Registry lookup is O(1) after lazy load.
  NOTE: In-memory task graph syncs from DB on each iteration.
  NOTE: All state writes go through persistence layer.
```

**Key interactions:**

- **Loader** resolves team roster by looking up each agent in `AgentRegistry`.
- **AgentRegistry** is lazily populated from disk (Markdown) and DB on first access.
- **TaskStore** maintains in-memory task graph synced from DB via `refresh_graph()`.
- **Runner** dispatches tasks via `spawn_ephemeral_agent()`, updating DB after each step.
- **AgentRunTracker** creates/finishes agent run records for every agent execution.

---

## 6. Agent Configuration Summary

**Customization knobs per agent:**

| Field | Impact | Example |
|-------|--------|---------|
| `system_prompt` | Core behavioral instruction | Markdown body or API field |
| `model` | LLM selection; "inherit" uses default | "claude-opus-4", "inherit" |
| `effort` | Heuristic budget; low/medium/high | High = larger tool_call_limit |
| `tool_call_limit` | Max tool calls before agent stops | 50, 100, unlimited (None) |
| `tools` | Tool names to register for the agent | ["daytona_shell", "ci_query_symbol"] |
| `skills` | Skill playbooks to inject | ["team-developer-playbook"] |
| `role` | Team dispatch label (planner, developer, reviewer) | "developer" |
| `agent_type` | agent \| subagent (capability flag) | "agent" |
| `can_spawn_subagents` | Whether agent can spawn background work | true (default) |
| `background` | Run without awaiting completion | false (default) |
| `initial_prompt` | First-turn user message override | "Start by reading..." |

---

## 7. Team Configuration Summary

**Customization via Markdown:**

```yaml
---
name: my_team
description: "Coordinated coding team"
entry_planner: team_planner
roster:
  planner: [team_planner]
  developer: [developer]
  reviewer: [validator, scout]
---
This team coordinates...
```

**Core fields:**

- `name`: Team identifier
- `entry_planner`: Agent name that receives the initial goal
- `roster`: Mapping of role → list of agent names. Replanners can be dynamically selected by role.

---

## 8. Key Types & Classes

### Agents Module

- **`AgentDefinition`** (`types.py`): Full runtime agent config (Pydantic model).
- **`AgentLoader`** (`loader.py`): Parses Markdown, loads from disk.
- **`AgentRegistry`** (`registry.py`): In-memory lookup map.
- **`AgentRunTracker`** (`run_tracker.py`): Wraps agent execution lifecycle.

### Teams Module

- **`TeamDefinition`** (`models.py`): Roster + entry planner.
- **`TeamLoader`** (`loader.py`): Parses Markdown, loads team definitions.
- **`TeamRegistry`** (`registry.py`): In-memory lookup.
- **`Task`** / **`TaskSpec`** (`models.py`): Execution units and plan items.
- **`TaskStore`** (`persistence/task_store.py`): SQL persistence for task queue.
- **`TeamRun`** (`runtime/team_run.py`): Orchestrates a single team execution.

---

## 9. Configuration Directories

Builtin agent, team, and skill definitions live in:

```
backend/config/agents/
  ├── developer.md
  ├── root_planner.md
  ├── parent_summarizer.md
  ├── validator.md
  ├── team_planner.md
  ├── team_replanner.md
  └── scout.md

backend/config/teams/
  ├── sweevo_benchmark.md
  └── ...

backend/config/skills/
  ├── team-developer-playbook/
  ├── team-planner-playbook/
  ├── team-replanner-playbook/
  ├── team-root-planner-playbook/
  ├── team-scout-playbook/
  └── team-validator-playbook/
```

Definition APIs are read-only; edit these files to change agent, team, or skill definitions.

---

## Summary

**Customization** flows through Markdown frontmatter under `backend/config`. **Loading** parses disk files into runtime definition objects, which populate in-memory registries. **Runtime** lookups are O(1) after registration. **Teams** compose agents by role and use a persistent task queue backed by PostgreSQL. Definition persistence is file-backed, while run/task persistence remains database-backed.
