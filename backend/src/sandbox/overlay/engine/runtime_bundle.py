"""Build the Python runtime bundle uploaded for overlay commands."""

from __future__ import annotations

import io
import tarfile
from pathlib import Path

_OVERLAY_RUNTIME_BUNDLE_CACHE: bytes | None = None


def overlay_runtime_bundle_bytes() -> bytes:
    """Return a tar.gz containing the sandbox-side overlay runtime."""
    global _OVERLAY_RUNTIME_BUNDLE_CACHE
    if _OVERLAY_RUNTIME_BUNDLE_CACHE is not None:
        return _OVERLAY_RUNTIME_BUNDLE_CACHE

    root = Path(__file__).parents[1]
    runtime_dir = root / "runtime"
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as tar:
        for path in sorted(runtime_dir.rglob("*.py")):
            rel = path.relative_to(runtime_dir).as_posix()
            tar.add(path, arcname=f"overlay_runtime/{rel}")
    _OVERLAY_RUNTIME_BUNDLE_CACHE = buffer.getvalue()
    return _OVERLAY_RUNTIME_BUNDLE_CACHE


__all__ = ["overlay_runtime_bundle_bytes"]
