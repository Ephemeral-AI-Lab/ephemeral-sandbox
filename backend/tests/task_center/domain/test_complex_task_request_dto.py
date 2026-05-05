"""Domain DTO tests for ComplexTaskRequest."""

from __future__ import annotations

from dataclasses import FrozenInstanceError
from datetime import UTC, datetime

import pytest

from task_center.mission.mission import (
    ComplexTaskCloseReport,
    ComplexTaskRequest,
    ComplexTaskRequestStatus,
)


def _request(**overrides) -> ComplexTaskRequest:
    base = dict(
        id="r1",
        task_center_run_id="run1",
        requested_by_task_id="t1",
        goal="goal",
        status=ComplexTaskRequestStatus.OPEN,
        task_segment_ids=(),
        final_outcome=None,
        created_at=datetime.now(UTC),
        updated_at=datetime.now(UTC),
        closed_at=None,
    )
    base.update(overrides)
    return ComplexTaskRequest(**base)


def test_is_open_matches_status():
    assert _request(status=ComplexTaskRequestStatus.OPEN).is_open is True
    assert _request(status=ComplexTaskRequestStatus.SUCCEEDED).is_open is False
    assert _request(status=ComplexTaskRequestStatus.FAILED).is_open is False
    assert _request(status=ComplexTaskRequestStatus.CANCELLED).is_open is False


def test_request_dto_is_frozen():
    req = _request()
    with pytest.raises(FrozenInstanceError):
        req.status = ComplexTaskRequestStatus.SUCCEEDED  # type: ignore[misc]


def test_close_report_constructs():
    rep = ComplexTaskCloseReport(
        complex_task_request_id="r1",
        requested_by_task_id="t1",
        outcome="success",
        final_segment_id="s1",
        final_harness_graph_id="g1",
    )
    assert rep.outcome == "success"
