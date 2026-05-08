from __future__ import annotations

from typing import Any

import pytest

import sandbox.api as sandbox_api
from benchmarks.sweevo.mock_agent_execution import (
    run_sweevo_task_center_with_mock_agent_execution,
)
from benchmarks.sweevo.models import SWEEvoInstance
from benchmarks.sweevo.task_center_runner import build_sweevo_user_prompt
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
async def test_real_task_center_with_mock_agent_execution_context_and_sandbox_tools(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fake = _FakeSandboxApi()
    monkeypatch.setattr(sandbox_api, "write_file", fake.write_file)
    monkeypatch.setattr(sandbox_api, "read_file", fake.read_file)
    monkeypatch.setattr(sandbox_api, "edit_file", fake.edit_file)
    monkeypatch.setattr(sandbox_api, "shell", fake.shell)

    instance = _instance()
    prompt = build_sweevo_user_prompt(instance, _REPO_DIR)

    report = await run_sweevo_task_center_with_mock_agent_execution(
        instance=instance,
        user_prompt=prompt,
        sandbox_id="sbx-1",
        repo_dir=_REPO_DIR,
    )

    assert report["task_center_status"] == "done"
    assert all(item["passed"] for item in report["prompt_inspections"])
    assert all(item["passed"] for item in report["sandbox_checks"])

    delegated = [
        mission
        for mission in report["graph"]["missions"]
        if len(mission["episodes"]) == 2
    ][0]
    assert delegated["status"] == "succeeded"
    assert [
        attempt["status"]
        for episode in delegated["episodes"]
        for attempt in episode["attempts"]
    ] == ["failed", "passed", "passed"]
    assert delegated["episodes"][0]["continuation_goal"]
    assert delegated["episodes"][1]["creation_reason"] == "partial_continuation"

    planner_reviews = [
        item for item in report["prompt_inspections"] if item["role"] == "planner"
    ]
    assert any(item["checks"].get("failed_attempts") for item in planner_reviews)
    assert any(
        item["checks"].get("previous_episode_results") for item in planner_reviews
    )

    tool_names = {item["tool_name"] for item in report["tool_calls"]}
    assert {
        "request_mission_solution",
        "submit_full_plan",
        "submit_partial_plan",
        "write_file",
        "read_file",
        "edit_file",
        "shell",
        "submit_execution_success",
        "submit_evaluation_failure",
        "submit_evaluation_success",
    } <= tool_names

    check_names = {item["name"] for item in report["sandbox_checks"]}
    assert {
        "tool.write_file.direct_merge",
        "tool.edit_file.direct_merge",
        "tool.shell.gated_merge",
        "tool.shell.squash_append",
        "api.edit_file.batch",
        "api.edit_file.conflict_detection",
    } <= check_names
    assert fake.files[_PROBE_PATH] == "alpha-batch\nbeta-batch\nsquash-check\n"
