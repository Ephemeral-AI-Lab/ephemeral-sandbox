"""Mode and terminal tools for executor, evaluator, and explorer agents.

Exports :func:`make_mode_tools` returning the BaseTool instances that
should be registered in the global tool factory.
"""

from __future__ import annotations

from tools.core.base import BaseTool
from tools.mode_tool.enter_plan_for_handoff import enter_plan_for_handoff
from tools.mode_tool.enter_prepare_continue_to_work import (
    enter_prepare_continue_to_work,
)
from tools.mode_tool.submit_continue_work_handoff import submit_continue_work_handoff
from tools.mode_tool.submit_exploration_result import submit_exploration_result
from tools.mode_tool.submit_plan_handoff import submit_plan_handoff
from tools.mode_tool.submit_task_completion import submit_task_completion


def make_mode_tools() -> list[BaseTool]:
    """Return the mode-entry and terminal tools as BaseTool instances."""
    return [
        submit_task_completion,
        submit_plan_handoff,
        submit_continue_work_handoff,
        submit_exploration_result,
        enter_plan_for_handoff,
        enter_prepare_continue_to_work,
    ]


__all__ = [
    "enter_plan_for_handoff",
    "enter_prepare_continue_to_work",
    "make_mode_tools",
    "submit_continue_work_handoff",
    "submit_exploration_result",
    "submit_plan_handoff",
    "submit_task_completion",
]
