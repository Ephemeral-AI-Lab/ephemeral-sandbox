"""Task management toolkit — task tracking tools."""

from ephemeralos.tools.base import BaseToolkit
from ephemeralos.tools.task_create_tool import TaskCreateTool
from ephemeralos.tools.task_get_tool import TaskGetTool
from ephemeralos.tools.task_list_tool import TaskListTool
from ephemeralos.tools.task_output_tool import TaskOutputTool
from ephemeralos.tools.task_stop_tool import TaskStopTool
from ephemeralos.tools.task_update_tool import TaskUpdateTool


class TaskManagementToolkit(BaseToolkit):
    """Task tracking: create, get, list, update, stop, output."""

    def __init__(self) -> None:
        super().__init__(
            name="task_management",
            description="Task tracking: create, get, list, update, stop, output",
            tools=[
                TaskCreateTool(),
                TaskGetTool(),
                TaskListTool(),
                TaskUpdateTool(),
                TaskStopTool(),
                TaskOutputTool(),
            ],
        )
