"""submit_exploration_result terminal tool."""

from __future__ import annotations

from pydantic import BaseModel, Field

from tools._framework.core.context import ToolExecutionContextService
from sandbox._shared.models import Intent
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput, ToolResult
from .prompt import (
    get_submit_exploration_result_description,
)


class SubmitExplorationResultInput(BaseModel):
    summary: str = Field(..., min_length=1)
    findings: list[str] = Field(default_factory=list)
    references: list[str] = Field(default_factory=list)


@tool(
    name="submit_exploration_result",
    description=get_submit_exploration_result_description(),
    input_model=SubmitExplorationResultInput,
    output_model=TextToolOutput,
    intent=Intent.READ_ONLY,
    is_terminal_tool=True,
)
async def submit_exploration_result(
    summary: str,
    findings: list[str],
    references: list[str],
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    del context
    return ToolResult(
        output=summary,
        metadata={
            "subagent_role": "explorer",
            "findings": findings,
            "references": references,
        },
    )
