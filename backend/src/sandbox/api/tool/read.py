"""Public sandbox file-read verb."""

from __future__ import annotations

from sandbox.api.tool._payload import timings_from_payload
from sandbox.contract import ReadFileRequest, ReadFileResult
from sandbox.host.daemon_client import call_daemon_api


async def read_file(sandbox_id: str, request: ReadFileRequest) -> ReadFileResult:
    """Read one UTF-8 text file through the sandbox daemon."""
    raw = await call_daemon_api(
        sandbox_id,
        "api.read_file",
        {"path": request.path},
        timeout=60,
    )
    return ReadFileResult(
        success=bool(raw.get("success", False)),
        exists=bool(raw.get("exists", False)),
        content=str(raw.get("content", "")),
        encoding=str(raw.get("encoding", "utf-8")),
        timings=timings_from_payload(raw.get("timings")),
    )


__all__ = ["read_file"]
