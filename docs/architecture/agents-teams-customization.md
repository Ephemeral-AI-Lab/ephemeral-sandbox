# Agents, Teams, and Customization

## Overview

EphemeralOS agents are customizable runtime units defined through Markdown files or API endpoints. Teams orchestrate multiple agents in coordinated workflows with persistent task queues, task notes, and replanning. This document describes the customization surface, loading pipeline, persistence model, and team run lifecycle.

---

## 1. Customization Surface

Agents are customized via two complementary paths: **Markdown frontmatter** (for builtin agents and disk-based definitions) and **REST API** (for user-created agents stored in the database).

```
┌─────────────────────────────────┐        ┌─────────────────────────┐
│  Markdown Agent Definitions     │        │  REST API               │
│  backend/src/prompt/agents/*.md     │        │  POST /api/agents       │
└────────────────┬────────────────┘        └───────────┬─────────────┘
                 │ YAML frontmatter + body              │ JSON payload
                 ▼                                      ▼
        ┌─────────────────┐               ┌────────────────────────┐
        │ AgentDefinition │               │  AgentDefinitionCreate │
        └────────┬────────┘               └───────────┬────────────┘
                 │                                     │ validation
                 │                                     ▼
                 │                        ┌────────────────────────┐
                 │                        │  AgentBuilderService   │
                 │                        └───────────┬────────────┘
                 │                                     │ insert/update
                 │                                     ▼
                 │                        ┌────────────────────────────┐
                 │                        │  AgentDefinitionRecord     │
                 │                        │  SQL: agent_definitions    │
                 │                        └───────────┬────────────────┘
                 │ loader                              │ builder
                 └──────────────┬──────────────────────┘
                                ▼
                    ┌─────────────────────┐
                    │    AgentRegistry    │
                    └──────────┬──────────┘
                               │ lookup at runtime
                               ▼
                    ┌─────────────────────┐
                    │   AgentDefinition   │
                    │     (in memory)     │
                    └─────────────────────┘
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
toolkits: ["sandbox_operations", "code_intelligence"]
allowed_tools: ["daytona_shell", "ci_query_symbol", "ci_diagnostics"]
blocked_tools: []
terminal_tools: ["submit_task_success", "request_replan"]
---
# System Prompt (body of the file)
Execute one bounded coding task...
```

**Frontmatter fields:**
- `name`, `description` (required)
- `system_prompt` (optional; overridden by body text)
- `model`, `effort`, `tool_call_limit`
- `toolkits`, `skills`, `allowed_tools`, `blocked_tools`, `terminal_tools`
- `background`, `role`, `agent_type` (agent | subagent)
- `source` (builtin | user | plugin), capability flags

`terminal_tools` is the runtime source of truth for tools that may end the
agent's loop. For team-mode agents using the `submission` toolkit, it also
determines which submission tools remain visible after toolkit assembly.

### REST API Surface

**Create custom agent:**
```
POST /api/agents
{
  "name": "my-researcher",
  "description": "...",
  "model": "claude-opus",
  "system_prompt": "...",
  "toolkits": ["search", "note_taking"],
  "allowed_tools": ["search_web", "read_note"],
  "blocked_tools": [],
  "terminal_tools": [],
  "effort": "high",
  "tool_call_limit": 50
}
```

**Update agent:**
```
PATCH /api/agents/{name}
{
  "system_prompt": "...",
  "toolkits": ["new_toolkit"]
}
```

**List & get:**
```
GET /api/agents?source=user
GET /api/agents/{name}
```

---

## 2. Loader → Registry → Runtime Flow

Three distinct layers orchestrate the transition from disk/database to runtime execution:

```
┌──────────────────────────────────────────────────────────┐
│                    Disk & Database                       │
│  ┌────────────────────────┐  ┌──────────────────────┐   │
│  │  Builtin Agent         │  │  Agent Definition    │   │
│  │  Markdown Files        │  │  Database Records    │   │
│  └───────────┬────────────┘  └──────────┬───────────┘   │
└──────────────┼───────────────────────────┼───────────────┘
               │ _parse_frontmatter        │ seed_builtin /
               ▼                           │ load_all_from_db
┌──────────────────────────────────────────┼───────────────┐
│  Load Phase                              │               │
│  ┌──────────────────────────────┐        ▼               │
│  │  load_agents_dir             │  ┌──────────────────┐  │
│  │  Parse YAML frontmatter      │  │ AgentBuilderSvc  │  │
│  └──────────────┬───────────────┘  │ record_to_defn   │  │
│                 │ AgentDefinition   └────────┬─────────┘  │
│                 ▼ .model_validate            │            │
│  ┌──────────────────────────────┐           │            │
│  │  load_external_agents        │◀──────────┘            │
│  │  User + plugin agents        │                        │
│  └──────────────┬───────────────┘                        │
└─────────────────┼──────────────────────────────────────  ┘
                  │ AgentDefinition
                  ▼
┌─────────────────────────────────────────────────────────┐
│  Registry Phase                                         │
│  ┌──────────────────────────────────────────────────┐  │
│  │  AgentRegistry  ─  in-memory map  _DEFINITIONS   │  │
│  └───────────────────────┬──────────────────────────┘  │
└───────────────────────────┼─────────────────────────────┘
                            │ lazy load on first access
          ┌─────────────────┼──────────────────┐
          ▼                 ▼                  ▼
┌──────────────────┐ ┌──────────────────┐ ┌──────────────────┐
│  get_definition  │ │ list_definitions │ │    get_role      │
│  Lookup by name  │ │  Filter by src   │ │  Team dispatch   │
└──────────────────┘ └──────────────────┘ └──────────────────┘
```

**Key components:**

- **`AgentLoader`** (`backend/src/agents/loader.py`): Parses Markdown frontmatter via `_parse_frontmatter()`, calls `load_agents_dir()` to scan disk, calls `load_external_agents()` to gather user/plugin definitions.

- **`AgentRegistry`** (`backend/src/agents/registry.py`): Single in-memory `_DEFINITIONS` dict holding all registered `AgentDefinition` objects. Lazily loads external agents on first `get_definition()` or `list_definitions()` call. Provides lookup functions: `get_definition(name)`, `find_by_role(role)`, `has_role(agent_name, role)`.

- **`AgentBuilderService`** (`backend/src/agents/builder/service.py`): Converts database `AgentDefinitionRecord` ↔ runtime `AgentDefinition` via `record_to_definition()`. Seeds builtin definitions into DB via `seed_builtin()`.

---

## 3. Persistence Model

### Entity-Relationship Diagram

```
┌─────────────────────────────────────┐
│           AGENT_DEFINITIONS         │
│─────────────────────────────────────│
│  id              PK  string         │
│  name            UK  string         │
│  description         string         │
│  system_prompt       string         │
│  model               string         │
│  effort              string         │
│  tool_call_limit     int            │
│  toolkits            json           │
│  skills              json           │
│  allowed_tools       json           │
│  blocked_tools       json           │
│  hooks               json           │
│  background          boolean        │
│  role                string         │
│  agent_type          string         │
│  can_spawn_subagents boolean        │
│  require_fresh_client boolean       │
│  source              string         │
│  version             int            │
│  is_active           boolean        │
│  created_at          timestamp      │
│  updated_at          timestamp      │
└──────────────────┬──────────────────┘
                   │ spawns (1 to 0..*)
                   ▼
┌─────────────────────────────────────┐
│             AGENT_RUNS              │
│─────────────────────────────────────│
│  id              PK  string         │
│  agent_name      FK  string         │
│  session_id      FK  string         │
│  parent_task_id  FK  string         │
│  status              string         │
│  created_at          timestamp      │
│  finished_at         timestamp      │
└──────────────────┬──────────────────┘
                   │ assigned_to (0..1 to 1)
                   │
                   ▼
┌─────────────────────────────────────┐      ┌──────────────────────────────────┐
│               TASKS                 │      │         TEAM_DEFINITIONS         │
│─────────────────────────────────────│      │──────────────────────────────────│
│  id              PK  string         │      │  id           PK  string         │
│  team_run_id     FK  string         │      │  name         UK  string         │
│  agent_name      FK  string         │      │  description      string         │
│  status              string         │      │  planner_agent FK string         │
│  objective           string         │      │  worker_agents    json           │
│  description         string         │      │  roster           json           │
│  deps                json           │      │  created_at       timestamp      │
│  scope_paths         json           │      │  updated_at       timestamp      │
│  parent_id       FK  string         │      │                                  │
│  root_id             string         │      └────────────────┬─────────────────┘
│  depth               int            │                       │ starts (1 to 0..*)
│  agent_run_id    FK  string         │      ┌──────────────────────────────────┐
│  created_at          timestamp      │      │           TEAM_RUNS              │
│  started_at          timestamp      │      │──────────────────────────────────│
│  finished_at         timestamp      │◀─────│  id           PK  string         │
└─────────┬───────────────────────────┘      │  team_def_id  FK  string         │
          │ parent-child (self FK)           │  session_id   FK  string         │
          └──────────┐                       │  status           string         │
                     ▼ (Tasks.parent_id)     │  replan_count     int            │
                                            │  created_at       timestamp      │
                                            │  finished_at      timestamp      │
              ┌────────────┐                 └──────────────────────────────────┘
              │  (TASKS)   │
              │  sub-tasks │
              └────────────┘
```

**Key tables:**

- **`agent_definitions`** (durable): User-created agents, seeded builtin definitions. Tracks `source` (user | builtin | plugin), `version`, `is_active`.

- **`team_definitions`** (durable): Team rosters mapping roles to agent names. `planner_agent` is the entry point; `worker_agents` list eligible task executors. Mirrors legacy `roster` JSON for backward compatibility.

- **`team_runs`** (durable): Team execution instances. Tracks `team_definition_id`, `session_id`, `status` (pending | running | succeeded | failed), replan count.

- **`tasks`** (partitioned by `team_run_id`): Task queue for a single team run. Fields: `status` (pending | ready | running | expanded | expanded_awaiting_summary | request_replan | done | failed | cancelled), `agent_name` (assigned worker), `deps` (task IDs), `parent_id` (parent task for expansion), `depth`, `agent_run_id` (link to agent execution), `fired_by_task_id` (for replanner tasks, points to original task).

- **`agent_runs`** (durable): Every individual agent invocation (ephemeral or team). Links task to agent execution via `parent_task_id`.

### Ephemeral vs. Durable State

| Layer | Ephemeral | Durable |
|-------|-----------|---------|
| **Agent Definition** | In-memory registry (`_DEFINITIONS` dict) | Database table `agent_definitions` |
| **Team Definition** | In-memory registry | Database table `team_definitions` |
| **Run Execution** | Task graph snapshot | `team_runs`, `tasks`, `agent_runs` rows |
| **Task Notes** | Task Center in-memory cache | Note records in `team_runs` (persisted asynchronously) |

---

## 4. Creating a Custom Agent via API

Sequence showing the builder service integrating user input with database persistence:

```
  User            /api/agents      AgentBuilderSvc   AgentDefnStore    PostgreSQL       AgentRegistry
REST Client         Router                                            agent_definitions   in-memory
    │                 │                  │                 │                 │                │
    │─POST /api/agents─▶                 │                 │                 │                │
    │  {name,model,    │                 │                 │                 │                │
    │   system_prompt} │                 │                 │                 │                │
    │                 │──create_agent()──▶                 │                 │                │
    │                 │                  │                 │                 │                │
    │                 │                  │─AgentDefinitionCreate.validate()  │                │
    │                 │                  │─_record_payload_from_request()    │                │
    │                 │                  │                 │                 │                │
    │                 │                  │──insert(name, payload)──▶         │                │
    │                 │                  │                 │──INSERT INTO─────▶               │
    │                 │                  │                 │  agent_definitions               │
    │                 │                  │                 │◀─AgentDefinitionRecord───────────│
    │                 │                  │◀────record──────│                 │                │
    │                 │                  │                 │                 │                │
    │                 │                  │─record_to_definition(record)      │                │
    │                 │◀─AgentDefinition─│                 │                 │                │
    │                 │                  │                 │                 │                │
    │                 │─────────────────────────────────────────────────register_definition()─▶
    │                 │                  │                 │                 │  _DEFINITIONS  │
    │                 │                  │                 │                 │  [name] = defn │
    │◀─200 OK─────────│                  │                 │                 │                │
    │  AgentDefinition│                  │                 │                 │                │
    │  Response       │                  │                 │                 │                │

  NOTE: Validation includes effort levels, model keys, and toolkit names.
  NOTE: Next get_definition(name) lookup returns immediately from registry.
```

**Steps:**

1. **API** receives `AgentDefinitionCreate` payload.
2. **Validation** checks effort levels, model keys, toolkit names.
3. **Builder** converts payload → `_record_payload_from_request()`.
4. **Store** inserts `AgentDefinitionRecord` into DB.
5. **Builder** converts record back → `AgentDefinition` via `record_to_definition()`.
6. **Registry** stores `AgentDefinition` in `_DEFINITIONS` map.
7. **Response** sent to user; registry is now hot for lookups.

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
| `toolkits` | Allowed tool groups (sandbox, code_intelligence, search) | ["sandbox_operations", "code_intelligence"] |
| `allowed_tools` | Optional allowlist inside the assembled toolkits | ["daytona_shell", "ci_query_symbol"] |
| `blocked_tools` | Tool names to remove after assembly | [] |
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
- **`AgentDefinitionRecord`** (`db/model.py`): SQLAlchemy ORM row (durable).
- **`AgentBuilderService`** (`builder/service.py`): Converts records ↔ definitions.
- **`AgentDefinitionStore`** (`db/store.py`): CRUD on `agent_definitions` table.
- **`AgentLoader`** (`loader.py`): Parses Markdown, loads from disk.
- **`AgentRegistry`** (`registry.py`): In-memory lookup map.
- **`AgentRunTracker`** (`run_tracker.py`): Wraps agent execution lifecycle.

### Teams Module

- **`TeamDefinition`** (`models.py`): Roster + entry planner.
- **`TeamDefinitionRecord`** (`persistence/model.py`): SQLAlchemy ORM row.
- **`TeamLoader`** (`loader.py`): Parses Markdown, loads team definitions.
- **`TeamRegistry`** (`registry.py`): In-memory lookup.
- **`Task`** / **`TaskSpec`** (`models.py`): Execution units and plan items.
- **`TaskStore`** (`persistence/task_store.py`): SQL persistence for task queue.
- **`TeamRun`** (`runtime/team_run.py`): Orchestrates a single team execution.

---

## 9. Configuration Directories

Builtin agent and team definitions live in:

```
backend/src/prompt/agents/
  ├── developer.md
  ├── validator.md
  ├── team_planner.md
  ├── team_replanner.md
  └── scout.md

backend/config/teams/
  ├── default_team.md
  └── ...
```

User-created agents are:
- **Defined via API** → stored in `agent_definitions` table
- **Loaded at startup** via `AgentLoader.load_external_agents()` → stored in `AgentRegistry`

---

## Summary

**Customization** flows through Markdown frontmatter (builtin) and REST API (user-created) into a unified database. **Loading** parses disk files and DB records into `AgentDefinition` objects, which populate the in-memory `AgentRegistry`. **Runtime** lookups are O(1) after lazy initialization. **Teams** compose agents by role and use a persistent task queue backed by PostgreSQL. **Persistence** separates ephemeral state (in-memory graphs) from durable state (DB tables), enabling crash recovery via `TaskStore.refresh_graph()`.
