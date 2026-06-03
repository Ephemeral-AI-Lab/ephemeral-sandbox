"""Regression test for BL-01: failed warm must not wedge the plugin registry.

Before the fix, ``plugin_ensure`` wrote ``_LOADED_PLUGIN_RUNTIMES`` *before*
awaiting ``_warm_plugin_runtime``. When warm raised, those mutations were not
rolled back, so every subsequent call (with the same digest) took the
"already loaded" branch and re-invoked warm, which kept failing forever — only
a host process restart escaped the wedge.

After the fix, a failed warm leaves ``_LOADED_PLUGIN_RUNTIMES`` empty so the next call retries
the full ensure path. The plugin recovers without restart.
"""

from __future__ import annotations

import asyncio
import sys
import textwrap
import types
from collections.abc import Iterator

import pytest

from sandbox.ephemeral_workspace.plugin import runtime_api as runtime_api_mod
from sandbox.ephemeral_workspace.plugin import op_registry as registry_mod
from sandbox.ephemeral_workspace.plugin.op_registry import register_plugin_op


@pytest.fixture(autouse=True)
def _isolate_plugin_state() -> Iterator[None]:
    runtime_api_mod._LOADED_PLUGIN_RUNTIMES.clear()
    runtime_api_mod._WORKSPACE_PROJECTIONS.clear()
    registry_mod._PENDING.clear()
    pre_existing = [
        name for name in sys.modules if name.startswith("plugins.catalog.")
    ]
    yield
    runtime_api_mod._LOADED_PLUGIN_RUNTIMES.clear()
    runtime_api_mod._WORKSPACE_PROJECTIONS.clear()
    registry_mod._PENDING.clear()
    for name in [n for n in sys.modules if n.startswith("plugins.catalog.")]:
        if name not in pre_existing:
            sys.modules.pop(name, None)


def _inject_runtime(plugin: str, op: str) -> types.ModuleType:
    """Build a synthetic plugins.catalog.<plugin>.runtime.server module.

    The module exposes one op handler and no warm hook; warm behavior is
    controlled by monkeypatching ``runtime_api_mod._warm_plugin_runtime`` in the
    test so we can deterministically fail-then-succeed.
    """
    from sandbox._shared.models import Intent

    module_name = f"plugins.catalog.{plugin}.runtime.server"
    namespace: dict[str, object] = {
        "__name__": module_name,
        "register_plugin_op": register_plugin_op,
        "Intent": Intent,
    }
    body = textwrap.dedent(
        f"""
        @register_plugin_op({plugin!r}, {op!r}, intent=Intent.READ_ONLY)
        async def {op}(args):
            return {{"echo": args}}
        """
    ).strip()
    exec(body, namespace)
    mod = types.ModuleType(module_name)
    for key, value in namespace.items():
        setattr(mod, key, value)
    sys.modules[module_name] = mod
    return mod


def test_plugin_ensure_recovers_from_transient_warm_failure(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A failed warm must NOT wedge the registry — the next call retries."""
    _inject_runtime("wedge_demo", "hover")

    calls: list[int] = []

    async def flaky_warm(
        plugin_name: str, args: dict[str, object]
    ) -> dict[str, object]:
        calls.append(len(calls) + 1)
        if len(calls) == 1:
            raise runtime_api_mod.PluginEnsureError("simulated warm failure")
        return {"runtime_warmed": True, "warm_result": {"ok": True}}

    monkeypatch.setattr(runtime_api_mod, "_warm_plugin_runtime", flaky_warm)

    # First call raises — warm fails.
    with pytest.raises(runtime_api_mod.PluginEnsureError, match="simulated warm"):
        asyncio.run(
            runtime_api_mod.plugin_ensure({"plugin": "wedge_demo", "digest": "a"})
        )

    # Registry MUST be empty after the failed warm.
    assert "wedge_demo" not in runtime_api_mod._LOADED_PLUGIN_RUNTIMES, (
        "BL-01: _LOADED_PLUGIN_RUNTIMES was written before warm completed; registry is wedged"
    )
    assert "plugins.catalog.wedge_demo.runtime.server" not in sys.modules

    _inject_runtime("wedge_demo", "hover")

    # Second call must take the full ensure path (not "already loaded") and
    # succeed because warm now returns normally.
    response = asyncio.run(
        runtime_api_mod.plugin_ensure({"plugin": "wedge_demo", "digest": "a"})
    )
    assert response["success"] is True
    assert response["already_loaded"] is False
    assert response["registered_ops"] == ["plugin.wedge_demo.hover"]
    assert response["runtime_warmed"] is True

    # Now the registry is populated.
    assert "wedge_demo" in runtime_api_mod._LOADED_PLUGIN_RUNTIMES
    assert runtime_api_mod._LOADED_PLUGIN_RUNTIMES["wedge_demo"].digest == "a"

    # Warm was invoked exactly twice (once failed, once succeeded).
    assert calls == [1, 2]
