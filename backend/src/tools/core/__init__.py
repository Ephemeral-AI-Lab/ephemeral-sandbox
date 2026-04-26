"""Core tool abstractions and decorators."""

from tools.core.base import (
    BaseTool,
    TextToolOutput,
    ToolExecutionContextService,
    ToolRegistry,
    ToolResult,
    decorate_schemas_for_background,
    validate_tool_output,
)
from tools.core.decorator import tool
from tools.core.hooks import HookResult, HookStatus, ToolPostHook, ToolPreHook
from notification.service import SystemNotificationService

__all__ = [
    "BaseTool",
    "TextToolOutput",
    "ToolExecutionContextService",
    "ToolRegistry",
    "ToolResult",
    "HookResult",
    "HookStatus",
    "ToolPostHook",
    "ToolPreHook",
    "SystemNotificationService",
    "decorate_schemas_for_background",
    "validate_tool_output",
    "tool",
]
