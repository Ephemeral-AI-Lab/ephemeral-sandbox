"""Copy-backed workspace command execution strategy.

Also hosts the declared-workspace path-rewrite helpers used only by this
strategy: when a command lands on the copy-backed merged tree we cannot
replace the declared workspace path at the kernel mount layer, so any
argv element or env value that literally names the declared workspace
must be rewritten to point at the temporary merged tree.
"""

from __future__ import annotations

import shutil
from collections.abc import Mapping
from dataclasses import replace
from pathlib import Path
from typing import AbstractSet

from sandbox.execution.contract import (
    CommandExecRequest,
    MountMode,
    ShellProcessResult,
    WorkspaceReplacementMountSpec,
)
from sandbox.execution.env_policy import (
    DEFAULT_COMMAND_EXEC_POLICY,
    CommandExecPolicy,
)
from sandbox.execution.strategy_base import ExecutionStrategy
from sandbox.execution.subprocess_runner import run_command_to_refs
from sandbox.timing import monotonic_now

WORKSPACE_ENV_KEYS = frozenset({"WORKSPACE_DIR", "PWD", "OLDPWD"})


class CopyBackedStrategy(ExecutionStrategy):
    """Run a command against a copied lowerdir and capture that upperdir."""

    name = "copy_backed"

    def __init__(
        self,
        *,
        policy: CommandExecPolicy = DEFAULT_COMMAND_EXEC_POLICY,
    ) -> None:
        self._policy = policy

    def is_available(self) -> bool:
        return True

    def run(
        self,
        *,
        spec: WorkspaceReplacementMountSpec,
        request: CommandExecRequest,
        run_dir: Path,
        timings: dict[str, float],
    ) -> ShellProcessResult:
        lowerdir = Path(spec.lowerdir)
        upperdir = Path(spec.upperdir)
        workdir = Path(spec.workdir)
        merged = run_dir / "workspace"
        stdout_ref = run_dir / "stdout.bin"
        stderr_ref = run_dir / "stderr.bin"

        mount_start = monotonic_now()
        for directory in (upperdir, workdir, merged):
            _assert_under_scratch_root(directory, spec)
            if directory.exists():
                shutil.rmtree(directory)
            directory.mkdir(parents=True)
        if lowerdir.exists():
            shutil.copytree(lowerdir, merged, symlinks=True, dirs_exist_ok=True)
        timings["command_exec.mount_workspace_s"] = monotonic_now() - mount_start

        run_request = replace(
            request,
            command=rewrite_declared_workspace_refs(
                request.command,
                workspace_root=spec.workspace_root,
                mounted_workspace_root=str(merged),
            ),
            env=rewrite_declared_workspace_env(
                request.env,
                workspace_root=spec.workspace_root,
                mounted_workspace_root=str(merged),
                workspace_env_keys=self._policy.workspace_env_keys,
            ),
        )
        run_start = monotonic_now()
        exit_code = run_command_to_refs(
            command=run_request.command,
            declared_workspace_root=spec.workspace_root,
            mounted_workspace_root=merged,
            cwd=run_request.cwd,
            env=run_request.env,
            timeout_seconds=run_request.timeout_seconds,
            stdout_ref=stdout_ref,
            stderr_ref=stderr_ref,
            policy=self._policy,
        )
        timings["command_exec.run_command_s"] = monotonic_now() - run_start
        return ShellProcessResult(
            exit_code=exit_code,
            stdout_ref=str(stdout_ref),
            stderr_ref=str(stderr_ref),
            mounted_workspace_root=str(merged),
            mount_mode=MountMode.COPY_BACKED,
        )

    def is_recoverable_failure(
        self,
        result: ShellProcessResult,
        *,
        run_dir: Path,
    ) -> bool:
        del result, run_dir
        return False


def _assert_under_scratch_root(
    path: Path,
    spec: WorkspaceReplacementMountSpec,
) -> None:
    scratch_root = Path(spec.scratch_root).resolve(strict=False)
    resolved = path.resolve(strict=False)
    if resolved == scratch_root or not resolved.is_relative_to(scratch_root):
        raise RuntimeError(f"path escapes scratch root: {resolved}")


def rewrite_declared_workspace_refs(
    command: tuple[str, ...],
    workspace_root: str,
    mounted_workspace_root: str,
) -> tuple[str, ...]:
    """Map path-like workspace references to the copy-backed mounted tree."""
    root = str(workspace_root).rstrip("/") or "/"
    if root == "/":
        return command
    return tuple(
        _rewrite_workspace_paths(
            part,
            workspace_root=root,
            mounted_workspace_root=str(mounted_workspace_root),
        )
        for part in command
    )


def rewrite_declared_workspace_env(
    env: Mapping[str, str],
    *,
    workspace_root: str,
    mounted_workspace_root: str,
    workspace_env_keys: AbstractSet[str] = WORKSPACE_ENV_KEYS,
) -> dict[str, str]:
    """Rewrite env values that explicitly name the assigned workspace."""
    root = str(workspace_root).rstrip("/") or "/"
    rewritten: dict[str, str] = {}
    for key, value in env.items():
        env_key = str(key)
        env_value = str(value)
        if env_key in workspace_env_keys:
            env_value = _rewrite_path_token(
                env_value,
                workspace_root=root,
                mounted_workspace_root=mounted_workspace_root,
            )
        rewritten[env_key] = env_value
    return rewritten


def _rewrite_workspace_paths(
    value: str,
    *,
    workspace_root: str,
    mounted_workspace_root: str,
) -> str:
    result: list[str] = []
    index = 0
    while index < len(value):
        if _path_starts_at(value, index, workspace_root):
            result.append(mounted_workspace_root)
            index += len(workspace_root)
            continue
        result.append(value[index])
        index += 1
    return "".join(result)


def _rewrite_path_token(
    value: str,
    *,
    workspace_root: str,
    mounted_workspace_root: str,
) -> str:
    if value == workspace_root:
        return mounted_workspace_root
    if value.startswith(workspace_root + "/"):
        return mounted_workspace_root + value[len(workspace_root):]
    return value


def _path_starts_at(value: str, index: int, workspace_root: str) -> bool:
    if not value.startswith(workspace_root, index):
        return False
    before = value[index - 1] if index > 0 else ""
    after_index = index + len(workspace_root)
    after = value[after_index] if after_index < len(value) else ""
    if before and before not in " \t\n\r=:;,&|>(\"'":
        return False
    return not after or after in "/ \t\n\r:;,&|)<\"'"


__all__ = [
    "CopyBackedStrategy",
    "WORKSPACE_ENV_KEYS",
    "rewrite_declared_workspace_env",
    "rewrite_declared_workspace_refs",
]
