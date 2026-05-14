"""Tests for sandbox API transport/version contracts."""

from __future__ import annotations

from sandbox.api.protocol import SandboxTransport
from sandbox.api.transport import (
    DAEMON_OP_EDIT_FILE,
    DAEMON_OP_READ_FILE,
    DAEMON_OP_SHELL,
    DAEMON_OP_WRITE_FILE,
    DAEMON_PROTOCOL_FIELD,
    DAEMON_PROTOCOL_VERSION,
    versioned_payload,
)


class RecordingTransport:
    async def call(
        self,
        sandbox_id: str,
        op: str,
        payload: dict[str, object],
        *,
        timeout: int,
    ) -> dict[str, object]:
        del sandbox_id, op, payload, timeout
        return {}


def test_recording_transport_matches_protocol_shape() -> None:
    transport: SandboxTransport = RecordingTransport()
    assert transport is not None


def test_versioned_payload_attaches_daemon_protocol_version() -> None:
    assert versioned_payload({"path": "a.py"}) == {
        DAEMON_PROTOCOL_FIELD: DAEMON_PROTOCOL_VERSION,
        "path": "a.py",
    }


def test_public_daemon_ops_are_versioned() -> None:
    assert DAEMON_OP_READ_FILE == "api.v1.read_file"
    assert DAEMON_OP_WRITE_FILE == "api.v1.write_file"
    assert DAEMON_OP_EDIT_FILE == "api.v1.edit_file"
    assert DAEMON_OP_SHELL == "api.v1.shell"
