"""Toolkit definitions — grouped by capability."""

from ephemeralos.tools.base import (
    BaseTool,
    BaseToolkit,
    ToolExecutionContext,
    ToolRegistry,
    ToolResult,
)
from ephemeralos.tools.filesystem import FilesystemToolkit
from ephemeralos.tools.execution import ExecutionToolkit
from ephemeralos.tools.web import WebToolkit
from ephemeralos.tools.task_management import TaskManagementToolkit
from ephemeralos.tools.scheduling import SchedulingToolkit
from ephemeralos.tools.worktree import WorktreeToolkit
from ephemeralos.tools.planning import PlanningToolkit
from ephemeralos.tools.collaboration import CollaborationToolkit
from ephemeralos.tools.code_analysis import CodeAnalysisToolkit
from ephemeralos.tools.discovery import DiscoveryToolkit
from ephemeralos.tools.system import SystemToolkit
from ephemeralos.tools.daytona_toolkit import DaytonaToolkit
from ephemeralos.tools.ci_toolkit import CIToolkit



def create_default_tool_registry() -> ToolRegistry:
    """Return the default built-in tool registry."""
    registry = ToolRegistry()
    for toolkit in (
        FilesystemToolkit(),
        ExecutionToolkit(),
        WebToolkit(),
        TaskManagementToolkit(),
        SchedulingToolkit(),
        WorktreeToolkit(),
        PlanningToolkit(),
        CollaborationToolkit(),
        CodeAnalysisToolkit(),
        DiscoveryToolkit(),
        SystemToolkit(),
    ):
        registry.register_toolkit(toolkit)
    return registry


__all__ = [
    "create_default_tool_registry",
    "BaseTool",
    "BaseToolkit",
    "ToolExecutionContext",
    "ToolRegistry",
    "ToolResult",
    "FilesystemToolkit",
    "ExecutionToolkit",
    "WebToolkit",
    "TaskManagementToolkit",
    "SchedulingToolkit",
    "WorktreeToolkit",
    "PlanningToolkit",
    "CollaborationToolkit",
    "CodeAnalysisToolkit",
    "DiscoveryToolkit",
    "SystemToolkit",
    "DaytonaToolkit",
    "CIToolkit",
]
