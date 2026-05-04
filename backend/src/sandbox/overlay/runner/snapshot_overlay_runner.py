"""Acquire a frozen layer-stack snapshot and run one overlay shell request."""

from __future__ import annotations

from typing import Protocol

from sandbox.layer_stack.manifest import Manifest
from sandbox.layer_stack.stack_manager import LayerStackManager
from sandbox.overlay.runner.runtime_invoker import RuntimeInvoker
from sandbox.overlay.types import OverlayShellRequest
from sandbox.runtime.overlay_shell.result_envelope import RuntimeResultEnvelope


class _RuntimeInvoker(Protocol):
    async def invoke(
        self,
        *,
        request: OverlayShellRequest,
        manifest: Manifest,
    ) -> RuntimeResultEnvelope: ...


class SnapshotOverlayRunner:
    """Lease a snapshot, invoke runtime capture, and release the lease."""

    def __init__(
        self,
        layer_stack: LayerStackManager,
        *,
        invoker: _RuntimeInvoker | None = None,
    ) -> None:
        self._layer_stack = layer_stack
        self._invoker = invoker or RuntimeInvoker(storage_root=layer_stack.storage_root)

    async def shell(self, request: OverlayShellRequest) -> RuntimeResultEnvelope:
        lease = self._layer_stack.acquire_snapshot_lease(request.request_id)
        try:
            return await self._invoker.invoke(
                request=request,
                manifest=lease.manifest,
            )
        finally:
            self._layer_stack.release_lease(lease.lease_id)


__all__ = [
    "SnapshotOverlayRunner",
]
