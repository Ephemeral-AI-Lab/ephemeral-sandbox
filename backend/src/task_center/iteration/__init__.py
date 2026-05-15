"""Episode package facade.

Episode DTOs/enums live in :mod:`task_center.episode.state`; lifecycle
coordination lives in :mod:`task_center.episode.manager`.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from task_center.episode.state import (
    AttemptedPlanEntry,
    AttemptPlanFailed,
    ClosureOutcome,
    Episode,
    EpisodeClosureReport,
    EpisodeCreationReason,
    EpisodeStatus,
    SuccessContinue,
    TerminalSuccess,
)

if TYPE_CHECKING:
    from task_center.episode.manager import (
        AttemptClosedCallback,
        ClosureReportSink,
        EpisodeManager,
        EpisodeManagerRegistry,
        OrchestratorFactory,
    )

_MANAGER_EXPORTS: dict[str, tuple[str, str]] = {
    "AttemptClosedCallback": (
        "task_center.episode.manager",
        "AttemptClosedCallback",
    ),
    "ClosureReportSink": (
        "task_center.episode.manager",
        "ClosureReportSink",
    ),
    "EpisodeManager": ("task_center.episode.manager", "EpisodeManager"),
    "EpisodeManagerRegistry": (
        "task_center.episode.manager",
        "EpisodeManagerRegistry",
    ),
    "OrchestratorFactory": (
        "task_center.episode.manager",
        "OrchestratorFactory",
    ),
}

_STATE_EXPORTS = [
    "AttemptPlanFailed",
    "AttemptedPlanEntry",
    "ClosureOutcome",
    "Episode",
    "EpisodeClosureReport",
    "EpisodeCreationReason",
    "EpisodeStatus",
    "SuccessContinue",
    "TerminalSuccess",
]


def __getattr__(name: str) -> object:
    target = _MANAGER_EXPORTS.get(name)
    if target is None:
        raise AttributeError(
            f"module 'task_center.episode' has no attribute {name!r}"
        )
    module_path, attr = target
    import importlib

    module = importlib.import_module(module_path)
    value = getattr(module, attr)
    globals()[name] = value
    return value


__all__ = [
    "AttemptClosedCallback",
    "AttemptPlanFailed",
    "AttemptedPlanEntry",
    "ClosureOutcome",
    "ClosureReportSink",
    "Episode",
    "EpisodeClosureReport",
    "EpisodeCreationReason",
    "EpisodeManager",
    "EpisodeManagerRegistry",
    "EpisodeStatus",
    "OrchestratorFactory",
    "SuccessContinue",
    "TerminalSuccess",
]
