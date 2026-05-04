"""Unit tests for the eager in-sandbox runtime bootstrap hook + lifecycle wiring.

Covers:

* :func:`bootstrap_in_sandbox_runtime` no-ops when the flag is off,
  sandbox id is missing, or workspace is empty.
* :func:`bootstrap_in_sandbox_runtime` uploads the command runtime when the flag is set.
* the lifecycle hook skips when the flag is unset and propagates upload errors.
"""

from __future__ import annotations

import asyncio
from typing import Any
from unittest.mock import patch

import pytest


@pytest.fixture
def flag_on(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EOS_SANDBOX_RUNTIME_BOOTSTRAP", "1")


@pytest.fixture
def flag_off(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("EOS_SANDBOX_RUNTIME_BOOTSTRAP", raising=False)


# ---------------------------------------------------------------------------
# bootstrap_in_sandbox_runtime
# ---------------------------------------------------------------------------


def test_bootstrap_helper_noop_when_flag_off(flag_off: None) -> None:
    from sandbox.lifecycle.workspace import bootstrap_in_sandbox_runtime

    asyncio.run(
        bootstrap_in_sandbox_runtime(
            sandbox_id="sb-1",
            workspace_root="/ws",
        )
    )  # No exception, upload helper is gated by the flag.


def test_bootstrap_helper_uploads_by_sandbox_id(flag_on: None) -> None:
    from sandbox.lifecycle.workspace import bootstrap_in_sandbox_runtime

    calls: list[str] = []

    async def fake_upload(sandbox_id: str) -> str:
        calls.append(sandbox_id)
        return "deadbeef"

    with patch("sandbox.control.daemon.bundle.ensure_runtime_uploaded", new=fake_upload):
        asyncio.run(
            bootstrap_in_sandbox_runtime(
                sandbox_id="sb-1",
                workspace_root="/ws",
            )
        )

    assert calls == ["sb-1"]


def test_bootstrap_helper_noop_when_workspace_empty(flag_on: None) -> None:
    from sandbox.lifecycle.workspace import bootstrap_in_sandbox_runtime

    asyncio.run(
        bootstrap_in_sandbox_runtime(
            sandbox_id="sb-1",
            workspace_root="",
        )
    )


def test_bootstrap_helper_raises_on_runtime_upload_failure(flag_on: None) -> None:
    from sandbox.lifecycle.workspace import bootstrap_in_sandbox_runtime

    async def fail_upload(*_: Any, **__: Any) -> str:
        raise RuntimeError("runtime unavailable")

    with patch("sandbox.control.daemon.bundle.ensure_runtime_uploaded", new=fail_upload), pytest.raises(
        RuntimeError, match="runtime unavailable"
    ):
        asyncio.run(
            bootstrap_in_sandbox_runtime(
                sandbox_id="sb-1",
                workspace_root="/ws",
            )
        )


# ---------------------------------------------------------------------------
# _maybe_run_eager_runtime_bootstrap (lifecycle entry point)
# ---------------------------------------------------------------------------


def _make_raw_sandbox(project_dir: str | None) -> Any:
    return type(
        "RawSandbox",
        (),
        {"project_dir": project_dir, "labels": None},
    )()


def test_maybe_bootstrap_skips_when_flag_off(flag_off: None) -> None:
    from sandbox.providers.daytona.lifecycle import _maybe_run_eager_runtime_bootstrap

    sentinel_called = {"called": False}

    async def boom(*_: Any, **__: Any) -> None:
        sentinel_called["called"] = True

    with patch(
        "sandbox.providers.daytona.lifecycle.bootstrap_in_sandbox_runtime",
        new=boom,
    ):
        _maybe_run_eager_runtime_bootstrap(_make_raw_sandbox("/ws"), "sb-1")
    assert sentinel_called["called"] is False


def test_maybe_bootstrap_skips_when_workspace_unresolvable(
    flag_on: None,
) -> None:
    from sandbox.providers.daytona.lifecycle import _maybe_run_eager_runtime_bootstrap

    sentinel_called = {"called": False}

    async def boom(*_: Any, **__: Any) -> None:
        sentinel_called["called"] = True

    with patch(
        "sandbox.providers.daytona.lifecycle.bootstrap_in_sandbox_runtime",
        new=boom,
    ):
        _maybe_run_eager_runtime_bootstrap(_make_raw_sandbox(None), "sb-1")
    assert sentinel_called["called"] is False


def test_maybe_bootstrap_invokes_helper_when_flag_on(
    flag_on: None,
) -> None:
    from sandbox.providers.daytona.lifecycle import _maybe_run_eager_runtime_bootstrap

    calls: list[dict[str, Any]] = []

    async def fake_helper(sandbox_id: str, workspace_root: str) -> None:
        calls.append(
            {
                "sandbox_id": sandbox_id,
                "workspace_root": workspace_root,
            }
        )

    with patch(
        "sandbox.providers.daytona.lifecycle.bootstrap_in_sandbox_runtime",
        new=fake_helper,
    ):
        _maybe_run_eager_runtime_bootstrap(_make_raw_sandbox("/ws"), "sb-1")

    assert len(calls) == 1
    assert calls[0]["sandbox_id"] == "sb-1"
    assert calls[0]["workspace_root"] == "/ws"


def test_maybe_bootstrap_propagates_runtime_upload_error(flag_on: None) -> None:
    from sandbox.providers.daytona.lifecycle import _maybe_run_eager_runtime_bootstrap

    async def fake_helper(*_: Any, **__: Any) -> None:
        raise RuntimeError("runtime crashed")

    with patch(
        "sandbox.providers.daytona.lifecycle.bootstrap_in_sandbox_runtime",
        new=fake_helper,
    ), pytest.raises(RuntimeError, match="runtime crashed"):
        _maybe_run_eager_runtime_bootstrap(_make_raw_sandbox("/ws"), "sb-1")


# ---------------------------------------------------------------------------
# bootstrap_upload_runtime_bundle (background-upload phase)
# ---------------------------------------------------------------------------


def test_upload_helper_noop_when_flag_off(flag_off: None) -> None:
    from sandbox.lifecycle.workspace import bootstrap_upload_runtime_bundle

    asyncio.run(
        bootstrap_upload_runtime_bundle(
            sandbox_id="sb-1",
            workspace_root="/ws",
        )
    )


def test_upload_helper_noop_on_missing_inputs(flag_on: None) -> None:
    from sandbox.lifecycle.workspace import bootstrap_upload_runtime_bundle

    for missing in ({"sandbox_id": ""}, {"workspace_root": ""}):
        kwargs: dict[str, Any] = {
            "sandbox_id": "sb-1",
            "workspace_root": "/ws",
        }
        kwargs.update(missing)
        asyncio.run(bootstrap_upload_runtime_bundle(**kwargs))


def test_upload_helper_uploads_without_running_lifecycle_bootstrap(
    flag_on: None,
) -> None:
    """Background upload runs ensure_runtime_uploaded directly."""
    from sandbox.lifecycle.workspace import bootstrap_upload_runtime_bundle

    upload_calls: list[str] = []

    async def fake_upload(sandbox_id: str) -> str:
        upload_calls.append(sandbox_id)
        return "deadbeef"

    with patch(
        "sandbox.control.daemon.bundle.ensure_runtime_uploaded",
        new=fake_upload,
    ):
        asyncio.run(
            bootstrap_upload_runtime_bundle(
                sandbox_id="sb-1",
                workspace_root="/ws",
            )
        )

    assert upload_calls == ["sb-1"]


# ---------------------------------------------------------------------------
# _maybe_start_eager_runtime_bundle_upload / _finish_eager_runtime_bundle_upload
# ---------------------------------------------------------------------------


def test_start_upload_returns_none_when_flag_off(flag_off: None) -> None:
    from sandbox.providers.daytona.lifecycle import _maybe_start_eager_runtime_bundle_upload

    assert (
        _maybe_start_eager_runtime_bundle_upload(_make_raw_sandbox("/ws"), "sb-1")
        is None
    )


def test_start_upload_returns_none_when_workspace_missing(flag_on: None) -> None:
    from sandbox.providers.daytona.lifecycle import _maybe_start_eager_runtime_bundle_upload

    assert _maybe_start_eager_runtime_bundle_upload(_make_raw_sandbox(None), "sb-1") is None


def test_start_upload_submits_future_and_invokes_helper(flag_on: None) -> None:
    """Future resolves successfully when the background upload completes."""
    import threading

    from sandbox.providers.daytona.lifecycle import (
        _finish_eager_runtime_bundle_upload,
        _maybe_start_eager_runtime_bundle_upload,
    )

    helper_done = threading.Event()
    helper_args: dict[str, Any] = {}

    async def fake_helper(sandbox_id: str, workspace_root: str) -> None:
        helper_args.update(
            sandbox_id=sandbox_id,
            workspace_root=workspace_root,
        )
        helper_done.set()

    with patch(
        "sandbox.providers.daytona.lifecycle.bootstrap_upload_runtime_bundle",
        new=fake_helper,
    ):
        future = _maybe_start_eager_runtime_bundle_upload(_make_raw_sandbox("/ws"), "sb-1")
        assert future is not None
        # Caller drains the future; success path must not raise.
        _finish_eager_runtime_bundle_upload(future, "sb-1")

    assert helper_done.is_set()
    assert helper_args == {
        "sandbox_id": "sb-1",
        "workspace_root": "/ws",
    }


def test_finish_upload_swallows_helper_failure(flag_on: None) -> None:
    """Background failure must not propagate — sequential bootstrap retries."""
    from sandbox.providers.daytona.lifecycle import (
        _finish_eager_runtime_bundle_upload,
        _maybe_start_eager_runtime_bundle_upload,
    )

    async def boom(*_: Any, **__: Any) -> None:
        raise RuntimeError("upload exploded")

    with patch(
        "sandbox.providers.daytona.lifecycle.bootstrap_upload_runtime_bundle",
        new=boom,
    ):
        future = _maybe_start_eager_runtime_bundle_upload(_make_raw_sandbox("/ws"), "sb-1")
        assert future is not None
        _finish_eager_runtime_bundle_upload(future, "sb-1")  # MUST NOT raise


def test_finish_upload_noop_when_future_none() -> None:
    from sandbox.providers.daytona.lifecycle import _finish_eager_runtime_bundle_upload

    _finish_eager_runtime_bundle_upload(None, "sb-1")  # MUST NOT raise
