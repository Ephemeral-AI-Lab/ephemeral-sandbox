"""Phase 05 — in-workspace edit byte derivation tests.

In-workspace edit reads bytes via the SnapshotReader port (LayerStackClient
.read_bytes), validates anchors against snapshot N, derives final bytes,
and submits a single WriteChange to OCC. OCC sees only final bytes —
not search/replace anchors — so it cannot silently re-derive against
a moved active manifest M.
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import patch

import pytest

from sandbox.layer_stack import LayerStackManager
from sandbox.layer_stack.workspace.base import build_workspace_base
from sandbox.occ.changeset.types import WriteChange
from sandbox.runtime.daemon.service import occ_backend
from sandbox.runtime.daemon.handler.request_context import services as request_services
from sandbox.runtime.daemon.handler.tools import edit, write


@pytest.mark.asyncio
async def test_in_workspace_edit_reads_bytes_via_snapshot_reader(
    tmp_path: Path,
) -> None:
    occ_backend.clear_backend_cache()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "a.txt").write_text("hello world\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    services = request_services(stack.as_posix())
    seen_manifests: list[object] = []
    real_read_bytes = services.layer_stack.read_bytes

    def tracking_read_bytes(
        path: str,
        manifest: object | None = None,
    ) -> tuple[bytes | None, bool]:
        seen_manifests.append(manifest)
        return real_read_bytes(path, manifest)

    services.layer_stack.read_bytes = tracking_read_bytes  # type: ignore[method-assign]
    try:
        result = await edit.edit_file(
            {
                "layer_stack_root": stack.as_posix(),
                "path": "a.txt",
                "edits": [{"old_text": "hello", "new_text": "hi"}],
            }
        )
    finally:
        services.layer_stack.read_bytes = real_read_bytes  # type: ignore[method-assign]

    assert result["success"] is True
    assert result["changed_paths"] == ["a.txt"]
    assert result["applied_edits"] == 1
    # The handler reached SnapshotReader.read_bytes with a leased manifest at
    # least once (OCC may call back through the same port for base-hash
    # inference + revalidation; the first call is the byte-derivation read).
    assert seen_manifests, "edit handler must read bytes via SnapshotReader port"
    assert seen_manifests[0] is not None


@pytest.mark.asyncio
async def test_in_workspace_edit_submits_write_change_with_derived_bytes(
    tmp_path: Path,
) -> None:
    """OCC sees a WriteChange (not EditChange) carrying final derived bytes."""
    occ_backend.clear_backend_cache()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "config.toml").write_text("name = \"old\"\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    services = request_services(stack.as_posix())

    submitted_changes: list[object] = []
    real_commit = services.occ_client.commit_prepared

    async def tracking_commit(prepared, **kwargs):
        for group in prepared.path_groups:
            submitted_changes.extend(group.changes)
        return await real_commit(prepared, **kwargs)

    with patch.object(
        services.occ_client,
        "commit_prepared",
        tracking_commit,
    ):
        result = await edit.edit_file(
            {
                "layer_stack_root": stack.as_posix(),
                "path": "config.toml",
                "edits": [{"old_text": "old", "new_text": "new"}],
            }
        )

    assert result["success"] is True
    assert len(submitted_changes) == 1
    submitted = submitted_changes[0]
    # Critical assertion: OCC sees a WriteChange with the FINAL derived bytes.
    # If we leaked an EditChange, OCC could silently re-derive against a
    # moved manifest M, which the plan explicitly forbids.
    assert isinstance(submitted, WriteChange)
    assert submitted.final_content == b"name = \"new\"\n"


@pytest.mark.asyncio
async def test_in_workspace_edit_anchor_miss_raises(tmp_path: Path) -> None:
    """Anchor validation runs against snapshot N, not a moved manifest."""
    occ_backend.clear_backend_cache()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "a.txt").write_text("foo\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    with pytest.raises(ValueError, match="anchor not found"):
        await edit.edit_file(
            {
                "layer_stack_root": stack.as_posix(),
                "path": "a.txt",
                "edits": [{"old_text": "missing", "new_text": "anything"}],
            }
        )


@pytest.mark.asyncio
async def test_in_workspace_edit_rejects_non_utf8(tmp_path: Path) -> None:
    occ_backend.clear_backend_cache()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "blob.bin").write_bytes(b"\xff\xfe\x00\x00bad")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    with pytest.raises(ValueError, match="not valid UTF-8 text"):
        await edit.edit_file(
            {
                "layer_stack_root": stack.as_posix(),
                "path": "blob.bin",
                "edits": [{"old_text": "bad", "new_text": "good"}],
            }
        )


@pytest.mark.asyncio
async def test_in_workspace_edit_same_path_M_gt_N_surfaces_hard_conflict(
    tmp_path: Path,
) -> None:
    """Same-path M>N race: prepared from snapshot N, intervening publish on
    the SAME path moves manifest to M, commit must surface ABORTED_VERSION
    — OCC must NOT silently re-derive bytes against M.
    """
    from sandbox.layer_stack import WriteLayerChange
    from sandbox.occ.changeset.builders import build_api_write_change
    from sandbox.occ.changeset.prepared import CommitOptions
    from sandbox.occ.changeset.types import FileStatus
    from sandbox.occ.content.hashing import ContentHasher

    occ_backend.clear_backend_cache()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "shared.txt").write_text("hello world\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    services = request_services(stack.as_posix())
    manager: LayerStackManager = services.manager
    occ_service = services.occ_client._service  # type: ignore[attr-defined]

    # Lease snapshot N=1 (the leased identity command-exec would carry).
    lease = manager.acquire_snapshot_lease("test-edit-N")
    try:
        bytes_n, exists_n = services.layer_stack.read_bytes(
            "shared.txt", lease.manifest
        )
        assert exists_n and bytes_n is not None
        derived_final = bytes_n.replace(b"hello", b"hi")

        # Prepare against snapshot N — OCC will infer base_hash from N.
        prepared = await occ_service.prepare_changeset(
            [
                build_api_write_change(
                    path="shared.txt",
                    final_content=derived_final,
                )
            ],
            snapshot=lease.manifest,
            options=CommitOptions(),
        )

        # Concurrent publish on the SAME path bumps the manifest to M = N+1.
        intervening_source = tmp_path / "intervening.txt"
        intervening_source.write_text("DIFFERENT\n", encoding="utf-8")
        manager.publish_changes(
            [
                WriteLayerChange(
                    path="shared.txt",
                    content_hash=ContentHasher().hash_bytes(b"DIFFERENT\n"),
                    source_path=str(intervening_source),
                )
            ]
        )
        active_M = manager.read_active_manifest()
        assert active_M.version > lease.manifest.version

        # Now commit the prepared changeset. OCC must NOT silently re-derive;
        # base_hash from N no longer matches M's bytes → ABORTED_VERSION.
        result = await occ_service.commit_prepared(prepared)
    finally:
        manager.release_lease(lease.lease_id)

    assert result.success is False
    assert result.files[0].status is FileStatus.ABORTED_VERSION
    # The intervening commit's bytes are still active; OCC did not overwrite.
    assert manager.read_text("shared.txt") == ("DIFFERENT\n", True)


@pytest.mark.asyncio
async def test_in_workspace_create_only_rejects_existing_path(
    tmp_path: Path,
) -> None:
    """create-only in-workspace write rejects when the path exists in the
    validation snapshot — base_hash mismatch on existing-path content."""
    occ_backend.clear_backend_cache()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "exists.txt").write_text("base\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    result = await write.write_file(
        {
            "layer_stack_root": stack.as_posix(),
            "path": "exists.txt",
            "content": "new\n",
            "overwrite": False,  # create-only
        }
    )

    # The existing path in snapshot N triggers a per-path conflict; the
    # tracked base_hash does NOT match a "create-only" empty assertion, so
    # OCC declines to publish (rides the same content-CAS gate).
    assert result["success"] is False
    assert result["status"] != "ok"
    assert result["conflict"] is not None
