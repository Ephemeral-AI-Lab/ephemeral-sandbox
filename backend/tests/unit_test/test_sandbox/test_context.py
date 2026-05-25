"""Tests for Daytona sandbox execution-context preparation."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from sandbox.provider.daytona.exec_context import DaytonaContextPreparer
from tools._framework.core.base import ToolExecutionContextService


def _ctx(services=None) -> ToolExecutionContextService:
    return ToolExecutionContextService(cwd=Path("/tmp"), services=services or {})


def test_sandbox_exports_context_preparer() -> None:
    from sandbox.provider.daytona.exec_context import DaytonaContextPreparer as DCP

    assert DCP is DaytonaContextPreparer


def test_sandbox_api_context_preparer_uses_registered_adapter(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import sandbox.api as sandbox_api
    import sandbox.api.provider_control as provider_control

    sentinel = object()

    class Adapter:
        def context_preparer(self, sandbox_id: str) -> object:
            assert sandbox_id == "sb-test123"
            return sentinel

    monkeypatch.setattr(
        provider_control, "get_adapter", lambda _sandbox_id: Adapter()
    )

    assert sandbox_api.context_preparer_for("sb-test123") is sentinel


def test_sandbox_api_context_preparer_requires_provider_hook(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import sandbox.api as sandbox_api
    import sandbox.api.provider_control as provider_control

    monkeypatch.setattr(
        provider_control, "get_adapter", lambda _sandbox_id: object()
    )

    with pytest.raises(RuntimeError, match="does not expose context_preparer"):
        sandbox_api.context_preparer_for("sb-test123")


def test_context_preparer_instantiation() -> None:
    preparer = DaytonaContextPreparer(sandbox_id="sb-test123")
    assert preparer.sandbox_id == "sb-test123"


def test_get_sandbox_no_id_raises() -> None:
    tk = DaytonaContextPreparer("")
    with pytest.raises(RuntimeError, match="No sandbox_id"):
        tk._get_sandbox()


def test_get_sandbox_caches_instance() -> None:
    tk = DaytonaContextPreparer(sandbox_id="sb-abc")
    fake_sb = MagicMock()
    with patch(
        "sandbox.provider.daytona.exec_context.DaytonaContextPreparer._get_sandbox"
    ) as mock_get:
        mock_get.return_value = fake_sb
        result = tk._get_sandbox()
        assert result is fake_sb


def test_get_sandbox_refetches_sync_sandbox(monkeypatch: pytest.MonkeyPatch) -> None:
    tk = DaytonaContextPreparer(sandbox_id="sb-abc")
    first = MagicMock()
    second = MagicMock()
    calls: list[str] = []

    def fetch_sandbox(sandbox_id: str):
        calls.append(sandbox_id)
        return first if len(calls) == 1 else second

    monkeypatch.setattr(
        "sandbox.provider.daytona.client.fetch_sandbox",
        fetch_sandbox,
    )

    assert tk._get_sandbox() is first
    assert tk._get_sandbox() is second
    assert calls == ["sb-abc", "sb-abc"]


async def test_get_sandbox_async_no_id_raises() -> None:
    tk = DaytonaContextPreparer("")
    with pytest.raises(RuntimeError, match="No sandbox_id"):
        await tk._get_sandbox_async()


async def test_get_sandbox_async_caches_per_loop() -> None:
    tk = DaytonaContextPreparer(sandbox_id="sb-xyz")
    fake_sb = MagicMock()

    async def fake_get_async(_sandbox_id):
        return fake_sb

    with patch(
        "sandbox.provider.daytona.client.get_async_sandbox",
        new=fake_get_async,
        create=True,
    ):
        result = await tk._get_sandbox_async()
        assert result is fake_sb
        result2 = await tk._get_sandbox_async()
        assert result2 is fake_sb


async def test_get_sandbox_async_invalidates_on_new_loop() -> None:
    tk = DaytonaContextPreparer(sandbox_id="sb-xyz")
    old_sb = MagicMock()
    tk._sandbox = old_sb
    tk._sandbox_loop_id = 999999
    new_sb = MagicMock()

    async def fake_get_async(_sandbox_id):
        return new_sb

    with patch(
        "sandbox.provider.daytona.client.get_async_sandbox",
        new=fake_get_async,
        create=True,
    ):
        result = await tk._get_sandbox_async()
        assert result is new_sb


def test_prepare_context_injects_workspace_metadata() -> None:
    tk = DaytonaContextPreparer(sandbox_id="sb-test")
    fake_sb = MagicMock()
    ctx = _ctx()

    with (
        patch.object(tk, "_get_sandbox", return_value=fake_sb),
        patch("sandbox.provider.daytona.exec_context.discover_workspace", return_value="/workspace"),
        patch("sandbox.provider.daytona.exec_context.has_registered_adapter", return_value=True),
    ):
        tk.prepare_context(ctx)

    assert "daytona_sandbox" not in ctx
    assert ctx["repo_root"] == "/workspace"
    assert ctx["exec_cwd"] == "/workspace"


def test_prepare_context_no_cwd_skips_metadata_key() -> None:
    tk = DaytonaContextPreparer(sandbox_id="sb-test")
    fake_sb = MagicMock()
    ctx = _ctx()

    with (
        patch.object(tk, "_get_sandbox", return_value=fake_sb),
        patch("sandbox.provider.daytona.exec_context.discover_workspace", return_value=None),
        patch("sandbox.provider.daytona.exec_context.has_registered_adapter", return_value=True),
    ):
        tk.prepare_context(ctx)

    assert "daytona_sandbox" not in ctx
    assert "repo_root" not in ctx
    assert "exec_cwd" not in ctx


def test_prepare_context_respects_preseeded_workspace_root_override() -> None:
    tk = DaytonaContextPreparer(sandbox_id="sb-test")
    fake_sb = MagicMock()
    ctx = _ctx({"repo_root": "/testbed"})

    with (
        patch.object(tk, "_get_sandbox", return_value=fake_sb),
        patch(
            "sandbox.provider.daytona.exec_context.discover_workspace",
            return_value="/workspace",
        ) as discover_mock,
        patch("sandbox.provider.daytona.exec_context.has_registered_adapter", return_value=True),
    ):
        tk.prepare_context(ctx)

    discover_mock.assert_not_called()
    assert "daytona_sandbox" not in ctx
    assert ctx["repo_root"] == "/testbed"
    assert ctx["exec_cwd"] == "/testbed"


def test_daytona_runtime_context_registers_provider_adapter() -> None:
    from sandbox.provider.daytona.adapter import DaytonaProviderAdapter
    from sandbox.provider.daytona.exec_context import prepare_daytona_runtime_context
    from sandbox.provider.registry import dispose_adapter, get_adapter

    sandbox_id = "daytona-context-provider-registration"
    dispose_adapter(sandbox_id)
    ctx = _ctx()
    fake_sb = MagicMock()

    prepare_daytona_runtime_context(
        ctx,
        sandbox_id=sandbox_id,
        sandbox=fake_sb,
        workspace_root="/workspace",
    )

    assert "daytona_sandbox" not in ctx
    assert ctx["repo_root"] == "/workspace"
    assert isinstance(get_adapter(sandbox_id), DaytonaProviderAdapter)
    dispose_adapter(sandbox_id)


async def test_prepare_context_async_injects_workspace_metadata() -> None:
    tk = DaytonaContextPreparer(sandbox_id="sb-test")
    fake_sb = MagicMock()
    ctx = _ctx()

    with (
        patch.object(tk, "_get_sandbox_async", new=AsyncMock(return_value=fake_sb)),
        patch(
            "sandbox.provider.daytona.exec_context.discover_workspace_async",
            new=AsyncMock(return_value="/async/workspace"),
        ),
        patch("sandbox.provider.daytona.exec_context.has_registered_adapter", return_value=True),
    ):
        await tk.prepare_context_async(ctx)

    assert "daytona_sandbox" not in ctx
    assert ctx["repo_root"] == "/async/workspace"
    assert ctx["exec_cwd"] == "/async/workspace"


async def test_prepare_context_async_no_cwd() -> None:
    tk = DaytonaContextPreparer(sandbox_id="sb-test")
    fake_sb = MagicMock()
    ctx = _ctx()

    with (
        patch.object(tk, "_get_sandbox_async", new=AsyncMock(return_value=fake_sb)),
        patch(
            "sandbox.provider.daytona.exec_context.discover_workspace_async",
            new=AsyncMock(return_value=None),
        ),
        patch("sandbox.provider.daytona.exec_context.has_registered_adapter", return_value=True),
    ):
        await tk.prepare_context_async(ctx)

    assert "daytona_sandbox" not in ctx
    assert "repo_root" not in ctx
    assert "exec_cwd" not in ctx


async def test_prepare_context_async_respects_preseeded_workspace_root_override() -> None:
    tk = DaytonaContextPreparer(sandbox_id="sb-test")
    fake_sb = MagicMock()
    ctx = _ctx({"repo_root": "/testbed"})

    with (
        patch.object(tk, "_get_sandbox_async", new=AsyncMock(return_value=fake_sb)),
        patch(
            "sandbox.provider.daytona.exec_context.discover_workspace_async",
            new=AsyncMock(return_value="/workspace"),
        ) as discover_mock,
        patch("sandbox.provider.daytona.exec_context.has_registered_adapter", return_value=True),
    ):
        await tk.prepare_context_async(ctx)

    discover_mock.assert_not_called()
    assert "daytona_sandbox" not in ctx
    assert ctx["repo_root"] == "/testbed"
    assert ctx["exec_cwd"] == "/testbed"


def test_daytona_context_preparer_has_no_instructions() -> None:
    tk = DaytonaContextPreparer("sb-test")
    assert not hasattr(tk, "instructions")
