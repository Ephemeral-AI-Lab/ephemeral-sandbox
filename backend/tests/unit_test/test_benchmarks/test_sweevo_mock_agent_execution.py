from __future__ import annotations

import os
from pathlib import Path
from typing import Any

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.benchmarks.sweevo.setup import build_sweevo_user_prompt
import sandbox.api as sandbox_api
from sandbox.api import (
    ConflictInfo,
    EditFileRequest,
    EditFileResult,
    ReadFileRequest,
    ReadFileResult,
    ShellRequest,
    ShellResult,
    WriteFileRequest,
    WriteFileResult,
)
from task_center_runner.core.stores import create_per_test_task_center_stores
from task_center_runner.environments.sweevo_image.fixtures import (
    run_scenario_on_sweevo_image,
)
from task_center_runner.scenarios.correctness_testing import CorrectnessTesting

pytestmark = pytest.mark.skipif(
    not os.environ.get("EPHEMERALOS_DATABASE_URL"),
    reason=(
        "EPHEMERALOS_DATABASE_URL not configured — create_per_test_task_center_stores "
        "requires PostgreSQL"
    ),
)


_DASK_INSTANCE_ID = "dask__dask_2023.3.2_2023.4.0"
_REPO_DIR = "/testbed"
_PROBE_PATH = ".ephemeralos/sweevo-mock/probe.txt"


class _FakeSandboxApi:
    def __init__(self, repo_dir: str = _REPO_DIR) -> None:
        self.repo_dir = repo_dir.rstrip("/")
        self.files: dict[str, str] = {}

    async def write_file(
        self,
        _sandbox_id: str,
        request: WriteFileRequest,
        **_kwargs: object,
    ) -> WriteFileResult:
        path = self._key(request.path)
        self.files[path] = request.content
        return WriteFileResult(
            success=True,
            changed_paths=(request.path,),
            status="committed",
        )

    async def read_file(
        self,
        _sandbox_id: str,
        request: ReadFileRequest,
        **_kwargs: object,
    ) -> ReadFileResult:
        path = self._key(request.path)
        exists = path in self.files
        return ReadFileResult(
            success=exists,
            exists=exists,
            content=self.files.get(path, ""),
        )

    async def edit_file(
        self,
        _sandbox_id: str,
        request: EditFileRequest,
        **_kwargs: object,
    ) -> EditFileResult:
        path = self._key(request.path)
        content = self.files.get(path)
        if content is None:
            return self._conflict(request.path, "not_found")

        applied = 0
        updated = content
        for edit in request.edits:
            if edit.old_text not in updated:
                return EditFileResult(
                    success=False,
                    changed_paths=(request.path,),
                    status="old_text_not_found",
                    conflict=ConflictInfo(
                        reason="old_text_not_found",
                        conflict_file=request.path,
                        message="old text not found",
                    ),
                    conflict_reason="old_text_not_found",
                    applied_edits=applied,
                )
            updated = updated.replace(edit.old_text, edit.new_text, 1)
            applied += 1

        self.files[path] = updated
        return EditFileResult(
            success=True,
            changed_paths=(request.path,),
            status="committed",
            applied_edits=applied,
        )

    async def shell(
        self,
        _sandbox_id: str,
        request: ShellRequest,
        **_kwargs: object,
    ) -> ShellResult:
        command = request.command
        changed_paths: tuple[str, ...] = ()
        stdout = ""
        exit_code = 0
        success = True

        if "git rev-parse --is-inside-work-tree" in command:
            stdout = f"{self.repo_dir}\ntrue\n"
        elif "shell-created" in command:
            self.files[".ephemeralos/sweevo-mock/shell.txt"] = "shell-created\n"
            changed_paths = (".ephemeralos/sweevo-mock/shell.txt",)
        elif "squash-check" in command and ">>" in command:
            self.files[_PROBE_PATH] = self.files.get(_PROBE_PATH, "") + "squash-check\n"
            changed_paths = (_PROBE_PATH,)
        elif "grep -q 'squash-check'" in command:
            if "squash-check" not in self.files.get(_PROBE_PATH, ""):
                exit_code = 1
                success = False
        else:
            raise AssertionError(f"Unexpected shell command: {command}")

        return ShellResult(
            success=success,
            exit_code=exit_code,
            stdout=stdout,
            stderr="",
            changed_paths=changed_paths,
            status="committed" if success else "error",
        )

    @staticmethod
    def _conflict(path: str, reason: str) -> EditFileResult:
        return EditFileResult(
            success=False,
            changed_paths=(path,),
            status=reason,
            conflict=ConflictInfo(
                reason=reason,
                conflict_file=path,
                message=reason,
            ),
            conflict_reason=reason,
        )

    def _key(self, path: str) -> str:
        if path.startswith(f"{self.repo_dir}/"):
            return path[len(self.repo_dir) + 1 :]
        return path


def _instance(**overrides: Any) -> SWEEvoInstance:
    base = {
        "instance_id": _DASK_INSTANCE_ID,
        "repo": "dask/dask",
        "base_commit": "abc123",
        "problem_statement": "Dataset fallback problem statement",
        "patch": "",
        "fail_to_pass": ["dask/tests/test_cli.py::test_config_get"],
        "pass_to_pass": ["dask/tests/test_config.py::test_collect"],
        "docker_image": "example/image:1",
        "test_cmds": "pytest -q",
        "environment_setup_commit": "",
        "instance_id_swe": _DASK_INSTANCE_ID,
    }
    base.update(overrides)
    return SWEEvoInstance(**base)


@pytest.mark.asyncio
async def test_run_scenario_correctness_testing_with_fake_sandbox(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    fake = _FakeSandboxApi()
    monkeypatch.setattr(sandbox_api, "write_file", fake.write_file)
    monkeypatch.setattr(sandbox_api, "read_file", fake.read_file)
    monkeypatch.setattr(sandbox_api, "edit_file", fake.edit_file)
    monkeypatch.setattr(sandbox_api, "shell", fake.shell)

    instance = _instance()
    user_prompt = build_sweevo_user_prompt(instance, _REPO_DIR)

    bundle = create_per_test_task_center_stores()
    try:
        report = await run_scenario_on_sweevo_image(
            CorrectnessTesting(),
            instance=instance,
            sandbox_id="sbx-1",
            audit_dir=tmp_path,
            stores=bundle,
            repo_dir=_REPO_DIR,
            user_prompt=user_prompt,
        )
    finally:
        bundle.close()

    # --- Existing parity assertions -----------------------------------
    assert report.task_center_status == "done"
    assert report.passed_prompt_inspections
    assert report.passed_sandbox_checks

    delegated = [
        goal for goal in report.graph_summary["goals"] if len(goal["iterations"]) == 2
    ][0]
    assert delegated["status"] == "succeeded"
    assert [
        attempt["status"]
        for iteration in delegated["iterations"]
        for attempt in iteration["attempts"]
    ] == ["failed", "passed", "passed"]
    assert delegated["iterations"][0]["deferred_goal_for_next_iteration"]
    assert delegated["iterations"][1]["creation_reason"] == "partial_continuation"

    planner_reviews = [
        item for item in report.prompt_inspections if item.role == "planner"
    ]
    assert any(item.checks.get("failed_attempts") for item in planner_reviews)
    assert any(
        item.checks.get("previous_iteration_results") for item in planner_reviews
    )

    tool_names = {item.tool_name for item in report.tool_calls}
    assert {
        "submit_execution_handoff",
        "submit_plan_closes_goal",
        "submit_plan_defers_goal",
        "write_file",
        "read_file",
        "edit_file",
        "shell",
        "submit_execution_success",
        "submit_evaluation_failure",
        "submit_evaluation_success",
    } <= tool_names

    check_names = {item.name for item in report.sandbox_checks}
    assert {
        "tool.write_file.direct_merge",
        "tool.edit_file.direct_merge",
        "tool.shell.gated_merge",
        "tool.shell.squash_append",
        "api.edit_file.batch",
        "api.edit_file.conflict_detection",
    } <= check_names
    assert fake.files[_PROBE_PATH] == "alpha-batch\nbeta-batch\nsquash-check\n"

    # --- New audit-tree assertions ------------------------------------
    run_dir = report.run_dir
    assert run_dir.exists() and run_dir.is_dir()
    assert (run_dir / "run.json").exists()
    assert (run_dir / "metrics.json").exists()

    goal_dirs = list(run_dir.glob("goal_*_*"))
    assert goal_dirs, f"no goal_NN_<id> dir under {run_dir}"
    delegated_goal_dirs = []
    for goal_dir in goal_dirs:
        assert (goal_dir / "goal.json").exists()
        if list(goal_dir.glob("iteration_*_*")):
            delegated_goal_dirs.append(goal_dir)
    assert delegated_goal_dirs, "no goal with iterations — delegated path missing"
    found_attempt_with_role_dir = False
    for goal_dir in delegated_goal_dirs:
        iteration_dirs = list(goal_dir.glob("iteration_*_*"))
        for iteration_dir in iteration_dirs:
            assert (iteration_dir / "iteration.json").exists()
            attempt_dirs = list(iteration_dir.glob("attempt_*_*"))
            if not attempt_dirs:
                # Older run fixtures may include attempt-less entry artifacts.
                # Delegated goals must still contain normal attempts.
                continue
            for attempt_dir in attempt_dirs:
                assert (attempt_dir / "attempt.json").exists()
                role_dirs = list(attempt_dir.glob("[0-9][0-9]_*"))
                assert role_dirs, (
                    f"no NN_<role>_<task_id> dir under {attempt_dir}"
                )
                found_attempt_with_role_dir = True
                for role_dir in role_dirs:
                    assert (role_dir / "task.json").exists()
                    role_dir_name = role_dir.name
                    role_segment = role_dir_name.split("_", 2)[1]
                    # Helper / unknown roles must not earn an attempt-child dir.
                    assert role_segment in {
                        "planner",
                        "executor",
                        "evaluator",
                        "generator",
                    }
    assert found_attempt_with_role_dir, "no attempt_NN_<id> dir found anywhere"
