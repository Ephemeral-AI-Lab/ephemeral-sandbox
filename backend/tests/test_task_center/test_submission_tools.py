"""Unit tests for the submission tools."""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

import pytest

from task_center.graph import TaskGraph
from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.runtime import ExecutionMetadata
from tools.submission.submit_continue_to_work import (
    ContinueToWorkInput,
    submit_continue_to_work,
)
from tools.submission.submit_full_plan_handoff import (
    FullPlanHandoffInput,
    submit_full_plan_handoff,
)
from tools.submission.submit_partial_plan_handoff import (
    PartialPlanHandoffInput,
    submit_partial_plan_handoff,
)
from tools.submission.submit_task_completion import (
    TaskCompletionInput,
    submit_task_completion,
)


# --------------------------------------------------------------------------- #
# Fakes                                                                       #
# --------------------------------------------------------------------------- #


@dataclass
class _FakeTC:
    """Records submission calls; mirrors compile_dag for handoff inputs."""

    graph: TaskGraph = field(default_factory=TaskGraph)
    calls: list[tuple] = field(default_factory=list)

    def submit_task_completion(self, task_id, summary):
        self.calls.append(("complete", task_id, summary))

    def submit_full_handoff(self, task_id, tasks, task_specs, ac):
        from task_center.dag import compile_dag
        compile_dag(tasks, task_specs)  # raises PlanValidationError on bad input
        self.calls.append(("full", task_id, tasks, task_specs, ac))

    def submit_partial_handoff(self, task_id, tasks, task_specs, ac, note):
        from task_center.dag import compile_dag
        compile_dag(tasks, task_specs)
        self.calls.append(("partial", task_id, tasks, task_specs, ac, note))

    def submit_continue_to_work(self, evaluator_id, summary):
        self.calls.append(("continue", evaluator_id, summary))


def _ctx(tc: _FakeTC, *, task_id: str = "self", role: str = "executor") -> ToolExecutionContext:
    meta = ExecutionMetadata()
    meta["task_center"] = tc
    meta["task_id"] = task_id
    meta["role"] = role
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=meta)


# --------------------------------------------------------------------------- #
# submit_task_completion                                                      #
# --------------------------------------------------------------------------- #


@pytest.mark.asyncio
async def test_completion_calls_task_center() -> None:
    tc = _FakeTC()
    arg = TaskCompletionInput(summary="all good")
    res = await submit_task_completion.execute(arg, _ctx(tc, task_id="t1"))
    assert isinstance(res, ToolResult)
    assert res.is_error is False
    assert tc.calls == [("complete", "t1", "all good")]


@pytest.mark.asyncio
async def test_completion_missing_metadata() -> None:
    bad_ctx = ToolExecutionContext(cwd=Path("/tmp"), metadata=ExecutionMetadata())
    res = await submit_task_completion.execute(
        TaskCompletionInput(summary="x"), bad_ctx
    )
    assert res.is_error is True
    assert "missing" in res.output


# --------------------------------------------------------------------------- #
# submit_full_plan_handoff                                                    #
# --------------------------------------------------------------------------- #


@pytest.mark.asyncio
async def test_full_handoff_happy_path() -> None:
    tc = _FakeTC()
    arg = FullPlanHandoffInput(
        tasks=[{"id": "A"}, {"id": "B", "deps": ["A"]}],
        task_specs={"A": {"title": "A", "spec": "..."}, "B": {"title": "B", "spec": "..."}},
        acceptance_criteria="Both A and B complete.",
    )
    res = await submit_full_plan_handoff.execute(arg, _ctx(tc, task_id="parent"))
    assert res.is_error is False
    assert tc.calls[0][0] == "full"
    assert tc.calls[0][1] == "parent"


@pytest.mark.asyncio
async def test_full_handoff_rejects_cycle() -> None:
    """Invalid plan -> PlanValidationError -> tool returns is_error."""
    tc = _FakeTC()
    arg = FullPlanHandoffInput(
        tasks=[
            {"id": "A", "deps": ["B"]},
            {"id": "B", "deps": ["A"]},
        ],
        task_specs={"A": {"title": "A", "spec": "..."}, "B": {"title": "B", "spec": "..."}},
        acceptance_criteria="x",
    )
    res = await submit_full_plan_handoff.execute(arg, _ctx(tc, task_id="parent"))
    assert res.is_error is True
    assert "rejected" in res.output
    assert tc.calls == []


# --------------------------------------------------------------------------- #
# submit_partial_plan_handoff                                                 #
# --------------------------------------------------------------------------- #


@pytest.mark.asyncio
async def test_partial_handoff_requires_non_empty_note() -> None:
    from pydantic import ValidationError
    with pytest.raises(ValidationError):
        PartialPlanHandoffInput(
            tasks=[{"id": "A"}],
            task_specs={"A": {"title": "A", "spec": "..."}},
            acceptance_criteria="x",
            handoff_note="",
        )


@pytest.mark.asyncio
async def test_partial_handoff_passes_note_through() -> None:
    tc = _FakeTC()
    arg = PartialPlanHandoffInput(
        tasks=[{"id": "A"}],
        task_specs={"A": {"title": "A", "spec": "..."}},
        acceptance_criteria="x",
        handoff_note="covers half; gap = Y",
    )
    res = await submit_partial_plan_handoff.execute(arg, _ctx(tc, task_id="p"))
    assert res.is_error is False
    assert tc.calls[0][0] == "partial"
    assert tc.calls[0][-1] == "covers half; gap = Y"


# --------------------------------------------------------------------------- #
# submit_continue_to_work                                                     #
# --------------------------------------------------------------------------- #


@pytest.mark.asyncio
async def test_continue_rejects_executor_role() -> None:
    tc = _FakeTC()
    arg = ContinueToWorkInput(summary="gap")
    res = await submit_continue_to_work.execute(
        arg, _ctx(tc, task_id="x", role="executor")
    )
    assert res.is_error is True
    assert "evaluator-only" in res.output
    assert tc.calls == []


@pytest.mark.asyncio
async def test_continue_accepts_evaluator_role() -> None:
    tc = _FakeTC()
    arg = ContinueToWorkInput(summary="gap")
    res = await submit_continue_to_work.execute(
        arg, _ctx(tc, task_id="ev", role="evaluator")
    )
    assert res.is_error is False
    assert tc.calls == [("continue", "ev", "gap")]
