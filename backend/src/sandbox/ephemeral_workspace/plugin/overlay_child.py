"""Child helper for automatic plugin workspace overlay dispatch."""

from __future__ import annotations

import asyncio
import dataclasses
import importlib
import json
import sys
from contextlib import asynccontextmanager
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from sandbox.overlay.kernel_mount import (
    MountInputs,
    mount_overlay,
    umount,
    validate_mount_inputs,
)
from sandbox.layer_stack.workspace_binding import require_workspace_binding
from sandbox.ephemeral_workspace.plugin.op_context import (
    PluginOpContext,
    caller_from_audit_payload,
    plugin_intent_from_payload,
)
from sandbox.ephemeral_workspace.plugin.op_registry import (
    clear_plugin_registrations,
    pending_plugin_registrations,
)


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if len(args) != 1:
        sys.stderr.write("plugin overlay child requires one JSON payload path\n")
        return 2
    try:
        payload = json.loads(Path(args[0]).read_text(encoding="utf-8"))
        if not isinstance(payload, dict):
            raise ValueError("payload must be an object")
        return asyncio.run(_run(payload))
    except Exception as exc:
        sys.stderr.write(f"plugin overlay child failed: {exc}\n")
        return 126


async def _run(payload: dict[str, Any]) -> int:
    request = _PluginOverlayRequest(payload)
    _validate_binding(request)
    mount_inputs: MountInputs | None = None
    try:
        mount_inputs = validate_mount_inputs(
            workspace_root=request.workspace_root,
            layer_paths=request.layer_paths,
            upperdir=request.upperdir,
            workdir=request.workdir,
        )
        mount_overlay(
            workspace_root=mount_inputs.workspace_root,
            layer_paths=mount_inputs.layer_paths,
            upperdir=mount_inputs.upperdir,
            workdir=mount_inputs.workdir,
        )
        result = await _invoke_plugin_handler(request)
        request.output_ref.write_text(
            json.dumps(_to_jsonable(result), separators=(",", ":"), sort_keys=True),
            encoding="utf-8",
        )
        return 0
    finally:
        if mount_inputs is not None:
            mount_inputs.close()
        umount(request.workspace_root)


class _PluginOverlayRequest:
    def __init__(self, payload: dict[str, Any]) -> None:
        self.plugin_name = str(payload["plugin_name"])
        self.op_name = str(payload["op_name"])
        raw_args = payload.get("args")
        self.args = raw_args if isinstance(raw_args, dict) else {}
        self.layer_stack_root = str(payload["layer_stack_root"])
        self.workspace_root = Path(str(payload["workspace_root"]))
        raw_layers = payload["layer_paths"]
        if not isinstance(raw_layers, list) or not raw_layers:
            raise ValueError("layer_paths must be a non-empty list")
        self.layer_paths = tuple(Path(str(path)) for path in raw_layers)
        self.upperdir = Path(str(payload["upperdir"]))
        self.workdir = Path(str(payload["workdir"]))
        self.output_ref = Path(str(payload["output_ref"]))
        self.manifest_key = str(payload.get("manifest_key") or "")
        self.manifest_version = int(payload.get("manifest_version") or 0)
        self.root_hash = str(payload.get("root_hash") or "")
        self.intent = plugin_intent_from_payload(payload.get("intent"))
        raw_caller = payload.get("caller")
        self.caller = raw_caller if isinstance(raw_caller, dict) else {}
        raw_metadata = payload.get("metadata")
        self.metadata = raw_metadata if isinstance(raw_metadata, dict) else {}


def _validate_binding(request: _PluginOverlayRequest) -> None:
    binding = require_workspace_binding(request.layer_stack_root)
    if Path(binding.workspace_root) != request.workspace_root:
        raise ValueError(
            "plugin overlay child workspace_root does not match binding: "
            f"{request.workspace_root} != {binding.workspace_root}"
        )


async def _invoke_plugin_handler(request: _PluginOverlayRequest) -> Any:
    handler = _load_handler(request.plugin_name, request.op_name)
    ctx = PluginOpContext(
        layer_stack_root=request.layer_stack_root,
        caller=caller_from_audit_payload(request.caller),
        projection=_MountedPluginProjection(request),
        overlay=_MountedPluginWorkspace(request),
        intent=request.intent,
        metadata=dict(request.metadata),
    )
    result = handler(request.args, ctx)
    if asyncio.iscoroutine(result) or hasattr(result, "__await__"):
        result = await result
    return result


def _load_handler(plugin_name: str, op_name: str) -> Any:
    clear_plugin_registrations(plugin_name)
    importlib.import_module(f"plugins.catalog.{plugin_name}.runtime.server")
    matches = [
        entry.handler
        for entry in pending_plugin_registrations(plugin_name)
        if entry.op_name == op_name
    ]
    if len(matches) != 1:
        raise RuntimeError(
            f"expected one registered handler for {plugin_name}.{op_name}, "
            f"found {len(matches)}"
        )
    return matches[0]


class _MountedPluginProjection:
    def __init__(self, request: _PluginOverlayRequest) -> None:
        self._request = request
        self.layer_stack_root = Path(request.layer_stack_root)

    def active_manifest_key(self) -> str:
        return self._request.manifest_key

    def acquire(self, owner_request_id: str) -> Any:
        del owner_request_id
        return SimpleNamespace(
            lease_id="plugin-overlay-child",
            manifest_key=self._request.manifest_key,
            manifest_version=self._request.manifest_version,
            root_hash=self._request.root_hash,
            manifest=SimpleNamespace(version=self._request.manifest_version),
            layer_paths=None,
            release=lambda: None,
        )


class _MountedPluginWorkspace:
    def __init__(self, request: _PluginOverlayRequest) -> None:
        self._request = request
        self.workspace_root = request.workspace_root.as_posix()

    def active_manifest_key(self) -> str:
        return self._request.manifest_key

    async def ensure_current(self, *, reason: str = "ensure_current") -> str:
        del reason
        return self._request.manifest_key

    def current_manifest(self) -> Any:
        return SimpleNamespace(version=self._request.manifest_version)

    @asynccontextmanager
    async def workspace_operation(self, *, reason: str = "operation") -> Any:
        del reason
        yield self.current_manifest()


def _to_jsonable(value: Any) -> Any:
    if dataclasses.is_dataclass(value) and not isinstance(value, type):
        return {key: _to_jsonable(item) for key, item in dataclasses.asdict(value).items()}
    if isinstance(value, SimpleNamespace):
        return {str(key): _to_jsonable(item) for key, item in vars(value).items()}
    if isinstance(value, dict):
        return {str(key): _to_jsonable(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_to_jsonable(item) for item in value]
    return value


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
