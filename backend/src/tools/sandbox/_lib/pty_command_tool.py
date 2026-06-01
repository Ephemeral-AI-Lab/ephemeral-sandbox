"""Shared helpers for PTY command control tools."""

from __future__ import annotations

import json

from pydantic import BaseModel

from sandbox.shared.models import ExecCommandResult
from tools._framework.core.base import ToolResult


class PtyCommandOutput(BaseModel):
    status: str
    exit_code: int | None
    output: dict[str, str]
    pty_session_id: str | None = None


def pty_tool_result(result: ExecCommandResult) -> ToolResult:
    payload = {
        "status": result.status,
        "exit_code": result.exit_code,
        "output": {
            "stdout": result.output.stdout,
            "stderr": result.output.stderr,
        },
    }
    if result.pty_session_id:
        payload["pty_session_id"] = result.pty_session_id
    return ToolResult(
        output=json.dumps(payload),
        is_error=result.status == "error",
        metadata={"status": result.status, "pty_session_id": result.pty_session_id or ""},
    )


__all__ = ["PtyCommandOutput", "pty_tool_result"]
