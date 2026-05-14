"""Append-only sandbox layer-stack storage primitives."""

from __future__ import annotations

from sandbox.layer_stack.layer.change import (
    DeleteLayerChange,
    LayerChange,
    LayerDelta,
    OpaqueDirLayerChange,
    SymlinkLayerChange,
    WriteLayerChange,
    aggregate_layer_changes,
    make_layer_change,
    normalize_layer_path,
)
from sandbox.layer_stack.errors import LayerStackStorageError
from sandbox.layer_stack.manifest import (
    LayerRef,
    MANIFEST_SCHEMA_VERSION,
    Manifest,
    ManifestConflictError,
)
from sandbox.layer_stack.manager import (
    LayerStackManager,
    PrepareWorkspaceSnapshotResult,
)

__all__ = [
    "LayerChange",
    "LayerDelta",
    "LayerRef",
    "LayerStackStorageError",
    "LayerStackManager",
    "MANIFEST_SCHEMA_VERSION",
    "Manifest",
    "ManifestConflictError",
    "DeleteLayerChange",
    "OpaqueDirLayerChange",
    "PrepareWorkspaceSnapshotResult",
    "SymlinkLayerChange",
    "WriteLayerChange",
    "aggregate_layer_changes",
    "make_layer_change",
    "normalize_layer_path",
]
