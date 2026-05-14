"""Phase 05 — write/edit/read dispatch + classifier predicate tests.

Covers the §6 classifier-predicate bullets, the single-path contract,
the OP_TABLE wiring, and the shared-LeaseRegistry assertion.
"""

from __future__ import annotations

import os
from pathlib import Path
from uuid import uuid4

import pytest

from sandbox.layer_stack.workspace_base import build_workspace_base
from sandbox.daemon.handler import metrics
from sandbox.daemon.handler.request_context import (
    ClassifiedPath,
    classify_path,
    services as request_services,
)
from sandbox.daemon.handler.tools import edit, read, write
from sandbox.daemon.rpc import dispatcher as server
from sandbox.daemon.service import occ_backend
from sandbox.daemon.service import shell_runner
from sandbox.daemon.service.workspace_server import get_layer_stack_manager


# ---------------------------------------------------------------------------
# Classifier predicate
# ---------------------------------------------------------------------------


def test_classify_workspace_relative_path_in_workspace(tmp_path: Path) -> None:
    workspace = (tmp_path / "ws").resolve()
    workspace.mkdir()
    result = classify_path("foo", workspace.as_posix())
    assert result.classification == "in_workspace"
    assert Path(result.abs_path).resolve() == (workspace / "foo").resolve()
    assert result.layer_path == "foo"


def test_classify_absolute_workspace_path_in_workspace(tmp_path: Path) -> None:
    workspace = (tmp_path / "ws").resolve()
    workspace.mkdir()
    result = classify_path((workspace / "foo").as_posix(), workspace.as_posix())
    assert result.classification == "in_workspace"
    assert result.layer_path == "foo"


def test_classify_relative_and_absolute_resolve_to_same_layer_path(
    tmp_path: Path,
) -> None:
    workspace = (tmp_path / "ws").resolve()
    workspace.mkdir()
    relative = classify_path("nested/a.py", workspace.as_posix())
    absolute = classify_path(
        (workspace / "nested" / "a.py").as_posix(),
        workspace.as_posix(),
    )
    assert relative.layer_path == absolute.layer_path == "nested/a.py"


def test_classify_symlink_to_outside_workspace_classifies_out(
    tmp_path: Path,
) -> None:
    workspace = (tmp_path / "ws").resolve()
    workspace.mkdir()
    target = (tmp_path / "outside").resolve()
    target.mkdir()
    (target / "foo").write_text("x")
    (workspace / "link").symlink_to(target / "foo")
    result = classify_path((workspace / "link").as_posix(), workspace.as_posix())
    assert result.classification == "out_of_workspace"
    assert Path(result.abs_path).resolve() == (target / "foo").resolve()


def test_classify_symlink_inside_workspace_classifies_in(
    tmp_path: Path,
) -> None:
    workspace = (tmp_path / "ws").resolve()
    workspace.mkdir()
    inner = workspace / "inner"
    inner.mkdir()
    (inner / "foo").write_text("x")
    (workspace / "link").symlink_to(inner / "foo")
    result = classify_path((workspace / "link").as_posix(), workspace.as_posix())
    assert result.classification == "in_workspace"
    assert result.layer_path == "inner/foo"


def test_classify_dotdot_escape_is_hard_error(tmp_path: Path) -> None:
    workspace = (tmp_path / "ws").resolve()
    workspace.mkdir()
    with pytest.raises(ValueError, match="escapes workspace"):
        classify_path((workspace / ".." / "etc" / "passwd").as_posix(), workspace.as_posix())
    with pytest.raises(ValueError, match="escapes workspace"):
        classify_path("../etc/passwd", workspace.as_posix())


def test_classify_outside_absolute_path_classifies_out_of_workspace(
    tmp_path: Path,
) -> None:
    workspace = (tmp_path / "ws").resolve()
    workspace.mkdir()
    result = classify_path("/tmp/foo", workspace.as_posix())
    assert result.classification == "out_of_workspace"
    assert isinstance(result, ClassifiedPath)


# ---------------------------------------------------------------------------
# OP_TABLE wiring (write/edit/read/shell dispatch from runtime handler tools)
# ---------------------------------------------------------------------------


def test_op_table_dispatches_data_ops_to_runtime_handlers() -> None:
    server._load_peer_bootstraps()
    assert server.OP_TABLE["api.write_file"] is write.write_file
    assert server.OP_TABLE["api.v1.write_file"] is write.write_file
    assert server.OP_TABLE["api.edit_file"] is edit.edit_file
    assert server.OP_TABLE["api.v1.edit_file"] is edit.edit_file
    assert server.OP_TABLE["api.read_file"] is read.read_file
    assert server.OP_TABLE["api.v1.read_file"] is read.read_file
    assert server.OP_TABLE["api.shell"] is shell_runner.execute_shell_api
    assert server.OP_TABLE["api.v1.shell"] is shell_runner.execute_shell_api
    assert server.OP_TABLE["api.layer_metrics"] is metrics.layer_metrics


# ---------------------------------------------------------------------------
# Single-path contract
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_write_file_rejects_list_path_argument(tmp_path: Path) -> None:
    occ_backend.clear_backend_cache()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)
    with pytest.raises(ValueError, match="single-path contract"):
        await write.write_file(
            {
                "layer_stack_root": stack.as_posix(),
                "path": ["a", "b"],
                "content": "x",
            }
        )


@pytest.mark.asyncio
@pytest.mark.parametrize("bad_path", [("a", "b"), {"path": "a"}, 123, b"a"])
async def test_write_file_rejects_non_string_path_argument(
    tmp_path: Path,
    bad_path: object,
) -> None:
    occ_backend.clear_backend_cache()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)
    with pytest.raises(ValueError, match="single-path contract"):
        await write.write_file(
            {
                "layer_stack_root": stack.as_posix(),
                "path": bad_path,
                "content": "x",
            }
        )


@pytest.mark.asyncio
async def test_edit_file_rejects_list_path_argument(tmp_path: Path) -> None:
    occ_backend.clear_backend_cache()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)
    with pytest.raises(ValueError, match="single-path contract"):
        await edit.edit_file(
            {
                "layer_stack_root": stack.as_posix(),
                "path": ["a", "b"],
                "edits": [{"old_text": "x", "new_text": "y"}],
            }
        )


# ---------------------------------------------------------------------------
# Shared LeaseRegistry across shell + write/edit/read
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_write_edit_read_share_lease_registry_with_shell(
    tmp_path: Path,
) -> None:
    """All four flows acquire leases from the SAME registry instance — layer-stack
    GC sees a unified pin set."""
    occ_backend.clear_backend_cache()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "a.txt").write_text("base\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    write_services = request_services(stack.as_posix())
    manager_via_singleton = get_layer_stack_manager(stack.as_posix())

    # The write/edit/read services point at the same LayerStackManager singleton
    # as the shell path; the LeaseRegistry is internal to that manager, so all
    # four flows pin layers through one registry.
    assert write_services.manager is manager_via_singleton
    assert write_services.layer_stack.manager is manager_via_singleton

    # Active counts reflect a single registry: a fresh acquire bumps the count
    # observed by the OTHER consumer.
    starting_active = manager_via_singleton.active_lease_count()
    lease = manager_via_singleton.acquire_snapshot_lease("test")
    try:
        assert manager_via_singleton.active_lease_count() == starting_active + 1
        # Same view across the in-process service cache.
        assert (
            write_services.manager.active_lease_count()
            == manager_via_singleton.active_lease_count()
        )
    finally:
        manager_via_singleton.release_lease(lease.lease_id)


@pytest.mark.asyncio
async def test_in_workspace_write_pins_lease_then_releases(tmp_path: Path) -> None:
    """An in-workspace write_file holds a lease covering prepare → publish."""
    occ_backend.clear_backend_cache()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "seed.txt").write_text("seed\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)
    manager = get_layer_stack_manager(stack.as_posix())
    starting_count = manager.active_lease_count()

    result = await write.write_file(
        {
            "layer_stack_root": stack.as_posix(),
            "path": "new.txt",
            "content": "fresh\n",
            "actor_id": f"agent-{uuid4().hex[:6]}",
        }
    )

    assert result["success"] is True
    # Lease is released after publish — in-flight count returns to baseline.
    assert manager.active_lease_count() == starting_count


@pytest.mark.asyncio
async def test_layer_metrics_reports_no_cache_storage_fields(tmp_path: Path) -> None:
    occ_backend.clear_backend_cache()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "seed.txt").write_text("seed\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    payload = await metrics.layer_metrics(
        {"layer_stack_root": stack.as_posix()}
    )

    assert {
        "manifest_version",
        "manifest_depth",
        "active_leases",
        "pinned_layers",
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
        "materialized_lowerdirs",
    }
    assert payload.keys().isdisjoint(forbidden)


@pytest.mark.asyncio
async def test_layer_metrics_reports_active_lease_pins(tmp_path: Path) -> None:
    occ_backend.clear_backend_cache()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "seed.txt").write_text("seed\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)
    manager = get_layer_stack_manager(stack.as_posix())
    lease = manager.acquire_snapshot_lease("metrics-reader")
    try:
        payload = await metrics.layer_metrics(
            {"layer_stack_root": stack.as_posix()}
        )
    finally:
        manager.release_lease(lease.lease_id)

    assert payload["active_leases"] == 1
    assert payload["pinned_layers"] == len(set(lease.manifest.layers))


@pytest.mark.asyncio
async def test_write_file_single_path_prepare_reports_gitignore_timing(
    tmp_path: Path,
) -> None:
    occ_backend.clear_backend_cache()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / ".gitignore").write_text("dist/\n", encoding="utf-8")
    (workspace / "a.txt").write_text("base\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    result = await write.write_file(
        {
            "layer_stack_root": stack.as_posix(),
            "path": "a.txt",
            "content": "changed\n",
        }
    )

    assert result["success"] is True
    timings = result["timings"]
    assert timings["occ.prepare.single_path_fast_s"] >= 0.0
    assert timings["occ.prepare.gitignore_s"] >= 0.0


@pytest.mark.asyncio
async def test_edit_file_single_path_prepare_reuses_target_read_for_base_hash(
    tmp_path: Path,
) -> None:
    occ_backend.clear_backend_cache()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / ".gitignore").write_text("dist/\n", encoding="utf-8")
    (workspace / "a.txt").write_text("alpha=old\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    result = await edit.edit_file(
        {
            "layer_stack_root": stack.as_posix(),
            "path": "a.txt",
            "edits": [{"old_text": "alpha=old", "new_text": "alpha=new"}],
        }
    )

    assert result["success"] is True
    timings = result["timings"]
    assert timings["occ.prepare.single_path_fast_s"] >= 0.0
    assert timings["occ.prepare.single_path_base_hash_s"] == 0.0


# ---------------------------------------------------------------------------
# Sanity: real read_file in-workspace returns layer-stack bytes (not real FS)
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_read_file_in_workspace_returns_layer_stack_bytes(
    tmp_path: Path,
) -> None:
    occ_backend.clear_backend_cache()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "a.txt").write_text("base\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)
    # Mutate the real workspace file AFTER base build — read_file must NOT see this
    (workspace / "a.txt").write_text("mutated\n", encoding="utf-8")

    result = await read.read_file(
        {
            "layer_stack_root": stack.as_posix(),
            "path": "a.txt",
        }
    )

    assert result["success"] is True
    assert result["exists"] is True
    assert result["content"] == "base\n"


def test_classifier_resolves_workspace_real_path_when_input_uses_literal(
    tmp_path: Path,
) -> None:
    """When workspace_root is a symlink, literal-prefixed input still classifies in."""
    real_workspace = (tmp_path / "real-ws").resolve()
    real_workspace.mkdir()
    link_workspace = tmp_path / "ws"
    os.symlink(real_workspace, link_workspace)
    # Pass the LITERAL (symlink) workspace path; input uses literal-prefixed form.
    result = classify_path(f"{link_workspace}/foo", str(link_workspace))
    assert result.classification == "in_workspace"
    assert result.layer_path == "foo"
