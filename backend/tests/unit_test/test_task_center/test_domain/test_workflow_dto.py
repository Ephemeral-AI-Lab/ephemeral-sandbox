"""Domain DTO tests for Workflow."""

from __future__ import annotations

from dataclasses import FrozenInstanceError
from datetime import UTC, datetime

import pytest

from task_center.workflow.state import (
    WorkflowClosureReport,
    WorkflowClosureDeliveryResult,
    WorkflowClosureDeliveryStatus,
    Workflow,
    WorkflowOriginKind,
    WorkflowStatus,
)


def _request(**overrides) -> Workflow:
    base = dict(
        id="r1",
        task_center_run_id="run1",
        requested_by_task_id="t1",
        goal="goal",
        status=WorkflowStatus.OPEN,
        iteration_ids=(),
        final_outcome=None,
        created_at=datetime.now(UTC),
        updated_at=datetime.now(UTC),
        closed_at=None,
    )
    base.update(overrides)
    return Workflow(**base)


def test_is_open_matches_status():
    assert _request(status=WorkflowStatus.OPEN).is_open is True
    assert _request(status=WorkflowStatus.SUCCEEDED).is_open is False
    assert _request(status=WorkflowStatus.FAILED).is_open is False
    assert _request(status=WorkflowStatus.CANCELLED).is_open is False


def test_request_dto_is_frozen():
    req = _request()
    with pytest.raises(FrozenInstanceError):
        req.status = WorkflowStatus.SUCCEEDED  # type: ignore[misc]


def test_closure_report_constructs():
    rep = WorkflowClosureReport(
        workflow_id="r1",
        task_center_run_id="run1",
        origin_kind=WorkflowOriginKind.TASK,
        requested_by_task_id="t1",
        outcome="success",
        final_iteration_id="s1",
        final_attempt_id="g1",
    )
    assert rep.outcome == "success"


def test_goal_closure_delivery_result_constructs():
    status: WorkflowClosureDeliveryStatus = "delivered"
    result = WorkflowClosureDeliveryResult(
        status=status,
        requested_by_task_id="t1",
        parent_attempt_id="a1",
    )

    assert result.status == "delivered"
