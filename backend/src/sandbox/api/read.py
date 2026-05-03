"""Public sandbox file-read verb."""

from __future__ import annotations

import json
import shlex

from sandbox.api.models import ReadFileRequest, ReadFileResult
from sandbox.api.raw_exec import raw_exec


async def read_file(sandbox_id: str, request: ReadFileRequest) -> ReadFileResult:
    """Read one UTF-8 text file through raw provider exec."""
    script = (
        "import json,pathlib,sys; "
        "p=pathlib.Path(sys.argv[1]); "
        "\ntry:\n"
        " data=p.read_text(encoding='utf-8')\n"
        " print(json.dumps({'exists': True, 'content': data}))\n"
        "except FileNotFoundError:\n"
        " print(json.dumps({'exists': False, 'content': ''}))"
    )
    result = await raw_exec(
        sandbox_id,
        f"python3 -c {shlex.quote(script)} {shlex.quote(request.path)}",
    )
    if result.exit_code != 0:
        return ReadFileResult(
            success=False,
            exists=False,
            content="",
        )
    try:
        payload = json.loads(result.stdout or "{}")
    except json.JSONDecodeError:
        return ReadFileResult(success=False, exists=False, content="")
    return ReadFileResult(
        success=True,
        exists=bool(payload.get("exists", False)),
        content=str(payload.get("content", "")),
    )


__all__ = ["read_file"]
