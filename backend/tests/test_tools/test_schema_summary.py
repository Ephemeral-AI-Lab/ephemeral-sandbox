"""Tests for live tool schema summary rendering."""

from __future__ import annotations

from typing import Literal

from pydantic import BaseModel, Field, RootModel

from tools.core.base import BaseTool, ToolExecutionContextService, ToolResult
from tools.core.schema_summary import collect_schema_tools, format_tool_schema_summary


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


def test_schema_summary_prints_live_input_and_output_models(tmp_path):
    tools = collect_schema_tools(cwd=tmp_path, caller_agent="analysis_agent")

    summary = format_tool_schema_summary(tools, include_descriptions=False)

    assert "Tool: ci_workspace_structure" in summary
    assert "      - max_depth: int [default 3]" in summary
    assert "      - paths: list[str] [default []]" in summary
    assert "Tool: ci_query_symbol" in summary
    assert "      - definitions: list[CiSymbolDefinitionOutput] [default []]" in summary

    assert "Tool: submit_task_success" not in summary
    assert "Tool: request_replan" not in summary
    assert "Tool: submit_replan" not in summary
    # ``submit_plan_handoff`` is the consolidated tool (US-006); the legacy
    # ``submit_full_plan_handoff`` and ``submit_partial_plan_handoff`` are gone.
    assert "Tool: submit_full_plan_handoff" not in summary
    assert "Tool: submit_partial_plan_handoff" not in summary

    # Executor-evaluator tree submission + mode-entry tools.
    assert "Tool: submit_task_completion" in summary
    assert "Tool: submit_plan_handoff" in summary
    assert "Tool: submit_continue_work_handoff" in summary
    assert "Tool: enter_plan_for_handoff" in summary
    assert "Tool: enter_prepare_continue_to_work" in summary


def test_schema_summary_has_input_and_output_section_for_every_tool(tmp_path):
    tools = collect_schema_tools(cwd=tmp_path)
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


def test_schema_summary_omits_instruction_blocks(tmp_path):
    tools = collect_schema_tools(cwd=tmp_path)

    summary = format_tool_schema_summary(tools, include_descriptions=False)

    assert "Tool: ci_query_symbol" in summary
    assert "  instructions:" not in summary


def test_sandbox_summary_lists_unprefixed_tools_without_instruction_block(tmp_path):
    tools = collect_schema_tools(cwd=tmp_path)

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
