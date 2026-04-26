"""Mode-entry tool: evaluator commits to prepare_continue_to_work mode."""

from __future__ import annotations

from pydantic import BaseModel

from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.decorator import tool
from tools.mode_tool._mode_entry import enter_secondary_mode
from tools.mode_tool._models import SubmissionOutput


class EnterPrepareContinueToWorkInput(BaseModel):
    """Mode-entry tools take no arguments — entry is the commitment itself."""

    pass


@tool(
    name="enter_prepare_continue_to_work",
    description=(
        "Mode-entry (evaluator-only): commit to prepare_continue_to_work mode "
        "and read the briefing. From this mode the only exit is "
        "submit_continue_work_handoff. Idempotent if already in "
        "prepare_continue_to_work. Rejects from a subagent context or if the "
        "task is already in any other secondary mode."
    ),
    input_model=EnterPrepareContinueToWorkInput,
    output_model=SubmissionOutput,
    is_mode_entry_tool=True,
)
async def enter_prepare_continue_to_work(
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    return enter_secondary_mode(
        context,
        target_mode="prepare_continue_to_work",
        required_role="evaluator",
        briefing="""\
You have entered prepare_continue_to_work mode. This is a one-way commitment:
the only way out is to call submit_continue_work_handoff with continuation input.

Purpose
  You have judged the parent task's acceptance_criteria as not yet satisfied.
  Prepare the gap analysis that will drive the continuation executor — your
  summary is its input.

Allowed tools (read-only investigation)
  - read_file, grep, glob
  - ci_query_symbol, ci_diagnostics, ci_workspace_structure

Terminal tool
  - submit_continue_work_handoff — submit continuation input and exit this mode.

Required field on submit_continue_work_handoff
  - task_input: which acceptance_criteria items remain unmet, what evidence
    proves the gap, and what the continuation executor should focus on.

You cannot edit, write, run shell commands, spawn subagents, or call any
other terminal in this mode. The dispatcher will reject any tool that is
not in the allowed list above. To leave this mode, call
submit_continue_work_handoff with continuation input.
""",
        tool_name="enter_prepare_continue_to_work",
    )
