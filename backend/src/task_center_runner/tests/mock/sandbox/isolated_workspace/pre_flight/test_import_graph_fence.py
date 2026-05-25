"""Phase 2 isolated workspace routing surface checks."""

from __future__ import annotations

from pathlib import Path

from sandbox.daemon.rpc import dispatcher


def test_legacy_isolated_workspace_tool_surfaces_were_deleted() -> None:
    src_root = Path(__file__).resolve().parents[7] / "src"
    assert not (src_root / "sandbox" / "isolated_workspace" / "ops_handlers.py").exists()
    assert not (src_root / "sandbox" / "isolated_workspace" / "scripts" / "in_ns_write.py").exists()


def test_iws_tool_ops_route_through_api_v1_only() -> None:
    dispatcher._register_builtin_operations()
    prefix = "api.isolated_workspace."
    obsolete = {
        prefix + "shell",
        prefix + "read_file",
        prefix + "write_file",
        prefix + "edit_file",
        prefix + "grep",
    }
    assert obsolete.isdisjoint(dispatcher.OP_TABLE)
