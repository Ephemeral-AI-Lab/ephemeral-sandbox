# Implementation Plan: Agent Builder with Database Storage

## Task Type
- [x] Backend (Python / FastAPI / SQLAlchemy)

## Overview

Build an agent builder system inspired by Synthetic OS's `SpecialistDefinition → build_agent()` pattern, but storing agent definitions in PostgreSQL instead of JSON/YAML files. This enables runtime CRUD of agent configurations via API, while coexisting with built-in YAML-loaded agents.

### Key Learnings from Synthetic OS
1. **Declarative definitions** — agents defined as data (JSON), not code
2. **Builder pattern** — `build_agent(defn)` resolves tools/toolkits/skills/MCP at build time
3. **Factory registry** — agents registered as factories (lambdas) that call `build_agent(defn)`
4. **Toolkit context injection** — runtime context passed via `ToolkitContext` dataclass
5. **Validation layer** — validates tools, toolkits, MCPs, skills exist before saving
6. **CRUD API** — full REST endpoints for agent lifecycle
7. **Ephemeral agents** — `build_ephemeral_agent()` for one-shot agents without registry

## Technical Solution

Store agent definitions as a new `AgentDefinitionRecord` table. A builder service converts DB records → `AgentDefinition` Pydantic model → runtime agent. The existing YAML-loaded agents remain as `source="builtin"`, while DB agents are `source="user"`. A unified registry merges both sources.

---

## Implementation Steps

### Step 1: DB Model — `AgentDefinitionRecord`

**File:** `backend/src/db/models/agent_definition.py` (NEW)

```python
class AgentDefinitionRecord(Base):
    __tablename__ = "agent_definitions"

    id: Mapped[str] = mapped_column(String(36), primary_key=True)  # UUID
    name: Mapped[str] = mapped_column(String(128), unique=True, index=True)
    description: Mapped[str] = mapped_column(Text)
    
    # Prompt & behavior
    system_prompt: Mapped[str | None] = mapped_column(Text, nullable=True)
    model: Mapped[str | None] = mapped_column(String(128), nullable=True)
    effort: Mapped[str | None] = mapped_column(String(16), nullable=True)
    permission_mode: Mapped[str | None] = mapped_column(String(32), nullable=True)
    max_turns: Mapped[int | None] = mapped_column(Integer, nullable=True)
    
    # Tools & skills (JSON arrays)
    tools: Mapped[list | None] = mapped_column(JSON, nullable=True)       # ["Read", "Write"] or null=all
    disallowed_tools: Mapped[list | None] = mapped_column(JSON, nullable=True)
    toolkits: Mapped[list | None] = mapped_column(JSON, nullable=True)    # ["daytona", "mcp"]
    skills: Mapped[list] = mapped_column(JSON, default=list)              # ["skill-slug-1"]
    mcp_servers: Mapped[list | None] = mapped_column(JSON, nullable=True)
    
    # Hooks (JSON object)
    hooks: Mapped[dict | None] = mapped_column(JSON, nullable=True)
    
    # UI & lifecycle
    color: Mapped[str | None] = mapped_column(String(16), nullable=True)
    background: Mapped[bool] = mapped_column(default=False)
    initial_prompt: Mapped[str | None] = mapped_column(Text, nullable=True)
    memory: Mapped[str | None] = mapped_column(String(16), nullable=True)
    isolation: Mapped[str | None] = mapped_column(String(16), nullable=True)
    subagent_type: Mapped[str] = mapped_column(String(64), default="general-purpose")
    
    # Metadata & versioning
    version: Mapped[int] = mapped_column(Integer, default=1)
    is_active: Mapped[bool] = mapped_column(default=True)   # soft delete
    created_by: Mapped[str | None] = mapped_column(String(128), nullable=True)
    tags: Mapped[list | None] = mapped_column(JSON, nullable=True)       # ["coding", "research"]
    metadata_json: Mapped[dict | None] = mapped_column("metadata", JSON, nullable=True)
    
    # Timestamps
    created_at: Mapped[datetime]   # default=utcnow
    updated_at: Mapped[datetime]   # default=utcnow, onupdate=utcnow
```

**Expected deliverable:** New model file, imported in `backend/src/db/models/__init__.py` so `Base.metadata.create_all()` picks it up.

---

### Step 2: DB Store — `AgentDefinitionStore`

**File:** `backend/src/db/stores/agent_definition_store.py` (NEW)

```python
class AgentDefinitionStore:
    def __init__(self, session_factory):
        self._session_factory = session_factory

    def create(self, record: AgentDefinitionRecord) -> AgentDefinitionRecord: ...
    def get_by_name(self, name: str) -> AgentDefinitionRecord | None: ...
    def get_by_id(self, id: str) -> AgentDefinitionRecord | None: ...
    def list_active(self, tags: list[str] | None = None,
                    limit: int = 50, offset: int = 0) -> list[AgentDefinitionRecord]: ...
    def update(self, name: str, updates: dict) -> AgentDefinitionRecord: ...
    def soft_delete(self, name: str) -> bool: ...
    def hard_delete(self, name: str) -> bool: ...
    def clone(self, source_name: str, new_name: str) -> AgentDefinitionRecord: ...
    def bump_version(self, name: str) -> AgentDefinitionRecord: ...
```

**Expected deliverable:** Store class with all CRUD methods, version bumping on update.

---

### Step 3: Pydantic Schemas for API

**File:** `backend/src/ui/schemas/agent_schemas.py` (NEW)

```python
class AgentDefinitionCreate(BaseModel):
    """Request body for creating an agent definition."""
    name: str                                    # required, unique
    description: str                             # required
    system_prompt: str | None = None
    model: str | None = None
    effort: str | None = None                    # "low" | "medium" | "high"
    permission_mode: str | None = None
    max_turns: int | None = None
    tools: list[str] | None = None               # None = all tools
    disallowed_tools: list[str] | None = None
    toolkits: list[str] | None = None
    skills: list[str] = []
    mcp_servers: list[Any] | None = None
    hooks: dict[str, Any] | None = None
    color: str | None = None
    background: bool = False
    initial_prompt: str | None = None
    memory: str | None = None
    isolation: str | None = None
    subagent_type: str = "general-purpose"
    tags: list[str] | None = None
    metadata: dict[str, Any] | None = None

class AgentDefinitionUpdate(BaseModel):
    """Partial update — only provided fields are changed."""
    # All fields optional (same as Create but with Optional wrappers)
    ...

class AgentDefinitionResponse(BaseModel):
    """API response for an agent definition."""
    id: str
    name: str
    description: str
    # ... all fields ...
    version: int
    is_active: bool
    created_at: datetime
    updated_at: datetime

class AgentValidationResult(BaseModel):
    """Result of validating an agent definition."""
    valid: bool
    errors: list[str] = []
    warnings: list[str] = []
```

**Expected deliverable:** Request/response models with field validation (effort in EFFORT_LEVELS, color in AGENT_COLORS, etc.)

---

### Step 4: Validation Service

**File:** `backend/src/services/agent_builder/validation.py` (NEW)

```python
class AgentDefinitionValidator:
    def __init__(self, tool_registry: ToolRegistry, skill_registry, toolkit_factories):
        ...

    def validate(self, defn: AgentDefinitionCreate | AgentDefinitionUpdate) -> AgentValidationResult:
        """Validate that referenced tools, skills, toolkits exist."""
        errors = []
        warnings = []

        # 1. Check tool names exist in ToolRegistry
        if defn.tools:
            for t in defn.tools:
                if t != "*" and not self.tool_registry.get(t):
                    errors.append(f"Unknown tool: {t}")

        # 2. Check toolkit names have registered factories
        if defn.toolkits:
            for tk in defn.toolkits:
                if not has_factory(tk):
                    errors.append(f"Unknown toolkit: {tk}")

        # 3. Check skills exist in skill registry
        for s in (defn.skills or []):
            if not self.skill_registry.get(s):
                warnings.append(f"Skill not found: {s}")

        # 4. Validate enum fields
        if defn.effort and defn.effort not in EFFORT_LEVELS:
            errors.append(f"Invalid effort: {defn.effort}")
        if defn.color and defn.color not in AGENT_COLORS:
            errors.append(f"Invalid color: {defn.color}")
        if defn.permission_mode and defn.permission_mode not in PERMISSION_MODES:
            errors.append(f"Invalid permission_mode: {defn.permission_mode}")

        return AgentValidationResult(valid=len(errors) == 0, errors=errors, warnings=warnings)
```

**Expected deliverable:** Validator that checks all references are resolvable.

---

### Step 5: Agent Builder Service

**File:** `backend/src/services/agent_builder/builder.py` (NEW)

This is the core — converts a DB record into a live `AgentDefinition` that the existing system can use.

```python
class AgentBuilderService:
    def __init__(
        self,
        store: AgentDefinitionStore,
        validator: AgentDefinitionValidator,
        tool_registry: ToolRegistry,
    ):
        self._store = store
        self._validator = validator
        self._tool_registry = tool_registry

    def record_to_definition(self, record: AgentDefinitionRecord) -> AgentDefinition:
        """Convert a DB record to the existing AgentDefinition Pydantic model."""
        return AgentDefinition(
            name=record.name,
            description=record.description,
            system_prompt=record.system_prompt,
            tools=record.tools,
            disallowed_tools=record.disallowed_tools,
            model=record.model,
            effort=record.effort,
            permission_mode=record.permission_mode,
            max_turns=record.max_turns,
            skills=record.skills or [],
            mcp_servers=record.mcp_servers,
            hooks=record.hooks,
            color=record.color,
            background=record.background,
            initial_prompt=record.initial_prompt,
            memory=record.memory,
            isolation=record.isolation,
            subagent_type=record.subagent_type,
            source="user",
        )

    def create_agent(self, data: AgentDefinitionCreate) -> AgentDefinitionResponse:
        """Validate + persist + register a new agent definition."""
        result = self._validator.validate(data)
        if not result.valid:
            raise ValueError(f"Validation failed: {result.errors}")

        record = AgentDefinitionRecord(
            id=str(uuid4()),
            **data.model_dump(),
        )
        record = self._store.create(record)

        # Register into runtime
        defn = self.record_to_definition(record)
        self._register_definition(defn)

        return AgentDefinitionResponse.model_validate(record, from_attributes=True)

    def update_agent(self, name: str, data: AgentDefinitionUpdate) -> AgentDefinitionResponse:
        """Validate + update + re-register."""
        updates = data.model_dump(exclude_unset=True)
        if updates:
            result = self._validator.validate(data)
            if not result.valid:
                raise ValueError(f"Validation failed: {result.errors}")
        record = self._store.update(name, updates)
        defn = self.record_to_definition(record)
        self._register_definition(defn)
        return AgentDefinitionResponse.model_validate(record, from_attributes=True)

    def delete_agent(self, name: str) -> bool:
        """Soft-delete + unregister from runtime."""
        ok = self._store.soft_delete(name)
        if ok:
            self._unregister_definition(name)
        return ok

    def clone_agent(self, source_name: str, new_name: str) -> AgentDefinitionResponse:
        """Clone an existing definition under a new name."""
        record = self._store.clone(source_name, new_name)
        defn = self.record_to_definition(record)
        self._register_definition(defn)
        return AgentDefinitionResponse.model_validate(record, from_attributes=True)

    def load_all_from_db(self) -> list[AgentDefinition]:
        """Load all active DB agents into runtime registry (called at startup)."""
        records = self._store.list_active(limit=1000)
        definitions = []
        for rec in records:
            defn = self.record_to_definition(rec)
            self._register_definition(defn)
            definitions.append(defn)
        return definitions

    def _register_definition(self, defn: AgentDefinition) -> None:
        """Add/replace in the global agent definition registry."""
        from ephemeralos.coordinator.agent_definitions import register_definition
        register_definition(defn)

    def _unregister_definition(self, name: str) -> None:
        """Remove from the global agent definition registry."""
        from ephemeralos.coordinator.agent_definitions import unregister_definition
        unregister_definition(name)
```

**Expected deliverable:** Builder service that bridges DB ↔ runtime, with validation.

---

### Step 6: Update Agent Definition Registry

**File:** `backend/src/coordinator/agent_definitions.py` (MODIFY)

Add a mutable runtime registry alongside the existing YAML loader:

```python
# --- Module-level registry ---
_DEFINITIONS: dict[str, AgentDefinition] = {}   # name → definition

def register_definition(defn: AgentDefinition) -> None:
    """Register or replace an agent definition at runtime."""
    _DEFINITIONS[defn.name] = defn

def unregister_definition(name: str) -> bool:
    """Remove an agent definition. Returns True if it existed."""
    return _DEFINITIONS.pop(name, None) is not None

def get_definition(name: str) -> AgentDefinition | None:
    """Look up by name."""
    return _DEFINITIONS.get(name)

def list_definitions(source: str | None = None) -> list[AgentDefinition]:
    """List all registered definitions, optionally filtered by source."""
    defs = list(_DEFINITIONS.values())
    if source:
        defs = [d for d in defs if d.source == source]
    return defs
```

The existing `load_agent_definitions()` function should call `register_definition()` for each YAML-loaded agent, so built-in and user agents share one registry.

**Expected deliverable:** Unified registry functions added to existing module.

---

### Step 7: API Router — `/api/agents`

**File:** `backend/src/ui/routers/agents.py` (NEW)

```python
router = APIRouter(prefix="/api/agents", tags=["agents"])

@router.get("/")
async def list_agents(source: str | None = None, tags: str | None = None):
    """List all agent definitions (built-in + user-created)."""

@router.get("/{name}")
async def get_agent(name: str):
    """Get a single agent definition by name."""

@router.get("/{name}/detail")
async def get_agent_detail(name: str):
    """Get agent with resolved tool/skill metadata."""

@router.post("/", status_code=201)
async def create_agent(body: AgentDefinitionCreate):
    """Create a new agent definition (stored in DB)."""

@router.put("/{name}")
async def update_agent(name: str, body: AgentDefinitionUpdate):
    """Update an existing user-created agent."""

@router.delete("/{name}")
async def delete_agent(name: str):
    """Soft-delete a user-created agent."""

@router.post("/{name}/clone")
async def clone_agent(name: str, new_name: str):
    """Clone an agent under a new name."""

@router.post("/validate")
async def validate_agent(body: AgentDefinitionCreate):
    """Dry-run validation without persisting."""

@router.get("/toolkits/available")
async def list_available_toolkits():
    """List all registered toolkit factory names."""

@router.get("/tools/available")
async def list_available_tools():
    """List all registered tool names."""
```

**Expected deliverable:** New router registered in `web_server.py`.

---

### Step 8: Bootstrap Integration

**File:** `backend/src/ui/runtime.py` or bootstrap equivalent (MODIFY)

During app startup, after YAML agents are loaded:

```python
# In build_runtime() or startup event:
async def startup():
    # 1. Load built-in YAML agents (existing)
    builtin_defs = load_agent_definitions()
    for d in builtin_defs:
        register_definition(d)

    # 2. Load user agents from DB
    if session_factory:
        store = AgentDefinitionStore(session_factory)
        validator = AgentDefinitionValidator(tool_registry, skill_registry, toolkit_factories)
        builder = AgentBuilderService(store, validator, tool_registry)
        db_defs = builder.load_all_from_db()
        logger.info("Loaded %d user agents from DB", len(db_defs))
```

**Expected deliverable:** DB agents loaded at startup, available for agent spawning.

---

### Step 9: Wire into Agent Tool

**File:** `backend/src/tools/agent_tool.py` (MODIFY)

The existing `AgentTool` resolves agent definitions by `subagent_type`. Update to check the unified registry:

```python
# In AgentTool.execute():
defn = get_definition(subagent_type)
if defn is None:
    # fallback to built-in defaults
    ...
```

**Expected deliverable:** Agent spawning works for both built-in and DB-stored agents.

---

## Key Files

| File | Operation | Description |
|------|-----------|-------------|
| `backend/src/db/models/agent_definition.py` | **Create** | New SQLAlchemy model for agent definitions |
| `backend/src/db/models/__init__.py` | Modify | Import new model for DDL |
| `backend/src/db/stores/agent_definition_store.py` | **Create** | CRUD store for agent definitions |
| `backend/src/ui/schemas/agent_schemas.py` | **Create** | Pydantic request/response models |
| `backend/src/services/agent_builder/__init__.py` | **Create** | Package init |
| `backend/src/services/agent_builder/validation.py` | **Create** | Validation service |
| `backend/src/services/agent_builder/builder.py` | **Create** | Core builder service |
| `backend/src/coordinator/agent_definitions.py` | Modify | Add runtime registry functions |
| `backend/src/ui/routers/agents.py` | **Create** | REST API router |
| `backend/src/ui/web_server.py` | Modify | Register new router |
| `backend/src/ui/runtime.py` | Modify | Bootstrap DB agents at startup |
| `backend/src/tools/agent_tool.py` | Modify | Resolve from unified registry |

## Risks and Mitigation

| Risk | Mitigation |
|------|------------|
| Name collision between built-in and user agents | User agents cannot overwrite `source="builtin"` agents; enforce in validation |
| DB unavailable at startup | Graceful fallback — log warning, continue with only built-in agents |
| Stale runtime registry after DB update from another instance | For single-instance: CRUD updates registry inline. For multi-instance: add optional cache-invalidation event |
| Large JSON columns (hooks, mcp_servers) | Use PostgreSQL JSONB for indexing; add size limits in validation |
| Version conflicts on concurrent updates | Use optimistic locking via `version` column (WHERE version = expected) |
| Tool/skill references become invalid after toolkit changes | Validation runs at create/update time; optional re-validation endpoint |

## Architecture Decisions

1. **Reuse `AgentDefinition` Pydantic model** — DB records convert to the same model the YAML loader produces, so all downstream code (AgentTool, swarm, etc.) works unchanged.
2. **Soft delete** — `is_active=False` instead of hard delete, preserving history and allowing recovery.
3. **Version tracking** — `version` column auto-increments on update, enables optimistic concurrency.
4. **Source field** — `source` ("builtin" vs "user") prevents user agents from shadowing built-in ones.
5. **Toolkits as JSON array** — New field not in Synthetic OS's YAML agents; enables composition like `["daytona", "mcp"]`.
6. **Tags** — Lightweight categorization for filtering agents by purpose.
