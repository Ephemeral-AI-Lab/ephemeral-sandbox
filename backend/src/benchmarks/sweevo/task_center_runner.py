"""TaskCenter runner helpers for SWE-EVO benchmark instances."""

from __future__ import annotations

import csv
import functools
import logging
import os
import time
from pathlib import Path
from typing import Any

from benchmarks.sweevo.dataset import select_sweevo_instance, summarize_sweevo_instance
from benchmarks.sweevo.evaluation import _extract_combined_patch, evaluate_sweevo_result
from benchmarks.sweevo.models import (
    SWEEvoInstance,
    SWEEvoResult,
    _DEFAULT_DATASET_SOURCE,
    _DEFAULT_TARGET_BULLETS,
    _REPO_DIR,
)
from benchmarks.sweevo.sandbox import create_sweevo_test_sandbox
from task_center.summaries import latest_summary_text

logger = logging.getLogger(__name__)

_PROJECT_ROOT = Path(__file__).resolve().parents[4]
_PR_DESCRIPTION_CSV_ENV = "SWEEVO_PR_DESCRIPTIONS_CSV"
_PR_DESCRIPTION_CSV_PATH = (
    _PROJECT_ROOT
    / "backend"
    / "config"
    / "benchmarks"
    / "sweevo_gpt5_2025_08_07_pr_descriptions.csv"
)


def _emit_progress(printer: Any, line: str) -> None:
    if printer is None or not hasattr(printer, "raw_line"):
        return
    try:
        printer.raw_line("task-center", line)
    except Exception:
        logger.debug("Failed to emit SWE-EVO progress line", exc_info=True)


@functools.lru_cache(maxsize=8)
def load_pr_description_overrides(csv_path: str) -> dict[str, str]:
    """Load SWE-EVO instance-id to PR-description overrides from a CSV."""
    path = Path(csv_path)
    if not path.exists():
        return {}

    descriptions: dict[str, str] = {}
    try:
        with path.open(encoding="utf-8", newline="") as handle:
            for row in csv.DictReader(handle):
                instance_id = str(row.get("test_folder") or "").strip()
                if not instance_id:
                    continue
                descriptions[instance_id] = str(row.get("pr_description") or "")
    except OSError:
        logger.debug("Unable to load SWE-EVO PR descriptions from %s", path, exc_info=True)
        return {}
    return descriptions


def pr_description_for_instance(
    instance: SWEEvoInstance,
    *,
    csv_path: str | os.PathLike[str] | None = None,
) -> str:
    """Return the benchmark prompt description for *instance*.

    The local GPT-5 SWE-EVO CSV is the primary source for the first user
    message, because it mirrors the benchmark logs. The dataset field and
    problem statement remain fallbacks for local or custom datasets.
    """
    resolved_csv = os.fspath(
        csv_path
        or os.environ.get(_PR_DESCRIPTION_CSV_ENV)
        or _PR_DESCRIPTION_CSV_PATH
    )
    overrides = load_pr_description_overrides(resolved_csv)
    for instance_id in (instance.instance_id, instance.instance_id_swe):
        if instance_id and (description := overrides.get(instance_id, "")).strip():
            return description

    explicit = getattr(instance, "pr_description", "")
    if explicit:
        return explicit
    return instance.problem_statement


def build_sweevo_user_prompt(
    instance: SWEEvoInstance,
    repo_dir: str = _REPO_DIR,
    *,
    csv_path: str | os.PathLike[str] | None = None,
) -> str:
    """Return the SWE-agent-style first user message for a SWE-EVO instance."""
    pr_description = pr_description_for_instance(instance, csv_path=csv_path).strip()
    return (
        f"<Workspace Root>\n"
        f"{repo_dir}\n"
        f"<Workspace Root>\n\n"
        f"I've uploaded a python code repository in the directory {repo_dir}. "
        f"Consider the following PR description:\n"
        f"<pr_description>\n"
        f"{pr_description}\n"
        f"</pr_description>\n\n"
        f"Can you help me implement the necessary changes to the repository so that "
        f"the requirements specified in the <pr_description> are met?\n"
        f"I've already taken care of all changes to any of the test files described "
        f"in the <pr_description>. This means you DON'T have to modify the testing "
        f"logic or any of the tests in any way!\n"
        f"Your task is to make the minimal changes to non-tests files in the "
        f"{repo_dir} directory to ensure the <pr_description> is satisfied."
    )


async def run_sweevo_with_task_center(
    *,
    printer: Any = None,
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
    evaluate: bool = True,
    pr_descriptions_csv: str | os.PathLike[str] | None = None,
) -> dict[str, Any]:
    """Run one SWE-EVO instance through the production TaskCenter path."""
    from agents.builtins import register_builtin_agents
    from config.model_config import NoActiveModelError, try_get_active_model_kwargs
    from config.settings import load_settings
    from server.app_factory import RuntimeConfig, ensure_runtime_stores_ready
    from task_center.runtime import TaskCenter, build_production_spawn

    try:
        from sandbox.lifecycle import shutdown_cached_client_async
    except Exception:
        shutdown_cached_client_async = None

    start = time.monotonic()
    try:
        settings = load_settings()
        ensure_runtime_stores_ready(settings)
        db_kwargs = try_get_active_model_kwargs() or {}
        if not db_kwargs.get("model"):
            raise NoActiveModelError(
                "SWE-EVO TaskCenter run requires an active model registration"
            )

        register_builtin_agents()

        instance = select_sweevo_instance(
            source=source,
            instance_id=instance_id,
            size=size,
            target_bullets=target_bullets,
        )
        instance_summary = summarize_sweevo_instance(instance)
        user_prompt = build_sweevo_user_prompt(
            instance,
            repo_dir,
            csv_path=pr_descriptions_csv,
        )

        _emit_progress(
            printer,
            (
                "[setup] "
                f"instance={instance.instance_id} repo={instance.repo} "
                f"size={instance_summary['size']} bullets={instance_summary['bullet_count']} "
                f"prompt_chars={len(user_prompt)}"
            ),
        )
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
        sandbox_id = str(sandbox_result["sandbox_id"])
        _emit_progress(
            printer,
            (
                "[setup] "
                f"sandbox_id={sandbox_id} reused_existing="
                f"{sandbox_result.get('reused_existing', False)}"
            ),
        )

        runtime_config = RuntimeConfig(cwd=str(_PROJECT_ROOT))
        task_center = TaskCenter(
            runtime_config,
            spawn_func=build_production_spawn(
                runtime_config,
                extra_tool_metadata={
                    "repo_root": repo_dir,
                    "exec_cwd": repo_dir,
                    "ci_workspace_root": repo_dir,
                    "verification_surface_write_enforcement": "warn",
                },
            ),
        )

        events: list[Any] = []

        async def _on_event(event: Any) -> None:
            events.append(event)
            if printer is not None and hasattr(printer, "emit"):
                printer.emit(event)

        task_center.set_event_callback(_on_event)
        try:
            root = await task_center.run_query(user_prompt, sandbox_id=sandbox_id)
        finally:
            task_center.set_event_callback(None)
            if printer is not None and hasattr(printer, "flush"):
                printer.flush()

        agent_patch = await _extract_combined_patch(sandbox_id, repo_dir)

        grading: dict[str, Any] | None = None
        if evaluate:
            _emit_progress(
                printer,
                "[grading] evaluating fail-to-pass and pass-to-pass results",
            )
            grading_result = await evaluate_sweevo_result(
                instance,
                SWEEvoResult(
                    plan_id="task-center",
                    instance_id=instance.instance_id,
                    status="completed",
                    agent_patch=agent_patch,
                    duration_s=time.monotonic() - start,
                    task_count=len(task_center.graph.tasks),
                    tasks_completed=sum(
                        1
                        for task in task_center.graph.tasks.values()
                        if task.status.value == "done"
                    ),
                    tasks_failed=sum(
                        1
                        for task in task_center.graph.tasks.values()
                        if task.status.value == "failed"
                    ),
                ),
                sandbox_id,
                repo_dir=repo_dir,
            )
            grading = {
                "resolved": grading_result.resolved,
                "fix_rate": grading_result.fix_rate,
                "fail_to_pass_passed": grading_result.fail_to_pass_passed,
                "fail_to_pass_total": grading_result.fail_to_pass_total,
                "pass_to_pass_broken": grading_result.pass_to_pass_broken,
                "pass_to_pass_total": grading_result.pass_to_pass_total,
                "status": grading_result.status,
            }

        return {
            "instance": instance_summary,
            "sandbox": sandbox_result["sandbox"],
            "snapshot_name": sandbox_result["snapshot_name"],
            "repo_dir": repo_dir,
            "user_prompt": user_prompt,
            "agent_patch": agent_patch,
            "task_center_status": root.status.value,
            "root_task_id": root.id,
            "root_summary": latest_summary_text(root) or "",
            "task_count": len(task_center.graph.tasks),
            "tasks_completed": sum(
                1 for task in task_center.graph.tasks.values() if task.status.value == "done"
            ),
            "tasks_failed": sum(
                1 for task in task_center.graph.tasks.values() if task.status.value == "failed"
            ),
            "agent_events": len(events),
            "duration_s": time.monotonic() - start,
            "grading": grading,
        }
    finally:
        if shutdown_cached_client_async is not None:
            try:
                await shutdown_cached_client_async()
            except Exception:
                logger.debug("Failed to close cached AsyncDaytona client", exc_info=True)


__all__ = [
    "build_sweevo_user_prompt",
    "load_pr_description_overrides",
    "pr_description_for_instance",
    "run_sweevo_with_task_center",
]
