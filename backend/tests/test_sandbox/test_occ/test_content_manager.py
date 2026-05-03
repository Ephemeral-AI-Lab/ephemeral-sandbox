"""Tests for local/transport-aware content reads."""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

from sandbox.occ.content.hashing import content_hash
from sandbox.occ.content.manager import (
    CheckedApplyChange,
    ContentManager,
)


class _UnexpectedProcess:
    def __init__(self) -> None:
        self.calls = 0

    def exec(self, *args, **kwargs):
        del args, kwargs
        self.calls += 1
        raise AssertionError("process.exec should not be used")


class _FakeFs:
    def __init__(self, files: dict[str, bytes] | None = None) -> None:
        self.files = dict(files or {})
        self.download_requests: list[str] = []
        self.uploads: list[tuple[str, bytes]] = []
        self.deletes: list[str] = []

    async def download_file(self, path: str) -> bytes:
        self.download_requests.append(path)
        try:
            return self.files[path]
        except KeyError as exc:
            raise FileNotFoundError(path) from exc

    async def download_files(self, requests):
        self.download_requests.extend(request.source for request in requests)
        return [
            SimpleNamespace(source=request.source, result=self.files.get(request.source))
            for request in requests
        ]

    async def upload_file(self, content: bytes, path: str) -> None:
        self.uploads.append((path, content))
        self.files[path] = content

    async def delete_file(self, path: str) -> None:
        self.deletes.append(path)
        self.files.pop(path, None)


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
    assert (tmp_path / "pkg" / "new.py").read_text(encoding="utf-8") == (
        "CREATED = True\n"
    )

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


def test_process_backed_sandbox_no_longer_handles_file_io(tmp_path: Path) -> None:
    target = tmp_path / "local.py"
    target.write_text("value = 1\n", encoding="utf-8")
    process = _UnexpectedProcess()
    content = ContentManager(str(tmp_path), sandbox=SimpleNamespace(process=process))

    assert content.read("local.py") == ("value = 1\n", True)

    content.write("created.py", "created = True\n")
    assert (tmp_path / "created.py").read_text(encoding="utf-8") == "created = True\n"

    content.delete("created.py")
    assert not (tmp_path / "created.py").exists()
    assert process.calls == 0


def test_read_many_uses_filesystem_batch_download(tmp_path: Path) -> None:
    a = str(tmp_path / "a.py")
    b = str(tmp_path / "b.py")
    process = _UnexpectedProcess()
    fs = _FakeFs({a: b"A = 1\n", b: b"B = 2\n"})
    content = ContentManager(str(tmp_path), sandbox=SimpleNamespace(fs=fs, process=process))

    result = content.read_many([a, b, a], allow_missing=False)

    assert result[a] == ("A = 1\n", True)
    assert result[b] == ("B = 2\n", True)
    assert fs.download_requests == [a, b]
    assert process.calls == 0


def test_fs_only_sandbox_accepts_async_fs_methods(tmp_path: Path) -> None:
    fs = _FakeFs()
    manager = ContentManager(str(tmp_path), sandbox=SimpleNamespace(fs=fs))
    target = str(tmp_path / "async.py")

    manager.write(target, "value = 1\n")
    assert manager.read(target) == ("value = 1\n", True)

    manager.delete(target)
    assert target not in fs.files
    assert fs.uploads == [(target, b"value = 1\n")]
    assert fs.deletes == [target]


def test_apply_many_uses_surviving_write_delete_paths(tmp_path: Path) -> None:
    target_a = tmp_path / "a.py"
    target_b = tmp_path / "b.py"
    target_b.write_text("old\n", encoding="utf-8")
    manager = ContentManager(str(tmp_path))

    manager.apply_many([(str(target_a), "A = 1\n"), (str(target_b), None)])

    assert target_a.read_text(encoding="utf-8") == "A = 1\n"
    assert not target_b.exists()


def test_apply_many_with_base_check_handles_large_local_payload(tmp_path: Path) -> None:
    big_content = "".join(f"line_{i} = {i}\n" for i in range(5000))
    target = tmp_path / "groupby.py"
    target.write_text("seed\n", encoding="utf-8")
    manager = ContentManager(str(tmp_path))

    result = manager.apply_many_with_base_check(
        [
            CheckedApplyChange(
                file_path=str(target),
                base_hash=content_hash("seed\n"),
                base_existed=True,
                final_content=big_content,
            ),
        ],
    )

    assert result.success, (result.conflict_reason, result.message)
    assert target.read_text(encoding="utf-8") == big_content
