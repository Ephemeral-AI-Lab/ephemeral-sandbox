"""Tool for exiting plan mode."""

from __future__ import annotations

from pydantic import BaseModel

from ephemeralos.tools.base import BaseTool, ToolExecutionContext, ToolResult


class ExitPlanModeToolInput(BaseModel):
    """Arguments for exiting plan mode."""

    pass


class ExitPlanModeTool(BaseTool):
    """Exit plan mode and return to normal execution."""

    name = "exit_plan_mode"
    description = "Exit plan mode and return to normal execution."
    input_model = ExitPlanModeToolInput

    async def execute(
        self,
        arguments: ExitPlanModeToolInput,
        context: ToolExecutionContext,
    ) -> ToolResult:
        return ToolResult(output="Exited plan mode.")
