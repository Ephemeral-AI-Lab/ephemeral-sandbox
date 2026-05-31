"""Snapshot/lease/read-manifest slice of the layer stack that workspace pipelines need.

The iws pipeline binds a :class:`sandbox.occ.layer_stack_adapter.LayerStackPortAdapter`
at construction time, so both eph and iws speak the same kwarg-only contract
defined here.
"""

from __future__ import annotations

from pathlib import Path
from typing import Protocol

from sandbox.shared.command_exec_contract import (
    SnapshotManifest,
    WorkspaceSnapshotLease,
)


class LayerStackSnapshotPort(Protocol):
    """Layer-stack surface a workspace pipeline needs.

    The kwarg-only signature lets concrete implementations (e.g.
    ``LayerStackPortAdapter`` wrapping the in-process ``LayerStack``) keep their
    own positional-arg internal call shape without leaking it through this
    Protocol.
    """

    storage_root: Path

    def acquire_snapshot(
        self,
        *,
        request_id: str,
    ) -> WorkspaceSnapshotLease: ...

    def release_lease(self, *, lease_id: str) -> bool: ...

    def read_active_manifest(self) -> SnapshotManifest: ...


__all__ = ["LayerStackSnapshotPort"]
