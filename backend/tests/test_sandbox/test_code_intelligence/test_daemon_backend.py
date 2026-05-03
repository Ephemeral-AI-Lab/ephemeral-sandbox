"""Unit tests for ``DaemonBackend.ensure_initialized`` and runtime commands.

These tests exercise the runtime-route contract: ``ensure_initialized`` uploads
the runtime bundle (mocked); command calls route through the dispatcher.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any
from unittest.mock import patch

from sandbox.runtime.backends import DaemonBackend
from sandbox.runtime import command_client
from sandbox.runtime.command_client import RuntimeCommandClient


class _NullTransport:
    """Minimal stub for tests that patch the runtime upload boundary."""

    name = "null"

    async def exec(self, *args: Any, **kwargs: Any) -> Any:
        del args, kwargs
        raise AssertionError("DaemonBackend should not call transport.exec post-3.5")


class _FakeRuntime:
    """Stand-in for :class:`DaemonBackend` returning canned runtime responses."""

    def __init__(
        self,
        *,
        cmd_response: dict[str, Any] | None = None,
        raise_for_op: dict[str, Exception] | None = None,
    ) -> None:
        self.calls: list[tuple[str, dict[str, Any] | None]] = []
        self._cmd_response = cmd_response or {}
        self._raise_for_op = dict(raise_for_op or {})

    async def _call_runtime_command(
        self,
        op: str,
        args: dict[str, Any] | None = None,
        *,
        timeout: float = 30.0,
    ) -> Any:
        del timeout
        self.calls.append((op, args))
        if op in self._raise_for_op:
            raise self._raise_for_op[op]
        if op == "shell":
            return self._cmd_response
        return None


def _backend_with_fake_runtime(runtime: _FakeRuntime) -> DaemonBackend:
    backend = DaemonBackend(
        sandbox_id="sb-test",
        workspace_root="/ws",
        transport=_NullTransport(),  # type: ignore[arg-type]
    )
    backend._call_runtime_command = runtime._call_runtime_command  # type: ignore[method-assign]
    return backend


def test_ensure_initialized_uploads_runtime_bundle() -> None:
    runtime = _FakeRuntime()
    backend = _backend_with_fake_runtime(runtime)
    calls: list[tuple[Any, str]] = []

    async def fake_upload(transport: Any, sandbox_id: str) -> str:
        calls.append((transport, sandbox_id))
        return "digest"

    with patch(
        "sandbox.runtime.command_client._ensure_runtime_uploaded_via_transport",
        fake_upload,
    ):
        ok = backend.ensure_initialized(wait=True)
    assert ok is True
    assert backend.is_initialized is True
    assert calls == [(backend._transport, "sb-test")]


def test_ensure_initialized_idempotent() -> None:
    runtime = _FakeRuntime()
    backend = _backend_with_fake_runtime(runtime)
    calls = 0

    async def fake_upload(*_: Any, **__: Any) -> str:
        nonlocal calls
        calls += 1
        return "digest"

    with patch(
        "sandbox.runtime.command_client._ensure_runtime_uploaded_via_transport",
        fake_upload,
    ):
        backend.ensure_initialized(wait=True)
        backend.ensure_initialized(wait=True)
    assert calls == 1


def test_cmd_routes_through_runtime_and_reconstructs_namespace() -> None:
    """``cmd`` uses the runtime ``shell`` op and preserves result fields."""
    import asyncio
    from unittest.mock import MagicMock

    runtime = _FakeRuntime(
        cmd_response={
            "result": "hi\n",
            "exit_code": 0,
            "changed_paths": ["/ws/a.py"],
            "conflict_file": None,
            "conflict_reason": None,
            "warnings": [],
            "overlay_run_timings": {"total": 0.2},
        }
    )
    backend = _backend_with_fake_runtime(runtime)
    progress: list[str] = []

    async def _run() -> None:
        result = await backend.cmd(
            MagicMock(),
            "echo hi",
            timeout=5,
            agent_id="agent-a",
            on_progress_line=progress.append,
        )
        assert result.result == "hi\n"
        assert result.exit_code == 0
        assert result.changed_paths == ["/ws/a.py"]
        assert result.overlay_run_timings == {"total": 0.2}
        assert result.runtime_call_timings["total"] >= 0.0

    asyncio.run(_run())
    assert progress == ["hi\n"]
    assert runtime.calls == [
        (
            "shell",
            {
                "sandbox_id": "sb-test",
                "workspace_root": "/ws",
                "command": "echo hi",
                "timeout": 5,
                "agent_id": "agent-a",
            },
        )
    ]


def test_rebind_sandbox_is_noop() -> None:
    backend = DaemonBackend(
        sandbox_id="sb-test",
        workspace_root="/ws",
        transport=_NullTransport(),  # type: ignore[arg-type]
    )
    backend.rebind_sandbox(object())


def test_init_drops_snapshot_cache_attributes() -> None:
    """Cleanup invariant: the orchestrator-side snapshot cache attributes
    are gone (Phase 3.5 retirement)."""
    backend = DaemonBackend(
        sandbox_id="sb-test",
        workspace_root="/ws",
        transport=_NullTransport(),  # type: ignore[arg-type]
    )
    for attr in (
        "_symbol_cache",
        "_cached_file_count",
        "_cached_symbol_count",
        "_snapshot_bytes",
    ):
        assert not hasattr(backend, attr), (
            f"Phase 3.5 cleanup regression: {attr} still on DaemonBackend"
        )


def test_runtime_client_module_has_no_semantic_query_methods() -> None:
    """Boundary invariant: command_client.py stays runtime-command-only."""
    source = Path(command_client.__file__).read_text(encoding="utf-8")
    forbidden = (
        "hover_result_from_dict",
        "reference_info_from_dict",
        "diagnostic_from_dict",
        "semantic_query",
        "symbol_query",
    )
    for token in forbidden:
        assert token not in source
    for method in ("semantic_query", "symbol_query"):
        assert not hasattr(RuntimeCommandClient, method)
