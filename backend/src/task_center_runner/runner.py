"""``run_scenario`` — thin shim around :func:`task_center_runner.core.engine.run_pipeline`.

Phase 4e of the task_center_runner restructure
(.omc/plans/task_center_runner-restructure.md). The legacy ``run_scenario``
contract is preserved (same arguments, same :class:`RunReport` shape — see
``tests/golden/run_report_structural.json``), but the actual orchestration
now happens inside :func:`task_center_runner.core.engine.run_pipeline`.

What the shim adds on top of the engine:

- Constructs ``RunConfig`` via :func:`build_scenario_config` so the
  ``MockSquadRunner`` factory, ``MutableMockState``, ``HookSet``, and
  ``ScenarioLifecycle`` all share state inside one place.
- Wraps the ``runner_factory`` to capture the ``MockSquadRunner`` instance
  via a list-box; the shim reads ``squad.launches``/``tool_calls``/
  ``prompt_inspections``/``sandbox_checks`` for the legacy ``RunReport``
  view. Phase 4g will switch this to ``MOCK_*`` event accumulation and
  delete the list attributes — the engine remains literally
  runner-agnostic in both phases.
- Owns the ``TaskCenterStoreBundle`` lifecycle: passes the bundle to
  ``run_pipeline`` via ``config.stores`` so the engine does not close it,
  then computes :func:`_graph_summary` against the still-open stores before
  closing.
"""

from __future__ import annotations

import asyncio
import dataclasses as _dataclasses
import hashlib
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
from task_center_runner.scenarios.builder import build_scenario_config
from task_center_runner.squad.definitions import registered_mock_agents  # noqa: F401 — re-export
from task_center_runner.squad.prompt_inspector import (
    LaunchRecord,
    PromptInspection,
    ToolCallRecord,
)
from task_center_runner.squad.runner import MockSquadRunner
from task_center_runner.squad.sandbox_probe import SandboxCheck
from task_center_runner.stores import (
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
    missions: list[dict[str, Any]] = []
    for mission in bundle.mission_store.list_for_run(task_center_run_id):
        episodes: list[dict[str, Any]] = []
        for episode in bundle.episode_store.list_for_mission(mission.id):
            attempts: list[dict[str, Any]] = []
            for attempt in bundle.attempt_store.list_for_episode(episode.id):
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
                        "continuation_goal": attempt.continuation_goal,
                        "task_ids": list(attempt.generator_task_ids),
                        "tasks": task_rows,
                    }
                )
            episodes.append(
                {
                    "id": episode.id,
                    "sequence_no": episode.sequence_no,
                    "creation_reason": episode.creation_reason.value,
                    "status": episode.status.value,
                    "goal": episode.goal,
                    "continuation_goal": episode.continuation_goal,
                    "attempts": attempts,
                }
            )
        missions.append(
            {
                "id": mission.id,
                "status": mission.status.value,
                "requested_by_task_id": mission.requested_by_task_id,
                "final_outcome": mission.final_outcome,
                "episodes": episodes,
            }
        )
    return {"missions": missions}


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

    runner_box: list[MockSquadRunner | None] = [None]
    original_factory = config.runner_factory

    def _capturing_factory(ctx: Any) -> Any:
        squad = original_factory(ctx)
        runner_box[0] = squad  # type: ignore[assignment]
        return squad

    # ``registered_mock_agents`` registers the mock agent definitions for the
    # duration of the run; restore the registry on exit. The original
    # ``run_scenario`` wrapped its core call in this context manager too.
    with registered_mock_agents():
        config = _dataclasses.replace(
            config, runner_factory=_capturing_factory, stores=bundle
        )
        pipeline_report = await run_pipeline(config)

    try:
        squad = runner_box[0]
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
        # Phase 4g-step2: launches / tool_calls / prompt_inspections now come
        # from MOCK_* events accumulated by the lifecycle subscriber, not from
        # MockSquadRunner's list attributes. ``sandbox_checks`` still uses the
        # runner attribute because probe helpers append to it via pass-by-ref
        # without yet publishing MOCK_SANDBOX_CHECK_RECORDED events; Phase
        # 4g-step3 threads publish through those helpers.
        launches=list(lifecycle.launches),
        tool_calls=list(lifecycle.tool_calls),
        prompt_inspections=list(lifecycle.prompt_inspections),
        sandbox_checks=list(squad.sandbox_checks) if squad is not None else [],
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
