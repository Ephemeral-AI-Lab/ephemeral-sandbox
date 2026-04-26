"""Mode-entry tool: executor commits to plan_for_handoff mode."""

from __future__ import annotations

from pydantic import BaseModel

from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.decorator import tool
from tools.mode_tool._mode_entry import enter_secondary_mode
from tools.mode_tool._models import SubmissionOutput


class EnterPlanForHandoffInput(BaseModel):
    """Mode-entry tools take no arguments — entry is the commitment itself."""

    pass


@tool(
    name="enter_plan_for_handoff",
    description=(
        "Mode-entry (executor-only): commit to plan_for_handoff mode and read "
        "the briefing. From this mode the only exit is submit_plan_handoff. "
        "Idempotent if already in plan_for_handoff. Rejects from a subagent "
        "context or if the task is already in any other secondary mode."
    ),
    input_model=EnterPlanForHandoffInput,
    output_model=SubmissionOutput,
    is_mode_entry_tool=True,
)
async def enter_plan_for_handoff(*, context: ToolExecutionContextService) -> ToolResult:
    return enter_secondary_mode(
        context,
        target_mode="plan_for_handoff",
        required_role="executor",
        briefing="""\
You have entered plan_for_handoff mode. This is a one-way commitment: the only
way out is to call submit_plan_handoff with a complete DAG plan.

Purpose
  Decompose the task into a DAG of child executors. Your output is the plan
  itself — the evaluator will validate the children's combined work against
  the acceptance_criteria you submit.

Allowed tools (read-only investigation)
  - read_file, grep, glob
  - ci_query_symbol, ci_diagnostics, ci_workspace_structure

Terminal tool
  - submit_plan_handoff — submit the DAG plan and exit this mode.

Required fields on submit_plan_handoff
  - tasks: flat DAG entries {id, deps}; transitive deps are implicit.
  - task_specs: map of id -> {title, task_input} for every task above.
  - acceptance_criteria: the closure contract the evaluator will check.
  - handoff_note: articulate what the plan covers, what risks remain, and
    which acceptance_criteria items are most fragile. The evaluator reads
    this before validating child outputs.

You cannot edit, write, run shell commands, spawn subagents, or call any
other terminal in this mode. The dispatcher will reject any tool that is
not in the allowed list above. To leave this mode, call
submit_plan_handoff with a well-formed plan.
""",
        tool_name="enter_plan_for_handoff",
    )
