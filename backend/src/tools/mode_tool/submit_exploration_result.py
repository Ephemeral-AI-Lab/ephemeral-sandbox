"""Terminal tool (subagent-only): return findings to the parent agent."""

from __future__ import annotations

from pydantic import BaseModel, Field

from tools.core.base import TextToolOutput, ToolExecutionContextService, ToolResult
from tools.core.decorator import tool


class ExplorationResultInput(BaseModel):
    findings: str = Field(
        ...,
        min_length=1,
        description=(
            "Free-form findings text returned to the parent agent verbatim. "
            "Include any structured payload as text inside this field — the "
            "parent receives it as the run_subagent tool result."
        ),
    )


@tool(
    name="submit_exploration_result",
    description=(
        "Terminal: return your findings to the parent agent. The findings "
        "string becomes the run_subagent tool result the parent reads. Call "
        "this exactly once when your work is complete."
    ),
    input_model=ExplorationResultInput,
    output_model=TextToolOutput,
    is_terminal_tool=True,
)
async def submit_exploration_result(
    findings: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    del context
    return ToolResult(output=findings)
