"""SWE-EVO team runner.

Drives a full builtin team (planner → developer → validator) against a
SWE-EVO instance inside its Daytona sandbox. Each task spawned by
the team dispatcher runs through :func:`engine.runtime.agent.spawn_agent`
with its full production tool surface, and every ``StreamEvent`` is
forwarded to the shared :class:`MultiAgentEventPrinter` so the CLI shows
all agents in the same multi-column log.
"""

from __future__ import annotations

import logging
from typing import Any

from benchmarks.sweevo.dataset import select_sweevo_instance, summarize_sweevo_instance
from benchmarks.sweevo.evaluation import _extract_combined_patch, evaluate_sweevo_result
from benchmarks.sweevo.models import (
    _DEFAULT_DATASET_SOURCE,
    _DEFAULT_TARGET_BULLETS,
    _REPO_DIR,
    SWEEvoResult,
)
from benchmarks.sweevo.sandbox import (
    create_sweevo_test_sandbox,
)

logger = logging.getLogger(__name__)


def _emit_progress(printer: Any, line: str) -> None:
    if printer is None or not hasattr(printer, "raw_line"):
        return
    try:
        printer.raw_line("team", line)
    except Exception:
        logger.debug("Failed to emit benchmark progress line", exc_info=True)


async def run_sweevo_with_agent(
    *,
    printer: "Any",
    team_name: str = "sweevo_benchmark",
    source: str = _DEFAULT_DATASET_SOURCE,
    instance_id: str | None = None,
    size: str = "medium",
    target_bullets: int = _DEFAULT_TARGET_BULLETS,
    snapshot_name: str = "",
    sandbox_name: str = "",
    register_snapshot: bool = True,
    cpu: int = 2,
    disk: int = 10,
    repo_dir: str = _REPO_DIR,
    team_run_id: str | None = None,
    structured_log_path: str | None = None,
) -> dict[str, Any]:
    """Drive a team against a SWE-EVO instance and grade it.

    Provisions the sandbox, runs the builtin team (planner/developer/validator DAG)
    through :func:`run_sweevo_team`, then executes the explicit F2P/P2P grader.

    Returns a dict with ``instance``, ``sandbox``, ``team_status``,
    ``agent_patch`` (combined git diff), and ``grading`` (F2P/P2P metrics).
    """
    from benchmarks.sweevo import team_runner as sweevo_team_runner

    try:
        from sandbox.lifecycle import shutdown_cached_client_async

        instance = select_sweevo_instance(
            source=source,
            instance_id=instance_id,
            size=size,
            target_bullets=target_bullets,
        )
        if printer is not None:
            summary = summarize_sweevo_instance(instance)
            _emit_progress(
                printer,
                (
                    "[setup] "
                    f"instance={instance.instance_id} repo={instance.repo} "
                    f"size={summary['size']} bullets={summary['bullet_count']}"
                ),
            )

        if printer is not None:
            _emit_progress(
                printer,
                (
                    "[setup] "
                    f"creating sandbox register_snapshot={register_snapshot} "
                    f"sandbox_name={sandbox_name or '<fresh>'}"
                ),
            )

        sandbox_result = await create_sweevo_test_sandbox(
            instance,
            snapshot_name=snapshot_name,
            sandbox_name=sandbox_name,
            register_snapshot=register_snapshot,
            cpu=cpu,
            disk=disk,
            repo_dir=repo_dir,
        )
        sandbox_id = sandbox_result["sandbox_id"]
        if printer is not None:
            setup_line = (
                "[setup] "
                f"sandbox_id={sandbox_id} reused_existing={sandbox_result.get('reused_existing', False)}"
            )
            fallback_reason = str(sandbox_result.get("fallback_reason") or "").strip()
            if fallback_reason:
                setup_line += f" fallback_reason={fallback_reason}"
            _emit_progress(printer, setup_line)

        try:
            team_result = await sweevo_team_runner.run_sweevo_team(
                instance,
                sandbox_id,
                team_name=team_name,
                team_run_id=team_run_id,
                repo_dir=repo_dir,
                printer=printer,
                structured_log_path=structured_log_path,
            )
        finally:
            try:
                printer.flush()
            except Exception:
                pass

        team_status = team_result.get("status")
        task_count = int(team_result.get("work_items") or 0)
        team_details = dict(team_result)

        agent_patch = await _extract_combined_patch(sandbox_id, repo_dir)

        if printer is not None:
            _emit_progress(printer, "[grading] evaluating fail-to-pass and pass-to-pass results")
        grading_result = await evaluate_sweevo_result(
            instance,
            SWEEvoResult(
                plan_id="team",
                instance_id=instance.instance_id,
                status="completed",
                agent_patch=agent_patch,
                task_count=task_count,
            ),
            sandbox_id,
            repo_dir=repo_dir,
        )

        return {
            "instance": summarize_sweevo_instance(instance),
            "snapshot_name": sandbox_result["snapshot_name"],
            "sandbox": sandbox_result["sandbox"],
            "repo_dir": repo_dir,
            "structured_log_path": structured_log_path,
            "agent_patch": agent_patch,
            "team_name": team_details.get("team_name") or team_name,
            "team_run_id": team_details.get("team_run_id"),
            "team_status": (
                team_status.value if hasattr(team_status, "value") else team_status
            ),
            "team_work_items": task_count,
            "team": team_details,
            "agent_events": task_count,
            "grading": {
                "resolved": grading_result.resolved,
                "fix_rate": grading_result.fix_rate,
                "fail_to_pass_passed": grading_result.fail_to_pass_passed,
                "fail_to_pass_total": grading_result.fail_to_pass_total,
                "pass_to_pass_broken": grading_result.pass_to_pass_broken,
                "pass_to_pass_total": grading_result.pass_to_pass_total,
                "status": grading_result.status,
            },
        }
    finally:
        try:
            await shutdown_cached_client_async()
        except Exception:
            logger.debug("Failed to close cached AsyncDaytona client", exc_info=True)
