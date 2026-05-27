"""Real-agent SWE-EVO runner — thin shim around :func:`run_pipeline`.

Delegates orchestration to ``task_center_runner.core.engine.run_pipeline``
with a :class:`SweevoLifecycle` that runs F2P/P2P evaluation in
``after_run`` and a :class:`SweevoProvisioner` that runs
``setup_sweevo_sandbox`` against the externally-created Daytona sandbox.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from pathlib import Path

from runtime.app_factory import RuntimeConfig
from task_center_runner.benchmarks.sweevo.eval import SweevoLifecycle
from task_center_runner.benchmarks.sweevo.models import (
    SWEEvoInstance,
    SWEEvoResult,
    _REPO_DIR,
)
from task_center_runner.benchmarks.sweevo.run import SweevoProvisioner
from task_center_runner.benchmarks.sweevo.setup import build_sweevo_user_prompt
from task_center_runner.core.config import RunConfig
from task_center_runner.core.engine import run_pipeline
from task_center_runner.core.bootstrap import bootstrap_real_agent_runtime
from task_center_runner.core.stores import (
    TaskCenterStoreBundle,
    create_per_test_task_center_stores,
)


def _use_production_attempt_runner(_ctx: object) -> None:
    return None


@dataclass(slots=True)
class RealAgentRunReport:
    """Compact result handed back to the CLI / pytest entrypoints.

    ``sweevo_result`` is always populated — F2P/P2P only when the task center
    reached ``done`` and the wall-clock cap was not hit; otherwise a failure
    sentinel with ``resolved=False`` and ``fix_rate=0.0``.
    """

    instance_id: str
    task_center_run_id: str
    sandbox_id: str
    run_dir: Path
    task_center_status: str | None
    sweevo_result: SWEEvoResult
    aborted_by_timeout: bool = False
    performance_report_task: asyncio.Task[Path] | None = None


async def run_sweevo_real_agent(
    *,
    instance: SWEEvoInstance,
    sandbox_id: str,
    audit_dir: Path,
    repo_dir: str = _REPO_DIR,
    stores: TaskCenterStoreBundle | None = None,
    max_duration_s: float = 1800.0,
) -> RealAgentRunReport:
    """Drive one SWE-EVO instance through the real-LLM task-center pipeline.

    Thin shim over :func:`run_pipeline`. The :class:`SweevoLifecycle` handles
    F2P/P2P scoring in ``after_run`` and writes ``sweevo_result.json``; the
    shim reads the stashed result back out of ``PipelineReport.lifecycle_extras``.
    """
    owns_stores = stores is None
    bundle = stores or create_per_test_task_center_stores()

    runtime_cfg = RuntimeConfig(cwd=str(Path.cwd()), external_api_client=None)
    config = RunConfig(
        entry_prompt=build_sweevo_user_prompt(instance, repo_dir=repo_dir),
        repo_dir=repo_dir,
        sandbox=SweevoProvisioner(instance, sandbox_id, repo_dir=repo_dir),
        runner_factory=_use_production_attempt_runner,
        lifecycle=SweevoLifecycle(instance, repo_dir=repo_dir),
        bootstrap=bootstrap_real_agent_runtime,
        stores=bundle,
        audit_dir=audit_dir,
        run_label=f"benchmark/sweevo/{instance.instance_id}",
        instance_id=instance.instance_id,
        max_duration_s=max_duration_s,
        extras={"runtime_config": runtime_cfg},
    )

    try:
        pipeline_report = await run_pipeline(config)
    finally:
        if owns_stores:
            bundle.close()

    sweevo_result = pipeline_report.lifecycle_extras.get("sweevo_result")
    if not isinstance(sweevo_result, SWEEvoResult):
        # Defensive: ``SweevoLifecycle.after_run`` always stashes the result;
        # if it ever fails to we still hand back a sensible sentinel.
        sweevo_result = SWEEvoResult(
            plan_id=pipeline_report.task_center_run_id,
            instance_id=instance.instance_id,
            status="failed",
            duration_s=pipeline_report.duration_s,
            task_count=pipeline_report.task_count,
            tasks_completed=pipeline_report.tasks_completed,
            tasks_failed=pipeline_report.tasks_failed,
            error="missing_sweevo_result",
        )

    return RealAgentRunReport(
        instance_id=instance.instance_id,
        task_center_run_id=pipeline_report.task_center_run_id,
        sandbox_id=pipeline_report.sandbox_id,
        run_dir=pipeline_report.run_dir,
        task_center_status=pipeline_report.task_center_status,
        sweevo_result=sweevo_result,
        aborted_by_timeout=pipeline_report.aborted_by_timeout,
        performance_report_task=pipeline_report.performance_report_task,
    )


__all__ = ["RealAgentRunReport", "run_sweevo_real_agent"]
