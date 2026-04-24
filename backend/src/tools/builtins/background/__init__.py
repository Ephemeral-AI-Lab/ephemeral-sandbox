"""Background task management tools.

Provides tools to monitor, wait for, and cancel long-running background tasks,
plus a factory to assemble them for registration.
"""

from __future__ import annotations

from tools.core.base import BaseTool
from tools.builtins.background.check_background_progress import CheckBackgroundProgressTool
from tools.builtins.background.cancel_background_task import CancelBackgroundTaskTool
from tools.builtins.background.wait_for_background_task import WaitForBackgroundTaskTool


def make_background_tools(bg_tool_names: list[str]) -> list[BaseTool]:
    """Create background task management tools."""
    del bg_tool_names
    return [
        CheckBackgroundProgressTool(),
        CancelBackgroundTaskTool(),
        WaitForBackgroundTaskTool(),
    ]
