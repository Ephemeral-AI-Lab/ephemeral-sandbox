"""Namespace setup for per-call snapshot overlay execution."""

from __future__ import annotations

from sandbox.overlay.namespace.command import CommandResult, run_user_command
from sandbox.overlay.namespace.mounts import MountedSnapshot, mount_snapshot

__all__ = [
    "CommandResult",
    "MountedSnapshot",
    "mount_snapshot",
    "run_user_command",
]
