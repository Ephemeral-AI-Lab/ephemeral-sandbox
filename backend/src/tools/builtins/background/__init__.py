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
    """Create the background task management toolkit."""
    tools_list = ", ".join(f"`{n}`" for n in bg_tool_names)
    toolkit = BaseToolkit(
        name="background",
        description="Background task management — launch, monitor, and cancel long-running tools.",
        tools=[CheckBackgroundProgressTool(), CancelBackgroundTaskTool(), WaitForBackgroundTaskTool()],
        instructions=(
            f"Background-capable tools: {tools_list}\n"
            '- Launch long work with `"background": true` so you can keep moving.\n'
            "- After launching background work, keep using the foreground turn on remaining analysis or other ready tasks; do not immediately block on the new task unless it is the only blocker left.\n"
            "- Prefer foreground work or a single wait when blocked; call `check_background_progress` only when live status will change whether you continue, wait, or cancel. Do not poll for reassurance.\n"
            "- Treat `delivered`, `[COMPLETED]`, `[ALREADY_COMPLETED]`, and `[NO TASKS RUNNING]` as terminal signals; retire those task ids and act on the result instead of polling or waiting again.\n"
            "- For `run_subagent` results that say `Posted.`, background tools will only repeat the delivery envelope; use the relevant note/artifact reader next. In team-planner contexts, read current-task notes with `read_task_note(scope=\"own\", paths=None, task_note=\"Read posted scout notes\")` when exact scout paths are unclear, or `read_task_note(paths=[...])` for known scout scopes.\n"
            "- Use `wait_for_background_task` when you are otherwise idle or blocked on the result.\n"
            "- If progress shows failure, fatal output, or low-value work, cancel it immediately with `cancel_background_task`.\n"
            "- `check_background_progress` and `wait_for_background_task` accept `task_id=\"all\"`; `cancel_background_task` does not."
        ),
    )
    toolkit.background_capable_tools = list(bg_tool_names)
    return toolkit
