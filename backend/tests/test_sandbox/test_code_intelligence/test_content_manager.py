"""Tests for local/sandbox-aware content reads."""

from __future__ import annotations

import subprocess
import sys
import types
from pathlib import Path
from types import SimpleNamespace

from sandbox.code_intelligence.mutations.content_manager import ContentManager


class _RecordingProcess:
    def __init__(self) -> None:
        self.commands: list[str] = []

    def exec(self, command: str):
        self.commands.append(command)
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


class _FakeDaytonaFs:
    __module__ = "daytona_sdk._sync.filesystem"

    def __init__(self, files: dict[str, bytes]) -> None:
        self.files = files
        self.requests: list[str] = []

    def download_files(self, requests):
        self.requests.extend(request.source for request in requests)
        return [
            SimpleNamespace(source=request.source, result=self.files.get(request.source))
            for request in requests
        ]


def test_read_many_reads_local_files(tmp_path: Path) -> None:
    a = tmp_path / "a.py"
    missing = tmp_path / "missing.py"
    a.write_text("A = 1\n", encoding="utf-8")

    content = ContentManager(str(tmp_path))

    result = content.read_many([str(a), str(missing)], allow_missing=True)

    assert result[str(a)] == ("A = 1\n", True)
    assert result[str(missing)] == ("", False)


def test_relative_local_paths_resolve_under_workspace_root(tmp_path: Path) -> None:
    target = tmp_path / "pkg" / "mod.py"
    target.parent.mkdir()
    target.write_text("VALUE = 1\n", encoding="utf-8")
    content = ContentManager(str(tmp_path))

    assert content.read("pkg/mod.py") == ("VALUE = 1\n", True)

    content.write("pkg/new.py", "CREATED = True\n")
    assert (tmp_path / "pkg" / "new.py").read_text(encoding="utf-8") == ("CREATED = True\n")

    content.delete("pkg/new.py")
    assert not (tmp_path / "pkg" / "new.py").exists()


def test_relative_paths_cannot_escape_workspace_root(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    content = ContentManager(str(workspace))

    for action in (
        lambda: content.read("../outside.py", allow_missing=True),
        lambda: content.write("../outside.py", "ESCAPE = True\n"),
        lambda: content.delete("../outside.py"),
    ):
        try:
            action()
        except ValueError as exc:
            assert "escapes workspace root" in str(exc)
        else:
            raise AssertionError("relative path traversal was not rejected")

    assert not (tmp_path / "outside.py").exists()


def test_read_many_prefers_real_daytona_batch_download(
    tmp_path: Path,
    monkeypatch,
) -> None:
    class FileDownloadRequest:
        def __init__(self, source: str) -> None:
            self.source = source

    common = types.ModuleType("daytona_sdk.common")
    filesystem = types.ModuleType("daytona_sdk.common.filesystem")
    filesystem.FileDownloadRequest = FileDownloadRequest
    monkeypatch.setitem(sys.modules, "daytona_sdk.common", common)
    monkeypatch.setitem(sys.modules, "daytona_sdk.common.filesystem", filesystem)

    a = str(tmp_path / "a.py")
    b = str(tmp_path / "b.py")
    process = _RecordingProcess()
    fs = _FakeDaytonaFs({a: b"A = 1\n", b: b"B = 2\n"})
    sandbox = SimpleNamespace(fs=fs, process=process)
    content = ContentManager(str(tmp_path), sandbox=sandbox)

    result = content.read_many([a, b, a], allow_missing=False)

    assert result[a] == ("A = 1\n", True)
    assert result[b] == ("B = 2\n", True)
    assert fs.requests == [a, b]
    assert process.commands == []


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


def test_read_many_falls_back_to_fs_when_remote_batch_is_not_json(
    tmp_path: Path,
) -> None:
    class _MockProcess:
        def exec(self, command: str):
            return SimpleNamespace(result=f"mock output for: {command}", exit_code=0)

    class _AsyncFs:
        async def download_file(self, path: str) -> bytes:
            if path == str(tmp_path / "a.py"):
                return b"A = 1\n"
            raise FileNotFoundError(path)

    sandbox = SimpleNamespace(fs=_AsyncFs(), process=_MockProcess())
    content = ContentManager(str(tmp_path), sandbox=sandbox)

    result = content.read_many([str(tmp_path / "a.py")], allow_missing=False)

    assert result[str(tmp_path / "a.py")] == ("A = 1\n", True)


def test_read_falls_back_to_fs_when_process_read_is_not_json(tmp_path: Path) -> None:
    class _MockProcess:
        def exec(self, command: str):
            return SimpleNamespace(result=f"mock output for: {command}", exit_code=0)

    class _AsyncFs:
        async def download_file(self, path: str) -> bytes:
            if path == str(tmp_path / "a.py"):
                return b"A = 1\n"
            raise FileNotFoundError(path)

    sandbox = SimpleNamespace(fs=_AsyncFs(), process=_MockProcess())
    content = ContentManager(str(tmp_path), sandbox=sandbox)

    assert content.read(str(tmp_path / "a.py")) == ("A = 1\n", True)


def test_fs_only_sandbox_accepts_async_fs_methods(tmp_path: Path) -> None:
    class _AsyncFs:
        def __init__(self) -> None:
            self.files: dict[str, bytes] = {}

        async def download_file(self, path: str) -> bytes:
            return self.files[path]

        async def upload_file(self, content: bytes, path: str) -> None:
            self.files[path] = content

        async def delete_file(self, path: str) -> None:
            self.files.pop(path, None)

    fs = _AsyncFs()
    sandbox = SimpleNamespace(fs=fs, process=None)
    manager = ContentManager(str(tmp_path), sandbox=sandbox)
    target = str(tmp_path / "async.py")

    manager.write(target, "value = 1\n")
    assert manager.read(target) == ("value = 1\n", True)

    manager.delete(target)
    assert target not in fs.files


def test_write_remote_chunks_large_file_payload(tmp_path: Path) -> None:
    target = tmp_path / "large.py"
    content = "".join(f"line_{index} = {index}\n" for index in range(5000))
    process = _RecordingProcess()
    sandbox = SimpleNamespace(process=process)
    manager = ContentManager(str(tmp_path), sandbox=sandbox)

    manager.write(str(target), content)

    assert target.read_text(encoding="utf-8") == content
    assert len(process.commands) > 2


def test_apply_many_stages_payload_for_large_files(tmp_path: Path) -> None:
    """Large batch payloads must stage through a tmp file, not inline argv.

    Inlining base64 of large files into ``python3 -c`` overflows ARG_MAX and
    surfaces as the bare-string ``"checked batch apply failed"`` from the
    sandbox; verify the staged path produces correct results.
    """
    big_content = "".join(f"line_{i} = {i}\n" for i in range(5000))
    target_a = tmp_path / "big_a.py"
    target_b = tmp_path / "big_b.py"
    process = _RecordingProcess()
    sandbox = SimpleNamespace(process=process)
    manager = ContentManager(str(tmp_path), sandbox=sandbox)

    manager.apply_many([(str(target_a), big_content), (str(target_b), big_content)])

    assert target_a.read_text(encoding="utf-8") == big_content
    assert target_b.read_text(encoding="utf-8") == big_content
    # Staging must produce more than the single inline-script invocation.
    assert len(process.commands) > 3
    # No tmp staging file should leak in /tmp.
    leftover = list(Path("/tmp").glob("codex-batch-apply-*.json"))
    assert leftover == []


def test_apply_many_with_base_check_stages_large_payloads(tmp_path: Path) -> None:
    from sandbox.code_intelligence.core.hashing import content_hash
    from sandbox.code_intelligence.mutations.content_manager import CheckedApplyChange

    big_content = "".join(f"line_{i} = {i}\n" for i in range(5000))
    target = tmp_path / "groupby.py"
    target.write_text("seed\n", encoding="utf-8")
    base_hash = content_hash("seed\n")

    process = _RecordingProcess()
    sandbox = SimpleNamespace(process=process)
    manager = ContentManager(str(tmp_path), sandbox=sandbox)

    result = manager.apply_many_with_base_check(
        [
            CheckedApplyChange(
                file_path=str(target),
                base_hash=base_hash,
                base_existed=True,
                final_content=big_content,
            ),
        ],
    )

    assert result.success, (result.conflict_reason, result.message)
    assert target.read_text(encoding="utf-8") == big_content
    leftover = list(Path("/tmp").glob("codex-batch-apply-*.json"))
    assert leftover == []
