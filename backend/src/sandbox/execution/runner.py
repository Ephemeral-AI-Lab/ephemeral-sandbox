"""Strategy dispatch for workspace-replaced command execution.

``run_workspace_replaced_command`` is the seam between the lifecycle
service (``service.py``) and the strategies (``strategies/``). It picks
an availability-ordered list of strategies and runs them until one
returns a non-fallback result.
"""

from __future__ import annotations

from collections.abc import Sequence
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
from sandbox.execution.strategies.base import ExecutionStrategy
from sandbox.execution.strategies.copy_backed import CopyBackedStrategy
from sandbox.execution.strategies.namespace import (
    PrivateNamespaceStrategy,
    detect_private_mount_namespace,
)


def run_workspace_replaced_command(
    *,
    spec: OverlayLayout,
    request: CommandExecRequest,
    run_dir: str | Path,
    timings: dict[str, float],
    strategies: Sequence[ExecutionStrategy] | None = None,
    mount_mode: MountMode | None = None,
    policy: CommandExecPolicy = DEFAULT_COMMAND_EXEC_POLICY,
) -> ShellProcessResult:
    """Run a command with the assigned workspace replaced by the leased view."""
    run_root = Path(run_dir)
    run_root.mkdir(parents=True, exist_ok=True)
    strategy_list: tuple[ExecutionStrategy, ...] = (
        tuple(strategies)
        if strategies is not None
        else _strategies_for_mount_mode(mount_mode, policy=policy)
    )
    for strategy in strategy_list:
        if not strategy.is_available():
            continue
        process = strategy.run(
            spec=spec,
            request=request,
            run_dir=run_root,
            timings=timings,
        )
        if not strategy.should_fall_back(process, run_dir=run_root):
            return process
        fallback_key = (
            "command_exec.private_mount_fallback"
            if strategy.name == MountMode.PRIVATE_NAMESPACE.value
            else f"command_exec.{strategy.name}_fallback"
        )
        timings[fallback_key] = 1.0
    raise RuntimeError("no command execution strategy succeeded")


def _strategies_for_mount_mode(
    mount_mode: MountMode | None,
    *,
    policy: CommandExecPolicy,
) -> tuple[ExecutionStrategy, ...]:
    modes = (
        (MountMode.PRIVATE_NAMESPACE, MountMode.COPY_BACKED)
        if mount_mode is None
        else (MountMode(mount_mode),)
    )
    return tuple(_build_strategy(mode, policy=policy) for mode in modes)


def _build_strategy(
    mode: MountMode, *, policy: CommandExecPolicy
) -> ExecutionStrategy:
    # detect_private_mount_namespace() is only invoked for modes that
    # actually need it, so callers pinning COPY_BACKED never pay the
    # `unshare -Urm true` probe cost.
    if mode is MountMode.COPY_BACKED:
        return CopyBackedStrategy(policy=policy)
    return PrivateNamespaceStrategy(
        available=detect_private_mount_namespace(),
        policy=policy,
    )


__all__ = ["run_workspace_replaced_command"]
