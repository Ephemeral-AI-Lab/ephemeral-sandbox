"""Tests for sandbox API transport contracts."""

from __future__ import annotations

from sandbox.api.transport import (
    DAEMON_OP_COMMAND_CANCEL,
    DAEMON_OP_COMMAND_COLLECT_COMPLETED,
    DAEMON_OP_COMMAND_SESSION_COUNT,
    DAEMON_OP_COMMAND_WRITE_STDIN,
    DAEMON_OP_EDIT_FILE,
    DAEMON_OP_EXEC_COMMAND,
    DAEMON_OP_INFLIGHT_COUNT,
    DAEMON_OP_INVOCATION_CANCEL,
    DAEMON_OP_INVOCATION_HEARTBEAT,
    DAEMON_OP_READ_FILE,
    DAEMON_OP_WRITE_FILE,
    SandboxTransport,
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


def test_public_daemon_ops_use_api_v1_names() -> None:
    assert DAEMON_OP_READ_FILE == "api.v1.read_file"
    assert DAEMON_OP_WRITE_FILE == "api.v1.write_file"
    assert DAEMON_OP_EDIT_FILE == "api.v1.edit_file"
    assert DAEMON_OP_EXEC_COMMAND == "api.v1.exec_command"
    assert DAEMON_OP_COMMAND_WRITE_STDIN == "api.v1.write_stdin"
    assert DAEMON_OP_COMMAND_CANCEL == "api.v1.command.cancel"
    assert DAEMON_OP_COMMAND_COLLECT_COMPLETED == "api.v1.command.collect_completed"
    assert DAEMON_OP_COMMAND_SESSION_COUNT == "api.v1.command_session_count"
    assert DAEMON_OP_INVOCATION_CANCEL == "api.v1.cancel"
    assert DAEMON_OP_INVOCATION_HEARTBEAT == "api.v1.heartbeat"
    assert DAEMON_OP_INFLIGHT_COUNT == "api.v1.inflight_count"
