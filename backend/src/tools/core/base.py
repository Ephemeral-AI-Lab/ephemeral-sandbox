"""Tool abstractions."""

from __future__ import annotations

import json
from abc import ABC, abstractmethod
from collections.abc import Iterable, Mapping
from dataclasses import dataclass, field
from inspect import isawaitable
from pathlib import Path
from typing import Any, Literal

from pydantic import BaseModel, Field, RootModel, ValidationError

from tools.core.runtime import ExecutionMetadata

__all__ = [
    "BackgroundMode",
    "BaseTool",
    "ExecutionMetadata",
    "TextToolOutput",
    "ToolInputParseResult",
    "ToolExecutionContextService",
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


@dataclass(init=False)
class ToolExecutionContextService:
    """Service and runtime state store injected into a tool invocation.

    Well-known runtime services and identifiers are exposed directly on this
    object through attribute delegation. Tool-specific extras are available
    through mapping-style access.
    """

    cwd: Path
    _metadata: ExecutionMetadata = field(default_factory=ExecutionMetadata, repr=False)

    def __init__(
        self,
        cwd: Path | str,
        services: ExecutionMetadata | Mapping[str, Any] | None = None,
        **service_overrides: Any,
    ) -> None:
        object.__setattr__(self, "cwd", Path(cwd))
        object.__setattr__(self, "_metadata", self._coerce_services(services))
        if service_overrides:
            self._metadata.update(service_overrides)

    @staticmethod
    def _coerce_services(
        services: ExecutionMetadata | Mapping[str, Any] | None,
    ) -> ExecutionMetadata:
        if services is None:
            return ExecutionMetadata()
        if isinstance(services, ExecutionMetadata):
            return services
        meta = ExecutionMetadata()
        for key, value in services.items():
            meta[key] = value
        return meta

    def __getattr__(self, name: str) -> Any:
        if name in ExecutionMetadata._TYPED_FIELDS:
            return getattr(self._metadata, name)
        raise AttributeError(name)

    def __setattr__(self, name: str, value: Any) -> None:
        if name in {"cwd", "_metadata"}:
            object.__setattr__(self, name, value)
            return
        if name in ExecutionMetadata._TYPED_FIELDS:
            setattr(self._metadata, name, value)
            return
        object.__setattr__(self, name, value)

    def services_copy(self) -> ExecutionMetadata:
        return self._metadata.copy()

    def services_with_overrides(self, **overrides: Any) -> ExecutionMetadata:
        return self._metadata.with_overrides(**overrides)

    def update_services(
        self,
        other: Mapping[str, Any] | ExecutionMetadata | None = None,
        /,
        **kwargs: Any,
    ) -> None:
        self._metadata.update(other, **kwargs)

    def get(self, key: str, default: Any = None) -> Any:
        return self._metadata.get(key, default)

    def __getitem__(self, key: str) -> Any:
        return self._metadata[key]

    def __setitem__(self, key: str, value: Any) -> None:
        self._metadata[key] = value

    def __contains__(self, key: object) -> bool:
        return key in self._metadata

    async def notify_system(self, text: str, *, category: str = "") -> None:
        """Emit a system notification through the injected notification service."""

        service = self.get("system_notification_service")
        if service is None:
            return
        notify = getattr(service, "notify_system", None)
        if notify is None:
            notify = getattr(service, "notify", None)
        if notify is None:
            return
        result = notify(text, category=category)
        if isawaitable(result):
            await result


@dataclass(frozen=True)
class ToolResult:
    """Normalized tool execution result."""

    output: str
    is_error: bool = False
    metadata: dict[str, Any] = field(default_factory=dict)
    # Set by tool execution helpers when a successful invocation of a tool with
    # ``is_terminal_tool=True`` has completed. The query loop reads this on the
    # resulting ToolResultBlock to decide whether to exit with
    # QueryExitReason.TOOL_STOP.
    does_terminate: bool = False
    # Set by mode-entry tools when they flip the agent's typestate. The
    # dispatcher reads it after the turn's tool batch runs and updates
    # ``QueryContext.active_mode`` so the next turn's gate sees the new mode.
    # Wire-irrelevant — never serialised to the provider.
    mode_transition: str | None = None


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
    # When True, a successful invocation ends the agent's query loop. The
    # engine stamps does_terminate=True on the resulting ToolResult and the
    # query loop exits with TOOL_STOP after the turn completes.
    is_terminal_tool: bool = False
    # When True, the tool is a mode-entry tool. The query loop enforces that
    # such tools are batch-exclusive (mirror the terminal-tool rule in
    # ``validate_tool_batch``) so the same turn cannot mutate the active
    # mode and dispatch siblings under conflicting gates.
    is_mode_entry_tool: bool = False
    # Tool-specific hooks. These are intentionally explicit per tool and do
    # not affect the LLM-facing schema.
    pre_hooks: tuple[Any, ...] = ()
    post_hooks: tuple[Any, ...] = ()

    @abstractmethod
    async def execute(self, arguments: BaseModel, context: ToolExecutionContextService) -> ToolResult:
        """Execute the tool."""

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
    context: "ToolExecutionContextService",
) -> ToolResult:
    """Validate input, execute *tool*, and normalise errors to a ``ToolResult``.

    Used by both the streaming executor and the background-dispatch path
    so validation and error framing stay consistent across the engine's
    tool invocation sites. ``asyncio.CancelledError`` is intentionally
    not caught — callers decide how to handle cancellation.
    """
    async def _noop_emit(_event: Any) -> None:
        return None

    from tools.core.tool_execution import execute_tool_once

    return await execute_tool_once(
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
    context: "ToolExecutionContextService",
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
                    "This supports long-running operations such as builds, test suites, "
                    "and installs."
                ),
            }
    return schemas
