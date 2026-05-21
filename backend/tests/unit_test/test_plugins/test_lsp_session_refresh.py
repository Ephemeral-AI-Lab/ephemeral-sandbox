"""Unit tests for LSP Pyright session freshness behavior."""

from __future__ import annotations

import asyncio
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from plugins.catalog.lsp.runtime import server as lsp_server
from plugins.catalog.lsp.runtime import pyright_session as pyright_session_module
from plugins.catalog.lsp.runtime import session_manager
from plugins.catalog.lsp.runtime.pyright_session import PyrightSession


@pytest.fixture(autouse=True)
def _clear_session_manager_cache() -> Iterator[None]:
    session_manager._sessions.clear()
    session_manager._locks.clear()
    for task in session_manager._event_tasks.values():
        task.cancel()
    session_manager._event_tasks.clear()
    session_manager._event_subscriptions.clear()
    yield
    session_manager._sessions.clear()
    session_manager._locks.clear()
    for task in session_manager._event_tasks.values():
        task.cancel()
    session_manager._event_tasks.clear()
    session_manager._event_subscriptions.clear()


class _Overlay:
    def __init__(self, *, workspace_root: str, manifest_key: str) -> None:
        self.workspace_root = workspace_root
        self.manifest_key = manifest_key
        self.ensure_count = 0
        self.reasons: list[str] = []

    async def ensure_current(self, *, reason: str = "ensure_current") -> str:
        self.ensure_count += 1
        self.reasons.append(reason)
        return self.manifest_key

    def active_manifest_key(self) -> str:
        return self.manifest_key


class _OperationOverlay(_Overlay):
    def __init__(self, *, workspace_root: str, manifest_key: str) -> None:
        super().__init__(workspace_root=workspace_root, manifest_key=manifest_key)
        self.handles: list[_OverlayHandle] = []

    def acquire_operation_overlay(
        self,
        *,
        request_id: str,
        workspace_root: str | None = None,
        materialize: bool = False,
    ) -> "_OverlayHandle":
        handle = _OverlayHandle(
            request_id=request_id,
            workspace_root=workspace_root or self.workspace_root,
            manifest_key=self.manifest_key,
            materialize=materialize,
        )
        self.handles.append(handle)
        return handle


class _OverlayHandle:
    def __init__(
        self,
        *,
        request_id: str,
        workspace_root: str,
        manifest_key: str,
        materialize: bool,
    ) -> None:
        self.request_id = request_id
        self.workspace_root = workspace_root
        self.manifest_key = manifest_key
        self.materialize = materialize
        self.manifest_version = int(manifest_key.rsplit("@", 1)[-1])
        self.root_hash = manifest_key.rsplit("@", 1)[0]
        self.lowerdir = None if not materialize else f"/tmp/{self.root_hash}/lower"
        self.layer_paths = None if materialize else ("/layers/L1",)
        self.run_dir = "/tmp/run"
        self.upperdir = "/tmp/run/upper"
        self.workdir = "/tmp/run/work"
        self.released = False

    def release(self) -> None:
        self.released = True


@dataclass(frozen=True)
class _Caller:
    agent_run_id: str = "run"
    agent_id: str = "agent"


@dataclass(frozen=True)
class _Ctx:
    layer_stack_root: str
    overlay: _Overlay
    caller: _Caller = _Caller()
    metadata: dict[str, Any] | None = None


class _FakeSession:
    def __init__(
        self,
        *,
        manifest_key: str,
        workspace_root: str,
        **_kwargs: Any,
    ) -> None:
        self.manifest_key = manifest_key
        self.workspace_root = workspace_root
        self.overlay_handle = _kwargs.get("overlay_handle")
        self._overlay_handle = self.overlay_handle
        self.refresh_count = 0
        self.evict_count = 0

    async def refresh_manifest(
        self,
        *,
        manifest_key: str,
        overlay_handle: Any | None = None,
        workspace_root: str | None = None,
    ) -> None:
        old_handle = self._overlay_handle
        self.refresh_count += 1
        self.manifest_key = manifest_key
        if workspace_root is not None:
            self.workspace_root = workspace_root
        if overlay_handle is not None:
            self.overlay_handle = overlay_handle
            self._overlay_handle = overlay_handle
            if old_handle is not None and old_handle is not overlay_handle:
                release = getattr(old_handle, "release", None)
                if callable(release):
                    release()

    async def evict(self) -> None:
        self.evict_count += 1
        release = getattr(self._overlay_handle, "release", None)
        if callable(release):
            release()


class _StartableFakeSession(_FakeSession):
    def __init__(
        self,
        *,
        manifest_key: str,
        workspace_root: str,
        **kwargs: Any,
    ) -> None:
        super().__init__(
            manifest_key=manifest_key,
            workspace_root=workspace_root,
            **kwargs,
        )
        self.start_count = 0

    async def start(self) -> None:
        self.start_count += 1


class _SlowStartFakeSession(_StartableFakeSession):
    async def start(self) -> None:
        self.start_count += 1
        await asyncio.sleep(3600)


@pytest.mark.asyncio
async def test_session_manager_ensures_overlay_current_on_every_tool_call(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    monkeypatch.setattr(session_manager, "PyrightSession", _FakeSession)

    overlay = _Overlay(workspace_root="/testbed", manifest_key="hash-a@1")
    ctx = _Ctx(
        layer_stack_root=str(tmp_path / "layer-stack"),
        overlay=overlay,
        metadata={"op_name": "hover"},
    )

    session = await session_manager.get_session(ctx)
    overlay.manifest_key = "hash-b@2"
    refreshed = await session_manager.get_session(ctx)

    assert refreshed is session
    assert refreshed.manifest_key == "hash-b@2"
    assert refreshed.refresh_count == 1
    assert overlay.ensure_count == 2
    assert overlay.reasons == ["lsp:hover:enter", "lsp:hover:enter"]


@pytest.mark.asyncio
async def test_session_manager_restarts_when_workspace_root_changes(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    monkeypatch.setattr(session_manager, "PyrightSession", _FakeSession)

    first_overlay = _Overlay(workspace_root="/testbed", manifest_key="hash-a@1")
    second_overlay = _Overlay(workspace_root="/workspace", manifest_key="hash-a@1")
    first = await session_manager.get_session(
        _Ctx(layer_stack_root=str(tmp_path / "stack"), overlay=first_overlay)
    )
    second = await session_manager.get_session(
        _Ctx(layer_stack_root=str(tmp_path / "stack"), overlay=second_overlay)
    )

    assert second is not first
    assert first.evict_count == 1
    assert second.workspace_root == "/workspace"


@pytest.mark.asyncio
async def test_session_manager_uses_daemon_operation_overlay_for_lsp_session(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    monkeypatch.setattr(session_manager, "PyrightSession", _FakeSession)
    monkeypatch.setattr(session_manager, "_overlay_namespace_available", lambda: True)

    overlay = _OperationOverlay(workspace_root="/testbed", manifest_key="hash-a@1")
    session = await session_manager.get_session(
        _Ctx(
            layer_stack_root=str(tmp_path / "stack"),
            overlay=overlay,
            metadata={"op_name": "hover"},
        )
    )

    assert session.workspace_root == "/testbed"
    assert session.overlay_handle is overlay.handles[0]
    assert overlay.handles[0].materialize is False
    assert overlay.handles[0].layer_paths == ("/layers/L1",)


@pytest.mark.asyncio
async def test_session_manager_refreshes_owned_overlay_without_restart(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    monkeypatch.setattr(session_manager, "PyrightSession", _FakeSession)
    monkeypatch.setattr(session_manager, "_overlay_namespace_available", lambda: True)

    overlay = _OperationOverlay(workspace_root="/testbed", manifest_key="hash-a@1")
    ctx = _Ctx(
        layer_stack_root=str(tmp_path / "stack"),
        overlay=overlay,
        metadata={"op_name": "hover"},
    )

    session = await session_manager.get_session(ctx)
    first_handle = overlay.handles[0]
    overlay.manifest_key = "hash-b@2"
    refreshed = await session_manager.get_session(ctx)

    assert refreshed is session
    assert session.evict_count == 0
    assert session.refresh_count == 1
    assert session.manifest_key == "hash-b@2"
    assert session.overlay_handle is overlay.handles[1]
    assert first_handle.released is True
    assert overlay.handles[1].released is False


@pytest.mark.asyncio
async def test_lsp_runtime_warm_hook_starts_cached_session(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    monkeypatch.setattr(session_manager, "PyrightSession", _StartableFakeSession)

    overlay = _Overlay(workspace_root="/testbed", manifest_key="hash-a@1")
    ctx = _Ctx(
        layer_stack_root=str(tmp_path / "layer-stack"),
        overlay=overlay,
        metadata={"op_name": "__warm__"},
    )

    result = await lsp_server.warm_plugin_runtime({}, ctx)
    session = await session_manager.get_session(ctx)

    assert result == {"success": True, "manifest_key": "hash-a@1"}
    assert isinstance(session, _StartableFakeSession)
    assert session.start_count == 1


@pytest.mark.asyncio
async def test_lsp_runtime_warm_hook_defers_slow_start(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    monkeypatch.setattr(session_manager, "PyrightSession", _SlowStartFakeSession)
    monkeypatch.setenv("EOS_LSP_WARM_START_TIMEOUT_S", "0.1")

    overlay = _Overlay(workspace_root="/testbed", manifest_key="hash-a@1")
    ctx = _Ctx(
        layer_stack_root=str(tmp_path / "layer-stack"),
        overlay=overlay,
        metadata={"op_name": "__warm__"},
    )

    result = await lsp_server.warm_plugin_runtime({}, ctx)
    session = await session_manager.get_session(ctx)

    assert result == {
        "success": True,
        "manifest_key": "hash-a@1",
        "runtime_start_timeout_s": 0.1,
        "runtime_start_deferred": True,
    }
    assert isinstance(session, _SlowStartFakeSession)
    assert session.start_count == 1


class _Client:
    def __init__(self) -> None:
        self.notifications: list[tuple[str, dict[str, Any]]] = []

    async def notify(self, method: str, params: dict[str, Any]) -> None:
        self.notifications.append((method, params))


class _HangingClient(_Client):
    def __init__(self) -> None:
        super().__init__()
        self.closed = False

    async def request(self, method: str, params: dict[str, Any]) -> Any:
        del method, params
        await asyncio.sleep(3600)

    async def close(self) -> None:
        self.closed = True


@pytest.mark.asyncio
async def test_pyright_session_overlay_namespace_reads_open_text_from_layers(
    tmp_path: Path,
) -> None:
    layer = tmp_path / "layers" / "L1"
    module = layer / "pkg" / "mod.py"
    module.parent.mkdir(parents=True)
    module.write_text("class Schedule:\n    pass\n", encoding="utf-8")
    session = PyrightSession(
        manifest_key="hash-a@1",
        workspace_root="/testbed",
        overlay_handle=SimpleNamespace(layer_paths=(layer.as_posix(),), lowerdir=None),
    )
    client = _Client()
    session._client = client  # type: ignore[assignment]
    session._started = True

    uri = await session._open_document("pkg/mod.py")

    assert uri == "file:///testbed/pkg/mod.py"
    assert client.notifications[0][1]["textDocument"]["text"] == (
        "class Schedule:\n    pass\n"
    )
    assert session._fallback_document_symbols("pkg/mod.py", "Schedule")[0][
        "name"
    ] == "Schedule"


@pytest.mark.asyncio
async def test_pyright_session_refresh_notifies_open_docs(tmp_path: Path) -> None:
    workspace = tmp_path / "testbed"
    (workspace / "pkg").mkdir(parents=True)
    module = workspace / "pkg" / "mod.py"
    module.write_text("value = 1\n", encoding="utf-8")
    session = PyrightSession(
        manifest_key="hash-a@1",
        workspace_root=str(workspace),
    )
    client = _Client()
    session._client = client  # type: ignore[assignment]
    session._started = True
    uri = await session._open_document("pkg/mod.py")
    client.notifications.clear()

    module.write_text("value = 2\n", encoding="utf-8")
    await session.refresh_manifest(manifest_key="hash-b@2")

    assert session.manifest_key == "hash-b@2"
    assert ("workspace/didChangeWatchedFiles", {"changes": [{"uri": session._workspace_uri(), "type": 2}]}) in client.notifications
    did_change = [
        params
        for method, params in client.notifications
        if method == "textDocument/didChange"
    ]
    assert did_change
    assert did_change[-1]["contentChanges"] == [{"text": "value = 2\n"}]
    assert uri in session._opened


@pytest.mark.asyncio
async def test_pyright_session_refresh_remounts_private_namespace(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    old_layer = tmp_path / "old" / "L1"
    new_layer = tmp_path / "new" / "L2"
    old_module = old_layer / "pkg" / "mod.py"
    new_module = new_layer / "pkg" / "mod.py"
    old_module.parent.mkdir(parents=True)
    new_module.parent.mkdir(parents=True)
    old_module.write_text("value = 1\n", encoding="utf-8")
    new_module.write_text("value = 2\n", encoding="utf-8")
    old_handle = _OverlayHandle(
        request_id="lsp-session:hover",
        workspace_root="/testbed",
        manifest_key="hash-a@1",
        materialize=False,
    )
    old_handle.layer_paths = (old_layer.as_posix(),)
    new_handle = _OverlayHandle(
        request_id="lsp-session:hover",
        workspace_root="/testbed",
        manifest_key="hash-b@2",
        materialize=False,
    )
    new_handle.layer_paths = (new_layer.as_posix(),)
    new_handle.run_dir = (tmp_path / "run").as_posix()
    new_handle.upperdir = (tmp_path / "run" / "upper").as_posix()
    new_handle.workdir = (tmp_path / "run" / "work").as_posix()
    calls: list[tuple[tuple[Any, ...], dict[str, Any]]] = []

    class _Proc:
        returncode = 0

        async def communicate(self) -> tuple[bytes, bytes]:
            return b"", b""

    async def _create_subprocess_exec(
        *args: Any,
        **kwargs: Any,
    ) -> _Proc:
        calls.append((args, kwargs))
        return _Proc()

    monkeypatch.setattr(
        pyright_session_module.asyncio,
        "create_subprocess_exec",
        _create_subprocess_exec,
    )
    monkeypatch.setattr(
        pyright_session_module.shutil,
        "which",
        lambda name: "/usr/bin/nsenter" if name == "nsenter" else None,
    )
    session = PyrightSession(
        manifest_key="hash-a@1",
        workspace_root="/testbed",
        overlay_handle=old_handle,
    )
    client = _Client()
    session._client = client  # type: ignore[assignment]
    session._proc = SimpleNamespace(pid=4321)  # type: ignore[assignment]
    session._started = True
    uri = await session._open_document("pkg/mod.py")
    client.notifications.clear()

    await session.refresh_manifest(
        manifest_key="hash-b@2",
        overlay_handle=new_handle,
        workspace_root="/testbed",
    )

    assert session.manifest_key == "hash-b@2"
    assert session._overlay_handle is new_handle
    assert session._overlay_layer_paths == (new_layer.as_posix(),)
    assert old_handle.released is True
    assert new_handle.released is False
    assert calls
    argv = calls[0][0]
    assert argv[:6] == (
        "/usr/bin/nsenter",
        "-t",
        "4321",
        "-U",
        "-m",
        "--preserve-credentials",
    )
    assert "plugins.catalog.lsp.runtime.namespace_remount" in argv
    assert (
        "workspace/didChangeWatchedFiles",
        {"changes": [{"uri": session._workspace_uri(), "type": 2}]},
    ) in client.notifications
    did_change = [
        params
        for method, params in client.notifications
        if method == "textDocument/didChange"
    ]
    assert did_change[-1]["contentChanges"] == [{"text": "value = 2\n"}]
    assert uri in session._opened


@pytest.mark.asyncio
async def test_pyright_session_diagnostics_pulls_current_report(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    workspace = tmp_path / "testbed"
    (workspace / "pkg").mkdir(parents=True)
    (workspace / "pkg" / "mod.py").write_text("x = 1\n", encoding="utf-8")
    session = PyrightSession(
        manifest_key="hash-a@1",
        workspace_root=str(workspace),
    )
    session._client = _Client()  # type: ignore[assignment]
    session._started = True
    pulled = [
        {
            "message": "Operator '+' not supported",
            "range": {"start": {"line": 1, "character": 11}},
        }
    ]

    async def _send_request(
        method: str,
        params: dict[str, Any],
    ) -> dict[str, Any]:
        assert method == "textDocument/diagnostic"
        assert params["textDocument"]["uri"].endswith("/pkg/mod.py")
        return {"items": pulled, "kind": "full", "resultId": "1"}

    monkeypatch.setattr(session, "_send_request", _send_request)

    result = await session.diagnostics({"file_path": "pkg/mod.py"})

    assert result == {
        "diagnostics": pulled,
        "kind": "full",
        "result_id": "1",
    }
    assert session._to_uri("pkg/mod.py") in session._opened


@pytest.mark.asyncio
async def test_pyright_session_diagnostics_unchanged_reuses_cached_report(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    workspace = tmp_path / "testbed"
    (workspace / "pkg").mkdir(parents=True)
    (workspace / "pkg" / "mod.py").write_text("x = 1\n", encoding="utf-8")
    session = PyrightSession(
        manifest_key="hash-a@1",
        workspace_root=str(workspace),
    )
    session._client = _Client()  # type: ignore[assignment]
    session._started = True
    pulled = [
        {
            "message": "Operator '+' not supported",
            "range": {"start": {"line": 1, "character": 11}},
        }
    ]
    responses: list[dict[str, Any]] = [
        {"items": pulled, "kind": "full", "resultId": "1"},
        {"kind": "unchanged", "resultId": "1"},
    ]

    async def _send_request(
        method: str,
        params: dict[str, Any],
    ) -> dict[str, Any]:
        assert method == "textDocument/diagnostic"
        assert params["textDocument"]["uri"].endswith("/pkg/mod.py")
        return responses.pop(0)

    monkeypatch.setattr(session, "_send_request", _send_request)

    first = await session.diagnostics({"file_path": "pkg/mod.py"})
    second = await session.diagnostics({"file_path": "pkg/mod.py"})

    assert first["diagnostics"] == pulled
    assert second == {
        "diagnostics": pulled,
        "kind": "unchanged",
        "result_id": "1",
    }


@pytest.mark.asyncio
async def test_pyright_query_symbols_falls_back_to_ast_for_empty_document_symbols(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    workspace = tmp_path / "testbed"
    (workspace / "pkg").mkdir(parents=True)
    (workspace / "pkg" / "priority.py").write_text(
        "from enum import IntEnum\n\nclass Priority(IntEnum):\n    LOW = 0\n",
        encoding="utf-8",
    )
    session = PyrightSession(
        manifest_key="hash-a@1",
        workspace_root=str(workspace),
    )
    session._client = _Client()  # type: ignore[assignment]
    session._started = True

    async def _send_request(
        method: str,
        params: dict[str, Any],
    ) -> list[dict[str, Any]]:
        assert method == "textDocument/documentSymbol"
        assert params["textDocument"]["uri"].endswith("/pkg/priority.py")
        return []

    monkeypatch.setattr(session, "_send_request", _send_request)

    result = await session.query_symbols(
        {"file_path": "pkg/priority.py", "query": "Priority"}
    )

    assert result["symbols"][0]["name"] == "Priority"
    assert result["symbols"][0]["uri"].endswith("/pkg/priority.py")
    assert result["symbols"][0]["location"]["range"]["start"]["line"] == 2


@pytest.mark.asyncio
async def test_pyright_find_references_timeout_returns_empty_list(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    workspace = tmp_path / "testbed"
    (workspace / "pkg").mkdir(parents=True)
    (workspace / "pkg" / "mod.py").write_text("value = 1\n", encoding="utf-8")
    session = PyrightSession(
        manifest_key="hash-a@1",
        workspace_root=str(workspace),
    )
    client = _HangingClient()
    session._client = client  # type: ignore[assignment]
    session._started = True
    monkeypatch.setattr(
        "plugins.catalog.lsp.runtime.pyright_session._REFERENCES_TIMEOUT_S",
        0.1,
    )

    result = await session.find_references(
        {
            "file_path": "pkg/mod.py",
            "line": 0,
            "character": 0,
            "include_declaration": True,
        }
    )

    assert result == {"references": [], "timeout": True}
    assert client.closed is True
