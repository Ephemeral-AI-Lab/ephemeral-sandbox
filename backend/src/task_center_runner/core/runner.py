"""``run_scenario`` — thin shim around :func:`task_center_runner.core.engine.run_pipeline`.

The legacy ``run_scenario`` contract is preserved (same arguments, same
:class:`RunReport` shape — see
``tests/golden/run_report_structural.json``), but the actual orchestration
now happens inside :func:`task_center_runner.core.engine.run_pipeline`.

What the shim adds on top of the engine:

- Constructs ``RunConfig`` via :func:`build_scenario_config` so the
  ``MockSquadRunner`` factory, ``MutableMockState``, ``HookSet``, and
  ``ScenarioLifecycle`` all share state inside one place.
- Rebuilds the legacy ``RunReport`` view from ``ScenarioLifecycle`` event
  accumulation and the ``PipelineReport`` returned by the engine.
- Owns the ``TaskCenterStoreBundle`` lifecycle: passes the bundle to
  ``run_pipeline`` via ``config.stores`` so the engine does not close it,
  then computes :func:`_graph_summary` against the still-open stores before
  closing.
"""

from __future__ import annotations

import asyncio
import contextlib
import dataclasses as _dataclasses
import hashlib
import uuid
from collections.abc import Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from task_center_runner.audit.events import Event, EventType
from task_center_runner.core.engine import run_pipeline
from task_center_runner.hooks.registry import (
    Hook,
    HookResult,
)
from task_center_runner.scenarios.base import Scenario
from task_center_runner.scenarios.builder import (
    _event_source_runner_enabled,
    build_scenario_config,
)
from task_center_runner.agent.mock.definitions import registered_mock_agents  # noqa: F401 — re-export
from task_center_runner.agent.mock.prompt_inspector import (
    LaunchRecord,
    PromptInspection,
    ToolCallRecord,
)
from task_center_runner.agent.mock.sandbox_probe import SandboxCheck
from task_center_runner.core.stores import (
    TaskCenterStoreBundle,
    create_per_test_task_center_stores,
)


@dataclass(slots=True)
class RunReport:
    """Result of one :func:`run_scenario` invocation."""

    scenario_name: str
    task_center_run_id: str
    request_id: str
    sandbox_id: str
    instance_id: str
    run_dir: Path
    task_center_status: str | None
    duration_s: float
    events: list[Event] = field(default_factory=list)
    seen_event_types: list[EventType] = field(default_factory=list)
    hook_results: list[HookResult] = field(default_factory=list)
    mutable_state_flags: dict[str, Any] = field(default_factory=dict)
    launches: list[LaunchRecord] = field(default_factory=list)
    tool_calls: list[ToolCallRecord] = field(default_factory=list)
    prompt_inspections: list[PromptInspection] = field(default_factory=list)
    sandbox_checks: list[SandboxCheck] = field(default_factory=list)
    metrics: dict[str, Any] = field(default_factory=dict)
    graph_summary: dict[str, Any] = field(default_factory=dict)
    entry_prompt_sha256: str = ""
    entry_prompt_length: int = 0
    requirement_ledger: list[dict[str, Any]] = field(default_factory=list)
    package_plan: list[dict[str, Any]] = field(default_factory=list)
    matrix_plan: list[dict[str, Any]] = field(default_factory=list)
    performance_report_task: asyncio.Task[Path] | None = None

    @property
    def passed_prompt_inspections(self) -> bool:
        return all(item.passed for item in self.prompt_inspections)

    @property
    def passed_sandbox_checks(self) -> bool:
        return all(item.passed for item in self.sandbox_checks)


def _graph_summary(
    bundle: TaskCenterStoreBundle,
    task_center_run_id: str,
) -> dict[str, Any]:
    workflows: list[dict[str, Any]] = []
    for workflow in bundle.workflow_store.list_for_run(task_center_run_id):
        iterations: list[dict[str, Any]] = []
        for iteration in bundle.iteration_store.list_for_workflow(workflow.id):
            attempts: list[dict[str, Any]] = []
            for attempt in bundle.attempt_store.list_for_iteration(iteration.id):
                task_rows = bundle.task_store.list_tasks_for_attempt(attempt.id)
                attempts.append(
                    {
                        "id": attempt.id,
                        "sequence_no": attempt.attempt_sequence_no,
                        "stage": attempt.stage.value,
                        "status": attempt.status.value,
                        "fail_reason": (
                            attempt.fail_reason.value
                            if attempt.fail_reason is not None
                            else None
                        ),
                        # Dict key mirrors the Python attribute name so test
                        # consumers can read ``attempt["deferred_goal_for_next_iteration"]``
                        # without having to know the DB column alias.
                        "deferred_goal_for_next_iteration": attempt.deferred_goal_for_next_iteration,
                        "task_ids": list(attempt.generator_task_ids),
                        "tasks": task_rows,
                    }
                )
            iterations.append(
                {
                    "id": iteration.id,
                    "sequence_no": iteration.sequence_no,
                    "creation_reason": iteration.creation_reason.value,
                    "status": iteration.status.value,
                    "goal": iteration.goal,
                    # Dict key mirrors the Python attribute name.
                    "deferred_goal_for_next_iteration": iteration.deferred_goal_for_next_iteration,
                    "attempts": attempts,
                }
            )
        workflows.append(
            {
                "id": workflow.id,
                "status": workflow.status.value,
                "origin_kind": workflow.origin_kind.value,
                "requested_by_task_id": workflow.requested_by_task_id,
                "final_outcome": workflow.final_outcome,
                "iterations": iterations,
            }
        )
    return {"workflows": workflows}


@contextlib.contextmanager
def _active_mock_model_if_enabled(bundle: TaskCenterStoreBundle, scenario_name: str):
    """Register a throwaway active model row for the event-source runner path.

    Under ``EOS_MOCK_EVENT_SOURCE_RUNNER`` the mock drives the REAL loop via
    ``spawn_agent``, which requires an active model registration even though the
    api_client is never streamed from (the injected ``ScenarioEventSource``
    short-circuits it). The old ``MockSquadRunner`` never spawned agents, so no
    row was needed. Gating on the flag keeps the default-off path untouched; all
    scenario tests funnel through here, so none need a per-test fixture.
    """
    if not _event_source_runner_enabled(scenario_name):
        yield
        return
    from config.model_config import get_active_model_kwargs
    from runtime.app_factory import model_store

    prior_sf = model_store._session_factory  # noqa: SLF001 — restored on exit
    model_store.initialize(bundle.session_factory)
    # Skip if a caller (e.g. a proof-test fixture) already activated a model
    # against these stores — avoid a double registration / multiple-active row.
    try:
        get_active_model_kwargs()
        already_active = True
    except Exception:
        already_active = False
    key: str | None = None
    try:
        if not already_active:
            key = f"test/mock-loop-{uuid.uuid4().hex[:8]}"
            model_store.register(
                key=key,
                label="Mock Loop Runner",
                class_path="providers.clients.anthropic_native:AnthropicClient",
                kwargs={"model": "mock-loop", "max_tokens": 4096},
                activate=True,
            )
        yield
    finally:
        if key is not None:
            with contextlib.suppress(Exception):
                model_store.delete(key)
        model_store._session_factory = prior_sf  # noqa: SLF001


async def run_scenario(
    scenario: Scenario,
    *,
    sandbox_id: str,
    audit_dir: Path,
    repo_dir: str,
    entry_prompt: str,
    stores: TaskCenterStoreBundle | None = None,
    extra_hooks: Sequence[Hook] = (),
    instance_id: str = "",
) -> RunReport:
    """Run *scenario* end-to-end against ``sandbox_id``.

    Thin shim over :func:`run_pipeline`. The legacy ``RunReport`` view is
    rebuilt from the ``PipelineReport`` plus state accumulated by the
    ``ScenarioLifecycle`` and the ``MockSquadRunner`` instance captured via
    a wrapped ``runner_factory``.
    """
    owns_stores = stores is None
    bundle = stores or create_per_test_task_center_stores()

    config, mutable_state, lifecycle = build_scenario_config(
        scenario,
        sandbox_id=sandbox_id,
        audit_dir=audit_dir,
        repo_dir=repo_dir,
        entry_prompt=entry_prompt,
        extra_hooks=extra_hooks,
        instance_id=instance_id,
    )

    # ``registered_mock_agents`` registers the mock agent definitions for the
    # duration of the run; restore the registry on exit. The original
    # ``run_scenario`` wrapped its core call in this context manager too.
    with registered_mock_agents(), _active_mock_model_if_enabled(bundle, scenario.name):
        config = _dataclasses.replace(config, stores=bundle)
        pipeline_report = await run_pipeline(config)

    try:
        graph_summary = _graph_summary(bundle, pipeline_report.task_center_run_id)
    finally:
        if owns_stores:
            bundle.close()

    return RunReport(
        scenario_name=scenario.name,
        task_center_run_id=pipeline_report.task_center_run_id,
        request_id=pipeline_report.request_id,
        sandbox_id=pipeline_report.sandbox_id,
        instance_id=pipeline_report.instance_id,
        run_dir=pipeline_report.run_dir,
        task_center_status=pipeline_report.task_center_status,
        duration_s=pipeline_report.duration_s,
        events=list(lifecycle.captured_events),
        seen_event_types=list(mutable_state.seen_events),
        hook_results=list(lifecycle.hook_results),
        mutable_state_flags=dict(mutable_state.flags),
        launches=list(lifecycle.launches),
        tool_calls=list(lifecycle.tool_calls),
        prompt_inspections=list(lifecycle.prompt_inspections),
        sandbox_checks=list(lifecycle.sandbox_checks),
        metrics=dict(pipeline_report.metrics),
        graph_summary=graph_summary,
        entry_prompt_sha256=hashlib.sha256(entry_prompt.encode("utf-8")).hexdigest(),
        entry_prompt_length=len(entry_prompt),
        requirement_ledger=list(getattr(scenario, "requirement_ledger", [])),
        package_plan=list(getattr(scenario, "package_plan", [])),
        matrix_plan=list(getattr(scenario, "matrix_plan", [])),
        performance_report_task=pipeline_report.performance_report_task,
    )


__all__ = ["RunReport", "run_scenario"]
