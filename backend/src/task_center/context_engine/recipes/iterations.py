"""Goal + iteration scaffold blocks for the planner recipe.

Owns :func:`goal_iteration_blocks` — the standalone ``<goal>`` block followed
by prior + current iteration groups. Living in its own module keeps consuming
recipe modules independent of each other. (The evaluator recipe no longer uses
this scaffold — see ``recipes/evaluator.py`` for its flat current-attempt shape.)

The XML structure produced by this module is the same for every iteration:

* Standalone ``<goal>`` block.
* One ``<iteration iteration_no="K" status="prior">`` group per prior closed
  iteration (each wrapping ``<accepted_plan>`` + ``<summary>``).
* The current iteration's group ``<iteration iteration_no="N" status="current">``
  containing an ``<iteration_goal>`` child. For iteration 1 the
  ``<iteration_goal>`` body reads ``(identical to <goal>)`` rather than
  duplicating the goal text.

Failed prior attempts (:func:`failed_attempt_blocks`) join the current
iteration's group by sharing :func:`current_iteration_group_id`.
"""

from __future__ import annotations

from task_center.context_engine.exceptions import ContextEngineError
from task_center.context_engine.packet import (
    ContextBlock,
    ContextBlockKind,
    ContextPriority,
)
from task_center.iteration.state import Iteration
from task_center.goal.state import Goal


def current_iteration_group_id(iteration: Iteration) -> str:
    """Shared group key for blocks wrapped in ``<iteration status="current">``."""
    return f"iteration_{iteration.sequence_no}_current"


def current_iteration_group_attrs(iteration: Iteration) -> str:
    return f'iteration_no="{iteration.sequence_no}" status="current"'


# For iteration 1 the iteration goal equals the user's request. Echoing the
# full text twice is pure noise; the planner / evaluator skills know what
# this marker means.
_ITERATION_GOAL_IDENTITY_BODY = "(identical to &lt;goal&gt;)"


def goal_iteration_blocks(
    *,
    goal: Goal,
    current_iteration: Iteration,
    iterations: list[Iteration],
) -> list[ContextBlock]:
    """Return the goal/iteration frame in LLM-facing semantic order.

    Always emits ``<goal>`` followed by zero or more ``<iteration status="prior">``
    groups and the ``<iteration status="current">`` group with its
    ``<iteration_goal>`` child. Iteration 1's iteration goal collapses to the
    literal marker ``(identical to <goal>)``.
    """
    blocks: list[ContextBlock] = [_goal_statement_block(goal)]
    blocks.extend(_prior_iteration_blocks(current=current_iteration, iterations=iterations))
    blocks.append(_current_iteration_goal_child(current_iteration))
    return blocks


def _goal_statement_block(goal: Goal) -> ContextBlock:
    """Standalone ``<goal>`` block (every iteration)."""
    return ContextBlock(
        kind=ContextBlockKind.GOAL_STATEMENT,
        priority=ContextPriority.REQUIRED,
        text=goal.goal,
        source_id=goal.id,
        source_kind="goal",
        metadata={"tag": "goal"},
    )


def _current_iteration_goal_child(iteration: Iteration) -> ContextBlock:
    """Child block inside ``<iteration status="current">``: ``<iteration_goal>``.

    For iteration 1 the body collapses to ``(identical to <goal>)`` since the
    iteration scope is the user's request verbatim.
    """
    if iteration.sequence_no == 1:
        body = _ITERATION_GOAL_IDENTITY_BODY
    else:
        body = iteration.goal
    return ContextBlock(
        kind=ContextBlockKind.ITERATION_STATEMENT,
        priority=ContextPriority.REQUIRED,
        text=body,
        source_id=iteration.id,
        source_kind="iteration",
        metadata={
            "group_id": current_iteration_group_id(iteration),
            "group_tag": "iteration",
            "group_attrs": current_iteration_group_attrs(iteration),
            "child_tag": "iteration_goal",
            "iteration_no": str(iteration.sequence_no),
        },
    )


def _prior_iteration_blocks(
    *,
    current: Iteration,
    iterations: list[Iteration],
) -> list[ContextBlock]:
    priors = sorted(
        (s for s in iterations if s.sequence_no < current.sequence_no),
        key=lambda s: s.sequence_no,
    )
    out: list[ContextBlock] = []
    immediate_prior = current.sequence_no - 1
    for prior in priors:
        if prior.plan_spec is None or prior.task_summary is None:
            raise ContextEngineError(
                f"Prior iteration {prior.id!r} (seq={prior.sequence_no}) is "
                "missing plan_spec or task_summary; chain integrity violated."
            )
        priority = (
            ContextPriority.HIGH if prior.sequence_no == immediate_prior else ContextPriority.MEDIUM
        )
        group_id = f"iteration_{prior.sequence_no}_prior"
        group_attrs = f'iteration_no="{prior.sequence_no}" status="prior"'
        out.append(
            ContextBlock(
                kind=ContextBlockKind.PRIOR_ITERATION_SPECIFICATION,
                priority=priority,
                text=prior.plan_spec,
                source_id=prior.id,
                source_kind="iteration",
                metadata={
                    "group_id": group_id,
                    "group_tag": "iteration",
                    "group_attrs": group_attrs,
                    "child_tag": "accepted_plan",
                },
            )
        )
        out.append(
            ContextBlock(
                kind=ContextBlockKind.PRIOR_ITERATION_SUMMARY,
                priority=priority,
                text=prior.task_summary,
                source_id=prior.id,
                source_kind="iteration",
                metadata={
                    "group_id": group_id,
                    "group_tag": "iteration",
                    "group_attrs": group_attrs,
                    "child_tag": "summary",
                },
            )
        )
    return out
