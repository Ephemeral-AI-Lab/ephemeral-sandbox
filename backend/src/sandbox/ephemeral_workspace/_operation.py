"""Per-operation overlay handle helpers for EphemeralPipeline."""

from __future__ import annotations

import shutil
from uuid import uuid4

from sandbox._shared.models import ToolCallResult
from sandbox._shared.resource_audit import command_exec_resource_timings
from sandbox.ephemeral_workspace._types import OperationOverlayHandle
from sandbox.ephemeral_workspace._utils import safe_request_part
from sandbox.overlay.handle import OverlayHandle
from sandbox.overlay.writable_dirs import allocate_overlay_writable_dirs


class EphemeralOperationMixin:
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
            command_exec_resource_timings(
                storage_root=self._layer_stack.storage_root,
                writable_root=self._writable_root,
                run_dir=handle.upperdir.parent,
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
    ) -> OperationOverlayHandle:
        """Lease the latest snapshot and allocate a private overlay upperdir."""
        if self._layer_stack is None:
            raise RuntimeError("acquire_operation_overlay requires layer_stack")
        run_dir = (
            self._writable_root
            / "runtime"
            / "sandbox-overlay-ops"
            / self._runtime_key(self._workspace_ref, self._workspace_root)
            / f"{safe_request_part(invocation_id)}-{uuid4().hex[:8]}"
        )
        snapshot = self._layer_stack.prepare_workspace_snapshot(
            request_id=invocation_id,
        )
        lease_id = str(getattr(snapshot, "lease_id"))
        try:
            writable_dirs = allocate_overlay_writable_dirs(run_dir)
            manifest = getattr(snapshot, "manifest")
            manifest_version = int(getattr(snapshot, "manifest_version"))
            root_hash = str(getattr(snapshot, "root_hash"))
            return OperationOverlayHandle(
                lease_id=lease_id,
                manifest_key=f"{root_hash}@{manifest_version}",
                manifest_version=manifest_version,
                root_hash=root_hash,
                manifest=manifest,
                workspace_root=str(workspace_root or self.workspace_root).rstrip("/")
                or "/",
                run_dir=run_dir.as_posix(),
                upperdir=writable_dirs.upperdir.as_posix(),
                workdir=writable_dirs.workdir.as_posix(),
                layer_paths=getattr(snapshot, "layer_paths", None),
                _overlay=self,
            )
        except Exception:
            self._release_lease(lease_id)
            shutil.rmtree(run_dir, ignore_errors=True)
            raise

    def release_operation_overlay(self, handle: OperationOverlayHandle) -> None:
        """Release a per-operation overlay lease and remove upper/work dirs."""
        self._release_lease(handle.lease_id)
        shutil.rmtree(handle.run_dir, ignore_errors=True)


__all__ = ["EphemeralOperationMixin"]
