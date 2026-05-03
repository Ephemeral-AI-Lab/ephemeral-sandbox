"""Daemon-backed CodeIntelligenceService backend."""

from __future__ import annotations

from sandbox.code_intelligence.daemon.client import DaemonCommandClient
from sandbox.code_intelligence.language_server.daemon_queries import (
    DaemonLanguageServerQueries,
)


class DaemonBackend(DaemonLanguageServerQueries, DaemonCommandClient):
    """Full daemon backend composed from transport and query adapters."""


__all__ = ["DaemonBackend"]
