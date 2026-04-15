"""@tool decorator — convert a function into a BaseTool with auto-generated schema.

Usage::

    @tool(name="my_tool", description="Does something useful.")
    async def my_tool(
        query: str,
        limit: int = 10,
        *,
        context: ToolExecutionContext,
    ) -> ToolResult:
        \"\"\"Detailed docstring.

        Args:
            query: The search query
            limit: Max results to return

        Returns:
            results (list): Matching items
            total (int): Total count
        \"\"\"
        ...

The decorator inspects the function signature and docstring to:

1. Build a Pydantic ``input_model`` from type hints (excluding ``context``)
2. Extract parameter descriptions from the ``Args:`` docstring section
3. Parse output schema from the ``Returns:`` docstring section
4. Support ``read_only`` and ``stop_after_tool_call`` flags
"""

from __future__ import annotations

import inspect
import re
from typing import Any, Literal, get_type_hints
from collections.abc import Callable

from pydantic import BaseModel, Field, create_model

from tools.core.base import BaseTool, ToolExecutionContext, ToolResult, _parse_returns_schema


# Parameters that are injected by the framework, not user-supplied
_FRAMEWORK_PARAMS = frozenset({"context", "self", "cls"})


def _parse_arg_descriptions(docstring: str | None) -> dict[str, str]:
    """Extract parameter descriptions from an ``Args:`` docstring section.

    Supports::

        Args:
            param_name: Description of the parameter
            param_name (type): Description of the parameter
    """
    if not docstring:
        return {}

    match = re.search(r"Args:\s*\n((?:\s+.*\n?)*)", docstring)
    if not match:
        return {}

    descriptions: dict[str, str] = {}
    block = match.group(1)
    for line in block.splitlines():
        line = line.strip()
        if not line:
            continue
        # param_name (type): description
        m = re.match(r"(\w+)\s*(?:\([^)]*\))?\s*:\s*(.*)", line)
        if m:
            descriptions[m.group(1)] = m.group(2).strip()
    return descriptions


def _build_input_model(
    func: Callable[..., Any],
    name: str,
    arg_descriptions: dict[str, str],
) -> type[BaseModel]:
    """Build a Pydantic model from function signature type hints.

    Parameters named in ``_FRAMEWORK_PARAMS`` are excluded.
    Keyword-only parameters without defaults become required fields.
    """
    try:
        hints = get_type_hints(func)
    except Exception:
        hints = {}

    sig = inspect.signature(func)
    fields: dict[str, Any] = {}

    for param_name, param in sig.parameters.items():
        if param_name in _FRAMEWORK_PARAMS:
            continue
        if param.kind in (
            inspect.Parameter.VAR_POSITIONAL,
            inspect.Parameter.VAR_KEYWORD,
        ):
            continue

        annotation = hints.get(param_name, str)
        # Skip ToolExecutionContext and ToolResult types
        if annotation is ToolExecutionContext or annotation is ToolResult:
            continue

        description = arg_descriptions.get(param_name, "")
        has_default = param.default is not inspect.Parameter.empty

        if has_default:
            fields[param_name] = (
                annotation,
                Field(default=param.default, description=description),
            )
        else:
            fields[param_name] = (
                annotation,
                Field(description=description),
            )

    model_name = f"{name.title().replace('_', '')}Input"
    return create_model(model_name, **fields)


def tool(
    name: str | None = None,
    description: str | None = None,
    short_description: str | None = None,
    *,
    read_only: bool = False,
    stop_after_tool_call: bool = False,
    background: Literal["forbidden", "optional", "always"] = "forbidden",
    task_type: str = "agent",
) -> Callable[[Callable[..., Any]], BaseTool]:
    """Decorator that converts a function into a ``BaseTool`` instance.

    Args:
        name: Tool name (defaults to function name).
        description: Tool description (defaults to first docstring line).
        short_description: Concise prompt-facing description for toolkit summaries.
        read_only: Whether the tool is read-only.
        stop_after_tool_call: Whether the agent loop should stop after this tool.
        background: Background dispatch policy.
            ``"forbidden"`` — never run in background (default).
            ``"optional"`` — LLM may opt in via input ``background=true``.
            ``"always"``   — engine ALWAYS dispatches as background.
        task_type: Discriminator propagated to the background task manager so
            monitoring/UI/audit can tell ordinary background tools ("agent")
            apart from tools that spawn a nested agent ("subagent").

    Returns:
        A ``BaseTool`` instance ready for registration in a toolkit or registry.
    """

    def decorator(func: Callable[..., Any]) -> BaseTool:
        tool_name = name or func.__name__
        docstring = inspect.getdoc(func) or ""

        # Extract description from first non-empty docstring line
        tool_description = description
        if tool_description is None:
            first_line = docstring.split("\n")[0].strip() if docstring else ""
            tool_description = first_line or f"Tool: {tool_name}"

        # Parse Args: section for parameter descriptions
        arg_descriptions = _parse_arg_descriptions(docstring)

        # Build Pydantic input model from signature
        input_model = _build_input_model(func, tool_name, arg_descriptions)

        # Parse Returns: section for output schema
        output = _parse_returns_schema(docstring)

        # Determine if the function is async
        is_async = inspect.iscoroutinefunction(func)

        # Build the BaseTool subclass dynamically
        class FunctionTool(BaseTool):
            __doc__ = docstring

            # Dynamically set by the decorator
            _stop_after_tool_call: bool
            _entrypoint: Callable[..., Any]

            async def execute(
                self, arguments: BaseModel, context: ToolExecutionContext
            ) -> ToolResult:
                kwargs = arguments.model_dump()
                kwargs["context"] = context
                if is_async:
                    return await func(**kwargs)
                else:
                    return func(**kwargs)

            def is_read_only(self, arguments: BaseModel) -> bool:
                return read_only

            def background_preflight(
                self,
                arguments: BaseModel,
                context: ToolExecutionContext,
            ) -> ToolResult | None:
                hook = getattr(self, "_background_preflight", None)
                if callable(hook):
                    return hook(arguments, context)
                return None

            def output_schema(self) -> dict[str, Any] | None:
                return output

        instance = FunctionTool()
        instance.name = tool_name
        instance.description = tool_description
        instance.short_description = short_description
        instance.input_model = input_model
        instance._stop_after_tool_call = stop_after_tool_call
        instance.background = background
        instance.task_type = task_type
        # Preserve the original function for testing/introspection
        instance._entrypoint = func

        return instance

    return decorator
