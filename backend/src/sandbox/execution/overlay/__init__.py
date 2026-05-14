"""Policy-blind snapshot overlay runtime package."""

from sandbox.execution.overlay.capture import capture_changes
from sandbox.execution.overlay.change import (
    OverlayPathChange,
    OverlayPathChangeKind,
    content_hash,
)
from sandbox.execution.overlay.pipeline import OverlayCommandResult, run_user_command
from sandbox.execution.overlay.pipeline import create_overlay_invoker
from sandbox.execution.overlay.pipeline import OverlayInvoker, OverlayRuntimeInvoker
from sandbox.execution.overlay.mounts import (
    OverlayMountedSnapshot,
    cleanup_runtime_run_dir,
    mount_snapshot,
)
from sandbox.execution.overlay.request import OverlayShellRequest
from sandbox.execution.overlay.result import OverlayCapture, read_output_ref, write_overlay_capture
from sandbox.execution.overlay.runner import OverlaySnapshotRunner

__all__ = [
    "OverlayCapture",
    "OverlayCommandResult",
    "OverlayInvoker",
    "OverlayMountedSnapshot",
    "OverlayPathChange",
    "OverlayPathChangeKind",
    "OverlayRuntimeInvoker",
    "OverlayShellRequest",
    "OverlaySnapshotRunner",
    "capture_changes",
    "cleanup_runtime_run_dir",
    "content_hash",
    "create_overlay_invoker",
    "mount_snapshot",
    "read_output_ref",
    "run_user_command",
    "write_overlay_capture",
]
