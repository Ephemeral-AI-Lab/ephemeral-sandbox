"""Phase 2 guard for the ephemeral tool-primitives corpus fixture."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from sandbox._shared.tool_primitives.edit import edit_file
from sandbox._shared.tool_primitives.glob import glob_files
from sandbox._shared.tool_primitives.grep import grep_files
from sandbox._shared.tool_primitives.read import read_file
from sandbox._shared.tool_primitives.write import write_file

_CORPUS = (
    Path(__file__).resolve().parents[3]
    / "src"
    / "test_runner"
    / "tests"
    / "mock"
    / "sandbox"
    / "_fixtures"
    / "tool_primitives_parity_corpus.json"
)


def _load_cases() -> list[dict[str, str]]:
    payload = json.loads(_CORPUS.read_text(encoding="utf-8"))
    cases = payload["cases"]
    assert payload["schema_version"] == 1
    assert len(cases) >= 40
    assert {case["mode"] for case in cases} == {"ephemeral"}
    assert {case["verb"] for case in cases} == {
        "edit",
        "glob",
        "grep",
        "read",
        "shell",
        "write",
    }
    assert len({case["id"] for case in cases}) == len(cases)
    return cases


def test_tool_primitives_corpus_schema_is_still_ephemeral_only() -> None:
    _load_cases()


@pytest.mark.parametrize(
    "case_id",
    [
        "read.in_workspace_utf8",
        "write.in_workspace_create",
        "edit.in_workspace_single_replace",
        "grep.files_with_matches",
        "glob.basic_pattern",
    ],
)
def test_representative_corpus_cases_replay_against_shared_primitives(
    case_id: str,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    workspace = _seed_workspace(tmp_path)
    monkeypatch.chdir(workspace)

    if case_id == "read.in_workspace_utf8":
        result = read_file({"path": "docs/readme.txt"})
        assert result.content == "hello world\n"
        return
    if case_id == "write.in_workspace_create":
        result = write_file({"path": "created.txt", "content": "created\n"})
        assert result.success is True
        assert (workspace / "created.txt").read_text(encoding="utf-8") == "created\n"
        return
    if case_id == "edit.in_workspace_single_replace":
        result = edit_file(
            {
                "path": "docs/readme.txt",
                "edits": [
                    {
                        "old_text": "hello",
                        "new_text": "hi",
                    }
                ],
            }
        )
        assert result.applied_edits == 1
        assert (workspace / "docs/readme.txt").read_text(encoding="utf-8") == "hi world\n"
        return
    if case_id == "grep.files_with_matches":
        result = grep_files({"path": ".", "pattern": "hello"})
        assert result.filenames == ("docs/readme.txt", "src/app.py", "src/other.py")
        return
    if case_id == "glob.basic_pattern":
        result = glob_files({"path": ".", "pattern": "**/*.py"})
        assert result.filenames == ("src/app.py", "src/other.py")
        return
    raise AssertionError(f"unhandled corpus case: {case_id}")


def _seed_workspace(tmp_path: Path) -> Path:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    files: dict[str, str | bytes] = {
        "docs/readme.txt": "hello world\n",
        "src/app.py": "print('hello')\nprint('bye')\n",
        "src/other.py": "hello again\n",
        "notes.md": "plain notes\n",
        ".git/config": "secret\n",
        "binary.bin": b"\xff\xfe\x00",
    }
    for rel, content in files.items():
        target = workspace.joinpath(*rel.split("/"))
        target.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(content, bytes):
            target.write_bytes(content)
        else:
            target.write_text(content, encoding="utf-8")
    return workspace
