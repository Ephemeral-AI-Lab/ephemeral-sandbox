"""Pathspec-backed ``.gitignore`` oracle for OCC route decisions.

Production gitignore routing uses the pure-Python ``pathspec`` evaluator only.
The snapshot oracle reads ``.gitignore`` files directly from the active
layer-stack manifest, so it does not materialize a temporary git workspace and
does not shell out to ``git check-ignore``.
"""

from __future__ import annotations

import importlib
from collections.abc import Callable, Iterable
from pathlib import Path
from typing import TYPE_CHECKING, Any, Protocol, runtime_checkable

from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.ports import SnapshotReader

if TYPE_CHECKING:  # pragma: no cover - import only for type-checkers
    import pathspec  # noqa: F401


_pathspec_module: Any | None = None


def _load_pathspec() -> Any:
    """Lazy-import ``pathspec`` so importing the runtime module stays cheap."""
    global _pathspec_module
    if _pathspec_module is None:
        _pathspec_module = importlib.import_module("pathspec")
    return _pathspec_module


ReadGitignoreFn = Callable[[str], str | None]


class GitignoreMatcher(Protocol):
    """Small contract consumed by OCC routing."""

    def is_ignored(self, path: str) -> bool: ...


@runtime_checkable
class SnapshotGitignoreMatcher(GitignoreMatcher, Protocol):
    """Gitignore contract for routing against a known layer-stack snapshot."""

    def is_ignored_in_snapshot(self, path: str, snapshot: Manifest) -> bool: ...


@runtime_checkable
class GitignoreCacheStats(Protocol):
    """Optional gitignore cache counters used by result formatting."""

    cache_hits: int
    cache_misses: int


class PathspecGitignoreOracle:
    """Pure-Python gitignore evaluator backed by the ``pathspec`` library.

    Honours the standard nested-``.gitignore`` semantics: a ``.gitignore`` at
    directory ``D`` applies only to paths under ``D``, and a deeper match
    overrides a shallower one. Within one ``.gitignore``, ``!`` re-includes
    work via ``GitIgnoreSpec.check_file``.

    ``read_gitignore(dir_rel)`` returns the contents of the ``.gitignore``
    inside ``dir_rel`` (``""`` for the workspace root) or ``None`` if absent.
    When omitted, the oracle reads from ``<workspace_root>/<dir_rel>/.gitignore``
    on the filesystem.

    Note: this backend is case-sensitive. Git on case-folding filesystems may
    accept ``error.log`` for a pattern of ``Error.log``; that mode is rare in
    sandbox workloads and out of scope for the parity guarantees here.
    """

    def __init__(
        self,
        workspace_root: str,
        *,
        read_gitignore: ReadGitignoreFn | None = None,
    ) -> None:
        self._workspace_root = str(workspace_root or "")
        self._read = read_gitignore or self._read_from_disk
        self._path_cache: dict[str, bool] = {}
        self._dir_cache: dict[str, bool] = {}
        self._spec_cache: dict[str, Any | None] = {}
        self._pathspec = _load_pathspec()

    def is_ignored(self, path: str) -> bool:
        if path in self._path_cache:
            return self._path_cache[path]
        result = self._evaluate_file(path)
        self._path_cache[path] = result
        return result

    def filter_ignored(self, paths: Iterable[str]) -> set[str]:
        unique_paths = list(dict.fromkeys(paths))
        return {p for p in unique_paths if self.is_ignored(p)}

    def _evaluate_file(self, path: str) -> bool:
        rel = path.lstrip("/")
        if not rel:
            return False
        parts = rel.split("/")
        # Git's directory-exclusion seal: if any ancestor directory of *path*
        # is excluded, no deeper ``!`` re-include can rescue contents under
        # that directory. Test each ancestor (root to path.parent) first.
        accum = ""
        for depth in range(len(parts) - 1):
            accum = f"{accum}/{parts[depth]}" if accum else parts[depth]
            if self._is_dir_excluded(accum):
                return True
        return self._match_with_inheritance(rel, as_directory=False)

    def _is_dir_excluded(self, dir_rel: str) -> bool:
        if dir_rel in self._dir_cache:
            return self._dir_cache[dir_rel]
        parts = [part for part in dir_rel.split("/") if part]
        accum = ""
        excluded = False
        for part in parts:
            accum = f"{accum}/{part}" if accum else part
            cached = self._dir_cache.get(accum)
            if cached is not None:
                excluded = cached
                continue
            if not excluded:
                excluded = self._match_with_inheritance(accum, as_directory=True)
            self._dir_cache[accum] = excluded
        return self._dir_cache.get(dir_rel, excluded)

    def _match_with_inheritance(self, path: str, *, as_directory: bool) -> bool:
        """Last-match-wins evaluation across every ``.gitignore`` above *path*.

        ``as_directory`` appends a trailing slash so directory-only patterns
        (``foo/``) take effect. Caller is responsible for the directory-seal
        early-exit; this is the unsealed evaluator.
        """
        parts = path.split("/")
        target = path + "/" if as_directory else path
        ignored = False
        accum = ""
        for depth in range(len(parts)):
            spec = self._spec_for_dir(accum)
            if spec is not None:
                sub = target[len(accum) :].lstrip("/") if accum else target
                if sub:
                    outcome = spec.check_file(sub)
                    if outcome.include is True:
                        ignored = True
                    elif outcome.include is False:
                        ignored = False
            accum = f"{accum}/{parts[depth]}" if accum else parts[depth]
        return ignored

    def _spec_for_dir(self, dir_rel: str) -> Any | None:
        if dir_rel in self._spec_cache:
            return self._spec_cache[dir_rel]
        content = self._read(dir_rel)
        spec: Any | None = None
        if content:
            spec = self._pathspec.GitIgnoreSpec.from_lines(content.splitlines())
        self._spec_cache[dir_rel] = spec
        return spec

    def _read_from_disk(self, dir_rel: str) -> str | None:
        root = Path(self._workspace_root) if self._workspace_root else Path()
        gitignore = (root / dir_rel / ".gitignore") if dir_rel else (root / ".gitignore")
        try:
            if gitignore.is_file():
                return gitignore.read_text(encoding="utf-8")
        except OSError:
            return None
        return None


class SnapshotGitignoreOracle:
    """Evaluate gitignore rules directly from a layer-stack snapshot.

    The oracle reads ``.gitignore`` files through ``SnapshotReader.read_text``.
    It keeps one pathspec evaluator per manifest version so repeated lookups in
    the same runtime process reuse parsed ``.gitignore`` specs and path verdicts.
    """

    def __init__(self, snapshot_reader: SnapshotReader) -> None:
        self._snapshot_reader = snapshot_reader
        self._oracles: dict[int, PathspecGitignoreOracle] = {}
        self.cache_hits: int = 0
        self.cache_misses: int = 0

    def is_ignored(self, path: str) -> bool:
        return self.is_ignored_in_snapshot(
            path,
            self._snapshot_reader.read_active_manifest(),
        )

    def filter_ignored(self, paths: Iterable[str]) -> set[str]:
        snapshot = self._snapshot_reader.read_active_manifest()
        return {path for path in paths if self.is_ignored_in_snapshot(path, snapshot)}

    def is_ignored_in_snapshot(self, path: str, snapshot: Manifest) -> bool:
        return self._oracle_for_snapshot(snapshot).is_ignored(path)

    def _oracle_for_snapshot(self, snapshot: Manifest) -> PathspecGitignoreOracle:
        version = snapshot.version
        cached = self._oracles.get(version)
        if cached is not None:
            self.cache_hits += 1
            return cached

        self.cache_misses += 1
        oracle = self._build_pathspec_oracle(snapshot)
        self._oracles[version] = oracle
        return oracle

    def _build_pathspec_oracle(self, snapshot: Manifest) -> PathspecGitignoreOracle:
        def _read_gitignore(dir_rel: str) -> str | None:
            rel = f"{dir_rel}/.gitignore" if dir_rel else ".gitignore"
            content, exists = self._snapshot_reader.read_text(rel, snapshot)
            return content if exists else None

        return PathspecGitignoreOracle(
            workspace_root="",
            read_gitignore=_read_gitignore,
        )


__all__ = [
    "GitignoreMatcher",
    "GitignoreCacheStats",
    "PathspecGitignoreOracle",
    "ReadGitignoreFn",
    "SnapshotGitignoreMatcher",
    "SnapshotGitignoreOracle",
]
