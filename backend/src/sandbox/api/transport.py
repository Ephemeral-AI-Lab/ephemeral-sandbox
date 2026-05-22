"""Default transport for sandbox daemon API calls."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

from sandbox.host.daemon_client import (
    call_daemon_api,
    versioned_payload as _versioned_payload,
)

DAEMON_OP_READ_FILE = "api.v1.read_file"
DAEMON_OP_WRITE_FILE = "api.v1.write_file"
DAEMON_OP_EDIT_FILE = "api.v1.edit_file"
DAEMON_OP_SHELL = "api.v1.shell"
DAEMON_OP_SHELL_LAUNCH = "api.v1.shell.launch"
DAEMON_OP_SHELL_POLL = "api.v1.shell.poll"
DAEMON_OP_SHELL_CANCEL = "api.v1.shell.cancel"
DAEMON_OP_SHELL_REAP = "api.v1.shell.reap"
DAEMON_OP_SHELL_METRICS = "api.v1.shell.metrics"
DAEMON_OP_FIND_FILES = "api.v1.find_files"
DAEMON_OP_SEARCH_CONTENT = "api.v1.search_content"


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


__all__ = [
    "DAEMON_OP_EDIT_FILE",
    "DAEMON_OP_FIND_FILES",
    "DAEMON_OP_READ_FILE",
    "DAEMON_OP_SEARCH_CONTENT",
    "DAEMON_OP_SHELL",
    "DAEMON_OP_SHELL_CANCEL",
    "DAEMON_OP_SHELL_LAUNCH",
    "DAEMON_OP_SHELL_METRICS",
    "DAEMON_OP_SHELL_POLL",
    "DAEMON_OP_SHELL_REAP",
    "DAEMON_OP_WRITE_FILE",
    "DaemonSandboxTransport",
]
