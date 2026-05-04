"""Layer-stack backed content reads for OCC validation."""

from __future__ import annotations

from sandbox.layer_stack.manifest import Manifest
from sandbox.layer_stack.stack_manager import LayerStackManager


class LayerBackedContent:
    """Read path bytes from a specific layer-stack manifest."""

    def __init__(self, layer_stack: LayerStackManager) -> None:
        self._layer_stack = layer_stack

    def read_bytes(self, path: str, manifest: Manifest) -> tuple[bytes | None, bool]:
        """Return ``(content, exists)`` for *path* in *manifest*."""
        return self._layer_stack.read_bytes(path, manifest)


__all__ = ["LayerBackedContent"]
