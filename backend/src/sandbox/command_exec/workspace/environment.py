"""cwd and environment policy for workspace-replaced commands."""

from __future__ import annotations

import os
import subprocess
from collections.abc import Mapping, Sequence
from pathlib import Path


def resolve_workspace_cwd(
    *,
    declared_workspace_root: str | Path,
    mounted_workspace_root: str | Path,
    cwd: str,
) -> Path:
    """Resolve *cwd* after replacing the declared workspace path.

    Absolute paths must stay under the declared workspace root. Relative paths
    resolve inside the mounted workspace. The returned path is inside
    ``mounted_workspace_root`` so copy-backed test mounts and real namespace
    mounts share the same policy.
    """
    declared_root = Path(declared_workspace_root)
    mounted_root = Path(mounted_workspace_root)
    raw = str(cwd or ".").strip() or "."
    candidate = Path(raw)
    if candidate.is_absolute():
        rel = _relative_to_declared_workspace(candidate, declared_root)
        resolved = mounted_root / rel
    else:
        resolved = mounted_root / candidate

    # Belt-and-suspenders containment check: the request boundary already
    # rejects `..` in relative cwd, but verify the resolved path still falls
    # inside the mounted workspace root before any side effect (mkdir).
    mounted_root_resolved = mounted_root.resolve(strict=False)
    resolved_check = resolved.resolve(strict=False)
    try:
        resolved_check.relative_to(mounted_root_resolved)
    except ValueError as exc:
        raise ValueError(
            f"cwd escapes workspace replacement root: {raw}"
        ) from exc

    resolved.mkdir(parents=True, exist_ok=True)
    return resolved


def run_command_to_refs(
    *,
    command: Sequence[str],
    declared_workspace_root: str | Path,
    mounted_workspace_root: str | Path,
    cwd: str,
    env: Mapping[str, str],
    timeout_seconds: float | None,
    stdout_ref: str | Path,
    stderr_ref: str | Path,
) -> int:
    """Run a guarded command and write stdout/stderr to reference files."""
    stdout_path = Path(stdout_ref)
    stderr_path = Path(stderr_ref)
    stdout_path.parent.mkdir(parents=True, exist_ok=True)
    stderr_path.parent.mkdir(parents=True, exist_ok=True)
    resolved_cwd = resolve_workspace_cwd(
        declared_workspace_root=declared_workspace_root,
        mounted_workspace_root=mounted_workspace_root,
        cwd=cwd,
    )
    with stdout_path.open("wb") as stdout_file, stderr_path.open("wb") as stderr_file:
        completed = subprocess.run(
            list(command),
            cwd=resolved_cwd,
            env=_command_environment(env),
            stdout=stdout_file,
            stderr=stderr_file,
            timeout=timeout_seconds,
            check=False,
        )
    return int(completed.returncode)


def _relative_to_declared_workspace(candidate: Path, declared_root: Path) -> Path:
    candidate_text = os.path.normpath(candidate.as_posix())
    root_text = os.path.normpath(declared_root.as_posix())
    if os.path.commonpath([root_text, candidate_text]) != root_text:
        raise ValueError(f"cwd escapes workspace replacement root: {candidate}")
    try:
        return Path(candidate_text).relative_to(root_text)
    except ValueError as exc:  # pragma: no cover - commonpath guards this.
        raise ValueError(f"cwd escapes workspace replacement root: {candidate}") from exc


# Env-var names a caller is NEVER allowed to override. These either alter
# the loader/interpreter trust boundary (LD_PRELOAD, BASH_ENV) or steer
# lookups for binaries the workspace-replacement layer assumes are sourced
# from the host (PATH, PYTHONPATH). Caller-supplied values are silently
# dropped — the host's value (if any) wins.
_RESTRICTED_ENV_KEYS = frozenset(
    {
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "LD_AUDIT",
        "DYLD_INSERT_LIBRARIES",
        "DYLD_LIBRARY_PATH",
        "PATH",
        "PYTHONPATH",
        "BASH_ENV",
        "ENV",
    }
)


def _command_environment(extra: Mapping[str, str]) -> dict[str, str]:
    safe_extra = {
        k: v for k, v in extra.items() if k not in _RESTRICTED_ENV_KEYS
    }
    return {**os.environ, **safe_extra, "GIT_OPTIONAL_LOCKS": "0"}


__all__ = [
    "resolve_workspace_cwd",
    "run_command_to_refs",
]
