"""Append-only sandbox layer-stack storage primitives."""

from __future__ import annotations

from sandbox.layer_stack.commit import CommitStagingArea
from sandbox.layer_stack.errors import LayerStackStorageError
from sandbox.layer_stack.layer_change import (
    DeleteLayerChange,
    LayerChange,
    LayerDelta,
    OpaqueDirLayerChange,
    SymlinkLayerChange,
    WriteLayerChange,
    aggregate_layer_changes,
    normalize_layer_path,
)
from sandbox.layer_stack.manager import (
    LayerStackManager,
    PrepareWorkspaceSnapshotResult,
)
from sandbox.layer_stack.manifest import (
    LayerRef,
    MANIFEST_SCHEMA_VERSION,
    Manifest,
    ManifestConflictError,
)
from sandbox.layer_stack.transaction import LayerStackTransaction
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
    "LayerDelta",
    "LayerRef",
    "LayerStackManager",
    "LayerStackStorageError",
    "LayerStackTransaction",
    "MANIFEST_SCHEMA_VERSION",
    "Manifest",
    "ManifestConflictError",
    "OpaqueDirLayerChange",
    "PrepareWorkspaceSnapshotResult",
    "SymlinkLayerChange",
    "WorkspaceBinding",
    "WorkspaceBindingError",
    "WriteLayerChange",
    "aggregate_layer_changes",
    "normalize_layer_path",
    "read_workspace_binding",
    "require_workspace_binding",
]
