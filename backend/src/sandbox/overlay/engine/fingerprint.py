"""Workspace fingerprint guard helpers."""

from __future__ import annotations

from pathlib import Path

from sandbox.overlay.engine.constants import WorkspaceFingerprint


def workspace_fingerprint(workspace_root: str) -> WorkspaceFingerprint:
    root = Path(workspace_root)
    rows: list[tuple[str, int, int, int, int]] = []
    for path in (root,):
        try:
            st = path.stat()
        except OSError:
            rows.append((str(path), -1, -1, -1, -1))
            continue
        rows.append((str(path), st.st_dev, st.st_ino, st.st_mtime_ns, st.st_size))
    return tuple(rows)


__all__ = ["workspace_fingerprint"]
