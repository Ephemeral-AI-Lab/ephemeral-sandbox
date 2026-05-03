"""Process-local state for the in-sandbox code-intelligence daemon."""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

DAEMON_VERSION = "0.3.0"
STARTED_AT = time.time()


@dataclass
class DaemonState:
    """Process-singleton holding the daemon's CodeIntelligenceService."""

    svc: Any = None
    ledger: Any = None
    index_store: Any = None
    workspace_root: str = ""
    started_at: float = 0.0
    guard_enabled: bool = True
    guard_strict: bool = False
    state_dir: Path | None = None
    test_ops_enabled: bool = False
    extra_dispatch: dict[str, Any] = field(default_factory=dict)


DAEMON_STATE = DaemonState()

QUERY_OPS = frozenset(
    {
        "ping",
        "version",
        "index_ready",
        "query_symbols",
        "find_definitions",
        "find_references",
        "hover",
        "diagnostics",
        "list_folder_files",
        "status",
        "get_telemetry",
    }
)
