"""Parity matrix: the pathspec oracle matches ``git check-ignore`` fixtures."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from sandbox.occ.content.gitignore_oracle import (
    PathspecGitignoreOracle,
)


def _have_git() -> bool:
    return shutil.which("git") is not None


pytestmark = pytest.mark.skipif(not _have_git(), reason="git binary not on PATH")


def _init_git(workspace: Path) -> None:
    subprocess.run(
        ["git", "-C", str(workspace), "init", "-q"],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def _git_is_ignored(workspace: Path, path: str) -> bool:
    completed = subprocess.run(
        ["git", "-C", str(workspace), "check-ignore", "-q", path],
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    if completed.returncode == 0:
        return True
    if completed.returncode == 1:
        return False
    stderr = completed.stderr.decode("utf-8", "replace")
    raise RuntimeError(f"git check-ignore failed: {stderr!r}")


def _make_workspace(tmp_path: Path, files: dict[str, str]) -> Path:
    root = tmp_path / "repo"
    root.mkdir()
    for rel, body in files.items():
        target = root / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(body, encoding="utf-8")
    _init_git(root)
    return root


_PARITY_CASES: list[tuple[str, dict[str, str], list[str]]] = [
    (
        "nested-reinclude-and-character-classes",
        {
            ".gitignore": ("build/*\n!build/keep.txt\nlogs/[Ee]rror.[Ll][Oo][Gg]\n"),
            "pkg/.gitignore": "*.tmp\n!important.tmp\n",
        },
        [
            "build/out.o",
            "build/keep.txt",
            "build/sub/keep.txt",
            "pkg/cache.tmp",
            "pkg/important.tmp",
            "pkg/nested/x.tmp",
            "logs/error.log",
            "logs/Error.LOG",
            "logs/info.log",
            "src/main.py",
        ],
    ),
    (
        "anchored-vs-unanchored",
        {
            ".gitignore": "/dist\n*.bak\n",
        },
        ["dist", "dist/x", "sub/dist", "foo.bak", "sub/foo.bak"],
    ),
    (
        "deep-reinclude-overrides-parent",
        {
            ".gitignore": "secret/\n",
            "secret/.gitignore": "!keep.md\n",
        },
        ["secret/private.txt", "secret/keep.md"],
    ),
]


@pytest.mark.parametrize(
    "label,files,paths",
    _PARITY_CASES,
    ids=[c[0] for c in _PARITY_CASES],
)
def test_pathspec_matches_git_check_ignore(
    tmp_path: Path,
    label: str,
    files: dict[str, str],
    paths: list[str],
) -> None:
    workspace = _make_workspace(tmp_path, files)
    pathspec_oracle = PathspecGitignoreOracle(str(workspace))

    for p in paths:
        git_verdict = _git_is_ignored(workspace, p)
        pathspec_verdict = pathspec_oracle.is_ignored(p)
        assert pathspec_verdict == git_verdict, (
            f"divergence on {p!r} in case {label}: git={git_verdict}, pathspec={pathspec_verdict}"
        )


def test_pathspec_filter_ignored_is_subset_of_inputs(tmp_path: Path) -> None:
    workspace = _make_workspace(
        tmp_path,
        {".gitignore": "*.log\n", "pkg/.gitignore": "*.tmp\n"},
    )
    oracle = PathspecGitignoreOracle(str(workspace))
    paths = ["a.log", "pkg/x.tmp", "src/keep.py"]
    ignored = oracle.filter_ignored(paths)
    assert ignored == {"a.log", "pkg/x.tmp"}


def test_pathspec_caches_per_path_lookup(tmp_path: Path) -> None:
    workspace = _make_workspace(
        tmp_path,
        {".gitignore": "*.log\n"},
    )
    calls: list[str] = []

    def reader(dir_rel: str) -> str | None:
        calls.append(dir_rel)
        gi = (
            (Path(workspace) / dir_rel / ".gitignore")
            if dir_rel
            else (Path(workspace) / ".gitignore")
        )
        return gi.read_text(encoding="utf-8") if gi.is_file() else None

    oracle = PathspecGitignoreOracle(str(workspace), read_gitignore=reader)
    assert oracle.is_ignored("a.log") is True
    assert oracle.is_ignored("a.log") is True  # cached path
    # One read per ancestor dir on first call: only "" (root) for "a.log".
    # Second call must hit the per-path cache, no further reads.
    assert calls == [""]


def test_pathspec_via_callback_matches_disk_backend(tmp_path: Path) -> None:
    workspace = _make_workspace(
        tmp_path,
        {".gitignore": "*.log\n!keep.log\n", "pkg/.gitignore": "*.tmp\n"},
    )
    disk = PathspecGitignoreOracle(str(workspace))

    def reader(dir_rel: str) -> str | None:
        gi = (
            (Path(workspace) / dir_rel / ".gitignore")
            if dir_rel
            else (Path(workspace) / ".gitignore")
        )
        return gi.read_text(encoding="utf-8") if gi.is_file() else None

    callback = PathspecGitignoreOracle("", read_gitignore=reader)

    for p in ["a.log", "keep.log", "pkg/x.tmp", "src/main.py"]:
        assert disk.is_ignored(p) == callback.is_ignored(p), p
