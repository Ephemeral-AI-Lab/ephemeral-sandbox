"""Phase 2 unified workspace dispatch invariants."""

from __future__ import annotations

import shutil
from pathlib import Path
from typing import Any

import pytest

from sandbox.daemon import builtin_operations, occ_runtime_services
from sandbox.daemon.workspace_tool import payloads as workspace_tool_payloads
from sandbox.daemon.rpc import dispatcher as server
from sandbox.daemon.layer_stack_runtime import get_layer_stack_manager
from sandbox.layer_stack import WriteLayerChange
from sandbox.layer_stack.workspace_base import build_workspace_base


def _tool_handler(verb: str) -> Any:
    return builtin_operations.WORKSPACE_TOOL_HANDLERS[verb]


def test_operation_payload_classifier_helpers_removed() -> None:
    assert not hasattr(workspace_tool_payloads, "ClassifiedPath")
    assert not hasattr(workspace_tool_payloads, "classify_path")


def test_op_table_dispatches_data_ops_to_unified_handlers() -> None:
    server._register_builtin_operations()
    assert server.OP_TABLE["api.write_file"] is _tool_handler("write_file")
    assert server.OP_TABLE["api.v1.write_file"] is _tool_handler("write_file")
    assert server.OP_TABLE["api.edit_file"] is _tool_handler("edit_file")
    assert server.OP_TABLE["api.v1.edit_file"] is _tool_handler("edit_file")
    assert server.OP_TABLE["api.read_file"] is _tool_handler("read_file")
    assert server.OP_TABLE["api.v1.read_file"] is _tool_handler("read_file")
    assert server.OP_TABLE["api.v1.exec_command"] is builtin_operations.exec_command
    assert "api.v1.shell" not in server.OP_TABLE
    assert server.OP_TABLE["api.v1.write_stdin"] is builtin_operations.command_write_stdin
    assert server.OP_TABLE["api.v1.command.write_stdin"] is builtin_operations.command_write_stdin
    assert server.OP_TABLE["api.v1.command.cancel"] is builtin_operations.command_cancel
    assert server.OP_TABLE["api.layer_metrics"] is builtin_operations.layer_metrics


@pytest.mark.asyncio
@pytest.mark.parametrize("bad_path", [["a", "b"], ("a", "b"), {"path": "a"}, 123, b"a"])
async def test_write_file_rejects_non_string_path_argument(bad_path: object) -> None:
    with pytest.raises(ValueError, match="single-path contract"):
        await _tool_handler("write_file")(
            {
                "layer_stack_root": "/tmp/unused-layer-stack",
                "path": bad_path,
                "content": "x",
            }
        )


@pytest.mark.asyncio
async def test_edit_file_rejects_list_path_argument() -> None:
    with pytest.raises(ValueError, match="single-path contract"):
        await _tool_handler("edit_file")(
            {
                "layer_stack_root": "/tmp/unused-layer-stack",
                "path": ["a", "b"],
                "edits": [{"old_text": "x", "new_text": "y"}],
            }
        )


@pytest.mark.asyncio
async def test_read_file_rejects_list_path_argument() -> None:
    with pytest.raises(ValueError, match="single-path contract"):
        await _tool_handler("read_file")(
            {
                "layer_stack_root": "/tmp/unused-layer-stack",
                "path": ["a", "b"],
            }
        )


@pytest.mark.asyncio
async def test_layer_stack_services_share_lease_registry(tmp_path: Path) -> None:
    """Layer-stack services still share one manager/LeaseRegistry instance."""
    occ_runtime_services.clear_occ_runtime_services()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "a.txt").write_text("base\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    write_services = occ_runtime_services.get_occ_runtime_services(stack.as_posix())
    manager_via_singleton = get_layer_stack_manager(stack.as_posix())

    assert write_services.layer_stack_manager is manager_via_singleton
    assert write_services.layer_stack.manager is manager_via_singleton

    starting_active = manager_via_singleton.active_lease_count()
    lease = manager_via_singleton.acquire_lease_record("test")
    try:
        assert manager_via_singleton.active_lease_count() == starting_active + 1
        assert (
            write_services.layer_stack_manager.active_lease_count()
            == manager_via_singleton.active_lease_count()
        )
    finally:
        manager_via_singleton.release_lease(lease.lease_id)


@pytest.mark.asyncio
async def test_layer_metrics_reports_no_cache_storage_fields(tmp_path: Path) -> None:
    occ_runtime_services.clear_occ_runtime_services()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "seed.txt").write_text("seed\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    payload = await builtin_operations.layer_metrics({"layer_stack_root": stack.as_posix()})

    assert {
        "manifest_version",
        "manifest_depth",
        "active_leases",
        "leased_layers",
        "layer_dirs",
        "staging_dirs",
        "storage_bytes",
        "workspace_bound",
        "workspace_root",
        "base_root_hash",
    } <= payload.keys()
    forbidden = {
        "cache_hit",
        "cache_policy",
        "lowerdir_cache_hits",
        "lowerdir_cache_misses",
        "lowerdir_cache_entries",
        "tree_copy_lowerdirs",
    }
    assert payload.keys().isdisjoint(forbidden)


@pytest.mark.asyncio
async def test_layer_metrics_reports_active_lease_pins(tmp_path: Path) -> None:
    occ_runtime_services.clear_occ_runtime_services()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "seed.txt").write_text("seed\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)
    manager = get_layer_stack_manager(stack.as_posix())
    lease = manager.acquire_lease_record("metrics-reader")
    try:
        payload = await builtin_operations.layer_metrics({"layer_stack_root": stack.as_posix()})
    finally:
        manager.release_lease(lease.lease_id)

    assert payload["active_leases"] == 1
    assert payload["leased_layers"] == len(set(lease.manifest.layers))


@pytest.mark.asyncio
async def test_layer_metrics_reports_orphan_and_missing_layers(tmp_path: Path) -> None:
    occ_runtime_services.clear_occ_runtime_services()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "seed.txt").write_text("seed\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)
    manager = get_layer_stack_manager(stack.as_posix())

    source = tmp_path / "source.txt"
    source.write_text("changed\n", encoding="utf-8")
    manager.publish_changes([WriteLayerChange(path="tracked.txt", source_path=source.as_posix())])
    manifest = manager.read_active_manifest()
    active_layer_id = manifest.layers[0].layer_id

    shutil.rmtree(manager.storage_root / "layers" / active_layer_id)
    (manager.storage_root / "layers" / "orphan-layer").mkdir()

    payload = await builtin_operations.layer_metrics({"layer_stack_root": stack.as_posix()})

    assert payload["referenced_layers"] == len(manifest.layers)
    assert payload["layer_dirs"] == len(manifest.layers)
    assert payload["missing_layer_count"] == 1
    assert payload["missing_layer_ids"] == [active_layer_id]
    assert payload["orphan_layer_count"] == 1
    assert payload["orphan_layer_ids"] == ["orphan-layer"]
