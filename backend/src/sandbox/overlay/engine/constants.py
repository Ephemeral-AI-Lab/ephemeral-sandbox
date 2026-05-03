"""Shared constants for overlay execution."""

from __future__ import annotations

RUN_DIR_PREFIX = "/tmp/eos-shell-overlay"
PROGRESS_POLL_INTERVAL_SECONDS = 2.0
PROGRESS_READ_CHUNK_BYTES = 64 * 1024
SLOW_OVERLAY_STAGE_SECONDS = 1.0
SLOW_OVERLAY_TOTAL_SECONDS = 5.0
COMMAND_SAMPLE_LIMIT = 160

WorkspaceFingerprint = tuple[tuple[str, int, int, int, int], ...]


__all__ = [
    "COMMAND_SAMPLE_LIMIT",
    "PROGRESS_POLL_INTERVAL_SECONDS",
    "PROGRESS_READ_CHUNK_BYTES",
    "RUN_DIR_PREFIX",
    "SLOW_OVERLAY_STAGE_SECONDS",
    "SLOW_OVERLAY_TOTAL_SECONDS",
    "WorkspaceFingerprint",
]
