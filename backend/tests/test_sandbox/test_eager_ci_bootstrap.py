"""Unit tests for the eager in-sandbox CI bootstrap hook + lifecycle wiring.

Covers:

* :func:`bootstrap_in_sandbox_ci_runtime` no-ops when the flag is off,
  transport is missing, or workspace is empty.
* :func:`bootstrap_in_sandbox_ci_runtime` starts the daemon when the flag is set.
* :meth:`SandboxService.create_sandbox` (a) calls the hook when the flag is
  set, (b) skips when the flag is unset, (c) propagates errors from the hook.
"""

from __future__ import annotations

import asyncio
from typing import Any
from unittest.mock import patch

import pytest


@pytest.fixture
def flag_on(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EOS_CI_IN_SANDBOX", "1")


@pytest.fixture
def flag_off(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("EOS_CI_IN_SANDBOX", raising=False)


# ---------------------------------------------------------------------------
# bootstrap_in_sandbox_ci_runtime
# ---------------------------------------------------------------------------


def test_bootstrap_helper_noop_when_flag_off(flag_off: None) -> None:
    from sandbox.lifecycle.workspace import bootstrap_in_sandbox_ci_runtime

    sentinel = object()
    transport = type("T", (), {"exec": lambda *_, **__: sentinel})()

    asyncio.run(
        bootstrap_in_sandbox_ci_runtime(
            sandbox_id="sb-1",
            workspace_root="/ws",
            transport=transport,
        )
    )  # No exception, no exec called (lambda would have returned sentinel).


def test_bootstrap_helper_noop_when_transport_none(flag_on: None) -> None:
    from sandbox.lifecycle.workspace import bootstrap_in_sandbox_ci_runtime

    asyncio.run(
        bootstrap_in_sandbox_ci_runtime(
            sandbox_id="sb-1",
            workspace_root="/ws",
            transport=None,
        )
    )


def test_bootstrap_helper_noop_when_workspace_empty(flag_on: None) -> None:
    from sandbox.lifecycle.workspace import bootstrap_in_sandbox_ci_runtime

    transport = type(
        "T",
        (),
        {"exec": lambda *_, **__: pytest.fail("transport.exec must not be called")},
    )()

    asyncio.run(
        bootstrap_in_sandbox_ci_runtime(
            sandbox_id="sb-1",
            workspace_root="",
            transport=transport,
        )
    )


def test_bootstrap_helper_starts_daemon(flag_on: None) -> None:
    from sandbox.code_intelligence.daemon.launcher import bundle_hash
    from sandbox.lifecycle.workspace import bootstrap_in_sandbox_ci_runtime

    calls: list[tuple[str, str]] = []

    class FakeTransport:
        async def exec(self, sandbox_id: str, command: str, **_: Any) -> Any:
            calls.append((sandbox_id, command))
            if 'printf %s "$HOME"' in command:
                return type("R", (), {"exit_code": 0, "stdout": "/home/u"})()
            if "daemon.pid" in command and "kill -0" in command:
                return type("R", (), {"exit_code": 1, "stdout": ""})()
            if ".bundle-hash" in command and "tar -xzf" not in command:
                return type("R", (), {"exit_code": 0, "stdout": bundle_hash()})()
            return type("R", (), {"exit_code": 0, "stdout": "{\"ok\": true}"})()

        async def write_bytes(
            self, sandbox_id: str, path: str, content: bytes
        ) -> None:
            del sandbox_id, path, content
            pytest.fail("write_bytes must not be used (use chunked exec)")

    asyncio.run(
        bootstrap_in_sandbox_ci_runtime(
            sandbox_id="sb-1",
            workspace_root="/ws",
            transport=FakeTransport(),
        )
    )
    assert any(
        "setsid nohup python3 -m sandbox.code_intelligence.daemon" in cmd
        for _, cmd in calls
    )
    assert any("--workspace-root /ws" in cmd for _, cmd in calls)
    assert any("test -S" in cmd and "daemon.sock" in cmd for _, cmd in calls)


def test_bootstrap_helper_raises_on_daemon_failure(flag_on: None) -> None:
    from sandbox.code_intelligence.daemon.launcher import DaemonUnavailable
    from sandbox.lifecycle.workspace import bootstrap_in_sandbox_ci_runtime

    async def fail_ensure(*_: Any, **__: Any) -> None:
        raise DaemonUnavailable("socket timeout")

    with patch(
        "sandbox.code_intelligence.daemon.launcher.DaemonLauncher.ensure_daemon",
        new=fail_ensure,
    ), pytest.raises(DaemonUnavailable, match="socket timeout"):
        asyncio.run(
            bootstrap_in_sandbox_ci_runtime(
                sandbox_id="sb-1",
                workspace_root="/ws",
                transport=object(),
            )
        )


# ---------------------------------------------------------------------------
# _maybe_run_eager_ci_bootstrap (lifecycle entry point)
# ---------------------------------------------------------------------------


def _make_raw_sandbox(project_dir: str | None) -> Any:
    return type(
        "RawSandbox",
        (),
        {"project_dir": project_dir, "labels": None},
    )()


def test_maybe_bootstrap_skips_when_flag_off(flag_off: None) -> None:
    from sandbox.lifecycle.service import _maybe_run_eager_ci_bootstrap

    sentinel_called = {"called": False}

    async def boom(*_: Any, **__: Any) -> None:
        sentinel_called["called"] = True

    with patch(
        "sandbox.lifecycle.service.bootstrap_in_sandbox_ci_runtime",
        new=boom,
    ):
        _maybe_run_eager_ci_bootstrap(_make_raw_sandbox("/ws"), "sb-1")
    assert sentinel_called["called"] is False


def test_maybe_bootstrap_skips_when_workspace_unresolvable(
    flag_on: None,
) -> None:
    from sandbox.lifecycle.service import _maybe_run_eager_ci_bootstrap

    sentinel_called = {"called": False}

    async def boom(*_: Any, **__: Any) -> None:
        sentinel_called["called"] = True

    with patch(
        "sandbox.lifecycle.service.bootstrap_in_sandbox_ci_runtime",
        new=boom,
    ):
        _maybe_run_eager_ci_bootstrap(_make_raw_sandbox(None), "sb-1")
    assert sentinel_called["called"] is False


def test_maybe_bootstrap_invokes_helper_when_flag_on(
    flag_on: None,
) -> None:
    from sandbox.lifecycle.service import _maybe_run_eager_ci_bootstrap

    calls: list[dict[str, Any]] = []

    async def fake_helper(
        sandbox_id: str, workspace_root: str, *, transport: Any
    ) -> None:
        calls.append(
            {
                "sandbox_id": sandbox_id,
                "workspace_root": workspace_root,
                "transport": transport,
            }
        )

    fake_transport = object()
    with patch(
        "sandbox.lifecycle.service.bootstrap_in_sandbox_ci_runtime",
        new=fake_helper,
    ), patch(
        "sandbox.daytona.transport.DaytonaTransport",
        return_value=fake_transport,
    ):
        _maybe_run_eager_ci_bootstrap(_make_raw_sandbox("/ws"), "sb-1")

    assert len(calls) == 1
    assert calls[0]["sandbox_id"] == "sb-1"
    assert calls[0]["workspace_root"] == "/ws"
    assert calls[0]["transport"] is fake_transport


def test_maybe_bootstrap_propagates_runtime_error(flag_on: None) -> None:
    from sandbox.lifecycle.service import _maybe_run_eager_ci_bootstrap

    async def fake_helper(*_: Any, **__: Any) -> None:
        raise RuntimeError("daemon crashed")

    with patch(
        "sandbox.lifecycle.service.bootstrap_in_sandbox_ci_runtime",
        new=fake_helper,
    ), patch(
        "sandbox.daytona.transport.DaytonaTransport",
        return_value=object(),
    ), pytest.raises(RuntimeError, match="daemon crashed"):
        _maybe_run_eager_ci_bootstrap(_make_raw_sandbox("/ws"), "sb-1")


# ---------------------------------------------------------------------------
# bootstrap_upload_runtime_bundle (background-upload phase)
# ---------------------------------------------------------------------------


def test_upload_helper_noop_when_flag_off(flag_off: None) -> None:
    from sandbox.lifecycle.workspace import bootstrap_upload_runtime_bundle

    transport = type(
        "T",
        (),
        {"exec": lambda *_, **__: pytest.fail("transport.exec must not be called")},
    )()

    asyncio.run(
        bootstrap_upload_runtime_bundle(
            sandbox_id="sb-1",
            workspace_root="/ws",
            transport=transport,
        )
    )


def test_upload_helper_noop_on_missing_inputs(flag_on: None) -> None:
    from sandbox.lifecycle.workspace import bootstrap_upload_runtime_bundle

    transport = type(
        "T",
        (),
        {"exec": lambda *_, **__: pytest.fail("transport.exec must not be called")},
    )()

    for missing in ({"transport": None}, {"sandbox_id": ""}, {"workspace_root": ""}):
        kwargs: dict[str, Any] = {
            "sandbox_id": "sb-1",
            "workspace_root": "/ws",
            "transport": transport,
        }
        kwargs.update(missing)
        asyncio.run(bootstrap_upload_runtime_bundle(**kwargs))


def test_upload_helper_uploads_without_spawning_daemon(flag_on: None) -> None:
    """Background upload runs ensure_runtime_uploaded but never the daemon spawn."""
    from sandbox.lifecycle.workspace import bootstrap_upload_runtime_bundle

    upload_calls: list[tuple[Any, str]] = []
    spawn_called = {"value": False}

    async def fake_upload(transport: Any, sandbox_id: str) -> str:
        upload_calls.append((transport, sandbox_id))
        return "deadbeef"

    async def fake_spawn(*_: Any, **__: Any) -> None:
        spawn_called["value"] = True

    fake_transport = object()
    with patch(
        "sandbox.code_intelligence.daemon.launcher.ensure_runtime_uploaded",
        new=fake_upload,
    ), patch(
        "sandbox.code_intelligence.daemon.launcher.DaemonLauncher.spawn",
        new=fake_spawn,
    ):
        asyncio.run(
            bootstrap_upload_runtime_bundle(
                sandbox_id="sb-1",
                workspace_root="/ws",
                transport=fake_transport,
            )
        )

    assert upload_calls == [(fake_transport, "sb-1")]
    assert spawn_called["value"] is False


# ---------------------------------------------------------------------------
# _maybe_start_eager_ci_bundle_upload / _finish_eager_ci_bundle_upload
# ---------------------------------------------------------------------------


def test_start_upload_returns_none_when_flag_off(flag_off: None) -> None:
    from sandbox.lifecycle.service import _maybe_start_eager_ci_bundle_upload

    assert (
        _maybe_start_eager_ci_bundle_upload(_make_raw_sandbox("/ws"), "sb-1")
        is None
    )


def test_start_upload_returns_none_when_workspace_missing(flag_on: None) -> None:
    from sandbox.lifecycle.service import _maybe_start_eager_ci_bundle_upload

    assert _maybe_start_eager_ci_bundle_upload(_make_raw_sandbox(None), "sb-1") is None


def test_start_upload_submits_future_and_invokes_helper(flag_on: None) -> None:
    """Future resolves successfully when the background upload completes."""
    import threading

    from sandbox.lifecycle.service import (
        _finish_eager_ci_bundle_upload,
        _maybe_start_eager_ci_bundle_upload,
    )

    helper_done = threading.Event()
    helper_args: dict[str, Any] = {}

    async def fake_helper(
        sandbox_id: str, workspace_root: str, *, transport: Any
    ) -> None:
        helper_args.update(
            sandbox_id=sandbox_id,
            workspace_root=workspace_root,
            transport=transport,
        )
        helper_done.set()

    fake_transport = object()
    with patch(
        "sandbox.lifecycle.service.bootstrap_upload_runtime_bundle",
        new=fake_helper,
    ), patch(
        "sandbox.daytona.transport.DaytonaTransport",
        return_value=fake_transport,
    ):
        future = _maybe_start_eager_ci_bundle_upload(_make_raw_sandbox("/ws"), "sb-1")
        assert future is not None
        # Caller drains the future; success path must not raise.
        _finish_eager_ci_bundle_upload(future, "sb-1")

    assert helper_done.is_set()
    assert helper_args == {
        "sandbox_id": "sb-1",
        "workspace_root": "/ws",
        "transport": fake_transport,
    }


def test_finish_upload_swallows_helper_failure(flag_on: None) -> None:
    """Background failure must not propagate — sequential bootstrap retries."""
    from sandbox.lifecycle.service import (
        _finish_eager_ci_bundle_upload,
        _maybe_start_eager_ci_bundle_upload,
    )

    async def boom(*_: Any, **__: Any) -> None:
        raise RuntimeError("upload exploded")

    with patch(
        "sandbox.lifecycle.service.bootstrap_upload_runtime_bundle",
        new=boom,
    ), patch(
        "sandbox.daytona.transport.DaytonaTransport",
        return_value=object(),
    ):
        future = _maybe_start_eager_ci_bundle_upload(_make_raw_sandbox("/ws"), "sb-1")
        assert future is not None
        _finish_eager_ci_bundle_upload(future, "sb-1")  # MUST NOT raise


def test_finish_upload_noop_when_future_none() -> None:
    from sandbox.lifecycle.service import _finish_eager_ci_bundle_upload

    _finish_eager_ci_bundle_upload(None, "sb-1")  # MUST NOT raise
