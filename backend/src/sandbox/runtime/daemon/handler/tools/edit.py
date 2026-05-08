"""``api.edit_file`` dispatch entry."""

from __future__ import annotations

import time
from collections.abc import Mapping, Sequence
from pathlib import Path
from uuid import uuid4

from sandbox.layer_stack.workspace.binding import require_workspace_binding
from sandbox.occ.changeset.builders import build_api_write_change
from sandbox.occ.routing.runtime_ops import content_hash_bytes
from sandbox.occ.routing.single_path import prepare_single_path_changeset
from sandbox.runtime.async_bridge import run_sync_in_executor
from sandbox.runtime.daemon.handler.request_context import (
    _layer_stack_root,
    _project_changeset,
    _required_single_path,
    _services,
    classify_path,
)


async def edit_file(args: dict[str, object]) -> dict[str, object]:
    """Single-path edit_file dispatch with in/out-of-workspace classification."""
    total_start = time.perf_counter()
    layer_stack_root = _layer_stack_root(args)
    binding = require_workspace_binding(layer_stack_root)
    raw_path = _required_single_path(args)
    classified = classify_path(raw_path, binding.workspace_root)

    edits_raw = args.get("edits")
    if not isinstance(edits_raw, Sequence) or isinstance(edits_raw, (str, bytes)):
        raise ValueError("edits must be a list of search/replace objects")
    edits: list[tuple[str, str, int]] = []
    for edit in edits_raw:
        if not isinstance(edit, Mapping):
            raise ValueError("each edit must be an object")
        old_text = str(edit.get("old_text") or "")
        new_text = str(edit.get("new_text") or "")
        expected = int(edit.get("expected_occurrences") or 1)
        edits.append((old_text, new_text, expected))

    if classified.classification == "out_of_workspace":
        return _edit_out_of_workspace(
            abs_path=classified.abs_path,
            edits=edits,
            total_start=total_start,
        )

    return await _edit_in_workspace(
        layer_stack_root=layer_stack_root,
        layer_path=classified.layer_path,
        edits=edits,
        total_start=total_start,
    )


async def _edit_in_workspace(
    *,
    layer_stack_root: str,
    layer_path: str,
    edits: Sequence[tuple[str, str, int]],
    total_start: float,
) -> dict[str, object]:
    services = _services(layer_stack_root)
    request_id = uuid4().hex
    lease_start = time.perf_counter()
    lease = await run_sync_in_executor(
        services.manager.acquire_snapshot_lease, request_id
    )
    lease_acquired_s = time.perf_counter() - lease_start
    try:
        read_start = time.perf_counter()
        bytes_, exists = await run_sync_in_executor(
            services.layer_stack.read_bytes, layer_path, lease.manifest
        )
        read_elapsed = time.perf_counter() - read_start
        if not exists or bytes_ is None:
            raise FileNotFoundError(f"file not found in workspace: {layer_path}")
        try:
            text = bytes_.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise ValueError(
                f"file is not valid UTF-8 text: {layer_path}"
            ) from exc

        derive_start = time.perf_counter()
        try:
            final_text = _apply_edits(text, edits, path=layer_path)
        except ValueError as exc:
            derive_elapsed = time.perf_counter() - derive_start
            return _edit_conflict_payload(
                path=layer_path,
                message=str(exc),
                total_start=total_start,
                timings_extra={
                    "api.edit.lease_acquire_s": lease_acquired_s,
                    "api.edit.snapshot_read_s": read_elapsed,
                    "api.edit.derive_bytes_s": derive_elapsed,
                },
            )
        derive_elapsed = time.perf_counter() - derive_start

        change = build_api_write_change(
            path=layer_path,
            final_content=final_text.encode("utf-8"),
            base_hash=content_hash_bytes(bytes_),
        )
        prepared = await run_sync_in_executor(
            prepare_single_path_changeset,
            change,
            snapshot=lease.manifest,
            gitignore=services.gitignore,
            atomic=False,
        )
        apply_start = time.perf_counter()
        result = await services.occ_client.commit_prepared_changeset(
            prepared,
            workspace_ref=layer_stack_root,
        )
        apply_elapsed = time.perf_counter() - apply_start
    finally:
        await run_sync_in_executor(services.manager.release_lease, lease.lease_id)

    payload = _project_changeset(
        result,
        fallback_path=layer_path,
        verb="edit",
        total_start=total_start,
        gitignore=services.gitignore,
        timings_extra={
            "api.edit.lease_acquire_s": lease_acquired_s,
            "api.edit.snapshot_read_s": read_elapsed,
            "api.edit.derive_bytes_s": derive_elapsed,
            "api.edit.occ_apply_s": apply_elapsed,
        },
    )
    payload["applied_edits"] = len(edits) if payload["success"] else 0
    return payload


def _edit_out_of_workspace(
    *,
    abs_path: str,
    edits: Sequence[tuple[str, str, int]],
    total_start: float,
) -> dict[str, object]:
    target = Path(abs_path)
    if not target.exists():
        raise FileNotFoundError(f"file not found: {abs_path}")
    read_start = time.perf_counter()
    raw = target.read_bytes()
    read_elapsed = time.perf_counter() - read_start
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise ValueError(f"file is not valid UTF-8 text: {abs_path}") from exc
    derive_start = time.perf_counter()
    try:
        final_text = _apply_edits(text, edits, path=abs_path)
    except ValueError as exc:
        derive_elapsed = time.perf_counter() - derive_start
        return _edit_conflict_payload(
            path=abs_path,
            message=str(exc),
            total_start=total_start,
            timings_extra={
                "api.edit.host_fs_read_s": read_elapsed,
                "api.edit.derive_bytes_s": derive_elapsed,
            },
        )
    derive_elapsed = time.perf_counter() - derive_start
    write_start = time.perf_counter()
    target.write_text(final_text, encoding="utf-8")
    write_elapsed = time.perf_counter() - write_start
    return {
        "success": True,
        "changed_paths": [abs_path],
        "applied_edits": len(edits),
        "status": "ok",
        "conflict": None,
        "conflict_reason": None,
        "timings": {
            "api.edit.host_fs_read_s": read_elapsed,
            "api.edit.derive_bytes_s": derive_elapsed,
            "api.edit.host_fs_write_s": write_elapsed,
            "api.edit.total_s": time.perf_counter() - total_start,
        },
    }


def _edit_conflict_payload(
    *,
    path: str,
    message: str,
    total_start: float,
    timings_extra: dict[str, float],
) -> dict[str, object]:
    return {
        "success": False,
        "changed_paths": [path],
        "applied_edits": 0,
        "status": "aborted_overlap",
        "conflict": {
            "reason": "aborted_overlap",
            "conflict_file": path,
            "message": message,
        },
        "conflict_reason": message,
        "timings": {
            **timings_extra,
            "api.edit.total_s": time.perf_counter() - total_start,
        },
    }


def _apply_edits(
    text: str,
    edits: Sequence[tuple[str, str, int]],
    *,
    path: str,
) -> str:
    """Apply search/replace edits with anchor occurrence validation."""
    current = text
    for old_text, new_text, expected in edits:
        if not old_text:
            raise ValueError(f"edit anchor old_text must be non-empty for {path}")
        found = current.count(old_text)
        if found != expected:
            raise ValueError(
                f"anchor not found in {path}: expected {expected} "
                f"occurrences of {old_text!r}, found {found}"
            )
        current = current.replace(old_text, new_text, expected)
    return current


__all__ = ["edit_file"]
