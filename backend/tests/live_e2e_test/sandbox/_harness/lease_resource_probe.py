"""Command-exec telemetry harness for O(1) overlay-mount verification.

The O(1) native tests intentionally consume the same per-shell timing map that
live command execution emits. That keeps the probe on the production lease,
mount, capture, and resource-audit path instead of measuring a synthetic
``du``/``df`` side channel.
"""

from __future__ import annotations

import asyncio
import importlib
import sys
import statistics
from collections.abc import Iterator, Sequence
from contextlib import contextmanager
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Literal, Protocol

from sandbox._shared.models import Intent, ToolCallRequest

OverlayPath = Literal["mount_syscalls"]

MOUNT_SYSCALLS: OverlayPath = "mount_syscalls"

LOWER_SIDE_LIMIT_BYTES = 4 * 1024
UPPERDIR_NO_WRITE_LIMIT_BYTES = 64 * 1024
MOUNT_SLOPE_LIMIT_S_PER_LAYER = 0.005
READ_CPU_SLOPE_LIMIT_S_PER_LAYER = 50.0 / 1_000_000.0
RSS_LIMIT_BYTES_PER_LEASE = 2 * 1024 * 1024


class LayerStackLike(Protocol):
    storage_root: Path

    def publish_changes(self, changes: Sequence[object]) -> object: ...


class _NoopOccClient:
    async def apply_changeset(self, *args: object, **kwargs: object) -> object:
        del args, kwargs
        changeset_result = importlib.import_module(
            "sandbox.occ.changeset"
        ).ChangesetResult
        return changeset_result(files=(), timings={}, published_manifest_version=None)

    async def run_maintenance_after_publish(
        self,
        result: object,
        *,
        workspace_ref: str | None = None,
    ) -> dict[str, float]:
        del result, workspace_ref
        return {}


@dataclass(frozen=True)
class ShellTelemetry:
    request_id: str
    requested_path: OverlayPath
    mount_mode: str
    timings: dict[str, float]
    stdout: str
    stderr: str

    @property
    def lower_side_bytes(self) -> int:
        """Workspace bytes materialized below the command upperdir."""
        return int(self.timings.get("resource.command_exec.workspace_tree_bytes", 0.0))

    @property
    def upperdir_bytes(self) -> int:
        return int(self.timings.get("resource.command_exec.upperdir_tree_bytes", 0.0))

    @property
    def mount_workspace_s(self) -> float:
        return float(self.timings.get("command_exec.mount_workspace_s", 0.0))

    @property
    def cmd_user_s(self) -> float:
        return float(self.timings.get("cmd.exec.user_s", 0.0))

    @property
    def rss_bytes(self) -> int:
        return int(self.timings.get("resource.process.rss_bytes", 0.0))

    @property
    def manifest_depth(self) -> int:
        return int(self.timings.get("resource.layer_stack.manifest_depth", 0.0))


def has_cap_sys_admin() -> bool:
    """Return whether the native O(1) harness can exercise mount syscalls."""
    if sys.platform != "linux":
        return False
    detect_private_mount_namespace = importlib.import_module(
        "sandbox.overlay.namespace_runner"
    ).detect_private_mount_namespace
    mount_syscalls_supported = importlib.import_module(
        "sandbox.overlay.mount_syscalls"
    ).mount_syscalls_supported
    return detect_private_mount_namespace() and mount_syscalls_supported()


def build_layer_stack(
    root: Path,
    *,
    manifest_depth: int,
    base_payload_bytes: int = 4096,
    per_layer_payload_bytes: int = 128,
) -> LayerStackLike:
    """Create a LayerStack with a bottom ``known_file.bin`` and M-1 overlays."""
    stack_cls = importlib.import_module("sandbox.layer_stack.stack").LayerStack
    write_layer_change = importlib.import_module(
        "sandbox.layer_stack.changes"
    ).WriteLayerChange
    root.mkdir(parents=True, exist_ok=True)
    source_root = root / "sources"
    source_root.mkdir(parents=True, exist_ok=True)
    stack = stack_cls(root / "stack")

    for index in range(manifest_depth):
        if index == 0:
            source = _write_payload(
                source_root / "known_file.bin",
                size=base_payload_bytes,
                byte=b"k",
            )
            path = "known_file.bin"
        else:
            source = _write_payload(
                source_root / f"layer-{index:04d}.txt",
                size=per_layer_payload_bytes,
                byte=bytes([97 + (index % 26)]),
            )
            path = f"layers/{index:04d}.txt"
        stack.publish_changes([write_layer_change(path=path, source_path=str(source))])

    return stack


def as_requested_path(
    telemetry: ShellTelemetry,
    requested_path: OverlayPath,
    *,
    request_id: str | None = None,
) -> ShellTelemetry:
    return replace(
        telemetry,
        requested_path=requested_path,
        request_id=request_id or telemetry.request_id,
    )


async def run_shell_batch(
    *,
    stack: LayerStackLike,
    workspace_root: Path,
    writable_root: Path,
    requested_path: OverlayPath,
    commands: Sequence[str],
    request_prefix: str,
    timeout_seconds: float = 60.0,
) -> list[ShellTelemetry]:
    """Run commands through command-exec using the namespace-only overlay path."""
    workspace_root.mkdir(parents=True, exist_ok=True)
    writable_root.mkdir(parents=True, exist_ok=True)
    ephemeral_pipeline = importlib.import_module(
        "sandbox.ephemeral_workspace.pipeline"
    ).EphemeralPipeline
    layer_stack_adapter = importlib.import_module(
        "sandbox.occ.layer_stack_adapter"
    ).LayerStackPortAdapter
    pipeline = ephemeral_pipeline(
        occ_client=_NoopOccClient(),
        workspace_ref=str(stack.storage_root),
        layer_stack=layer_stack_adapter(stack),
        workspace_root=workspace_root.as_posix(),
    )
    expected_mode = "private_namespace"

    async def _run_one(index: int, command: str) -> ShellTelemetry:
        req = ToolCallRequest(
            invocation_id=f"{request_prefix}-{index:04d}",
            agent_id="lease-resource-probe",
            verb="shell",
            intent=Intent.WRITE_ALLOWED,
            args={
                "command": command,
                "cwd": ".",
                "timeout_seconds": timeout_seconds,
                "description": f"o1 {requested_path}",
            },
        )
        result = await pipeline.run_tool_call(req)
        actual_mode = expected_mode
        if actual_mode != expected_mode:
            raise AssertionError(
                f"{req.invocation_id} ran {actual_mode}; expected "
                f"{expected_mode} for {requested_path}"
            )
        exit_code = int(result.get("exit_code", 1))
        stderr = str(result.get("stderr") or "")
        if exit_code != 0:
            raise AssertionError(
                f"{req.invocation_id} exited {exit_code}: {stderr}"
            )
        return ShellTelemetry(
            request_id=req.invocation_id,
            requested_path=requested_path,
            mount_mode=actual_mode,
            timings=dict(result.get("timings") or {}),
            stdout=str(result.get("stdout") or ""),
            stderr=stderr,
        )

    with _overlay_writable_root(writable_root):
        return list(await asyncio.gather(*(_run_one(i, cmd) for i, cmd in enumerate(commands))))


def assert_mount_syscalls_o1_bounds(rows: Sequence[ShellTelemetry]) -> None:
    """Assert mount-syscall lower-side O(1) and no unexpected upper writes.

    Collect every offending lease before raising so adversarial self-tests can
    prove multiple regressions are named in one failure message.
    """
    failures: list[str] = []
    for row in rows:
        if row.lower_side_bytes > LOWER_SIDE_LIMIT_BYTES:
            failures.append(
                f"{row.request_id}: lower-side bytes "
                f"{row.lower_side_bytes} > {LOWER_SIDE_LIMIT_BYTES}"
            )
        if row.upperdir_bytes > UPPERDIR_NO_WRITE_LIMIT_BYTES:
            failures.append(
                f"{row.request_id}: upper write bytes "
                f"{row.upperdir_bytes} > {UPPERDIR_NO_WRITE_LIMIT_BYTES}"
            )
    if failures:
        raise AssertionError("O(1) lease bound regression(s): " + "; ".join(failures))


def assert_mount_slope_by_depth(
    rows_by_depth: dict[int, Sequence[ShellTelemetry]],
) -> None:
    medians = {
        depth: statistics.median(row.mount_workspace_s for row in rows)
        for depth, rows in sorted(rows_by_depth.items())
        if rows
    }
    _assert_slope(
        medians,
        limit=MOUNT_SLOPE_LIMIT_S_PER_LAYER,
        label="mount_workspace_s",
    )
    lower_side_by_depth = {
        depth: max(row.lower_side_bytes for row in rows)
        for depth, rows in rows_by_depth.items()
        if rows
    }
    offenders = {
        depth: value
        for depth, value in lower_side_by_depth.items()
        if value > LOWER_SIDE_LIMIT_BYTES
    }
    if offenders:
        raise AssertionError(
            "Bound B lower-side disk is not flat in M: "
            + ", ".join(f"M={depth} lower={value}" for depth, value in offenders.items())
        )


def assert_read_cpu_slope_by_depth(
    rows_by_depth: dict[int, Sequence[ShellTelemetry]],
) -> None:
    medians = {
        depth: statistics.median(row.cmd_user_s for row in rows)
        for depth, rows in sorted(rows_by_depth.items())
        if rows
    }
    _assert_slope(
        medians,
        limit=READ_CPU_SLOPE_LIMIT_S_PER_LAYER,
        label="cmd.exec.user_s",
    )


def assert_memory_bound(*, n1: Sequence[ShellTelemetry], n200: Sequence[ShellTelemetry]) -> None:
    rss_at_1 = max((row.rss_bytes for row in n1), default=0)
    rss_at_200 = max((row.rss_bytes for row in n200), default=0)
    delta_per_lease = max(0, rss_at_200 - rss_at_1) / 200.0
    if delta_per_lease > RSS_LIMIT_BYTES_PER_LEASE:
        raise AssertionError(
            "Memory bound FAIL: "
            f"(rss_at_N200={rss_at_200} - rss_at_N1={rss_at_1}) / 200 "
            f"= {delta_per_lease:.0f} bytes > {RSS_LIMIT_BYTES_PER_LEASE}"
        )


def fail_if_depth_errors(errors: dict[int, BaseException], *, label: str) -> None:
    if not errors:
        return
    details = "; ".join(f"M={depth}: {exc!r}" for depth, exc in sorted(errors.items()))
    raise AssertionError(f"{label} failed at one or more depths: {details}")


def _write_payload(path: Path, *, size: int, byte: bytes) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(byte * size)
    return path


def _assert_slope(
    values_by_depth: dict[int, float],
    *,
    limit: float,
    label: str,
) -> None:
    if len(values_by_depth) < 2:
        return
    depths = sorted(values_by_depth)
    for left, right in zip(depths, depths[1:]):
        slope = (values_by_depth[right] - values_by_depth[left]) / (right - left)
        if slope > limit:
            raise AssertionError(
                f"{label} slope FAIL between M={left} and M={right}: "
                f"{slope:.9f}s/layer > {limit:.9f}s/layer "
                f"({values_by_depth[left]:.6f}s -> {values_by_depth[right]:.6f}s)"
            )


@contextmanager
def _overlay_writable_root(path: Path) -> Iterator[None]:
    writable_dirs_mod = importlib.import_module("sandbox.overlay.writable_dirs")
    previous = writable_dirs_mod.OVERLAY_WRITABLE_ROOT
    path.mkdir(parents=True, exist_ok=True)
    writable_dirs_mod.OVERLAY_WRITABLE_ROOT = path
    try:
        yield
    finally:
        writable_dirs_mod.OVERLAY_WRITABLE_ROOT = previous
