"""Toolkit definitions — grouped by capability."""

# filesystem (6): read, write, edit, search
from ephemeralos.toolkits.local.filesystem_toolkit import FilesystemToolkit
# execution (1): shell command execution
from ephemeralos.toolkits.local.execution_toolkit import ExecutionToolkit
# web (2): web fetch and search
from ephemeralos.toolkits.local.web_toolkit import WebToolkit
# task_management (6): create, get, list, update, stop, output
from ephemeralos.toolkits.local.task_toolkit import TaskManagementToolkit
# scheduling (4): create, list, delete, toggle
from ephemeralos.toolkits.local.scheduling_toolkit import SchedulingToolkit
# worktree (2): enter, exit
from ephemeralos.toolkits.local.worktree_toolkit import WorktreeToolkit
# planning (3): enter, exit, todo
from ephemeralos.toolkits.local.planning_toolkit import PlanningToolkit
# collaboration (5): agent, send message, team create/delete, ask user
from ephemeralos.toolkits.local.collaboration_toolkit import CollaborationToolkit
# code_analysis (1): LSP
from ephemeralos.toolkits.local.code_analysis_toolkit import CodeAnalysisToolkit
# discovery (2): skill and tool search
from ephemeralos.toolkits.local.discovery_toolkit import DiscoveryToolkit
# system (4): config, brief, sleep, remote trigger
from ephemeralos.toolkits.local.system_toolkit import SystemToolkit
# integrations
from ephemeralos.toolkits.integrations.daytona_toolkit import DaytonaToolkit
from ephemeralos.toolkits.integrations.mcp_toolkit import McpToolkit

__all__ = [
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
    "McpToolkit",
    "DaytonaToolkit",
]
