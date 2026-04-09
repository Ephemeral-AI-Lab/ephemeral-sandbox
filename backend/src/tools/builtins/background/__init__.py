"""Background task management toolkit.

Provides tools to monitor, wait for, and cancel long-running background tasks,
plus a factory to assemble them into a toolkit with instructions.
"""

from __future__ import annotations

from tools.core.base import BaseToolkit
from tools.builtins.background.check_background_progress import CheckBackgroundProgressTool
from tools.builtins.background.cancel_background_task import CancelBackgroundTaskTool
from tools.builtins.background.wait_for_background_task import WaitForBackgroundTaskTool


def make_background_toolkit(bg_tool_names: list[str]) -> BaseToolkit:
    """Create the background task management toolkit.

    Args:
        bg_tool_names: Names of tools that support background execution.
    """
    tools_list = ", ".join(f"`{n}`" for n in bg_tool_names)
    return BaseToolkit(
        name="background",
        description="Background task management — launch, monitor, and cancel long-running tools.",
        tools=[CheckBackgroundProgressTool(), CancelBackgroundTaskTool(), WaitForBackgroundTaskTool()],
        instructions=(
            f"Background-capable tools: {tools_list}\n"
            '- Launch long work with `"background": true` so you can keep moving.\n'
            "- After launching background work, keep using the foreground turn on remaining analysis or other ready tasks; do not immediately block on the new task unless it is the only blocker left.\n"
            "- Prefer `check_background_progress` to inspect live output and decide whether to continue, wait, or cancel.\n"
            "- Use `wait_for_background_task` only when you are otherwise idle and ready to join a healthy task.\n"
            "- If progress shows failure, fatal output, or low-value work, cancel it immediately with `cancel_background_task`.\n"
            "- `check_background_progress` and `wait_for_background_task` accept `task_id=\"all\"`; `cancel_background_task` does not."
        ),
    )
