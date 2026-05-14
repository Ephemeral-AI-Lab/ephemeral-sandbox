"""Tests for sandbox API payload helper contracts."""

from __future__ import annotations

import pytest

from sandbox.api.tool._payload import (
    caller_audit_fields,
    error_message,
    int_from_payload,
    is_transient_transport_error,
    normalize_overlay_cwd,
)
from sandbox.api._impl._classifiers import is_edit_conflict, is_shell_conflict
from sandbox.models import SandboxCaller


def test_caller_audit_fields_keeps_required_daemon_keys_and_non_empty_fields() -> None:
    caller = SandboxCaller(
        agent_id="agent-1",
        task_center_run_id="tc-run",
        tool_id="tool-1",
    )

    assert caller_audit_fields(caller) == {
        "agent_id": "agent-1",
        "run_id": "",
        "agent_run_id": "",
        "task_id": "",
        "task_center_run_id": "tc-run",
        "tool_id": "tool-1",
    }
    assert caller.audit_fields() == caller_audit_fields(caller)


def test_normalize_overlay_cwd_strips_non_empty_paths() -> None:
    assert normalize_overlay_cwd(None) == "."
    assert normalize_overlay_cwd("") == "."
    assert normalize_overlay_cwd("   ") == "."
    assert normalize_overlay_cwd("  src/pkg  ") == "src/pkg"


def test_error_message_strips_internal_error_prefix() -> None:
    assert error_message(RuntimeError("internal_error: anchor not found")) == (
        "anchor not found"
    )


def test_transient_transport_error_uses_word_boundaries() -> None:
    assert is_transient_transport_error(
        RuntimeError("DaytonaError: Failed to execute command")
    )
    assert not is_transient_transport_error(RuntimeError("NotDaytonaError"))


def test_int_from_payload_is_strict_about_boundary_types() -> None:
    assert int_from_payload(3, default=0) == 3
    assert int_from_payload(None, default=7) == 7
    with pytest.raises(TypeError):
        int_from_payload(True, default=0)
    with pytest.raises(TypeError):
        int_from_payload("1", default=0)
    with pytest.raises(TypeError):
        int_from_payload(1.5, default=0)


def test_conflict_classifiers_prefer_typed_error_codes() -> None:
    class CodedError(RuntimeError):
        code = "anchor_not_found"

    assert is_edit_conflict(CodedError("wording can change"))

    class DetailedError(RuntimeError):
        details = {"code": "unsupported_symlink_change"}

    assert is_shell_conflict(DetailedError("wording can change"))
