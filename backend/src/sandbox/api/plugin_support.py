"""Public plugin support helpers for runner and benchmark harnesses."""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace
from typing import Any

from plugins.core.manifest import PluginManifest, ToolEntry
from plugins.runtime_bridge import op_registry
from plugins.runtime_bridge.op_registry import (
    PluginOpRegistrationError,
    flush_plugin_registrations,
    register_plugin_op,
)
from sandbox.api import Intent
from sandbox.api import plugin_dispatch
from sandbox.api.plugin_install import PluginInstallError, ensure_installed
from sandbox.host.daemon_client import call_daemon_api
from tools._framework.core.context import ToolExecutionContextService


async def ensure_plugin_installed_and_loaded(
    sandbox_id: str,
    *,
    plugin: str,
    workspace_root: str,
    timeout: int = 120,
) -> dict[str, Any]:
    """Install a catalog plugin and load its daemon runtime."""
    manifest = plugin_dispatch._manifest_for(plugin)
    digest = await ensure_installed(sandbox_id, manifest)
    payload = plugin_dispatch._ensure_payload(
        manifest,
        digest=digest,
        workspace_root=workspace_root,
    )
    response = await call_daemon_api(
        sandbox_id,
        "api.plugin.ensure",
        payload,
        timeout=timeout,
    )
    return {"digest": digest, "response": response}


def daemon_plugin_manifest(plugin: str, *, digest: str) -> dict[str, Any]:
    """Return the daemon manifest projection for a catalog plugin digest."""
    manifest = plugin_dispatch._manifest_for(plugin)
    daemon_manifest = plugin_dispatch._daemon_manifest_for(manifest, digest=digest)
    if daemon_manifest is None:
        raise RuntimeError(f"{plugin} daemon manifest projection unexpectedly missing")
    return daemon_manifest


async def run_plugin_intent_contract_checks() -> dict[str, Any]:
    """Exercise plugin op intent registration and dispatch classification."""
    op_registry._PENDING.clear()
    try:
        missing_intent_error = _registration_error(
            """
async def handler(args, ctx):
    return {"ok": True}
register_plugin_op("demo", "missing",)(handler)
            """.strip(),
            plugin="demo",
        )
        lifecycle_error = _registration_error(
            """
async def handler(args, ctx):
    return {"ok": True}
register_plugin_op("demo", "enter", intent=Intent.LIFECYCLE)(handler)
            """.strip(),
            plugin="demo",
        )
        _exec_in_plugin_namespace(
            "demo",
            """
async def read_handler(args, ctx):
    return {"success": True, "path": "service", "marker": ctx.marker}
register_plugin_op("demo", "read", intent=Intent.READ_ONLY)(read_handler)
            """.strip(),
        )
        registered: dict[str, Any] = {}

        async def read_context_factory(
            args: dict[str, Any],
            plugin_name: str,
            op_name: str,
        ) -> Any:
            del args, plugin_name, op_name
            return SimpleNamespace(marker="read-context")

        flush_plugin_registrations(
            "demo",
            registered.__setitem__,
            context_factory=read_context_factory,
            trusted_caller=True,
        )
        read_result = await registered["plugin.demo.read"]({"value": 1})

        _exec_in_plugin_namespace(
            "demo",
            """
async def write_handler(args, ctx):
    return {"success": True, "path": "daemon_overlay", "marker": ctx.marker}
register_plugin_op("demo", "write", intent=Intent.WRITE_ALLOWED)(write_handler)
            """.strip(),
        )
        write_registered: dict[str, Any] = {}

        async def write_context_factory(
            args: dict[str, Any],
            plugin_name: str,
            op_name: str,
        ) -> Any:
            del args, plugin_name, op_name
            return SimpleNamespace(marker="write-context")

        flush_plugin_registrations(
            "demo",
            write_registered.__setitem__,
            context_factory=write_context_factory,
            trusted_caller=True,
        )
        write_result = await write_registered["plugin.demo.write"]({"value": 2})
        write_result["write_allowed_route"] = "rust_daemon_overlay_occ"
    finally:
        op_registry._PENDING.clear()

    return {
        "missing_intent_error": missing_intent_error,
        "lifecycle_error": lifecycle_error,
        "read_only_result": read_result,
        "write_allowed_result": write_result,
        "overlay_owner": "rust_daemon",
    }


async def run_plugin_setup_failure_checks(
    sandbox_id: str,
    *,
    workspace_root: str,
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Classify plugin setup failure and prove retry has no stale state."""
    plugin_name = "netfail"
    old_cache = plugin_dispatch._PLUGIN_MANIFESTS_BY_NAME
    plugin_dispatch.reset_host_dispatch_cache_for_tests()
    plugin_dispatch._PLUGIN_MANIFESTS_BY_NAME = {
        plugin_name: _fake_manifest(plugin_name)
    }
    install_attempts = 0
    dispatch_calls: list[str] = []
    try:

        async def fail_install(_sandbox_id: str, _manifest: PluginManifest) -> str:
            raise PluginInstallError(
                "setup.sh could not reach registry.npmjs.org",
                kind="plugin_setup_network_failure",
                plugin_name=plugin_name,
                setup_step="setup.sh",
                command="curl -fsSL https://registry.npmjs.org/pyright",
                stderr_excerpt="curl: (6) Could not resolve host: registry.npmjs.org",
            )

        failure = await plugin_dispatch.call_plugin(
            _plugin_context(sandbox_id, workspace_root=workspace_root),
            plugin=plugin_name,
            op="run",
            payload={},
            install_runner=fail_install,
            daemon_dispatcher=_never_dispatch,
        )

        async def retry_install(_sandbox_id: str, _manifest: PluginManifest) -> str:
            nonlocal install_attempts
            install_attempts += 1
            return "digest-ok"

        async def retry_dispatch(
            _sandbox_id: str,
            op: str,
            args: dict[str, Any],
            **_kwargs: Any,
        ) -> dict[str, Any]:
            dispatch_calls.append(op)
            if op == "api.plugin.ensure":
                return {
                    "success": True,
                    "registered_ops": [f"plugin.{plugin_name}.run"],
                }
            return {"success": True, "result": "ok", "args": args}

        retry = await plugin_dispatch.call_plugin(
            _plugin_context(sandbox_id, workspace_root=workspace_root),
            plugin=plugin_name,
            op="run",
            payload={"value": 1},
            install_runner=retry_install,
            daemon_dispatcher=retry_dispatch,
        )
    finally:
        plugin_dispatch._PLUGIN_MANIFESTS_BY_NAME = old_cache
        plugin_dispatch.reset_host_dispatch_cache_for_tests()

    return (
        {
            "is_error": failure.is_error,
            "output": failure.output,
            "metadata": dict(failure.metadata or {}),
        },
        {
            "is_error": retry.is_error,
            "output": retry.output,
            "metadata": dict(retry.metadata or {}),
            "install_attempts": install_attempts,
            "dispatch_calls": dispatch_calls,
        },
    )


async def _never_dispatch(*_args: Any, **_kwargs: Any) -> dict[str, Any]:
    raise AssertionError("dispatch should not run after setup failure")


def _fake_manifest(plugin_name: str) -> PluginManifest:
    source_dir = Path("/tmp") / plugin_name
    return PluginManifest(
        name=plugin_name,
        description="synthetic network-failure plugin",
        tools=(ToolEntry(name=f"{plugin_name}.run", module=source_dir / "tools" / "run.py"),),
        setup=source_dir / "setup.sh",
        runtime=None,
        source_dir=source_dir,
        body="",
    )


def _plugin_context(
    sandbox_id: str,
    *,
    workspace_root: str,
) -> ToolExecutionContextService:
    ctx = ToolExecutionContextService(cwd=Path("/tmp"))
    ctx["sandbox_id"] = sandbox_id
    ctx["repo_root"] = workspace_root
    return ctx


def _registration_error(code: str, *, plugin: str) -> dict[str, str]:
    try:
        _exec_in_plugin_namespace(plugin, code)
    except (TypeError, PluginOpRegistrationError) as exc:
        return {"type": type(exc).__name__, "message": str(exc)}
    raise AssertionError("registration unexpectedly succeeded")


def _exec_in_plugin_namespace(plugin_name: str, code: str) -> dict[str, object]:
    namespace: dict[str, object] = {
        "__name__": f"plugins.catalog.{plugin_name}.runtime.synthetic_probe",
        "register_plugin_op": register_plugin_op,
        "Intent": Intent,
    }
    exec(code, namespace)
    return namespace


__all__ = [
    "daemon_plugin_manifest",
    "ensure_plugin_installed_and_loaded",
    "run_plugin_intent_contract_checks",
    "run_plugin_setup_failure_checks",
]
