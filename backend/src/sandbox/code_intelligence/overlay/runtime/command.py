"""User command execution for the sandbox-side overlay runtime."""

from __future__ import annotations

import os
import subprocess


def run_user_command(
    *, user_cmd: str, stdin_bytes: bytes | None, cwd: str, stdout_path: str
) -> tuple[bytes, int]:
    """Run the user command under the merged overlay view."""
    proc = subprocess.Popen(
        ["bash", "-o", "pipefail", "-lc", user_cmd],
        stdin=subprocess.PIPE if stdin_bytes is not None else None,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        cwd=cwd,
        env={**os.environ, "GIT_OPTIONAL_LOCKS": "0"},
    )
    if stdin_bytes is not None:
        assert proc.stdin is not None
        proc.stdin.write(stdin_bytes)
        proc.stdin.close()
    assert proc.stdout is not None
    chunks: list[bytes] = []
    with open(stdout_path, "wb") as stdout_file:
        while True:
            chunk = os.read(proc.stdout.fileno(), 8192)
            if not chunk:
                break
            chunks.append(chunk)
            stdout_file.write(chunk)
            stdout_file.flush()
    exit_code = proc.wait()
    return b"".join(chunks), exit_code


__all__ = ["run_user_command"]
