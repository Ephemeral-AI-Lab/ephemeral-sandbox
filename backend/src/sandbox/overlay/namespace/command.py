"""User-command execution inside a mounted snapshot workspace."""

from __future__ import annotations

import os
import subprocess
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class CommandResult:
    exit_code: int
    stdout_ref: str
    stderr_ref: str


def run_user_command(
    *,
    command: tuple[str, ...],
    workspace_root: str | Path,
    cwd: str,
    env: dict[str, str],
    timeout_seconds: float | None,
    stdout_ref: str | Path,
    stderr_ref: str | Path,
) -> CommandResult:
    root = Path(workspace_root)
    resolved_cwd = _resolve_cwd(root, cwd)
    stdout_path = Path(stdout_ref)
    stderr_path = Path(stderr_ref)
    stdout_path.parent.mkdir(parents=True, exist_ok=True)
    stderr_path.parent.mkdir(parents=True, exist_ok=True)

    with stdout_path.open("wb") as stdout_file, stderr_path.open("wb") as stderr_file:
        completed = subprocess.run(
            list(command),
            cwd=resolved_cwd,
            env={**os.environ, **env, "GIT_OPTIONAL_LOCKS": "0"},
            stdout=stdout_file,
            stderr=stderr_file,
            timeout=timeout_seconds,
            check=False,
        )
    return CommandResult(
        exit_code=int(completed.returncode),
        stdout_ref=str(stdout_path),
        stderr_ref=str(stderr_path),
    )


def _resolve_cwd(workspace_root: Path, cwd: str) -> Path:
    root = workspace_root.resolve()
    candidate = Path(cwd)
    if not candidate.is_absolute():
        candidate = root / candidate
    resolved = candidate.resolve()
    if os.path.commonpath([str(root), str(resolved)]) != str(root):
        raise ValueError(f"cwd escapes mounted workspace: {cwd!r}")
    resolved.mkdir(parents=True, exist_ok=True)
    return resolved


__all__ = [
    "CommandResult",
    "run_user_command",
]
