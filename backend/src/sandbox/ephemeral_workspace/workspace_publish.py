"""OCC publish helpers for captured workspace overlay changes."""

from __future__ import annotations

from collections.abc import Sequence
from pathlib import Path

from sandbox._shared.clock import monotonic_now
from sandbox._shared.models import ToolCallResult
from sandbox.daemon.workspace_tool.changeset_projection import (
    conflict_and_status,
    conflict_to_dict,
    published_paths,
)
from sandbox.ephemeral_workspace.events import (
    WorkspacePathChange,
    WorkspaceChangeEvent,
)
from sandbox._shared.command_exec_contract import (
    ChangesetResultLike,
    CommandExecRequest,
    SnapshotManifest,
    WorkspaceCapturePublishResult,
)
from sandbox.occ.changeset import (
    ChangesetResult,
    CommitOptions,
)
from sandbox.occ.overlay_change_conversion import overlay_path_changes_to_occ_changes
from sandbox.overlay.capture import walk_upperdir
from sandbox.overlay.path_change import OverlayPathChange


class WorkspacePublishMixin:
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
        payload["changed_path_kinds"] = {
            change.path: change.kind for change in path_changes
        }
        payload["mutation_source"] = source
        existing_conflict = payload.get("conflict")
        existing_conflict_reason = payload.get("conflict_reason")
        existing_status = payload.get("status")
        payload["conflict"] = (
            conflict_to_dict(conflict) if conflict is not None else existing_conflict
        )
        payload["conflict_reason"] = (
            conflict.message if conflict is not None else existing_conflict_reason
        )
        payload["status"] = status if conflict is not None else existing_status or "ok"
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
            run_maintenance=run_maintenance,
        )

    async def publish_pending_changes(
        self,
        *,
        snapshot: SnapshotManifest,
        run_maintenance: bool = True,
    ) -> WorkspaceCapturePublishResult:
        """Capture and publish the persistent overlay upperdir."""
        return await self._publish_upperdir(
            upperdir=self._upperdir,
            snapshot=snapshot,
            workspace_ref=self._workspace_ref,
            timing_prefix="overlay",
            run_maintenance=run_maintenance,
        )

    async def _publish_upperdir(
        self,
        *,
        upperdir: str | Path,
        snapshot: SnapshotManifest,
        workspace_ref: str,
        timing_prefix: str,
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
            self._remount_active(reason="publish")
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
                    reason="publish",
                    from_version=int(old_version),
                    to_version=self._active_manifest_version
                    or int(changeset.published_manifest_version or old_version),
                    changes=tuple(
                        WorkspacePathChange.from_overlay_change(change)
                        for change in path_changes
                    ),
                )
            )
        return WorkspaceCapturePublishResult(
            path_changes=path_changes,
            changeset=changeset,
            timings={**timings, **maintenance_timings},
        )

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


__all__ = ["WorkspacePublishMixin"]
