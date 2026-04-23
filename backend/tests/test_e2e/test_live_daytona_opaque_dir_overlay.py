"""Live E2E: overlay commit handles opaque-dir scenarios without rejecting.

Regression coverage for the `overlay_unsupported_opaque_dir` bug where
directory-replacement workloads (pytest cache invalidation, pip install
upgrade, Python bytecode recreation) tripped the overlay kind-gate and
aborted the daytona_shell commit.

Scenarios:

1. `__pycache__` creation — Python bytecode compilation during module
   import is the minimal repro: a gitignored dir is created in the
   overlay's upperdir; no opaque marker on first creation but subsequent
   re-imports can set one.
2. `.pytest_cache` with `--cache-clear` — pytest removes the cache dir
   and recreates it; the rmdir+mkdir sequence is exactly what sets the
   overlay opaque xattr.
3. pip install `--upgrade` into a gitignored target dir — upgrades
   replace per-package directories and reliably produce opaque markers.

All three must commit successfully (`audit_success=True`, no
`overlay_unsupported_opaque_dir` / `overlay_refused_opaque_dir` in the
reason string) and leave the expected files in the live workspace.

Run with: pytest backend/tests/test_e2e/test_live_daytona_opaque_dir_overlay.py -m live -v
"""

from __future__ import annotations

import asyncio
import json
import os
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
from tools.daytona_toolkit.shell_tool import daytona_shell

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


# ---------------------------------------------------------------------------
# Sandbox fixture — one sandbox shared across all scenarios in this module.
# ---------------------------------------------------------------------------


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
        return SimpleNamespace(
            result=getattr(response, "result", "") or "",
            exit_code=getattr(response, "exit_code", None),
        )

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
class _OpaqueEnv:
    sandbox_id: str
    raw_sandbox: Any
    async_sandbox: Any
    workspace_root: str

    def exec(self, command: str, *, timeout: int = 180) -> tuple[int, str]:
        response = self.raw_sandbox.process.exec(
            _wrap_bash_command(command), timeout=timeout
        )
        raw = getattr(response, "result", "") or ""
        cleaned, exit_code = _extract_exit_code(
            raw,
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        return exit_code, cleaned

    def exec_checked(self, command: str, *, timeout: int = 180) -> str:
        exit_code, stdout = self.exec(command, timeout=timeout)
        if exit_code != 0:
            raise AssertionError(
                f"sandbox command failed (exit={exit_code}): {stdout.strip()[:400]}"
            )
        return stdout

    def make_ci_service(self) -> CodeIntelligenceService:
        return CodeIntelligenceService(
            sandbox_id=self.sandbox_id,
            workspace_root=self.workspace_root,
            sandbox=self.raw_sandbox,
        )

    def make_ctx(
        self, ci_service: CodeIntelligenceService, *, agent_run_id: str
    ) -> ToolExecutionContext:
        return ToolExecutionContext(
            cwd=Path(self.workspace_root),
            metadata={
                "daytona_sandbox": self.async_sandbox,
                "daytona_cwd": self.workspace_root,
                "ci_service": ci_service,
                "agent_run_id": agent_run_id,
            },
        )


@pytest.fixture(scope="module")
def opaque_env():
    if not HAS_DAYTONA:
        pytest.skip("Daytona credentials not configured")

    from sandbox.testing import (
        create_test_sandbox,
        delete_test_sandbox,
        get_sandbox_service,
    )

    info = create_test_sandbox(name="overlay-opaque-live")
    sandbox_id = info["id"]
    try:
        raw_sandbox = get_sandbox_service().get_sandbox_object(sandbox_id)
        home_resp = raw_sandbox.process.exec("pwd", timeout=10)
        home = (getattr(home_resp, "result", "") or "").strip() or "/home/daytona"
        workspace = f"{home}/overlay_opaque_{uuid.uuid4().hex[:8]}"
        env = _OpaqueEnv(
            sandbox_id=sandbox_id,
            raw_sandbox=raw_sandbox,
            async_sandbox=_AsyncSandboxWrapper(raw_sandbox),
            workspace_root=workspace,
        )
        # Seed a real git repo at workspace_root with a gitignore covering
        # all three scenarios. Overlay requires workspace_root to be a git
        # repo (snapshot step runs git add + git write-tree).
        env.exec_checked(
            " && ".join(
                [
                    f"mkdir -p {shlex.quote(workspace)}",
                    f"cd {shlex.quote(workspace)}",
                    "git init -q",
                    "git config user.email overlay-opaque@test.invalid",
                    "git config user.name overlay-opaque",
                    "printf '%s\\n' '__pycache__/' '.pytest_cache/' 'vendor/' > .gitignore",
                    "echo 'placeholder' > README.md",
                    "git add -A",
                    "git commit -q -m seed",
                ]
            ),
            timeout=60,
        )
        yield env
    finally:
        delete_test_sandbox(sandbox_id)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


_OPAQUE_REJECT_TOKENS = (
    "overlay_unsupported_opaque_dir",
    "overlay_refused_opaque_dir",
)


def _assert_shell_succeeded(result, *, scenario: str) -> dict[str, Any]:
    # The bug path surfaces as is_error=True with the reject sentinel in
    # output (via audit_conflict_reason). We want neither.
    assert not result.is_error, (
        f"{scenario}: shell reported error\n"
        f"output={result.output[:1500]}"
    )
    for token in _OPAQUE_REJECT_TOKENS:
        assert token not in result.output, (
            f"{scenario}: overlay rejected with {token}\n"
            f"output={result.output[:1500]}"
        )
    payload = json.loads(result.output)
    assert payload["status"] == "ok", f"{scenario}: status={payload.get('status')}"
    shells = payload.get("shell_outputs") or []
    assert shells, f"{scenario}: no shell output"
    return payload


async def _run_shell(env: _OpaqueEnv, command: str, *, scenario: str) -> dict[str, Any]:
    svc = env.make_ci_service()
    ctx = env.make_ctx(svc, agent_run_id=f"{scenario}-{uuid.uuid4().hex[:8]}")
    result = await daytona_shell.execute(
        daytona_shell.input_model(command=command),
        ctx,
    )
    return _assert_shell_succeeded(result, scenario=scenario)


# ---------------------------------------------------------------------------
# 1. __pycache__ — Python bytecode compilation.
# ---------------------------------------------------------------------------


def test_overlay_commits_pycache_creation_and_update(opaque_env: _OpaqueEnv):
    env = opaque_env
    # Seed a python module under the repo.
    env.exec_checked(
        f"printf '%s\\n' 'def greet(): return \"v1\"' > {shlex.quote(env.workspace_root)}/mymod.py",
        timeout=30,
    )

    # First import: creates __pycache__/ and a .pyc.
    asyncio.run(
        _run_shell(
            env,
            f"cd {shlex.quote(env.workspace_root)} && "
            "python3 -c 'import mymod; print(mymod.greet())'",
            scenario="pycache-create",
        )
    )
    exit_code, out = env.exec(
        f"ls {shlex.quote(env.workspace_root)}/__pycache__/", timeout=30
    )
    assert exit_code == 0, f"__pycache__ not merged: {out}"
    assert ".pyc" in out, f"expected .pyc in __pycache__: {out}"

    # Modify source: next import regenerates the .pyc. On rootless
    # overlay this sequence frequently sets the opaque xattr because
    # python unlinks the stale .pyc and rewrites.
    env.exec_checked(
        f"printf '%s\\n' 'def greet(): return \"v2\"' > {shlex.quote(env.workspace_root)}/mymod.py",
        timeout=30,
    )
    asyncio.run(
        _run_shell(
            env,
            f"cd {shlex.quote(env.workspace_root)} && "
            "python3 -c 'import mymod; print(mymod.greet())'",
            scenario="pycache-update",
        )
    )


# ---------------------------------------------------------------------------
# 2. .pytest_cache — pytest with --cache-clear.
# ---------------------------------------------------------------------------


def test_overlay_commits_pytest_cache_with_clear(opaque_env: _OpaqueEnv):
    env = opaque_env
    # Seed a trivial passing test.
    env.exec_checked(
        f"printf '%s\\n' 'def test_ok(): assert 1 == 1' "
        f"> {shlex.quote(env.workspace_root)}/test_trivial.py",
        timeout=30,
    )
    # Ensure pytest is available; skip cleanly if not.
    rc, _ = env.exec("python3 -m pytest --version", timeout=30)
    if rc != 0:
        pytest.skip("pytest not installed in sandbox image")

    # First run: creates .pytest_cache/ fresh (no opaque — new dir).
    asyncio.run(
        _run_shell(
            env,
            f"cd {shlex.quote(env.workspace_root)} && "
            "python3 -m pytest -q test_trivial.py",
            scenario="pytest-cache-create",
        )
    )
    rc, out = env.exec(
        f"ls {shlex.quote(env.workspace_root)}/.pytest_cache/", timeout=30
    )
    assert rc == 0, f".pytest_cache not merged after first run: {out}"

    # Second run with --cache-clear: pytest removes then recreates the
    # cache dir, which is the canonical overlay opaque trigger.
    asyncio.run(
        _run_shell(
            env,
            f"cd {shlex.quote(env.workspace_root)} && "
            "python3 -m pytest -q --cache-clear test_trivial.py",
            scenario="pytest-cache-clear",
        )
    )
    # Cache dir should still exist and be populated by the second run.
    rc, out = env.exec(
        f"ls {shlex.quote(env.workspace_root)}/.pytest_cache/", timeout=30
    )
    assert rc == 0, f".pytest_cache missing after cache-clear run: {out}"


# ---------------------------------------------------------------------------
# 3. pip install --upgrade — package-directory replacement.
# ---------------------------------------------------------------------------


def test_overlay_commits_pip_install_upgrade_into_vendor(opaque_env: _OpaqueEnv):
    env = opaque_env
    rc, _ = env.exec("python3 -m pip --version", timeout=30)
    if rc != 0:
        pytest.skip("pip not installed in sandbox image")
    vendor = f"{env.workspace_root}/vendor"

    # Cold install: no opaque markers expected (fresh dirs).
    asyncio.run(
        _run_shell(
            env,
            f"cd {shlex.quote(env.workspace_root)} && "
            f"python3 -m pip install --quiet --target {shlex.quote(vendor)} "
            "'six==1.15.0'",
            scenario="pip-install-cold",
        )
    )
    rc, out = env.exec(
        f"cat {shlex.quote(vendor)}/six-1.15.0.dist-info/METADATA "
        "| grep ^Version",
        timeout=30,
    )
    assert rc == 0 and "1.15.0" in out, f"1.15.0 not installed: {out}"

    # Upgrade: pip removes the old six dist-info + package dir and
    # recreates them. That rm+mkdir is the opaque-marker trigger the
    # classifier used to reject.
    asyncio.run(
        _run_shell(
            env,
            f"cd {shlex.quote(env.workspace_root)} && "
            f"python3 -m pip install --quiet --target {shlex.quote(vendor)} "
            "--upgrade 'six==1.16.0'",
            scenario="pip-install-upgrade",
        )
    )
    rc, out = env.exec(
        f"cat {shlex.quote(vendor)}/six-1.16.0.dist-info/METADATA "
        "| grep ^Version",
        timeout=30,
    )
    assert rc == 0 and "1.16.0" in out, f"1.16.0 not installed after upgrade: {out}"
    # Note: `pip install --target --upgrade` does not rmtree old dist-info
    # dirs (version is in the dir name, so they are separate paths), so we
    # do not assert the 1.15.0 dist-info is gone. The overlay fix being
    # verified here is: the commit accepts the upgrade's directory
    # replacements without rejecting on opaque xattrs. Both daytona_shell calls
    # above already assert no `overlay_*_opaque_dir` token appears in
    # the tool output.
