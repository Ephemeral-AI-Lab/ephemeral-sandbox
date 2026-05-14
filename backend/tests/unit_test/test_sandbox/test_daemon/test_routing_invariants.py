"""Daemon OP_TABLE routing invariants."""

from __future__ import annotations

from sandbox.runtime.daemon.handler import overlay as overlay_run
from sandbox.runtime.daemon.handler import (
    health,
    metrics,
    workspace,
)
from sandbox.plugin import handler as plugin_handler
from sandbox.runtime.daemon.handler.tools import edit, read, write
from sandbox.runtime.daemon.rpc import dispatcher as server
from sandbox.runtime.daemon.service import shell_runner


def test_daemon_op_table_routes_to_current_handler_layout() -> None:
    server._load_peer_bootstraps()

    expected = {
        "api.write_file": write.write_file,
        "api.v1.write_file": write.write_file,
        "api.edit_file": edit.edit_file,
        "api.v1.edit_file": edit.edit_file,
        "api.read_file": read.read_file,
        "api.v1.read_file": read.read_file,
        "api.shell": shell_runner.execute_shell_api,
        "api.v1.shell": shell_runner.execute_shell_api,
        "api.layer_metrics": metrics.layer_metrics,
        "api.ensure_workspace_base": workspace.ensure_workspace_base,
        "api.build_workspace_base": workspace.build_workspace_base,
        "api.prepare_workspace_snapshot": (
            workspace.prepare_workspace_snapshot
        ),
        "api.release_workspace_snapshot": (
            workspace.release_workspace_snapshot
        ),
        "api.workspace_binding": workspace.workspace_binding,
        "overlay.run": overlay_run.handle,
        "api.runtime.ready": health.runtime_ready,
        "api.layer_stack.fence_stale_staging": (
            workspace.fence_stale_staging
        ),
        "api.plugin.ensure": plugin_handler.plugin_ensure,
        "api.plugin.status": plugin_handler.plugin_status,
    }
    # Plugin-specific ops (plugin.<name>.<op>) appear when api.plugin.ensure
    # flushes pending registrations; only the static OP_TABLE entries are
    # asserted here.
    static_ops = {
        op: handler
        for op, handler in server.OP_TABLE.items()
        if not op.startswith("plugin.")
    }
    assert static_ops == expected


def test_daemon_op_table_does_not_route_through_occ_server() -> None:
    server._load_peer_bootstraps()

    for handler in server.OP_TABLE.values():
        assert handler.__module__ != "sandbox.runtime.daemon.service.occ_backend"
