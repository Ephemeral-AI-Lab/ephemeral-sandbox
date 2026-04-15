"""Tool abstractions."""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Literal, Sequence

from pydantic import BaseModel, ValidationError

from tools.core.runtime import ExecutionMetadata

__all__ = [
    "BackgroundMode",
    "BaseTool",
    "BaseToolkit",
    "ExecutionMetadata",
    "ToolExecutionContext",
    "ToolRegistry",
    "ToolResult",
    "decorate_schemas_for_background",
    "run_tool_safely",
]


BackgroundMode = Literal["forbidden", "optional", "always"]


@dataclass
class ToolExecutionContext:
    """Shared execution context for tool invocations."""

    cwd: Path
    metadata: ExecutionMetadata = field(default_factory=ExecutionMetadata)

    def __post_init__(self) -> None:
        # Accept a plain dict for backward compatibility with older call
        # sites (in particular test fixtures). Coerce it into a typed
        # ``ExecutionMetadata`` so downstream code can rely on attribute
        # access without branching on the input shape.
        if isinstance(self.metadata, dict):
            raw: dict[str, Any] = self.metadata
            meta = ExecutionMetadata()
            for key, value in raw.items():
                meta[key] = value
            self.metadata = meta


@dataclass(frozen=True)
class ToolResult:
    """Normalized tool execution result."""

    output: str
    is_error: bool = False
    metadata: dict[str, Any] = field(default_factory=dict)


_TYPE_MAP = {
    "str": "string",
    "string": "string",
    "int": "integer",
    "integer": "integer",
    "float": "number",
    "number": "number",
    "bool": "boolean",
    "boolean": "boolean",
    "list": "array",
    "array": "array",
    "dict": "object",
    "object": "object",
}


def _parse_returns_schema(docstring: str | None) -> dict[str, Any] | None:
    """Parse a ``Returns:`` section from a docstring into JSON Schema.

    Supports two formats::

        Returns:
            field_name (type): Description of the field
            field_name: Description (defaults to type "string")
    """
    import re

    if not docstring:
        return None

    match = re.search(r"Returns:\s*\n((?:\s+.*\n?)*)", docstring)
    if not match:
        return None

    block = match.group(1)
    properties: dict[str, Any] = {}
    required: list[str] = []

    for line in block.splitlines():
        line = line.strip()
        if not line:
            continue
        # field_name (type): description
        m = re.match(r"(\w+)\s*\((\w+)\)\s*:\s*(.*)", line)
        if m:
            name, typ, desc = m.group(1), m.group(2), m.group(3).strip()
            properties[name] = {
                "type": _TYPE_MAP.get(typ.lower(), "string"),
                "description": desc,
            }
            required.append(name)
            continue
        # field_name: description
        m = re.match(r"(\w+)\s*:\s*(.*)", line)
        if m:
            name, desc = m.group(1), m.group(2).strip()
            properties[name] = {"type": "string", "description": desc}
            required.append(name)

    if not properties:
        return None

    return {"type": "object", "properties": properties, "required": required}


class BaseTool(ABC):
    """Base class for all EphemeralOS tools."""

    name: str
    description: str
    short_description: str | None = None
    input_model: type[BaseModel]
    # Background dispatch policy:
    #   "forbidden" — tool cannot run in background (default)
    #   "optional"  — LLM may opt in by passing background=true
    #   "always"    — engine ALWAYS dispatches as background, regardless of input
    background: BackgroundMode = "forbidden"
    # Discriminator for monitoring/UI/audit so the engine never sniffs tool names.
    # "agent" is the default for ordinary background tools; tools that spawn a
    # nested agent (e.g. run_subagent) override it to "subagent".
    task_type: str = "agent"

    @abstractmethod
    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        """Execute the tool."""

    def is_read_only(self, arguments: BaseModel) -> bool:
        """Return whether the invocation is read-only."""
        del arguments
        return False

    def background_preflight(
        self,
        arguments: BaseModel,
        context: ToolExecutionContext,
    ) -> ToolResult | None:
        """Optionally reject a background launch before the task is spawned."""
        del arguments, context
        return None

    def output_schema(self) -> dict[str, Any] | None:
        """Return the output JSON Schema, parsed from the class docstring.

        If the subclass docstring has a ``Returns:`` section, it is
        automatically converted to a JSON Schema.  Returns ``None``
        if no output schema is documented.
        """
        return _parse_returns_schema(self.__class__.__doc__)

    def to_api_schema(self) -> dict[str, Any]:
        """Return the tool schema expected by the Anthropic Messages API."""
        schema: dict[str, Any] = {
            "name": self.name,
            "description": self.description,
            "input_schema": self.input_model.model_json_schema(),
        }
        out = self.output_schema()
        if out is not None:
            schema["output_schema"] = out
        return schema


class BaseToolkit:
    """Named collection of related tools."""

    def __init__(
        self,
        name: str = "",
        description: str = "",
        tools: Sequence[BaseTool] | None = None,
        instructions: str | None = None,
    ) -> None:
        self.name = name
        self.description = description
        self.instructions = instructions
        self._tools: dict[str, BaseTool] = {}
        for tool in tools or []:
            self.register(tool)

    @classmethod
    def from_context(cls, ctx: Any) -> BaseToolkit:
        """Construct an instance from a ToolkitContext.

        Default implementation calls ``cls()`` with no arguments. Override
        in subclasses that need to pull values out of ``ctx.metadata``.
        """
        del ctx
        return cls()

    def register(self, tool: BaseTool) -> None:
        """Add a tool to this toolkit."""
        self._tools[tool.name] = tool

    def get(self, name: str) -> BaseTool | None:
        """Return a tool by name."""
        return self._tools.get(name)

    def list_tools(self) -> list[BaseTool]:
        """Return all tools in this toolkit."""
        return list(self._tools.values())

    def tool_names(self) -> list[str]:
        """Return names of all tools in this toolkit."""
        return list(self._tools.keys())

    async def prepare_context_async(self, context: Any) -> None:
        """Override in subclass to inject async sandbox into context."""

    def prepare_context(self, context: Any) -> None:
        """Override in subclass to inject sandbox into context."""


class ToolRegistry:
    """Map tool names to implementations, with optional toolkit grouping."""

    def __init__(self) -> None:
        self._tools: dict[str, BaseTool] = {}
        self._toolkits: dict[str, BaseToolkit] = {}

    def register(self, tool: BaseTool) -> None:
        """Register a tool instance."""
        self._tools[tool.name] = tool

    def register_toolkit(self, toolkit: BaseToolkit) -> None:
        """Register a toolkit and all its tools individually."""
        self._toolkits[toolkit.name] = toolkit
        for tool in toolkit.list_tools():
            self._tools[tool.name] = tool

    def get(self, name: str) -> BaseTool | None:
        """Return a registered tool by name."""
        return self._tools.get(name)

    def get_toolkit(self, name: str) -> BaseToolkit | None:
        """Return a registered toolkit by name."""
        return self._toolkits.get(name)

    def list_tools(self) -> list[BaseTool]:
        """Return all registered tools."""
        return list(self._tools.values())

    def list_toolkits(self) -> list[BaseToolkit]:
        """Return all registered toolkits."""
        return list(self._toolkits.values())

    def restrict_to_toolkits(self, toolkit_names: list[str]) -> None:
        """Remove all tools and toolkits not in *toolkit_names*."""
        allowed = set(toolkit_names)
        allowed_tools: set[str] = set()
        kept_toolkits: dict[str, BaseToolkit] = {}
        for name, tk in self._toolkits.items():
            if name in allowed:
                kept_toolkits[name] = tk
                allowed_tools.update(tk.tool_names())
        self._toolkits = kept_toolkits
        self._tools = {k: v for k, v in self._tools.items() if k in allowed_tools}

    def remove_tools(self, tool_names: list[str]) -> None:
        """Remove specific tools by name (blocklist). Toolkits are kept."""
        blocked = set(tool_names)
        self._tools = {k: v for k, v in self._tools.items() if k not in blocked}

    def to_api_schema(self) -> list[dict[str, Any]]:
        """Return all tool schemas in API format.

        Cross-cutting decorations like the required ``task_note`` field
        and the optional ``background`` flag are applied separately by
        :func:`decorate_schemas_for_background` so the registry stays
        a dumb collection.
        """
        return [tool.to_api_schema() for tool in self._tools.values()]


async def run_tool_safely(
    tool: "BaseTool",
    raw_input: dict[str, Any],
    context: "ToolExecutionContext",
) -> ToolResult:
    """Validate input, execute *tool*, and normalise errors to a ``ToolResult``.

    Used by both the streaming executor and the background-dispatch path
    so validation and error framing stay consistent across the engine's
    tool invocation sites. ``asyncio.CancelledError`` is intentionally
    not caught — callers decide how to handle cancellation.
    """
    try:
        parsed_input = tool.input_model.model_validate(raw_input)
    except ValidationError as exc:
        errors = "; ".join(
            f"{'.'.join(str(p) for p in e['loc'])}: {e['msg']}" for e in exc.errors()
        )
        return ToolResult(
            output=(
                f"Invalid input for {tool.name}: {errors}. "
                "Please retry the tool call with valid arguments."
            ),
            is_error=True,
        )
    except Exception as exc:
        return ToolResult(
            output=f"Invalid input for {tool.name}: {exc}",
            is_error=True,
        )

    try:
        return await tool.execute(parsed_input, context)
    except Exception as exc:
        return ToolResult(
            output=f"Tool execution failed: {exc}",
            is_error=True,
        )


def decorate_schemas_for_background(
    registry: ToolRegistry, schemas: list[dict[str, Any]]
) -> list[dict[str, Any]]:
    """Inject ``task_note`` (required) and ``background`` (optional) fields.

    Mutates each schema in-place and returns the list. ``task_note`` is
    added to every tool so the LLM must explain what and why on each call.
    ``background`` is added only to tools whose ``background`` policy is
    ``"optional"`` (LLM may choose). Tools marked ``"always"`` are dispatched
    in the background unconditionally and need no LLM-facing flag.
    """
    for schema in schemas:
        tool = registry.get(schema["name"])
        inp = schema.setdefault("input_schema", {})
        props = inp.setdefault("properties", {})
        props["task_note"] = {
            "type": "string",
            "description": "Brief note: what and why",
        }
        req = inp.setdefault("required", [])
        if "task_note" not in req:
            req.append("task_note")
        if tool is not None and getattr(tool, "background", "forbidden") == "optional":
            props["background"] = {
                "type": "boolean",
                "description": (
                    "Set to true to run this tool asynchronously in the background. "
                    "Use for long-running operations (builds, test suites, installs)."
                ),
            }
    return schemas
