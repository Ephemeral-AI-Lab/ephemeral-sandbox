"""One-shot briefings returned by the mode-entry tools.

Each briefing is the full ``tool_result`` body that lands in the agent's
conversation history when an entry tool succeeds. The system prompt does
NOT change across modes; the briefing is the only mode-specific framing.
It is delivered exactly once at entry — when the agent later strays, the
authorization gate's deny message is the focused reminder.

The allowed-tool lists in each briefing are the EXACT names the gate will
admit. Keep this file in sync with the matching mode's
``ModeDefinition.allowed_tools`` in :mod:`agents.builtins` — divergence
breaks the "one-shot briefing" contract.

See ``docs/architecture/agent-mode-system-v1.md`` for the design rationale.
"""

from __future__ import annotations


PLAN_FOR_HANDOFF_BRIEFING = """\
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
"""


PREPARE_CONTINUE_TO_WORK_BRIEFING = """\
You have entered prepare_continue_to_work mode. This is a one-way commitment:
the only way out is to call submit_continue_work_handoff with continuation input.

Purpose
  You have judged the parent task's acceptance_criteria as not yet satisfied.
  Prepare the gap analysis that will drive the continuation executor.

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
"""


__all__ = [
    "PLAN_FOR_HANDOFF_BRIEFING",
    "PREPARE_CONTINUE_TO_WORK_BRIEFING",
]
