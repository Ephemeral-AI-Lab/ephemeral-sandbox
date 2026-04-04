# Implementation Plan: Group Tools into Toolkits

## Task Type
- [x] Backend (Python)

## Background

**Current EphemeralOS state**: Flat `ToolRegistry` with ~40 individual `BaseTool` instances. No grouping or toolkit concept. All tools registered one-by-one in `create_default_tool_registry()`.

**Synthetic-OS reference**: Two-level architecture:
1. `ToolRegistry` — holds both individual tools (`_tools`) and toolkit bundles (`_toolkits`)
2. `ToolkitFactory` — factory pattern with `ToolkitContext` for context-aware, stateful toolkit instantiation at agent build time
3. Toolkits extend `agno.tools.Toolkit`, bind `@tool()`-decorated functions, define `PROMPT_VISIBLE_TOOL_NAMES`
4. Agent definitions reference toolkits by name (e.g., `"toolkits": ["daytona_tools", "ci"]`)

**Key difference**: EphemeralOS does NOT use agno. We need our own `BaseToolkit` abstraction that wraps multiple `BaseTool` instances.

---

## Technical Solution

Introduce a lightweight `BaseToolkit` class that groups related `BaseTool` instances into named collections. Extend `ToolRegistry` with toolkit awareness. Add a `ToolkitFactory` for context-aware instantiation. Reorganize existing tools into logical toolkit directories.

### Proposed Toolkit Groupings

| Toolkit Name | Tools | Rationale |
|---|---|---|
| `filesystem` | FileReadTool, FileWriteTool, FileEditTool, NotebookEditTool, GlobTool, GrepTool | Core file operations |
| `execution` | BashTool | Shell execution |
| `web` | WebFetchTool, WebSearchTool | Internet access |
| `task_management` | TaskCreateTool, TaskGetTool, TaskListTool, TaskUpdateTool, TaskStopTool, TaskOutputTool | Task tracking |
| `scheduling` | CronCreateTool, CronListTool, CronDeleteTool, CronToggleTool | Cron jobs |
| `worktree` | EnterWorktreeTool, ExitWorktreeTool | Git worktree isolation |
| `planning` | EnterPlanModeTool, ExitPlanModeTool, TodoWriteTool | Plan mode tools |
| `collaboration` | AgentTool, SendMessageTool, TeamCreateTool, TeamDeleteTool, AskUserQuestionTool | Multi-agent & user interaction |
| `mcp` | McpAuthTool, ListMcpResourcesTool, ReadMcpResourceTool, (dynamic McpToolAdapters) | MCP integration |
| `code_analysis` | LspTool | Language server |
| `discovery` | SkillTool, ToolSearchTool | Tool/skill lookup |
| `system` | ConfigTool, BriefTool, SleepTool, RemoteTriggerTool | System/config utilities |

---

## Implementation Steps

### Step 1: Add `BaseToolkit` class — `src/ephemeralos/tools/base.py`

Add to existing `base.py`:

```python
class BaseToolkit:
    """Named collection of related tools."""
    
    name: str
    description: str
    
    def __init__(self, name: str, description: str, tools: list[BaseTool] | None = None):
        self.name = name
        self.description = description
        self._tools: dict[str, BaseTool] = {}
        for tool in (tools or []):
            self.register(tool)
    
    def register(self, tool: BaseTool) -> None:
        self._tools[tool.name] = tool
    
    def get(self, name: str) -> BaseTool | None:
        return self._tools.get(name)
    
    def list_tools(self) -> list[BaseTool]:
        return list(self._tools.values())
    
    def tool_names(self) -> list[str]:
        return list(self._tools.keys())
```

**Expected deliverable**: `BaseToolkit` class in `base.py`, exported from `__init__.py`

### Step 2: Extend `ToolRegistry` with toolkit support — `src/ephemeralos/tools/base.py`

Modify existing `ToolRegistry`:

```python
class ToolRegistry:
    def __init__(self) -> None:
        self._tools: dict[str, BaseTool] = {}
        self._toolkits: dict[str, BaseToolkit] = {}
    
    # ... existing methods unchanged ...
    
    def register_toolkit(self, toolkit: BaseToolkit) -> None:
        """Register a toolkit and all its tools."""
        self._toolkits[toolkit.name] = toolkit
        for tool in toolkit.list_tools():
            self._tools[tool.name] = tool  # tools still individually accessible
    
    def get_toolkit(self, name: str) -> BaseToolkit | None:
        return self._toolkits.get(name)
    
    def list_toolkits(self) -> list[BaseToolkit]:
        return list(self._toolkits.values())
```

**Key design**: Registering a toolkit also registers its tools individually — backward compatible with existing code that looks up tools by name.

**Expected deliverable**: Extended `ToolRegistry` with toolkit methods

### Step 3: Create toolkit directory structure — `src/ephemeralos/toolkits/`

```
src/ephemeralos/toolkits/
├── __init__.py                  # create_default_toolkits() + exports
├── filesystem_toolkit.py        # FilesystemToolkit
├── execution_toolkit.py         # ExecutionToolkit
├── web_toolkit.py               # WebToolkit
├── task_toolkit.py              # TaskManagementToolkit
├── scheduling_toolkit.py        # SchedulingToolkit
├── worktree_toolkit.py          # WorktreeToolkit
├── planning_toolkit.py          # PlanningToolkit
├── collaboration_toolkit.py     # CollaborationToolkit
├── mcp_toolkit.py               # McpToolkit
├── code_analysis_toolkit.py     # CodeAnalysisToolkit
├── discovery_toolkit.py         # DiscoveryToolkit
└── system_toolkit.py            # SystemToolkit
```

Each file follows the same pattern:

```python
# Example: filesystem_toolkit.py
from ephemeralos.tools.base import BaseToolkit
from ephemeralos.tools.file_read_tool import FileReadTool
from ephemeralos.tools.file_write_tool import FileWriteTool
# ...

class FilesystemToolkit(BaseToolkit):
    """File system operations: read, write, edit, search."""
    
    def __init__(self) -> None:
        super().__init__(
            name="filesystem",
            description="File system operations: read, write, edit, search",
            tools=[
                FileReadTool(),
                FileWriteTool(),
                FileEditTool(),
                NotebookEditTool(),
                GlobTool(),
                GrepTool(),
            ],
        )
```

**Expected deliverable**: 12 toolkit classes, each grouping related tools

### Step 4: Add `ToolkitFactory` for context-aware instantiation — `src/ephemeralos/toolkits/factory.py`

Mirror synthetic-os pattern for stateful toolkits:

```python
from dataclasses import dataclass, field
from typing import Any, Callable

@dataclass
class ToolkitContext:
    """Runtime context for toolkit factories."""
    agent_name: str = ""
    cwd: str = ""
    metadata: dict[str, Any] = field(default_factory=dict)
    mcp_manager: Any = None

ToolkitFactoryFn = Callable[[ToolkitContext], BaseToolkit]

_factories: dict[str, ToolkitFactoryFn] = {}

def register_toolkit_factory(name: str, factory: ToolkitFactoryFn) -> None:
    _factories[name] = factory

def create_toolkit(name: str, ctx: ToolkitContext) -> BaseToolkit:
    factory = _factories.get(name)
    if factory is not None:
        return factory(ctx)
    raise KeyError(f"Toolkit factory '{name}' not registered")

def list_factories() -> list[str]:
    return list(_factories.keys())
```

Register MCP toolkit factory (needs `mcp_manager` at runtime):

```python
def _register_builtins() -> None:
    def _create_mcp(ctx: ToolkitContext) -> BaseToolkit:
        from ephemeralos.toolkits.mcp_toolkit import McpToolkit
        return McpToolkit(mcp_manager=ctx.mcp_manager)
    
    register_toolkit_factory("mcp", _create_mcp)

_register_builtins()
```

**Expected deliverable**: Factory registry for toolkits that need runtime context

### Step 5: Update `create_default_tool_registry()` — `src/ephemeralos/tools/__init__.py`

Refactor to register toolkits instead of individual tools:

```python
from ephemeralos.toolkits import (
    FilesystemToolkit, ExecutionToolkit, WebToolkit,
    TaskManagementToolkit, SchedulingToolkit, WorktreeToolkit,
    PlanningToolkit, CollaborationToolkit, CodeAnalysisToolkit,
    DiscoveryToolkit, SystemToolkit,
)
from ephemeralos.toolkits.mcp_toolkit import McpToolkit

def create_default_tool_registry(mcp_manager=None) -> ToolRegistry:
    registry = ToolRegistry()
    
    # Register all static toolkits
    for toolkit in (
        FilesystemToolkit(),
        ExecutionToolkit(),
        WebToolkit(),
        TaskManagementToolkit(),
        SchedulingToolkit(),
        WorktreeToolkit(),
        PlanningToolkit(),
        CollaborationToolkit(),
        CodeAnalysisToolkit(),
        DiscoveryToolkit(),
        SystemToolkit(),
    ):
        registry.register_toolkit(toolkit)
    
    # Context-aware toolkit: MCP
    if mcp_manager is not None:
        registry.register_toolkit(McpToolkit(mcp_manager))
    
    return registry
```

**Expected deliverable**: Refactored registration using toolkits (backward compatible — all tools still individually accessible by name)

### Step 6: Add toolkit API endpoints (optional, mirrors synthetic-os)

Add to the query engine or future API layer:

```python
# Expose toolkit listing in tool schemas
def to_api_schema(self) -> dict:
    return {
        "tools": [t.to_api_schema() for t in self._tools.values()],
        "toolkits": [
            {"name": tk.name, "description": tk.description, "tools": tk.tool_names()}
            for tk in self._toolkits.values()
        ],
    }
```

**Expected deliverable**: Toolkit metadata exposed alongside tool schemas

---

## Key Files

| File | Operation | Description |
|------|-----------|-------------|
| `src/ephemeralos/tools/base.py` | Modify | Add `BaseToolkit` class, extend `ToolRegistry` |
| `src/ephemeralos/tools/__init__.py` | Modify | Refactor `create_default_tool_registry()` to use toolkits |
| `src/ephemeralos/toolkits/__init__.py` | Create | Toolkit package init + exports |
| `src/ephemeralos/toolkits/filesystem_toolkit.py` | Create | FilesystemToolkit |
| `src/ephemeralos/toolkits/execution_toolkit.py` | Create | ExecutionToolkit |
| `src/ephemeralos/toolkits/web_toolkit.py` | Create | WebToolkit |
| `src/ephemeralos/toolkits/task_toolkit.py` | Create | TaskManagementToolkit |
| `src/ephemeralos/toolkits/scheduling_toolkit.py` | Create | SchedulingToolkit |
| `src/ephemeralos/toolkits/worktree_toolkit.py` | Create | WorktreeToolkit |
| `src/ephemeralos/toolkits/planning_toolkit.py` | Create | PlanningToolkit |
| `src/ephemeralos/toolkits/collaboration_toolkit.py` | Create | CollaborationToolkit |
| `src/ephemeralos/toolkits/mcp_toolkit.py` | Create | McpToolkit (context-aware) |
| `src/ephemeralos/toolkits/code_analysis_toolkit.py` | Create | CodeAnalysisToolkit |
| `src/ephemeralos/toolkits/discovery_toolkit.py` | Create | DiscoveryToolkit |
| `src/ephemeralos/toolkits/system_toolkit.py` | Create | SystemToolkit |
| `src/ephemeralos/toolkits/factory.py` | Create | ToolkitFactory + ToolkitContext |

## Risks and Mitigation

| Risk | Mitigation |
|------|------------|
| Breaking existing tool lookup by name | `register_toolkit()` also registers tools individually — fully backward compatible |
| MCP tools need runtime `mcp_manager` | Use factory pattern for MCP toolkit, same as synthetic-os |
| Tool instantiation order issues | Toolkits own their tool instances; no cross-toolkit dependencies |
| Divergence from synthetic-os patterns | We deliberately use our own `BaseToolkit` (not agno's `Toolkit`) since EphemeralOS doesn't depend on agno |

## Architecture Comparison

| Aspect | Synthetic-OS | EphemeralOS (proposed) |
|--------|-------------|----------------------|
| Base class | `agno.tools.Toolkit` | `BaseToolkit` (own) |
| Tool decorator | `@tool()` from agno | Class-based `BaseTool` (unchanged) |
| Factory | `ToolkitFactory` callable | Same pattern |
| Context | `ToolkitContext` dataclass | Same pattern (simpler fields) |
| Registration | Singleton `tool_registry` + auto-discovery | `create_default_tool_registry()` function (unchanged pattern) |
| Agent config | JSON `"toolkits": ["ci", "daytona"]` | Future: agent definitions reference toolkit names |
