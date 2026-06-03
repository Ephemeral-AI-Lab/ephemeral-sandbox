"""Unit tests for sandbox.ephemeral_workspace.plugin.host_dispatch.call_plugin."""

from __future__ import annotations

import asyncio
import json
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest

from plugins.core.discovery import DEFAULT_CATALOG_DIR
from plugins.core.manifest import PluginManifest, parse_plugin_manifest
from sandbox.host.paths import BUNDLE_REMOTE_DIR
from sandbox.ephemeral_workspace.plugin.op_registry import clear_plugin_registrations
from sandbox.ephemeral_workspace.plugin import host_dispatch as host_dispatch_mod
from sandbox._shared.models import Intent
from sandbox.ephemeral_workspace.plugin.host_dispatch import call_plugin, call_plugin_write
from tools._framework.core.context import ToolExecutionContextService


def _make_context(sandbox_id: str = "sb-1") -> ToolExecutionContextService:
    ctx = ToolExecutionContextService(cwd=Path("/tmp"))
    ctx["sandbox_id"] = sandbox_id
    ctx["repo_root"] = "/testbed"
    return ctx


def _runtime_cache_key(
    *,
    sandbox_id: str = "sb-1",
    plugin: str = "demo",
    workspace_root: str = "/testbed",
) -> tuple[str, str, str, str]:
    return host_dispatch_mod._runtime_cache_key(
        sandbox_id,
        plugin,
        layer_stack_root=host_dispatch_mod.DEFAULT_LAYER_STACK_ROOT,
        workspace_root=workspace_root,
    )


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


def test_ensure_payload_includes_runtime_manifest_for_rust_daemon() -> None:
    manifest = parse_plugin_manifest(DEFAULT_CATALOG_DIR / "lsp")
    try:
        payload = host_dispatch_mod._ensure_payload(
            manifest,
            digest="digest-lsp",
            workspace_root="/testbed",
        )
    finally:
        clear_plugin_registrations("lsp")

    assert payload["plugin"] == "lsp"
    assert payload["digest"] == "digest-lsp"
    assert payload["start_services"] is True
    daemon_manifest = payload["manifest"]
    assert daemon_manifest["plugin_id"] == "lsp"
    assert daemon_manifest["plugin_digest"] == "digest-lsp"
    assert len(daemon_manifest["services"]) == 1
    service = daemon_manifest["services"][0]
    assert service == {
        "service_id": "runtime",
        "service_profile_digest": service["service_profile_digest"],
        "service_mode": "workspace_snapshot_refresh",
        "refresh_strategy": "remount_workspace_and_notify",
        "command": service["command"],
        "ppc_protocol_version": 1,
    }
    assert service["command"][:2] == ["python3", "-c"]
    assert BUNDLE_REMOTE_DIR in service["command"][2]
    assert "ppc_service" in service["command"][2]
    operations = {entry["op_name"]: entry for entry in daemon_manifest["operations"]}
    assert operations["diagnostics"] == {
        "op_name": "diagnostics",
        "intent": Intent.READ_ONLY.value,
        "auto_workspace_overlay": True,
        "service_id": "runtime",
    }
    assert operations["apply_workspace_edit"] == {
        "op_name": "apply_workspace_edit",
        "intent": Intent.WRITE_ALLOWED.value,
        "auto_workspace_overlay": False,
        "service_id": "runtime",
    }


def test_call_plugin_forwards_caller_audit_fields(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manifest = _seed_demo_manifest(tmp_path)
    monkeypatch.setattr(
        host_dispatch_mod, "_PLUGIN_MANIFESTS_BY_NAME", {"demo": manifest}, raising=False
    )
    ctx = _make_context()
    ctx["task_id"] = "task-1"
    ctx["attempt_id"] = "attempt-1"
    ctx["workflow_id"] = "goal-1"
    ctx["request_id"] = "request-1"
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
        "task_id": "task-1",
        "attempt_id": "attempt-1",
        "workflow_id": "goal-1",
        "request_id": "request-1",
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


def test_call_plugin_reensures_runtime_when_workspace_root_changes(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manifest = _seed_demo_manifest(tmp_path)
    monkeypatch.setattr(
        host_dispatch_mod, "_PLUGIN_MANIFESTS_BY_NAME", {"demo": manifest}, raising=False
    )
    ensure_payloads: list[dict[str, Any]] = []

    async def fake_install(sandbox_id: str, m: PluginManifest) -> str:
        del sandbox_id, m
        return "digest-a"

    async def fake_dispatch(
        sandbox_id: str, op: str, args: dict[str, Any], **kwargs: Any
    ) -> dict[str, Any]:
        del sandbox_id, kwargs
        if op == "api.plugin.ensure":
            ensure_payloads.append(dict(args))
        return {"success": True}

    first = _make_context()
    second = _make_context()
    second["repo_root"] = "/ephemeral-os"

    for ctx in (first, second):
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

    assert ensure_payloads == [
        {"plugin": "demo", "digest": "digest-a", "workspace_root": "/testbed"},
        {"plugin": "demo", "digest": "digest-a", "workspace_root": "/ephemeral-os"},
    ]


def test_call_plugin_recovers_stale_runtime_cache_on_unknown_op(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manifest = _seed_demo_manifest(tmp_path)
    monkeypatch.setattr(
        host_dispatch_mod, "_PLUGIN_MANIFESTS_BY_NAME", {"demo": manifest}, raising=False
    )
    host_dispatch_mod._RUNTIME_DIGEST_BY_SANDBOX_PLUGIN[_runtime_cache_key()] = "abc"
    dispatch_ops: list[str] = []
    plugin_attempts = 0

    class UnknownPluginOp(RuntimeError):
        kind = "unknown_op"

    async def fake_install(sandbox_id: str, m: PluginManifest) -> str:
        del sandbox_id, m
        return "abc"

    async def fake_dispatch(
        sandbox_id: str,
        op: str,
        args: dict[str, Any],
        **kwargs: Any,
    ) -> dict[str, Any]:
        del sandbox_id, args, kwargs
        nonlocal plugin_attempts
        dispatch_ops.append(op)
        if op == "plugin.demo.run":
            plugin_attempts += 1
            if plugin_attempts == 1:
                raise UnknownPluginOp("unknown op: plugin.demo.run")
        return {"success": True, "result": "ok"}

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

    assert not result.is_error
    assert dispatch_ops == ["plugin.demo.run", "api.plugin.ensure", "plugin.demo.run"]


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


def test_call_plugin_singleflights_concurrent_runtime_ensure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    manifest = _seed_demo_manifest(tmp_path)
    monkeypatch.setattr(
        host_dispatch_mod, "_PLUGIN_MANIFESTS_BY_NAME", {"demo": manifest}, raising=False
    )

    ensure_count = 0
    ensure_running = 0
    max_ensure_concurrency = 0
    dispatch_count = 0

    async def fake_install(sandbox_id: str, m: PluginManifest) -> str:
        del sandbox_id, m
        return "abc"

    async def fake_dispatch(
        sandbox_id: str, op: str, args: dict[str, Any], **kwargs: Any
    ) -> dict[str, Any]:
        nonlocal ensure_count, ensure_running, max_ensure_concurrency, dispatch_count
        del sandbox_id, args, kwargs
        if op == "api.plugin.ensure":
            ensure_running += 1
            max_ensure_concurrency = max(max_ensure_concurrency, ensure_running)
            await asyncio.sleep(0.01)
            ensure_count += 1
            ensure_running -= 1
            return {"success": True, "registered_ops": []}
        dispatch_count += 1
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
    assert ensure_count == 1
    assert max_ensure_concurrency == 1
    assert dispatch_count == 5


def test_call_plugin_does_not_serialize_dispatch_after_runtime_loaded(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    manifest = _seed_demo_manifest(tmp_path)
    monkeypatch.setattr(
        host_dispatch_mod, "_PLUGIN_MANIFESTS_BY_NAME", {"demo": manifest}, raising=False
    )
    host_dispatch_mod._RUNTIME_DIGEST_BY_SANDBOX_PLUGIN[_runtime_cache_key()] = "abc"
    dispatch_running = 0
    max_dispatch_concurrency = 0

    async def fake_install(sandbox_id: str, m: PluginManifest) -> str:
        del sandbox_id, m
        return "abc"

    async def fake_dispatch(
        sandbox_id: str, op: str, args: dict[str, Any], **kwargs: Any
    ) -> dict[str, Any]:
        nonlocal dispatch_running, max_dispatch_concurrency
        del sandbox_id, args, kwargs
        if op == "api.plugin.ensure":
            raise AssertionError("runtime ensure should be skipped for cached digest")
        dispatch_running += 1
        max_dispatch_concurrency = max(max_dispatch_concurrency, dispatch_running)
        await asyncio.sleep(0.01)
        dispatch_running -= 1
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
    assert max_dispatch_concurrency > 1
