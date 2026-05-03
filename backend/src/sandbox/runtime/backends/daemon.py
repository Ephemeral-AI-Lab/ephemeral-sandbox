"""Daemon-backed CodeIntelligenceService backend."""

from __future__ import annotations

from sandbox.runtime.legacy_command_client import DaemonCommandClient


class DaemonBackend(DaemonCommandClient):
    """Transport-backed backend for daemon-owned mutation and status commands."""


__all__ = ["DaemonBackend"]
