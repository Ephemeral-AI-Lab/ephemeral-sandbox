"""Unit tests for mandatory in-sandbox runtime bootstrap helpers."""

from __future__ import annotations

import asyncio
from typing import Any
from unittest.mock import patch

import pytest


def test_bootstrap_helper_uploads_by_sandbox_id() -> None:
    from sandbox.control.ops.setup import bootstrap_in_sandbox_runtime

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


def test_bootstrap_helper_noop_when_workspace_empty() -> None:
    from sandbox.control.ops.setup import bootstrap_in_sandbox_runtime

    asyncio.run(
        bootstrap_in_sandbox_runtime(
            sandbox_id="sb-1",
            workspace_root="",
        )
    )


def test_bootstrap_helper_raises_on_runtime_upload_failure() -> None:
    from sandbox.control.ops.setup import bootstrap_in_sandbox_runtime

    async def fail_upload(*_: Any, **__: Any) -> str:
        raise RuntimeError("runtime unavailable")

    with patch(
        "sandbox.control.daemon.bundle.ensure_runtime_uploaded",
        new=fail_upload,
    ), pytest.raises(RuntimeError, match="runtime unavailable"):
        asyncio.run(
            bootstrap_in_sandbox_runtime(
                sandbox_id="sb-1",
                workspace_root="/ws",
            )
        )


def test_run_runtime_bootstrap_skips_when_workspace_unresolvable() -> None:
    from sandbox.control.ops.setup import run_runtime_bootstrap

    sentinel_called = {"called": False}

    async def boom(*_: Any, **__: Any) -> None:
        sentinel_called["called"] = True

    with patch(
        "sandbox.control.ops.setup.bootstrap_in_sandbox_runtime",
        new=boom,
    ):
        run_runtime_bootstrap("sb-1", None)
    assert sentinel_called["called"] is False


def test_run_runtime_bootstrap_invokes_helper() -> None:
    from sandbox.control.ops.setup import run_runtime_bootstrap

    calls: list[dict[str, Any]] = []

    async def fake_helper(sandbox_id: str, workspace_root: str) -> None:
        calls.append(
            {
                "sandbox_id": sandbox_id,
                "workspace_root": workspace_root,
            }
        )

    with patch(
        "sandbox.control.ops.setup.bootstrap_in_sandbox_runtime",
        new=fake_helper,
    ):
        run_runtime_bootstrap("sb-1", "/ws")

    assert calls == [{"sandbox_id": "sb-1", "workspace_root": "/ws"}]


def test_run_runtime_bootstrap_propagates_runtime_upload_error() -> None:
    from sandbox.control.ops.setup import run_runtime_bootstrap

    async def fake_helper(*_: Any, **__: Any) -> None:
        raise RuntimeError("runtime crashed")

    with patch(
        "sandbox.control.ops.setup.bootstrap_in_sandbox_runtime",
        new=fake_helper,
    ), pytest.raises(RuntimeError, match="runtime crashed"):
        run_runtime_bootstrap("sb-1", "/ws")


def test_ensure_workspace_base_skips_when_workspace_missing() -> None:
    from sandbox.control.ops.setup import ensure_workspace_base

    with patch("sandbox.api.tool._runtime.call_runtime_api") as call:
        ensure_workspace_base("sb-1", None)

    call.assert_not_called()


def test_ensure_workspace_base_invokes_runtime_op() -> None:
    from sandbox.control.ops.setup import ensure_workspace_base

    calls: list[dict[str, Any]] = []

    async def fake_call_runtime_api(
        sandbox_id: str,
        op: str,
        args: dict[str, Any],
        *,
        timeout: int,
    ) -> dict[str, Any]:
        calls.append(
            {
                "sandbox_id": sandbox_id,
                "op": op,
                "args": args,
                "timeout": timeout,
            }
        )
        return {"success": True}

    with patch("sandbox.api.tool._runtime.call_runtime_api", new=fake_call_runtime_api):
        ensure_workspace_base("sb-1", "/testbed")

    assert calls == [
        {
            "sandbox_id": "sb-1",
            "op": "api.ensure_workspace_base",
            "args": {"workspace_root": "/testbed"},
            "timeout": 180,
        }
    ]


def test_upload_helper_noop_on_missing_inputs() -> None:
    from sandbox.control.ops.setup import bootstrap_upload_runtime_bundle

    for missing in ({"sandbox_id": ""}, {"workspace_root": ""}):
        kwargs: dict[str, Any] = {
            "sandbox_id": "sb-1",
            "workspace_root": "/ws",
        }
        kwargs.update(missing)
        asyncio.run(bootstrap_upload_runtime_bundle(**kwargs))


def test_upload_helper_uploads_without_running_lifecycle_bootstrap() -> None:
    """Background upload runs ensure_runtime_uploaded directly."""
    from sandbox.control.ops.setup import bootstrap_upload_runtime_bundle

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


def test_start_upload_returns_none_when_workspace_missing() -> None:
    from sandbox.control.ops.setup import start_runtime_bundle_upload

    assert start_runtime_bundle_upload("sb-1", None) is None


def test_start_upload_submits_future_and_invokes_helper() -> None:
    """Future resolves successfully when the background upload completes."""
    import threading

    from sandbox.control.ops.setup import (
        finish_runtime_bundle_upload,
        start_runtime_bundle_upload,
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
        "sandbox.control.ops.setup.bootstrap_upload_runtime_bundle",
        new=fake_helper,
    ):
        future = start_runtime_bundle_upload("sb-1", "/ws")
        assert future is not None
        finish_runtime_bundle_upload(future, "sb-1")

    assert helper_done.is_set()
    assert helper_args == {
        "sandbox_id": "sb-1",
        "workspace_root": "/ws",
    }


def test_finish_upload_swallows_helper_failure() -> None:
    """Background failure must not propagate because sequential bootstrap retries."""
    from sandbox.control.ops.setup import (
        finish_runtime_bundle_upload,
        start_runtime_bundle_upload,
    )

    async def boom(*_: Any, **__: Any) -> None:
        raise RuntimeError("upload exploded")

    with patch(
        "sandbox.control.ops.setup.bootstrap_upload_runtime_bundle",
        new=boom,
    ):
        future = start_runtime_bundle_upload("sb-1", "/ws")
        assert future is not None
        finish_runtime_bundle_upload(future, "sb-1")


def test_finish_upload_noop_when_future_none() -> None:
    from sandbox.control.ops.setup import finish_runtime_bundle_upload

    finish_runtime_bundle_upload(None, "sb-1")
