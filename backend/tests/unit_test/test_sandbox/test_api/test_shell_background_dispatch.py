"""Unit tests for the engine-side ``sandbox.api.tool.shell`` background path.

Drives ``_shell_background_dispatch`` + ``_send_cancel_then_reap`` with a
stub transport so we can verify the launch -> reap sequence, the
``CancelledError`` -> cancel + reap fallback, and the result projection
(``shell.reap`` payload -> :class:`ShellResult`).
"""

from __future__ import annotations

import asyncio
from collections.abc import Mapping
from typing import Any

import pytest

from audit.base import AuditEvent
from sandbox.api.tool.shell import shell as shell_api
from sandbox.api.transport import (
    DAEMON_OP_SHELL_CANCEL,
    DAEMON_OP_SHELL_LAUNCH,
    DAEMON_OP_SHELL_REAP,
)
from sandbox.audit import events as sandbox_audit_events
from sandbox._shared.models import SandboxCaller, ShellRequest


pytestmark = pytest.mark.asyncio


class _RecordingAuditSink:
    """Captures every AuditEvent published, in order."""

    def __init__(self) -> None:
        self.events: list[AuditEvent] = []

    def publish(self, event: AuditEvent) -> None:
        self.events.append(event)

    def shell_event_types(self) -> list[str]:
        return [e.type for e in self.events if e.type.startswith("sandbox.shell.")]


class _StubTransport:
    """Records every transport.call and returns canned responses by op."""

    def __init__(
        self,
        *,
        launch_response: dict[str, Any] | None = None,
        reap_response: dict[str, Any] | None = None,
        reap_raises: BaseException | None = None,
        cancel_response: dict[str, Any] | None = None,
    ) -> None:
        self.calls: list[tuple[str, Mapping[str, object]]] = []
        self._launch = launch_response or {
            "success": True,
            "job_id": "shell-test-job",
            "lease_id": "lease-x",
            "timings": {},
        }
        self._reap = reap_response or {
            "success": True,
            "job_id": "shell-test-job",
            "status": "finished",
            "exit_code": 0,
            "stdout": "ok\n",
            "stderr": "",
            "changed_paths": ["modified.py"],
            "timings": {"command_exec.total_s": 0.01},
            "error": None,
        }
        self._reap_raises = reap_raises
        self._cancel = cancel_response or {
            "success": True,
            "job_id": "shell-test-job",
            "cancelled": True,
            "timings": {},
        }

    async def call(
        self,
        sandbox_id: str,
        op: str,
        payload: Mapping[str, object],
        *,
        timeout: int,
    ) -> dict[str, Any]:
        self.calls.append((op, dict(payload)))
        if op == DAEMON_OP_SHELL_LAUNCH:
            return dict(self._launch)
        if op == DAEMON_OP_SHELL_REAP:
            if self._reap_raises is not None:
                exc = self._reap_raises
                self._reap_raises = None
                raise exc
            return dict(self._reap)
        if op == DAEMON_OP_SHELL_CANCEL:
            return dict(self._cancel)
        raise AssertionError(f"unexpected op: {op}")


def _request(*, background: bool = True) -> ShellRequest:
    return ShellRequest(
        command="echo hi",
        cwd=".",
        timeout=60,
        background=background,
        caller=SandboxCaller(agent_id="test-agent"),
        description="shell.test",
    )


async def test_background_dispatch_fires_launch_then_reap() -> None:
    transport = _StubTransport()
    result = await shell_api("sandbox-1", _request(), transport=transport)
    op_sequence = [op for op, _ in transport.calls]
    assert op_sequence == [DAEMON_OP_SHELL_LAUNCH, DAEMON_OP_SHELL_REAP]
    # Reap result projects into a ShellResult with status="ok" + exit_code=0.
    assert result.success is True
    assert result.exit_code == 0
    assert result.stdout == "ok\n"
    assert result.status == "ok"
    assert result.changed_paths == ("modified.py",)


async def test_background_dispatch_cancel_routes_cancel_then_reap() -> None:
    """Mid-reap ``CancelledError`` triggers shell.cancel + shell.reap.

    Mirrors the engine-cancel path: ``BackgroundTaskManager.cancel`` calls
    ``asyncio_task.cancel()``, which raises ``CancelledError`` inside the
    coroutine. The shell tool's background dispatcher must catch it, send
    the daemon cancel, then re-raise so the asyncio task transitions to
    cancelled.
    """
    transport = _StubTransport(reap_raises=asyncio.CancelledError())
    with pytest.raises(asyncio.CancelledError):
        await shell_api("sandbox-1", _request(), transport=transport)
    op_sequence = [op for op, _ in transport.calls]
    assert op_sequence == [
        DAEMON_OP_SHELL_LAUNCH,
        DAEMON_OP_SHELL_REAP,  # initial reap that raised
        DAEMON_OP_SHELL_CANCEL,  # cancel + best-effort reap from the handler
        DAEMON_OP_SHELL_REAP,
    ]
    # The cancel payload must include the job_id from launch.
    cancel_call = next(call for op, call in transport.calls if op == DAEMON_OP_SHELL_CANCEL)
    assert cancel_call["job_id"] == "shell-test-job"


async def test_background_dispatch_launch_failure_returns_error_result() -> None:
    """``shell.launch`` failure becomes a shell_error_result, not an exception."""
    transport = _StubTransport(
        launch_response={
            "success": False,
            "error": {"kind": "lease_conflict", "message": "no lease available"},
            "timings": {},
        }
    )
    result = await shell_api("sandbox-1", _request(), transport=transport)
    assert result.success is False
    assert result.exit_code == 1
    # Only launch was attempted.
    assert [op for op, _ in transport.calls] == [DAEMON_OP_SHELL_LAUNCH]


async def test_foreground_dispatch_unchanged_by_background_field() -> None:
    """``background=False`` MUST still take the single-RPC path."""
    fg_response = {
        "success": True,
        "exit_code": 0,
        "stdout": "fg-out",
        "stderr": "",
        "changed_paths": [],
        "status": "ok",
        "timings": {},
        "warnings": [],
    }
    calls: list[tuple[str, Mapping[str, object]]] = []

    class _FgTransport:
        async def call(
            self,
            sandbox_id: str,
            op: str,
            payload: Mapping[str, object],
            *,
            timeout: int,
        ) -> dict[str, Any]:
            calls.append((op, dict(payload)))
            return dict(fg_response)

    result = await shell_api(
        "sandbox-1", _request(background=False), transport=_FgTransport()
    )
    assert result.exit_code == 0
    assert result.stdout == "fg-out"
    # Foreground hits ``api.v1.shell`` exactly once; no launch/reap.
    ops = [op for op, _ in calls]
    assert ops == ["api.v1.shell"]


async def test_background_dispatch_cancelled_status_projects_correctly() -> None:
    """When reap returns ``status=cancelled``, ShellResult.status == 'cancelled'."""
    transport = _StubTransport(
        reap_response={
            "success": True,
            "job_id": "shell-test-job",
            "status": "cancelled",
            "exit_code": -15,
            "stdout": "partial",
            "stderr": "",
            "changed_paths": [],
            "timings": {},
            "error": None,
        }
    )
    result = await shell_api("sandbox-1", _request(), transport=transport)
    assert result.status == "cancelled"
    assert result.success is False
    assert result.exit_code == -15


async def test_audit_emits_launched_then_reaped_on_golden_path() -> None:
    """Golden path publishes exactly ``[LAUNCHED, REAPED]`` to the audit sink."""
    transport = _StubTransport()
    sink = _RecordingAuditSink()
    await shell_api("sandbox-1", _request(), transport=transport, audit_sink=sink)
    shell_types = sink.shell_event_types()
    assert shell_types == [
        sandbox_audit_events.SHELL_LAUNCHED,
        sandbox_audit_events.SHELL_REAPED,
    ]
    launched = next(e for e in sink.events if e.type == sandbox_audit_events.SHELL_LAUNCHED)
    assert launched.payload["job_id"] == "shell-test-job"
    assert launched.payload["lease_id"] == "lease-x"
    reaped = next(e for e in sink.events if e.type == sandbox_audit_events.SHELL_REAPED)
    assert reaped.payload["job_id"] == "shell-test-job"
    assert reaped.payload["status"] == "finished"
    assert reaped.payload["changed_paths_count"] == 1


async def test_audit_emits_launched_cancelled_reaped_on_cancel_path() -> None:
    """Mid-reap cancellation publishes ``[LAUNCHED, CANCELLED, REAPED]``.

    Order matters: SHELL_CANCELLED MUST come before the cancel RPC fires (so a
    post-mortem invariant scan sees the cancel intent even when the daemon
    drops the reap response). The trailing SHELL_REAPED carries
    ``status='cancelled'`` on a clean cancel path.
    """
    transport = _StubTransport(reap_raises=asyncio.CancelledError())
    sink = _RecordingAuditSink()
    with pytest.raises(asyncio.CancelledError):
        await shell_api("sandbox-1", _request(), transport=transport, audit_sink=sink)
    shell_types = sink.shell_event_types()
    assert shell_types == [
        sandbox_audit_events.SHELL_LAUNCHED,
        sandbox_audit_events.SHELL_CANCELLED,
        sandbox_audit_events.SHELL_REAPED,
    ]
    cancelled = next(
        e for e in sink.events if e.type == sandbox_audit_events.SHELL_CANCELLED
    )
    assert cancelled.payload["reason"] == "engine_cancel"
    assert cancelled.payload["job_id"] == "shell-test-job"


async def test_audit_no_emit_on_launch_failure() -> None:
    """Failed shell.launch must NOT emit SHELL_LAUNCHED (lease never acquired)."""
    transport = _StubTransport(
        launch_response={
            "success": False,
            "error": {"kind": "lease_conflict", "message": "no lease available"},
            "timings": {},
        }
    )
    sink = _RecordingAuditSink()
    await shell_api("sandbox-1", _request(), transport=transport, audit_sink=sink)
    assert sink.shell_event_types() == []
