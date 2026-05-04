"""Tests for host-side runtime setup orchestration."""

from __future__ import annotations

from typing import Any

import pytest

from sandbox.api.utils.models import RawExecResult
from sandbox.runtime.bundle import BUNDLE_REMOTE_DIR
from sandbox.runtime.setup_orchestrator import SetupRegistry, SetupScript


async def test_run_all_noops_when_registry_empty() -> None:
    registry = SetupRegistry()

    async def fail_exec(*_: Any, **__: Any) -> RawExecResult:
        raise AssertionError("exec should not run")

    async def fail_upload(*_: Any, **__: Any) -> str:
        raise AssertionError("upload should not run")

    assert await registry.run_all(
        "sb-1",
        exec_fn=fail_exec,
        ensure_uploaded=fail_upload,
    ) == []


async def test_run_all_uploads_once_and_executes_setup_scripts_in_order() -> None:
    registry = SetupRegistry()
    registry.register(
        SetupScript(
            name="peer_a",
            package="sandbox.runtime.peer_a",
            relative_path="sandbox/runtime/peer_a/setup.sh",
        )
    )
    registry.register(
        SetupScript(
            name="peer_b",
            package="sandbox.runtime.peer_b",
            relative_path="sandbox/runtime/peer_b/setup.sh",
        )
    )
    calls: list[tuple[str, str, str | None, int | None]] = []
    uploads: list[str] = []

    async def fake_upload(sandbox_id: str) -> str:
        uploads.append(sandbox_id)
        return "digest"

    async def fake_exec(
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> RawExecResult:
        calls.append((sandbox_id, command, cwd, timeout))
        return RawExecResult(exit_code=0, stdout="")

    await registry.run_all("sb-1", exec_fn=fake_exec, ensure_uploaded=fake_upload)

    assert uploads == ["sb-1"]
    assert calls == [
        (
            "sb-1",
            "bash sandbox/runtime/peer_a/setup.sh",
            BUNDLE_REMOTE_DIR,
            300,
        ),
        (
            "sb-1",
            "bash sandbox/runtime/peer_b/setup.sh",
            BUNDLE_REMOTE_DIR,
            300,
        ),
    ]


def test_setup_script_must_point_to_bundled_setup_sh() -> None:
    with pytest.raises(ValueError, match="setup.sh"):
        SetupScript(
            name="bad",
            package="sandbox.bad",
            relative_path="sandbox/bad/install.sh",
        )
