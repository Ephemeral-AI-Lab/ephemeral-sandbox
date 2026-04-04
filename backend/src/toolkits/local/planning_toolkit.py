"""Planning toolkit — todo tools."""

from ephemeralos.tools.base import BaseToolkit
from ephemeralos.tools.todo_write_tool import TodoWriteTool


class PlanningToolkit(BaseToolkit):
    """Todo management."""

    def __init__(self) -> None:
        super().__init__(
            name="planning",
            description="Todo management",
            tools=[TodoWriteTool()],
        )
