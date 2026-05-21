"""Plugin-agnostic, lease-backed workspace projection.

Stateful plugins (e.g. LSP) need a real on-disk filesystem tree to point a
language server at, but the workspace truth is the active layer-stack
manifest — not the mutable provider workspace directory. This module
materializes the active manifest into a transient lowerdir and exposes a
``manifest_key`` so plugin sessions can detect when the manifest changes and
refresh their state against the new snapshot.

Lives under ``sandbox/plugin/`` rather than the LSP catalog so future stateful
plugins reuse it. It MUST stay plugin-agnostic — no plugin-name string
switches, no LSP-specific code paths.
"""

from __future__ import annotations

import shutil
from dataclasses import dataclass
from pathlib import Path
from uuid import uuid4
from typing import TYPE_CHECKING

from sandbox.layer_stack.stack import LayerStack, PrepareWorkspaceSnapshotResult
from sandbox.layer_stack.manifest import manifest_root_hash
from sandbox.layer_stack.paths import TRANSIENT_LOWERDIR_DIR
from sandbox.layer_stack.view import LayerStackStorageError
from sandbox.execution.scratch import command_exec_scratch_root

if TYPE_CHECKING:  # pragma: no cover
    pass

__all__ = [
    "OverlayProjectionHandle",
    "ProjectionHandle",
    "WorkspaceProjection",
    "build_manifest_key",
]


def build_manifest_key(root_hash: str, manifest_version: int) -> str:
    """Stable key for caching plugin sessions across manifest revisions."""
    return f"{root_hash}@{manifest_version}"


@dataclass
class ProjectionHandle:
    """Lease-backed view of the active layer-stack manifest."""

    lease_id: str
    manifest_key: str
    lowerdir: str | None
    manifest_version: int
    root_hash: str
    manifest: object | None
    layer_paths: tuple[str, ...] | None
    _manager: LayerStack
    _released: bool = False

    def release(self) -> None:
        """Release the underlying lease. Idempotent."""
        if self._released:
            return
        self._released = True
        self._manager.release_lease(self.lease_id)

    @property
    def released(self) -> bool:
        return self._released


@dataclass
class OverlayProjectionHandle:
    """Lease plus private upper/work dirs for one overlay-backed operation."""

    lease: ProjectionHandle
    workspace_root: str
    run_dir: str
    upperdir: str
    workdir: str

    @property
    def lease_id(self) -> str:
        return self.lease.lease_id

    @property
    def manifest_key(self) -> str:
        return self.lease.manifest_key

    @property
    def manifest_version(self) -> int:
        return self.lease.manifest_version

    @property
    def root_hash(self) -> str:
        return self.lease.root_hash

    @property
    def manifest(self) -> object | None:
        return self.lease.manifest

    @property
    def lowerdir(self) -> str | None:
        return self.lease.lowerdir

    @property
    def layer_paths(self) -> tuple[str, ...] | None:
        return self.lease.layer_paths

    def release(self) -> None:
        self.lease.release()
        shutil.rmtree(self.run_dir, ignore_errors=True)

    @property
    def released(self) -> bool:
        return self.lease.released


class WorkspaceProjection:
    """Wrapper around :class:`LayerStack` for plugin runtime ops.

    Constructor takes a layer_stack_root (filesystem path); each acquire call
    materializes the active manifest into a transient lowerdir and returns a
    :class:`ProjectionHandle` keyed by ``manifest_key``. Plugin runtime code can
    use that key to decide whether a long-lived session is already current or
    must reconcile itself with the latest projection.
    """

    def __init__(
        self,
        layer_stack_root: str | Path,
        *,
        manager: LayerStack | None = None,
    ) -> None:
        self._layer_stack_root = Path(layer_stack_root).resolve()
        # Reuse the daemon's cached LayerStack when one is injected so
        # the plugin path and the OCC backend share a single writer flock +
        # transaction RLock. Constructing a fresh manager here is the legacy
        # path retained for unit tests and out-of-daemon callers.
        self._manager = (
            manager if manager is not None else LayerStack(self._layer_stack_root)
        )

    @property
    def layer_stack_root(self) -> Path:
        return self._layer_stack_root

    def acquire(
        self,
        owner_request_id: str,
        *,
        lowerdir_root: str | Path | None = None,
        materialize: bool = True,
    ) -> ProjectionHandle:
        result = self._prepare_snapshot_with_retry(
            owner_request_id,
            lowerdir_root=lowerdir_root,
            materialize=materialize,
        )
        return ProjectionHandle(
            lease_id=result.lease_id,
            manifest_key=build_manifest_key(
                result.root_hash, result.manifest_version
            ),
            lowerdir=result.lowerdir,
            manifest_version=result.manifest_version,
            root_hash=result.root_hash,
            manifest=getattr(result, "manifest", None),
            layer_paths=getattr(result, "layer_paths", None),
            _manager=self._manager,
        )

    def acquire_overlay(
        self,
        owner_request_id: str,
        *,
        workspace_root: str,
        materialize: bool = False,
    ) -> OverlayProjectionHandle:
        scratch_root = command_exec_scratch_root(self._manager.storage_root)
        run_dir = (
            scratch_root
            / "runtime"
            / "plugin_overlay"
            / f"{_safe_request_part(owner_request_id)}-{uuid4().hex[:8]}"
        )
        lease = self.acquire(
            owner_request_id,
            lowerdir_root=scratch_root / "runtime" / TRANSIENT_LOWERDIR_DIR,
            materialize=materialize,
        )
        upperdir = run_dir / "upper"
        workdir = run_dir / "work"
        upperdir.mkdir(parents=True, exist_ok=True)
        workdir.mkdir(parents=True, exist_ok=True)
        return OverlayProjectionHandle(
            lease=lease,
            workspace_root=str(workspace_root).rstrip("/") or "/",
            run_dir=run_dir.as_posix(),
            upperdir=upperdir.as_posix(),
            workdir=workdir.as_posix(),
        )

    def _prepare_snapshot_with_retry(
        self,
        owner_request_id: str,
        *,
        lowerdir_root: str | Path | None,
        materialize: bool,
    ) -> PrepareWorkspaceSnapshotResult:
        try:
            return _prepare_snapshot(
                self._manager,
                owner_request_id=owner_request_id,
                lowerdir_root=lowerdir_root,
                materialize=materialize,
            )
        except (FileNotFoundError, LayerStackStorageError):
            return _prepare_snapshot(
                self._manager,
                owner_request_id=owner_request_id,
                lowerdir_root=lowerdir_root,
                materialize=materialize,
            )

    def active_manifest_key(self) -> str:
        manifest = self._manager.read_active_manifest()
        return build_manifest_key(
            manifest_root_hash(manifest), manifest.version
        )

    def active_lease_count(self) -> int:
        return self._manager.active_lease_count()


def _prepare_snapshot(
    manager: LayerStack,
    *,
    owner_request_id: str,
    lowerdir_root: str | Path | None,
    materialize: bool,
) -> PrepareWorkspaceSnapshotResult:
    try:
        return manager.prepare_workspace_snapshot(
            owner_request_id=owner_request_id,
            lowerdir_root=lowerdir_root,
            materialize=materialize,
        )
    except TypeError:
        # Older test doubles only accepted owner_request_id. Keep the
        # projection contract duck-typed without forcing every focused test to
        # emulate layer-stack snapshot options.
        return manager.prepare_workspace_snapshot(
            owner_request_id=owner_request_id,
        )


def _safe_request_part(value: str) -> str:
    safe = "".join(
        char if char.isalnum() or char in ("-", "_") else "-"
        for char in str(value)
    ).strip("-")
    return safe or "plugin"
