"""Shared types for the daemon-owned ephemeral workspace pipeline."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Protocol

from sandbox._shared.shell_contract import SnapshotManifest

if TYPE_CHECKING:
    from sandbox.ephemeral_workspace.pipeline import EphemeralPipeline


class OverlayLayerStackClient(Protocol):
    storage_root: Path

    def read_active_manifest(self) -> SnapshotManifest: ...

    def prepare_workspace_snapshot(
        self,
        *,
        request_id: str,
    ) -> object: ...

    def release_lease(self, *, lease_id: str) -> bool: ...

    def flush_to_workspace(
        self,
        *,
        workspace_root: str | Path,
        timings: dict[str, float] | None = None,
    ) -> SnapshotManifest: ...


@dataclass(frozen=True)
class _OverlaySnapshot:
    lease_id: str
    manifest: SnapshotManifest
    layer_paths: tuple[Path, ...]


@dataclass
class OperationOverlayHandle:
    """Daemon-owned lease plus private upper/work dirs for one operation."""

    lease_id: str
    manifest_key: str
    manifest_version: int
    root_hash: str
    manifest: SnapshotManifest
    workspace_root: str
    run_dir: str
    upperdir: str
    workdir: str
    layer_paths: tuple[str, ...] | None
    _overlay: EphemeralPipeline
    _released: bool = False

    def release(self) -> None:
        if self._released:
            return
        self._released = True
        self._overlay.release_operation_overlay(self)

    @property
    def released(self) -> bool:
        return self._released


__all__ = [
    "OperationOverlayHandle",
    "OverlayLayerStackClient",
    "_OverlaySnapshot",
]
