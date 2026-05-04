"""Build a small runtime bundle for snapshot overlay shell execution."""

from __future__ import annotations

import io
import tarfile
from pathlib import Path

_BUNDLE_CACHE: bytes | None = None


def snapshot_overlay_runtime_bundle_bytes() -> bytes:
    """Return a tar.gz containing the Phase 02 overlay shell runtime."""
    global _BUNDLE_CACHE
    if _BUNDLE_CACHE is not None:
        return _BUNDLE_CACHE

    sandbox_dir = Path(__file__).resolve().parents[2]
    runtime_overlay_shell = sandbox_dir / "runtime" / "overlay_shell"
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as tar:
        tar.add(sandbox_dir / "__init__.py", arcname="sandbox/__init__.py")
        tar.add(
            sandbox_dir / "runtime" / "__init__.py",
            arcname="sandbox/runtime/__init__.py",
        )
        tar.add(
            sandbox_dir / "overlay" / "__init__.py",
            arcname="sandbox/overlay/__init__.py",
        )
        tar.add(sandbox_dir / "overlay" / "types.py", arcname="sandbox/overlay/types.py")
        for package in (
            sandbox_dir / "layer_stack",
            sandbox_dir / "overlay" / "capture",
            sandbox_dir / "overlay" / "namespace",
            runtime_overlay_shell,
        ):
            for path in sorted(package.rglob("*.py")):
                rel = path.relative_to(sandbox_dir).as_posix()
                tar.add(path, arcname=f"sandbox/{rel}")
    _BUNDLE_CACHE = buffer.getvalue()
    return _BUNDLE_CACHE


__all__ = ["snapshot_overlay_runtime_bundle_bytes"]
