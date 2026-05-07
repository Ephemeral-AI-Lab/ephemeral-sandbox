"""Public tools facade.

Runtime code should import tool primitives, factories, and execution helpers
from this module. Subpackages under ``tools.*`` are implementation modules.
"""

from __future__ import annotations

from typing import Any

from tools.core import (
    BaseTool,
    HookResult,
    HookStatus,
    TextToolOutput,
    ToolExecutionContextService,
    ToolPostHook,
    ToolPreHook,
    ToolRegistry,
    ToolResult,
    tool,
)
from tools.core.runtime import ExecutionMetadata

_LAZY_EXPORTS = {
    "CancelBackgroundTaskTool": "tools.builtins.background",
    "CheckBackgroundTaskResultTool": "tools.builtins.background",
    "ToolCatalogEntry": "tools.core.catalog",
    "ToolFactoryContext": "tools.core.factory_context",
    "WaitBackgroundTasksTool": "tools.builtins.background",
    "_consume_tool_budget_or_reject": "tools.core.tool_execution",
    "build_background_snapshot_metadata": "tools.builtins.background._common",
    "collect_schema_tools": "tools.core.schema_summary",
    "collect_tool_catalog": "tools.core.catalog",
    "create_tool": "tools.core.factory",
    "create_tools": "tools.core.factory",
    "decorate_schemas_for_background": "tools.core.validation",
    "execute_tool_call": "tools.core.tool_execution",
    "execute_tool_call_streaming": "tools.core.tool_execution",
    "execute_tool_once": "tools.core.tool_execution",
    "format_tool_schema_summary": "tools.core.schema_summary",
    "has_tool": "tools.core.factory",
    "list_available_tools": "tools.core.factory",
    "make_background_tools": "tools.builtins.background",
    "make_sandbox_tools": "tools.sandbox_toolkit",
    "make_skills_tools": "tools.builtins.skills",
    "make_subagent_tool_from_context": "tools.subagent",
    "make_subagent_tools": "tools.subagent",
    "make_submission_tools": "tools.submission",
    "register_tool_factory": "tools.core.factory",
    "register_tool_instance": "tools.core.factory",
    "render_background_snapshot": "tools.builtins.background._common",
    "resolve_harness_notification_triggers": "tools.submission.notification_triggers",
    "SANDBOX_CONTEXT": "tools.core.context_requirements",
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
    "HookStatus",
    "SANDBOX_CONTEXT",
    "TextToolOutput",
    "ToolCatalogEntry",
    "ToolExecutionContextService",
    "ToolFactoryContext",
    "ToolPostHook",
    "ToolPreHook",
    "ToolRegistry",
    "ToolResult",
    "WaitBackgroundTasksTool",
    "_consume_tool_budget_or_reject",
    "build_background_snapshot_metadata",
    "collect_schema_tools",
    "collect_tool_catalog",
    "create_default_tool_registry",
    "create_tool",
    "create_tools",
    "decorate_schemas_for_background",
    "execute_tool_call",
    "execute_tool_call_streaming",
    "execute_tool_once",
    "format_tool_schema_summary",
    "has_tool",
    "list_available_tools",
    "make_background_tools",
    "make_sandbox_tools",
    "make_skills_tools",
    "make_subagent_tool_from_context",
    "make_subagent_tools",
    "make_submission_tools",
    "register_tool_factory",
    "register_tool_instance",
    "render_background_snapshot",
    "resolve_harness_notification_triggers",
    "tool",
]
