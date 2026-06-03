"""Per-operation overlay lease helpers for EphemeralPipeline."""

from __future__ import annotations

from sandbox._shared.models import ToolCallResult
from sandbox._shared.command_exec_resource_metrics import (
    collect_command_exec_resource_metrics,
)
from sandbox.overlay import lifecycle as overlay_lifecycle
from sandbox.overlay.handle import OverlayHandle


class OperationOverlayMixin:
    def _attach_resource_timings(
        self,
        result: ToolCallResult,
        *,
        handle: OverlayHandle,
        changed_path_count: int,
    ) -> ToolCallResult:
        if self._layer_stack is None:
            return result
        payload = dict(result)
        timings = dict(
            payload.get("timings") if isinstance(payload.get("timings"), dict) else {}
        )
        timings.update(
            collect_command_exec_resource_metrics(
                storage_root=self._layer_stack.storage_root,
                writable_root=self._writable_root,
                run_dir=handle.run_dir,
                upperdir=handle.upperdir,
                manifest=handle.snapshot_manifest,
                changed_path_count=changed_path_count,
            )
        )
        payload["timings"] = timings
        return payload

    def acquire_operation_overlay(
        self,
        *,
        invocation_id: str,
        workspace_root: str | None = None,
    ) -> OverlayHandle:
        """Lease the latest snapshot and allocate a private overlay upperdir."""
        if self._layer_stack is None:
            raise RuntimeError("acquire_operation_overlay requires layer_stack")
        return overlay_lifecycle.acquire(
            self._layer_stack,
            invocation_id=invocation_id,
            workspace_root=str(workspace_root or self.workspace_root),
            release_hook=self._release_lease,
        )

__all__ = ["OperationOverlayMixin"]
