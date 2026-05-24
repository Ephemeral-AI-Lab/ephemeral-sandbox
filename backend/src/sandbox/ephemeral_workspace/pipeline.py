"""Daemon-owned overlay lifecycle and publish boundary."""

from __future__ import annotations

import asyncio
from collections.abc import Sequence
from contextlib import asynccontextmanager, suppress
from pathlib import Path
import shutil
from typing import AsyncIterator

from sandbox._shared.lease_guard import LeaseGuard
from sandbox._shared.models import Intent, ToolCallRequest, ToolCallResult
from sandbox.ephemeral_workspace._manager import (
    clear_overlay_manager_for_tests,
    get_sandbox_overlay,
    stop_all_overlays,
    stop_sandbox_overlay,
)
from sandbox.ephemeral_workspace._operation import EphemeralOperationMixin
from sandbox.ephemeral_workspace._publishing import EphemeralPublishMixin
from sandbox.ephemeral_workspace._types import (
    OperationOverlayHandle,
    OverlayLayerStackClient,
    _OverlaySnapshot,
)
from sandbox.ephemeral_workspace._utils import (
    foreign_watch_interval_s,
    runtime_key,
)
from sandbox.ephemeral_workspace.events import (
    EphemeralPipelineEventBus,
    WorkspaceChangeEvent,
)
from sandbox._shared.shell_contract import (
    OCCMutationClient,
    SnapshotManifest,
)
from sandbox.layer_stack.manifest import manifest_root_hash
from sandbox.overlay import lifecycle as overlay_lifecycle
from sandbox.overlay.kernel_mount import (
    mount_overlay,
    umount,
    validate_mount_inputs,
)
from sandbox.overlay.namespace_runner import run_in_namespace
from sandbox.overlay.path_change import OverlayPathChange
from sandbox.overlay.writable_dirs import overlay_writable_root


class EphemeralPipeline(EphemeralOperationMixin, EphemeralPublishMixin):
    """Facade hiding overlay freshness, capture, and OCC behind the daemon boundary.

    Audit/event divergence vs ``IsolatedPipeline``:
        ``EphemeralPipeline`` uses ``events.WorkspaceChangeEvent`` via the
        in-process ``event_bus.emit()`` — this is RUNTIME CONTROL FLOW
        consumed by ``_watch_foreign_publishes``, not audit. For lifecycle
        audit, see ``IsolatedPipeline``'s ``_JsonlAuditSink`` pattern in
        ``isolated_workspace/_manager.py``.
    """

    def __init__(
        self,
        *,
        occ_client: OCCMutationClient,
        workspace_ref: str,
        layer_stack: OverlayLayerStackClient | None = None,
        workspace_root: str = "/testbed",
        event_bus: EphemeralPipelineEventBus | None = None,
    ) -> None:
        self._occ_client = occ_client
        self._workspace_ref = workspace_ref
        self._layer_stack = layer_stack
        self._workspace_root = workspace_root.rstrip("/") or "/"
        self.event_bus = event_bus or EphemeralPipelineEventBus()
        self._active_manifest_key = ""
        self._active_manifest_version = 0
        self._mounted = False
        self._active_lease_id = ""
        self._operation_lock = asyncio.Lock()
        self._foreign_watch_task: asyncio.Task[None] | None = None
        self._lease_guard = LeaseGuard()
        self._writable_root = overlay_writable_root()
        self._runtime_dir_path = (
            self._writable_root
            / "runtime"
            / "sandbox-overlay"
            / self._runtime_key(workspace_ref, self._workspace_root)
        )
        self._upperdir = self._runtime_dir / "upper"
        self._workdir = self._runtime_dir / "work"
        if layer_stack is not None and hasattr(layer_stack, "read_active_manifest"):
            self._mark_active(layer_stack.read_active_manifest())

    @property
    def workspace_root(self) -> str:
        return self._workspace_root

    @property
    def is_mounted(self) -> bool:
        return self._mounted

    @property
    def upperdir(self) -> Path:
        return self._upperdir

    @property
    def writable_root(self) -> Path:
        return self._writable_root

    @property
    def runtime_dir(self) -> Path:
        return self._runtime_dir

    @property
    def _runtime_dir(self) -> Path:
        return self._runtime_dir_path

    @asynccontextmanager
    async def workspace_operation(
        self,
        *,
        reason: str = "operation",
    ) -> AsyncIterator[SnapshotManifest]:
        async with self._operation_lock:
            await self.ensure_current(reason=reason)
            yield self.current_manifest()

    async def run_tool_call(self, req: ToolCallRequest) -> ToolCallResult:
        """Run one foreground tool call through a fresh overlay lifecycle."""
        if self._layer_stack is None:
            raise RuntimeError("EphemeralPipeline.run_tool_call requires layer_stack")
        handle = await overlay_lifecycle.create(
            self._layer_stack,
            agent_id=req.agent_id,
            workspace_root=self._workspace_root,
        )
        try:
            path_changes: Sequence[OverlayPathChange] = ()
            result = await run_in_namespace(handle, req)
            if req.intent == Intent.WRITE_ALLOWED:
                path_changes = await overlay_lifecycle.capture_changes(handle)
                paths = {change.path for change in path_changes}
                source = (
                    "api_write"
                    if req.verb in {"write_file", "edit_file"} and len(paths) == 1
                    else "overlay_capture"
                )
                result = await self._commit_and_attach(
                    result,
                    path_changes=path_changes,
                    snapshot=handle.snapshot_manifest,
                    source=source,
                )
            return self._attach_resource_timings(
                result,
                handle=handle,
                changed_path_count=len(path_changes),
            )
        finally:
            await self._lease_guard.destroy(handle, overlay_lifecycle.destroy)

    def active_manifest_key(self) -> str:
        if self._layer_stack is None:
            return self._active_manifest_key
        manifest = self._layer_stack.read_active_manifest()
        self._mark_active(manifest)
        return self._active_manifest_key

    def current_manifest(self) -> SnapshotManifest:
        if self._layer_stack is None:
            raise RuntimeError("EphemeralPipeline.current_manifest requires layer_stack")
        manifest = self._layer_stack.read_active_manifest()
        self._mark_active(manifest)
        return manifest

    async def start(self) -> None:
        """Mount the daemon-owned overlay at the workspace root."""
        if self._layer_stack is None:
            raise RuntimeError("EphemeralPipeline.start requires layer_stack")
        if self._mounted:
            return
        self._mount_active(reason="start")
        self._start_foreign_publish_watcher()

    async def stop(self) -> None:
        """Detach the daemon-owned overlay and remove upper/work dirs."""
        await self._stop_foreign_publish_watcher()
        umount(Path(self.workspace_root))
        self._mounted = False
        self._release_lease(self._active_lease_id)
        self._active_lease_id = ""
        shutil.rmtree(self._runtime_dir, ignore_errors=True)

    async def ensure_current(self, *, reason: str = "ensure_current") -> str:
        """Refresh daemon-owned overlay state to the latest manifest if needed."""
        if self._layer_stack is None:
            return self._active_manifest_key
        old_version = self._active_manifest_version
        manifest = self._layer_stack.read_active_manifest()
        new_key = self._manifest_key(manifest)
        if new_key == self._active_manifest_key:
            return self._active_manifest_key
        if self._mounted:
            manifest = self._remount_active(reason=reason)
        else:
            self._mark_active(manifest)
        self.event_bus.emit(
            WorkspaceChangeEvent(
                reason="foreign_publish" if reason != "start" else "remount",
                from_version=old_version,
                to_version=manifest.version,
                changes=(),
            )
        )
        return self._active_manifest_key

    def _mark_active(self, manifest: SnapshotManifest) -> None:
        self._active_manifest_version = int(manifest.version)
        self._active_manifest_key = self._manifest_key(manifest)

    def _manifest_key(self, manifest: SnapshotManifest) -> str:
        try:
            root_hash = manifest_root_hash(manifest)  # type: ignore[arg-type]
        except Exception:
            root_hash = "unknown"
        return f"{root_hash}@{int(manifest.version)}"

    def _runtime_key(self, workspace_ref: str, workspace_root: str) -> str:
        return runtime_key(workspace_ref, workspace_root)

    def _prepare_mount_dirs(self) -> None:
        self._upperdir.mkdir(parents=True, exist_ok=True)
        self._workdir.mkdir(parents=True, exist_ok=True)

    def _remount_active(self, *, reason: str) -> SnapshotManifest:
        self._detach_active_mount()
        return self._mount_active(reason=reason)

    def _detach_active_mount(self) -> None:
        if not self._mounted:
            return
        umount(Path(self.workspace_root))
        self._mounted = False
        self._release_lease(self._active_lease_id)
        self._active_lease_id = ""
        shutil.rmtree(self._upperdir, ignore_errors=True)
        shutil.rmtree(self._workdir, ignore_errors=True)

    def _mount_active(self, *, reason: str) -> SnapshotManifest:
        snapshot = self._prepare_overlay_snapshot(f"sandbox-overlay-{reason}")
        self._prepare_mount_dirs()
        try:
            self._mount_layer_paths(snapshot.layer_paths)
        except Exception:
            self._release_lease(snapshot.lease_id)
            raise
        self._active_lease_id = snapshot.lease_id
        self._mounted = True
        self._mark_active(snapshot.manifest)
        return snapshot.manifest

    def _mount_layer_paths(self, layer_paths: tuple[Path, ...]) -> None:
        if self._layer_stack is None:
            raise RuntimeError("mount requires layer_stack")
        mount_inputs = validate_mount_inputs(
            workspace_root=Path(self.workspace_root),
            layer_paths=layer_paths,
            upperdir=self._upperdir,
            workdir=self._workdir,
        )
        try:
            mount_overlay(
                workspace_root=mount_inputs.workspace_root,
                layer_paths=mount_inputs.layer_paths,
                upperdir=mount_inputs.upperdir,
                workdir=mount_inputs.workdir,
            )
        finally:
            mount_inputs.close()

    def _prepare_overlay_snapshot(self, invocation_id: str) -> _OverlaySnapshot:
        if self._layer_stack is None:
            raise RuntimeError("snapshot requires layer_stack")
        snapshot = self._layer_stack.prepare_workspace_snapshot(
            request_id=invocation_id,
        )
        raw_paths = getattr(snapshot, "layer_paths", None)
        lease_id = str(getattr(snapshot, "lease_id", ""))
        if raw_paths is None:
            self._release_lease(lease_id)
            raise RuntimeError("overlay snapshot did not provide layer paths")
        return _OverlaySnapshot(
            lease_id=lease_id,
            manifest=getattr(snapshot, "manifest"),
            layer_paths=tuple(Path(path) for path in raw_paths),
        )

    def _release_lease(self, lease_id: str) -> None:
        if not lease_id or self._layer_stack is None:
            return
        if not self._lease_guard.mark_released(lease_id):
            return
        self._layer_stack.release_lease(lease_id=lease_id)

    def _relative_workspace_path(self, path: str) -> str:
        raw = str(path or "").strip()
        if not raw:
            raise ValueError("workspace path must not be empty")
        full = Path(raw)
        root = Path(self.workspace_root)
        if not full.is_absolute():
            return full.as_posix().strip("/")
        try:
            return full.resolve(strict=False).relative_to(
                root.resolve(strict=False)
            ).as_posix()
        except ValueError:
            raise ValueError(f"path is outside workspace root: {path}") from None

    def _start_foreign_publish_watcher(self) -> None:
        if self._foreign_watch_task is not None and not self._foreign_watch_task.done():
            return
        self._foreign_watch_task = asyncio.create_task(
            self._watch_foreign_publishes()
        )

    async def _stop_foreign_publish_watcher(self) -> None:
        task = self._foreign_watch_task
        self._foreign_watch_task = None
        if task is None:
            return
        task.cancel()
        with suppress(asyncio.CancelledError):
            await task

    async def _watch_foreign_publishes(self) -> None:
        interval = foreign_watch_interval_s()
        while True:
            await asyncio.sleep(interval)
            if not self._mounted:
                return
            async with self._operation_lock:
                await self.ensure_current(reason="foreign_watch")


__all__ = [
    "OperationOverlayHandle",
    "EphemeralPipeline",
    "OverlayLayerStackClient",
    "clear_overlay_manager_for_tests",
    "get_sandbox_overlay",
    "stop_all_overlays",
    "stop_sandbox_overlay",
]
