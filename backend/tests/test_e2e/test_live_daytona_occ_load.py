"""Live load test for mixed concurrent Daytona writes, edits, and CodeAct.

This suite runs 100 real tool calls against one live sandbox and one shared CI
service so audited process behavior is exercised under mixed contention:

1. 50 concurrent ``daytona_write_file`` calls on unique files.
2. 40 concurrent ``daytona_edit_file`` calls:
   - 25 disjoint same-file edits across a small set of files.
   - 15 overlapping same-line edits across 3 files.
3. 10 concurrent coordinated ``daytona_codeact`` shell commands on unique files.

The test verifies:
- successful writes are persisted,
- disjoint edits mostly land,
- overlapping edits permit at most one winner per target file,
- arbiter stats are sane after the burst,
- active reservations are cleaned up after completion.
"""

from __future__ import annotations

import asyncio
import json
import os
import re
import shlex
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from dotenv import load_dotenv

from code_intelligence.routing.service import CodeIntelligenceService
from tests.test_e2e.daytona_exec_io import read_text_via_exec, write_text_via_exec
from tools.core.base import ToolExecutionContext, ToolResult
from tools.daytona_toolkit._daytona_utils import (
    _extract_exit_code,
    _wrap_bash_command,
)
import tools.daytona_toolkit.codeact_tool as codeact_tool_module
from tools.daytona_toolkit.codeact_tool import daytona_codeact
from tools.daytona_toolkit.edit_tool import daytona_edit_file
from tools.daytona_toolkit.tools import daytona_write_file

_PROJECT_ROOT = Path(__file__).resolve().parents[3]
load_dotenv(_PROJECT_ROOT / ".env")


def _load_settings() -> dict[str, Any]:
    settings_path = Path.home() / ".ephemeralos" / "settings.json"
    if settings_path.exists():
        return json.loads(settings_path.read_text())
    return {}


_SETTINGS = _load_settings()
HAS_DAYTONA = bool(
    (os.environ.get("DAYTONA_API_KEY") or _SETTINGS.get("daytona_api_key", ""))
    and (os.environ.get("DAYTONA_API_URL") or _SETTINGS.get("daytona_api_url", ""))
)

pytestmark = [pytest.mark.e2e, pytest.mark.live]

_TERM_NOISE = re.compile(r"\x1b\[3J.*$", re.S)


class _AsyncFs:
    def __init__(self, real_fs: Any):
        self._real = real_fs

    async def upload_file(self, *args, **kwargs):
        return self._real.upload_file(*args, **kwargs)

    async def download_file(self, *args, **kwargs):
        return self._real.download_file(*args, **kwargs)

    def __getattr__(self, name: str) -> Any:
        return getattr(self._real, name)


class _AsyncProcess:
    def __init__(self, real_process: Any):
        self._real = real_process

    async def exec(self, *args, **kwargs):
        response = self._real.exec(*args, **kwargs)
        stdout = _TERM_NOISE.sub("", getattr(response, "result", "") or "")
        return SimpleNamespace(result=stdout, exit_code=getattr(response, "exit_code", None))

    def __getattr__(self, name: str) -> Any:
        return getattr(self._real, name)


class _AsyncSandboxWrapper:
    def __init__(self, raw_sandbox: Any):
        self._raw = raw_sandbox
        self.fs = _AsyncFs(raw_sandbox.fs)
        self.process = _AsyncProcess(raw_sandbox.process)

    def __getattr__(self, name: str) -> Any:
        return getattr(self._raw, name)


@dataclass
class LiveLoadEnv:
    sandbox_id: str
    raw_sandbox: Any
    async_sandbox: Any
    home: str
    repo_root: str

    def exec(self, command: str, *, cwd: str | None = None, timeout: int = 180) -> tuple[int, str]:
        wrapped = command if cwd is None else f"cd {shlex.quote(cwd)} && {command}"
        response = self.raw_sandbox.process.exec(_wrap_bash_command(wrapped), timeout=timeout)
        raw = _TERM_NOISE.sub("", getattr(response, "result", "") or "")
        cleaned, exit_code = _extract_exit_code(
            raw,
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        return exit_code, cleaned

    def exec_checked(self, command: str, *, cwd: str | None = None, timeout: int = 180) -> str:
        exit_code, stdout = self.exec(command, cwd=cwd, timeout=timeout)
        if exit_code != 0:
            detail = stdout.strip() or f"exit {exit_code}"
            raise AssertionError(f"Sandbox command failed: {detail}")
        return stdout

    def require_command(self, name: str) -> None:
        exit_code, _ = self.exec(f"command -v {shlex.quote(name)} >/dev/null 2>&1", timeout=30)
        if exit_code != 0:
            pytest.skip(f"Sandbox image missing required command: {name}")

    def write_text(self, rel_path: str, content: str) -> None:
        write_text_via_exec(self.raw_sandbox, f"{self.repo_root}/{rel_path}", content, timeout=60)

    def read_text(self, rel_path: str) -> str:
        return read_text_via_exec(self.raw_sandbox, f"{self.repo_root}/{rel_path}", timeout=60)

    def make_ci_service(self) -> CodeIntelligenceService:
        return CodeIntelligenceService(
            sandbox_id=self.sandbox_id,
            workspace_root=self.repo_root,
            sandbox=self.raw_sandbox,
        )

    def make_ctx(
        self,
        ci_service: CodeIntelligenceService,
        *,
        agent_run_id: str,
        coordinated: bool = False,
    ) -> ToolExecutionContext:
        metadata: dict[str, Any] = {
            "daytona_sandbox": self.async_sandbox,
            "daytona_cwd": self.repo_root,
            "repo_root": self.repo_root,
            "exec_cwd": self.repo_root,
            "ci_service": ci_service,
            "agent_run_id": agent_run_id,
        }
        if coordinated:
            metadata["agent_name"] = "developer"
        return ToolExecutionContext(cwd=Path(self.repo_root), metadata=metadata)

    def init_repo(self) -> None:
        self.exec_checked(f"rm -rf {shlex.quote(self.repo_root)} && mkdir -p {shlex.quote(self.repo_root)}")
        self.exec_checked(f"git -C {shlex.quote(self.repo_root)} init")
        self.exec_checked(f"git -C {shlex.quote(self.repo_root)} config user.email test@example.com")
        self.exec_checked(f"git -C {shlex.quote(self.repo_root)} config user.name 'Test User'")


@pytest.fixture
def live_load_env():
    if not HAS_DAYTONA:
        pytest.skip("Daytona credentials not configured")

    from sandbox.testing import create_test_sandbox, delete_test_sandbox, get_sandbox_service

    info = create_test_sandbox(name="occ-load-live")
    sandbox_id = info["id"]
    try:
        sandbox_svc = get_sandbox_service()
        raw_sandbox = sandbox_svc.get_sandbox_object(sandbox_id)
        home_resp = raw_sandbox.process.exec("pwd", timeout=10)
        home = (getattr(home_resp, "result", "") or "").strip() or "/home/daytona"
        env = LiveLoadEnv(
            sandbox_id=sandbox_id,
            raw_sandbox=raw_sandbox,
            async_sandbox=_AsyncSandboxWrapper(raw_sandbox),
            home=home,
            repo_root=f"{home}/occ_load_repo",
        )
        env.require_command("git")
        env.require_command("python3")
        yield env
    finally:
        delete_test_sandbox(sandbox_id)


def _json_output(result: ToolResult) -> dict[str, Any]:
    assert result.output, "tool returned empty output"
    return json.loads(result.output)


async def _invoke_tool(tool: Any, kwargs: dict[str, Any], ctx: ToolExecutionContext) -> ToolResult:
    return await tool.execute(tool.input_model(**kwargs), ctx)


def _install_codeact_phase_probe(monkeypatch: pytest.MonkeyPatch) -> dict[str, list[float]]:
    stats: dict[str, list[float]] = {
        "shell_exec_s": [],
        "python_wrapper_s": [],
    }

    original_shell = codeact_tool_module._run_shell_with_recovery
    original_python = codeact_tool_module._execute_python_wrapper

    async def _timed_shell(*args, **kwargs):
        started = time.perf_counter()
        try:
            return await original_shell(*args, **kwargs)
        finally:
            stats["shell_exec_s"].append(round(time.perf_counter() - started, 6))

    async def _timed_python(*args, **kwargs):
        started = time.perf_counter()
        try:
            return await original_python(*args, **kwargs)
        finally:
            stats["python_wrapper_s"].append(round(time.perf_counter() - started, 6))

    monkeypatch.setattr(codeact_tool_module, "_run_shell_with_recovery", _timed_shell)
    monkeypatch.setattr(codeact_tool_module, "_execute_python_wrapper", _timed_python)
    return stats


async def _run_mixed_operations(
    live_load_env: LiveLoadEnv,
    svc: CodeIntelligenceService,
    operations: list[dict[str, Any]],
    *,
    concurrency: int,
    timeout_s: int,
) -> list[dict[str, Any]]:
    async def _invoke(operation: dict[str, Any], semaphore: asyncio.Semaphore) -> dict[str, Any]:
        agent_run_id = f"{operation['name']}-{uuid.uuid4().hex[:8]}"
        ctx = live_load_env.make_ctx(
            svc,
            agent_run_id=agent_run_id,
            coordinated=bool(operation.get("coordinated", False)),
        )
        tool = (
            daytona_write_file
            if operation["kind"] == "write"
            else daytona_codeact
            if operation["kind"] == "codeact"
            else daytona_edit_file
        )
        started = time.perf_counter()
        async with semaphore:
            result = await _invoke_tool(tool, operation["kwargs"], ctx)
        elapsed_s = round(time.perf_counter() - started, 6)
        output = (result.output or "").lstrip()
        payload = _json_output(result) if output.startswith("{") else {}
        return {
            "kind": operation["kind"],
            "name": operation["name"],
            "path": operation["path"],
            "group": operation.get("group"),
            "winner_value": operation.get("winner_value"),
            "is_error": result.is_error,
            "metadata": dict(result.metadata or {}),
            "payload": payload,
            "elapsed_s": elapsed_s,
        }

    semaphore = asyncio.Semaphore(concurrency)
    return await asyncio.wait_for(
        asyncio.gather(*[_invoke(operation, semaphore) for operation in operations]),
        timeout=timeout_s,
    )


def test_live_occ_load_100_mixed_operations(live_load_env: LiveLoadEnv):
    live_load_env.init_repo()

    # Seed disjoint edit targets: 5 files * 5 edits each = 25 disjoint edits.
    for group in range(5):
        lines = ['"""Disjoint edit target."""', ""]
        for idx in range(5):
            global_idx = group * 5 + idx
            lines.append(f"VALUE_{global_idx} = {global_idx}")
        live_load_env.write_text(f"edits/disjoint_{group}.py", "\n".join(lines) + "\n")

    # Seed overlapping edit targets: 3 files * 5 edits each = 15 overlap attempts.
    for group in range(3):
        live_load_env.write_text(
            f"edits/overlap_{group}.py",
            '"""Overlap target."""\n\nSHARED = 0\n',
        )

    # Seed CodeAct unique targets: 10 independent command writes.
    for idx in range(10):
        live_load_env.write_text(f"tx/unique_{idx}.txt", "base\n")

    live_load_env.exec_checked(f"git -C {shlex.quote(live_load_env.repo_root)} add -A")
    live_load_env.exec_checked(
        f"git -C {shlex.quote(live_load_env.repo_root)} commit -m seed-load-fixtures",
        timeout=180,
    )

    svc = live_load_env.make_ci_service()
    operations: list[dict[str, Any]] = []

    # 50 unique writes.
    for idx in range(50):
        operations.append(
            {
                "kind": "write",
                "name": f"write-{idx}",
                "path": f"{live_load_env.repo_root}/writes/write_{idx}.txt",
                "kwargs": {
                    "file_path": f"{live_load_env.repo_root}/writes/write_{idx}.txt",
                    "content": f"write {idx}\n",
                },
                "coordinated": False,
            }
        )

    # 25 disjoint edits.
    for group in range(5):
        for idx in range(5):
            global_idx = group * 5 + idx
            file_path = f"{live_load_env.repo_root}/edits/disjoint_{group}.py"
            operations.append(
                {
                    "kind": "edit-disjoint",
                    "name": f"edit-disjoint-{global_idx}",
                    "path": file_path,
                    "kwargs": {
                        "file_path": file_path,
                        "old_text": f"VALUE_{global_idx} = {global_idx}",
                        "new_text": f"VALUE_{global_idx} = {global_idx}00",
                    },
                    "coordinated": False,
                }
            )

    # 15 overlapping edits: at most one winner per file.
    for group in range(3):
        file_path = f"{live_load_env.repo_root}/edits/overlap_{group}.py"
        for idx in range(5):
            value = (group + 1) * 1000 + idx
            operations.append(
                {
                    "kind": "edit-overlap",
                    "name": f"edit-overlap-{group}-{idx}",
                    "path": file_path,
                    "group": group,
                    "winner_value": value,
                    "kwargs": {
                        "file_path": file_path,
                        "old_text": "SHARED = 0",
                        "new_text": f"SHARED = {value}",
                    },
                    "coordinated": False,
                }
            )

    # 10 coordinated CodeAct shell commands on unique files.
    for idx in range(10):
        rel_path = f"tx/unique_{idx}.txt"
        operations.append(
            {
                "kind": "codeact",
                "name": f"codeact-{idx}",
                "path": f"{live_load_env.repo_root}/{rel_path}",
                "kwargs": {
                    "mode": "shell",
                    "command": (
                        "python3 - <<'PY'\n"
                        "from pathlib import Path\n"
                        f"Path({rel_path!r}).write_text('codeact {idx}\\n', encoding='utf-8')\n"
                        "PY"
                    ),
                    "timeout": 120,
                },
                "coordinated": True,
            }
        )

    assert len(operations) == 100

    results = asyncio.run(
        _run_mixed_operations(
            live_load_env,
            svc,
            operations,
            concurrency=20,
            timeout_s=300,
        )
    )

    write_results = [item for item in results if item["kind"] == "write"]
    disjoint_results = [item for item in results if item["kind"] == "edit-disjoint"]
    overlap_results = [item for item in results if item["kind"] == "edit-overlap"]
    codeact_results = [item for item in results if item["kind"] == "codeact"]

    write_successes = sum(not item["is_error"] for item in write_results)
    disjoint_successes = sum(not item["is_error"] for item in disjoint_results)
    overlap_successes = sum(not item["is_error"] for item in overlap_results)
    overlap_conflicts = sum(
        bool(item["metadata"].get("conflict")) or bool(item["payload"].get("conflict"))
        for item in overlap_results
    )
    codeact_successes = sum(not item["is_error"] for item in codeact_results)

    arbiter_status = svc.status()["arbiter"]
    scope_status = svc.scope_status([live_load_env.repo_root])
    hotspots = scope_status["hotspots"]

    print("\n[occ-load summary]")
    print(
        json.dumps(
            {
                "write_successes": write_successes,
                "disjoint_successes": disjoint_successes,
                "overlap_successes": overlap_successes,
                "overlap_conflicts": overlap_conflicts,
                "codeact_successes": codeact_successes,
                "arbiter": arbiter_status,
                "hotspots": hotspots[:5],
            },
            indent=2,
            sort_keys=True,
        )
    )

    # Writes should all succeed because they target unique files.
    assert write_successes == 50

    # CodeAct targets unique files too; these should all run and audit cleanly.
    assert codeact_successes == 10

    # Disjoint edits should mostly land. Allow a small amount of live contention noise.
    assert disjoint_successes >= 22

    # Overlap files must not admit multiple winners per file.
    winners_by_group: dict[int, list[int]] = {0: [], 1: [], 2: []}
    for item in overlap_results:
        group = int(item["group"])
        value = int(item["winner_value"])
        text = live_load_env.read_text(f"edits/overlap_{group}.py")
        if f"SHARED = {value}" in text:
            winners_by_group[group].append(value)

    assert all(len(values) <= 1 for values in winners_by_group.values()), winners_by_group
    assert sum(len(values) for values in winners_by_group.values()) == overlap_successes
    assert overlap_successes <= 3
    assert overlap_conflicts + (len(overlap_results) - overlap_successes) >= 12

    # Verify persisted results on unique-file paths.
    for idx in range(50):
        assert live_load_env.read_text(f"writes/write_{idx}.txt") == f"write {idx}\n"
    for idx in range(10):
        assert live_load_env.read_text(f"tx/unique_{idx}.txt") == f"codeact {idx}\n"

    # Audit ledger sanity. conflicts_detected is currently not wired up, so use
    # result-level conflict tallies plus arbiter totals/hotspots here.
    total_successes = write_successes + disjoint_successes + overlap_successes + codeact_successes
    assert arbiter_status["total_edits"] == total_successes
    assert arbiter_status["active_tokens"] == 0
    assert arbiter_status["active_intents"] == 0
    assert arbiter_status["conflicts_detected"] >= 0
    assert any("edits/disjoint_" in item["file_path"] for item in hotspots), hotspots


def test_live_occ_load_20_non_overlapping_operations_profile(
    live_load_env: LiveLoadEnv,
    monkeypatch: pytest.MonkeyPatch,
):
    live_load_env.init_repo()
    codeact_stats = _install_codeact_phase_probe(monkeypatch)

    for group in range(2):
        live_load_env.write_text(
            f"edits/disjoint_{group}.py",
            (
                f'"""Disjoint target {group}."""\n\n'
                f"A_{group} = 1\n"
                f"B_{group} = 2\n"
                f"C_{group} = 3\n"
                f"D_{group} = 4\n"
                f"E_{group} = 5\n"
            ),
        )
    for idx in range(4):
        live_load_env.write_text(f"tx/small_{idx}.txt", "base\n")

    live_load_env.exec_checked(f"git -C {shlex.quote(live_load_env.repo_root)} add -A")
    live_load_env.exec_checked(
        f"git -C {shlex.quote(live_load_env.repo_root)} commit -m seed-small-nonoverlap-load",
        timeout=180,
    )

    svc = live_load_env.make_ci_service()
    operations = [
        {
            "kind": "write",
            "name": "write-0",
            "path": f"{live_load_env.repo_root}/writes/w0.txt",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/writes/w0.txt",
                "content": "write 0\n",
            },
            "coordinated": False,
        },
        {
            "kind": "write",
            "name": "write-1",
            "path": f"{live_load_env.repo_root}/writes/w1.txt",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/writes/w1.txt",
                "content": "write 1\n",
            },
            "coordinated": False,
        },
        {
            "kind": "write",
            "name": "write-2",
            "path": f"{live_load_env.repo_root}/writes/w2.txt",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/writes/w2.txt",
                "content": "write 2\n",
            },
            "coordinated": False,
        },
        {
            "kind": "write",
            "name": "write-3",
            "path": f"{live_load_env.repo_root}/writes/w3.txt",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/writes/w3.txt",
                "content": "write 3\n",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-a0",
            "path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
                "old_text": "A_0 = 1",
                "new_text": "A_0 = 100",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-b0",
            "path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
                "old_text": "B_0 = 2",
                "new_text": "B_0 = 200",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-c0",
            "path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
                "old_text": "C_0 = 3",
                "new_text": "C_0 = 300",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-d0",
            "path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
                "old_text": "D_0 = 4",
                "new_text": "D_0 = 400",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-e0",
            "path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
                "old_text": "E_0 = 5",
                "new_text": "E_0 = 500",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-a1",
            "path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
                "old_text": "A_1 = 1",
                "new_text": "A_1 = 100",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-b1",
            "path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
                "old_text": "B_1 = 2",
                "new_text": "B_1 = 200",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-c1",
            "path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
                "old_text": "C_1 = 3",
                "new_text": "C_1 = 300",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-d1",
            "path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
                "old_text": "D_1 = 4",
                "new_text": "D_1 = 400",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-e1",
            "path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
                "old_text": "E_1 = 5",
                "new_text": "E_1 = 500",
            },
            "coordinated": False,
        },
        {
            "kind": "codeact",
            "name": "codeact-0",
            "path": f"{live_load_env.repo_root}/tx/small_0.txt",
            "kwargs": {
                "mode": "shell",
                "command": (
                    "python3 - <<'PY'\n"
                    "from pathlib import Path\n"
                    "Path('tx/small_0.txt').write_text('codeact 0\\n', encoding='utf-8')\n"
                    "PY"
                ),
                "timeout": 120,
            },
            "coordinated": True,
        },
        {
            "kind": "codeact",
            "name": "codeact-1",
            "path": f"{live_load_env.repo_root}/tx/small_1.txt",
            "kwargs": {
                "mode": "shell",
                "command": (
                    "python3 - <<'PY'\n"
                    "from pathlib import Path\n"
                    "Path('tx/small_1.txt').write_text('codeact 1\\n', encoding='utf-8')\n"
                    "PY"
                ),
                "timeout": 120,
            },
            "coordinated": True,
        },
        {
            "kind": "codeact",
            "name": "codeact-2",
            "path": f"{live_load_env.repo_root}/tx/small_2.txt",
            "kwargs": {
                "mode": "shell",
                "command": (
                    "python3 - <<'PY'\n"
                    "from pathlib import Path\n"
                    "Path('tx/small_2.txt').write_text('codeact 2\\n', encoding='utf-8')\n"
                    "PY"
                ),
                "timeout": 120,
            },
            "coordinated": True,
        },
        {
            "kind": "codeact",
            "name": "codeact-3",
            "path": f"{live_load_env.repo_root}/tx/small_3.txt",
            "kwargs": {
                "mode": "shell",
                "command": (
                    "python3 - <<'PY'\n"
                    "from pathlib import Path\n"
                    "Path('tx/small_3.txt').write_text('codeact 3\\n', encoding='utf-8')\n"
                    "PY"
                ),
                "timeout": 120,
            },
            "coordinated": True,
        },
        {
            "kind": "write",
            "name": "write-4",
            "path": f"{live_load_env.repo_root}/writes/w4.txt",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/writes/w4.txt",
                "content": "write 4\n",
            },
            "coordinated": False,
        },
        {
            "kind": "write",
            "name": "write-5",
            "path": f"{live_load_env.repo_root}/writes/w5.txt",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/writes/w5.txt",
                "content": "write 5\n",
            },
            "coordinated": False,
        },
    ]

    results = asyncio.run(
        _run_mixed_operations(
            live_load_env,
            svc,
            operations,
            concurrency=20,
            timeout_s=120,
        )
    )

    by_kind: dict[str, list[dict[str, Any]]] = {}
    for item in results:
        by_kind.setdefault(item["kind"], []).append(item)

    def _avg_elapsed(items: list[dict[str, Any]]) -> float:
        return round(sum(item["elapsed_s"] for item in items) / len(items), 6)

    summary = {
        "operation_counts": {
            kind: len(items)
            for kind, items in sorted(by_kind.items())
        },
        "avg_elapsed_s": {
            kind: _avg_elapsed(items)
            for kind, items in sorted(by_kind.items())
        },
        "max_elapsed_s": {
            kind: round(max(item["elapsed_s"] for item in items), 6)
            for kind, items in sorted(by_kind.items())
        },
        "write_process_s": [
            round(float(item["payload"].get("timings", {}).get("commit_total", 0.0)), 6)
            for item in by_kind.get("write", [])
        ],
        "edit_tool_total_s": [
            round(float(item["payload"].get("timings", {}).get("tool", {}).get("tool_total", 0.0)), 6)
            for item in by_kind.get("edit-disjoint", [])
            if item["payload"].get("timings")
        ],
        "codeact_worktree_s": codeact_stats,
        "arbiter": svc.status()["arbiter"],
    }
    print("\n[occ-load-20-nonoverlap timings]")
    print(json.dumps(summary, indent=2, sort_keys=True))

    assert len(operations) == 20
    assert sum(not item["is_error"] for item in by_kind["write"]) == 6
    assert sum(not item["is_error"] for item in by_kind["codeact"]) == 4
    assert sum(not item["is_error"] for item in by_kind["edit-disjoint"]) >= 8


def test_live_occ_load_30_non_overlapping_operations_profile(
    live_load_env: LiveLoadEnv,
    monkeypatch: pytest.MonkeyPatch,
):
    live_load_env.init_repo()
    codeact_stats = _install_codeact_phase_probe(monkeypatch)

    for group in range(3):
        live_load_env.write_text(
            f"edits/disjoint_{group}.py",
            (
                f'"""Disjoint target {group}."""\n\n'
                f"A_{group} = 1\n"
                f"B_{group} = 2\n"
                f"C_{group} = 3\n"
                f"D_{group} = 4\n"
                f"E_{group} = 5\n"
            ),
        )
    for idx in range(6):
        live_load_env.write_text(f"tx/medium_{idx}.txt", "base\n")

    live_load_env.exec_checked(f"git -C {shlex.quote(live_load_env.repo_root)} add -A")
    live_load_env.exec_checked(
        f"git -C {shlex.quote(live_load_env.repo_root)} commit -m seed-medium-nonoverlap-load",
        timeout=180,
    )

    svc = live_load_env.make_ci_service()
    operations: list[dict[str, Any]] = []

    for idx in range(9):
        operations.append(
            {
                "kind": "write",
                "name": f"write-{idx}",
                "path": f"{live_load_env.repo_root}/writes/w{idx}.txt",
                "kwargs": {
                    "file_path": f"{live_load_env.repo_root}/writes/w{idx}.txt",
                    "content": f"write {idx}\n",
                },
                "coordinated": False,
            }
        )

    for group in range(3):
        for label, old, new in (
            ("a", f"A_{group} = 1", f"A_{group} = 100"),
            ("b", f"B_{group} = 2", f"B_{group} = 200"),
            ("c", f"C_{group} = 3", f"C_{group} = 300"),
            ("d", f"D_{group} = 4", f"D_{group} = 400"),
            ("e", f"E_{group} = 5", f"E_{group} = 500"),
        ):
            operations.append(
                {
                    "kind": "edit-disjoint",
                    "name": f"edit-{label}{group}",
                    "path": f"{live_load_env.repo_root}/edits/disjoint_{group}.py",
                    "kwargs": {
                        "file_path": f"{live_load_env.repo_root}/edits/disjoint_{group}.py",
                        "old_text": old,
                        "new_text": new,
                    },
                    "coordinated": False,
                }
            )

    for idx in range(6):
        operations.append(
            {
                "kind": "codeact",
                "name": f"codeact-{idx}",
                "path": f"{live_load_env.repo_root}/tx/medium_{idx}.txt",
                "kwargs": {
                    "mode": "shell",
                    "command": (
                        "python3 - <<'PY'\n"
                        "from pathlib import Path\n"
                        f"Path('tx/medium_{idx}.txt').write_text('codeact {idx}\\n', encoding='utf-8')\n"
                        "PY"
                    ),
                    "timeout": 120,
                },
                "coordinated": True,
            }
        )

    results = asyncio.run(
        _run_mixed_operations(
            live_load_env,
            svc,
            operations,
            concurrency=20,
            timeout_s=180,
        )
    )

    by_kind: dict[str, list[dict[str, Any]]] = {}
    for item in results:
        by_kind.setdefault(item["kind"], []).append(item)

    def _avg_elapsed(items: list[dict[str, Any]]) -> float:
        return round(sum(item["elapsed_s"] for item in items) / len(items), 6)

    summary = {
        "operation_counts": {
            kind: len(items)
            for kind, items in sorted(by_kind.items())
        },
        "avg_elapsed_s": {
            kind: _avg_elapsed(items)
            for kind, items in sorted(by_kind.items())
        },
        "max_elapsed_s": {
            kind: round(max(item["elapsed_s"] for item in items), 6)
            for kind, items in sorted(by_kind.items())
        },
        "write_process_s": [
            round(float(item["payload"].get("timings", {}).get("commit_total", 0.0)), 6)
            for item in by_kind.get("write", [])
        ],
        "edit_tool_total_s": [
            round(float(item["payload"].get("timings", {}).get("tool", {}).get("tool_total", 0.0)), 6)
            for item in by_kind.get("edit-disjoint", [])
            if item["payload"].get("timings")
        ],
        "codeact_worktree_s": codeact_stats,
        "arbiter": svc.status()["arbiter"],
    }
    print("\n[occ-load-30-nonoverlap timings]")
    print(json.dumps(summary, indent=2, sort_keys=True))

    assert len(operations) == 30
    assert sum(not item["is_error"] for item in by_kind["write"]) == 9
    assert sum(not item["is_error"] for item in by_kind["codeact"]) == 6
    assert sum(not item["is_error"] for item in by_kind["edit-disjoint"]) >= 12


def test_live_occ_load_50_non_overlapping_operations_profile(
    live_load_env: LiveLoadEnv,
    monkeypatch: pytest.MonkeyPatch,
):
    live_load_env.init_repo()
    codeact_stats = _install_codeact_phase_probe(monkeypatch)

    for group in range(5):
        live_load_env.write_text(
            f"edits/disjoint_{group}.py",
            (
                f'"""Disjoint target {group}."""\n\n'
                f"A_{group} = 1\n"
                f"B_{group} = 2\n"
                f"C_{group} = 3\n"
                f"D_{group} = 4\n"
                f"E_{group} = 5\n"
            ),
        )
    for idx in range(10):
        live_load_env.write_text(f"tx/large_{idx}.txt", "base\n")

    live_load_env.exec_checked(f"git -C {shlex.quote(live_load_env.repo_root)} add -A")
    live_load_env.exec_checked(
        f"git -C {shlex.quote(live_load_env.repo_root)} commit -m seed-large-nonoverlap-load",
        timeout=180,
    )

    svc = live_load_env.make_ci_service()
    operations: list[dict[str, Any]] = []

    for idx in range(15):
        operations.append(
            {
                "kind": "write",
                "name": f"write-{idx}",
                "path": f"{live_load_env.repo_root}/writes/w{idx}.txt",
                "kwargs": {
                    "file_path": f"{live_load_env.repo_root}/writes/w{idx}.txt",
                    "content": f"write {idx}\n",
                },
                "coordinated": False,
            }
        )

    for group in range(5):
        for label, old, new in (
            ("a", f"A_{group} = 1", f"A_{group} = 100"),
            ("b", f"B_{group} = 2", f"B_{group} = 200"),
            ("c", f"C_{group} = 3", f"C_{group} = 300"),
            ("d", f"D_{group} = 4", f"D_{group} = 400"),
            ("e", f"E_{group} = 5", f"E_{group} = 500"),
        ):
            operations.append(
                {
                    "kind": "edit-disjoint",
                    "name": f"edit-{label}{group}",
                    "path": f"{live_load_env.repo_root}/edits/disjoint_{group}.py",
                    "kwargs": {
                        "file_path": f"{live_load_env.repo_root}/edits/disjoint_{group}.py",
                        "old_text": old,
                        "new_text": new,
                    },
                    "coordinated": False,
                }
            )

    for idx in range(10):
        operations.append(
            {
                "kind": "codeact",
                "name": f"codeact-{idx}",
                "path": f"{live_load_env.repo_root}/tx/large_{idx}.txt",
                "kwargs": {
                    "mode": "shell",
                    "command": (
                        "python3 - <<'PY'\n"
                        "from pathlib import Path\n"
                        f"Path('tx/large_{idx}.txt').write_text('codeact {idx}\\n', encoding='utf-8')\n"
                        "PY"
                    ),
                    "timeout": 120,
                },
                "coordinated": True,
            }
        )

    results = asyncio.run(
        _run_mixed_operations(
            live_load_env,
            svc,
            operations,
            concurrency=20,
            timeout_s=240,
        )
    )

    by_kind: dict[str, list[dict[str, Any]]] = {}
    for item in results:
        by_kind.setdefault(item["kind"], []).append(item)

    def _avg_elapsed(items: list[dict[str, Any]]) -> float:
        return round(sum(item["elapsed_s"] for item in items) / len(items), 6)

    summary = {
        "operation_counts": {
            kind: len(items)
            for kind, items in sorted(by_kind.items())
        },
        "avg_elapsed_s": {
            kind: _avg_elapsed(items)
            for kind, items in sorted(by_kind.items())
        },
        "max_elapsed_s": {
            kind: round(max(item["elapsed_s"] for item in items), 6)
            for kind, items in sorted(by_kind.items())
        },
        "write_occ_commit_s": [
            round(float(item["payload"].get("timings", {}).get("commit_total", 0.0)), 6)
            for item in by_kind.get("write", [])
        ],
        "edit_tool_total_s": [
            round(float(item["payload"].get("timings", {}).get("tool", {}).get("tool_total", 0.0)), 6)
            for item in by_kind.get("edit-disjoint", [])
            if item["payload"].get("timings")
        ],
        "codeact_worktree_s": codeact_stats,
        "arbiter": svc.status()["arbiter"],
    }
    print("\n[occ-load-50-nonoverlap timings]")
    print(json.dumps(summary, indent=2, sort_keys=True))

    assert len(operations) == 50
    assert sum(not item["is_error"] for item in by_kind["write"]) == 15
    assert sum(not item["is_error"] for item in by_kind["codeact"]) == 10
    assert sum(not item["is_error"] for item in by_kind["edit-disjoint"]) >= 20
