"""Process.exec based file I/O helpers for sync Daytona live tests."""

from __future__ import annotations

import json
import shlex
from typing import Any

from sandbox.bash import extract_exit_code, wrap_bash_command


def write_text_via_exec(
    sandbox: Any,
    file_path: str,
    content: bytes | str,
    *,
    timeout: int = 120,
) -> None:
    """Write a UTF-8 text file through sync sandbox.process.exec."""
    text = content.decode("utf-8") if isinstance(content, bytes) else content
    response = sandbox.process.exec(
        wrap_bash_command(_build_write_text_file_command(file_path, text)),
        timeout=timeout,
    )
    cleaned, exit_code = extract_exit_code(
        getattr(response, "result", "") or "",
        fallback_exit_code=getattr(response, "exit_code", None),
    )
    if exit_code not in (0, None):
        raise RuntimeError(cleaned or f"write failed for {file_path}")


def read_text_via_exec(
    sandbox: Any,
    file_path: str,
    *,
    timeout: int = 120,
) -> str:
    """Read a UTF-8 text file through sync sandbox.process.exec."""
    response = sandbox.process.exec(
        wrap_bash_command(_build_read_text_file_command(file_path)),
        timeout=timeout,
    )
    cleaned, exit_code = extract_exit_code(
        getattr(response, "result", "") or "",
        fallback_exit_code=getattr(response, "exit_code", None),
    )
    if exit_code not in (0, None):
        raise RuntimeError(cleaned or f"read failed for {file_path}")
    payload = json.loads(cleaned or "{}")
    if not payload.get("exists"):
        raise FileNotFoundError(file_path)
    return str(payload.get("content", "") or "")


def _build_read_text_file_command(file_path: str) -> str:
    script = """
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
try:
    content = path.read_text(encoding="utf-8")
except FileNotFoundError:
    print(json.dumps({"exists": False}))
else:
    print(json.dumps({"exists": True, "content": content}))
"""
    return f"python3 -c {shlex.quote(script)} {shlex.quote(file_path)}"


def _build_write_text_file_command(file_path: str, content: str) -> str:
    import base64

    payload = base64.b64encode(content.encode("utf-8")).decode("ascii")
    script = """
import base64
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
path.parent.mkdir(parents=True, exist_ok=True)
path.write_text(base64.b64decode(sys.argv[2]).decode("utf-8"), encoding="utf-8")
"""
    return (
        f"python3 -c {shlex.quote(script)} "
        f"{shlex.quote(file_path)} {shlex.quote(payload)}"
    )
