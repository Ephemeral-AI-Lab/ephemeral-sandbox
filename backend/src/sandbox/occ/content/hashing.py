"""Stable content hashing for OCC merge validation."""

from __future__ import annotations

import hashlib

from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.ports import SnapshotReader


class ContentHasher:
    """Hash bytes with the layer-stack OCC hash policy."""

    def hash_bytes(self, content: bytes) -> str:
        return hashlib.sha256(content).hexdigest()

    def hash_current(self, content: bytes | None, *, exists: bool) -> str | None:
        if not exists or content is None:
            return None
        return self.hash_bytes(content)


def content_hash_bytes(content: bytes) -> str:
    """Return the layer-stack OCC hash for file bytes."""
    return ContentHasher().hash_bytes(content)


def infer_manifest_base_hash(
    *,
    snapshot_reader: SnapshotReader,
    manifest: Manifest,
    path: str,
) -> str | None:
    """Hash *path* content as it existed in a leased manifest."""
    content, exists = snapshot_reader.read_bytes(path, manifest)
    if not exists or content is None:
        return None
    return content_hash_bytes(content)


__all__ = [
    "ContentHasher",
    "content_hash_bytes",
    "infer_manifest_base_hash",
]
