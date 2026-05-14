"""Factory for default overlay runtime dependencies."""

from __future__ import annotations

from sandbox.layer_stack.manager import LayerStackManager
from sandbox.overlay.invoker import OverlayInvoker, OverlayRuntimeInvoker


def create_overlay_invoker(layer_stack: LayerStackManager) -> OverlayInvoker:
    return OverlayRuntimeInvoker(storage_root=layer_stack.storage_root)


__all__ = ["create_overlay_invoker"]
