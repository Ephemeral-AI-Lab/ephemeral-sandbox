"""Live E2E coverage for direct Daytona toolkit OCC tool calls.

This suite exercises the actual tool implementations, not just CI service
helpers:
  1. `daytona_write_file` seeds live files in a real Daytona sandbox.
  2. `daytona_edit_file` performs concurrent same-file search/replace edits.
  3. `daytona_codeact` verifies final on-disk state via real sandbox commands.

Run with:
    .venv/bin/python -m pytest backend/tests/test_e2e/test_live_daytona_tool_occ_calls.py -m live -v -s
"""

from __future__ import annotations

import asyncio
import concurrent.futures
import json
import os
import re
import shlex
import threading
import uuid
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from dotenv import load_dotenv

from code_intelligence.routing.service import CodeIntelligenceService
from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit._daytona_utils import _extract_exit_code, _wrap_bash_command
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
class LiveToolEnv:
    sandbox_id: str
    raw_sandbox: Any
    async_sandbox: Any
    home: str
    root_dir: str

    def exec(self, command: str, *, timeout: int = 180) -> tuple[int, str]:
        response = self.raw_sandbox.process.exec(_wrap_bash_command(command), timeout=timeout)
        raw = _TERM_NOISE.sub("", getattr(response, "result", "") or "")
        cleaned, exit_code = _extract_exit_code(
            raw,
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        return exit_code, cleaned

    def exec_checked(self, command: str, *, timeout: int = 180) -> str:
        exit_code, stdout = self.exec(command, timeout=timeout)
        if exit_code != 0:
            detail = stdout.strip() or f"exit {exit_code}"
            raise AssertionError(f"Sandbox command failed: {detail}")
        return stdout

    def require_command(self, name: str) -> None:
        exit_code, _ = self.exec(f"command -v {shlex.quote(name)} >/dev/null 2>&1", timeout=30)
        if exit_code != 0:
            pytest.skip(f"Sandbox image missing required command: {name}")

    def make_ci_service(self) -> CodeIntelligenceService:
        return CodeIntelligenceService(
            sandbox_id=self.sandbox_id,
            workspace_root=self.home,
            sandbox=self.raw_sandbox,
        )

    def make_ctx(
        self,
        ci_service: CodeIntelligenceService,
        *,
        agent_run_id: str,
    ) -> ToolExecutionContext:
        return ToolExecutionContext(
            cwd=Path(self.home),
            metadata={
                "daytona_sandbox": self.async_sandbox,
                "daytona_cwd": self.home,
                "ci_service": ci_service,
                "agent_run_id": agent_run_id,
            },
        )


@pytest.fixture
def live_tool_env():
    if not HAS_DAYTONA:
        pytest.skip("Daytona credentials not configured")

    from sandbox.testing import create_test_sandbox, delete_test_sandbox, get_sandbox_service

    info = create_test_sandbox(name="tool-occ-live")
    sandbox_id = info["id"]
    try:
        sandbox_svc = get_sandbox_service()
        raw_sandbox = sandbox_svc.get_sandbox_object(sandbox_id)
        home_resp = raw_sandbox.process.exec("pwd", timeout=10)
        home = (getattr(home_resp, "result", "") or "").strip() or "/home/daytona"
        env = LiveToolEnv(
            sandbox_id=sandbox_id,
            raw_sandbox=raw_sandbox,
            async_sandbox=_AsyncSandboxWrapper(raw_sandbox),
            home=home,
            root_dir=f"{home}/tool_occ_live",
        )
        env.require_command("cat")
        env.exec_checked(f"mkdir -p {shlex.quote(env.root_dir)}")
        yield env
    finally:
        delete_test_sandbox(sandbox_id)


def _json_output(result) -> dict[str, Any]:
    assert not result.is_error, result.output
    return json.loads(result.output)


def test_live_tool_roundtrip_write_edit_codeact(live_tool_env: LiveToolEnv):
    svc = live_tool_env.make_ci_service()
    file_path = f"{live_tool_env.root_dir}/roundtrip_{uuid.uuid4().hex[:8]}.py"

    write_ctx = live_tool_env.make_ctx(svc, agent_run_id=f"write-{uuid.uuid4().hex[:8]}")
    write_result = asyncio.run(
        daytona_write_file.execute(
            daytona_write_file.input_model(
                file_path=file_path,
                content="VALUE = 'base'\n",
            ),
            write_ctx,
        )
    )
    write_payload = _json_output(write_result)
    assert write_payload["file_path"] == file_path

    edit_ctx = live_tool_env.make_ctx(svc, agent_run_id=f"edit-{uuid.uuid4().hex[:8]}")
    edit_result = asyncio.run(
        daytona_edit_file.execute(
            daytona_edit_file.input_model(
                file_path=file_path,
                old_text="VALUE = 'base'",
                new_text="VALUE = 'edited'",
            ),
            edit_ctx,
        )
    )
    edit_payload = _json_output(edit_result)
    assert edit_payload["occ"] is True

    codeact_ctx = live_tool_env.make_ctx(svc, agent_run_id=f"verify-{uuid.uuid4().hex[:8]}")
    codeact_result = asyncio.run(
        daytona_codeact.execute(
            daytona_codeact.input_model(command=f"cat {shlex.quote(file_path)}"),
            codeact_ctx,
        )
    )
    codeact_payload = _json_output(codeact_result)
    assert codeact_payload["status"] == "ok"
    stdout = codeact_payload["shell_outputs"][0]["stdout"]
    assert "VALUE = 'edited'" in stdout


def test_live_two_concurrent_same_file_overlap_has_single_winner(
    live_tool_env: LiveToolEnv,
    monkeypatch,
):
    import tools.daytona_toolkit.edit_tool as edit_tool_module

    svc = live_tool_env.make_ci_service()
    file_path = f"{live_tool_env.root_dir}/two_conflict_{uuid.uuid4().hex[:8]}.py"
    original = "def shared_conflict():\n    return 'base'\n"

    write_ctx = live_tool_env.make_ctx(svc, agent_run_id=f"seed-{uuid.uuid4().hex[:8]}")
    seed_result = asyncio.run(
        daytona_write_file.execute(
            daytona_write_file.input_model(file_path=file_path, content=original),
            write_ctx,
        )
    )
    _json_output(seed_result)

    edits = [
        ("conflict-a", "return 'base'", "return 'A_WON'"),
        ("conflict-b", "return 'base'", "return 'B_WON'"),
    ]
    barrier = threading.Barrier(len(edits), timeout=20)
    original_prepare_ci_write = edit_tool_module.prepare_ci_write

    def _prepare_ci_write_barrier(*args, **kwargs):
        prepared, scope_packet, err = original_prepare_ci_write(*args, **kwargs)
        if prepared is not None and err is None:
            try:
                barrier.wait(timeout=20)
            except threading.BrokenBarrierError as exc:  # pragma: no cover - defensive
                raise AssertionError(
                    "Two-writer live overlap barrier broke before both writers prepared"
                ) from exc
        return prepared, scope_packet, err

    monkeypatch.setattr(edit_tool_module, "prepare_ci_write", _prepare_ci_write_barrier)

    def _worker(agent_id: str, search: str, replace: str):
        ctx = live_tool_env.make_ctx(svc, agent_run_id=agent_id)
        return asyncio.run(
            daytona_edit_file.execute(
                daytona_edit_file.input_model(
                    file_path=file_path,
                    edits=[
                        {
                            "strategy": "search_replace",
                            "search": search,
                            "replace": replace,
                        }
                    ],
                    description=f"{agent_id}: replace {search!r}",
                ),
                ctx,
            )
        )

    with concurrent.futures.ThreadPoolExecutor(max_workers=len(edits)) as pool:
        futures = {
            agent_id: pool.submit(_worker, agent_id, search, replace)
            for agent_id, search, replace in edits
        }
        results = {agent_id: future.result(timeout=90) for agent_id, future in futures.items()}

    successes: dict[str, dict[str, Any]] = {}
    conflicts: dict[str, str] = {}
    unexpected_errors: dict[str, str] = {}

    for agent_id, result in results.items():
        if not result.is_error:
            payload = json.loads(result.output)
            assert payload["occ"] is True, f"{agent_id} should use OCC"
            successes[agent_id] = payload
            continue
        if result.metadata.get("conflict") is True:
            conflicts[agent_id] = result.output
            continue
        unexpected_errors[agent_id] = result.output

    assert not unexpected_errors, f"Unexpected non-conflict tool failures: {unexpected_errors}"
    assert len(successes) == 1, f"Expected exactly one overlapping success, got {results}"
    assert len(conflicts) == 1, f"Expected exactly one overlapping conflict, got {results}"

    codeact_ctx = live_tool_env.make_ctx(svc, agent_run_id=f"verify-{uuid.uuid4().hex[:8]}")
    verify_result = asyncio.run(
        daytona_codeact.execute(
            daytona_codeact.input_model(command=f"cat {shlex.quote(file_path)}"),
            codeact_ctx,
        )
    )
    verify_payload = _json_output(verify_result)
    final = verify_payload["shell_outputs"][0]["stdout"]

    landed_overlap_values = [token for token in ("A_WON", "B_WON") if token in final]
    assert len(landed_overlap_values) == 1, (
        f"Expected exactly one overlap winner in the file. File:\n{final}"
    )
    assert "return 'base'" not in final, (
        f"One conflicting edit should have committed, not leave base content intact. File:\n{final}"
    )


def test_live_five_concurrent_same_file_edit_tool_calls(live_tool_env: LiveToolEnv, monkeypatch):
    import tools.daytona_toolkit.edit_tool as edit_tool_module

    svc = live_tool_env.make_ci_service()
    file_path = f"{live_tool_env.root_dir}/concurrent_{uuid.uuid4().hex[:8]}.py"
    original = (
        "\n\n".join(
            f"def unique_{i}():\n    return {i}\n" for i in range(3)
        )
        + "\n\n"
        + "def shared_conflict():\n    return 'base'\n"
    )

    write_ctx = live_tool_env.make_ctx(svc, agent_run_id=f"seed-{uuid.uuid4().hex[:8]}")
    seed_result = asyncio.run(
        daytona_write_file.execute(
            daytona_write_file.input_model(file_path=file_path, content=original),
            write_ctx,
        )
    )
    _json_output(seed_result)

    edits = [
        (f"unique-{i}", f"return {i}", f"return {i + 1000}")
        for i in range(3)
    ] + [
        ("conflict-a", "return 'base'", "return 'A_WON'"),
        ("conflict-b", "return 'base'", "return 'B_WON'"),
    ]

    barrier = threading.Barrier(len(edits), timeout=20)
    original_prepare_ci_write = edit_tool_module.prepare_ci_write

    def _prepare_ci_write_barrier(*args, **kwargs):
        prepared, scope_packet, err = original_prepare_ci_write(*args, **kwargs)
        if prepared is not None and err is None:
            try:
                barrier.wait(timeout=20)
            except threading.BrokenBarrierError as exc:  # pragma: no cover - defensive
                raise AssertionError("Live concurrent edit barrier broke before all writers prepared") from exc
        return prepared, scope_packet, err

    monkeypatch.setattr(edit_tool_module, "prepare_ci_write", _prepare_ci_write_barrier)

    def _worker(agent_id: str, search: str, replace: str):
        ctx = live_tool_env.make_ctx(svc, agent_run_id=agent_id)
        return asyncio.run(
            daytona_edit_file.execute(
                daytona_edit_file.input_model(
                    file_path=file_path,
                    edits=[
                        {
                            "strategy": "search_replace",
                            "search": search,
                            "replace": replace,
                        }
                    ],
                    description=f"{agent_id}: replace {search!r}",
                ),
                ctx,
            )
        )

    with concurrent.futures.ThreadPoolExecutor(max_workers=len(edits)) as pool:
        futures = {
            agent_id: pool.submit(_worker, agent_id, search, replace)
            for agent_id, search, replace in edits
        }
        results = {agent_id: future.result(timeout=90) for agent_id, future in futures.items()}

    unique_results = {agent_id: results[agent_id] for agent_id, _, _ in edits[:3]}
    overlap_results = {agent_id: results[agent_id] for agent_id, _, _ in edits[3:]}

    success_payloads: dict[str, dict[str, Any]] = {}
    conflict_outputs: dict[str, str] = {}
    unexpected_errors: dict[str, str] = {}

    for agent_id, result in results.items():
        if not result.is_error:
            payload = json.loads(result.output)
            assert payload["occ"] is True, f"{agent_id} should use OCC"
            success_payloads[agent_id] = payload
            continue
        if result.metadata.get("conflict") is True:
            conflict_outputs[agent_id] = result.output
            continue
        unexpected_errors[agent_id] = result.output

    assert not unexpected_errors, f"Unexpected non-conflict tool failures: {unexpected_errors}"

    unique_successes = [agent_id for agent_id in unique_results if agent_id in success_payloads]
    unique_conflicts = [agent_id for agent_id in unique_results if agent_id in conflict_outputs]
    overlap_successes = [agent_id for agent_id in overlap_results if agent_id in success_payloads]
    overlap_conflicts = [agent_id for agent_id in overlap_results if agent_id in conflict_outputs]

    assert len(unique_successes) >= 2, (
        f"Expected at least two disjoint edits to land, got successes={unique_successes}, "
        f"conflicts={unique_conflicts}, outputs={conflict_outputs}"
    )
    assert len(overlap_successes) <= 1, (
        f"Overlapping edits must not both land. successes={overlap_successes}, outputs={conflict_outputs}"
    )
    assert len(overlap_conflicts) >= 1, (
        f"Expected at least one explicit conflict on the shared target. "
        f"successes={overlap_successes}, outputs={conflict_outputs}"
    )

    codeact_ctx = live_tool_env.make_ctx(svc, agent_run_id=f"verify-{uuid.uuid4().hex[:8]}")
    verify_result = asyncio.run(
        daytona_codeact.execute(
            daytona_codeact.input_model(
                command=(
                    f"python3 -m py_compile {shlex.quote(file_path)} && "
                    f"cat {shlex.quote(file_path)}"
                )
            ),
            codeact_ctx,
        )
    )
    verify_payload = _json_output(verify_result)
    final = verify_payload["shell_outputs"][0]["stdout"]

    landed_unique_values = [f"return {i + 1000}" for i in range(3) if f"return {i + 1000}" in final]
    assert len(landed_unique_values) >= 2, (
        f"Expected at least two disjoint live edits to persist. File:\n{final}"
    )

    landed_overlap_values = [token for token in ("A_WON", "B_WON") if token in final]
    assert len(landed_overlap_values) <= 1, (
        f"Overlapping edit-tool replacements must not merge. File:\n{final}"
    )
    assert "return 'base'" in final or landed_overlap_values, (
        f"Shared conflict line should either stay at base or reflect one winner. File:\n{final}"
    )
