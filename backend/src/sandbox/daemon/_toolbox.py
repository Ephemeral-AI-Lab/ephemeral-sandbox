"""Shared scaffolding for the per-verb handler modules.

Owns the single source of truth for:

* the in-workspace classifier predicate (``classify_path``),
* the host-side single-path / ``layer_stack_root`` validation contract,
* the result projection helpers used by ``write`` and
  ``edit`` to turn a :class:`ChangesetResult` into the
  host-visible payload.

The OCC backend tuple ``(LayerStackClient, OccClient, SnapshotGitignoreOracle,
LayerStack)`` is owned by :mod:`sandbox.daemon.occ_backend`.
The ``services`` helper is the canonical per-verb access point.

``shell`` does NOT use this module — the dispatcher routes it directly to
``service.shell_runner``, whose worker scaffolding still owns its own service
entrypoint and timing helpers.
"""

from __future__ import annotations

import errno
import os
from collections.abc import Mapping
from pathlib import Path
from typing import Literal, NamedTuple

from sandbox.occ.changeset import ChangesetResult
from sandbox.occ.gitignore import SnapshotGitignoreOracle
from sandbox.daemon._wire import (
    committed_paths,
    conflict_and_status,
    conflict_to_dict,
    gitignore_cache_timings,
)
from sandbox.daemon import occ_backend
from sandbox.daemon.occ_backend import OccBackend
from sandbox._shared.clock import monotonic_now

# -- classifier predicate ---------------------------------------------------


class ClassifiedPath(NamedTuple):
    classification: Literal["in_workspace", "out_of_workspace"]
    abs_path: str
    """Absolute filesystem path post-symlink-resolution."""
    layer_path: str
    """Workspace-relative layer path. Empty string for out-of-workspace."""


def classify_path(raw_path: str, workspace_root: str) -> ClassifiedPath:
    """Classify ``raw_path`` as in-workspace or out-of-workspace.

    Single source of truth for the §1 classifier predicate. Symlinks resolve
    before classification; ``..`` segments that escape a workspace-anchored
    input are a hard ``ValueError`` (not a silent direct-FS fallthrough).
    """
    raw = str(raw_path or "").strip()
    if not raw:
        raise ValueError("path is required")

    workspace_literal = workspace_root.rstrip("/") or "/"
    workspace_real = os.path.realpath(workspace_literal)

    if not raw.startswith("/"):
        candidate = os.path.join(workspace_real, raw)
        anchored_to_workspace = True
    else:
        candidate = raw
        anchored_to_workspace = (
            raw in (workspace_literal, workspace_real)
            or raw.startswith(workspace_literal + "/")
            or raw.startswith(workspace_real + "/")
        )

    normalized = os.path.normpath(candidate)

    if anchored_to_workspace:
        inside_literal = (
            normalized == workspace_literal
            or normalized.startswith(workspace_literal + "/")
        )
        inside_real = (
            normalized == workspace_real
            or normalized.startswith(workspace_real + "/")
        )
        if not (inside_literal or inside_real):
            raise ValueError(f"path escapes workspace via '..': {raw}")

    resolved = os.path.realpath(normalized)

    if resolved == workspace_real or resolved.startswith(workspace_real + "/"):
        rel = os.path.relpath(resolved, workspace_real)
        if rel == ".":
            rel = ""
        return ClassifiedPath("in_workspace", resolved, rel)

    return ClassifiedPath("out_of_workspace", resolved, "")


# -- argument validation ----------------------------------------------------


def require_arg(args: Mapping[str, object], key: str) -> str:
    """Return a stripped non-empty string ``args[key]`` or raise."""
    value = str(args.get(key) or "").strip()
    if not value:
        raise ValueError(f"{key} is required")
    return value


def layer_stack_root(args: Mapping[str, object]) -> str:
    return require_arg(args, "layer_stack_root")


def required_single_path(args: Mapping[str, object]) -> str:
    """Enforce single-path contract: ``args['path']`` must be one string."""
    raw = args.get("path")
    if not isinstance(raw, str):
        raise ValueError(
            "single-path contract: api.write_file/edit_file/read_file accept "
            "exactly one string path per request"
        )
    path = raw.strip()
    if not path:
        raise ValueError("path is required")
    return path


# -- service cache (delegates to occ_backend) -------------------------------


def services(layer_stack_root: str) -> OccBackend:
    return occ_backend.build_occ_backend(layer_stack_root)


# -- no-follow host filesystem helpers --------------------------------------


def read_bytes_no_follow(abs_path: str) -> bytes:
    fd = _open_no_follow(abs_path, os.O_RDONLY)
    with os.fdopen(fd, "rb") as file:
        return file.read()


def write_text_no_follow(
    abs_path: str,
    content: str,
    *,
    create_only: bool = False,
) -> None:
    target = Path(abs_path)
    target.parent.mkdir(parents=True, exist_ok=True)
    flags = os.O_WRONLY | os.O_CREAT | _o_no_follow()
    flags |= os.O_EXCL if create_only else os.O_TRUNC
    fd = _open_no_follow(abs_path, flags, mode=0o666)
    with os.fdopen(fd, "wb") as file:
        file.write(content.encode("utf-8"))


def _open_no_follow(abs_path: str, flags: int, mode: int = 0o666) -> int:
    path = Path(abs_path)
    if not path.is_absolute():
        raise ValueError(f"path must be absolute: {abs_path!r}")
    parts = path.parts
    if len(parts) < 2:
        raise ValueError(f"path must name a file: {abs_path!r}")

    dir_fd = os.open(parts[0], os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in parts[1:-1]:
            next_fd = os.open(
                part,
                os.O_RDONLY | os.O_DIRECTORY | _o_no_follow(),
                dir_fd=dir_fd,
            )
            os.close(dir_fd)
            dir_fd = next_fd
        return os.open(parts[-1], flags | _o_no_follow(), mode, dir_fd=dir_fd)
    except OSError as exc:
        if exc.errno == errno.ELOOP:
            raise ValueError(f"refusing to follow symlink: {abs_path}") from exc
        raise
    finally:
        os.close(dir_fd)


def _o_no_follow() -> int:
    value = getattr(os, "O_NOFOLLOW", None)
    if value is None:
        raise RuntimeError("O_NOFOLLOW is unavailable on this platform")
    return int(value)


# -- result projection ------------------------------------------------------


def project_changeset(
    result: ChangesetResult,
    *,
    fallback_path: str,
    verb: str,
    total_start: float,
    gitignore: SnapshotGitignoreOracle,
    timings_extra: dict[str, float],
) -> dict[str, object]:
    conflict, status = conflict_and_status(result.files)
    return {
        "success": result.success,
        "changed_paths": list(committed_paths(result.files, fallback_path=fallback_path)),
        "status": status,
        "conflict": conflict_to_dict(conflict),
        "conflict_reason": conflict.message if conflict is not None else None,
        "timings": {
            **result.timings,
            **gitignore_cache_timings(gitignore),
            **timings_extra,
            f"api.{verb}.total_s": monotonic_now() - total_start,
        },
    }


def project_conflict(
    *,
    verb: str,
    status: str,
    reason: str,
    path: str,
    message: str,
    total_start: float,
    timings_extra: dict[str, float] | None = None,
    changed_paths: list[str] | None = None,
    conflict_reason: str | None = None,
    **extras: object,
) -> dict[str, object]:
    """Project a single-path conflict into the guarded-result shape.

    ``status`` is the outer wire status (e.g. ``rejected``); ``reason`` is
    the inner ``conflict.reason`` (e.g. ``create_only_existing``). They
    coincide for the edit anchor-miss case. ``conflict_reason`` defaults
    to ``reason`` but the in-workspace edit path passes the raw exception
    text instead. ``extras`` carries verb-specific fields like
    ``applied_edits``.
    """
    payload: dict[str, object] = {
        "success": False,
        "changed_paths": list(changed_paths or []),
        "status": status,
        "conflict": {
            "reason": reason,
            "conflict_file": path,
            "message": message,
        },
        "conflict_reason": conflict_reason if conflict_reason is not None else reason,
        "timings": {
            **(timings_extra or {}),
            f"api.{verb}.total_s": monotonic_now() - total_start,
        },
    }
    payload.update(extras)
    return payload


__all__ = [
    "ClassifiedPath",
    "classify_path",
    "layer_stack_root",
    "project_changeset",
    "project_conflict",
    "read_bytes_no_follow",
    "required_single_path",
    "require_arg",
    "services",
    "write_text_no_follow",
]
