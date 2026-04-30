"""Shared path helpers for workspace-scoped code intelligence operations."""

from __future__ import annotations

from pathlib import Path


def relativize_workspace_path(path: str, workspace_root: str = "") -> str:
    """Return *path* normalized to a workspace-relative POSIX form.

    Strips the workspace root prefix if present, normalizes separators, and
    drops any leading ``./`` or ``/`` so the result is suitable as an indexed
    workspace key.
    """
    normalized = str(path or "").replace("\\", "/").strip()
    root = str(workspace_root or "").replace("\\", "/").rstrip("/")
    if root and normalized == root:
        return ""
    if root and normalized.startswith(root + "/"):
        normalized = normalized[len(root) + 1 :]
    while normalized.startswith("./"):
        normalized = normalized[2:]
    while normalized.startswith("/"):
        normalized = normalized[1:]
    return normalized.rstrip("/")


def resolve_workspace_path(file_path: str, workspace_root: str = "") -> str:
    """Resolve *file_path* against *workspace_root* without relative escape.

    Absolute paths are preserved because Daytona tools commonly pass canonical
    sandbox paths. Relative paths are normalized under ``workspace_root`` and
    rejected if they traverse outside that root.
    """
    path = Path(str(file_path))
    if path.is_absolute() or not workspace_root:
        return str(path)

    root = Path(workspace_root).resolve(strict=False)
    resolved = (root / path).resolve(strict=False)
    try:
        resolved.relative_to(root)
    except ValueError as exc:
        raise ValueError(f"path escapes workspace root: {file_path}") from exc
    return str(resolved)
