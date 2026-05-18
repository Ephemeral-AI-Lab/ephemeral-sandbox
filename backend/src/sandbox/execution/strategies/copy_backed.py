"""Copy-backed workspace command execution strategy."""

from __future__ import annotations

import shutil
from dataclasses import replace
from pathlib import Path

from sandbox.execution.contract import (
    CommandExecRequest,
    MountMode,
    OverlayLayout,
    ShellProcessResult,
)
from sandbox.execution.env_policy import (
    DEFAULT_COMMAND_EXEC_POLICY,
    CommandExecPolicy,
)
from sandbox.execution.overlay.change_synthesis import synthesize_writes
from sandbox.execution.strategies._workspace_rewrite import (
    rewrite_declared_workspace_env,
    rewrite_declared_workspace_refs,
)
from sandbox.execution.strategies.base import ExecutionStrategy
from sandbox.execution.subprocess_runner import run_command_to_refs
from sandbox._shared.clock import monotonic_now


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
        spec: OverlayLayout,
        request: CommandExecRequest,
        run_dir: Path,
        timings: dict[str, float],
    ) -> ShellProcessResult:
        base_repo = Path(spec.base_repo)
        writes = Path(spec.writes)
        kernel_scratch = Path(spec.kernel_scratch)
        merged = run_dir / "workspace"
        stdout_ref = run_dir / "stdout.bin"
        stderr_ref = run_dir / "stderr.bin"

        mount_start = monotonic_now()
        for directory in (writes, kernel_scratch, merged):
            _assert_under_scratch_root(directory, spec)
            if directory.exists():
                shutil.rmtree(directory)
            directory.mkdir(parents=True)
        if base_repo.exists():
            shutil.copytree(base_repo, merged, symlinks=True, dirs_exist_ok=True)
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
        synthesize_writes(
            merged=merged,
            base_repo=base_repo,
            into=writes,
            timings=timings,
        )
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
    spec: OverlayLayout,
) -> None:
    scratch_root = Path(spec.scratch_root).resolve(strict=False)
    resolved = path.resolve(strict=False)
    if resolved == scratch_root or not resolved.is_relative_to(scratch_root):
        raise RuntimeError(f"path escapes scratch root: {resolved}")


__all__ = [
    "CopyBackedStrategy",
    "rewrite_declared_workspace_env",
    "rewrite_declared_workspace_refs",
]
