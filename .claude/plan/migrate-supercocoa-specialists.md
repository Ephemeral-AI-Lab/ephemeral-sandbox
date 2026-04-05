# Implementation Plan: Migrate SuperCocoa Specialists to EphemeralOS DB

## Task Type
- [x] Backend (Python/FastAPI)

## Context

**Source**: 20 specialist agent definitions in `/Users/yifanxu/machine_learning/LoVC/synthetic-os/.super-cocoa-agents/specialist/*.json`

**Target**: EphemeralOS `agent_definitions` PostgreSQL table, loaded via `AgentBuilderService`

**Requirements**:
1. Remove the 7 existing builtin agents (general-purpose, statusline-setup, claude-code-guide, Explore, Plan, worker, verification)
2. Import all 20 SuperCocoa specialists into DB
3. Specialists load from DB on startup and register into the runtime registry

## Schema Mapping

SuperCocoa JSON → EphemeralOS `AgentDefinitionRecord`:

| SuperCocoa Field | EphemeralOS Field | Transform |
|---|---|---|
| `name` | `name` | Direct |
| `description` | `description` | Direct |
| `instructions` (string[]) | `system_prompt` (Text) | Join with `"\n\n"` |
| `model_key` | `model` | Direct |
| `toolkits` (string[]) | `toolkits` (JSON) | Direct |
| `skills` (string[]) | `skills` (JSON) | Direct |
| `metadata` (object) | `metadata_json` (JSON) | Direct |
| `response_format` | `metadata_json.response_format` | Merge into metadata |
| `kind` | — | Dropped (always "agent") |
| `tools` | — | Dropped (always empty) |
| `mcp` | — | Dropped (always empty) |

**Unmapped EphemeralOS fields** (use defaults):
- `subagent_type` → set to `name` (each specialist routes by its own name)
- `background` → `False`
- `effort` → `None`
- `max_turns` → `None`
- `hooks` → `None`
- `initial_prompt` → `None`
- `tags` → `["supercocoa", "specialist"]`
- `created_by` → `"supercocoa-migration"`

## Specialists to Migrate (20)

### Development (5)
1. **backend-developer** — Python/FastAPI specialist (14 instructions, model: minimax-m27-highspeed)
2. **frontend-developer** — React/TypeScript specialist (13 instructions, model: unset, skills: ui-ux-pro-max)
3. **fullstack-developer** — Full-stack coordinator (12 instructions, model: unset, skills: ui-ux-pro-max)
4. **python-developer** — General Python dev (12 instructions, model: unset, skills: worker-tooling-discipline, shared-sandbox-guardrails)
5. **devops-engineer** — Shell/Docker/CI specialist (11 instructions, model: unset)

### Testing (2)
6. **test-engineer** — pytest/vitest specialist (14 instructions, model: specialist)
7. **e2e-tester** — Code intelligence E2E tester (18 instructions, model: unset)

### Codebase Analysis (3)
8. **codebase-explorer** — Read-only explorer (3 instructions, model: explorer, response_format: json)
9. **codebase-partitioner** — Region partitioner (3 instructions, model: coordinator-no-explorer, response_format: json)
10. **codebase-synthesizer** — Multi-report synthesizer (2 instructions, model: coordinator-no-explorer, response_format: json)

### Planning Pipeline (4)
11. **planning-analyze** — Analyze phase (24 instructions, model: coordinator-no-explorer)
12. **planning-explore** — Explore phase coordinator (7 instructions, model: coordinator-no-explorer)
13. **planning-plan-tasks** — Task graph planner (76 instructions, model: coordinator-no-explorer)
14. **planning-synthesize** — Synthesis phase (9 instructions, model: coordinator-no-explorer)

### SWE-EVO Benchmark (4)
15. **python-developer-sweevo** — SWE-EVO Python dev (15 instructions, model: specialist)
16. **sweevo-planner** — Changelog decomposition planner (50 instructions, model: coordinator-no-explorer)
17. **replanner-sweevo** — Failure-driven replanner (8 instructions, model: specialist)
18. **verifier-sweevo** — Test verification agent (4 instructions, model: specialist)

### Test/Placeholder (2)
19. **wwx** — Placeholder ("xixi" / "you are the best programmer")
20. **yifa** — Placeholder ("asd" / "assistant")

## Implementation Steps

### Step 1: Create the specialist seed data module
**File**: `backend/src/agents/seed.py` (NEW)
- Create a new module that reads all 20 `.json` files from the SuperCocoa directory
- Parse each JSON and transform to `AgentDefinitionRecord` fields using the schema mapping above
- Provide `seed_specialists_from_supercocoa(store: AgentDefinitionStore, source_dir: Path)` function
- Idempotent: skip agents that already exist (check `store.get_by_name()`)
- Returns count of created/skipped agents

```python
# Pseudo-code
def seed_specialists_from_supercocoa(store: AgentDefinitionStore, source_dir: Path) -> tuple[int, int]:
    created, skipped = 0, 0
    for json_path in sorted(source_dir.glob("*.json")):
        data = json.loads(json_path.read_text())
        name = data["name"]
        if store.get_by_name(name) is not None:
            skipped += 1
            continue
        
        # Build metadata merging original metadata + response_format
        metadata = dict(data.get("metadata") or {})
        if data.get("response_format"):
            metadata["response_format"] = data["response_format"]
        
        record = AgentDefinitionRecord(
            id=str(uuid4()),
            name=name,
            description=data["description"],
            system_prompt="\n\n".join(data.get("instructions", [])),
            model=data.get("model_key"),
            toolkits=data.get("toolkits", []),
            skills=data.get("skills", []),
            subagent_type=name,
            tags=["supercocoa", "specialist"],
            metadata_json=metadata or None,
            created_by="supercocoa-migration",
        )
        store.create(record)
        created += 1
    return created, skipped
```

### Step 2: Remove existing builtin agents
**File**: `backend/src/agents/builtins.py` (MODIFY)
- Clear the `_BUILTIN_AGENTS` list to an empty list
- Keep the module structure intact (functions still exist, just return empty)
- Remove all the `_*_SYSTEM_PROMPT` constants and `AgentDefinition(...)` entries
- Keep `get_builtin_agent_definitions()` returning empty list

### Step 3: Update app startup to seed from SuperCocoa
**File**: `backend/src/server/app_factory.py` (MODIFY)
- After DB initialization and builder service creation (line ~202), call the seed function
- Pass the SuperCocoa specialist directory path (configurable via settings or hardcoded relative path)
- Log the seed results

```python
# After _builder_service creation (line ~201):
from ephemeralos.agents.seed import seed_specialists_from_supercocoa
specialist_dir = Path(__file__).resolve().parent.parent.parent.parent.parent / "synthetic-os" / ".super-cocoa-agents" / "specialist"
if specialist_dir.exists():
    created, skipped = seed_specialists_from_supercocoa(agent_definition_store, specialist_dir)
    logger.info("Seeded specialists: %d created, %d skipped", created, skipped)

# Then load all from DB (already exists):
db_agents = _builder_service.load_all_from_db()
```

### Step 4: Register DB agents into runtime registry
**File**: No change needed — `_builder_service.load_all_from_db()` already calls `self._register(defn)` for each DB agent, which puts them in the runtime registry. This existing flow handles it.

## Key Files

| File | Operation | Description |
|------|-----------|-------------|
| `backend/src/agents/seed.py` | Create | New seed module — reads SuperCocoa JSONs, persists to DB |
| `backend/src/agents/builtins.py` | Modify | Remove all 7 builtin agent definitions (empty the list) |
| `backend/src/server/app_factory.py:~195-206` | Modify | Add seed call after DB init, before `load_all_from_db()` |
| `backend/src/agents/db/store.py` | No change | Existing CRUD is sufficient |
| `backend/src/agents/builder/service.py` | No change | `load_all_from_db()` handles runtime registration |
| `backend/src/agents/registry.py` | No change | `initialize_builtin_definitions()` will now register nothing (empty list) |

## Risks and Mitigation

| Risk | Mitigation |
|------|------------|
| SuperCocoa dir not found at runtime | Guard with `if specialist_dir.exists()` — skip seeding gracefully, log warning |
| DB not configured → no agents at all | Log clear warning; consider keeping a minimal fallback or requiring DB |
| Duplicate seeds on restart | Idempotent check via `store.get_by_name()` before creating |
| Large `system_prompt` from 76-instruction planning-plan-tasks | DB column is `Text` (unlimited) — no issue |
| `model_key` values (e.g. "minimax-m27-highspeed", "specialist") may not match EphemeralOS model registry | Store as-is; model resolution happens at runtime and these are just preference hints |
| Toolkits referencing SuperCocoa-specific names (daytona_tools, ci, multi_agent, coordination) | These will fail validation but can be stored; map to EphemeralOS equivalents if needed later |

## SESSION_ID
- CODEX_SESSION: N/A (codeagent-wrapper not available)
- GEMINI_SESSION: N/A (codeagent-wrapper not available)
