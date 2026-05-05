"""HarnessGraphDispatcher — DAG dispatch helper for HarnessGraphOrchestrator.

Owns the launch/quiescence state machine for one graph's generators and
evaluator. Calls back into the orchestrator's ``_close_graph`` for the actual
graph-closing transition; the orchestrator remains the only owner of
close-graph state and the on_graph_closed signal to ``TaskSegmentManager``.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from typing import Any

from task_center.exceptions import GraphInvariantViolation
from task_center.attempt.state import (
    HarnessGraph,
    HarnessGraphFailReason,
    HarnessGraphStage,
    HarnessGraphStatus,
)
from task_center.context_engine.scope import ContextScope
from task_center.attempt.runtime import (
    AgentLaunch,
    HarnessGraphRuntime,
)
from task_center.attempt.generator_dag import (
    all_generators_done,
    all_generators_quiescent,
    any_generator_failed_or_blocked,
    blocked_descendant_ids,
    ready_pending_generator_ids,
)
from task_center.task import (
    HarnessTaskRole,
    HarnessTaskStatus,
    evaluator_task_id,
)

logger = logging.getLogger(__name__)


CloseGraphCallback = Callable[
    [HarnessGraphStatus, HarnessGraphFailReason | None], None
]


class HarnessGraphDispatcher:
    """Drives the generator-DAG and evaluator launch/quiescence machine."""

    def __init__(
        self,
        *,
        harness_graph_id: str,
        runtime: HarnessGraphRuntime,
        close_graph: CloseGraphCallback,
    ) -> None:
        self._harness_graph_id = harness_graph_id
        self._runtime = runtime
        self._close_graph = close_graph

    # ---- public API -----------------------------------------------------

    def dispatch_ready_work(self) -> None:
        graph = self._fresh_graph()
        if graph.is_closed:
            return
        if graph.stage == HarnessGraphStage.PLANNING:
            return
        if graph.stage == HarnessGraphStage.GENERATING:
            self._dispatch_generating(graph)
            return
        if graph.stage == HarnessGraphStage.EVALUATING:
            self._dispatch_evaluating(graph)

    def block_failed_descendants(self, failed_task_id: str) -> None:
        runtime = self._runtime
        graph = self._fresh_graph()
        task_records = runtime.task_store.list_generator_tasks_for_harness_graph(
            graph.id
        )
        for task_id in blocked_descendant_ids(
            failed_task_id=failed_task_id,
            task_records=task_records,
        ):
            runtime.task_store.set_task_status(
                task_id,
                status=HarnessTaskStatus.BLOCKED.value,
                summary={"blocked_by": failed_task_id},
            )

    # ---- internal -------------------------------------------------------

    def _dispatch_generating(self, graph: HarnessGraph) -> None:
        runtime = self._runtime
        task_records = runtime.task_store.list_generator_tasks_for_harness_graph(
            graph.id
        )
        ready_ids = ready_pending_generator_ids(task_records)
        if ready_ids:
            launch_failed = False
            for task_id in ready_ids:
                launch_failed = (
                    not self._launch_ready_generator(
                        graph=graph,
                        task_id=task_id,
                    )
                    or launch_failed
                )
            if launch_failed:
                self.dispatch_ready_work()
            return

        if not all_generators_quiescent(task_records):
            return

        if any_generator_failed_or_blocked(task_records):
            self._close_graph(
                HarnessGraphStatus.FAILED,
                HarnessGraphFailReason.GENERATOR_FAILED,
            )
            return

        if all_generators_done(task_records):
            self._spawn_evaluator(graph)

    def _dispatch_evaluating(self, graph: HarnessGraph) -> None:
        if graph.evaluator_task_id is None:
            raise GraphInvariantViolation(
                f"HarnessGraph {graph.id!r} is evaluating with no evaluator task"
            )
        runtime = self._runtime
        evaluator_task = runtime.task_store.get_task(graph.evaluator_task_id)
        if evaluator_task is None:
            raise GraphInvariantViolation(
                f"Evaluator task {graph.evaluator_task_id!r} not found"
            )
        status = HarnessTaskStatus(evaluator_task["status"])
        if status == HarnessTaskStatus.DONE:
            self._close_graph(HarnessGraphStatus.PASSED, None)
        elif status == HarnessTaskStatus.FAILED:
            self._close_graph(
                HarnessGraphStatus.FAILED,
                HarnessGraphFailReason.EVALUATOR_FAILED,
            )

    def _launch_ready_generator(
        self, *, graph: HarnessGraph, task_id: str
    ) -> bool:
        runtime = self._runtime
        current = runtime.task_store.get_task(task_id)
        if current is None:
            raise GraphInvariantViolation(f"Generator task {task_id!r} not found")
        agent_name = self._task_agent_name(current)
        task = runtime.task_store.set_task_status(
            task_id, status=HarnessTaskStatus.RUNNING.value
        )
        try:
            launch = self._build_generator_launch(
                graph=graph,
                task=task,
                task_id=task_id,
                base_agent_name=agent_name,
            )
            if launch.context_packet_id is not None:
                runtime.task_store.set_task_context_packet_id(
                    task_id,
                    context_packet_id=launch.context_packet_id,
                )
            runtime.agent_launcher.launch(launch)
        except Exception:
            logger.exception(
                "HarnessGraphDispatcher: generator launch failed",
                extra={"task_id": task_id, "harness_graph_id": graph.id},
            )
            runtime.task_store.set_task_status_if_current(
                task_id,
                expected_status=HarnessTaskStatus.RUNNING.value,
                status=HarnessTaskStatus.FAILED.value,
                summary={
                    "fail_reason": "agent_launch_failed",
                    "summary": "Generator agent launch failed.",
                },
            )
            self.block_failed_descendants(task_id)
            return False
        return True

    def _launch_evaluator(self, launch: AgentLaunch) -> None:
        runtime = self._runtime
        try:
            runtime.agent_launcher.launch(launch)
        except Exception:
            logger.exception(
                "HarnessGraphDispatcher: evaluator launch failed",
                extra={
                    "task_id": launch.task_id,
                    "harness_graph_id": launch.harness_graph_id,
                },
            )
            runtime.task_store.set_task_status_if_current(
                launch.task_id,
                expected_status=HarnessTaskStatus.RUNNING.value,
                status=HarnessTaskStatus.FAILED.value,
                summary={
                    "fail_reason": "agent_launch_failed",
                    "summary": "Evaluator agent launch failed.",
                },
            )
            self._close_graph(
                HarnessGraphStatus.FAILED,
                HarnessGraphFailReason.EVALUATOR_FAILED,
            )

    def _spawn_evaluator(self, graph: HarnessGraph) -> None:
        if graph.evaluator_task_id is not None:
            return
        runtime = self._runtime
        task_id = evaluator_task_id(graph.id)
        task_center_run_id = runtime.task_center_run_id_for_graph(graph)
        launch = self._build_evaluator_launch(
            graph=graph,
            task_id=task_id,
            task_center_run_id=task_center_run_id,
        )
        runtime.task_store.upsert_task(
            task_id=task_id,
            task_center_run_id=task_center_run_id,
            role=HarnessTaskRole.EVALUATOR.value,
            agent_name=launch.agent_name,
            task_input=launch.task_input,
            status=HarnessTaskStatus.RUNNING.value,
            summaries=[],
            needs=list(graph.generator_task_ids),
            task_center_harness_graph_id=graph.id,
            context_packet_id=launch.context_packet_id,
            spawn_reason="harness_graph_evaluator",
        )
        runtime.graph_store.set_evaluator_task_id(graph.id, task_id)
        runtime.graph_store.set_stage(graph.id, HarnessGraphStage.EVALUATING)
        self._launch_evaluator(launch)

    @staticmethod
    def _task_agent_name(task: dict[str, Any]) -> str:
        agent_name = str(task.get("agent_name") or "").strip()
        if not agent_name:
            raise GraphInvariantViolation(
                f"Task {task.get('id')!r} has no persisted agent profile"
            )
        return agent_name

    def _build_generator_launch(
        self,
        *,
        graph: HarnessGraph,
        task: dict[str, Any],
        task_id: str,
        base_agent_name: str,
    ) -> AgentLaunch:
        runtime = self._runtime
        composer = runtime.require_composer()
        segment = runtime.segment_store.get(graph.task_segment_id)
        if segment is None:
            raise GraphInvariantViolation(
                f"TaskSegment {graph.task_segment_id!r} not found"
            )
        bundle = composer.compose(
            base_agent_name=base_agent_name,
            scope=ContextScope(
                request_id=segment.complex_task_request_id,
                segment_id=segment.id,
                harness_graph_id=graph.id,
                task_id=task_id,
            ),
        )
        return AgentLaunch(
            task_id=task_id,
            task_center_run_id=task["task_center_run_id"],
            harness_graph_id=graph.id,
            role=HarnessTaskRole.GENERATOR,
            agent_name=bundle.agent_def.name,
            task_input=bundle.task_input,
            needs=tuple(task["needs"]),
            system_prompt=bundle.system_prompt,
            context_packet_id=bundle.context_packet_id,
            complex_task_request_id=segment.complex_task_request_id,
        )

    def _build_evaluator_launch(
        self,
        *,
        graph: HarnessGraph,
        task_id: str,
        task_center_run_id: str,
    ) -> AgentLaunch:
        runtime = self._runtime
        composer = runtime.require_composer()
        segment = runtime.segment_store.get(graph.task_segment_id)
        if segment is None:
            raise GraphInvariantViolation(
                f"TaskSegment {graph.task_segment_id!r} not found"
            )
        bundle = composer.compose(
            base_agent_name="evaluator",
            scope=ContextScope(
                request_id=segment.complex_task_request_id,
                segment_id=segment.id,
                harness_graph_id=graph.id,
            ),
        )
        return AgentLaunch(
            task_id=task_id,
            task_center_run_id=task_center_run_id,
            harness_graph_id=graph.id,
            role=HarnessTaskRole.EVALUATOR,
            agent_name=bundle.agent_def.name,
            task_input=bundle.task_input,
            needs=tuple(graph.generator_task_ids),
            system_prompt=bundle.system_prompt,
            context_packet_id=bundle.context_packet_id,
            complex_task_request_id=segment.complex_task_request_id,
        )

    def _fresh_graph(self) -> HarnessGraph:
        graph = self._runtime.graph_store.get(self._harness_graph_id)
        if graph is None:
            raise GraphInvariantViolation(
                f"HarnessGraph {self._harness_graph_id!r} not found"
            )
        return graph
