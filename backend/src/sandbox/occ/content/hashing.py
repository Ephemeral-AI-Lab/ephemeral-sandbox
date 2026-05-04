"""Stable content hashing for OCC merge validation."""

from __future__ import annotations

import hashlib


class ContentHasher:
    """Hash bytes with the layer-stack OCC hash policy."""

    def hash_bytes(self, content: bytes) -> str:
        return hashlib.sha256(content).hexdigest()

    def hash_current(self, content: bytes | None, *, exists: bool) -> str | None:
        if not exists or content is None:
            return None
        return self.hash_bytes(content)


__all__ = ["ContentHasher"]
