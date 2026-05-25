"""Daemon transport contracts and default public sandbox API transport."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any, Protocol

from sandbox.host.daemon_client import (
    call_daemon_api,
    versioned_payload as _versioned_payload,
)

DAEMON_OP_READ_FILE = "api.v1.read_file"
DAEMON_OP_WRITE_FILE = "api.v1.write_file"
DAEMON_OP_EDIT_FILE = "api.v1.edit_file"
DAEMON_OP_SHELL = "api.v1.shell"
DAEMON_OP_INVOCATION_CANCEL = "api.v1.cancel"
DAEMON_OP_INVOCATION_HEARTBEAT = "api.v1.heartbeat"
DAEMON_OP_INFLIGHT_COUNT = "api.v1.inflight_count"
DAEMON_OP_GLOB = "api.v1.glob"
DAEMON_OP_GREP = "api.v1.grep"


class SandboxTransport(Protocol):
    """Transport used by public workspace operations to call the sandbox daemon."""

    async def call(
        self,
        sandbox_id: str,
        op: str,
        payload: Mapping[str, object],
        *,
        timeout: int,
    ) -> dict[str, Any]:
        """Call one sandbox RPC.

        Implementations put a wire-level ``invocation_id`` on the daemon envelope.
        If ``payload`` already has ``invocation_id``, the same id is used for
        correlation between engine background tasks and daemon in-flight state.
        """
        ...


class DaemonSandboxTransport:
    """SandboxTransport implementation backed by the resident daemon."""

    async def call(
        self,
        sandbox_id: str,
        op: str,
        payload: Mapping[str, object],
        *,
        timeout: int,
    ) -> dict[str, Any]:
        return await call_daemon_api(
            sandbox_id,
            op,
            _versioned_payload(payload),
            timeout=timeout,
        )


async def call_sandbox_daemon(
    sandbox_id: str,
    op: str,
    payload: Mapping[str, object],
    *,
    timeout: int,
    transport: SandboxTransport | None = None,
) -> dict[str, Any]:
    """Call the provided transport or the resident daemon transport."""
    return await (transport or DaemonSandboxTransport()).call(
        sandbox_id,
        op,
        payload,
        timeout=timeout,
    )


__all__ = [
    "DAEMON_OP_EDIT_FILE",
    "DAEMON_OP_GLOB",
    "DAEMON_OP_GREP",
    "DAEMON_OP_INFLIGHT_COUNT",
    "DAEMON_OP_INVOCATION_CANCEL",
    "DAEMON_OP_INVOCATION_HEARTBEAT",
    "DAEMON_OP_READ_FILE",
    "DAEMON_OP_SHELL",
    "DAEMON_OP_WRITE_FILE",
    "DaemonSandboxTransport",
    "SandboxTransport",
    "call_sandbox_daemon",
]
