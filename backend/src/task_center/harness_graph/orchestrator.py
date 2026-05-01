"""HarnessGraphOrchestrator state machine."""

from __future__ import annotations

import logging
from collections.abc import Callable
from dataclasses import asdict
from datetime import UTC, datetime

from task_center.complex_task.request import ComplexTaskCloseReport
from task_center.exceptions import GraphInvariantViolation
from task_center.harness_graph.dispatcher import HarnessGraphDispatcher
from task_center.harness_graph.graph import (
    HarnessGraph,
    HarnessGraphFailReason,
    HarnessGraphStage,
    HarnessGraphStatus,
)
from task_center.harness_graph.runtime import (
    HarnessAgentLaunch,
    HarnessGraphRuntime,
)
from task_center.task import (
    EvaluatorSubmission,
    GeneratorSubmission,
    HarnessTaskRole,
    HarnessTaskStatus,
    PlannedGeneratorTask,
    PlannerFailureSubmission,
    PlannerSubmission,
    generator_task_id,
    planner_task_id,
)
from task_center.harness_graph.task_graph import (
    dependency_task_ids,
    ordered_generator_tasks,
)
from task_center.harness_graph.validation import (
    assert_evaluator_task_for_submission,
    assert_generator_task_for_submission,
    assert_graph_not_closed,
    assert_graph_stage,
    assert_task_belongs_to_graph,
    assert_valid_graph_close,
)

logger = logging.getLogger(__name__)


class HarnessGraphOrchestrator:
    """Runs one planner -> generator DAG -> evaluator harness graph."""

    def __init__(
        self,
        *,
        harness_graph: HarnessGraph,
        on_graph_closed: Callable[[str], None],
        runtime: HarnessGraphRuntime,
    ) -> None:
        self._harness_graph = harness_graph
        self._on_graph_closed = on_graph_closed
        self._runtime = runtime

        def _close_graph_callback(
            status: HarnessGraphStatus,
            fail_reason: HarnessGraphFailReason | None,
        ) -> None:
            self._close_graph(status=status, fail_reason=fail_reason)

        self._dispatcher = HarnessGraphDispatcher(
            harness_graph_id=harness_graph.id,
            runtime=runtime,
            close_graph=_close_graph_callback,
        )

    @property
    def harness_graph_id(self) -> str:
        return self._harness_graph.id

    def start(self) -> None:
        runtime = self._runtime
        graph = self._assert_stage(HarnessGraphStage.PLANNING)
        if graph.status != HarnessGraphStatus.RUNNING:
            raise GraphInvariantViolation(
                f"HarnessGraph {graph.id!r} is not running"
            )
        if graph.planner_task_id is not None:
            raise GraphInvariantViolation(
                f"HarnessGraph {graph.id!r} already has a planner task"
            )

        task_id = planner_task_id(graph.id)
        runtime.orchestrator_registry.register(self)
        try:
            task_input = runtime.task_input_for_graph(graph)
            task_center_run_id = runtime.task_center_run_id_for_graph(graph)
            runtime.task_store.upsert_task(
                task_id=task_id,
                task_center_run_id=task_center_run_id,
                role=HarnessTaskRole.PLANNER.value,
                agent_name=HarnessTaskRole.PLANNER.value,
                task_input=task_input,
                status=HarnessTaskStatus.RUNNING.value,
                summaries=[],
                needs=[],
                task_center_harness_graph_id=graph.id,
                spawn_reason="harness_graph_planner",
            )
            runtime.graph_store.set_planner_task_id(graph.id, task_id)
            runtime.agent_launcher.launch(
                HarnessAgentLaunch(
                    task_id=task_id,
                    task_center_run_id=task_center_run_id,
                    harness_graph_id=graph.id,
                    role=HarnessTaskRole.PLANNER,
                    agent_name=HarnessTaskRole.PLANNER.value,
                    task_input=task_input,
                    needs=(),
                )
            )
            self._dispatcher.dispatch_ready_work()
        except Exception:
            self._mark_startup_failed(planner_task_id=task_id)
            raise

    def apply_plan_submission(self, submission: PlannerSubmission) -> None:
        self._assert_submission_graph(submission.graph_id)
        graph = self._assert_stage(HarnessGraphStage.PLANNING)
        if graph.planner_task_id != submission.planner_task_id:
            raise GraphInvariantViolation(
                f"Planner submission task {submission.planner_task_id!r} does "
                f"not match graph planner {graph.planner_task_id!r}"
            )
        if submission.kind == "full" and submission.continuation_goal is not None:
            raise GraphInvariantViolation("Full plans cannot set continuation_goal")
        if submission.kind == "partial" and submission.continuation_goal is None:
            raise GraphInvariantViolation("Partial plans require continuation_goal")

        runtime = self._runtime
        planner_task = runtime.task_store.get_task(submission.planner_task_id)
        if planner_task is None:
            raise GraphInvariantViolation(
                f"Planner task {submission.planner_task_id!r} not found"
            )
        assert_task_belongs_to_graph(planner_task, graph)
        if planner_task["role"] != HarnessTaskRole.PLANNER.value:
            raise GraphInvariantViolation(
                f"Task {submission.planner_task_id!r} is not a planner task"
            )

        runtime.task_store.set_task_status(
            submission.planner_task_id,
            status=HarnessTaskStatus.DONE.value,
            summary={
                "kind": submission.kind,
                "summary": submission.summary,
            },
        )
        self._persist_plan_contract(submission)
        generator_ids = self._persist_generator_tasks(submission.tasks)
        runtime.graph_store.set_generator_task_ids(graph.id, list(generator_ids))
        runtime.graph_store.set_stage(graph.id, HarnessGraphStage.GENERATING)
        self._dispatcher.dispatch_ready_work()

    def apply_planner_failure(
        self, submission: PlannerFailureSubmission
    ) -> None:
        self._assert_submission_graph(submission.graph_id)
        graph = self._assert_stage(HarnessGraphStage.PLANNING)
        if graph.planner_task_id != submission.planner_task_id:
            raise GraphInvariantViolation(
                f"Planner failure task {submission.planner_task_id!r} does not "
                f"match graph planner {graph.planner_task_id!r}"
            )
        runtime = self._runtime
        planner_task = runtime.task_store.get_task(submission.planner_task_id)
        if planner_task is None:
            raise GraphInvariantViolation(
                f"Planner task {submission.planner_task_id!r} not found"
            )
        assert_task_belongs_to_graph(planner_task, graph)
        runtime.task_store.set_task_status(
            submission.planner_task_id,
            status=HarnessTaskStatus.FAILED.value,
            summary={
                "fail_reason": submission.fail_reason,
                "summary": submission.summary,
            },
        )
        self._close_graph(
            status=HarnessGraphStatus.FAILED,
            fail_reason=HarnessGraphFailReason.PLANNER_FAILED,
        )

    def apply_generator_submission(
        self, submission: GeneratorSubmission
    ) -> None:
        self._assert_submission_graph(submission.graph_id)
        self._mark_generator(submission)
        if submission.outcome == "failure":
            self._dispatcher.block_failed_descendants(submission.task_id)
        self._dispatcher.dispatch_ready_work()

    def apply_evaluator_submission(
        self, submission: EvaluatorSubmission
    ) -> None:
        self._assert_submission_graph(submission.graph_id)
        self._mark_evaluator(submission)
        self._dispatcher.dispatch_ready_work()

    def apply_complex_task_close_report(self, report: ComplexTaskCloseReport) -> None:
        """Resume a generator task waiting on a delegated complex-task request.

        Idempotent: if the parent has already been resumed (status moved off
        ``waiting_complex_task`` by an earlier delivery), return silently
        without re-asserting graph stage or appending another summary.
        """
        runtime = self._runtime
        task = runtime.task_store.get_task(report.requested_by_task_id)
        if task is None:
            raise GraphInvariantViolation(
                f"Generator task {report.requested_by_task_id!r} not found"
            )
        if task.get("status") != HarnessTaskStatus.WAITING_COMPLEX_TASK.value:
            # Already delivered; no further action.
            return

        graph = self._assert_stage(HarnessGraphStage.GENERATING)
        assert_generator_task_for_submission(task, graph)

        if report.outcome == "success":
            status = HarnessTaskStatus.DONE
            summary = (
                f"Delegated complex task {report.complex_task_request_id} succeeded."
            )
        else:
            status = HarnessTaskStatus.FAILED
            summary = (
                f"Delegated complex task {report.complex_task_request_id} failed."
            )

        updated = runtime.task_store.set_task_status_if_current(
            report.requested_by_task_id,
            expected_status=HarnessTaskStatus.WAITING_COMPLEX_TASK.value,
            status=status.value,
            summary={
                "outcome": report.outcome,
                "summary": summary,
                "payload": {
                    "complex_task_close_report": asdict(report),
                    "submission_kind": "complex_task_close_report",
                },
            },
        )
        if updated is None:
            # Race: another delivery moved the parent first. Idempotent.
            return
        if status == HarnessTaskStatus.FAILED:
            self._dispatcher.block_failed_descendants(report.requested_by_task_id)
        self._dispatcher.dispatch_ready_work()

    def _persist_plan_contract(self, submission: PlannerSubmission) -> None:
        self._runtime.graph_store.set_plan_contract(
            submission.graph_id,
            task_specification=submission.task_specification,
            evaluation_criteria=list(submission.evaluation_criteria),
            continuation_goal=submission.continuation_goal,
        )

    def _persist_generator_tasks(
        self, tasks: tuple[PlannedGeneratorTask, ...]
    ) -> tuple[str, ...]:
        runtime = self._runtime
        graph = self._fresh_graph()
        ordered = ordered_generator_tasks(tasks)
        task_center_run_id = runtime.task_center_run_id_for_graph(graph)
        task_ids: list[str] = []
        for task in ordered:
            task_id = generator_task_id(graph.id, task.local_id)
            needs = dependency_task_ids(
                harness_graph_id=graph.id,
                local_deps=task.deps,
            )
            runtime.task_store.upsert_task(
                task_id=task_id,
                task_center_run_id=task_center_run_id,
                role=HarnessTaskRole.GENERATOR.value,
                agent_name=task.agent_name,
                task_input=task.task_spec,
                status=HarnessTaskStatus.PENDING.value,
                summaries=[],
                needs=list(needs),
                task_center_harness_graph_id=graph.id,
                spawn_reason="harness_graph_generator",
            )
            task_ids.append(task_id)
        return tuple(task_ids)

    def _mark_generator(self, submission: GeneratorSubmission) -> None:
        runtime = self._runtime
        graph = self._assert_stage(HarnessGraphStage.GENERATING)
        task = runtime.task_store.get_task(submission.task_id)
        if task is None:
            raise GraphInvariantViolation(
                f"Generator task {submission.task_id!r} not found"
            )
        assert_generator_task_for_submission(task, graph)
        if task["status"] != HarnessTaskStatus.RUNNING.value:
            raise GraphInvariantViolation(
                f"Generator task {submission.task_id!r} is not running"
            )
        status = (
            HarnessTaskStatus.DONE
            if submission.outcome == "success"
            else HarnessTaskStatus.FAILED
        )
        runtime.task_store.set_task_status(
            submission.task_id,
            status=status.value,
            summary={
                "outcome": submission.outcome,
                "summary": submission.summary,
                "payload": submission.payload,
            },
        )

    def _mark_evaluator(self, submission: EvaluatorSubmission) -> None:
        runtime = self._runtime
        graph = self._assert_stage(HarnessGraphStage.EVALUATING)
        if graph.evaluator_task_id != submission.task_id:
            raise GraphInvariantViolation(
                f"Evaluator submission task {submission.task_id!r} does not "
                f"match graph evaluator {graph.evaluator_task_id!r}"
            )
        task = runtime.task_store.get_task(submission.task_id)
        if task is None:
            raise GraphInvariantViolation(
                f"Evaluator task {submission.task_id!r} not found"
            )
        assert_evaluator_task_for_submission(task, graph)
        if task["status"] != HarnessTaskStatus.RUNNING.value:
            raise GraphInvariantViolation(
                f"Evaluator task {submission.task_id!r} is not running"
            )
        status = (
            HarnessTaskStatus.DONE
            if submission.outcome == "success"
            else HarnessTaskStatus.FAILED
        )
        runtime.task_store.set_task_status(
            submission.task_id,
            status=status.value,
            summary={
                "outcome": submission.outcome,
                "summary": submission.summary,
                "payload": submission.payload,
            },
        )

    def _close_graph(
        self,
        *,
        status: HarnessGraphStatus,
        fail_reason: HarnessGraphFailReason | None,
    ) -> None:
        assert_valid_graph_close(status=status, fail_reason=fail_reason)
        graph = self._fresh_graph()
        assert_graph_not_closed(graph)
        if graph.status != HarnessGraphStatus.RUNNING:
            raise GraphInvariantViolation(
                f"HarnessGraph {graph.id!r} is not running"
            )
        self._runtime.graph_store.close(
            graph.id,
            status=status,
            fail_reason=fail_reason,
            closed_at=datetime.now(UTC),
        )
        self._runtime.orchestrator_registry.deregister(graph.id)
        self._on_graph_closed(graph.id)

    def _mark_startup_failed(self, *, planner_task_id: str) -> None:
        runtime = self._runtime
        runtime.orchestrator_registry.deregister(self._harness_graph.id)
        try:
            runtime.task_store.set_task_status_if_current(
                planner_task_id,
                expected_status=HarnessTaskStatus.RUNNING.value,
                status=HarnessTaskStatus.FAILED.value,
                summary={
                    "fail_reason": HarnessGraphFailReason.STARTUP_FAILED.value,
                },
            )
        except LookupError:
            pass
        except Exception:
            logger.exception(
                "HarnessGraphOrchestrator: startup task cleanup failed",
            )

        try:
            graph = runtime.graph_store.get(self._harness_graph.id)
            if graph is not None and not graph.is_closed:
                runtime.graph_store.close(
                    graph.id,
                    status=HarnessGraphStatus.FAILED,
                    fail_reason=HarnessGraphFailReason.STARTUP_FAILED,
                    closed_at=datetime.now(UTC),
                )
        except Exception:
            logger.exception(
                "HarnessGraphOrchestrator: startup graph cleanup failed",
            )

    def _fresh_graph(self) -> HarnessGraph:
        graph = self._runtime.graph_store.get(self._harness_graph.id)
        if graph is None:
            raise GraphInvariantViolation(
                f"HarnessGraph {self._harness_graph.id!r} not found"
            )
        self._harness_graph = graph
        return graph

    def _assert_stage(self, expected: HarnessGraphStage) -> HarnessGraph:
        graph = self._fresh_graph()
        assert_graph_not_closed(graph)
        assert_graph_stage(graph, expected)
        return graph

    def _assert_submission_graph(self, graph_id: str) -> None:
        if graph_id != self._harness_graph.id:
            raise GraphInvariantViolation(
                f"Submission graph {graph_id!r} does not match orchestrator "
                f"graph {self._harness_graph.id!r}"
            )
