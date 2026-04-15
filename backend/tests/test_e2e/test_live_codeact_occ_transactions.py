"""Live E2E coverage for coordinated CodeAct transactions and OCC behavior.

Scenarios covered:
  1. Coordinated Python mode seeds from dirty workspace state, exposes native
     writes to later shell/read steps, and commits create/modify/delete diffs.
  2. Coordinated shell mode commits multi-file repo mutations transactionally.
  3. Coordinated shell mode rolls back scratch mutations when the command fails.
  4. Two same-base transactions editing different regions merge through OCC.
  5. Two same-base transactions editing the same region conflict through OCC.

Run with:
    uv run pytest backend/tests/test_e2e/test_live_codeact_occ_transactions.py -v -s
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

from code_intelligence.routing.service import CodeIntelligenceService
from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit._daytona_utils import _extract_exit_code, _wrap_bash_command
from tools.daytona_toolkit.codeact_tool import daytona_codeact
from tools.daytona_toolkit.codeact_transaction import (
    cleanup_codeact_transaction,
    collect_transaction_changes,
    commit_transaction_changes,
    create_codeact_transaction,
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
_test_loop: asyncio.AbstractEventLoop | None = None


def _decode_text(raw: bytes | str) -> str:
    return raw.decode("utf-8") if isinstance(raw, bytes) else str(raw)


def _get_loop() -> asyncio.AbstractEventLoop:
    global _test_loop
    if _test_loop is None or _test_loop.is_closed():
        _test_loop = asyncio.new_event_loop()
    return _test_loop


def _run(coro):
    return _get_loop().run_until_complete(coro)


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
class LiveRepoEnv:
    sandbox_id: str
    raw_sandbox: Any
    async_sandbox: Any
    home: str
    repo_root: str

    def exec(self, command: str, *, cwd: str | None = None, timeout: int = 180) -> tuple[int, str]:
        wrapped = command if cwd is None else f"cd {shlex.quote(cwd)} && {command}"
        response = self.raw_sandbox.process.exec(_wrap_bash_command(wrapped), timeout=timeout)
        raw = _TERM_NOISE.sub("", getattr(response, "result", "") or "")
        cleaned, exit_code = _extract_exit_code(raw, fallback_exit_code=getattr(response, "exit_code", None))
        return exit_code, cleaned

    def exec_checked(self, command: str, *, cwd: str | None = None, timeout: int = 180) -> str:
        exit_code, stdout = self.exec(command, cwd=cwd, timeout=timeout)
        if exit_code != 0:
            detail = stdout.strip() or f"exit {exit_code}"
            raise AssertionError(f"Sandbox command failed: {detail}")
        return stdout

    def read_file(self, rel_path: str) -> str:
        raw = self.raw_sandbox.fs.download_file(f"{self.repo_root}/{rel_path}")
        return _decode_text(raw)

    def write_file(self, rel_path: str, content: str) -> None:
        self.raw_sandbox.fs.upload_file(content.encode("utf-8"), f"{self.repo_root}/{rel_path}")

    def exists(self, rel_path: str) -> bool:
        exit_code, _ = self.exec(f"test -e {shlex.quote(f'{self.repo_root}/{rel_path}')}", timeout=30)
        return exit_code == 0

    def require_command(self, name: str) -> None:
        exit_code, _ = self.exec(f"command -v {shlex.quote(name)} >/dev/null 2>&1", timeout=30)
        if exit_code != 0:
            pytest.skip(f"Sandbox image missing required command: {name}")

    def seed_repo(self, *, committed: dict[str, str], dirty: dict[str, str] | None = None) -> None:
        self.exec_checked(
            f"rm -rf {shlex.quote(self.repo_root)} && mkdir -p {shlex.quote(self.repo_root)}",
            timeout=120,
        )
        self.exec_checked(
            "\n".join(
                [
                    f"git -C {shlex.quote(self.repo_root)} init",
                    f"git -C {shlex.quote(self.repo_root)} config user.email test@example.com",
                    f"git -C {shlex.quote(self.repo_root)} config user.name 'Test User'",
                ]
            )
        )
        for rel_path, content in committed.items():
            self.raw_sandbox.fs.upload_file(
                content.encode("utf-8"),
                f"{self.repo_root}/{rel_path}",
            )
        self.exec_checked(
            f"git -C {shlex.quote(self.repo_root)} add -A && "
            f"git -C {shlex.quote(self.repo_root)} commit -m init",
            timeout=120,
        )
        for rel_path, content in (dirty or {}).items():
            self.raw_sandbox.fs.upload_file(
                content.encode("utf-8"),
                f"{self.repo_root}/{rel_path}",
            )

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
        extra_metadata: dict[str, Any] | None = None,
    ) -> ToolExecutionContext:
        metadata: dict[str, Any] = {
            "daytona_sandbox": self.async_sandbox,
            "daytona_cwd": self.repo_root,
            "ci_service": ci_service,
            "agent_name": "developer",
            "team_mode_enabled": True,
            "agent_run_id": agent_run_id,
        }
        if extra_metadata:
            metadata.update(extra_metadata)
        return ToolExecutionContext(cwd=Path(self.repo_root), metadata=metadata)


@pytest.fixture
def live_repo_env():
    if not HAS_DAYTONA:
        pytest.skip("Daytona credentials not configured")

    from sandbox.testing import create_test_sandbox, delete_test_sandbox, get_sandbox_service

    info = create_test_sandbox(name="codeact-occ-tx")
    sandbox_id = info["id"]
    try:
        sandbox_svc = get_sandbox_service()
        raw_sandbox = sandbox_svc.get_sandbox_object(sandbox_id)
        home_resp = raw_sandbox.process.exec("pwd", timeout=10)
        home = (getattr(home_resp, "result", "") or "").strip() or "/home/daytona"
        env = LiveRepoEnv(
            sandbox_id=sandbox_id,
            raw_sandbox=raw_sandbox,
            async_sandbox=_AsyncSandboxWrapper(raw_sandbox),
            home=home,
            repo_root=f"{home}/codeact_occ_repo",
        )
        env.require_command("git")
        env.require_command("python3")
        yield env
    finally:
        delete_test_sandbox(sandbox_id)


def _json_result(result) -> dict[str, Any]:
    assert not result.is_error, result.output
    return json.loads(result.output)


def test_live_coordinated_python_transaction_uses_dirty_workspace_seed(live_repo_env: LiveRepoEnv):
    live_repo_env.seed_repo(
        committed={
            "shared.py": "def greeting():\n    return 'HEAD_STATE'\n",
            "obsolete.txt": "remove me\n",
        },
        dirty={"shared.py": "def greeting():\n    return 'DIRTY_STATE'\n"},
    )
    svc = live_repo_env.make_ci_service()
    ctx = live_repo_env.make_ctx(svc, agent_run_id=f"python-tx-{uuid.uuid4().hex[:8]}")

    code = """
from pathlib import Path

content = Path("shared.py").read_text(encoding="utf-8")
if "DIRTY_STATE" not in content:
    raise RuntimeError(f"scratch missed dirty workspace state: {content}")

updated = content.replace("DIRTY_STATE", "TX_VISIBLE")
Path("shared.py").write_text(updated, encoding="utf-8")

shell_result = shell("cat shared.py")
if "TX_VISIBLE" not in shell_result["stdout"]:
    raise RuntimeError(f"shell saw stale content: {shell_result['stdout']}")

Path("generated.txt").write_text("created in transaction\\n", encoding="utf-8")
Path("obsolete.txt").unlink()
print("python transaction ok")
"""

    result = _run(
        daytona_codeact.execute(
            daytona_codeact.input_model(code=code),
            ctx,
        )
    )

    data = _json_result(result)
    assert data["status"] == "ok"
    assert data["files_written"] == 3
    assert data["shells_run"] == 1
    assert data["write_conflicts"] == []
    assert data["write_errors"] == []
    assert "TX_VISIBLE" in live_repo_env.read_file("shared.py")
    assert live_repo_env.read_file("generated.txt") == "created in transaction\n"
    assert not live_repo_env.exists("obsolete.txt")


def test_live_coordinated_shell_transaction_commits_multifile_diff(live_repo_env: LiveRepoEnv):
    live_repo_env.seed_repo(
        committed={
            "shell_target.py": "VALUE = 'base'\n",
            "shell_delete.txt": "delete me\n",
        }
    )
    svc = live_repo_env.make_ci_service()
    ctx = live_repo_env.make_ctx(svc, agent_run_id=f"shell-tx-{uuid.uuid4().hex[:8]}")

    command = """python3 - <<'PY'
from pathlib import Path
Path("shell_target.py").write_text("VALUE = 'shell_committed'\\n", encoding="utf-8")
Path("shell_created.txt").write_text("shell created\\n", encoding="utf-8")
Path("shell_delete.txt").unlink()
PY"""

    result = _run(
        daytona_codeact.execute(
            daytona_codeact.input_model(mode="shell", command=command),
            ctx,
        )
    )

    data = _json_result(result)
    assert data["status"] == "ok"
    assert data["files_written"] == 3
    assert data["write_conflicts"] == []
    assert data["write_errors"] == []
    assert "shell_committed" in live_repo_env.read_file("shell_target.py")
    assert live_repo_env.read_file("shell_created.txt") == "shell created\n"
    assert not live_repo_env.exists("shell_delete.txt")


def test_live_coordinated_shell_transaction_rolls_back_on_failure(live_repo_env: LiveRepoEnv):
    live_repo_env.seed_repo(committed={"rollback.py": "VALUE = 'base'\n"})
    svc = live_repo_env.make_ci_service()
    ctx = live_repo_env.make_ctx(svc, agent_run_id=f"shell-rollback-{uuid.uuid4().hex[:8]}")

    command = """python3 - <<'PY'
from pathlib import Path
Path("rollback.py").write_text("VALUE = 'mutated'\\n", encoding="utf-8")
Path("rollback_new.txt").write_text("should not persist\\n", encoding="utf-8")
raise SystemExit(17)
PY"""

    result = _run(
        daytona_codeact.execute(
            daytona_codeact.input_model(mode="shell", command=command),
            ctx,
        )
    )

    assert result.is_error, result.output
    data = json.loads(result.output)
    assert data["status"] == "error"
    assert data["files_written"] == 0
    assert live_repo_env.read_file("rollback.py") == "VALUE = 'base'\n"
    assert not live_repo_env.exists("rollback_new.txt")


def test_live_same_base_transactions_merge_non_overlapping_changes(live_repo_env: LiveRepoEnv):
    live_repo_env.seed_repo(
        committed={
            "shared.py": (
                "def alpha():\n"
                "    return 'alpha'\n\n"
                "def beta():\n"
                "    return 'beta'\n\n"
                "def gamma():\n"
                "    return 'gamma'\n"
            )
        }
    )
    svc = live_repo_env.make_ci_service()
    ctx_a = live_repo_env.make_ctx(svc, agent_run_id=f"merge-a-{uuid.uuid4().hex[:8]}")
    ctx_b = live_repo_env.make_ctx(svc, agent_run_id=f"merge-b-{uuid.uuid4().hex[:8]}")

    tx_a = _run(create_codeact_transaction(ctx_a, live_repo_env.async_sandbox, live_repo_env.repo_root))
    tx_b = _run(create_codeact_transaction(ctx_b, live_repo_env.async_sandbox, live_repo_env.repo_root))
    try:
        scratch_a = f"{tx_a.scratch_root}/shared.py"
        scratch_b = f"{tx_b.scratch_root}/shared.py"
        original_a = _decode_text(live_repo_env.raw_sandbox.fs.download_file(scratch_a))
        original_b = _decode_text(live_repo_env.raw_sandbox.fs.download_file(scratch_b))
        live_repo_env.raw_sandbox.fs.upload_file(
            original_a.replace("return 'alpha'", "return 'ALPHA_MERGED'").encode("utf-8"),
            scratch_a,
        )
        live_repo_env.raw_sandbox.fs.upload_file(
            original_b.replace("return 'gamma'", "return 'GAMMA_MERGED'").encode("utf-8"),
            scratch_b,
        )

        report_a = _run(
            commit_transaction_changes(
                ctx_a,
                live_repo_env.async_sandbox,
                tx_a,
                _run(collect_transaction_changes(live_repo_env.async_sandbox, tx_a)),
            )
        )
        report_b = _run(
            commit_transaction_changes(
                ctx_b,
                live_repo_env.async_sandbox,
                tx_b,
                _run(collect_transaction_changes(live_repo_env.async_sandbox, tx_b)),
            )
        )
    finally:
        _run(cleanup_codeact_transaction(live_repo_env.async_sandbox, tx_a))
        _run(cleanup_codeact_transaction(live_repo_env.async_sandbox, tx_b))

    assert len(report_a.committed) == 1
    assert report_a.conflicts == []
    assert report_a.errors == []
    assert len(report_b.committed) == 1
    assert report_b.conflicts == []
    assert report_b.errors == []

    final = live_repo_env.read_file("shared.py")
    assert "ALPHA_MERGED" in final
    assert "GAMMA_MERGED" in final
    assert "return 'beta'" in final


def test_live_same_base_transactions_conflict_on_overlap(live_repo_env: LiveRepoEnv):
    live_repo_env.seed_repo(
        committed={
            "shared.py": (
                "def beta():\n"
                "    return 'beta'\n"
            )
        }
    )
    svc = live_repo_env.make_ci_service()
    ctx_a = live_repo_env.make_ctx(svc, agent_run_id=f"conflict-a-{uuid.uuid4().hex[:8]}")
    ctx_b = live_repo_env.make_ctx(svc, agent_run_id=f"conflict-b-{uuid.uuid4().hex[:8]}")

    tx_a = _run(create_codeact_transaction(ctx_a, live_repo_env.async_sandbox, live_repo_env.repo_root))
    tx_b = _run(create_codeact_transaction(ctx_b, live_repo_env.async_sandbox, live_repo_env.repo_root))
    try:
        scratch_a = f"{tx_a.scratch_root}/shared.py"
        scratch_b = f"{tx_b.scratch_root}/shared.py"
        original = _decode_text(live_repo_env.raw_sandbox.fs.download_file(scratch_a))
        live_repo_env.raw_sandbox.fs.upload_file(
            original.replace("return 'beta'", "return 'beta from A'").encode("utf-8"),
            scratch_a,
        )
        live_repo_env.raw_sandbox.fs.upload_file(
            original.replace("return 'beta'", "return 'beta from B'").encode("utf-8"),
            scratch_b,
        )

        report_a = _run(
            commit_transaction_changes(
                ctx_a,
                live_repo_env.async_sandbox,
                tx_a,
                _run(collect_transaction_changes(live_repo_env.async_sandbox, tx_a)),
            )
        )
        report_b = _run(
            commit_transaction_changes(
                ctx_b,
                live_repo_env.async_sandbox,
                tx_b,
                _run(collect_transaction_changes(live_repo_env.async_sandbox, tx_b)),
            )
        )
    finally:
        _run(cleanup_codeact_transaction(live_repo_env.async_sandbox, tx_a))
        _run(cleanup_codeact_transaction(live_repo_env.async_sandbox, tx_b))

    assert len(report_a.committed) == 1
    assert report_a.errors == []
    assert len(report_b.conflicts) == 1
    assert report_b.committed == []

    final = live_repo_env.read_file("shared.py")
    assert "beta from A" in final
    assert "beta from B" not in final
