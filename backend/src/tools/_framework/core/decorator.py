"""@tool decorator — convert a function into a BaseTool.

Usage::

    class MyToolInput(BaseModel):
        query: str = Field(..., description="The search query.")
        limit: int = Field(default=10, description="Max results to return.")

    class MyToolOutput(BaseModel):
        results: list[str] = Field(..., description="Matching items.")
        total: int = Field(..., description="Total result count.")

    @tool(
        name="my_tool",
        description="Does something useful.",
        input_model=MyToolInput,
        output_model=MyToolOutput,
        intent=Intent.READ_ONLY,
    )
    async def my_tool(
        query: str,
        limit: int = 10,
        *,
        context: ToolExecutionContextService,
    ) -> ToolResult:
        ...

Every decorated tool supplies explicit Pydantic ``input_model`` and
``output_model`` definitions plus the foreground ``intent`` (see
:class:`sandbox._shared.models.Intent`). Field descriptions live on those
models.
"""

from __future__ import annotations

from collections.abc import Callable
import inspect
from typing import Any, cast

from pydantic import BaseModel

from sandbox._shared.models import Intent
from tools._framework.core.base import BaseTool
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.hooks import validate_hook_targets
from tools._framework.core.results import ToolResult


def tool(
    name: str | None = None,
    description: str | None = None,
    short_description: str | None = None,
    *,
    input_model: type[BaseModel],
    output_model: type[BaseModel],
    intent: Intent | None = None,
    task_type: str = "agent",
    is_terminal_tool: bool = False,
    context_requirements: list[str] | tuple[str, ...] = (),
    pre_hooks: list[object] | tuple[object, ...] = (),
    post_hooks: list[object] | tuple[object, ...] = (),
) -> Callable[[Callable[..., Any]], BaseTool]:
    """Decorator that converts a function into a ``BaseTool`` instance."""

    if intent is None:
        raise TypeError(
            "@tool requires intent=Intent.READ_ONLY|WRITE_ALLOWED|LIFECYCLE; "
            f"missing on {name or '<anonymous>'}"
        )

    def decorator(func: Callable[..., Any]) -> BaseTool:
        tool_name = name or func.__name__
        normalized_context_requirements = tuple(context_requirements)
        normalized_pre_hooks = tuple(pre_hooks)
        normalized_post_hooks = tuple(post_hooks)
        validate_hook_targets(
            tool_name=tool_name,
            pre_hooks=normalized_pre_hooks,
            post_hooks=normalized_post_hooks,
        )
        docstring = inspect.getdoc(func) or ""

        # Extract description from first non-empty docstring line
        tool_description = description
        if tool_description is None:
            first_line = docstring.split("\n")[0].strip() if docstring else ""
            tool_description = first_line or f"Tool: {tool_name}"

        is_async = inspect.iscoroutinefunction(func)
        signature = inspect.signature(func)
        accepts_context = "context" in signature.parameters or any(
            parameter.kind is inspect.Parameter.VAR_KEYWORD
            for parameter in signature.parameters.values()
        )

        # Build the BaseTool subclass dynamically
        class FunctionTool(BaseTool):
            __doc__ = docstring

            # Dynamically set by the decorator
            _entrypoint: Callable[..., Any]

            async def execute(
                self, arguments: BaseModel, context: ToolExecutionContextService
            ) -> ToolResult:
                kwargs = arguments.model_dump()
                if accepts_context:
                    context["__intent"] = self.intent
                    kwargs["context"] = context
                if is_async:
                    result = await func(**kwargs)
                else:
                    result = func(**kwargs)
                return cast(ToolResult, result)

        instance = FunctionTool()
        instance.name = tool_name
        instance.description = tool_description
        instance.short_description = short_description
        instance.input_model = input_model
        instance.output_model = output_model
        instance.intent = intent
        instance.task_type = task_type
        instance.is_terminal_tool = is_terminal_tool
        instance.context_requirements = normalized_context_requirements
        instance.pre_hooks = normalized_pre_hooks
        instance.post_hooks = normalized_post_hooks
        # Preserve the original function for testing/introspection
        instance._entrypoint = func

        return instance

    return decorator
