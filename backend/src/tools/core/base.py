"""Tool abstractions."""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from pydantic import BaseModel


@dataclass
class ToolExecutionContext:
    """Shared execution context for tool invocations."""

    cwd: Path
    metadata: dict[str, Any] = field(default_factory=dict)


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
    input_model: type[BaseModel]
    supports_background: bool = False

    @abstractmethod
    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        """Execute the tool."""

    def is_read_only(self, arguments: BaseModel) -> bool:
        """Return whether the invocation is read-only."""
        del arguments
        return False

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
        name: str,
        description: str,
        tools: list[BaseTool] | None = None,
        instructions: str | None = None,
    ) -> None:
        self.name = name
        self.description = description
        self.instructions = instructions
        self._tools: dict[str, BaseTool] = {}
        for tool in tools or []:
            self.register(tool)

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

    def to_api_schema(self, *, inject_task_note: bool = False) -> list[dict[str, Any]]:
        """Return all tool schemas in API format.

        When *inject_task_note* is True, every tool's input_schema gets a
        required ``task_note`` field so the LLM must provide it on every call.
        """
        schemas = []
        for tool in self._tools.values():
            schema = tool.to_api_schema()
            if inject_task_note:
                inp = schema.setdefault("input_schema", {})
                inp.setdefault("properties", {})["task_note"] = {
                    "type": "string",
                    "description": "Brief note: what and why",
                }
                req = inp.setdefault("required", [])
                if "task_note" not in req:
                    req.append("task_note")
            schemas.append(schema)
        return schemas
