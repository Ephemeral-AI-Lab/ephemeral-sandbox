"""Shared scaffolding for the per-verb handler modules.

Owns the single source of truth for:

* the in-workspace classifier predicate (``classify_path``),
* the host-side single-path / ``layer_stack_root`` validation contract,
* the result projection helpers used by ``write`` and
  ``edit`` to turn a :class:`ChangesetResult` into the
  host-visible payload.

The OCC backend tuple ``(LayerStackClient, OCCClient, SnapshotGitignoreOracle,
LayerStackManager)`` is owned by :mod:`sandbox.runtime.daemon.service.occ_backend`. The
``_services`` helper is the canonical per-verb access point.

``shell`` does NOT use this module — ``handler.tools.shell`` delegates to
``service.shell_runner``, whose worker scaffolding still owns its own service
entrypoint and timing helpers.
"""

from __future__ import annotations

import os
import time
from collections.abc import Mapping
from typing import Literal, NamedTuple

from sandbox.occ.result_projection import (
    committed_paths,
    conflict_and_status,
    conflict_to_dict,
    gitignore_cache_timings,
)
from sandbox.occ.changeset.types import ChangesetResult
from sandbox.occ.content.gitignore_oracle import SnapshotGitignoreOracle
from sandbox.runtime.daemon.service.occ_backend import OccBackend
from sandbox.runtime.daemon.service import occ_backend


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
            raw == workspace_literal
            or raw.startswith(workspace_literal + "/")
            or raw == workspace_real
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


def _layer_stack_root(args: Mapping[str, object]) -> str:
    layer_stack_root = str(args.get("layer_stack_root") or "").strip()
    if not layer_stack_root:
        raise ValueError("layer_stack_root is required")
    return layer_stack_root


def _required_single_path(args: Mapping[str, object]) -> str:
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


def _services(layer_stack_root: str) -> OccBackend:
    return occ_backend.build_occ_backend(layer_stack_root)


# -- result projection ------------------------------------------------------


def _project_changeset(
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
            f"api.{verb}.total_s": time.perf_counter() - total_start,
        },
    }


__all__ = [
    "ClassifiedPath",
    "_layer_stack_root",
    "_project_changeset",
    "_required_single_path",
    "_services",
    "classify_path",
]
