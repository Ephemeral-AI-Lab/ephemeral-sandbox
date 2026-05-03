"""Parity tests for daemon-local and multistage overlay readback."""

from __future__ import annotations

import asyncio
import base64
import json
import re
import subprocess
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from sandbox.runtime.shell_command_executor import AuditedCommandExecutor
from sandbox.overlay.engine import runner as overlay_runner
from sandbox.runtime.registry import dispose_all_code_intelligence
from sandbox.runtime.service import CodeIntelligenceService


@pytest.fixture(autouse=True)
def _registry() -> None:
    dispose_all_code_intelligence()
    yield
    dispose_all_code_intelligence()


def _meta_line(**overrides: Any) -> str:
    base = {
        "exit_code": 0,
        "upper_bytes": 0,
        "upper_files": 0,
        "upper_changes": 0,
        "run_timings": {"total": 0.6, "walk_upperdir": 0.07},
        "warnings": [],
    }
    base.update(overrides)
    return json.dumps({"_meta": base}, separators=(",", ":"))


def _change_line(rel: str, *, base: bytes | None, upper: bytes) -> str:
    return json.dumps(
        {
            "rel": rel,
            "kind": "regular",
            "base_bytes_b64": (
                None if base is None else base64.b64encode(base).decode("ascii")
            ),
            "upper_bytes_b64": base64.b64encode(upper).decode("ascii"),
            "base_existed": base is not None,
        },
        separators=(",", ":"),
    )


def _diff_for_case(case: str) -> tuple[str, int, str]:
    if case == "gitinclude":
        return (
            "\n".join(
                [
                    _meta_line(upper_changes=1, upper_files=1, upper_bytes=4),
                    _change_line("app.py", base=b"old\n", upper=b"new\n"),
                ]
            ),
            0,
            "gitinclude stdout\n",
        )
    return (_meta_line(exit_code=0), 0, "noop stdout\n")


def _init_repo(path: Path) -> None:
    subprocess.run(["git", "-C", str(path), "init", "-q"], check=True)
    subprocess.run(
        ["git", "-C", str(path), "config", "user.email", "t@example.invalid"],
        check=True,
    )
    subprocess.run(
        ["git", "-C", str(path), "config", "user.name", "Test"], check=True
    )


def _make_repo(tmp_path: Path, name: str) -> Path:
    repo = tmp_path / name
    repo.mkdir()
    _init_repo(repo)
    (repo / "app.py").write_text("old\n", encoding="utf-8")
    return repo


class _ScriptedSandbox:
    def __init__(self, *, diff_contents: str, user_exit: int, stdout: str) -> None:
        self._diff_contents = diff_contents
        self._user_exit = user_exit
        self._stdout = stdout

    async def exec(self, command: str, timeout: int | None = None) -> SimpleNamespace:
        del timeout
        if "unshare -Urm" in command:
            match = re.search(r"--run-dir\s+(\S+)", command)
            if match is None:
                return SimpleNamespace(result="missing run-dir", exit_code=1)
            run_dir = Path(match.group(1))
            run_dir.mkdir(parents=True, exist_ok=True)
            (run_dir / "diff.ndjson").write_text(
                self._diff_contents, encoding="utf-8"
            )
            (run_dir / "stdout.bin").write_text(self._stdout, encoding="utf-8")
            return SimpleNamespace(result="", exit_code=self._user_exit)
        completed = await asyncio.to_thread(
            subprocess.run,
            command,
            shell=True,
            text=True,
            capture_output=True,
            check=False,
        )
        return SimpleNamespace(
            result=completed.stdout + completed.stderr,
            exit_code=completed.returncode,
        )


async def _noop_exec(sandbox: _ScriptedSandbox, command: str, *, timeout=None):
    return await sandbox.exec(command, timeout=timeout)


async def _should_not_exec(_sandbox: Any, _command: str, *, timeout=None) -> None:
    del timeout
    raise AssertionError("daemon-local overlay branch should not call _do_exec")


def _make_executor(
    svc: CodeIntelligenceService,
    sandbox_id: str,
    workspace_root: str,
    *,
    daemon_local: bool,
    bridge,
) -> AuditedCommandExecutor:
    executor = AuditedCommandExecutor(
        sandbox_id=sandbox_id,
        workspace_root=workspace_root,
        write_coordinator=svc._write_coordinator,
        rebind_sandbox=lambda _sandbox: None,
        transport=None,
        daemon_local=daemon_local,
    )
    executor._exec_sandbox_process = bridge  # type: ignore[assignment]
    return executor


def _install_daemon_subprocess(monkeypatch: pytest.MonkeyPatch, diff: str, stdout: str):
    original_run = subprocess.run

    def _fake_run(argv: list[str], **kwargs: Any) -> subprocess.CompletedProcess[str]:
        if argv[0] != "unshare":
            return original_run(argv, **kwargs)
        match = re.search(r"--run-dir\s+(\S+)", argv[-1])
        if match is None:
            return subprocess.CompletedProcess(argv, 1, "", "missing run-dir")
        run_dir = Path(match.group(1))
        run_dir.mkdir(parents=True, exist_ok=True)
        (run_dir / "diff.ndjson").write_text(diff, encoding="utf-8")
        (run_dir / "stdout.bin").write_text(stdout, encoding="utf-8")
        (run_dir / "result.json").write_text(
            json.dumps(
                {"exit_code": 0, "rejected": None, "run_timings": {}},
                separators=(",", ":"),
            )
            + "\n",
            encoding="utf-8",
        )
        return subprocess.CompletedProcess(argv, 0, "", "")

    monkeypatch.setattr(overlay_runner.subprocess, "run", _fake_run)


async def _run_multistage(repo: Path, case: str) -> dict[str, Any]:
    diff, user_exit, stdout = _diff_for_case(case)
    sandbox = _ScriptedSandbox(
        diff_contents=diff,
        user_exit=user_exit,
        stdout=stdout,
    )
    svc = CodeIntelligenceService(
        sandbox_id=f"multi-{case}-{repo.name}",
        workspace_root=str(repo),
        sandbox=sandbox,
    )
    executor = _make_executor(
        svc,
        f"multi-{case}-{repo.name}",
        str(repo),
        daemon_local=False,
        bridge=_noop_exec,
    )
    result = await executor.cmd(sandbox, "echo phase6", timeout=60)
    return {
        "result": result.result,
        "changed_paths": [Path(p).name for p in result.changed_paths],
        "conflict_reason": result.conflict_reason,
    }


async def _run_daemon_local(
    repo: Path,
    case: str,
    monkeypatch: pytest.MonkeyPatch,
) -> dict[str, Any]:
    diff, _user_exit, stdout = _diff_for_case(case)
    _install_daemon_subprocess(monkeypatch, diff, stdout)
    svc = CodeIntelligenceService(
        sandbox_id=f"daemon-{case}-{repo.name}",
        workspace_root=str(repo),
    )
    executor = _make_executor(
        svc,
        f"daemon-{case}-{repo.name}",
        str(repo),
        daemon_local=True,
        bridge=_should_not_exec,
    )
    result = await executor.cmd(None, "echo phase6", timeout=60)
    return {
        "result": result.result,
        "changed_paths": [Path(p).name for p in result.changed_paths],
        "conflict_reason": result.conflict_reason,
    }


@pytest.mark.asyncio
@pytest.mark.parametrize("case", ["noop", "gitinclude"])
async def test_daemon_local_branch_matches_multistage_result_shape(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    case: str,
) -> None:
    multistage_repo = _make_repo(tmp_path, f"multi-{case}")
    daemon_repo = _make_repo(tmp_path, f"daemon-{case}")

    multistage = await _run_multistage(multistage_repo, case)
    daemon = await _run_daemon_local(daemon_repo, case, monkeypatch)

    assert daemon == multistage
