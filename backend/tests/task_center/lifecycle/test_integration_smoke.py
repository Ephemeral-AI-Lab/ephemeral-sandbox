"""Manager/handler closure smoke with a synchronous graph closer."""

from __future__ import annotations

from collections.abc import Callable

from db.stores.harness_graph_store import HarnessGraphStore
from task_center.config import HarnessLifecycleConfig
from task_center.mission.handler import ComplexTaskRequestHandler
from task_center.episode.manager import TaskSegmentManager
from task_center.episode.registry import SegmentManagerRegistry
from task_center.mission.mission import ComplexTaskRequestStatus
from task_center.attempt import (
    HarnessGraph,
    HarnessGraphFailReason,
    HarnessGraphStatus,
)
from task_center.episode.episode import TaskSegmentStatus


class _StubOrchestrator:
    """Synchronous stand-in for HarnessGraphOrchestrator.

    Closes the graph immediately on ``start`` with a caller-supplied verdict.
    """

    def __init__(
        self,
        *,
        harness_graph: HarnessGraph,
        graph_store: HarnessGraphStore,
        on_graph_closed: Callable[[str], None],
        verdict: tuple[
            HarnessGraphStatus, HarnessGraphFailReason | None, str | None
        ],
    ) -> None:
        self._g = harness_graph
        self._gs = graph_store
        self._cb = on_graph_closed
        self._verdict = verdict

    def start(self) -> None:
        status, fail_reason, continuation_goal = self._verdict
        if continuation_goal is not None:
            self._gs.set_plan_contract(
                self._g.id,
                task_specification="stub-spec",
                evaluation_criteria=["stub-criterion"],
                continuation_goal=continuation_goal,
            )
        self._gs.close(self._g.id, status=status, fail_reason=fail_reason)
        self._cb(self._g.id)


def _build_handler(request_store, segment_store, graph_store):
    return ComplexTaskRequestHandler(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        manager_registry=SegmentManagerRegistry(),
        config=HarnessLifecycleConfig(default_attempt_budget=2),
    )


def _drive_segment(
    *,
    handler,
    segment_id: str,
    graph_store: HarnessGraphStore,
    verdict: tuple[
        HarnessGraphStatus, HarnessGraphFailReason | None, str | None
    ],
) -> None:
    """Run a stub orchestrator against the manager-owned segment."""
    registry = handler._manager_registry  # type: ignore[attr-defined]
    mgr: TaskSegmentManager | None = registry.get(segment_id)
    assert mgr is not None
    g = mgr.create_initial_attempt()
    stub = _StubOrchestrator(
        harness_graph=g,
        graph_store=graph_store,
        on_graph_closed=mgr.handle_attempt_closed,
        verdict=verdict,
    )
    stub.start()


def test_smoke_terminal_success(
    request_store, segment_store, graph_store, task_center_run_id
):
    handler = _build_handler(request_store, segment_store, graph_store)
    req = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="exec-1",
        goal="solve X",
    )
    seg = handler.create_initial_episode(complex_task_request_id=req.id)
    _drive_segment(
        handler=handler,
        segment_id=seg.id,
        graph_store=graph_store,
        verdict=(HarnessGraphStatus.PASSED, None, None),
    )
    final_request = request_store.get(req.id)
    final_segment = segment_store.get(seg.id)
    assert final_request is not None and final_segment is not None
    assert final_request.status == ComplexTaskRequestStatus.SUCCEEDED
    assert final_segment.status == TaskSegmentStatus.SUCCEEDED


def test_smoke_attempt_plan_failed(
    request_store, segment_store, graph_store, task_center_run_id
):
    handler = _build_handler(request_store, segment_store, graph_store)
    req = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="exec-1",
        goal="solve X",
    )
    seg = handler.create_initial_episode(complex_task_request_id=req.id)
    # First attempt: fail with a generator error.
    registry = handler._manager_registry  # type: ignore[attr-defined]
    mgr = registry.get(seg.id)
    assert mgr is not None
    g1 = mgr.create_initial_attempt()
    graph_store.set_plan_contract(
        g1.id, task_specification="spec1", evaluation_criteria=["a"], continuation_goal=None
    )
    graph_store.close(
        g1.id, status=HarnessGraphStatus.FAILED,
        fail_reason=HarnessGraphFailReason.GENERATOR_FAILED,
    )
    mgr.handle_attempt_closed(g1.id)
    # Second (and budget-final) attempt: also fail.
    seg_after = segment_store.get(seg.id)
    assert seg_after is not None
    g2_id = seg_after.harness_graph_ids[-1]
    graph_store.set_plan_contract(
        g2_id, task_specification="spec2", evaluation_criteria=["b"], continuation_goal=None
    )
    graph_store.close(
        g2_id, status=HarnessGraphStatus.FAILED,
        fail_reason=HarnessGraphFailReason.EVALUATOR_FAILED,
    )
    mgr.handle_attempt_closed(g2_id)
    final_request = request_store.get(req.id)
    final_segment = segment_store.get(seg.id)
    assert final_request is not None and final_segment is not None
    assert final_request.status == ComplexTaskRequestStatus.FAILED
    assert final_segment.status == TaskSegmentStatus.FAILED
    assert final_request.final_outcome is not None
    assert final_request.final_outcome["outcome"] == "failed"


def test_smoke_success_continue_then_terminal(
    request_store, segment_store, graph_store, task_center_run_id
):
    handler = _build_handler(request_store, segment_store, graph_store)
    req = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="exec-1",
        goal="initial-goal",
    )
    seg1 = handler.create_initial_episode(complex_task_request_id=req.id)
    _drive_segment(
        handler=handler,
        segment_id=seg1.id,
        graph_store=graph_store,
        verdict=(HarnessGraphStatus.PASSED, None, "next-goal"),
    )
    refreshed = request_store.get(req.id)
    assert refreshed is not None
    assert len(refreshed.task_segment_ids) == 2
    assert refreshed.is_open
    seg2_id = refreshed.task_segment_ids[1]
    seg2 = segment_store.get(seg2_id)
    assert seg2 is not None
    assert seg2.goal == "next-goal"
    # Drive segment 2 to terminal success.
    _drive_segment(
        handler=handler,
        segment_id=seg2_id,
        graph_store=graph_store,
        verdict=(HarnessGraphStatus.PASSED, None, None),
    )
    final_request = request_store.get(req.id)
    assert final_request is not None
    assert final_request.status == ComplexTaskRequestStatus.SUCCEEDED
