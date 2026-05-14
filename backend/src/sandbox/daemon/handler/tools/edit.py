"""``api.edit_file`` dispatch entry."""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from uuid import uuid4

from sandbox.layer_stack.workspace_binding import require_workspace_binding
from sandbox.occ.changeset.types import build_api_write_change
from sandbox.occ.content.hashing import ContentHasher
from sandbox.occ.router import prepare_single_path_changeset
from sandbox.daemon.async_bridge import run_sync_in_executor
from sandbox.daemon.handler.request_context import (
    classify_path,
    layer_stack_root as require_layer_stack_root,
    project_changeset,
    read_bytes_no_follow,
    required_single_path,
    services as backend_services,
    write_text_no_follow,
)
from sandbox.timing import monotonic_now

_CONTENT_HASHER = ContentHasher()


async def edit_file(args: dict[str, object]) -> dict[str, object]:
    """Single-path edit_file dispatch with in/out-of-workspace classification."""
    total_start = monotonic_now()
    # Validate edits payload before binding/classification so structural
    # errors (negative expected_occurrences, malformed entries) surface as
    # the most-specific error rather than being masked by the
    # layer_stack_root requirement check.
    edits_raw = args.get("edits")
    if not isinstance(edits_raw, Sequence) or isinstance(edits_raw, (str, bytes)):
        raise ValueError("edits must be a list of search/replace objects")
    edits: list[tuple[str, str, int]] = []
    for edit in edits_raw:
        if not isinstance(edit, Mapping):
            raise ValueError("each edit must be an object")
        old_text = str(edit.get("old_text") or "")
        new_text = str(edit.get("new_text") or "")
        raw_expected = edit.get("expected_occurrences")
        expected = 1 if raw_expected is None else int(raw_expected)
        if expected < 0:
            raise ValueError("expected_occurrences must be >= 0")
        edits.append((old_text, new_text, expected))

    layer_stack_root = require_layer_stack_root(args)
    binding = require_workspace_binding(layer_stack_root)
    raw_path = required_single_path(args)
    classified = classify_path(raw_path, binding.workspace_root)

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
    services = backend_services(layer_stack_root)
    request_id = uuid4().hex
    lease_start = monotonic_now()
    lease = await run_sync_in_executor(services.manager.acquire_snapshot_lease, request_id)
    lease_acquired_s = monotonic_now() - lease_start
    try:
        read_start = monotonic_now()
        bytes_, exists = await run_sync_in_executor(
            services.layer_stack.read_bytes, layer_path, lease.manifest
        )
        read_elapsed = monotonic_now() - read_start
        if not exists or bytes_ is None:
            raise FileNotFoundError(f"file not found in workspace: {layer_path}")
        try:
            text = bytes_.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise ValueError(f"file is not valid UTF-8 text: {layer_path}") from exc

        derive_start = monotonic_now()
        # Anchor-miss / count-mismatch / non-utf8 must surface as a hard
        # ValueError rather than a silent "conflict" payload — silent
        # acceptance was the BL-01 contract violation in DirectStager and the
        # runtime handler must not undo that loudness at the API boundary.
        final_text = _apply_edits(text, edits, path=layer_path)
        derive_elapsed = monotonic_now() - derive_start

        change = build_api_write_change(
            path=layer_path,
            final_content=final_text.encode("utf-8"),
            base_hash=_CONTENT_HASHER.hash_bytes(bytes_),
        )
        prepared = await run_sync_in_executor(
            prepare_single_path_changeset,
            change,
            snapshot=lease.manifest,
            gitignore=services.gitignore,
            atomic=False,
        )
        apply_start = monotonic_now()
        result = await services.occ_client.commit_prepared(
            prepared,
            workspace_ref=layer_stack_root,
        )
        apply_elapsed = monotonic_now() - apply_start
    finally:
        await run_sync_in_executor(services.manager.release_lease, lease.lease_id)

    payload = project_changeset(
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
    read_start = monotonic_now()
    raw = read_bytes_no_follow(abs_path)
    read_elapsed = monotonic_now() - read_start
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise ValueError(f"file is not valid UTF-8 text: {abs_path}") from exc
    derive_start = monotonic_now()
    # Out-of-workspace edits return a structured conflict payload on
    # anchor-miss (preserves existing API contract). The in-workspace path
    # raises loud per the Theme 4 OCC realignment.
    try:
        final_text = _apply_edits(text, edits, path=abs_path)
    except ValueError as exc:
        derive_elapsed = monotonic_now() - derive_start
        return _edit_conflict_payload(
            path=abs_path,
            message=str(exc),
            total_start=total_start,
            timings_extra={
                "api.edit.host_fs_read_s": read_elapsed,
                "api.edit.derive_bytes_s": derive_elapsed,
            },
        )
    derive_elapsed = monotonic_now() - derive_start
    write_start = monotonic_now()
    write_text_no_follow(abs_path, final_text)
    write_elapsed = monotonic_now() - write_start
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
            "api.edit.total_s": monotonic_now() - total_start,
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
            "api.edit.total_s": monotonic_now() - total_start,
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
