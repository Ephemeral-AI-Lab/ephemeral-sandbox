"""Central tuning knobs for code-intelligence runtime behavior."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class CodeIntelligenceTuning:
    scope_recent_seconds: float = 300.0
    shell_default_timeout: int = 900
    shell_write_timeout: int = 5


CODE_INTELLIGENCE_TUNING = CodeIntelligenceTuning()


__all__ = [
    "CODE_INTELLIGENCE_TUNING",
    "CodeIntelligenceTuning",
]
