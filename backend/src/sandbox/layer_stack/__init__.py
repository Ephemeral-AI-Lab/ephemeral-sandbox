"""Append-only sandbox layer-stack storage primitives."""

from __future__ import annotations

from sandbox.layer_stack.changes import (
    DeleteLayerChange,
    LayerChange,
    OpaqueDirLayerChange,
    SymlinkLayerChange,
    WriteLayerChange,
    normalize_layer_path,
)
from sandbox.layer_stack.commit_staging import CommitStagingArea
from sandbox.layer_stack.stack import (
    LayerStack,
    LayerStackSnapshotLease,
)
from sandbox.layer_stack.manifest import (
    LayerRef,
    Manifest,
    ManifestConflictError,
)
from sandbox.layer_stack.workspace_binding import (
    WorkspaceBinding,
    WorkspaceBindingError,
    read_workspace_binding,
    require_workspace_binding,
)


__all__ = [
    "CommitStagingArea",
    "DeleteLayerChange",
    "LayerChange",
    "LayerRef",
    "LayerStack",
    "LayerStackSnapshotLease",
    "Manifest",
    "ManifestConflictError",
    "OpaqueDirLayerChange",
    "SymlinkLayerChange",
    "WorkspaceBinding",
    "WorkspaceBindingError",
    "WriteLayerChange",
    "normalize_layer_path",
    "read_workspace_binding",
    "require_workspace_binding",
]
