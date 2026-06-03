"""Unit tests for sandbox.ephemeral_workspace.plugin.runtime_api (api.plugin.ensure / api.plugin.status).

Synthetic plugin runtime modules are injected via ``sys.modules`` (and
created with ``exec()`` so the ``register_plugin_op`` namespace check passes)
to avoid polluting the production plugins/catalog tree during unit tests.
The handler's :func:`importlib.import_module` returns the cached module
without filesystem resolution.
"""

from __future__ import annotations

import asyncio
import sys
import textwrap
import types
from collections.abc import Iterator
from pathlib import Path

import pytest

from sandbox.layer_stack.workspace_binding import (
    WorkspaceBinding,
    write_workspace_binding_atomic,
)
from sandbox.ephemeral_workspace.plugin import runtime_api as runtime_api_mod
from sandbox.ephemeral_workspace.plugin import op_registry as registry_mod
from sandbox.ephemeral_workspace.pipeline_registry import clear_pipeline_registry_for_tests
from sandbox.ephemeral_workspace.plugin.op_registry import register_plugin_op


@pytest.fixture(autouse=True)
def _isolate_plugin_state() -> Iterator[None]:
    runtime_api_mod._LOADED_PLUGIN_RUNTIMES.clear()
    runtime_api_mod._WORKSPACE_PROJECTIONS.clear()
    clear_pipeline_registry_for_tests()
    registry_mod._PENDING.clear()
    pre_existing = [
        name for name in sys.modules if name.startswith("plugins.catalog.")
    ]
    yield
    runtime_api_mod._LOADED_PLUGIN_RUNTIMES.clear()
    runtime_api_mod._WORKSPACE_PROJECTIONS.clear()
    clear_pipeline_registry_for_tests()
    registry_mod._PENDING.clear()
    for name in [
        n for n in sys.modules if n.startswith("plugins.catalog.")
    ]:
        if name not in pre_existing:
            sys.modules.pop(name, None)


def _inject_runtime(
    plugin: str,
    ops: list[str],
    *,
    warm_hook: bool = False,
) -> types.ModuleType:
    """Build a synthetic plugins.catalog.<plugin>.runtime.server module.

    Uses exec() with __name__ set to the plugin runtime path so the
    register_plugin_op namespace check sees a valid caller frame; injects
    the resulting module into sys.modules so importlib.import_module
    returns it directly.
    """
    from sandbox._shared.models import Intent

    module_name = f"plugins.catalog.{plugin}.runtime.server"
    namespace: dict[str, object] = {
        "__name__": module_name,
        "register_plugin_op": register_plugin_op,
        "Intent": Intent,
    }
    body = "\n".join(
        textwrap.dedent(
            f"""
            @register_plugin_op({plugin!r}, {op!r}, intent=Intent.READ_ONLY)
            async def {op}(args):
                return {{"echo": args}}
            """
        ).strip()
        for op in ops
    )
    if warm_hook:
        body = (
            "WARM_CALLS = []\n"
            f"{body}\n"
            "async def warm_plugin_runtime(args, ctx):\n"
            "    WARM_CALLS.append((args.get('plugin'), ctx.layer_stack_root))\n"
            "    return {'manifest_key': 'hot@1'}\n"
        )
    exec(body, namespace)

    mod = types.ModuleType(module_name)
    for key, value in namespace.items():
        setattr(mod, key, value)
    sys.modules[module_name] = mod
    return mod


def _write_binding(layer_stack_root: Path) -> None:
    write_workspace_binding_atomic(
        WorkspaceBinding(
            workspace_root="/testbed",
            layer_stack_root=str(layer_stack_root),
            active_manifest_version=1,
            active_root_hash="active",
            base_manifest_version=1,
            base_root_hash="base",
        )
    )


def test_plugin_ensure_loads_runtime_and_registers_ops() -> None:
    _inject_runtime("demo", ["hover", "ping"])

    response = asyncio.run(runtime_api_mod.plugin_ensure({"plugin": "demo"}))

    assert response["success"] is True
    assert response["plugin"] == "demo"
    assert sorted(response["registered_ops"]) == [
        "plugin.demo.hover",
        "plugin.demo.ping",
    ]
    assert response["runtime_loaded"] is True
    assert response["already_loaded"] is False

    from sandbox.daemon.rpc.dispatcher import OP_TABLE

    assert "plugin.demo.hover" in OP_TABLE
    assert "plugin.demo.ping" in OP_TABLE


def test_plugin_ensure_runs_optional_runtime_warm_hook(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    runtime = _inject_runtime("hot_demo", ["hover"], warm_hook=True)
    layer_stack_root = tmp_path / "layer-stack"
    _write_binding(layer_stack_root)

    async def fake_get_ephemeral_pipeline(*_args: object, **_kwargs: object) -> object:
        return types.SimpleNamespace(workspace_root="/testbed")

    monkeypatch.setattr(runtime_api_mod, "get_ephemeral_pipeline", fake_get_ephemeral_pipeline)

    response = asyncio.run(
        runtime_api_mod.plugin_ensure(
            {
                "plugin": "hot_demo",
                "digest": "a",
                "layer_stack_root": str(layer_stack_root),
            }
        )
    )

    assert response["runtime_warmed"] is True
    assert response["warm_result"] == {"manifest_key": "hot@1"}
    assert runtime.WARM_CALLS == [("hot_demo", str(layer_stack_root))]

    second = asyncio.run(
        runtime_api_mod.plugin_ensure(
            {
                "plugin": "hot_demo",
                "digest": "a",
                "layer_stack_root": str(layer_stack_root),
            }
        )
    )

    assert second["already_loaded"] is True
    assert second["runtime_warmed"] is True
    assert runtime.WARM_CALLS == [
        ("hot_demo", str(layer_stack_root)),
        ("hot_demo", str(layer_stack_root)),
    ]


def test_plugin_warm_requires_workspace_binding(tmp_path: Path) -> None:
    _inject_runtime("missing_binding", ["hover"], warm_hook=True)

    with pytest.raises(runtime_api_mod.PluginEnsureError, match="workspace binding"):
        asyncio.run(
            runtime_api_mod.plugin_ensure(
                {
                    "plugin": "missing_binding",
                    "digest": "a",
                    "layer_stack_root": str(tmp_path / "missing-stack"),
                }
            )
        )


def test_plugin_context_rejects_workspace_root_mismatch(tmp_path: Path) -> None:
    layer_stack_root = tmp_path / "layer-stack"
    _write_binding(layer_stack_root)

    with pytest.raises(runtime_api_mod.PluginEnsureError, match="workspace_root"):
        asyncio.run(
            runtime_api_mod._build_plugin_op_context(
                {
                    "layer_stack_root": str(layer_stack_root),
                    "workspace_root": "/other",
                },
                "demo",
                "hover",
            )
        )


def test_plugin_context_does_not_start_persistent_overlay_mount(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    layer_stack_root = tmp_path / "layer-stack"
    _write_binding(layer_stack_root)
    calls: list[dict[str, object]] = []

    async def fake_get_ephemeral_pipeline(
        layer_stack_root_arg: str,
        *,
        workspace_root: str | None = None,
        start: bool = True,
    ) -> object:
        calls.append(
            {
                "layer_stack_root": layer_stack_root_arg,
                "workspace_root": workspace_root,
                "start": start,
            }
        )
        return object()

    monkeypatch.setattr(
        runtime_api_mod,
        "get_ephemeral_pipeline",
        fake_get_ephemeral_pipeline,
    )

    overlay = asyncio.run(
        runtime_api_mod._ephemeral_pipeline_for_layer_stack_root(
            str(layer_stack_root),
            workspace_root="/testbed",
        )
    )

    assert overlay is not None
    assert calls == [
        {
            "layer_stack_root": str(layer_stack_root),
            "workspace_root": "/testbed",
            "start": False,
        }
    ]


def test_plugin_ensure_is_idempotent() -> None:
    _inject_runtime("demo2", ["hover"])
    first = asyncio.run(
        runtime_api_mod.plugin_ensure({"plugin": "demo2", "digest": "a"})
    )
    second = asyncio.run(
        runtime_api_mod.plugin_ensure({"plugin": "demo2", "digest": "a"})
    )
    assert first["registered_ops"] == second["registered_ops"]
    assert second["already_loaded"] is True


def test_plugin_ensure_reloads_when_digest_changes(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    imports = [["hover"], ["ping"]]

    def import_runtime(module_name: str) -> types.ModuleType:
        assert module_name == "plugins.catalog.reloadable.runtime.server"
        return _inject_runtime("reloadable", imports.pop(0))

    monkeypatch.setattr(runtime_api_mod.importlib, "import_module", import_runtime)

    first = asyncio.run(
        runtime_api_mod.plugin_ensure({"plugin": "reloadable", "digest": "a"})
    )
    second = asyncio.run(
        runtime_api_mod.plugin_ensure({"plugin": "reloadable", "digest": "b"})
    )

    assert first["registered_ops"] == ["plugin.reloadable.hover"]
    assert second["already_loaded"] is False
    assert second["registered_ops"] == ["plugin.reloadable.ping"]

    from sandbox.daemon.rpc.dispatcher import OP_TABLE

    assert "plugin.reloadable.hover" not in OP_TABLE
    assert "plugin.reloadable.ping" in OP_TABLE


def test_plugin_ensure_when_no_runtime_module() -> None:
    """Plugins without a runtime/server.py register zero ops but succeed."""
    response = asyncio.run(
        runtime_api_mod.plugin_ensure({"plugin": "stateless_plugin"})
    )
    assert response["success"] is True
    assert response["registered_ops"] == []
    assert response["runtime_loaded"] is False


def test_plugin_status_lists_loaded_plugins() -> None:
    _inject_runtime("demo3", ["q"])
    asyncio.run(runtime_api_mod.plugin_ensure({"plugin": "demo3"}))

    status = asyncio.run(runtime_api_mod.plugin_status({}))
    assert status["success"] is True
    assert any(
        entry["name"] == "demo3" and "plugin.demo3.q" in entry["ops"]
        for entry in status["loaded_plugins"]
    )


def test_plugin_ensure_requires_plugin_name() -> None:
    with pytest.raises(runtime_api_mod.PluginEnsureError, match="requires plugin"):
        asyncio.run(runtime_api_mod.plugin_ensure({}))
