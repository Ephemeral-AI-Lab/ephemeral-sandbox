"""Tool abstractions."""

from __future__ import annotations

import json
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterable, Literal

from pydantic import BaseModel, Field, RootModel, ValidationError

from tools.core.runtime import ExecutionMetadata

__all__ = [
    "BackgroundMode",
    "BaseTool",
    "ExecutionMetadata",
    "TextToolOutput",
    "ToolInputParseResult",
    "ToolExecutionContext",
    "ToolRegistry",
    "ToolResult",
    "decorate_schemas_for_background",
    "execute_tool_body",
    "parse_tool_input",
    "run_tool_safely",
    "validate_tool_output",
]


BackgroundMode = Literal["forbidden", "optional", "always"]
_RUNTIME_CONTROL_FIELDS = frozenset({"background"})


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


@dataclass(frozen=True)
class ToolInputParseResult:
    """Result of validating raw tool input."""

    args: BaseModel | None = None
    error: ToolResult | None = None

    @property
    def is_error(self) -> bool:
        return self.error is not None

    @classmethod
    def success(cls, args: BaseModel) -> ToolInputParseResult:
        return cls(args=args)

    @classmethod
    def failure(cls, result: ToolResult) -> ToolInputParseResult:
        return cls(error=result)


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


class ToolRegistry:
    """Map tool names to implementations."""

    def __init__(self) -> None:
        self._tools: dict[str, BaseTool] = {}

    def register(self, tool: BaseTool) -> None:
        """Register a tool instance."""
        self._tools[tool.name] = tool

    def register_many(self, tools: Iterable[BaseTool]) -> None:
        """Register multiple tool instances."""
        for tool in tools:
            self.register(tool)

    def get(self, name: str) -> BaseTool | None:
        """Return a registered tool by name."""
        return self._tools.get(name)

    def list_tools(self) -> list[BaseTool]:
        """Return all registered tools."""
        return list(self._tools.values())

    def remove_tools(self, tool_names: list[str]) -> None:
        """Remove specific tools by name."""
        blocked = set(tool_names)
        self._tools = {k: v for k, v in self._tools.items() if k not in blocked}

    def restrict_to_tools(self, tool_names: list[str]) -> None:
        """Keep only the named tools."""
        allowed = set(tool_names)
        self._tools = {k: v for k, v in self._tools.items() if k in allowed}

    def to_api_schema(self) -> list[dict[str, Any]]:
        """Return all tool schemas in API format.

        Cross-cutting decorations like the optional ``background`` flag are
        applied separately by :func:`decorate_schemas_for_background` so the
        registry stays a dumb collection.
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

    Platform hooks run after pydantic input validation and after tool output.
    A hook registry with no matches makes this path a no-op. This function is
    intentionally non-streaming; production query paths should prefer the
    hook-aware execution primitive in :mod:`tools.core.tool_execution`.
    """
    from tools.core.hooks.execution import execute_tool_with_hooks

    async def _noop_emit(event: object) -> None:
        del event

    return await execute_tool_with_hooks(
        tool,
        raw_input,
        context,
        emit=_noop_emit,
        emit_started=False,
    )


def parse_tool_input(
    tool: "BaseTool",
    raw_input: dict[str, Any],
) -> ToolInputParseResult:
    """Validate raw tool input against the tool's pydantic model."""
    clean_input = _strip_runtime_control_fields(tool, raw_input)
    try:
        parsed_input = tool.input_model.model_validate(clean_input)
    except ValidationError as exc:
        errors = "; ".join(
            f"{'.'.join(str(p) for p in e['loc'])}: {e['msg']}" for e in exc.errors()
        )
        return ToolInputParseResult.failure(
            ToolResult(
                output=(
                    f"Invalid input for {tool.name}: {errors}. "
                    "Please retry the tool call with valid arguments."
                ),
                is_error=True,
            )
        )
    except Exception as exc:
        return ToolInputParseResult.failure(
            ToolResult(
                output=f"Invalid input for {tool.name}: {exc}",
                is_error=True,
            )
        )
    return ToolInputParseResult.success(parsed_input)


async def execute_tool_body(
    tool: "BaseTool",
    parsed_input: BaseModel,
    context: "ToolExecutionContext",
) -> ToolResult:
    """Execute a tool with already validated input and normalize exceptions."""
    try:
        return await tool.execute(parsed_input, context)
    except Exception as exc:
        return ToolResult(
            output=f"Tool execution failed: {exc}",
            is_error=True,
        )


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
    """Inject optional ``background`` fields for eligible non-terminal tools.

    Mutates each schema in-place and returns the list. Terminal tools are
    one-way submissions and must expose only their true payload schema.
    ``background`` is added only to non-terminal tools whose ``background``
    policy is ``"optional"`` (LLM may choose). Tools marked ``"always"`` are
    dispatched in the background unconditionally and need no LLM-facing flag.
    """
    terminal_tool_names = set(terminal_tools)
    for schema in schemas:
        tool_name = str(schema["name"])
        tool = registry.get(tool_name)
        inp = schema.setdefault("input_schema", {})
        props = inp.setdefault("properties", {})
        is_terminal = tool_name in terminal_tool_names
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
