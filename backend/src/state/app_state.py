"""Minimal application state model."""

from __future__ import annotations

from dataclasses import dataclass, field


@dataclass
class AppState:
    """Shared mutable UI/session state."""

    model: str
    theme: str
    cwd: str = "."
    provider: str = "unknown"
    auth_status: str = "missing"
    base_url: str = ""
    output_style: str = "default"
    keybindings: dict[str, str] = field(default_factory=dict)
    fast_mode: bool = False
    effort: str = "medium"
    passes: int = 1
    mcp_connected: int = 0
    mcp_failed: int = 0
    bridge_sessions: int = 0
