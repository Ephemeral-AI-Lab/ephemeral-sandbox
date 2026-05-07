"""Tests for live tool schema summary rendering."""

from __future__ import annotations

from typing import Literal

from pydantic import BaseModel, Field, RootModel

from tools.core.base import BaseTool, ToolExecutionContextService, ToolResult
from tools.introspection.schema_summary import collect_schema_tools, format_tool_schema_summary


class _SyntheticInput(BaseModel):
    mode: Literal["fast", "safe"] = Field(description="Execution mode.")
    labels: dict[str, str] = Field(default_factory=dict, description="Lookup labels.")
    seen: set[str] = Field(default_factory=set, description="Visited ids.")


class _SyntheticOutput(RootModel[str]):
    """Plain text synthetic result."""


class _SyntheticTool(BaseTool):
    name = "synthetic_tool"
    description = "Synthetic formatter coverage."
    input_model = _SyntheticInput
    output_model = _SyntheticOutput

    async def execute(
        self,
        arguments: BaseModel,
        context: ToolExecutionContextService,
    ) -> ToolResult:
        del arguments, context
        return ToolResult(output="ok")


def test_schema_summary_prints_live_input_and_output_models():
    tools = collect_schema_tools(caller_agent="analysis_agent")

    summary = format_tool_schema_summary(tools, include_descriptions=False)

    assert "Tool: submit_task_completion" not in summary
    assert "Tool: submit_continue_work_handoff" not in summary
    assert "Tool: request_replan" not in summary
    assert "Tool: submit_replan" not in summary
    assert "Tool: submit_full_plan_handoff" not in summary
    assert "Tool: submit_partial_plan_handoff" not in summary
    # Stage 7 of the four-role roadmap: legacy submit_plan_handoff dropped.
    assert "Tool: submit_plan_handoff" not in summary

    # Obsolete TaskCenter terminal tools remain absent from the default registry.
    assert "Tool: submit_task_success" not in summary
    assert "Tool: submit_task_failure" not in summary
    assert "Tool: request_plan" not in summary
    assert "Tool: enter_plan_for_handoff" not in summary
    assert "Tool: enter_prepare_continue_to_work" not in summary

    # Phase 03 submission tools are registered globally; agent definitions
    # still filter their visible tool surfaces.
    assert "Tool: submit_full_plan" in summary
    assert "Tool: submit_evaluation_success" in summary
    assert "Tool: submit_advisor_feedback" in summary


def test_schema_summary_has_input_and_output_section_for_every_tool():
    tools = collect_schema_tools()
    summary = format_tool_schema_summary(tools, include_descriptions=False)

    for tool in tools:
        assert f"Tool: {tool.name}" in summary
        lines = summary.splitlines()
        start = lines.index(f"Tool: {tool.name}")
        end = next(
            (
                idx
                for idx in range(start + 1, len(lines))
                if lines[idx].startswith("Tool: ")
            ),
            len(lines),
        )
        block = "\n".join(lines[start:end])
        assert "    input:" in block
        assert "    output:" in block


def test_schema_summary_omits_instruction_blocks():
    tools = collect_schema_tools()

    summary = format_tool_schema_summary(tools, include_descriptions=False)

    assert "Tool: read_file" in summary
    assert "  instructions:" not in summary


def test_sandbox_summary_lists_unprefixed_tools_without_instruction_block():
    tools = collect_schema_tools()

    summary = format_tool_schema_summary(tools, include_descriptions=True)

    assert "Tool: write_file" in summary
    assert "Tool: shell" in summary
    assert "Tool: daytona_write_file" not in summary
    assert "  instructions:" not in summary


def test_schema_summary_formats_literals_defaults_and_root_models():
    summary = format_tool_schema_summary(
        [_SyntheticTool()],
        include_descriptions=True,
    )

    assert "Tool: synthetic_tool" in summary
    assert "  description: Synthetic formatter coverage." in summary
    assert '      - mode: "fast" | "safe" [required] - Execution mode.' in summary
    assert (
        "      - labels: dict[str, str] [default {}] - Lookup labels."
        in summary
    )
    assert "      - seen: set[str] [default set()] - Visited ids." in summary
    assert "    output: str - Plain text synthetic result." in summary
