"""OCC publish and workspace-capture helpers for EphemeralPipeline."""

from __future__ import annotations

from collections.abc import Sequence
from pathlib import Path

from sandbox._shared.clock import monotonic_now
from sandbox._shared.models import ToolCallResult
from sandbox.daemon.result_projection import (
    conflict_and_status,
    conflict_to_dict,
    published_paths,
)
from sandbox.ephemeral_workspace._utils import event_path_change
from sandbox.ephemeral_workspace.events import (
    PathChange,
    WorkspaceChangeEvent,
)
from sandbox._shared.shell_contract import (
    ChangesetResultLike,
    CommandExecRequest,
    SnapshotManifest,
    WorkspaceCapturePublishResult,
)
from sandbox.occ.changeset import (
    ChangesetResult,
    CommitOptions,
    DeleteChange,
    WriteChange,
    WritePayload,
)
from sandbox.occ.overlay_change_conversion import overlay_path_changes_to_occ_changes
from sandbox.overlay.capture import walk_upperdir
from sandbox.overlay.path_change import OverlayPathChange


class EphemeralPublishMixin:
    async def _commit_and_attach(
        self,
        result: ToolCallResult,
        *,
        path_changes: Sequence[OverlayPathChange],
        snapshot: SnapshotManifest | None,
        source: str,
    ) -> ToolCallResult:
        changeset = await self._apply_workspace_capture(
            path_changes,
            snapshot=snapshot,
            workspace_ref=self._workspace_ref,
            source=source,
            run_maintenance=False,
        )
        maintenance_timings = await self.run_maintenance_after_publish(
            changeset,
            workspace_ref=self._workspace_ref,
        )
        conflict, status = conflict_and_status(getattr(changeset, "files", ()))
        payload = dict(result)
        timings = dict(
            payload.get("timings") if isinstance(payload.get("timings"), dict) else {}
        )
        timings.update(getattr(changeset, "timings", {}) or {})
        timings.update(maintenance_timings)
        payload["timings"] = timings
        payload["changed_paths"] = list(published_paths(getattr(changeset, "files", ())))
        payload["conflict"] = conflict_to_dict(conflict)
        payload["conflict_reason"] = conflict.message if conflict is not None else None
        payload["status"] = "ok" if conflict is None else status
        payload["success"] = bool(payload.get("success", True)) and conflict is None
        return payload

    async def publish_cycle(
        self,
        *,
        request: CommandExecRequest,
        upperdir: str | Path,
        snapshot: SnapshotManifest,
        run_maintenance: bool = True,
    ) -> WorkspaceCapturePublishResult:
        return await self._publish_upperdir(
            upperdir=upperdir,
            snapshot=snapshot,
            workspace_ref=request.workspace_ref,
            timing_prefix="command_exec",
            reason="publish",
            run_maintenance=run_maintenance,
        )

    async def publish_pending_changes(
        self,
        *,
        snapshot: SnapshotManifest,
        reason: str = "publish",
        run_maintenance: bool = True,
    ) -> WorkspaceCapturePublishResult:
        """Capture and publish the persistent overlay upperdir."""
        return await self._publish_upperdir(
            upperdir=self._upperdir,
            snapshot=snapshot,
            workspace_ref=self._workspace_ref,
            timing_prefix="overlay",
            reason=reason,
            run_maintenance=run_maintenance,
        )

    async def flush_to_workspace(self) -> dict[str, object]:
        """Publish pending upperdir edits, detach, rebuild base, and remount."""
        if self._layer_stack is None:
            raise RuntimeError("flush_to_workspace requires layer_stack")
        async with self._operation_lock:
            timings: dict[str, float] = {}
            was_mounted = self._mounted
            from_version = self._active_manifest_version
            if was_mounted:
                snapshot = self.current_manifest()
                publish = await self.publish_pending_changes(
                    snapshot=snapshot,
                    reason="flush",
                    run_maintenance=True,
                )
                timings.update(publish.timings)
                await self.stop()
            new_manifest = self._layer_stack.flush_to_workspace(
                workspace_root=self.workspace_root,
                timings=timings,
            )
            self._mark_active(new_manifest)
            if was_mounted:
                await self.start()
            self.event_bus.emit(
                WorkspaceChangeEvent(
                    reason="flush",
                    from_version=from_version,
                    to_version=self._active_manifest_version,
                    changes=(),
                )
            )
            return {
                "success": True,
                "manifest_version": self._active_manifest_version,
                "manifest_key": self._active_manifest_key,
                "timings": timings,
            }

    async def _publish_upperdir(
        self,
        *,
        upperdir: str | Path,
        snapshot: SnapshotManifest,
        workspace_ref: str,
        timing_prefix: str,
        reason: str,
        run_maintenance: bool = True,
    ) -> WorkspaceCapturePublishResult:
        timings: dict[str, float] = {}
        capture_start = monotonic_now()
        path_changes = walk_upperdir(upperdir, timings=timings)
        timings[f"{timing_prefix}.capture_upperdir_s"] = (
            monotonic_now() - capture_start
        )

        occ_start = monotonic_now()
        changeset = await self._apply_workspace_capture(
            path_changes,
            snapshot=snapshot,
            workspace_ref=workspace_ref,
            run_maintenance=False,
        )
        timings[f"{timing_prefix}.occ_apply_s"] = monotonic_now() - occ_start
        maintenance_timings: dict[str, float] = {}
        old_version = getattr(snapshot, "version", self._active_manifest_version)
        if changeset.published_manifest_version is not None and run_maintenance:
            maintenance_timings = await self.run_maintenance_after_publish(
                changeset,
                workspace_ref=workspace_ref,
            )
        elif changeset.published_manifest_version is not None and self._mounted:
            self._remount_active(reason=reason)
        elif (
            changeset.published_manifest_version is not None
            and self._layer_stack is not None
            and hasattr(self._layer_stack, "read_active_manifest")
        ):
            self._mark_active(self._layer_stack.read_active_manifest())
        elif changeset.published_manifest_version is not None:
            self._active_manifest_version = int(changeset.published_manifest_version)
            self._active_manifest_key = f"unknown@{self._active_manifest_version}"
        if path_changes:
            self.event_bus.emit(
                WorkspaceChangeEvent(
                    reason=reason if reason in {"publish", "flush"} else "publish",
                    from_version=int(old_version),
                    to_version=self._active_manifest_version
                    or int(changeset.published_manifest_version or old_version),
                    changes=tuple(event_path_change(change) for change in path_changes),
                )
            )
        return WorkspaceCapturePublishResult(
            path_changes=path_changes,
            changeset=changeset,
            timings={**timings, **maintenance_timings},
        )

    async def publish_workspace_paths(
        self,
        *,
        paths: Sequence[str],
        agent_id: str = "",
        description: str = "plugin workspace edit",
    ) -> ChangesetResult:
        """Publish direct writes made under the daemon overlay workspace root."""
        del agent_id, description
        if self._mounted:
            snapshot = self.current_manifest()
            publish = await self.publish_pending_changes(
                snapshot=snapshot,
                reason="publish",
                run_maintenance=True,
            )
            return publish.changeset
        if self._layer_stack is None:
            raise RuntimeError("publish_workspace_paths requires layer_stack")
        snapshot = self._layer_stack.read_active_manifest()
        old_version = int(getattr(snapshot, "version", self._active_manifest_version))
        changes = []
        event_changes: list[PathChange] = []
        for path in paths:
            rel = self._relative_workspace_path(path)
            full_path = Path(self.workspace_root) / rel
            if full_path.exists() or full_path.is_symlink():
                changes.append(
                    WriteChange(
                        path=rel,
                        source="overlay_capture",
                        payload=WritePayload(content_path=full_path.as_posix()),
                    )
                )
                event_changes.append(
                    PathChange(path=rel, kind="write", existed_before=True)
                )
            else:
                changes.append(DeleteChange(path=rel, source="overlay_capture"))
                event_changes.append(
                    PathChange(path=rel, kind="delete", existed_before=True)
                )
        if not changes:
            return ChangesetResult(
                files=(),
                timings={},
                published_manifest_version=None,
            )
        result = await self._occ_client.apply_changeset(
            tuple(changes),
            snapshot=snapshot,
            options=CommitOptions(atomic=len({change.path for change in changes}) > 1),
            workspace_ref=self._workspace_ref,
            run_maintenance=False,
        )
        await self.run_maintenance_after_publish(
            result,
            workspace_ref=self._workspace_ref,
        )
        if result.published_manifest_version is not None:
            self._mark_active(self._layer_stack.read_active_manifest())
            self.event_bus.emit(
                WorkspaceChangeEvent(
                    reason="publish",
                    from_version=old_version,
                    to_version=self._active_manifest_version,
                    changes=tuple(event_changes),
                )
            )
        return result

    async def run_maintenance_after_publish(
        self,
        result: ChangesetResultLike,
        *,
        workspace_ref: str | None = None,
    ) -> dict[str, float]:
        published = getattr(result, "published_manifest_version", None)
        if published is None:
            return await self._occ_client.run_maintenance_after_publish(
                result,
                workspace_ref=workspace_ref or self._workspace_ref,
            )
        was_mounted = self._mounted
        if was_mounted:
            self._detach_active_mount()
        try:
            return await self._occ_client.run_maintenance_after_publish(
                result,
                workspace_ref=workspace_ref or self._workspace_ref,
            )
        finally:
            if was_mounted:
                self._mount_active(reason="maintenance")
            elif self._layer_stack is not None:
                self._mark_active(self._layer_stack.read_active_manifest())

    async def _apply_workspace_capture(
        self,
        path_changes: Sequence[OverlayPathChange],
        *,
        snapshot: SnapshotManifest | None,
        workspace_ref: str,
        source: str = "overlay_capture",
        run_maintenance: bool = True,
    ) -> ChangesetResult:
        typed_changes = overlay_path_changes_to_occ_changes(path_changes, source=source)
        if not typed_changes:
            return ChangesetResult(
                files=(),
                timings={},
                published_manifest_version=None,
            )
        distinct_paths = {change.path for change in typed_changes}
        return await self._occ_client.apply_changeset(
            typed_changes,
            snapshot=snapshot,
            options=CommitOptions(atomic=len(distinct_paths) > 1),
            workspace_ref=workspace_ref,
            run_maintenance=run_maintenance,
        )


__all__ = ["EphemeralPublishMixin"]
