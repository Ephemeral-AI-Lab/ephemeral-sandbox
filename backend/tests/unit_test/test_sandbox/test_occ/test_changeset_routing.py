"""Phase 03 OCC changeset routing tests."""

from __future__ import annotations

from sandbox.occ.changeset.prepared import CommitOptions, RouteDecision
from sandbox.occ.changeset.types import (
    EditChange,
    OpaqueDirChange,
    SymlinkChange,
    WriteChange,
)
from sandbox.occ.routing.orchestrator import OccOrchestrator


class _Gitignore:
    def __init__(self, ignored: set[str] | None = None) -> None:
        self.ignored = ignored or set()
        self.calls: list[str] = []

    def is_ignored(self, path: str) -> bool:
        self.calls.append(path)
        return path in self.ignored


def _prepare(changes, *, ignored: set[str] | None = None):
    router = OccOrchestrator(_Gitignore(ignored))
    return router.prepare_sync(
        changes,
        snapshot=None,
        options=CommitOptions(),
    )


def test_routes_occ_gated_occ_skipped_drop_and_reject_groups() -> None:
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
        ("src/app.py", RouteDecision.GATED),
        ("dist/app.js", RouteDecision.DIRECT),
        (".git/config", RouteDecision.DROP),
        ("../escape", RouteDecision.REJECT),
    ]
    assert prepared.path_groups[-1].message is not None


def test_special_change_kinds_consult_gitignore_before_routing() -> None:
    gitignore = _Gitignore({"cache", "ignored-link"})
    router = OccOrchestrator(gitignore)
    prepared = router.prepare_sync(
        [
            SymlinkChange(path="bin/data.dat", target="/tmp/data"),
            SymlinkChange(path="ignored-link", target="/tmp/data"),
            OpaqueDirChange(path="cache", kept_children=frozenset({"keep"})),
            OpaqueDirChange(path="pkg", kept_children=frozenset({"keep"})),
        ],
        snapshot=None,
        options=CommitOptions(),
    )

    assert [(group.path, group.route) for group in prepared.path_groups] == [
        ("bin/data.dat", RouteDecision.GATED),
        ("ignored-link", RouteDecision.DIRECT),
        ("cache", RouteDecision.DIRECT),
        ("pkg", RouteDecision.GATED),
    ]
    assert gitignore.calls == ["bin/data.dat", "ignored-link", "cache", "pkg"]


def test_same_path_changes_remain_ordered_in_one_group() -> None:
    first = EditChange(path="src/app.py", old_text="a", new_text="b")
    second = EditChange(path="src/app.py", old_text="b", new_text="c")

    prepared = _prepare([first, second])

    [group] = prepared.path_groups
    assert group.route is RouteDecision.GATED
    assert group.changes == (first, second)
