"""Scheduling toolkit — cron job management tools."""

from ephemeralos.tools.base import BaseToolkit
from ephemeralos.tools.cron_create_tool import CronCreateTool
from ephemeralos.tools.cron_delete_tool import CronDeleteTool
from ephemeralos.tools.cron_list_tool import CronListTool
from ephemeralos.tools.cron_toggle_tool import CronToggleTool


class SchedulingToolkit(BaseToolkit):
    """Cron job management: create, list, delete, toggle."""

    def __init__(self) -> None:
        super().__init__(
            name="scheduling",
            description="Cron job management: create, list, delete, toggle",
            tools=[
                CronCreateTool(),
                CronListTool(),
                CronDeleteTool(),
                CronToggleTool(),
            ],
        )
