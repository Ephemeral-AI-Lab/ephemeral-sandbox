"""Tool-name constants.

Single source of truth for tool names referenced across `_prompt.py`
modules. Keep this file literals-only — no imports — so any tool module
can import these constants without risking circular imports.
"""

from __future__ import annotations

# Workspace tools (backend/src/tools/sandbox/)
SHELL_TOOL_NAME = "shell"
READ_FILE_TOOL_NAME = "read_file"
EDIT_FILE_TOOL_NAME = "edit_file"
WRITE_FILE_TOOL_NAME = "write_file"
GREP_TOOL_NAME = "grep"
GLOB_TOOL_NAME = "glob"

# Subagent
RUN_SUBAGENT_TOOL_NAME = "run_subagent"

# Background task tools (backend/src/tools/background/)
CHECK_BACKGROUND_TASK_RESULT_TOOL_NAME = "check_background_task_result"
WAIT_BACKGROUND_TASKS_TOOL_NAME = "wait_background_tasks"
CANCEL_BACKGROUND_TASK_TOOL_NAME = "cancel_background_task"

# Helper-ask tools (backend/src/tools/ask_helper/)
ASK_ADVISOR_TOOL_NAME = "ask_advisor"

# Executor terminal tools
SUBMIT_EXECUTION_SUCCESS_TOOL_NAME = "submit_execution_success"
SUBMIT_EXECUTION_BLOCKER_TOOL_NAME = "submit_execution_blocker"
SUBMIT_EXECUTION_HANDOFF_TOOL_NAME = "submit_execution_handoff"

# Verifier terminal tools
SUBMIT_VERIFICATION_SUCCESS_TOOL_NAME = "submit_verification_success"
SUBMIT_VERIFICATION_FAILURE_TOOL_NAME = "submit_verification_failure"

# Evaluator terminal tools
SUBMIT_EVALUATION_SUCCESS_TOOL_NAME = "submit_evaluation_success"
SUBMIT_EVALUATION_FAILURE_TOOL_NAME = "submit_evaluation_failure"

# Planner terminal tools
SUBMIT_PLAN_CLOSES_GOAL_TOOL_NAME = "submit_plan_closes_goal"
SUBMIT_PLAN_DEFERS_GOAL_TOOL_NAME = "submit_plan_defers_goal"

# Advisor terminal tool
SUBMIT_ADVISOR_FEEDBACK_TOOL_NAME = "submit_advisor_feedback"

# Explorer terminal tool
SUBMIT_EXPLORATION_RESULT_TOOL_NAME = "submit_exploration_result"


__all__ = [
    "SHELL_TOOL_NAME",
    "READ_FILE_TOOL_NAME",
    "EDIT_FILE_TOOL_NAME",
    "WRITE_FILE_TOOL_NAME",
    "GREP_TOOL_NAME",
    "GLOB_TOOL_NAME",
    "RUN_SUBAGENT_TOOL_NAME",
    "CHECK_BACKGROUND_TASK_RESULT_TOOL_NAME",
    "WAIT_BACKGROUND_TASKS_TOOL_NAME",
    "CANCEL_BACKGROUND_TASK_TOOL_NAME",
    "ASK_ADVISOR_TOOL_NAME",
    "SUBMIT_EXECUTION_SUCCESS_TOOL_NAME",
    "SUBMIT_EXECUTION_BLOCKER_TOOL_NAME",
    "SUBMIT_EXECUTION_HANDOFF_TOOL_NAME",
    "SUBMIT_VERIFICATION_SUCCESS_TOOL_NAME",
    "SUBMIT_VERIFICATION_FAILURE_TOOL_NAME",
    "SUBMIT_EVALUATION_SUCCESS_TOOL_NAME",
    "SUBMIT_EVALUATION_FAILURE_TOOL_NAME",
    "SUBMIT_PLAN_CLOSES_GOAL_TOOL_NAME",
    "SUBMIT_PLAN_DEFERS_GOAL_TOOL_NAME",
    "SUBMIT_ADVISOR_FEEDBACK_TOOL_NAME",
    "SUBMIT_EXPLORATION_RESULT_TOOL_NAME",
]
