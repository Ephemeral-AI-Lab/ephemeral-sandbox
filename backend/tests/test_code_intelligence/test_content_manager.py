"""Tests for local/sandbox-aware content reads."""

from __future__ import annotations

import subprocess
from pathlib import Path
from types import SimpleNamespace

from code_intelligence.routing.content_manager import ContentManager


class _RecordingProcess:
    def __init__(self, *, fail_first: bool = False) -> None:
        self.commands: list[str] = []
        self._fail_first = fail_first

    def exec(self, command: str):
        self.commands.append(command)
        if self._fail_first:
            self._fail_first = False
            return SimpleNamespace(result="batch unavailable", exit_code=1)
        proc = subprocess.run(
            command,
            shell=True,
            capture_output=True,
            text=True,
            timeout=10,
        )
        return SimpleNamespace(
            result=(proc.stdout or "") + (proc.stderr or ""),
            exit_code=proc.returncode,
        )


def test_read_many_reads_local_files(tmp_path: Path) -> None:
    a = tmp_path / "a.py"
    missing = tmp_path / "missing.py"
    a.write_text("A = 1\n", encoding="utf-8")

    content = ContentManager(str(tmp_path))

    result = content.read_many([str(a), str(missing)], allow_missing=True)

    assert result[str(a)] == ("A = 1\n", True)
    assert result[str(missing)] == ("", False)


def test_read_many_batches_remote_exec(tmp_path: Path) -> None:
    a = tmp_path / "a.py"
    b = tmp_path / "b.py"
    a.write_text("A = 1\n", encoding="utf-8")
    b.write_text("B = 2\n", encoding="utf-8")

    process = _RecordingProcess()
    sandbox = SimpleNamespace(process=process)
    content = ContentManager(str(tmp_path), sandbox=sandbox)

    result = content.read_many([str(a), str(b), str(a)], allow_missing=False)

    assert result[str(a)] == ("A = 1\n", True)
    assert result[str(b)] == ("B = 2\n", True)
    assert len(process.commands) == 1


def test_read_many_allows_missing_remote_files(tmp_path: Path) -> None:
    a = tmp_path / "a.py"
    missing = tmp_path / "missing.py"
    a.write_text("A = 1\n", encoding="utf-8")

    sandbox = SimpleNamespace(process=_RecordingProcess())
    content = ContentManager(str(tmp_path), sandbox=sandbox)

    result = content.read_many([str(a), str(missing)], allow_missing=True)

    assert result[str(a)] == ("A = 1\n", True)
    assert result[str(missing)] == ("", False)


def test_read_many_falls_back_when_remote_batch_fails(tmp_path: Path) -> None:
    a = tmp_path / "a.py"
    b = tmp_path / "b.py"
    a.write_text("A = 1\n", encoding="utf-8")
    b.write_text("B = 2\n", encoding="utf-8")

    process = _RecordingProcess(fail_first=True)
    sandbox = SimpleNamespace(process=process)
    content = ContentManager(str(tmp_path), sandbox=sandbox)

    result = content.read_many([str(a), str(b)], allow_missing=True)

    assert result[str(a)] == ("A = 1\n", True)
    assert result[str(b)] == ("B = 2\n", True)
    assert len(process.commands) == 3
