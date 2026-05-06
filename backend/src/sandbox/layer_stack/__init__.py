"""Append-only sandbox layer-stack storage primitives."""

from __future__ import annotations

from sandbox.layer_stack.changes import LayerChange, LayerDelta, aggregate_layer_changes
from sandbox.layer_stack.workspace_base import (
    WORKSPACE_BASE_LAYER_ID,
    WorkspaceBaseAlreadyExistsError,
    WorkspaceBaseIncompleteError,
    build_workspace_base,
)
from sandbox.layer_stack.lease_budget import (
    BudgetDecision,
    LeaseBudgetWorker,
    LeaseSnapshot,
)
from sandbox.layer_stack.lease_registry import Lease, LeaseRegistry
from sandbox.layer_stack.manifest import (
    LayerRef,
    Manifest,
    ManifestConflictError,
)
from sandbox.layer_stack.merged_view import LayerStackStorageError, MergedView
from sandbox.layer_stack.publisher import CommitBackpressureError, LayerPublisher
from sandbox.layer_stack.squash import SquashPlan, SquashWorker
from sandbox.layer_stack.stack_manager import (
    FsckResult,
    GCMarkSet,
    LayerStackManager,
    LayerStackTransaction,
)
from sandbox.layer_stack.workspace import (
    WORKSPACE_BINDING_FILE,
    WorkspaceBinding,
    WorkspaceBindingError,
    read_workspace_binding,
    require_workspace_binding,
    workspace_binding_path,
    write_workspace_binding_atomic,
)

__all__ = [
    "BudgetDecision",
    "CommitBackpressureError",
    "FsckResult",
    "GCMarkSet",
    "LayerChange",
    "LayerDelta",
    "LayerPublisher",
    "LayerRef",
    "LayerStackManager",
    "LayerStackStorageError",
    "LayerStackTransaction",
    "Lease",
    "LeaseBudgetWorker",
    "LeaseRegistry",
    "LeaseSnapshot",
    "Manifest",
    "ManifestConflictError",
    "MergedView",
    "SquashPlan",
    "SquashWorker",
    "WORKSPACE_BINDING_FILE",
    "WORKSPACE_BASE_LAYER_ID",
    "WorkspaceBaseAlreadyExistsError",
    "WorkspaceBaseIncompleteError",
    "WorkspaceBinding",
    "WorkspaceBindingError",
    "aggregate_layer_changes",
    "build_workspace_base",
    "read_workspace_binding",
    "require_workspace_binding",
    "workspace_binding_path",
    "write_workspace_binding_atomic",
]
