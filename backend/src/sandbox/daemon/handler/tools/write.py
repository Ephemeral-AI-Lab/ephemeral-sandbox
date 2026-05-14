"""``api.write_file`` dispatch entry."""

from __future__ import annotations

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
    required_single_path,
    services as backend_services,
    write_text_no_follow,
)
from sandbox.timing import monotonic_now

_CONTENT_HASHER = ContentHasher()


async def write_file(args: dict[str, object]) -> dict[str, object]:
    """Single-path write_file dispatch with in/out-of-workspace classification."""
    total_start = monotonic_now()
    layer_stack_root = require_layer_stack_root(args)
    binding = require_workspace_binding(layer_stack_root)
    raw_path = required_single_path(args)
    classified = classify_path(raw_path, binding.workspace_root)

    content = str(args.get("content") or "")
    overwrite = bool(args.get("overwrite", True))

    if classified.classification == "out_of_workspace":
        return _write_out_of_workspace(
            classified.abs_path,
            content,
            overwrite=overwrite,
            total_start=total_start,
        )

    return await _write_in_workspace(
        layer_stack_root=layer_stack_root,
        layer_path=classified.layer_path,
        content=content,
        overwrite=overwrite,
        total_start=total_start,
    )


async def _write_in_workspace(
    *,
    layer_stack_root: str,
    layer_path: str,
    content: str,
    overwrite: bool,
    total_start: float,
) -> dict[str, object]:
    services = backend_services(layer_stack_root)
    request_id = uuid4().hex
    lease_start = monotonic_now()
    lease = await run_sync_in_executor(services.manager.acquire_snapshot_lease, request_id)
    lease_acquired_s = monotonic_now() - lease_start
    snapshot_read_s = 0.0
    known_base_hash: str | None = None
    known_base_hash_ready = False
    try:
        if not overwrite:
            # create-only: reject if the path already exists in the leased
            # validation snapshot. Host-side existence check against snapshot
            # N is the source of truth for this API rule.
            read_start = monotonic_now()
            bytes_, exists_in_n = await run_sync_in_executor(
                services.layer_stack.read_bytes, layer_path, lease.manifest
            )
            snapshot_read_s += monotonic_now() - read_start
            known_base_hash = (
                _CONTENT_HASHER.hash_bytes(bytes_) if exists_in_n and bytes_ is not None else None
            )
            known_base_hash_ready = True
            if exists_in_n:
                return {
                    "success": False,
                    "changed_paths": [],
                    "status": "rejected",
                    "conflict": {
                        "reason": "create_only_existing",
                        "conflict_file": layer_path,
                        "message": (
                            "create-only write rejected: path exists in "
                            f"validation snapshot at {layer_path}"
                        ),
                    },
                    "conflict_reason": "create_only_existing",
                    "timings": {
                        "api.write.lease_acquire_s": lease_acquired_s,
                        "api.write.snapshot_read_s": snapshot_read_s,
                        "api.write.total_s": monotonic_now() - total_start,
                    },
                }

        change = build_api_write_change(
            path=layer_path,
            final_content=content,
        )

        def read_base_hash(path: str) -> str | None:
            nonlocal snapshot_read_s
            if path != layer_path:
                raise ValueError(f"unexpected single-path base hash read: {path}")
            if known_base_hash_ready:
                return known_base_hash
            read_start = monotonic_now()
            bytes_, exists = services.layer_stack.read_bytes(path, lease.manifest)
            snapshot_read_s += monotonic_now() - read_start
            return _CONTENT_HASHER.hash_bytes(bytes_) if exists and bytes_ is not None else None

        prepared = await run_sync_in_executor(
            prepare_single_path_changeset,
            change,
            snapshot=lease.manifest,
            gitignore=services.gitignore,
            base_hash_reader=read_base_hash,
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

    return project_changeset(
        result,
        fallback_path=layer_path,
        verb="write",
        total_start=total_start,
        gitignore=services.gitignore,
        timings_extra={
            "api.write.lease_acquire_s": lease_acquired_s,
            "api.write.snapshot_read_s": snapshot_read_s,
            "api.write.occ_apply_s": apply_elapsed,
        },
    )


def _write_out_of_workspace(
    abs_path: str,
    content: str,
    *,
    overwrite: bool,
    total_start: float,
) -> dict[str, object]:
    if not overwrite:
        try:
            write_start = monotonic_now()
            write_text_no_follow(abs_path, content, create_only=True)
        except FileExistsError:
            return {
                "success": False,
                "changed_paths": [],
                "status": "rejected",
                "conflict": {
                    "reason": "create_only_existing",
                    "conflict_file": abs_path,
                    "message": (f"create-only write rejected: path exists at {abs_path}"),
                },
                "conflict_reason": "create_only_existing",
                "timings": {
                    "api.write.total_s": monotonic_now() - total_start,
                },
            }
        write_elapsed = monotonic_now() - write_start
        return {
            "success": True,
            "changed_paths": [abs_path],
            "status": "ok",
            "conflict": None,
            "conflict_reason": None,
            "timings": {
                "api.write.host_fs_write_s": write_elapsed,
                "api.write.total_s": monotonic_now() - total_start,
            },
        }
    write_start = monotonic_now()
    write_text_no_follow(abs_path, content)
    write_elapsed = monotonic_now() - write_start
    return {
        "success": True,
        "changed_paths": [abs_path],
        "status": "ok",
        "conflict": None,
        "conflict_reason": None,
        "timings": {
            "api.write.host_fs_write_s": write_elapsed,
            "api.write.total_s": monotonic_now() - total_start,
        },
    }


__all__ = ["write_file"]
