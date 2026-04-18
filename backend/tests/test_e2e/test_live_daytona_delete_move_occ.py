"""Live OCC integrity tests for daytona_delete_file and daytona_move_file.

Spawns a single Daytona sandbox, a single CI service, and drives the two new
tools under concurrency to verify the OCC invariants:

- Concurrent deletes of the same file: exactly one winner, others abort with
  ``aborted_version`` or ``not_found``; no ledger partial state.
- Concurrent deletes on disjoint files: all succeed.
- Concurrent moves with the same src to different dsts: exactly one winner; src
  is gone; exactly one dst exists; others abort.
- Concurrent disjoint moves: all succeed.
- ``overwrite=True`` with drift on the dst aborts via ``strict_base`` and
  leaves src untouched.

Mirrors the infrastructure in ``test_live_daytona_occ_load.py``.
"""

from __future__ import annotations

import asyncio
import json
import os
import re
import shlex
import uuid
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from dotenv import load_dotenv

from code_intelligence.routing.service import (
    CodeIntelligenceService,
    dispose_all_code_intelligence,
)
from tests.test_e2e.daytona_exec_io import read_text_via_exec, write_text_via_exec
from tools.core.base import ToolExecutionContext, ToolResult
from tools.daytona_toolkit._daytona_utils import (
    _extract_exit_code,
    _wrap_bash_command,
)
from tools.daytona_toolkit.delete_move_tool import (
    daytona_delete_file,
    daytona_move_file,
)

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
        return await asyncio.to_thread(self._real.upload_file, *args, **kwargs)

    async def download_file(self, *args, **kwargs):
        return await asyncio.to_thread(self._real.download_file, *args, **kwargs)

    def __getattr__(self, name: str) -> Any:
        return getattr(self._real, name)


class _AsyncProcess:
    def __init__(self, real_process: Any):
        self._real = real_process

    async def exec(self, *args, **kwargs):
        response = await asyncio.to_thread(self._real.exec, *args, **kwargs)
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
class LiveEnv:
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

    def path_exists(self, rel_path: str) -> bool:
        full = f"{self.repo_root}/{rel_path}"
        code, _ = self.exec(f"test -e {shlex.quote(full)}", timeout=15)
        return code == 0

    def init_repo(self) -> None:
        self.exec_checked(f"rm -rf {shlex.quote(self.repo_root)} && mkdir -p {shlex.quote(self.repo_root)}")
        self.exec_checked(f"git -C {shlex.quote(self.repo_root)} init")
        self.exec_checked(f"git -C {shlex.quote(self.repo_root)} config user.email test@example.com")
        self.exec_checked(f"git -C {shlex.quote(self.repo_root)} config user.name 'Test User'")

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
    ) -> ToolExecutionContext:
        metadata: dict[str, Any] = {
            "daytona_sandbox": self.async_sandbox,
            "daytona_cwd": self.repo_root,
            "repo_root": self.repo_root,
            "exec_cwd": self.repo_root,
            "ci_service": ci_service,
            "agent_run_id": agent_run_id,
        }
        return ToolExecutionContext(cwd=Path(self.repo_root), metadata=metadata)


@pytest.fixture(autouse=True)
def _clear_ci_registry():
    """Keep the CI service registry from leaking between tests on a shared sandbox."""
    dispose_all_code_intelligence()
    yield
    dispose_all_code_intelligence()


@pytest.fixture(scope="module")
def live_env():
    """One sandbox shared by every test in this module — tests reset repo in init_repo()."""
    if not HAS_DAYTONA:
        pytest.skip("Daytona credentials not configured")

    from sandbox.testing import create_test_sandbox, delete_test_sandbox, get_sandbox_service

    info = create_test_sandbox(name="delete-move-occ-live")
    sandbox_id = info["id"]
    try:
        sandbox_svc = get_sandbox_service()
        raw_sandbox = sandbox_svc.get_sandbox_object(sandbox_id)
        home_resp = raw_sandbox.process.exec("pwd", timeout=10)
        home = (getattr(home_resp, "result", "") or "").strip() or "/home/daytona"
        env = LiveEnv(
            sandbox_id=sandbox_id,
            raw_sandbox=raw_sandbox,
            async_sandbox=_AsyncSandboxWrapper(raw_sandbox),
            home=home,
            repo_root=f"{home}/delete_move_occ_repo",
        )
        env.require_command("git")
        env.require_command("python3")
        yield env
    finally:
        delete_test_sandbox(sandbox_id)


def _json_output(result: ToolResult) -> dict[str, Any]:
    assert result.output, "tool returned empty output"
    return json.loads(result.output)


async def _invoke(tool: Any, kwargs: dict[str, Any], ctx: ToolExecutionContext) -> ToolResult:
    return await tool.execute(tool.input_model(**kwargs), ctx)


async def _run_many(
    env: LiveEnv,
    svc: CodeIntelligenceService,
    tool: Any,
    call_kwargs: list[dict[str, Any]],
    *,
    concurrency: int,
    timeout_s: int,
) -> list[dict[str, Any]]:
    semaphore = asyncio.Semaphore(concurrency)

    async def _one(idx: int, kwargs: dict[str, Any]) -> dict[str, Any]:
        ctx = env.make_ctx(svc, agent_run_id=f"agent-{idx}-{uuid.uuid4().hex[:8]}")
        async with semaphore:
            result = await _invoke(tool, kwargs, ctx)
        output = (result.output or "").lstrip()
        payload = _json_output(result) if output.startswith("{") else {}
        return {
            "kwargs": kwargs,
            "is_error": result.is_error,
            "payload": payload,
            "metadata": dict(result.metadata or {}),
        }

    return await asyncio.wait_for(
        asyncio.gather(*[_one(i, k) for i, k in enumerate(call_kwargs)]),
        timeout=timeout_s,
    )


# ---------------------------------------------------------------------------
# daytona_delete_file
# ---------------------------------------------------------------------------


def test_live_concurrent_delete_same_file_exactly_one_winner(live_env: LiveEnv):
    env = live_env
    env.init_repo()
    env.write_text("contended/target.txt", "target\n")

    svc = env.make_ci_service()
    target = f"{env.repo_root}/contended/target.txt"
    call_kwargs = [{"file_path": target} for _ in range(12)]

    results = asyncio.run(
        _run_many(env, svc, daytona_delete_file, call_kwargs, concurrency=12, timeout_s=180)
    )

    successes = [r for r in results if not r["is_error"] and r["payload"].get("status") == "deleted"]
    aborts = [
        r for r in results
        if r["is_error"]
        and r["payload"].get("status") in {"aborted_version", "not_found", "aborted_lock"}
    ]
    other = [r for r in results if r not in successes and r not in aborts]

    print("\n[delete-contention summary]", json.dumps({
        "successes": len(successes),
        "aborts": len(aborts),
        "other": len(other),
        "statuses": sorted({r["payload"].get("status") for r in results}),
    }, indent=2))

    assert len(successes) == 1, f"expected exactly one winner, got {len(successes)}"
    assert len(other) == 0, f"unexpected statuses: {other}"
    assert not env.path_exists("contended/target.txt")


def test_live_concurrent_delete_disjoint_all_succeed(live_env: LiveEnv):
    env = live_env
    env.init_repo()

    N = 20
    for i in range(N):
        env.write_text(f"disjoint/del_{i}.txt", f"body-{i}\n")

    svc = env.make_ci_service()
    call_kwargs = [
        {"file_path": f"{env.repo_root}/disjoint/del_{i}.txt"} for i in range(N)
    ]
    results = asyncio.run(
        _run_many(env, svc, daytona_delete_file, call_kwargs, concurrency=20, timeout_s=180)
    )

    successes = sum(1 for r in results if not r["is_error"] and r["payload"].get("status") == "deleted")
    assert successes == N, f"expected {N} deletes to succeed, got {successes}"
    for i in range(N):
        assert not env.path_exists(f"disjoint/del_{i}.txt"), f"file {i} still present"


# ---------------------------------------------------------------------------
# daytona_move_file
# ---------------------------------------------------------------------------


def test_live_concurrent_move_same_src_exactly_one_winner(live_env: LiveEnv):
    env = live_env
    env.init_repo()
    env.write_text("src/shared.txt", "payload\n")

    svc = env.make_ci_service()
    src = f"{env.repo_root}/src/shared.txt"
    call_kwargs = [
        {"src_path": src, "dst_path": f"{env.repo_root}/dst/out_{i}.txt"}
        for i in range(10)
    ]
    results = asyncio.run(
        _run_many(env, svc, daytona_move_file, call_kwargs, concurrency=10, timeout_s=180)
    )

    successes = [r for r in results if not r["is_error"] and r["payload"].get("status") == "moved"]
    aborts = [
        r for r in results
        if r["is_error"]
        and r["payload"].get("status") in {"aborted_version", "not_found", "aborted_lock"}
    ]
    other = [r for r in results if r not in successes and r not in aborts]

    print("\n[move-contention summary]", json.dumps({
        "successes": len(successes),
        "aborts": len(aborts),
        "other": len(other),
        "statuses": sorted({r["payload"].get("status") for r in results}),
    }, indent=2))

    assert len(successes) == 1, f"expected exactly one winner, got {len(successes)}"
    assert len(other) == 0, f"unexpected statuses: {other}"

    # src should be gone; exactly one dst should exist.
    assert not env.path_exists("src/shared.txt")
    extant = [i for i in range(10) if env.path_exists(f"dst/out_{i}.txt")]
    assert len(extant) == 1, f"expected exactly one dst to exist, got {extant}"


def test_live_concurrent_move_disjoint_all_succeed(live_env: LiveEnv):
    env = live_env
    env.init_repo()

    N = 15
    for i in range(N):
        env.write_text(f"mv/src_{i}.txt", f"body-{i}\n")

    svc = env.make_ci_service()
    call_kwargs = [
        {
            "src_path": f"{env.repo_root}/mv/src_{i}.txt",
            "dst_path": f"{env.repo_root}/mv/dst_{i}.txt",
        }
        for i in range(N)
    ]
    results = asyncio.run(
        _run_many(env, svc, daytona_move_file, call_kwargs, concurrency=15, timeout_s=180)
    )

    successes = sum(
        1 for r in results if not r["is_error"] and r["payload"].get("status") == "moved"
    )
    assert successes == N, f"expected {N} moves to succeed, got {successes}"

    for i in range(N):
        assert not env.path_exists(f"mv/src_{i}.txt"), f"src {i} still present"
        assert env.path_exists(f"mv/dst_{i}.txt"), f"dst {i} missing"
        assert env.read_text(f"mv/dst_{i}.txt") == f"body-{i}\n"


def test_live_move_overwrite_strict_base_aborts_on_dst_drift(live_env: LiveEnv):
    """Move with overwrite=True must abort when dst drifts concurrently."""
    env = live_env
    env.init_repo()
    env.write_text("drift/src.txt", "src-content\n")
    env.write_text("drift/dst.txt", "dst-original\n")

    svc = env.make_ci_service()

    # Drive a drift on dst while move_file's base-capture pass is mid-flight.
    # The tool reads src then dst; we corrupt dst between reads.
    import code_intelligence.routing.content_manager as content_mod

    original_read = svc._content.read
    calls: list[str] = []

    def _drifting_read(file_path: str, *, allow_missing: bool = False):
        calls.append(file_path)
        result = original_read(file_path, allow_missing=allow_missing)
        # After the tool reads dst (the 2nd read in move_file), drift it before the
        # coordinator's current-state read.
        if file_path.endswith("drift/dst.txt"):
            env.write_text("drift/dst.txt", "drifted!\n")
        return result

    svc._content.read = _drifting_read  # type: ignore[assignment]
    try:
        ctx = env.make_ctx(svc, agent_run_id="overwrite-drift")
        result = asyncio.run(
            _invoke(
                daytona_move_file,
                {
                    "src_path": f"{env.repo_root}/drift/src.txt",
                    "dst_path": f"{env.repo_root}/drift/dst.txt",
                    "overwrite": True,
                },
                ctx,
            ),
        )
    finally:
        svc._content.read = original_read  # type: ignore[assignment]
    del content_mod

    payload = json.loads(result.output)
    assert result.is_error is True
    assert payload["status"] == "aborted_version", payload
    # Src preserved; dst kept the drifted content; no partial move.
    assert env.path_exists("drift/src.txt")
    assert env.read_text("drift/src.txt") == "src-content\n"
    assert env.read_text("drift/dst.txt") == "drifted!\n"


# ---------------------------------------------------------------------------
# Cross-tool contention (delete vs move) — optional sanity
# ---------------------------------------------------------------------------


def test_live_delete_and_move_race_on_same_source(live_env: LiveEnv):
    """One agent deletes the file while another moves it.

    Both tools read the same base, then race into the OCC commit. Expected:
    exactly one winner, and the repo ends in a coherent state (either gone,
    or moved — never both, never zombie partial state).
    """
    env = live_env
    env.init_repo()
    env.write_text("race/payload.txt", "shared\n")

    svc = env.make_ci_service()

    async def _drive() -> tuple[dict[str, Any], dict[str, Any]]:
        ctx1 = env.make_ctx(svc, agent_run_id="delete-race")
        ctx2 = env.make_ctx(svc, agent_run_id="move-race")
        delete_task = _invoke(
            daytona_delete_file,
            {"file_path": f"{env.repo_root}/race/payload.txt"},
            ctx1,
        )
        move_task = _invoke(
            daytona_move_file,
            {
                "src_path": f"{env.repo_root}/race/payload.txt",
                "dst_path": f"{env.repo_root}/race/moved.txt",
            },
            ctx2,
        )
        del_r, mv_r = await asyncio.gather(delete_task, move_task)
        return _json_output(del_r), _json_output(mv_r)

    del_payload, mv_payload = asyncio.run(_drive())
    delete_won = del_payload.get("status") == "deleted"
    move_won = mv_payload.get("status") == "moved"

    print("\n[delete-vs-move race]", json.dumps({
        "delete_status": del_payload.get("status"),
        "move_status": mv_payload.get("status"),
    }, indent=2))

    assert delete_won ^ move_won, (
        f"expected exactly one winner, delete={delete_won} move={move_won} "
        f"del={del_payload} mv={mv_payload}"
    )
    if delete_won:
        assert not env.path_exists("race/payload.txt")
        assert not env.path_exists("race/moved.txt")
    else:
        assert not env.path_exists("race/payload.txt")
        assert env.path_exists("race/moved.txt")
        assert env.read_text("race/moved.txt") == "shared\n"
