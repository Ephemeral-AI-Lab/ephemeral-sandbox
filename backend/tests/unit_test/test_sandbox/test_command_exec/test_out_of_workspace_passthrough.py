"""Namespace-child path policy checks for Phase 2 foreground tools."""

from __future__ import annotations

from pathlib import Path

from sandbox._shared.models import Intent, ToolCallRequest
from sandbox.overlay.namespace_entrypoint import execute_tool_payload


def test_absolute_non_denylisted_path_can_be_written_by_primitive(
    tmp_path: Path,
) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    target = tmp_path / "outside" / "foo.txt"
    req = ToolCallRequest(
        invocation_id="r1",
        agent_id="agent",
        verb="write_file",
        intent=Intent.WRITE_ALLOWED,
        args={"path": target.as_posix(), "content": "fresh\n"},
    )

    result = execute_tool_payload(
        {
            "workspace_root": workspace.as_posix(),
            "tool_call": req.to_payload(),
            "stdout_ref": (tmp_path / "stdout").as_posix(),
            "stderr_ref": (tmp_path / "stderr").as_posix(),
        }
    )

    assert result["success"] is True
    assert target.read_text(encoding="utf-8") == "fresh\n"


def test_system_host_path_is_denied_before_write(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    req = ToolCallRequest(
        invocation_id="r1",
        agent_id="agent",
        verb="write_file",
        intent=Intent.WRITE_ALLOWED,
        args={"path": "/etc/hosts", "content": "bad"},
    )

    result = execute_tool_payload(
        {
            "workspace_root": workspace.as_posix(),
            "tool_call": req.to_payload(),
            "stdout_ref": (tmp_path / "stdout").as_posix(),
            "stderr_ref": (tmp_path / "stderr").as_posix(),
        }
    )

    assert result["success"] is False
    assert result["error"]["kind"] == "forbidden_host_path"
