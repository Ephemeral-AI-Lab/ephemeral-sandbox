"""Command-exec orchestration service."""

from __future__ import annotations

import logging
import shutil
from collections.abc import Callable, Mapping, Sequence
from pathlib import Path
from uuid import uuid4

from sandbox.execution.contract import (
    CommandExecRequest,
    CommandExecResult,
    MountMode,
    OCCMutationClient,
    SnapshotManifest,
    ShellProcessResult,
    WorkspaceCapture,
    WorkspaceLeaseClient,
    WorkspaceReplacementMountSpec,
)
from sandbox.execution.overlay_capture import capture_changes
from sandbox.execution.env_policy import (
    DEFAULT_COMMAND_EXEC_POLICY,
    CommandExecPolicy,
)
from sandbox.execution.strategy_base import ExecutionStrategy
from sandbox.execution.strategy_copy_backed import CopyBackedStrategy
from sandbox.execution.strategy_private_namespace import (
    PrivateNamespaceStrategy,
    detect_private_mount_namespace,
)
from sandbox.occ.changeset import ChangesetResult, CommitOptions
from sandbox.occ.overlay import overlay_path_changes_to_occ_changes
from sandbox.execution.path_change import OverlayPathChange
from sandbox.daemon.async_bridge import run_sync_in_executor
from sandbox._shared.clock import monotonic_now

logger = logging.getLogger(__name__)

_TRANSIENT_LOWERDIR_DIR = "transient-lowerdirs"

WorkspaceCommandRunner = Callable[
    ...,
    ShellProcessResult,
]


def run_workspace_replaced_command(
    *,
    spec: WorkspaceReplacementMountSpec,
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
        if not strategy.is_recoverable_failure(process, run_dir=run_root):
            return process
        fallback_key = (
            "command_exec.private_mount_fallback"
            if strategy.name == MountMode.PRIVATE_NAMESPACE.value
            else f"command_exec.{strategy.name}_fallback"
        )
        timings[fallback_key] = 1.0
    raise RuntimeError("no command execution strategy succeeded")


async def execute_command(
    request: CommandExecRequest,
    *,
    layer_stack: WorkspaceLeaseClient,
    occ_client: OCCMutationClient | None,
    storage_root: Path,
    timing_provider: Callable[[], Mapping[str, float]] | None = None,
    command_runner: WorkspaceCommandRunner = run_workspace_replaced_command,
    occ_apply: bool = True,
    mount_mode: MountMode | None = None,
) -> CommandExecResult:
    """Run one guarded command through snapshot, capture, and OCC apply."""
    if occ_apply and occ_client is None:
        raise ValueError("occ_client is required when occ_apply=True")
    total_start = monotonic_now()
    run_dir = _run_dir(storage_root, request.request_id)
    keep_capture_artifacts = False
    timings: dict[str, float] = {}
    timings["command_exec.handler_sync_prelude_s"] = (
        monotonic_now() - total_start
    )

    lease_start = monotonic_now()
    lease = layer_stack.prepare_workspace_snapshot(
        request_id=request.request_id,
    )
    timings.update(
        {
            **lease.timings,
            "command_exec.prepare_snapshot_s": monotonic_now() - lease_start,
        }
    )

    released = False
    try:
        spec = WorkspaceReplacementMountSpec(
            workspace_root=request.workspace_root,
            lowerdir=lease.lowerdir,
            upperdir=str(run_dir / "upper"),
            workdir=str(run_dir / "work"),
            scratch_root=str(storage_root),
        )
        runner_kwargs = {
            "spec": spec,
            "request": request,
            "run_dir": run_dir,
            "timings": timings,
        }
        if mount_mode is not None:
            runner_kwargs["mount_mode"] = mount_mode
        process = await run_sync_in_executor(command_runner, **runner_kwargs)

        capture_start = monotonic_now()
        if process.mount_mode == MountMode.COPY_BACKED:
            path_changes = capture_changes(
                spec.upperdir,
                lowerdir=spec.lowerdir,
                workspace_root=process.mounted_workspace_root,
                timings=timings,
            )
        else:
            path_changes = capture_changes(spec.upperdir, timings=timings)
        timings["command_exec.capture_upperdir_s"] = (
            monotonic_now() - capture_start
        )

        if occ_apply:
            assert occ_client is not None
            occ_start = monotonic_now()
            changeset = await _apply_workspace_capture(
                path_changes,
                occ_client=occ_client,
                snapshot=lease.manifest,
                request=request,
            )
            timings["command_exec.occ_apply_s"] = monotonic_now() - occ_start
        else:
            changeset = ChangesetResult(
                files=(),
                timings={},
                published_manifest_version=None,
            )
            timings["command_exec.occ_apply_s"] = 0.0
        release_start = monotonic_now()
        layer_stack.release_lease(
            lease_id=lease.lease_id,
        )
        released = True
        _drop_transient_lowerdir(lease, storage_root=storage_root)
        timings["command_exec.release_snapshot_s"] = (
            monotonic_now() - release_start
        )
        timings = {
            **timings,
            **changeset.timings,
            **(timing_provider() if timing_provider is not None else {}),
        }
        timings["api.shell.overlay_s"] = (
            timings.get("command_exec.mount_workspace_s", 0.0)
            + timings.get("command_exec.run_command_s", 0.0)
            + timings.get("command_exec.capture_upperdir_s", 0.0)
        )
        timings["api.shell.occ_apply_s"] = timings["command_exec.occ_apply_s"]
        timings["command_exec.total_s"] = monotonic_now() - total_start
        timings["api.shell.total_s"] = timings["command_exec.total_s"]
        result = CommandExecResult(
            exit_code=process.exit_code,
            stdout=_read_output_ref(process.stdout_ref),
            stderr=_read_output_ref(process.stderr_ref),
            stdout_ref=process.stdout_ref,
            stderr_ref=process.stderr_ref,
            workspace_capture=WorkspaceCapture(
                changes=path_changes,
                snapshot_version=lease.manifest_version,
                mount_mode=process.mount_mode,
                snapshot_manifest=lease.manifest,
            ),
            occ_result=changeset,
            timings=timings,
        )
        keep_capture_artifacts = not occ_apply
        return result
    finally:
        if not released:
            release_start = monotonic_now()
            layer_stack.release_lease(
                lease_id=lease.lease_id,
            )
            _drop_transient_lowerdir(lease, storage_root=storage_root)
            timings["command_exec.release_snapshot_s"] = (
                monotonic_now() - release_start
            )
        # Capture and OCC commit are done by the time we get here. For normal
        # shell execution, the run_dir tree is no longer load-bearing. The
        # no-OCC snapshot-overlay path returns stdout/stderr/content refs into
        # run_dir, so only its bulk intermediate trees are removed.
        if keep_capture_artifacts:
            _drop_non_capture_run_dir_entries(run_dir)
        else:
            shutil.rmtree(run_dir, ignore_errors=True)


def _strategies_for_mount_mode(
    mount_mode: MountMode | None,
    *,
    policy: CommandExecPolicy,
) -> tuple[ExecutionStrategy, ...]:
    if mount_mode is None:
        return (
            PrivateNamespaceStrategy(
                available=detect_private_mount_namespace(),
                policy=policy,
            ),
            CopyBackedStrategy(policy=policy),
        )
    selected = MountMode(mount_mode)
    if selected == MountMode.COPY_BACKED:
        return (CopyBackedStrategy(policy=policy),)
    return (
        PrivateNamespaceStrategy(
            available=detect_private_mount_namespace(),
            policy=policy,
        ),
    )


async def _apply_workspace_capture(
    path_changes: Sequence[OverlayPathChange],
    *,
    occ_client: OCCMutationClient,
    snapshot: SnapshotManifest,
    request: CommandExecRequest,
) -> ChangesetResult:
    typed_changes = overlay_path_changes_to_occ_changes(path_changes)
    if not typed_changes:
        return ChangesetResult(
            files=(),
            timings={},
            published_manifest_version=None,
        )
    # Single-path captures opt out of cross-path atomicity so
    # ``CommitQueue._disjoint_batches`` can coalesce them with other
    # concurrent disjoint commits. Multi-path captures keep ``atomic=True``
    # so a single failed validation rejects the whole capture.
    distinct_paths = {change.path for change in typed_changes}
    is_atomic = len(distinct_paths) > 1
    result = await occ_client.apply_changeset(
        typed_changes,
        snapshot=snapshot,
        options=CommitOptions(atomic=is_atomic),
        workspace_ref=request.workspace_ref,
    )
    return result


def _drop_non_capture_run_dir_entries(run_dir: Path) -> None:
    for name in ("workspace", "work", "lower", "merged"):
        shutil.rmtree(run_dir / name, ignore_errors=True)


def _read_output_ref(path: str) -> str:
    return Path(path).read_bytes().decode("utf-8", "replace")


def _run_dir(storage_root: Path, request_id: str) -> Path:
    safe_id = "".join(
        char if char.isalnum() or char in ("-", "_") else "-"
        for char in request_id
    ).strip("-")
    run_parent = _command_exec_runtime_root(storage_root)
    return run_parent / f"{safe_id or 'request'}-{uuid4().hex[:8]}"


def _command_exec_runtime_root(storage_root: Path) -> Path:
    return storage_root / "runtime" / "command_exec"


def _drop_transient_lowerdir(lease: object, *, storage_root: Path) -> None:
    raw = str(getattr(lease, "lowerdir", "")).strip()
    if not raw:
        return
    lowerdir = Path(raw)
    scratch_dir = lowerdir.parent
    transient_root = storage_root / "runtime" / _TRANSIENT_LOWERDIR_DIR
    if (
        lowerdir.name != "lower"
        or scratch_dir.parent.name != _TRANSIENT_LOWERDIR_DIR
        or not scratch_dir.resolve(strict=False).is_relative_to(
            transient_root.resolve(strict=False)
        )
    ):
        logger.warning(
            "refusing to drop unexpected transient lowerdir path: %s",
            lowerdir,
        )
        return
    try:
        shutil.rmtree(scratch_dir)
    except OSError:
        logger.warning(
            "failed to drop transient lowerdir scratch dir: %s",
            scratch_dir,
            exc_info=True,
        )


__all__ = [
    "execute_command",
    "run_workspace_replaced_command",
]
