"""Submission tools for the executor and evaluator agents.

Exports :func:`make_submission_tools` returning the four BaseTool instances
that should be registered in the global tool factory.
"""

from __future__ import annotations

from tools.core.base import BaseTool
from tools.submission.submit_continue_to_work import submit_continue_to_work
from tools.submission.submit_full_plan_handoff import submit_full_plan_handoff
from tools.submission.submit_partial_plan_handoff import submit_partial_plan_handoff
from tools.submission.submit_task_completion import submit_task_completion


def make_submission_tools() -> list[BaseTool]:
    """Return the four submission tools as BaseTool instances."""
    return [
        submit_task_completion,
        submit_full_plan_handoff,
        submit_partial_plan_handoff,
        submit_continue_to_work,
    ]


__all__ = [
    "make_submission_tools",
    "submit_continue_to_work",
    "submit_full_plan_handoff",
    "submit_partial_plan_handoff",
    "submit_task_completion",
]
