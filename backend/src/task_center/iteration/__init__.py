"""Iteration package facade.

Iteration DTOs/enums live in :mod:`task_center._core.state`; lifecycle
coordination lives in :mod:`task_center.iteration.attempt_coordinator`.

The facade re-exports only the names that callers actually reach through this
path. Internal callback aliases (``AttemptClosedCallback``,
``IterationClosedCallback``) and the iteration DTOs/enums
(``IterationCreationReason``, ``IterationStatus``) live on the canonical
``_core.state`` / ``.attempt_coordinator`` modules; import them from there.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from task_center.iteration.attempt_coordinator import (
        IterationAttemptCoordinator as IterationAttemptCoordinator,
        OpenIterationCoordinatorRegistry as OpenIterationCoordinatorRegistry,
        OrchestratorFactory as OrchestratorFactory,
    )

_COORDINATORS = "task_center.iteration.attempt_coordinator"

_EXPORTS: dict[str, tuple[str, str]] = {
    "IterationAttemptCoordinator": (
        _COORDINATORS,
        "IterationAttemptCoordinator",
    ),
    "OpenIterationCoordinatorRegistry": (
        _COORDINATORS,
        "OpenIterationCoordinatorRegistry",
    ),
    "OrchestratorFactory": (
        _COORDINATORS,
        "OrchestratorFactory",
    ),
}


def __getattr__(name: str) -> object:
    try:
        module_path, attr = _EXPORTS[name]
    except KeyError as exc:
        raise AttributeError(f"module 'task_center.iteration' has no attribute {name!r}") from exc
    import importlib

    module = importlib.import_module(module_path)
    value = getattr(module, attr)
    globals()[name] = value
    return value


__all__ = sorted(_EXPORTS)
