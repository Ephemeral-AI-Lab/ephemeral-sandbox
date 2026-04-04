"""Tool for entering plan mode."""

from __future__ import annotations

from pydantic import BaseModel, Field

from ephemeralos.tools.base import BaseTool, ToolExecutionContext, ToolResult


class EnterPlanModeToolInput(BaseModel):
    """Arguments for entering plan mode."""

    goal: str = Field(description="High-level goal or objective for the plan")


class EnterPlanModeTool(BaseTool):
    """Enter plan mode to outline steps before implementation."""

    name = "enter_plan_mode"
    description = "Enter plan mode to create an implementation plan before writing code."
    input_model = EnterPlanModeToolInput

    async def execute(
        self,
        arguments: EnterPlanModeToolInput,
        context: ToolExecutionContext,
    ) -> ToolResult:
        return ToolResult(output=f"Entered plan mode. Goal: {arguments.goal}")
