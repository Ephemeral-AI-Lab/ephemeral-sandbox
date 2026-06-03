"""Daemon-owned overlay lifecycle and publish boundary."""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
import hashlib
import os
from collections.abc import Sequence
from contextlib import asynccontextmanager, suppress
from pathlib import Path
import shutil
from typing import AsyncIterator

from sandbox._shared.async_bridge import run_sync_in_executor
from sandbox._shared.clock import monotonic_now
from sandbox._shared.layer_stack_port import LayerStackSnapshotPort
from sandbox._shared.lease_guard import LeaseGuard
from sandbox._shared.models import Intent, ToolCallRequest, ToolCallResult
from sandbox.ephemeral_workspace.operation_overlay import OperationOverlayMixin
from sandbox.ephemeral_workspace.workspace_publish import WorkspacePublishMixin
from sandbox.ephemeral_workspace.events import (
    WorkspaceChangeEventBus,
    WorkspaceChangeEvent,
)
from sandbox._shared.command_exec_contract import (
    OCCMutationClient,
    SnapshotManifest,
)
from sandbox.audit.schema import (
    OverlayWorkspaceSection,
    build_overlay_workspace_event,
    safe_emit,
)
from sandbox.layer_stack.manifest import manifest_root_hash
from sandbox.overlay import lifecycle as overlay_lifecycle
from sandbox.overlay.handle import OverlayHandle
from sandbox.overlay.kernel_mount import (
    mount_overlay,
    umount,
    validate_mount_inputs,
)
from sandbox.overlay.namespace_runner import run_in_namespace
from sandbox.overlay.path_change import OverlayPathChange
from sandbox.overlay.writable_dirs import overlay_writable_root


@dataclass(frozen=True)
class _PreparedOverlaySnapshot:
    lease_id: str
    manifest: SnapshotManifest
    layer_paths: tuple[Path, ...]


class EphemeralPipeline(OperationOverlayMixin, WorkspacePublishMixin):
    """Facade hiding overlay freshness, capture, and OCC behind the daemon boundary.

    Audit/event divergence vs ``IsolatedPipeline``:
        ``EphemeralPipeline`` uses ``events.WorkspaceChangeEvent`` via the
        in-process ``event_bus.emit()`` — this is RUNTIME CONTROL FLOW
        consumed by ``_watch_foreign_publishes``, not audit. For lifecycle
        audit, see ``IsolatedPipeline``'s ``_JsonlAuditSink`` pattern in
        ``isolated_workspace._control_plane.pipeline_registry``.
    """

    def __init__(
        self,
        *,
        occ_client: OCCMutationClient,
        workspace_ref: str,
        layer_stack: LayerStackSnapshotPort | None = None,
        workspace_root: str = "/testbed",
        event_bus: WorkspaceChangeEventBus | None = None,
    ) -> None:
        self._occ_client = occ_client
        self._workspace_ref = workspace_ref
        self._layer_stack = layer_stack
        self._workspace_root = workspace_root.rstrip("/") or "/"
        self.event_bus = event_bus or WorkspaceChangeEventBus()
        self._active_manifest_key = ""
        self._active_manifest_version = 0
        self._mounted = False
        self._active_lease_id = ""
        self._operation_lock = asyncio.Lock()
        self._shell_mount_maintenance_lock = asyncio.Lock()
        self._foreign_watch_task: asyncio.Task[None] | None = None
        self._lease_guard = LeaseGuard()
        self._writable_root = overlay_writable_root()
        self._runtime_dir_path = (
            self._writable_root
            / "runtime"
            / "sandbox-overlay"
            / _pipeline_runtime_key(workspace_ref, self._workspace_root)
        )
        self._upperdir = self._runtime_dir_path / "upper"
        self._workdir = self._runtime_dir_path / "work"
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
        total_start = monotonic_now()
        pre_mount_timings: dict[str, float] = {}
        if req.verb == "exec_command":
            pre_mount_timings = await self._run_shell_pre_mount_maintenance()
        handle = overlay_lifecycle.acquire(
            self._layer_stack,
            invocation_id=f"overlay:{req.agent_id}:{req.invocation_id}",
            workspace_root=self._workspace_root,
        )
        try:
            path_changes: Sequence[OverlayPathChange] = ()
            capture_upperdir_s = 0.0
            result = await run_in_namespace(handle, req)
            if req.intent == Intent.WRITE_ALLOWED:
                capture_start = monotonic_now()
                path_changes = await overlay_lifecycle.capture_changes(handle)
                capture_upperdir_s = monotonic_now() - capture_start
                paths = {change.path for change in path_changes}
                source = (
                    "api_write"
                    if req.verb in {"write_file", "edit_file"} and len(paths) == 1
                    else "overlay_capture"
                )
                publish_started = monotonic_now()
                result = await self._commit_and_attach(
                    result,
                    path_changes=path_changes,
                    snapshot=handle.snapshot_manifest,
                    source=source,
                )
                committed_layer_id = None
                committed_section = result.get("committed") if isinstance(result, dict) else None
                if isinstance(committed_section, dict):
                    committed_layer_id = (
                        committed_section.get("committed_layer_id")
                        or committed_section.get("layer_id")
                        or None
                    )
                safe_emit(
                    build_overlay_workspace_event(
                        "overlay_workspace.published",
                        OverlayWorkspaceSection(
                            operation_id=handle.operation_id or None,
                            workspace_handle_id=handle.lease_id or None,
                            lease_id=handle.lease_id or None,
                            manifest_root_hash=handle.root_hash or None,
                            committed_layer_id=committed_layer_id,
                            publish_layer_ms=(monotonic_now() - publish_started) * 1000.0,
                            changed_path_count=len(path_changes),
                            upperdir_bytes=_upperdir_total_bytes(handle.upperdir),
                        ),
                    ),
                    lane="critical",
                )
            result = self._attach_operation_timing_aliases(
                result,
                req=req,
                handle=handle,
                capture_upperdir_s=capture_upperdir_s,
                extra_timings=pre_mount_timings,
                total_start=total_start,
            )
            return self._attach_resource_timings(
                result,
                handle=handle,
                changed_path_count=len(path_changes),
            )
        finally:
            await self._lease_guard.release(handle, overlay_lifecycle.release_overlay)

    def _attach_operation_timing_aliases(
        self,
        result: ToolCallResult,
        *,
        req: ToolCallRequest,
        handle: OverlayHandle,
        capture_upperdir_s: float,
        extra_timings: dict[str, float],
        total_start: float,
    ) -> ToolCallResult:
        payload = dict(result)
        timings = dict(
            payload.get("timings") if isinstance(payload.get("timings"), dict) else {}
        )
        timings.update(extra_timings)
        timings.update(handle.snapshot_timings)
        if "workspace.mount_s" in timings:
            timings.setdefault(
                "command_exec.mount_workspace_s",
                float(timings["workspace.mount_s"]),
            )
        if "workspace.tool_s" in timings:
            timings.setdefault(
                "command_exec.run_command_s",
                float(timings["workspace.tool_s"]),
            )
        timings.setdefault("command_exec.capture_upperdir_s", capture_upperdir_s)
        if "occ.apply.total_s" in timings:
            timings.setdefault(
                "command_exec.occ_apply_s",
                float(timings["occ.apply.total_s"]),
            )
        timings.setdefault("command_exec.total_s", monotonic_now() - total_start)
        api_total_key = _api_total_timing_key(req.verb)
        if api_total_key:
            timings.setdefault(api_total_key, timings["command_exec.total_s"])
        payload["timings"] = timings
        return payload

    async def _run_shell_pre_mount_maintenance(self) -> dict[str, float]:
        """Collapse deep manifests before shell enters the kernel mount path."""
        if self._layer_stack is None:
            return {}
        max_depth = _shell_mount_squash_max_depth()
        if max_depth <= 0:
            return {}
        async with self._shell_mount_maintenance_lock:
            active = self._layer_stack.read_active_manifest()
            depth_before = _manifest_depth(active)
            if depth_before <= max_depth:
                return {}
            squash_start = monotonic_now()
            squashed = await run_sync_in_executor(
                self._layer_stack.squash,
                max_depth=max_depth,
            )
            elapsed = monotonic_now() - squash_start
            depth_after = (
                _manifest_depth(squashed)
                if squashed is not None
                else _manifest_depth(self._layer_stack.read_active_manifest())
            )
            timings = {
                "layer_stack.shell_pre_mount_squash.total_s": elapsed,
                "layer_stack.shell_pre_mount_squash.max_depth": float(max_depth),
                "layer_stack.shell_pre_mount_squash.depth_before": float(depth_before),
                "layer_stack.shell_pre_mount_squash.depth_after": float(depth_after),
            }
            if squashed is None:
                timings["layer_stack.shell_pre_mount_squash.raced"] = 1.0
            return timings

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
        shutil.rmtree(self._runtime_dir_path, ignore_errors=True)

    def subscribe_workspace_changes(
        self, subscriber_id: str
    ) -> asyncio.Queue[WorkspaceChangeEvent]:
        """Subscribe to daemon-local workspace change events."""
        return self.event_bus.subscribe(subscriber_id)

    def unsubscribe_workspace_changes(self, subscriber_id: str) -> None:
        """Stop receiving daemon-local workspace change events."""
        self.event_bus.unsubscribe(subscriber_id)

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
        snapshot = self._lease_overlay_snapshot(f"sandbox-overlay-{reason}")
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

    def _lease_overlay_snapshot(self, invocation_id: str) -> _PreparedOverlaySnapshot:
        if self._layer_stack is None:
            raise RuntimeError("snapshot requires layer_stack")
        snapshot = self._layer_stack.acquire_snapshot(
            request_id=invocation_id,
        )
        raw_paths = getattr(snapshot, "layer_paths", None)
        lease_id = str(getattr(snapshot, "lease_id", ""))
        if raw_paths is None:
            self._release_lease(lease_id)
            raise RuntimeError("overlay snapshot did not provide layer paths")
        return _PreparedOverlaySnapshot(
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
        interval = _foreign_watch_interval_s()
        while True:
            await asyncio.sleep(interval)
            if not self._mounted:
                return
            async with self._operation_lock:
                await self.ensure_current(reason="foreign_watch")


__all__ = ["EphemeralPipeline"]


def _shell_mount_squash_max_depth() -> int:
    raw = os.environ.get("EOS_SHELL_MOUNT_SQUASH_MAX_DEPTH")
    if raw is None:
        return 64
    try:
        return max(0, int(raw))
    except ValueError:
        return 64


def _manifest_depth(manifest: object) -> int:
    return len(tuple(getattr(manifest, "layers", ()) or ()))


def _api_total_timing_key(verb: str) -> str:
    suffix = {
        "read_file": "read",
        "write_file": "write",
        "edit_file": "edit",
        "exec_command": "exec_command",
        "grep": "grep",
        "glob": "glob",
    }.get(verb)
    return f"api.{suffix}.total_s" if suffix else ""


def _foreign_watch_interval_s() -> float:
    raw = os.environ.get("EOS_OVERLAY_FOREIGN_WATCH_INTERVAL_S", "").strip()
    if not raw:
        return 0.25
    try:
        return max(0.05, float(raw))
    except ValueError:
        return 0.25


def _pipeline_runtime_key(workspace_ref: str, workspace_root: str) -> str:
    raw = f"{workspace_ref}\0{workspace_root}".encode("utf-8", "surrogateescape")
    return hashlib.sha256(raw).hexdigest()[:16]


def _upperdir_total_bytes(upperdir: Path) -> int | None:
    """Sum allocated bytes under ``upperdir`` (best-effort).

    Returns ``None`` on any failure so the consumer-side percentile record
    distinguishes "not sampled" from "0 bytes captured". Bounded by
    ``EOS_OVERLAY_UPPERDIR_SAMPLE_ENTRY_LIMIT`` (default 5000) to keep the
    per-call cost within the V3 §Gate matrix latency budget.
    """
    if not upperdir.exists():
        return 0
    try:
        max_entries = _upperdir_sample_entry_limit()
        total = 0
        seen = 0
        for root, _dirs, files in os.walk(upperdir):
            for name in files:
                seen += 1
                if seen > max_entries:
                    return total
                try:
                    st = os.lstat(os.path.join(root, name))
                except OSError:
                    continue
                blocks = getattr(st, "st_blocks", None)
                total += int(blocks) * 512 if blocks is not None else int(st.st_size)
        return total
    except OSError:
        return None


def _upperdir_sample_entry_limit() -> int:
    raw = os.environ.get("EOS_OVERLAY_UPPERDIR_SAMPLE_ENTRY_LIMIT", "").strip()
    if not raw:
        return 5000
    try:
        return max(0, int(raw))
    except ValueError:
        return 5000
