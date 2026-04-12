"""Shared path utilities for scope overlap, path normalization, and ltree conversion."""

from __future__ import annotations

import re
from typing import Any

from code_intelligence.routing.scope_packets import (
    normalize_scope_paths,
    scope_paths_overlap,
)

# ---------------------------------------------------------------------------
# ltree conversion for PostgreSQL hierarchical queries
# ---------------------------------------------------------------------------

_LTREE_UNSAFE = re.compile(r"[^a-zA-Z0-9_]")


def _escape_ltree_char(ch: str) -> str:
    """Reversible character escaping for ltree labels.

    Uses a consistent X{hex} scheme for all unsafe characters.
    This avoids ambiguity — unescaped labels never contain 'X' followed
    by two hex digits because 'X' itself is escaped when present in input.
    """
    return f"X{ord(ch):02x}"


# Characters that are valid in ltree labels but need escaping when they
# could collide with our X{hex} escape sequences.
_LTREE_ESCAPE_PREFIX = re.compile(r"X([0-9a-fA-F]{2})")


def path_to_ltree(path: str) -> str:
    """Convert a file path to a PostgreSQL ltree label path.

    Examples:
        "src/auth/"           -> "src.auth"
        "src/auth/session.py" -> "src.auth.sessionX2epy"
        "src/my-module/foo.py"-> "src.myX2dmodule.fooX2epy"

    Raises ValueError if the path produces an empty ltree.
    """
    parts = path.strip("/").split("/")
    labels = []
    for part in parts:
        # First escape any existing X{hex} patterns to prevent ambiguity
        escaped = _LTREE_ESCAPE_PREFIX.sub(lambda m: f"X58{m.group(1)}", part)
        # Then escape all non-label-safe characters
        label = _LTREE_UNSAFE.sub(lambda m: _escape_ltree_char(m.group()), escaped)
        if label:
            labels.append(label)
    if not labels:
        raise ValueError(f"path {path!r} produced an empty ltree label")
    return ".".join(labels)


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
# Scope-aware path utilities (migrated from tools.daytona_toolkit.coordination)
# ---------------------------------------------------------------------------

_PY_PATH_RE = re.compile(r"(?<![A-Za-z0-9_./-])([A-Za-z0-9_./-]+\.py)(?![A-Za-z0-9_./-])")


def scopes_overlap(path_a: str, path_b: str) -> bool:
    """Return True when two file or directory scopes overlap."""
    return scope_paths_overlap(path_a, path_b)


def scope_paths_from_payload(payload: Any) -> list[str]:
    """Extract the most likely scope paths from a work-item payload."""
    if not isinstance(payload, dict):
        return []
    collected: list[str] = []
    for key in ("touches_paths", "target_paths", "stale_subsystems", "paths", "files", "owned_files"):
        raw = payload.get(key)
        if isinstance(raw, list):
            collected.extend(str(item) for item in raw if isinstance(item, str))
    raw_verify = payload.get("verify")
    if isinstance(raw_verify, list):
        for item in raw_verify:
            if isinstance(item, str):
                collected.extend(path.split("::", 1)[0].strip() for path in _PY_PATH_RE.findall(item))
    elif isinstance(raw_verify, str):
        collected.extend(path.split("::", 1)[0].strip() for path in _PY_PATH_RE.findall(raw_verify))
    for key in ("file_path", "path", "subsystem", "canonical_scope"):
        raw = payload.get(key)
        if isinstance(raw, str) and raw.strip():
            collected.append(raw)
    return normalize_scope_paths(collected)


def scope_paths_for_work_item(team_run: Any, wi: Any) -> list[str]:
    """Resolve a work item's owned scope from payload."""
    return scope_paths_from_payload(getattr(wi, "payload", None))
