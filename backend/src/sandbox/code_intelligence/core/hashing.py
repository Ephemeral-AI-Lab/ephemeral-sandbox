"""Shared hashing helpers for code-intelligence content snapshots."""

from __future__ import annotations

import hashlib


def content_hash(content: str) -> str:
    """Return the short stable digest used for text content snapshots."""
    return hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]
