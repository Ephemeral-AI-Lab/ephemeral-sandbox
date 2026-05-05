"""TaskCenter harness submission context resolution.

The submission tools (``submit_execution_success``, ``submit_execution_failure``,
``request_complex_task_solution``) live in two modes:

1. **Graph mode** — the task is attached to a :class:`HarnessGraph` and a
   running :class:`HarnessGraphOrchestrator`. Terminal events flow through
   the orchestrator's ``apply_*`` methods.
2. **Entry mode** — the task is the graph-less top-level entry executor,
   identified by ``task_center_harness_graph_id is None`` on the task row.
   Terminal events flow through :class:`EntryTaskController`.

:func:`resolve_harness_submission_context` keeps the legacy graph-only resolver
for callers that strictly require a graph (gates, evaluator surfaces). The new
:func:`resolve_executor_submission_context` returns
:class:`ExecutorSubmissionContext` — a tagged shape exposing graph-shape-agnostic
operations (``submit_executor_success`` / ``submit_executor_failure`` /
``start_mission_request``) that internally branch on which mode applies.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from task_center.mission.starter import (
    MissionRequestStarter,
    StartedMissionRequest,
)
from task_center.mission.mission import ComplexTaskRequest
from task_center.entry_task_controller import EntryTaskController
from task_center.exceptions import GraphInvariantViolation
from task_center.attempt import HarnessGraph
from task_center.attempt.orchestrator import HarnessGraphOrchestrator
from task_center.attempt.runtime import HarnessGraphRuntime
from task_center.episode.episode import TaskSegment
from task_center.task import GeneratorSubmission
from tools.core.context import ToolExecutionContextService


class HarnessSubmissionContextError(RuntimeError):
    """User-facing submission context resolution failure."""


@dataclass(frozen=True, slots=True)
class HarnessSubmissionContext:
    """Graph-bound submission context — the legacy shape.

    Resolved when the executor task is attached to a HarnessGraph. Tools
    that strictly require graph context (e.g. ``submit_evaluation``) keep
    using this resolver.
    """

    task_center_task_id: str
    task: dict[str, Any]
    graph: HarnessGraph
    segment: TaskSegment
    request: ComplexTaskRequest
    runtime: HarnessGraphRuntime
    orchestrator: HarnessGraphOrchestrator


@dataclass(frozen=True, slots=True)
class ExecutorSubmissionContext:
    """Unified context for executor-shaped terminal submissions.

    Tools call :meth:`submit_executor_success`,
    :meth:`submit_executor_failure`, or :meth:`start_mission_request`
    without knowing whether the task is graph-bound or entry-mode. The
    context dispatches to the right backend (orchestrator vs entry
    controller) internally.

    Exactly one of ``graph_ctx`` and ``entry_controller`` is set.
    """

    task_center_task_id: str
    task: dict[str, Any]
    runtime: HarnessGraphRuntime
    graph_ctx: HarnessSubmissionContext | None
    entry_controller: EntryTaskController | None

    @property
    def is_entry_mode(self) -> bool:
        return self.entry_controller is not None

    @property
    def graph_id(self) -> str | None:
        return self.graph_ctx.graph.id if self.graph_ctx is not None else None

    # ---- operations -------------------------------------------------------

    def submit_executor_success(
        self, *, summary: str, artifacts: list[str]
    ) -> None:
        if self.graph_ctx is not None:
            self.graph_ctx.orchestrator.apply_generator_submission(
                GeneratorSubmission(
                    graph_id=self.graph_ctx.graph.id,
                    task_id=self.task_center_task_id,
                    outcome="success",
                    summary=summary,
                    payload={
                        "generator_role": "executor",
                        "artifacts": artifacts,
                    },
                )
            )
            return
        assert self.entry_controller is not None
        self.entry_controller.apply_executor_success(
            summary=summary, artifacts=artifacts
        )

    def submit_executor_failure(
        self, *, summary: str, reason: str, details: list[str]
    ) -> None:
        if self.graph_ctx is not None:
            self.graph_ctx.orchestrator.apply_generator_submission(
                GeneratorSubmission(
                    graph_id=self.graph_ctx.graph.id,
                    task_id=self.task_center_task_id,
                    outcome="failure",
                    summary=summary,
                    payload={
                        "generator_role": "executor",
                        "reason": reason,
                        "details": details,
                    },
                )
            )
            return
        assert self.entry_controller is not None
        self.entry_controller.apply_executor_failure(
            summary=summary, reason=reason, details=details
        )

    def start_mission_request(
        self, *, goal: str
    ) -> StartedMissionRequest:
        coordinator = MissionRequestStarter(runtime=self.runtime)
        return coordinator.start(
            task_center_run_id=self.task["task_center_run_id"],
            parent_task_id=self.task_center_task_id,
            parent_harness_graph_id=self.graph_id,
            goal=goal,
        )


def resolve_harness_submission_context(
    context: ToolExecutionContextService,
) -> HarnessSubmissionContext:
    """Resolve the current TaskCenter task into durable harness graph context.

    Strict graph mode — raises :class:`HarnessSubmissionContextError` if the
    task is not attached to a HarnessGraph. Use this resolver from tools
    that genuinely require a graph (planner submissions, evaluator
    submissions).
    """
    runtime, task, task_id = _resolve_runtime_task(context)
    return _resolve_graph_context(
        runtime=runtime, task=task, task_id=task_id, context=context
    )


def resolve_executor_submission_context(
    context: ToolExecutionContextService,
) -> ExecutorSubmissionContext:
    """Resolve a unified executor submission context.

    Branches on whether the task row's ``task_center_harness_graph_id`` is
    set (graph mode) or ``None`` (entry mode). Tools that accept either
    shape — ``submit_execution_success`` / ``submit_execution_failure`` /
    ``request_complex_task_solution`` — call this resolver and use the
    resulting :class:`ExecutorSubmissionContext` operations.
    """
    runtime, task, task_id = _resolve_runtime_task(context)
    graph_id = str(task.get("task_center_harness_graph_id") or "")
    if graph_id and not graph_id.isspace():
        graph_ctx = _resolve_graph_context(
            runtime=runtime, task=task, task_id=task_id, context=context
        )
        return ExecutorSubmissionContext(
            task_center_task_id=task_id,
            task=task,
            runtime=runtime,
            graph_ctx=graph_ctx,
            entry_controller=None,
        )

    controller = runtime.entry_task_controller_for(task_id)
    if controller is None:
        raise HarnessSubmissionContextError(
            f"TaskCenter task {task_id!r} is graph-less but no entry "
            "controller is bound to it; the spawn was set up incorrectly."
        )
    return ExecutorSubmissionContext(
        task_center_task_id=task_id,
        task=task,
        runtime=runtime,
        graph_ctx=None,
        entry_controller=controller,
    )


def _resolve_runtime_task(
    context: ToolExecutionContextService,
) -> tuple[HarnessGraphRuntime, dict[str, Any], str]:
    """Shared prelude: pull runtime + task row + task id from tool context."""
    runtime = context.get("harness_graph_runtime")
    if not isinstance(runtime, HarnessGraphRuntime):
        raise HarnessSubmissionContextError(
            "Missing harness graph runtime for this TaskCenter submission."
        )

    task_id = str(context.get("task_center_task_id") or "")
    if not task_id or task_id.isspace():
        raise HarnessSubmissionContextError(
            "Missing TaskCenter task id for this submission."
        )

    task = runtime.task_store.get_task(task_id)
    if task is None:
        raise HarnessSubmissionContextError(
            f"TaskCenter task {task_id!r} was not found."
        )
    return runtime, task, task_id


def _resolve_graph_context(
    *,
    runtime: HarnessGraphRuntime,
    task: dict[str, Any],
    task_id: str,
    context: ToolExecutionContextService,
) -> HarnessSubmissionContext:
    """Build :class:`HarnessSubmissionContext` from an already-fetched task.

    Shared between :func:`resolve_harness_submission_context` and the
    graph-mode branch of :func:`resolve_executor_submission_context` so the
    task row is fetched exactly once per call.
    """
    graph_id = str(task.get("task_center_harness_graph_id") or "")
    if not graph_id or graph_id.isspace():
        raise HarnessSubmissionContextError(
            f"TaskCenter task {task_id!r} is not attached to a harness graph."
        )

    metadata_graph_id = str(context.get("task_center_harness_graph_id") or "")
    if metadata_graph_id.isspace():
        raise HarnessSubmissionContextError(
            "TaskCenter graph metadata is blank."
        )
    if metadata_graph_id and metadata_graph_id != graph_id:
        raise HarnessSubmissionContextError(
            "TaskCenter graph metadata does not match the persisted task row."
        )

    graph = runtime.graph_store.get(graph_id)
    if graph is None:
        raise HarnessSubmissionContextError(
            f"HarnessGraph {graph_id!r} was not found."
        )

    segment = runtime.segment_store.get(graph.task_segment_id)
    if segment is None:
        raise HarnessSubmissionContextError(
            f"TaskSegment {graph.task_segment_id!r} was not found."
        )

    request = runtime.request_store.get(segment.complex_task_request_id)
    if request is None:
        raise HarnessSubmissionContextError(
            f"ComplexTaskRequest {segment.complex_task_request_id!r} was not found."
        )

    try:
        orchestrator = runtime.orchestrator_registry.get_or_raise(graph_id)
    except GraphInvariantViolation as exc:
        raise HarnessSubmissionContextError(str(exc)) from exc

    return HarnessSubmissionContext(
        task_center_task_id=task_id,
        task=task,
        graph=graph,
        segment=segment,
        request=request,
        runtime=runtime,
        orchestrator=orchestrator,
    )
