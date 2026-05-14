"""Layer-stack domain errors."""

from __future__ import annotations


class ManifestConflictError(RuntimeError):
    """Raised when an active-manifest compare-and-swap check fails."""


class LayerStackStorageError(RuntimeError):
    """Raised when a manifest references missing or invalid layer storage."""

    def __init__(self, message: str, *, layer_id: str | None = None) -> None:
        super().__init__(message)
        self.layer_id = layer_id


__all__ = ["LayerStackStorageError", "ManifestConflictError"]
