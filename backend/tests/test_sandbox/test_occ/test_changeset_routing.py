"""Phase 03 OCC changeset routing tests."""

from __future__ import annotations

import asyncio

from sandbox.occ.changeset.intent import CommitIntent, RouteDecision
from sandbox.occ.changeset.types import (
    EditChange,
    SymlinkChange,
    WriteChange,
)
from sandbox.occ.orchestrator import OccOrchestrator


class _Gitignore:
    def __init__(self, ignored: set[str] | None = None) -> None:
        self.ignored = ignored or set()
        self.calls: list[str] = []

    def is_ignored(self, path: str) -> bool:
        self.calls.append(path)
        return path in self.ignored


def _prepare(changes, *, ignored: set[str] | None = None):
    router = OccOrchestrator(_Gitignore(ignored))
    return asyncio.run(
        router.prepare(
            changes,
            snapshot=None,
            options=CommitIntent(),
        )
    )


def test_routes_tracked_direct_drop_and_reject_groups() -> None:
    prepared = _prepare(
        [
            WriteChange(path="src/app.py", source="api_write", final_content=b"x"),
            WriteChange(path="dist/app.js", source="api_write", final_content=b"x"),
            WriteChange(path=".git/config", source="api_write", final_content=b"x"),
            WriteChange(path="../escape", source="api_write", final_content=b"x"),
        ],
        ignored={"dist/app.js"},
    )

    assert [(g.path, g.route) for g in prepared.path_groups] == [
        ("src/app.py", RouteDecision.TRACKED),
        ("dist/app.js", RouteDecision.DIRECT),
        (".git/config", RouteDecision.DROP),
        ("../escape", RouteDecision.REJECT),
    ]
    assert prepared.path_groups[-1].message is not None


def test_direct_change_kinds_stay_direct_without_gitignore_lookup() -> None:
    gitignore = _Gitignore()
    router = OccOrchestrator(gitignore)
    prepared = asyncio.run(
        router.prepare(
            [SymlinkChange(path="bin/data.dat", target="/tmp/data")],
            snapshot=None,
            options=CommitIntent(),
        )
    )

    assert prepared.path_groups[0].route is RouteDecision.DIRECT
    assert gitignore.calls == []


def test_same_path_changes_remain_ordered_in_one_group() -> None:
    first = EditChange(path="src/app.py", old_text="a", new_text="b")
    second = EditChange(path="src/app.py", old_text="b", new_text="c")

    prepared = _prepare([first, second])

    [group] = prepared.path_groups
    assert group.route is RouteDecision.TRACKED
    assert group.changes == (first, second)
