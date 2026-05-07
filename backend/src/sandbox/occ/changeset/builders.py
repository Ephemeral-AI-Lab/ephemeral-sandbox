"""Source-to-changeset converters for OCC mutation sources."""

from __future__ import annotations

from sandbox.occ.changeset.types import (
    DeleteChange,
    EditChange,
    WriteChange,
)


def build_api_write_change(
    *,
    path: str,
    final_content: bytes | str,
    base_hash: str | None = None,
    create_only: bool = False,
) -> WriteChange:
    """Build a source-tagged write change from the host write API."""
    return WriteChange(
        path=path,
        source="api_write",
        final_content=final_content,
        base_hash=base_hash,
        create_only=create_only,
    )


def build_api_edit_change(
    *,
    path: str,
    old_text: str,
    new_text: str,
    expected_occurrences: int = 1,
) -> EditChange:
    """Build a source-tagged edit change from the host edit API."""
    return EditChange(
        path=path,
        source="api_edit",
        old_text=old_text,
        new_text=new_text,
        expected_occurrences=expected_occurrences,
    )


def build_api_delete_change(*, path: str, base_hash: str) -> DeleteChange:
    """Build a source-tagged delete change from a host delete API."""
    return DeleteChange(path=path, source="api_write", base_hash=base_hash)


def build_overlay_write_change(
    *,
    path: str,
    final_content: bytes | None = None,
    content_path: str | None = None,
    precomputed_hash: str | None = None,
) -> WriteChange:
    """Build an overlay-captured full-file write without a caller base hash.

    Phase 3 improvement #2: when ``content_path`` and ``precomputed_hash``
    are supplied (the overlay-capture pipeline already computed them),
    the bytes stay on disk and are streamed kernel-to-kernel by the OCC
    stager. ``final_content`` becomes the legacy bytes-based path for
    callers that haven't migrated.
    """
    if final_content is None and content_path is None:
        raise ValueError(
            "build_overlay_write_change needs final_content or content_path"
        )
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
    "build_api_delete_change",
    "build_api_edit_change",
    "build_api_write_change",
    "build_overlay_delete_change",
    "build_overlay_write_change",
]
