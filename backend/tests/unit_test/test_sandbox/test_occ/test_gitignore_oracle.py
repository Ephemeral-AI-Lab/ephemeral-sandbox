"""Tests for the pathspec-backed gitignore oracle."""

from __future__ import annotations

from pathlib import Path

from sandbox.occ.content.gitignore_oracle import PathspecGitignoreOracle


def _make_workspace(tmp_path: Path, files: dict[str, str]) -> Path:
    root = tmp_path / "repo"
    root.mkdir()
    for rel, body in files.items():
        target = root / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(body, encoding="utf-8")
    return root


def test_is_ignored_caches_result_second_call_reader_free(tmp_path: Path) -> None:
    workspace = _make_workspace(tmp_path, {".gitignore": "*.log\n"})
    calls: list[str] = []

    def reader(dir_rel: str) -> str | None:
        calls.append(dir_rel)
        gitignore = workspace / dir_rel / ".gitignore" if dir_rel else workspace / ".gitignore"
        return gitignore.read_text(encoding="utf-8") if gitignore.is_file() else None

    oracle = PathspecGitignoreOracle(str(workspace), read_gitignore=reader)

    assert oracle.is_ignored("debug.log") is True
    assert calls == [""]
    assert oracle.is_ignored("debug.log") is True
    assert calls == [""]


def test_filter_ignored_returns_subset_of_inputs(tmp_path: Path) -> None:
    workspace = _make_workspace(
        tmp_path,
        {".gitignore": "*.log\n", "pkg/.gitignore": "*.tmp\n"},
    )
    oracle = PathspecGitignoreOracle(str(workspace))

    result = oracle.filter_ignored(
        ["src/main.py", "debug.log", "pkg/cache.tmp", "src/lib.py"],
    )

    assert result == {"debug.log", "pkg/cache.tmp"}


def test_negated_pattern_marks_path_not_ignored(tmp_path: Path) -> None:
    workspace = _make_workspace(
        tmp_path,
        {".gitignore": "build/*\n!build/keep.txt\n"},
    )
    oracle = PathspecGitignoreOracle(str(workspace))

    assert oracle.is_ignored("build/out.o") is True
    assert oracle.is_ignored("build/keep.txt") is False


def test_directory_exclusion_seals_deeper_reinclude(tmp_path: Path) -> None:
    workspace = _make_workspace(
        tmp_path,
        {
            ".gitignore": "secret/\n",
            "secret/.gitignore": "!keep.md\n",
        },
    )
    oracle = PathspecGitignoreOracle(str(workspace))

    assert oracle.is_ignored("secret/private.txt") is True
    assert oracle.is_ignored("secret/keep.md") is True
