"""TaskCenter prompt helpers and mocked agent execution runner for SWE-EVO."""

from __future__ import annotations

import csv
import functools
import json
import logging
import os
from pathlib import Path
from typing import Any

from benchmarks.sweevo.dataset import select_sweevo_instance
from benchmarks.sweevo.evaluation import evaluate_sweevo_result
from benchmarks.sweevo.mock_agent_execution import (
    run_sweevo_task_center_with_mock_agent_execution,
)
from benchmarks.sweevo.models import (
    SWEEvoInstance,
    SWEEvoResult,
    _DEFAULT_DATASET_SOURCE,
    _DEFAULT_TARGET_BULLETS,
    _REPO_DIR,
)
from benchmarks.sweevo.sandbox import create_sweevo_test_sandbox

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
    """Return the benchmark prompt description for *instance*."""
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
    printer: Any | None = None,
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
    message_log_path: str | os.PathLike[str] | None = None,
    pr_description_csv_path: str | os.PathLike[str] | None = None,
) -> dict[str, Any]:
    """Run a SWE-EVO instance through TaskCenter with mocked agent execution.

    The sandbox is real and is prepared from the selected SWE-EVO image. The
    TaskCenter runtime is real; only the planner/executor/evaluator model
    execution is replaced with deterministic Python handlers that call the same
    sandbox and terminal submission tools the real agent loop would call.
    """
    del printer  # Mocked agent execution returns structured events.
    instance = select_sweevo_instance(
        source=source,
        instance_id=instance_id,
        size=size,
        target_bullets=target_bullets,
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
    user_prompt = build_sweevo_user_prompt(
        instance,
        repo_dir=repo_dir,
        csv_path=pr_description_csv_path,
    )
    run = await run_sweevo_task_center_with_mock_agent_execution(
        instance=instance,
        user_prompt=user_prompt,
        sandbox_id=str(sandbox_result["sandbox_id"]),
        repo_dir=repo_dir,
    )
    run["sandbox"] = sandbox_result.get("sandbox")
    run["snapshot_name"] = sandbox_result.get("snapshot_name", "")
    run["repo_dir"] = repo_dir
    run["prompt_source"] = {
        "csv_path": os.fspath(
            pr_description_csv_path
            or os.environ.get(_PR_DESCRIPTION_CSV_ENV)
            or _PR_DESCRIPTION_CSV_PATH
        ),
        "uses_pr_description": pr_description_for_instance(
            instance,
            csv_path=pr_description_csv_path,
        ).strip()
        != (instance.problem_statement or "").strip(),
    }

    if evaluate:
        run["grading"] = await _evaluate_mock_agent_execution_run(
            instance=instance,
            run=run,
            sandbox_id=str(sandbox_result["sandbox_id"]),
            repo_dir=repo_dir,
        )
    else:
        run["grading"] = None

    if message_log_path:
        _append_message_log(message_log_path, run)

    return run


async def _evaluate_mock_agent_execution_run(
    *,
    instance: SWEEvoInstance,
    run: dict[str, Any],
    sandbox_id: str,
    repo_dir: str,
) -> dict[str, Any]:
    result = SWEEvoResult(
        plan_id=str(run.get("task_center_run_id") or ""),
        instance_id=instance.instance_id,
        status=(
            "completed"
            if run.get("task_center_status") == "done"
            else "failed"
        ),
        duration_s=float(run.get("duration_s") or 0.0),
        task_count=int(run.get("task_count") or 0),
        tasks_completed=int(run.get("tasks_completed") or 0),
        tasks_failed=int(run.get("tasks_failed") or 0),
    )
    graded = await evaluate_sweevo_result(
        instance,
        result,
        sandbox_id=sandbox_id,
        repo_dir=repo_dir,
    )
    return {
        "resolved": graded.resolved,
        "fix_rate": graded.fix_rate,
        "fail_to_pass_passed": graded.fail_to_pass_passed,
        "fail_to_pass_total": graded.fail_to_pass_total,
        "pass_to_pass_broken": graded.pass_to_pass_broken,
        "pass_to_pass_total": graded.pass_to_pass_total,
        "status": graded.status,
        "error": graded.error,
    }


def _append_message_log(
    message_log_path: str | os.PathLike[str],
    run: dict[str, Any],
) -> None:
    path = Path(message_log_path)
    with path.open("a", encoding="utf-8") as handle:
        for launch in run.get("launches", []):
            handle.write(json.dumps({"type": "launch", **launch}) + "\n")
        for call in run.get("tool_calls", []):
            handle.write(json.dumps({"type": "tool_call", **call}) + "\n")
        handle.write(
            json.dumps(
                {
                    "type": "summary",
                    "task_center_run_id": run.get("task_center_run_id"),
                    "task_center_status": run.get("task_center_status"),
                    "grading": run.get("grading"),
                }
            )
            + "\n"
        )


__all__ = [
    "build_sweevo_user_prompt",
    "load_pr_description_overrides",
    "pr_description_for_instance",
    "run_sweevo_with_task_center",
]
