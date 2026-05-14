"""Acquire a frozen layer-stack snapshot and run one overlay shell request."""

from __future__ import annotations

from dataclasses import replace

from sandbox.execution.overlay.pipeline import OverlayInvoker, OverlayRuntimeInvoker
from sandbox.execution.overlay.request import OverlayShellRequest
from sandbox.execution.overlay.result import OverlayCapture
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.timing import monotonic_now


class OverlaySnapshotRunner:
    """Lease a snapshot, invoke runtime capture, and release the lease."""

    def __init__(
        self,
        layer_stack: LayerStackManager,
        *,
        invoker: OverlayInvoker | None = None,
    ) -> None:
        self._layer_stack = layer_stack
        self._invoker = (
            invoker
            if invoker is not None
            else OverlayRuntimeInvoker(storage_root=layer_stack.storage_root)
        )

    async def shell(self, request: OverlayShellRequest) -> OverlayCapture:
        total_start = monotonic_now()
        lease_start = monotonic_now()
        lease = self._layer_stack.acquire_snapshot_lease(request.request_id)
        timings = {"overlay.lease_acquire_s": monotonic_now() - lease_start}
        invoke_start = monotonic_now()
        try:
            capture = await self._invoker.invoke(
                request=request, manifest=lease.manifest
            )
        finally:
            timings["overlay.invoke_total_s"] = monotonic_now() - invoke_start
            release_start = monotonic_now()
            self._layer_stack.release_lease(lease.lease_id)
            timings["overlay.lease_release_s"] = monotonic_now() - release_start
            timings["overlay.runner_total_s"] = monotonic_now() - total_start
        return replace(capture, timings={**capture.timings, **timings})


__all__ = ["OverlaySnapshotRunner"]
