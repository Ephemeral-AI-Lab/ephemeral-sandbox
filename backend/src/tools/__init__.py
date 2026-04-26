"""Tool definitions."""

from tools.core import (
    BaseTool,
    ToolExecutionContextService,
    ToolRegistry,
    ToolResult,
    tool,
)


def create_default_tool_registry() -> ToolRegistry:
    """Return an empty tool registry. Tools are registered during agent setup."""
    return ToolRegistry()


__all__ = [
    "BaseTool",
    "ToolExecutionContextService",
    "ToolRegistry",
    "ToolResult",
    "create_default_tool_registry",
    "tool",
]
