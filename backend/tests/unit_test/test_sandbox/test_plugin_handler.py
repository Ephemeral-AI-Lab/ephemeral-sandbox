"""Unit tests for sandbox.plugin.handler (api.plugin.ensure / api.plugin.status).

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
from sandbox.plugin import handler as handler_mod
from sandbox.plugin.runtime import register_plugin_op
from sandbox.plugin.runtime import registry as registry_mod


@pytest.fixture(autouse=True)
def _isolate_plugin_state() -> Iterator[None]:
    handler_mod._LOADED.clear()
    handler_mod._LOADED_DIGEST.clear()
    handler_mod._PROJECTIONS.clear()
    registry_mod._PENDING.clear()
    pre_existing = [
        name for name in sys.modules if name.startswith("plugins.catalog.")
    ]
    yield
    handler_mod._LOADED.clear()
    handler_mod._LOADED_DIGEST.clear()
    handler_mod._PROJECTIONS.clear()
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
    module_name = f"plugins.catalog.{plugin}.runtime.server"
    namespace: dict[str, object] = {
        "__name__": module_name,
        "register_plugin_op": register_plugin_op,
    }
    body = "\n".join(
        textwrap.dedent(
            f"""
            @register_plugin_op({plugin!r}, {op!r})
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

    response = asyncio.run(handler_mod.plugin_ensure({"plugin": "demo"}))

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


def test_plugin_ensure_runs_optional_runtime_warm_hook(tmp_path: Path) -> None:
    runtime = _inject_runtime("hot_demo", ["hover"], warm_hook=True)
    layer_stack_root = tmp_path / "layer-stack"
    _write_binding(layer_stack_root)

    response = asyncio.run(
        handler_mod.plugin_ensure(
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
        handler_mod.plugin_ensure(
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

    with pytest.raises(handler_mod.PluginEnsureError, match="workspace binding"):
        asyncio.run(
            handler_mod.plugin_ensure(
                {
                    "plugin": "missing_binding",
                    "digest": "a",
                    "layer_stack_root": str(tmp_path / "missing-stack"),
                }
            )
        )


def test_plugin_ensure_is_idempotent() -> None:
    _inject_runtime("demo2", ["hover"])
    first = asyncio.run(
        handler_mod.plugin_ensure({"plugin": "demo2", "digest": "a"})
    )
    second = asyncio.run(
        handler_mod.plugin_ensure({"plugin": "demo2", "digest": "a"})
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

    monkeypatch.setattr(handler_mod.importlib, "import_module", import_runtime)

    first = asyncio.run(
        handler_mod.plugin_ensure({"plugin": "reloadable", "digest": "a"})
    )
    second = asyncio.run(
        handler_mod.plugin_ensure({"plugin": "reloadable", "digest": "b"})
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
        handler_mod.plugin_ensure({"plugin": "stateless_plugin"})
    )
    assert response["success"] is True
    assert response["registered_ops"] == []
    assert response["runtime_loaded"] is False


def test_plugin_status_lists_loaded_plugins() -> None:
    _inject_runtime("demo3", ["q"])
    asyncio.run(handler_mod.plugin_ensure({"plugin": "demo3"}))

    status = asyncio.run(handler_mod.plugin_status({}))
    assert status["success"] is True
    assert any(
        entry["name"] == "demo3" and "plugin.demo3.q" in entry["ops"]
        for entry in status["loaded_plugins"]
    )


def test_plugin_ensure_requires_plugin_name() -> None:
    with pytest.raises(handler_mod.PluginEnsureError, match="requires plugin"):
        asyncio.run(handler_mod.plugin_ensure({}))
