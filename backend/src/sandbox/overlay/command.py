"""User-command execution inside a prepared snapshot workspace."""

from __future__ import annotations

import os
import subprocess
from dataclasses import dataclass
from pathlib import Path

# Host env vars that the user command needs to function (PATH for argv0
# resolution, HOME/TERM for shells, locale vars for tooling that branches on
# encoding). Host secrets are intentionally absent from this allow-list.
_HOST_ENV_ALLOWLIST: tuple[str, ...] = (
    "PATH",
    "HOME",
    "USER",
    "LANG",
    "LC_ALL",
    "TERM",
    "TZ",
)


@dataclass(frozen=True)
class OverlayCommandResult:
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
) -> OverlayCommandResult:
    root = Path(workspace_root)
    resolved_cwd = _validate_cwd(root, cwd)
    _ensure_cwd(resolved_cwd)
    stdout_path = Path(stdout_ref)
    stderr_path = Path(stderr_ref)
    stdout_path.parent.mkdir(parents=True, exist_ok=True)
    stderr_path.parent.mkdir(parents=True, exist_ok=True)

    base_env = {
        key: os.environ[key]
        for key in _HOST_ENV_ALLOWLIST
        if key in os.environ
    }
    child_env = {**base_env, **env, "GIT_OPTIONAL_LOCKS": "0"}

    with stdout_path.open("wb") as stdout_file, stderr_path.open("wb") as stderr_file:
        try:
            completed = subprocess.run(
                list(command),
                cwd=resolved_cwd,
                env=child_env,
                stdout=stdout_file,
                stderr=stderr_file,
                timeout=timeout_seconds,
                check=False,
            )
            exit_code = int(completed.returncode)
        except subprocess.TimeoutExpired:
            # 124 follows the GNU `timeout(1)` convention so callers can
            # distinguish a user-command timeout from infrastructure failure.
            exit_code = 124
    return OverlayCommandResult(
        exit_code=exit_code,
        stdout_ref=str(stdout_path),
        stderr_ref=str(stderr_path),
    )


def _validate_cwd(workspace_root: Path, cwd: str) -> Path:
    root = workspace_root.resolve()
    candidate = Path(cwd)
    if not candidate.is_absolute():
        candidate = root / candidate
    resolved = candidate.resolve()
    if os.path.commonpath([str(root), str(resolved)]) != str(root):
        raise ValueError(f"cwd escapes mounted workspace: {cwd!r}")
    return resolved


def _ensure_cwd(resolved_cwd: Path) -> None:
    resolved_cwd.mkdir(parents=True, exist_ok=True)


__all__ = [
    "OverlayCommandResult",
    "run_user_command",
]
