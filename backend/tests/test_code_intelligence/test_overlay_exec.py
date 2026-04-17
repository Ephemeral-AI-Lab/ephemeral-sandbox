"""Unit tests for :mod:`code_intelligence.routing.overlay_exec`.

Tests focus on the bash wrapper construction and sentinel round-trip
parsing. Live sandbox integration lives in the e2e suite.
"""

from __future__ import annotations

import base64
from types import SimpleNamespace
from typing import Any

import pytest

from code_intelligence.routing.overlay_exec import (
    OverlayExec,
    OverlayExecError,
    OverlayMountError,
    _build_inner_script,
    _build_overlay_bash,
    _sentinel,
)


def test_inner_script_contains_required_mounts() -> None:
    inner = _build_inner_script(
        user_command="echo hi",
        lowerdir="/snapshot/repo",
        repo_root="/testbed",
        run_dir="/tmp/overlay-abc",
        tmpfs_size="1g",
    )
    assert "mount -t tmpfs" in inner
    assert "mount -t overlay" in inner
    assert "userxattr" in inner
    assert "mount --bind" in inner
    assert "tar --numeric-owner" in inner
    assert "lowerdir=/snapshot/repo" in inner
    assert "--bind /tmp/overlay-abc/ns/merged /testbed" in inner


def test_outer_wrapper_emits_sentinel_frames_and_unshare() -> None:
    cmd = _build_overlay_bash(
        user_command="echo hi",
        lowerdir="/snap",
        repo_root="/rr",
        run_dir="/tmp/run",
        run_id="abc",
        tmpfs_size="2g",
    )
    assert "unshare -Urm" in cmd
    for section in ("EXEC", "EXIT", "TAR", "MOUNT_ERR"):
        assert _sentinel("abc", section, "OPEN") in cmd
        assert _sentinel("abc", section, "CLOSE") in cmd


def _frame(run_id: str, section: str, payload: str) -> str:
    return (
        f"{_sentinel(run_id, section, 'OPEN')}\n"
        f"{payload}\n"
        f"{_sentinel(run_id, section, 'CLOSE')}\n"
    )


def _framed_output(
    run_id: str,
    *,
    exec_payload: bytes,
    exit_code: int,
    tar_path: str,
    tar_rc: int,
    mount_err: str = "",
) -> str:
    exec_b64 = base64.b64encode(exec_payload).decode("ascii")
    return (
        _frame(run_id, "EXEC", exec_b64)
        + _frame(run_id, "EXIT", str(exit_code))
        + _frame(run_id, "TAR", f"{tar_path}|{tar_rc}")
        + _frame(run_id, "MOUNT_ERR", mount_err)
    )


@pytest.mark.asyncio
async def test_execute_round_trip_parses_exec_and_tar() -> None:
    captured_cmd: dict[str, Any] = {}

    async def fake_exec(sandbox: Any, command: str, *, timeout: Any) -> Any:
        captured_cmd["command"] = command
        # Pull the run_id out of the built command by matching a sentinel.
        prefix = "__OVERLAYAUDIT_"
        start = command.index(prefix) + len(prefix)
        run_id = command[start : start + 32]
        return SimpleNamespace(
            result=_framed_output(
                run_id,
                exec_payload=b"hello\n",
                exit_code=0,
                tar_path=f"/tmp/overlay-{run_id}/audit.tar",
                tar_rc=0,
            )
        )

    overlay = OverlayExec(exec_process=fake_exec)
    result = await overlay.execute(
        sandbox=object(),
        user_command="echo hello",
        lowerdir="/snap",
        repo_root="/testbed",
    )
    assert result.exit_code == 0
    assert result.stdout == "hello\n"
    assert result.audit_tar_path.endswith("/audit.tar")
    assert "unshare -Urm" in captured_cmd["command"]


@pytest.mark.asyncio
async def test_execute_raises_mount_error_when_mount_err_populated() -> None:
    async def fake_exec(sandbox: Any, command: str, *, timeout: Any) -> Any:
        prefix = "__OVERLAYAUDIT_"
        start = command.index(prefix) + len(prefix)
        run_id = command[start : start + 32]
        return SimpleNamespace(
            result=_framed_output(
                run_id,
                exec_payload=b"",
                exit_code=-1,
                tar_path="",
                tar_rc=-1,
                mount_err="rc=92\nmount: overlay not supported",
            )
        )

    overlay = OverlayExec(exec_process=fake_exec)
    with pytest.raises(OverlayMountError) as info:
        await overlay.execute(
            sandbox=object(),
            user_command="true",
            lowerdir="/snap",
            repo_root="/testbed",
        )
    assert "overlay not supported" in str(info.value)


@pytest.mark.asyncio
async def test_execute_raises_exec_error_on_missing_section() -> None:
    async def fake_exec(sandbox: Any, command: str, *, timeout: Any) -> Any:
        return SimpleNamespace(result="totally unrelated output")

    overlay = OverlayExec(exec_process=fake_exec)
    with pytest.raises(OverlayExecError):
        await overlay.execute(
            sandbox=object(),
            user_command="true",
            lowerdir="/snap",
            repo_root="/testbed",
        )


@pytest.mark.asyncio
async def test_execute_raises_mount_error_when_tar_fails() -> None:
    async def fake_exec(sandbox: Any, command: str, *, timeout: Any) -> Any:
        prefix = "__OVERLAYAUDIT_"
        start = command.index(prefix) + len(prefix)
        run_id = command[start : start + 32]
        return SimpleNamespace(
            result=_framed_output(
                run_id,
                exec_payload=b"",
                exit_code=-1,
                tar_path=f"/tmp/overlay-{run_id}/audit.tar",
                tar_rc=2,
            )
        )

    overlay = OverlayExec(exec_process=fake_exec)
    with pytest.raises(OverlayMountError) as info:
        await overlay.execute(
            sandbox=object(),
            user_command="true",
            lowerdir="/snap",
            repo_root="/testbed",
        )
    assert "rc=2" in str(info.value)
