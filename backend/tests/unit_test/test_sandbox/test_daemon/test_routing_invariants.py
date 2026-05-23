"""Daemon OP_TABLE routing invariants."""

from __future__ import annotations

from sandbox.daemon.handler import (
    health,
    metrics,
    overlay,
    workspace,
)
from sandbox.plugin import handler as plugin_handler
from sandbox.daemon.handler import edit, glob, grep, read, write
from sandbox.daemon.rpc import dispatcher as server
from sandbox.daemon.service import shell_runner, shell_job_handler
from sandbox.isolated_workspace import handlers as iws_handlers
from sandbox.isolated_workspace import ops_handlers as iws_ops_handlers


def test_daemon_op_table_routes_to_current_handler_layout() -> None:
    server._load_peer_bootstraps()

    expected = {
        "api.write_file": write.write_file,
        "api.v1.write_file": write.write_file,
        "api.edit_file": edit.edit_file,
        "api.v1.edit_file": edit.edit_file,
        "api.read_file": read.read_file,
        "api.v1.read_file": read.read_file,
        "api.glob": glob.glob,
        "api.v1.glob": glob.glob,
        "api.grep": grep.grep,
        "api.v1.grep": grep.grep,
        "api.shell": shell_runner.execute_shell_api,
        "api.v1.shell": shell_runner.execute_shell_api,
        "api.shell.launch": shell_job_handler.shell_launch,
        "api.v1.shell.launch": shell_job_handler.shell_launch,
        "api.shell.poll": shell_job_handler.shell_poll,
        "api.v1.shell.poll": shell_job_handler.shell_poll,
        "api.shell.cancel": shell_job_handler.shell_cancel,
        "api.v1.shell.cancel": shell_job_handler.shell_cancel,
        "api.shell.reap": shell_job_handler.shell_reap,
        "api.v1.shell.reap": shell_job_handler.shell_reap,
        "api.shell.metrics": shell_job_handler.shell_metrics,
        "api.v1.shell.metrics": shell_job_handler.shell_metrics,
        "api.overlay.flush": overlay.flush_workspace_overlay,
        "api.overlay.stop": overlay.stop_workspace_overlay,
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
        "overlay.run": overlay.run_snapshot_overlay,
        "api.runtime.ready": health.runtime_ready,
        "api.layer_stack.fence_stale_staging": (
            workspace.fence_stale_staging
        ),
        "api.plugin.ensure": plugin_handler.plugin_ensure,
        "api.plugin.status": plugin_handler.plugin_status,
        "api.isolated_workspace.enter": iws_handlers.enter,
        "api.isolated_workspace.exit": iws_handlers.exit_,
        "api.isolated_workspace.status": iws_handlers.status,
        "api.isolated_workspace.list_open": iws_handlers.list_open,
        "api.isolated_workspace.test_reset": iws_handlers.test_reset,
        "api.isolated_workspace.shell": iws_ops_handlers.shell,
        "api.isolated_workspace.read_file": iws_ops_handlers.read_file,
        "api.isolated_workspace.write_file": iws_ops_handlers.write_file,
        "api.isolated_workspace.edit_file": iws_ops_handlers.edit_file,
        "api.isolated_workspace.grep": iws_ops_handlers.grep,
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
        assert handler.__module__ != "sandbox.daemon.occ_backend"
