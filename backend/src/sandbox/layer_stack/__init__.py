"""Append-only sandbox layer-stack storage primitives."""

from __future__ import annotations

from sandbox.layer_stack.changes import LayerChange, LayerDelta
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
]
