"""Phase 05 — out-of-workspace passthrough tests.

Out-of-workspace write/edit/read bypass OCC entirely and land on the host
sandbox FS unchanged (matches shell namespace passthrough semantics).
"""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.layer_stack.workspace.base import build_workspace_base
from sandbox.runtime.daemon.service import occ_backend
from sandbox.runtime.daemon.handler.tools import edit, read, write
from sandbox.runtime.daemon.service.workspace_server import get_layer_stack_manager


@pytest.mark.asyncio
async def test_out_of_workspace_write_lands_on_host_fs(tmp_path: Path) -> None:
    """write_file('/tmp-like/foo') goes straight to host FS, no OCC."""
    occ_backend._backend_cache_clear()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)
    manager = get_layer_stack_manager(stack.as_posix())
    starting_version = manager.read_active_manifest().version
    starting_lease_count = manager.active_lease_count()

    target = tmp_path / "outside" / "foo.txt"
    result = await write.write_file(
        {
            "layer_stack_root": stack.as_posix(),
            "path": target.as_posix(),
            "content": "fresh\n",
        }
    )

    assert result["success"] is True
    assert result["status"] == "ok"
    assert result["changed_paths"] == [target.as_posix()]
    assert result["conflict"] is None
    # Manifest did NOT advance (no OCC publish).
    assert manager.read_active_manifest().version == starting_version
    # Lease registry untouched.
    assert manager.active_lease_count() == starting_lease_count
    # Host FS observably wrote the file.
    assert target.read_text(encoding="utf-8") == "fresh\n"


@pytest.mark.asyncio
async def test_out_of_workspace_edit_runs_against_host_bytes(tmp_path: Path) -> None:
    occ_backend._backend_cache_clear()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)
    manager = get_layer_stack_manager(stack.as_posix())
    starting_version = manager.read_active_manifest().version

    target = tmp_path / "outside" / "config.txt"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("key=before\n", encoding="utf-8")

    result = await edit.edit_file(
        {
            "layer_stack_root": stack.as_posix(),
            "path": target.as_posix(),
            "edits": [{"old_text": "before", "new_text": "after"}],
        }
    )

    assert result["success"] is True
    assert result["applied_edits"] == 1
    assert target.read_text(encoding="utf-8") == "key=after\n"
    # No OCC publish.
    assert manager.read_active_manifest().version == starting_version


@pytest.mark.asyncio
async def test_out_of_workspace_read_returns_host_bytes(tmp_path: Path) -> None:
    occ_backend._backend_cache_clear()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)
    manager = get_layer_stack_manager(stack.as_posix())
    starting_lease_count = manager.active_lease_count()

    target = tmp_path / "outside" / "hello.txt"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("hi\n", encoding="utf-8")

    result = await read.read_file(
        {
            "layer_stack_root": stack.as_posix(),
            "path": target.as_posix(),
        }
    )

    assert result["success"] is True
    assert result["exists"] is True
    assert result["content"] == "hi\n"
    # Lease registry not touched (read out-of-workspace skips the lease entirely).
    assert manager.active_lease_count() == starting_lease_count


@pytest.mark.asyncio
async def test_out_of_workspace_read_missing_path_does_not_raise(
    tmp_path: Path,
) -> None:
    occ_backend._backend_cache_clear()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    target = tmp_path / "outside" / "missing.txt"
    result = await read.read_file(
        {
            "layer_stack_root": stack.as_posix(),
            "path": target.as_posix(),
        }
    )

    assert result["success"] is True
    assert result["exists"] is False
    assert result["content"] == ""


@pytest.mark.asyncio
async def test_shell_namespace_passthrough_consistency(tmp_path: Path) -> None:
    """write_file('/tmp-like/foo','x'); read_file(same) reads back x.

    Confirms shell `echo > /tmp/foo` semantics: the same file written from
    write_file is observable via read_file as host-FS bytes.
    """
    occ_backend._backend_cache_clear()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    target = tmp_path / "outside" / "shared.txt"
    await write.write_file(
        {
            "layer_stack_root": stack.as_posix(),
            "path": target.as_posix(),
            "content": "first\n",
        }
    )
    read1 = await read.read_file(
        {"layer_stack_root": stack.as_posix(), "path": target.as_posix()}
    )
    assert read1["content"] == "first\n"

    await write.write_file(
        {
            "layer_stack_root": stack.as_posix(),
            "path": target.as_posix(),
            "content": "second\n",
        }
    )
    read2 = await read.read_file(
        {"layer_stack_root": stack.as_posix(), "path": target.as_posix()}
    )
    assert read2["content"] == "second\n"


@pytest.mark.asyncio
async def test_out_of_workspace_write_create_only_rejects_existing(
    tmp_path: Path,
) -> None:
    """create-only host-FS write rejects when the path already exists."""
    occ_backend._backend_cache_clear()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    target = tmp_path / "outside" / "already.txt"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("existing\n", encoding="utf-8")

    result = await write.write_file(
        {
            "layer_stack_root": stack.as_posix(),
            "path": target.as_posix(),
            "content": "new\n",
            "overwrite": False,
        }
    )

    assert result["success"] is False
    assert result["status"] == "rejected"
    assert result["conflict"] is not None
    assert target.read_text(encoding="utf-8") == "existing\n"
