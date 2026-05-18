"""Public tools facade.

Runtime code should import tool primitives, factories, and execution helpers
from this module. Subpackages under ``tools.*`` are implementation modules.
"""

from __future__ import annotations

from typing import Any

from tools._framework.core import (
    BaseTool,
    HookResult,
    TextToolOutput,
    ToolExecutionContextService,
    ToolRegistry,
    ToolResult,
    tool,
)
from tools._framework.core.runtime import ExecutionMetadata

_LAZY_EXPORTS = {
    "CancelBackgroundTaskTool": "tools.background",
    "CheckBackgroundTaskResultTool": "tools.background",
    "ToolCatalogEntry": "tools._framework.introspection.catalog",
    "ToolFactoryContext": "tools._framework.factory",
    "WaitBackgroundTasksTool": "tools.background",
    "_consume_tool_budget_or_reject": "tools._framework.execution.tool_call",
    "build_background_snapshot_metadata": "tools.background._lib._common",
    "collect_schema_tools": "tools._framework.introspection.schema_summary",
    "collect_tool_catalog": "tools._framework.introspection.catalog",
    "create_tool": "tools._framework.factory",
    "decorate_schemas_for_background": "tools._framework.core.validation",
    "execute_tool_call": "tools._framework.execution.tool_call",
    "execute_tool_call_streaming": "tools._framework.execution.tool_call",
    "execute_tool_once": "tools._framework.execution.tool_call",
    "format_tool_schema_summary": "tools._framework.introspection.schema_summary",
    "has_tool": "tools._framework.factory",
    "list_available_tools": "tools._framework.factory",
    "make_ask_helper_tools": "tools.ask_helper",
    "make_background_tools": "tools.background",
    "make_sandbox_tools": "tools.sandbox",
    "make_subagent_tool_from_context": "tools.subagent",
    "make_submission_tools": "tools.submission",
    "register_tool_factory": "tools._framework.factory",
    "render_background_snapshot": "tools.background._lib._common",
    "resolve_harness_notification_triggers": "tools.submission.notification_triggers",
    "SANDBOX_CONTEXT": "tools.sandbox._lib.context",
}


def create_default_tool_registry() -> ToolRegistry:
    """Return an empty tool registry. Tools are registered during agent setup."""
    return ToolRegistry()


def __getattr__(name: str) -> Any:
    module_name = _LAZY_EXPORTS.get(name)
    if module_name is None:
        raise AttributeError(name)
    from importlib import import_module

    value = getattr(import_module(module_name), name)
    globals()[name] = value
    return value


__all__ = [
    "BaseTool",
    "CancelBackgroundTaskTool",
    "CheckBackgroundTaskResultTool",
    "ExecutionMetadata",
    "HookResult",
    "SANDBOX_CONTEXT",
    "TextToolOutput",
    "ToolCatalogEntry",
    "ToolExecutionContextService",
    "ToolFactoryContext",
    "ToolRegistry",
    "ToolResult",
    "WaitBackgroundTasksTool",
    "_consume_tool_budget_or_reject",
    "build_background_snapshot_metadata",
    "collect_schema_tools",
    "collect_tool_catalog",
    "create_default_tool_registry",
    "create_tool",
    "decorate_schemas_for_background",
    "execute_tool_call",
    "execute_tool_call_streaming",
    "execute_tool_once",
    "format_tool_schema_summary",
    "has_tool",
    "list_available_tools",
    "make_ask_helper_tools",
    "make_background_tools",
    "make_sandbox_tools",
    "make_subagent_tool_from_context",
    "make_submission_tools",
    "register_tool_factory",
    "render_background_snapshot",
    "resolve_harness_notification_triggers",
    "tool",
]
