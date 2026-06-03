"""Search primitive and thin daemon-handler tests."""

from __future__ import annotations

import re
from pathlib import Path

import pytest

from sandbox._shared.tool_primitives.glob import DEFAULT_GLOB_LIMIT
from sandbox._shared.tool_primitives.glob import glob_files
from sandbox._shared.tool_primitives.grep import grep_files


def _seed_workspace(tmp_path: Path, *, files: dict[str, str]) -> Path:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    for rel, content in files.items():
        target = workspace.joinpath(*rel.split("/"))
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")
    return workspace


def test_glob_basic_pattern_returns_sorted_matches(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    workspace = _seed_workspace(
        tmp_path,
        files={
            "a.py": "alpha",
            "pkg/b.py": "beta",
            "pkg/c.txt": "ctext",
            "notes.md": "n",
        },
    )
    monkeypatch.chdir(workspace)

    result = glob_files({"pattern": "**/*.py", "path": "."})

    assert result.success is True
    assert result.filenames == ("a.py", "pkg/b.py")
    assert result.num_files == 2
    assert result.truncated is False


def test_glob_excludes_vcs_directories(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    workspace = _seed_workspace(
        tmp_path,
        files={
            ".git/config": "x",
            ".git/HEAD": "y",
            "src/main.py": "z",
        },
    )
    monkeypatch.chdir(workspace)

    result = glob_files({"pattern": "**/*", "path": "."})

    assert "src/main.py" in result.filenames
    assert all(not name.startswith(".git/") for name in result.filenames)


def test_glob_truncates_at_one_hundred(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    workspace = _seed_workspace(
        tmp_path,
        files={f"item_{i:03d}.txt": str(i) for i in range(105)},
    )
    monkeypatch.chdir(workspace)

    result = glob_files({"pattern": "item_*.txt", "path": "."})

    assert result.num_files == DEFAULT_GLOB_LIMIT
    assert len(result.filenames) == DEFAULT_GLOB_LIMIT
    assert result.truncated is True


def test_glob_missing_pattern_raises(tmp_path: Path) -> None:
    _seed_workspace(tmp_path, files={"a.txt": "a"})

    with pytest.raises(ValueError, match="pattern is required"):
        glob_files({"path": "."})


def test_grep_missing_pattern_raises(tmp_path: Path) -> None:
    _seed_workspace(tmp_path, files={"a.txt": "a"})

    with pytest.raises(ValueError, match="pattern is required"):
        grep_files({"path": "."})


def test_grep_files_with_matches_mode(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    workspace = _seed_workspace(
        tmp_path,
        files={
            "a.py": "hello world\n",
            "b.py": "goodbye\n",
            "c.txt": "hello again\n",
        },
    )
    monkeypatch.chdir(workspace)

    result = grep_files({"pattern": "hello", "path": "."})

    assert result.success is True
    assert result.output_mode == "files_with_matches"
    assert result.filenames == ("a.py", "c.txt")
    assert result.num_files == 2


def test_grep_accepts_single_file_path(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    workspace = _seed_workspace(
        tmp_path,
        files={"conftest.py": "# fixtures\n", "other.py": "# fixtures\n"},
    )
    monkeypatch.chdir(workspace)

    result = grep_files(
        {
            "pattern": re.escape("# fixtures\n"),
            "path": (workspace / "conftest.py").as_posix(),
            "multiline": True,
        }
    )

    assert result.success is True
    assert result.filenames == ("conftest.py",)
    assert result.num_matches == 1


def test_grep_count_mode_returns_per_file_counts(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    workspace = _seed_workspace(
        tmp_path,
        files={
            "a.py": "hello hello world\n",
            "b.py": "hello\nnope\nhello\n",
            "c.py": "miss\n",
        },
    )
    monkeypatch.chdir(workspace)

    result = grep_files({"pattern": "hello", "path": ".", "output_mode": "count"})

    assert result.output_mode == "count"
    assert result.filenames == ("a.py", "b.py")
    assert result.num_matches == 4
    assert dict(line.split(":", 1) for line in result.content.splitlines()) == {
        "a.py": "2",
        "b.py": "2",
    }


def test_grep_content_mode_emits_filename_line_pairs(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    workspace = _seed_workspace(
        tmp_path,
        files={"a.py": "alpha\nhello world\nbeta\n"},
    )
    monkeypatch.chdir(workspace)

    result = grep_files(
        {
            "pattern": "hello",
            "path": ".",
            "output_mode": "content",
            "line_numbers": True,
        }
    )

    assert result.output_mode == "content"
    assert "a.py:2:hello world\n" in result.content
    assert result.num_lines == 1
    assert result.num_matches == 1


def test_grep_case_insensitive_and_glob_filter(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    workspace = _seed_workspace(
        tmp_path,
        files={
            "a.py": "Hello there\n",
            "b.txt": "hello text\n",
            "c.py": "miss\n",
        },
    )
    monkeypatch.chdir(workspace)

    result = grep_files(
        {
            "pattern": "hello",
            "path": ".",
            "case_insensitive": True,
            "glob_filter": "*.py",
        }
    )

    assert result.filenames == ("a.py",)


def test_search_handlers_do_not_call_occ_client() -> None:
    """Read-only by construction: ``grep`` and ``glob`` must not touch OCC.

    Search operations are registered through ``WORKSPACE_TOOL_HANDLERS``;
    check the shared handler source rather than the module file (the module
    also hosts OCC-touching built-ins like ``layer_metrics``).
    """
    import inspect

    from sandbox.daemon import builtin_operations

    for verb in ("grep", "glob"):
        fn = builtin_operations.WORKSPACE_TOOL_HANDLERS[verb]
        code = inspect.getsource(fn)
        name = f"builtin_operations.WORKSPACE_TOOL_HANDLERS[{verb!r}]"
        assert "occ_client." not in code, f"{name} must not access occ_client"
        assert "OccClient" not in code, f"{name} must not reference OccClient"
        assert ".commit_" not in code, f"{name} must not call commit_* methods"
        assert ".apply_" not in code, f"{name} must not call apply_* methods"
