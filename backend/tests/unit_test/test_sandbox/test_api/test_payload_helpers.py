"""Tests for sandbox API payload helper contracts."""

from __future__ import annotations

import pytest

from sandbox.api.tool._daemon_results import (
    int_from_daemon_field,
    user_visible_error_message,
)
from sandbox.api.tool._conflict_detection import (
    is_edit_conflict,
    is_shell_conflict,
)
from sandbox._shared.models import SandboxCaller


def test_sandbox_caller_audit_fields_keeps_required_keys_and_non_empty_fields() -> None:
    caller = SandboxCaller(
        agent_id="agent-1",
        task_center_run_id="tc-run",
        tool_id="tool-1",
    )

    assert caller.audit_fields() == {
        "agent_id": "agent-1",
        "run_id": "",
        "agent_run_id": "",
        "task_id": "",
        "task_center_run_id": "tc-run",
        "tool_id": "tool-1",
    }


def test_user_visible_error_message_strips_internal_error_prefix() -> None:
    assert user_visible_error_message(RuntimeError("internal_error: anchor not found")) == (
        "anchor not found"
    )


def test_int_from_daemon_field_is_strict_about_boundary_types() -> None:
    assert int_from_daemon_field(3, default=0) == 3
    assert int_from_daemon_field(None, default=7) == 7
    with pytest.raises(TypeError):
        int_from_daemon_field(True, default=0)
    with pytest.raises(TypeError):
        int_from_daemon_field("1", default=0)
    with pytest.raises(TypeError):
        int_from_daemon_field(1.5, default=0)


def test_conflict_detection_prefers_typed_error_codes() -> None:
    class CodedError(RuntimeError):
        code = "anchor_not_found"

    assert is_edit_conflict(CodedError("wording can change"))

    class DetailedError(RuntimeError):
        details = {"code": "unsupported_symlink_change"}

    assert is_shell_conflict(DetailedError("wording can change"))
