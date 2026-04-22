"""Tests for live tool schema summary rendering."""

from __future__ import annotations

from typing import Literal

from pydantic import BaseModel, Field, RootModel

from tools.core.base import BaseTool, BaseToolkit, ToolExecutionContext, ToolResult
from tools.core.schema_summary import collect_schema_toolkits, format_tool_schema_summary


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
        context: ToolExecutionContext,
    ) -> ToolResult:
        del arguments, context
        return ToolResult(output="ok")


def test_schema_summary_prints_live_input_and_output_models(tmp_path):
    toolkits = collect_schema_toolkits(cwd=tmp_path, caller_agent="team_planner")

    summary = format_tool_schema_summary(toolkits, include_descriptions=False)

    assert "Toolkit: code_intelligence" in summary
    assert "  ci_workspace_structure\n" in summary
    assert "      - max_depth: int [default 3]" in summary
    assert "      - paths: list[str] [default []]" in summary
    assert "  ci_query_symbol\n" in summary
    assert "      - definitions: list[CiSymbolDefinitionOutput] [default []]" in summary

    assert "Toolkit: task_center" in summary
    assert "  submit_task_note\n" in summary
    assert "      - tags: list[str] | null [default null]" in summary
    assert "      - note_id: str [required]" in summary
    assert "      - task_id: str [required]" in summary

    assert "Toolkit: submission" in summary
    assert "  submit_task_summary\n" in summary
    assert "      - type: \"success\" | \"request_replan\" [required]" in summary


def test_schema_summary_has_input_and_output_section_for_every_tool(tmp_path):
    toolkits = collect_schema_toolkits(cwd=tmp_path)
    summary = format_tool_schema_summary(toolkits, include_descriptions=False)

    for toolkit in toolkits:
        assert f"Toolkit: {toolkit.name}" in summary
        for tool in toolkit.list_tools():
            lines = summary.splitlines()
            start = lines.index(f"  {tool.name}")
            end = next(
                (
                    idx
                    for idx in range(start + 1, len(lines))
                    if lines[idx].startswith("  ") and not lines[idx].startswith("    ")
                ),
                len(lines),
            )
            block = "\n".join(lines[start:end])
            assert "    input:" in block
            assert "    output:" in block


def test_schema_summary_can_include_toolkit_instructions(tmp_path):
    toolkits = collect_schema_toolkits(cwd=tmp_path)

    summary = format_tool_schema_summary(
        toolkits,
        include_descriptions=False,
        include_instructions=True,
    )

    assert "Toolkit: code_intelligence" in summary
    assert "  instructions:" in summary
    assert "    Code intelligence for grounding same-run work." in summary


def test_daytona_summary_rejects_unprefixed_write_file_alias(tmp_path):
    toolkits = collect_schema_toolkits(cwd=tmp_path)

    summary = format_tool_schema_summary(
        toolkits,
        include_descriptions=True,
        include_instructions=True,
    )

    assert "Toolkit: sandbox_operations" in summary
    assert "there is no `write_file` tool" in summary
    assert "do not call `write_file`" in summary
    assert "output is captured automatically" in summary
    assert "2>/dev/null" in summary


def test_schema_summary_formats_literals_defaults_and_root_models():
    summary = format_tool_schema_summary(
        [
            BaseToolkit(
                name="synthetic",
                description="Synthetic toolkit.",
                tools=[_SyntheticTool()],
            )
        ],
        include_descriptions=True,
    )

    assert "Toolkit: synthetic" in summary
    assert "  Synthetic toolkit." in summary
    assert "  synthetic_tool" in summary
    assert "    description: Synthetic formatter coverage." in summary
    assert '      - mode: "fast" | "safe" [required] - Execution mode.' in summary
    assert (
        "      - labels: dict[str, str] [default {}] - Lookup labels."
        in summary
    )
    assert "      - seen: set[str] [default set()] - Visited ids." in summary
    assert "    output: str - Plain text synthetic result." in summary
