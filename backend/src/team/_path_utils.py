"""Shared path utilities for scope overlap and path normalization."""

from __future__ import annotations

import re
from typing import Any


# ---------------------------------------------------------------------------
# Scope path utilities
# ---------------------------------------------------------------------------


def normalize_scope_paths(paths: list[str] | tuple[str, ...] | None) -> list[str]:
    out: list[str] = []
    seen: set[str] = set()
    for raw in paths or ():
        if not isinstance(raw, str):
            continue
        for part in raw.split("|"):
            cleaned = part.strip().replace("\\", "/").removeprefix("./").rstrip("/")
            if not cleaned or cleaned in seen:
                continue
            seen.add(cleaned)
            out.append(cleaned)
    out.sort()
    return out


def scope_paths_overlap(path_a: str, path_b: str) -> bool:
    left = (path_a or "").strip().rstrip("/")
    right = (path_b or "").strip().rstrip("/")
    if not left or not right:
        return False
    if left == right:
        return True
    if left.startswith(right + "/") or right.startswith(left + "/"):
        return True
    return (
        left.endswith("/" + right)
        or right.endswith("/" + left)
        or ("/" + right + "/") in (left + "/")
        or ("/" + left + "/") in (right + "/")
    )


# ---------------------------------------------------------------------------
# Path normalization and overlap
# ---------------------------------------------------------------------------


def normalize_path_list(raw: Any) -> list[str]:
    """Normalize a list of paths to a cleaned string list."""
    out: list[str] = []
    for item in raw if isinstance(raw, list) else [raw] if isinstance(raw, str) else []:
        if isinstance(item, str):
            cleaned = item.strip()
            if cleaned:
                out.append(cleaned)
    return out


def paths_overlap(path_a: str | None, path_b: str | None) -> bool:
    """Check if two paths overlap (one is a prefix of the other)."""
    left = _normalise_path(path_a) if path_a else ""
    right = _normalise_path(path_b) if path_b else ""
    if not left or not right:
        return False
    if left == right:
        return True
    return left.startswith(right + "/") or right.startswith(left + "/")


def _normalise_path(path: str | None) -> str:
    """Normalize a path: strip, remove ./, trailing slashes, backslash to forward."""
    return str(path or "").strip().replace("\\", "/").removeprefix("./").rstrip("/")


# ---------------------------------------------------------------------------
# Scope-aware path utilities
# ---------------------------------------------------------------------------


def scopes_overlap(path_a: str, path_b: str) -> bool:
    """Return True when two file or directory scopes overlap."""
    return scope_paths_overlap(path_a, path_b)


_PY_PATH_RE = re.compile(r"(?<![A-Za-z0-9_./-])([A-Za-z0-9_./-]+\.py)(?![A-Za-z0-9_./-])")


def scope_paths_from_payload(payload: Any) -> list[str]:
    """Extract the most likely scope paths from a work-item payload."""
    if not isinstance(payload, dict):
        return []
    collected: list[str] = []
    for key in (
        "touches_paths",
        "target_paths",
        "stale_subsystems",
        "paths",
        "files",
        "owned_files",
    ):
        raw = payload.get(key)
        if isinstance(raw, list):
            collected.extend(str(item) for item in raw if isinstance(item, str))
    raw_verify = payload.get("verify")
    if isinstance(raw_verify, list):
        for item in raw_verify:
            if isinstance(item, str):
                collected.extend(
                    path.split("::", 1)[0].strip() for path in _PY_PATH_RE.findall(item)
                )
    elif isinstance(raw_verify, str):
        collected.extend(path.split("::", 1)[0].strip() for path in _PY_PATH_RE.findall(raw_verify))
    for key in ("file_path", "path", "subsystem"):
        raw = payload.get(key)
        if isinstance(raw, str) and raw.strip():
            collected.append(raw)
    return normalize_scope_paths(collected)



