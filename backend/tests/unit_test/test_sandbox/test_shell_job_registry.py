"""Unit tests for :class:`sandbox.daemon.service.shell_job.ShellJobRegistry`.

These exercise launch / poll / cancel / reap lifecycle without a real
overlay or daemon, using a fake :class:`SandboxOverlay` whose
``acquire_operation_overlay`` returns a handle backed by tmp dirs. The
``run_workspace_replaced_command`` boundary is monkeypatched so the tests
run on macOS without namespace or overlay support.
"""

from __future__ import annotations

import asyncio
import os
import signal
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable
from unittest.mock import MagicMock
from uuid import uuid4

import pytest

from sandbox.daemon.service import shell_job as shell_job_module
from sandbox.daemon.service.shell_job import (
    ShellJob,
    ShellJobNotFound,
    ShellJobRegistry,
)
from sandbox.daemon.service.sandbox_overlay import OperationOverlayHandle
from sandbox.execution.contract import (
    CommandExecRequest,
    MountMode,
    ShellProcessResult,
)


pytestmark = pytest.mark.asyncio


@dataclass
class _FakePublishResult:
    path_changes: tuple[Any, ...] = ()
    changeset: Any = None
    timings: dict[str, float] = None  # type: ignore[assignment]

    def __post_init__(self) -> None:
        if self.timings is None:
            self.timings = {}


class _FakeSandboxOverlay:
    """Stand-in that exposes only the surface ShellJobRegistry calls."""

    def __init__(self, tmp_path: Path) -> None:
        self._scratch = tmp_path
        self.publish_calls = 0
        self.released_leases: list[str] = []
        self.maintenance_calls = 0

    def acquire_operation_overlay(
        self,
        *,
        request_id: str,
        materialize: bool = False,
    ) -> OperationOverlayHandle:
        del materialize
        rid = uuid4().hex[:8]
        run_dir = self._scratch / f"run-{request_id}-{rid}"
        upperdir = run_dir / "upper"
        workdir = run_dir / "work"
        lowerdir = run_dir / "lower"
        upperdir.mkdir(parents=True, exist_ok=True)
        workdir.mkdir(parents=True, exist_ok=True)
        lowerdir.mkdir(parents=True, exist_ok=True)
        owner = self

        class _ReleaseShim:
            """Captures release calls without needing the full SandboxOverlay."""

            def release_operation_overlay(_inner_self, handle: OperationOverlayHandle) -> None:
                owner.released_leases.append(handle.lease_id)

        return OperationOverlayHandle(
            lease_id=f"lease-{rid}",
            manifest_key=f"hash@1",
            manifest_version=1,
            root_hash="hash",
            manifest=MagicMock(version=1),
            workspace_root="/testbed",
            run_dir=str(run_dir),
            upperdir=str(upperdir),
            workdir=str(workdir),
            lowerdir=str(lowerdir),
            layer_paths=None,
            _overlay=_ReleaseShim(),  # type: ignore[arg-type]
        )

    async def publish_cycle(
        self,
        *,
        request: CommandExecRequest,
        upperdir: str,
        snapshot: Any,
        run_maintenance: bool,
    ) -> _FakePublishResult:
        self.publish_calls += 1
        # Synthesize a delete change so we don't need to materialize a real
        # content_path / final_hash; the registry only walks .path.
        from sandbox.execution.path_change import OverlayPathChange

        return _FakePublishResult(
            path_changes=(
                OverlayPathChange(
                    path="touched.txt",
                    kind="delete",
                    content_path=None,
                    final_hash=None,
                ),
            ),
            changeset=MagicMock(published_manifest_version=2),
            timings={"command_exec.capture_upperdir_s": 0.01},
        )

    async def run_maintenance_after_publish(
        self,
        changeset: Any,
        *,
        workspace_ref: str,
    ) -> dict[str, float]:
        self.maintenance_calls += 1
        return {"command_exec.run_maintenance_s": 0.0}


def _make_request(command: str = "true", timeout_seconds: float | None = 30.0) -> CommandExecRequest:
    return CommandExecRequest(
        request_id=uuid4().hex,
        workspace_ref="/tmp/fake-ws-ref",
        workspace_root="/testbed",
        command=("bash", "-lc", command),
        cwd=".",
        env={},
        timeout_seconds=timeout_seconds,
        actor_id="test",
        description="shell.test",
    )


def _stub_strategy_runner_factory(
    *,
    duration_s: float = 0.05,
    exit_code: int = 0,
    audit_events: list[tuple[str, dict[str, Any]]] | None = None,
) -> Callable[..., ShellProcessResult]:
    """Returns a stub ``run_workspace_replaced_command`` that exec's `sleep`.

    The stub spawns a real ``python -c "time.sleep(X)"`` subprocess so the
    cancel + killpg pipeline is exercised end-to-end. ``cancel_event`` is
    respected via :func:`wait_for_process_with_cancel`.
    """
    from sandbox.execution.subprocess_runner import wait_for_process_with_cancel

    def _stub(
        *,
        spec: Any,
        request: CommandExecRequest,
        run_dir: Path,
        timings: dict[str, float],
        cancel_event: threading.Event | None = None,
        pid_recorder: Callable[[int], None] | None = None,
        **_kwargs: Any,
    ) -> ShellProcessResult:
        stdout_ref = run_dir / "stdout.bin"
        stderr_ref = run_dir / "stderr.bin"
        stdout_ref.parent.mkdir(parents=True, exist_ok=True)
        # Touch the upperdir so the post-publish path has something to walk.
        (Path(spec.writes) / "touched.txt").write_bytes(b"hello")
        with stdout_ref.open("wb") as out, stderr_ref.open("wb") as err:
            proc = subprocess.Popen(
                [sys.executable, "-c", f"import time; time.sleep({duration_s}); print('done')"],
                stdout=out,
                stderr=err,
                start_new_session=True,
            )
            if pid_recorder is not None:
                pid_recorder(proc.pid)
            rc = wait_for_process_with_cancel(
                proc,
                timeout_seconds=request.timeout_seconds,
                cancel_event=cancel_event,
            )
        timings["command_exec.run_command_s"] = duration_s
        # Preserve the rc verbatim: a negative value (SIGTERM/SIGKILL) signals
        # that cancel terminated the child, which the registry uses to derive
        # the ``cancelled`` status.
        return ShellProcessResult(
            exit_code=rc,
            stdout_ref=str(stdout_ref),
            stderr_ref=str(stderr_ref),
            mounted_workspace_root=spec.workspace_root,
            mount_mode=MountMode.COPY_BACKED,
        )

    return _stub


async def _run_shell_job(
    registry: ShellJobRegistry,
    overlay: _FakeSandboxOverlay,
    storage_root: Path,
    *,
    command: str = "true",
    timeout_seconds: float | None = 30.0,
    cancel_after_s: float | None = None,
) -> dict[str, Any]:
    request = _make_request(command, timeout_seconds=timeout_seconds)
    launch = registry.launch(
        request=request,
        overlay=overlay,  # type: ignore[arg-type]
        storage_root=storage_root,
    )
    job_id = str(launch["job_id"])
    if cancel_after_s is not None:
        await asyncio.sleep(cancel_after_s)
        registry.cancel(job_id, reason="test_cancel")
    reap = await registry.reap(job_id, timeout_seconds=10.0)
    return reap


@pytest.fixture(autouse=True)
def _patch_runner(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(
        shell_job_module,
        "run_workspace_replaced_command",
        _stub_strategy_runner_factory(duration_s=0.05),
    )


@pytest.fixture
def overlay(tmp_path: Path) -> _FakeSandboxOverlay:
    return _FakeSandboxOverlay(tmp_path)


@pytest.fixture
def registry() -> ShellJobRegistry:
    reg = ShellJobRegistry(reaper_interval_s=10.0, ttl_seconds=10.0)
    yield reg
    reg.shutdown()


async def test_golden_path_launch_then_reap(
    registry: ShellJobRegistry,
    overlay: _FakeSandboxOverlay,
    tmp_path: Path,
) -> None:
    audit: list[tuple[str, dict[str, Any]]] = []
    registry._audit_callback = lambda name, payload: audit.append((name, payload))
    reap = await _run_shell_job(registry, overlay, tmp_path, command="echo hi")
    assert reap["status"] == "finished"
    assert reap["exit_code"] == 0
    # OCC publish must have happened on a non-cancelled job.
    assert overlay.publish_calls == 1
    assert overlay.maintenance_calls == 1
    assert reap["changed_paths"]
    # Audit events: exactly one launched + one reaped.
    launched = [e for e in audit if e[0].endswith("launched")]
    reaped = [e for e in audit if e[0].endswith("reaped")]
    assert len(launched) == 1
    assert len(reaped) == 1


async def test_cancel_skips_publish_and_releases_lease(
    monkeypatch: pytest.MonkeyPatch,
    registry: ShellJobRegistry,
    overlay: _FakeSandboxOverlay,
    tmp_path: Path,
) -> None:
    # Use a longer-running stub so cancel can race.
    monkeypatch.setattr(
        shell_job_module,
        "run_workspace_replaced_command",
        _stub_strategy_runner_factory(duration_s=10.0),
    )
    audit: list[tuple[str, dict[str, Any]]] = []
    registry._audit_callback = lambda name, payload: audit.append((name, payload))
    reap = await _run_shell_job(
        registry,
        overlay,
        tmp_path,
        command="sleep 10",
        cancel_after_s=0.2,
    )
    assert reap["status"] == "cancelled"
    # No OCC publish on cancelled jobs (plan principle: "cancelled discards all writes").
    assert overlay.publish_calls == 0
    assert overlay.released_leases  # lease released exactly once via the handle
    cancelled_events = [e for e in audit if e[0].endswith("cancelled")]
    reaped_events = [e for e in audit if e[0].endswith("reaped")]
    launched_events = [e for e in audit if e[0].endswith("launched")]
    assert len(cancelled_events) == 1
    assert len(launched_events) == 1
    assert len(reaped_events) == 1


async def test_cancel_unknown_job_raises_not_found(
    registry: ShellJobRegistry,
) -> None:
    with pytest.raises(ShellJobNotFound):
        registry.cancel("shell-nonexistent")


async def test_poll_returns_snapshot_with_status(
    monkeypatch: pytest.MonkeyPatch,
    registry: ShellJobRegistry,
    overlay: _FakeSandboxOverlay,
    tmp_path: Path,
) -> None:
    monkeypatch.setattr(
        shell_job_module,
        "run_workspace_replaced_command",
        _stub_strategy_runner_factory(duration_s=2.0),
    )
    request = _make_request(command="sleep 2", timeout_seconds=10.0)
    launch = registry.launch(
        request=request,
        overlay=overlay,  # type: ignore[arg-type]
        storage_root=tmp_path,
    )
    job_id = str(launch["job_id"])
    # Give it a beat to spawn.
    await asyncio.sleep(0.05)
    snapshot = registry.poll(job_id)
    assert snapshot["job_id"] == job_id
    assert snapshot["status"] in {"running", "finished"}
    # Cleanup.
    registry.cancel(job_id, reason="test cleanup")
    await registry.reap(job_id, timeout_seconds=5.0)


async def test_double_cancel_is_idempotent(
    monkeypatch: pytest.MonkeyPatch,
    registry: ShellJobRegistry,
    overlay: _FakeSandboxOverlay,
    tmp_path: Path,
) -> None:
    monkeypatch.setattr(
        shell_job_module,
        "run_workspace_replaced_command",
        _stub_strategy_runner_factory(duration_s=5.0),
    )
    request = _make_request(command="sleep 5", timeout_seconds=10.0)
    launch = registry.launch(
        request=request,
        overlay=overlay,  # type: ignore[arg-type]
        storage_root=tmp_path,
    )
    job_id = str(launch["job_id"])
    first = registry.cancel(job_id, reason="r1")
    second = registry.cancel(job_id, reason="r2")
    assert first["cancelled"] is True
    assert second["cancelled"] is True
    assert second.get("already_cancelled") is True
    await registry.reap(job_id, timeout_seconds=5.0)


async def test_reap_removes_job_from_registry(
    registry: ShellJobRegistry,
    overlay: _FakeSandboxOverlay,
    tmp_path: Path,
) -> None:
    request = _make_request(command="echo hi")
    launch = registry.launch(
        request=request,
        overlay=overlay,  # type: ignore[arg-type]
        storage_root=tmp_path,
    )
    job_id = str(launch["job_id"])
    assert registry.get(job_id) is not None
    await registry.reap(job_id, timeout_seconds=5.0)
    assert registry.get(job_id) is None


async def test_late_cancel_after_completion_preserves_status(
    registry: ShellJobRegistry,
    overlay: _FakeSandboxOverlay,
    tmp_path: Path,
) -> None:
    """T8 cousin: cancel arrives after the job already exited."""
    request = _make_request(command="echo hi", timeout_seconds=10.0)
    launch = registry.launch(
        request=request,
        overlay=overlay,  # type: ignore[arg-type]
        storage_root=tmp_path,
    )
    job_id = str(launch["job_id"])
    # Wait for the strategy thread to finish.
    job = registry.get(job_id)
    assert job is not None
    while not job.process_done.is_set():
        await asyncio.sleep(0.02)
    cancel_resp = registry.cancel(job_id, reason="late")
    assert cancel_resp["cancelled"] is False
    assert cancel_resp.get("already_done") is True
    reap = await registry.reap(job_id, timeout_seconds=5.0)
    assert reap["status"] == "finished"


async def test_metrics_reports_active_and_ttl_reaped(
    registry: ShellJobRegistry,
    overlay: _FakeSandboxOverlay,
    tmp_path: Path,
) -> None:
    """``metrics()`` surfaces active_jobs + ttl_reaped_total for the
    ``api.shell.metrics`` RPC (AC-13). Fresh registry → 0/0; after one
    synthesized TTL reap → ttl_reaped_total == 1.
    """
    snapshot = registry.metrics()
    assert snapshot == {"active_jobs": 0, "ttl_reaped_total": 0}

    # Launch a long-running job and force-age its last_poll_at past TTL.
    request = _make_request(command="sleep 5", timeout_seconds=30.0)
    launch = registry.launch(
        request=request,
        overlay=overlay,  # type: ignore[arg-type]
        storage_root=tmp_path,
    )
    job_id = str(launch["job_id"])
    job = registry.get(job_id)
    assert job is not None
    assert registry.metrics()["active_jobs"] == 1
    # Push last_poll_at into the past so the next reap-stale cycle picks it.
    job.last_poll_at -= registry._ttl_seconds + 10.0
    registry._reap_stale_jobs()
    snapshot = registry.metrics()
    assert snapshot["active_jobs"] == 0
    assert snapshot["ttl_reaped_total"] == 1
    # Allow the cancel-escalation thread to drain so the test process exits.
    if job.thread_future is not None:
        await asyncio.get_event_loop().run_in_executor(
            None,
            lambda: job.thread_future.result(timeout=5.0),
        )
