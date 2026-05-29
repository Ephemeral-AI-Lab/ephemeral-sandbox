"""Unit tests for sandbox.ephemeral_workspace.plugin.host_dispatch.call_plugin."""

from __future__ import annotations

import asyncio
import json
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest

from plugins.core.manifest import PluginManifest, parse_plugin_manifest
from sandbox.ephemeral_workspace.plugin import host_dispatch as host_dispatch_mod
from sandbox.shared.models import Intent
from sandbox.ephemeral_workspace.plugin.host_dispatch import call_plugin, call_plugin_write
from tools._framework.core.context import ToolExecutionContextService


def _make_context(sandbox_id: str = "sb-1") -> ToolExecutionContextService:
    ctx = ToolExecutionContextService(cwd=Path("/tmp"))
    ctx["sandbox_id"] = sandbox_id
    ctx["repo_root"] = "/testbed"
    return ctx


def _seed_demo_manifest(tmp_path: Path) -> PluginManifest:
    plugin_dir = tmp_path / "demo"
    plugin_dir.mkdir()
    (plugin_dir / "plugin.md").write_text(
        "---\nname: demo\ndescription: demo\ntools:\n"
        "  - name: demo.run\n    module: tools/run.py\nsetup: setup.sh\n"
        "---\n",
        encoding="utf-8",
    )
    (plugin_dir / "tools").mkdir()
    (plugin_dir / "tools" / "run.py").write_text("x=1\n", encoding="utf-8")
    (plugin_dir / "setup.sh").write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    return parse_plugin_manifest(plugin_dir)


@pytest.fixture(autouse=True)
def _isolate_session() -> Iterator[None]:
    host_dispatch_mod.reset_host_dispatch_cache_for_tests()
    yield
    host_dispatch_mod.reset_host_dispatch_cache_for_tests()


def test_call_plugin_happy_path(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    manifest = _seed_demo_manifest(tmp_path)
    monkeypatch.setattr(
        host_dispatch_mod, "_PLUGIN_MANIFESTS_BY_NAME", {"demo": manifest}, raising=False
    )

    install_calls: list[str] = []

    async def fake_install(sandbox_id: str, m: PluginManifest) -> str:
        install_calls.append(sandbox_id)
        return "abc123"

    dispatch_calls: list[tuple[str, str, dict[str, Any]]] = []

    async def fake_dispatch(
        sandbox_id: str, op: str, args: dict[str, Any], **kwargs: Any
    ) -> dict[str, Any]:
        del kwargs
        dispatch_calls.append((sandbox_id, op, dict(args)))
        if op == "api.plugin.ensure":
            return {"success": True, "plugin": "demo", "registered_ops": []}
        return {"success": True, "plugin": "demo", "result": {"value": 42}}

    result = asyncio.run(
        call_plugin(
            _make_context(),
            plugin="demo",
            op="run",
            payload={"x": 1},
            install_runner=fake_install,
            daemon_dispatcher=fake_dispatch,
        )
    )

    assert not result.is_error
    decoded = json.loads(result.output)
    assert decoded["result"] == {"value": 42}
    assert install_calls == ["sb-1"]
    op_names = [op for _sb, op, _args in dispatch_calls]
    assert op_names == ["api.plugin.ensure", "plugin.demo.run"]
    assert dispatch_calls[0][2] == {
        "plugin": "demo",
        "digest": "abc123",
        "workspace_root": "/testbed",
    }
    assert dispatch_calls[1][2]["intent"] == Intent.READ_ONLY.value


def test_call_plugin_forwards_caller_audit_fields(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manifest = _seed_demo_manifest(tmp_path)
    monkeypatch.setattr(
        host_dispatch_mod, "_PLUGIN_MANIFESTS_BY_NAME", {"demo": manifest}, raising=False
    )
    ctx = _make_context()
    ctx["task_center_run_id"] = "run-1"
    ctx["task_center_task_id"] = "task-1"
    ctx["task_center_attempt_id"] = "attempt-1"
    ctx["task_center_workflow_id"] = "goal-1"
    ctx["task_center_request_id"] = "request-1"
    ctx["tool_use_id"] = "tool-1"
    dispatch_payloads: list[dict[str, Any]] = []

    async def fake_install(sandbox_id: str, m: PluginManifest) -> str:
        del sandbox_id, m
        return "abc123"

    async def fake_dispatch(
        sandbox_id: str, op: str, args: dict[str, Any], **kwargs: Any
    ) -> dict[str, Any]:
        del sandbox_id, kwargs
        if op != "api.plugin.ensure":
            dispatch_payloads.append(dict(args))
        return {"success": True}

    result = asyncio.run(
        call_plugin(
            ctx,
            plugin="demo",
            op="run",
            payload={},
            install_runner=fake_install,
            daemon_dispatcher=fake_dispatch,
        )
    )

    assert not result.is_error
    assert dispatch_payloads[0]["caller"] == {
        "agent_id": "",
        "run_id": "",
        "agent_run_id": "",
        "task_id": "",
        "task_center_run_id": "run-1",
        "task_center_task_id": "task-1",
        "task_center_attempt_id": "attempt-1",
        "task_center_workflow_id": "goal-1",
        "task_center_request_id": "request-1",
        "tool_id": "tool-1",
    }


def test_call_plugin_write_requires_write_intent(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    manifest = _seed_demo_manifest(tmp_path)
    monkeypatch.setattr(
        host_dispatch_mod, "_PLUGIN_MANIFESTS_BY_NAME", {"demo": manifest}, raising=False
    )

    async def never_called(*args: Any, **kwargs: Any) -> Any:
        raise AssertionError("write plugin dispatch should be blocked")

    result = asyncio.run(
        call_plugin_write(
            _make_context(),
            plugin="demo",
            op="run",
            payload={},
            install_runner=never_called,
            daemon_dispatcher=never_called,
        )
    )

    assert result.is_error
    assert result.metadata["step"] == "intent"


def test_call_plugin_write_forwards_write_intent(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    manifest = _seed_demo_manifest(tmp_path)
    monkeypatch.setattr(
        host_dispatch_mod, "_PLUGIN_MANIFESTS_BY_NAME", {"demo": manifest}, raising=False
    )
    ctx = _make_context()
    ctx["__intent"] = Intent.WRITE_ALLOWED
    dispatch_payloads: list[dict[str, Any]] = []

    async def fake_install(sandbox_id: str, m: PluginManifest) -> str:
        del sandbox_id, m
        return "abc123"

    async def fake_dispatch(
        sandbox_id: str, op: str, args: dict[str, Any], **kwargs: Any
    ) -> dict[str, Any]:
        del sandbox_id, kwargs
        if op != "api.plugin.ensure":
            dispatch_payloads.append(dict(args))
        return {"success": True}

    result = asyncio.run(
        call_plugin_write(
            ctx,
            plugin="demo",
            op="run",
            payload={"x": 1},
            install_runner=fake_install,
            daemon_dispatcher=fake_dispatch,
        )
    )

    assert not result.is_error
    assert dispatch_payloads == [
        {
            "x": 1,
            "caller": {
                "agent_id": "",
                "run_id": "",
                "agent_run_id": "",
                "task_id": "",
            },
            "workspace_root": "/testbed",
            "intent": Intent.WRITE_ALLOWED.value,
        }
    ]


def test_call_plugin_install_failure_surfaces_error(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    manifest = _seed_demo_manifest(tmp_path)
    monkeypatch.setattr(
        host_dispatch_mod, "_PLUGIN_MANIFESTS_BY_NAME", {"demo": manifest}, raising=False
    )

    async def boom_install(sandbox_id: str, m: PluginManifest) -> str:
        raise RuntimeError("install boom")

    async def never_dispatch(*args: Any, **kwargs: Any) -> dict[str, Any]:
        raise AssertionError("dispatch should not be reached after install fails")

    result = asyncio.run(
        call_plugin(
            _make_context(),
            plugin="demo",
            op="run",
            payload={},
            install_runner=boom_install,
            daemon_dispatcher=never_dispatch,
        )
    )

    assert result.is_error
    assert "install" in result.metadata.get("step", "")
    assert "install boom" in result.output


def test_call_plugin_blank_install_error_names_exception(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    manifest = _seed_demo_manifest(tmp_path)
    monkeypatch.setattr(
        host_dispatch_mod, "_PLUGIN_MANIFESTS_BY_NAME", {"demo": manifest}, raising=False
    )

    async def boom_install(sandbox_id: str, m: PluginManifest) -> str:
        del sandbox_id, m
        raise TimeoutError()

    async def never_dispatch(*args: Any, **kwargs: Any) -> dict[str, Any]:
        raise AssertionError("dispatch should not be reached after install fails")

    result = asyncio.run(
        call_plugin(
            _make_context(),
            plugin="demo",
            op="run",
            payload={},
            install_runner=boom_install,
            daemon_dispatcher=never_dispatch,
        )
    )

    assert result.is_error
    assert result.metadata["step"] == "install"
    assert "TimeoutError" in result.output


def test_call_plugin_dispatch_error_surfaces(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    manifest = _seed_demo_manifest(tmp_path)
    monkeypatch.setattr(
        host_dispatch_mod, "_PLUGIN_MANIFESTS_BY_NAME", {"demo": manifest}, raising=False
    )

    async def fake_install(sandbox_id: str, m: PluginManifest) -> str:
        return "abc"

    async def fake_dispatch(
        sandbox_id: str, op: str, args: dict[str, Any], **kwargs: Any
    ) -> dict[str, Any]:
        if op == "api.plugin.ensure":
            return {"success": True, "registered_ops": []}
        return {
            "success": False,
            "error": {"kind": "OpFailed", "message": "boom in plugin op"},
        }

    result = asyncio.run(
        call_plugin(
            _make_context(),
            plugin="demo",
            op="run",
            payload={},
            install_runner=fake_install,
            daemon_dispatcher=fake_dispatch,
        )
    )

    assert result.is_error
    assert "boom in plugin op" in result.output
    assert result.metadata["step"] == "dispatch"


def test_wrap_response_rejects_non_json_payload() -> None:
    result = host_dispatch_mod._wrap_response(
        {"success": True, "value": object()},
        plugin="demo",
        op="run",
    )

    assert result.is_error
    assert result.metadata["step"] == "decode"


def test_wrap_response_rejects_oversize_payload() -> None:
    result = host_dispatch_mod._wrap_response(
        {"success": True, "value": "x" * host_dispatch_mod._MAX_RESPONSE_BYTES},
        plugin="demo",
        op="run",
    )

    assert result.is_error
    assert "byte limit" in result.output


def test_call_plugin_reensures_runtime_when_digest_changes(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manifest = _seed_demo_manifest(tmp_path)
    monkeypatch.setattr(
        host_dispatch_mod, "_PLUGIN_MANIFESTS_BY_NAME", {"demo": manifest}, raising=False
    )
    digests = iter(["digest-a", "digest-b"])
    ensure_payloads: list[dict[str, Any]] = []

    async def changing_install(sandbox_id: str, m: PluginManifest) -> str:
        del sandbox_id, m
        return next(digests)

    async def fake_dispatch(
        sandbox_id: str, op: str, args: dict[str, Any], **kwargs: Any
    ) -> dict[str, Any]:
        del sandbox_id, kwargs
        if op == "api.plugin.ensure":
            ensure_payloads.append(dict(args))
        return {"success": True}

    for _ in range(2):
        result = asyncio.run(
            call_plugin(
                _make_context(),
                plugin="demo",
                op="run",
                payload={},
                install_runner=changing_install,
                daemon_dispatcher=fake_dispatch,
            )
        )
        assert not result.is_error

    assert ensure_payloads == [
        {"plugin": "demo", "digest": "digest-a", "workspace_root": "/testbed"},
        {"plugin": "demo", "digest": "digest-b", "workspace_root": "/testbed"},
    ]


def test_call_plugin_missing_sandbox_id_returns_error() -> None:
    ctx = ToolExecutionContextService(cwd=Path("/tmp"))
    # No sandbox_id set.

    async def never_called(*args: Any, **kwargs: Any) -> Any:
        raise AssertionError("should not run when sandbox_id missing")

    result = asyncio.run(
        call_plugin(
            ctx,
            plugin="demo",
            op="run",
            payload={},
            install_runner=never_called,
            daemon_dispatcher=never_called,
        )
    )
    assert result.is_error


def test_call_plugin_unknown_plugin_returns_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(host_dispatch_mod, "_PLUGIN_MANIFESTS_BY_NAME", {}, raising=False)

    async def never_called(*args: Any, **kwargs: Any) -> Any:
        raise AssertionError("should not run for unknown plugin")

    result = asyncio.run(
        call_plugin(
            _make_context(),
            plugin="ghost",
            op="run",
            payload={},
            install_runner=never_called,
            daemon_dispatcher=never_called,
        )
    )
    assert result.is_error
    assert result.metadata["step"] == "manifest"


def test_call_plugin_serializes_concurrent_installs(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    manifest = _seed_demo_manifest(tmp_path)
    monkeypatch.setattr(
        host_dispatch_mod, "_PLUGIN_MANIFESTS_BY_NAME", {"demo": manifest}, raising=False
    )

    install_starts: list[int] = []
    install_running = 0
    max_install_concurrency = 0

    async def fake_install(sandbox_id: str, m: PluginManifest) -> str:
        nonlocal install_running, max_install_concurrency
        install_running += 1
        max_install_concurrency = max(
            max_install_concurrency, install_running
        )
        await asyncio.sleep(0.01)
        install_starts.append(install_running)
        install_running -= 1
        return "abc"

    async def fake_dispatch(
        sandbox_id: str, op: str, args: dict[str, Any], **kwargs: Any
    ) -> dict[str, Any]:
        if op == "api.plugin.ensure":
            return {"success": True, "registered_ops": []}
        return {"success": True}

    async def runner() -> None:
        await asyncio.gather(
            *(
                call_plugin(
                    _make_context(),
                    plugin="demo",
                    op="run",
                    payload={},
                    install_runner=fake_install,
                    daemon_dispatcher=fake_dispatch,
                )
                for _ in range(5)
            )
        )

    asyncio.run(runner())
    assert max_install_concurrency == 1
