"""Cancellation-aware namespace execution tests."""

from __future__ import annotations

import asyncio
import json
import threading
from pathlib import Path
from typing import Callable

import pytest

from sandbox._shared.models import Intent, ToolCallRequest
from sandbox.ephemeral_workspace.pipeline import EphemeralPipeline
from sandbox.overlay import namespace_runner as namespace_mod
from sandbox.occ.changeset import ChangesetResult
from sandbox.overlay.handle import OverlayHandle


pytestmark = pytest.mark.asyncio


async def test_run_in_namespace_signals_shell_cancellation(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    started = threading.Event()
    saw_cancel = threading.Event()

    async def _fake_child(
        *,
        payload_ref: Path,
        stdout_ref: Path,
        stderr_ref: Path,
        timeout: float | None,
        cancel_event: threading.Event | None,
        pid_recorder: Callable[[int], None] | None,
    ) -> int:
        del payload_ref, stdout_ref, stderr_ref, timeout
        assert cancel_event is not None
        if pid_recorder is not None:
            pid_recorder(99999999)
        started.set()
        if await asyncio.to_thread(cancel_event.wait, 2):
            saw_cancel.set()
        return -15

    monkeypatch.setattr(namespace_mod, "_run_namespace_entrypoint_async", _fake_child)
    handle = OverlayHandle(
        workspace_root="/testbed",
        layer_paths=((tmp_path / "lower").as_posix(),),
        upperdir=tmp_path / "upper",
        workdir=tmp_path / "work",
        lease_id="lease-1",
        holder_pid=None,
        run_dir=tmp_path,
        snapshot_manifest=None,
        _release=None,
    )
    handle.upperdir.mkdir(parents=True)
    handle.workdir.mkdir(parents=True)
    req = ToolCallRequest(
        invocation_id="req-1",
        agent_id="agent-a",
        verb="shell",
        intent=Intent.WRITE_ALLOWED,
        args={"command": "sleep 60", "cwd": ".", "timeout_seconds": 60},
    )

    task = asyncio.create_task(namespace_mod.run_in_namespace(handle, req))
    await asyncio.to_thread(started.wait, 2)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task

    assert saw_cancel.is_set()


async def test_namespace_runner_cancel_kills_child_and_discards_upperdir(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    started = threading.Event()
    saw_cancel = threading.Event()
    releases: list[str] = []

    class _Manifest:
        version = 1
        layers = ()

    class _Snapshot:
        lease_id = "lease-1"
        manifest_version = 1
        root_hash = "root"
        manifest = _Manifest()
        layer_paths = ((tmp_path / "lower").as_posix(),)

    class _LayerStack:
        def acquire_snapshot(self, *, request_id: str) -> _Snapshot:
            assert request_id.startswith("overlay:agent-a:")
            return _Snapshot()

        def release_lease(self, *, lease_id: str) -> bool:
            releases.append(lease_id)
            return True

        def read_active_manifest(self) -> _Manifest:
            return _Manifest()

    class _Occ:
        async def apply_changeset(self, *args, **kwargs):
            raise AssertionError("cancelled namespace run must not publish")

        async def run_maintenance_after_publish(self, *args, **kwargs):
            return {}

    async def fake_child(
        *,
        payload_ref: Path,
        stdout_ref: Path,
        stderr_ref: Path,
        timeout: float | None,
        cancel_event: threading.Event | None,
        pid_recorder: Callable[[int], None] | None,
    ) -> int:
        del payload_ref, stdout_ref, stderr_ref, timeout
        assert cancel_event is not None
        if pid_recorder is not None:
            pid_recorder(99999999)
        started.set()
        if await asyncio.to_thread(cancel_event.wait, 2):
            saw_cancel.set()
        return -15

    async def fail_capture(*_args, **_kwargs) -> ChangesetResult:
        raise AssertionError("cancelled namespace run must not capture upperdir")

    monkeypatch.setattr(namespace_mod, "_run_namespace_entrypoint_async", fake_child)
    monkeypatch.setattr(
        "sandbox.overlay.lifecycle.overlay_writable_root",
        lambda: tmp_path / "writable",
    )
    monkeypatch.setattr(
        "sandbox.ephemeral_workspace.pipeline.overlay_writable_root",
        lambda: tmp_path / "writable",
    )
    monkeypatch.setattr(
        "sandbox.ephemeral_workspace.pipeline.overlay_lifecycle.capture_changes",
        fail_capture,
    )
    pipeline = EphemeralPipeline(
        occ_client=_Occ(),
        workspace_ref=tmp_path.as_posix(),
        layer_stack=_LayerStack(),
    )
    req = ToolCallRequest(
        invocation_id="req-1",
        agent_id="agent-a",
        verb="shell",
        intent=Intent.WRITE_ALLOWED,
        args={"command": "sleep 60", "cwd": ".", "timeout_seconds": 60},
    )

    task = asyncio.create_task(pipeline.run_tool_call(req))
    await asyncio.to_thread(started.wait, 2)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task

    assert saw_cancel.is_set()
    assert releases == ["lease-1"]
    overlay_root = tmp_path / "writable" / "runtime" / "overlay"
    assert not overlay_root.exists() or list(overlay_root.iterdir()) == []


async def test_run_in_namespace_does_not_use_default_threadpool(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def fake_child(
        *,
        payload_ref: Path,
        stdout_ref: Path,
        stderr_ref: Path,
        timeout: float | None,
        cancel_event: threading.Event | None,
        pid_recorder: Callable[[int], None] | None,
    ) -> int:
        del stdout_ref, stderr_ref, timeout, cancel_event, pid_recorder
        payload = json.loads(payload_ref.read_text(encoding="utf-8"))
        Path(str(payload["result_ref"])).write_text(
            json.dumps(
                {
                    "success": True,
                    "status": "ok",
                    "workspace": "ephemeral",
                    "timings": {"workspace.tool_s": 0.001},
                }
            ),
            encoding="utf-8",
        )
        return 0

    async def fail_to_thread(*_args, **_kwargs):
        raise AssertionError("fresh namespace runner should not use to_thread")

    monkeypatch.setattr(namespace_mod, "_run_namespace_entrypoint_async", fake_child)
    monkeypatch.setattr(namespace_mod.asyncio, "to_thread", fail_to_thread)
    handle = OverlayHandle(
        workspace_root="/testbed",
        layer_paths=((tmp_path / "lower").as_posix(),),
        upperdir=tmp_path / "upper",
        workdir=tmp_path / "work",
        lease_id="lease-1",
        holder_pid=None,
        run_dir=tmp_path,
        snapshot_manifest=None,
        _release=None,
    )
    handle.upperdir.mkdir(parents=True)
    handle.workdir.mkdir(parents=True)
    req = ToolCallRequest(
        invocation_id="req-1",
        agent_id="agent-a",
        verb="shell",
        intent=Intent.WRITE_ALLOWED,
        args={"command": "true", "cwd": "."},
    )

    result = await namespace_mod.run_in_namespace(handle, req)

    assert result["success"] is True
    assert result["timings"]["workspace.tool_s"] == 0.001
