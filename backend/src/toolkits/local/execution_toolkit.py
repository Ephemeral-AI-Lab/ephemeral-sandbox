"""Execution toolkit — shell command execution."""

from ephemeralos.tools.base import BaseToolkit
from ephemeralos.tools.bash_tool import BashTool


class ExecutionToolkit(BaseToolkit):
    """Shell command execution."""

    def __init__(self) -> None:
        super().__init__(
            name="execution",
            description="Shell command execution",
            tools=[BashTool()],
        )
