"""Edit/OCC invariants after Phase 2 workspace unification."""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox._shared.tool_primitives.edit import edit_file
from sandbox.layer_stack import LayerStack, WriteLayerChange
from sandbox.layer_stack.workspace_base import build_workspace_base
from sandbox.occ.changeset import CommitOptions, FileStatus, build_api_write_change
from sandbox.occ.content_hashing import ContentHasher
from sandbox.daemon import occ_runtime_services


def test_edit_primitive_derives_final_bytes_before_overlay_capture(tmp_path: Path) -> None:
    target = tmp_path / "config.toml"
    target.write_text('name = "old"\n', encoding="utf-8")

    result = edit_file(
        {
            "path": target.as_posix(),
            "edits": [{"old_text": "old", "new_text": "new"}],
        }
    )

    assert result.success is True
    assert result.applied_edits == 1
    assert target.read_text(encoding="utf-8") == 'name = "new"\n'


def test_edit_primitive_anchor_miss_preserves_file(tmp_path: Path) -> None:
    target = tmp_path / "a.txt"
    target.write_text("foo\n", encoding="utf-8")

    with pytest.raises(ValueError, match="anchor not found"):
        edit_file(
            {
                "path": target.as_posix(),
                "edits": [{"old_text": "missing", "new_text": "anything"}],
            }
        )

    assert target.read_text(encoding="utf-8") == "foo\n"


@pytest.mark.asyncio
async def test_in_workspace_edit_same_path_M_gt_N_surfaces_hard_conflict(
    tmp_path: Path,
) -> None:
    """Same-path M>N race still aborts at OCC commit time."""
    occ_runtime_services.clear_occ_runtime_services()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "shared.txt").write_text("hello world\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    services = occ_runtime_services.get_occ_runtime_services(stack.as_posix())
    manager: LayerStack = services.layer_stack_manager
    occ_service = services.occ_client._service  # type: ignore[attr-defined]

    lease = manager.acquire_lease_record("test-edit-N")
    try:
        bytes_n, exists_n = services.layer_stack.read_bytes("shared.txt", lease.manifest)
        assert exists_n and bytes_n is not None
        derived_final = bytes_n.replace(b"hello", b"hi")

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
        active_m = manager.read_active_manifest()
        assert active_m.version > lease.manifest.version

        result = await occ_service.commit_prepared(prepared)
    finally:
        manager.release_lease(lease.lease_id)

    assert result.success is False
    assert result.files[0].status is FileStatus.ABORTED_VERSION
    assert manager.read_text("shared.txt") == ("DIFFERENT\n", True)
