"""Check recent output for an active PTY command."""

from __future__ import annotations

from pydantic import BaseModel, Field

import sandbox.api as sandbox_api
from sandbox.shared.models import Intent, PtyProgressRequest
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from tools.sandbox._lib.pty_command_tool import PtyCommandOutput, pty_tool_result
from tools.sandbox._lib.tool_context import (
    sandbox_caller_from_tool_context,
    sandbox_id_or_missing_error_result,
)


class PtyProgressInput(BaseModel):
    pty_session_id: str = Field(..., min_length=1)
    time: float = Field(default=1.0, ge=0.0)
    max_tokens: int | None = Field(default=None, ge=1)


@tool(
    name="check_pty_command_progress",
    description="Return recent terminal output for an active PTY command session.",
    short_description="Check PTY output.",
    input_model=PtyProgressInput,
    output_model=PtyCommandOutput,
    intent=Intent.READ_ONLY,
)
async def check_pty_command_progress(
    pty_session_id: str,
    time: float = 1.0,
    max_tokens: int | None = None,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    sandbox_id, sandbox_id_error = sandbox_id_or_missing_error_result(context)
    if sandbox_id_error is not None:
        return sandbox_id_error
    result = await sandbox_api.check_pty_command_progress(
        sandbox_id,
        PtyProgressRequest(
            caller=sandbox_caller_from_tool_context(context),
            pty_session_id=pty_session_id,
            time=time,
            max_tokens=max_tokens,
        ),
    )
    return pty_tool_result(result)


__all__ = ["check_pty_command_progress"]
