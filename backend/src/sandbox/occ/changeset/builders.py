"""Source-to-changeset converters for OCC mutation sources."""

from __future__ import annotations

from sandbox.occ.changeset.types import (
    DeleteChange,
    WriteChange,
)


def build_api_write_change(
    *,
    path: str,
    final_content: bytes | str,
    base_hash: str | None = None,
) -> WriteChange:
    """Build a source-tagged write change from the host write API."""
    return WriteChange(
        path=path,
        source="api_write",
        final_content=final_content,
        base_hash=base_hash,
    )


def build_overlay_write_change(
    *,
    path: str,
    final_content: bytes | None = None,
    content_path: str | None = None,
    precomputed_hash: str | None = None,
) -> WriteChange:
    """Build an overlay-captured full-file write without a caller base hash.

    When ``content_path`` and ``precomputed_hash`` are supplied, the
    bytes stay on disk and the OCC stager streams them kernel-to-kernel.
    ``final_content`` is the bytes-based fallback for callers that
    don't have a content path on disk.
    """
    if final_content is None and content_path is None:
        raise ValueError("build_overlay_write_change needs final_content or content_path")
    return WriteChange(
        path=path,
        source="overlay_capture",
        final_content=final_content,
        base_hash=None,
        content_path=content_path,
        precomputed_hash=precomputed_hash,
    )


def build_overlay_delete_change(
    *,
    path: str,
    base_hash: str | None = None,
) -> DeleteChange:
    """Build an overlay-captured delete whose base hash can be inferred later."""
    return DeleteChange(path=path, source="overlay_capture", base_hash=base_hash)


__all__ = [
    "build_api_write_change",
    "build_overlay_delete_change",
    "build_overlay_write_change",
]
