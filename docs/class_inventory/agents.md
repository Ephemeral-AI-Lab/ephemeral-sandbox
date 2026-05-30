# Module `agents` — Class Inventory

> Generated class & field reference. Source of truth is the code under
> `backend/src/agents/`. Field/type/default data is extracted directly from
> the AST; one-line purposes come from class docstrings (or, where absent, a
> reviewer summary). This generated inventory is distinct from the hand-curated
> `docs/architecture/` memory layer.

**5 classes across 2 files.**

The `agents` module owns the catalog of agent profiles that the multi-agent system launches: it defines each profile's configuration and loads, registers, and validates those profiles at startup. The core is `AgentDefinition` (a Pydantic model carrying prompt, model, tool-call budget, allowed/terminal tools, context recipe, and skill path) plus the `AgentKind`/`AgentType` enums and the `AgentNotificationRule` Protocol that classify profiles (planner, executor, verifier, evaluator, advisor, explorer) and gate planner-dispatchability. A loader parses `.md` files with YAML frontmatter (prepending the main role contract, resolving skill paths) into definitions held in a process-global registry, and resolved-reference validation cross-checks context recipes while running a `SkillLintError` lint (`scan_skill_file`/`validate_skill_files`) that enforces row-4 skill bodies stay "terminal-silent" so they never restate the row-3 terminal-tool catalog.

## Contents

- **`agents/definition/model.py`** — `AgentType`, `AgentKind`, `AgentNotificationRule`, `AgentDefinition`
- **`agents/skills/loader.py`** — `SkillLintError`

---

## `agents/definition/model.py`

#### `AgentType`  ·  _enum_  ·  bases: `StrEnum`  ·  [L17]

Runtime class of an agent profile.

**Enum members**: `AGENT = 'agent'`, `SUBAGENT = 'subagent'`

#### `AgentKind`  ·  _enum_  ·  bases: `StrEnum`  ·  [L24]

Canonical category of an agent profile.

**Enum members**: `PLANNER = 'planner'`, `EXECUTOR = 'executor'`, `VERIFIER = 'verifier'`, `EVALUATOR = 'evaluator'`, `ADVISOR = 'advisor'`, `EXPLORER = 'explorer'`

#### `AgentNotificationRule`  ·  _protocol_  ·  bases: `Protocol`  ·  decorators: `@runtime_checkable`  ·  [L52]

Runtime notification rule shape consumed by agent definitions.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `body` | `Callable[..., str]` |  |
| `trigger` | `Callable[..., bool]` |  |
| `fire_once` | `bool` |  |

#### `AgentDefinition`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L61]

Full agent definition with all configuration fields.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `description` | `str` |  |
| `system_prompt` | `str \| None` | `None` |
| `model` | `str \| None` | `None` |
| `tool_call_limit` | `int` | `Field(..., gt=0)` |
| `agent_kind` | `AgentKind` | `AgentKind.EXECUTOR` |
| `dispatchable_by_planner` | `bool` | `False` |
| `agent_type` | `AgentType` | `AgentType.AGENT` |
| `allowed_tools` | `list[str]` | `Field(default_factory=list)` |
| `terminals` | `list[str]` | `Field(..., min_length=1)` |
| `notification_triggers` | `list[str]` | `Field(default_factory=list)` |
| `notification_rules` | `list[AgentNotificationRule]` | `Field(default_factory=list)` |
| `skill` | `Path \| None` | `None` |
| `context_recipe` | `str \| None` | `None` |

**Class variables**: `model_config`

<details><summary>Methods (3)</summary>

`_coerce_int`, `_check_terminals`, `_check_notification_triggers`

</details>

---

## `agents/skills/loader.py`

#### `SkillLintError`  ·  _exception_  ·  bases: `ValueError`  ·  [L38]

Raised when a skill file violates the terminal-silence contract.

