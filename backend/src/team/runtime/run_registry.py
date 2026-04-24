"""In-process registry mapping ``team_run_id`` → live ``TeamRun``.

Tools that need run-scoped state (notably ``share_briefing``) look up
their owning ``TeamRun`` here using the ``team_run_id`` plumbed onto
``ExecutionMetadata`` by the executor's query-context builder.

The registry is a module-level dict guarded by a ``threading.Lock``.
Single-process by design — distributed coordination would use a
different mechanism entirely. The lock protects against both thread
interleaving and the (currently hypothetical) case of cross-TeamRun
register/unregister races during async context switches.
"""

from __future__ import annotations

import threading
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from team.runtime.team_run import TeamRun


_active: dict[str, "TeamRun"] = {}
_lock = threading.Lock()


def register(team_run: "TeamRun") -> None:
    with _lock:
        _active[team_run.id] = team_run


def unregister(team_run_id: str) -> None:
    with _lock:
        _active.pop(team_run_id, None)


def get(team_run_id: str) -> "TeamRun | None":
    with _lock:
        return _active.get(team_run_id)
