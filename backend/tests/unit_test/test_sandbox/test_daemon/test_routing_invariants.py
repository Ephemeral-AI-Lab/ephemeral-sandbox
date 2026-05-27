"""Daemon OP_TABLE routing invariants."""

from __future__ import annotations

from sandbox.daemon import builtin_operations
from sandbox.daemon.rpc import dispatcher as server
from sandbox.ephemeral_workspace.plugin import runtime_api as plugin_runtime_api


def test_daemon_op_table_routes_to_current_handler_layout() -> None:
    server._register_builtin_operations()

    expected = {
        **builtin_operations.WORKSPACE_TOOL_OPS,
        "api.v1.cancel": builtin_operations.cancel,
        "api.v1.heartbeat": builtin_operations.heartbeat,
        "api.v1.inflight_count": builtin_operations.inflight_count,
        "api.layer_metrics": builtin_operations.layer_metrics,
        "api.ensure_workspace_base": builtin_operations.ensure_workspace_base,
        "api.build_workspace_base": builtin_operations.build_workspace_base,
        "api.acquire_snapshot": builtin_operations.acquire_snapshot,
        "api.commit_to_workspace": builtin_operations.commit_to_workspace,
        "api.release_lease": builtin_operations.release_lease,
        "api.workspace_binding": builtin_operations.workspace_binding,
        "api.runtime.ready": builtin_operations.runtime_ready,
        "api.layer_stack.fence_stale_staging": builtin_operations.fence_stale_staging,
        "api.plugin.ensure": plugin_runtime_api.plugin_ensure,
        "api.plugin.status": plugin_runtime_api.plugin_status,
        "api.isolated_workspace.enter": server._isolated_workspace_enter,
        "api.isolated_workspace.exit": server._isolated_workspace_exit,
        "api.isolated_workspace.status": server._isolated_workspace_status,
        "api.isolated_workspace.list_open": server._isolated_workspace_list_open,
        "api.isolated_workspace.test_reset": server._isolated_workspace_test_reset,
        "api.audit.pull": server._audit_pull_handler,
        "api.audit.snapshot": server._audit_snapshot_handler,
        "api.audit.reset_floor": server._audit_reset_floor_handler,
    }
    # Plugin-specific ops (plugin.<name>.<op>) appear when api.plugin.ensure
    # flushes pending registrations; only the static OP_TABLE entries are
    # asserted here.
    static_ops = {
        op: handler for op, handler in server.OP_TABLE.items() if not op.startswith("plugin.")
    }
    assert static_ops == expected


def test_daemon_op_table_does_not_route_through_occ_server() -> None:
    server._register_builtin_operations()

    for handler in server.OP_TABLE.values():
        assert handler.__module__ != "sandbox.daemon.occ_runtime_services"
