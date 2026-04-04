"""Worktree toolkit — git worktree isolation tools."""

from ephemeralos.tools.base import BaseToolkit
from ephemeralos.tools.enter_worktree_tool import EnterWorktreeTool
from ephemeralos.tools.exit_worktree_tool import ExitWorktreeTool


class WorktreeToolkit(BaseToolkit):
    """Git worktree isolation: enter and exit."""

    def __init__(self) -> None:
        super().__init__(
            name="worktree",
            description="Git worktree isolation: enter and exit",
            tools=[EnterWorktreeTool(), ExitWorktreeTool()],
        )
