"""Tool abstractions."""

from __future__ import annotations

import json
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterable, Literal, Sequence

from pydantic import BaseModel, Field, RootModel, ValidationError

from tools.core.runtime import ExecutionMetadata

__all__ = [
    "BackgroundMode",
    "BaseTool",
    "BaseToolkit",
    "ExecutionMetadata",
    "TextToolOutput",
    "ToolExecutionContext",
    "ToolRegistry",
    "ToolResult",
    "decorate_schemas_for_background",
    "run_tool_safely",
    "validate_tool_output",
]


BackgroundMode = Literal["forbidden", "optional", "always"]
_RUNTIME_CONTROL_FIELDS = frozenset({"background", "task_note"})


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


class TextToolOutput(RootModel[str]):
    """Successful output for tools whose true output is plain text."""

    root: str = Field(..., description="Plain text returned by the tool.")


class BaseTool(ABC):
    """Base class for all EphemeralOS tools."""

    name: str
    description: str
    short_description: str | None = None
    input_model: type[BaseModel]
    output_model: type[BaseModel] = TextToolOutput
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

    def background_preflight(
        self,
        arguments: BaseModel,
        context: ToolExecutionContext,
    ) -> ToolResult | None:
        """Optionally reject a background launch before the task is spawned."""
        del arguments, context
        return None

    def output_schema(self) -> dict[str, Any]:
        """Return the output JSON Schema for successful tool output."""
        return self.output_model.model_json_schema()

    def to_api_schema(self) -> dict[str, Any]:
        """Return the tool schema expected by the Anthropic Messages API."""
        schema: dict[str, Any] = {
            "name": self.name,
            "description": self.description,
            "input_schema": self.input_model.model_json_schema(),
        }
        schema["output_schema"] = self.output_schema()
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
        for toolkit in self._toolkits.values():
            toolkit._tools = {
                name: tool for name, tool in toolkit._tools.items() if name not in blocked
            }

    def restrict_to_tools(self, tool_names: list[str]) -> None:
        """Keep only the named tools and prune toolkit contents to match."""
        allowed = set(tool_names)
        self._tools = {k: v for k, v in self._tools.items() if k in allowed}
        kept_toolkits: dict[str, BaseToolkit] = {}
        for name, toolkit in self._toolkits.items():
            toolkit._tools = {
                tool_name: tool
                for tool_name, tool in toolkit._tools.items()
                if tool_name in allowed
            }
            if toolkit._tools:
                kept_toolkits[name] = toolkit
        self._toolkits = kept_toolkits

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

    Registered tool guards (see :mod:`tools.core.guards`) run after pydantic
    input validation and again after successful output validation. An empty
    registry makes this path a no-op.
    """
    from tools.core.guards import run_post as _run_post_guards
    from tools.core.guards import run_pre as _run_pre_guards

    clean_input = _strip_runtime_control_fields(tool, raw_input)
    try:
        parsed_input = tool.input_model.model_validate(clean_input)
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

    pre = await _run_pre_guards(tool.name, parsed_input, context)
    if pre.deny is not None:
        return ToolResult(
            output=pre.deny.message,
            is_error=pre.deny.is_error,
            metadata=_guard_warnings_metadata({}, pre.warnings),
        )
    parsed_input = pre.args
    # Bridge pre-phase warnings into the tool's execution context so tool
    # bodies that fold warnings into their output payload (e.g.
    # ``daytona_edit_file``'s ``warnings`` field) can read them.
    context.metadata["guard_pre_warnings"] = list(pre.warnings)

    try:
        result = await tool.execute(parsed_input, context)
    except Exception as exc:
        return ToolResult(
            output=f"Tool execution failed: {exc}",
            is_error=True,
            metadata=_guard_warnings_metadata({}, pre.warnings),
        )
    validated = validate_tool_output(tool, result)
    post = await _run_post_guards(tool.name, parsed_input, context, validated)
    all_warnings = [*pre.warnings, *post.warnings]
    if all_warnings:
        return ToolResult(
            output=validated.output,
            is_error=validated.is_error,
            metadata=_guard_warnings_metadata(validated.metadata, all_warnings),
        )
    return validated


def _guard_warnings_metadata(
    base: dict[str, Any],
    warnings: list[str],
) -> dict[str, Any]:
    """Fold guard-pipeline warnings into a tool-result metadata dict."""
    if not warnings:
        return dict(base)
    merged = dict(base)
    existing = list(merged.get("guard_warnings", []))
    existing.extend(warnings)
    merged["guard_warnings"] = existing
    return merged


def _format_validation_errors(exc: ValidationError) -> str:
    return "; ".join(
        f"{'.'.join(str(p) for p in e['loc'])}: {e['msg']}" for e in exc.errors()
    )


def validate_tool_output(tool: "BaseTool", result: ToolResult) -> ToolResult:
    """Validate successful tool output against the tool's declared output model."""
    if result.is_error:
        return result

    model = tool.output_model
    try:
        if issubclass(model, RootModel):
            model.model_validate(result.output)
        else:
            try:
                payload = json.loads(result.output)
            except json.JSONDecodeError as exc:
                return ToolResult(
                    output=(
                        f"Invalid output from {tool.name}: expected JSON matching "
                        f"{model.__name__}, got non-JSON output ({exc.msg})."
                    ),
                    is_error=True,
                    metadata={
                        **result.metadata,
                        "output_validation_error": exc.msg,
                    },
                )
            model.model_validate(payload)
    except ValidationError as exc:
        errors = _format_validation_errors(exc)
        return ToolResult(
            output=(
                f"Invalid output from {tool.name}: output did not match "
                f"{model.__name__}: {errors}."
            ),
            is_error=True,
            metadata={
                **result.metadata,
                "output_validation_error": errors,
            },
        )
    return result


def _strip_runtime_control_fields(tool: "BaseTool", raw_input: dict[str, Any]) -> dict[str, Any]:
    """Remove engine-level schema decorations before tool-local validation."""

    model_fields = set(tool.input_model.model_fields)
    return {
        key: value
        for key, value in raw_input.items()
        if key not in _RUNTIME_CONTROL_FIELDS or key in model_fields
    }


def decorate_schemas_for_background(
    registry: ToolRegistry,
    schemas: list[dict[str, Any]],
    *,
    terminal_tools: Iterable[str] = (),
) -> list[dict[str, Any]]:
    """Inject ``task_note`` (required) and ``background`` (optional) fields.

    Mutates each schema in-place and returns the list. ``task_note`` is
    added to non-terminal tools so the LLM must explain what and why on
    each call. Terminal tools are one-way submissions and must expose only
    their true payload schema. ``background`` is added only to non-terminal
    tools whose ``background`` policy is ``"optional"`` (LLM may choose).
    Tools marked ``"always"`` are dispatched in the background
    unconditionally and need no LLM-facing flag.
    """
    terminal_tool_names = set(terminal_tools)
    for schema in schemas:
        tool_name = str(schema["name"])
        tool = registry.get(tool_name)
        inp = schema.setdefault("input_schema", {})
        props = inp.setdefault("properties", {})
        is_terminal = tool_name in terminal_tool_names
        if not is_terminal:
            props["task_note"] = {
                "type": "string",
                "description": "Brief note: what and why",
            }
            req = inp.setdefault("required", [])
            if "task_note" not in req:
                req.append("task_note")
        if (
            not is_terminal
            and tool is not None
            and getattr(tool, "background", "forbidden") == "optional"
        ):
            props["background"] = {
                "type": "boolean",
                "description": (
                    "Set to true to run this tool asynchronously in the background. "
                    "Use for long-running operations (builds, test suites, installs)."
                ),
            }
    return schemas
