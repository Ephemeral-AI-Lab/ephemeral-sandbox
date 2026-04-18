"""Generic in-process tool-guard (prehook) system.

Parallel to :mod:`hooks` (subprocess-based Command/Prompt/HTTP/Agent hooks),
this package hosts host-side Python guards that run inside
``run_tool_safely``. Guards register against tool-name globs and a phase
(``pre`` or ``post``), and return one of four outcomes: ``Allow``,
``Deny``, ``MutateArgs``, ``Advisory``.

See ``.ephemeralos/prompt-reports/tool-guards-plan.md`` for the full
design and migration plan.
"""

from __future__ import annotations

from tools.core.guards.pipeline import PreResult, PostResult, run_post, run_pre
from tools.core.guards.registry import GuardEntry, Phase, ToolGuardRegistry, default_registry
from tools.core.guards.types import (
    Advisory,
    Allow,
    Deny,
    GuardOutcome,
    MutateArgs,
    PostToolGuard,
    PreToolGuard,
)

__all__ = [
    "Advisory",
    "Allow",
    "Deny",
    "GuardEntry",
    "GuardOutcome",
    "MutateArgs",
    "Phase",
    "PostResult",
    "PostToolGuard",
    "PreResult",
    "PreToolGuard",
    "ToolGuardRegistry",
    "default_registry",
    "run_post",
    "run_pre",
]
