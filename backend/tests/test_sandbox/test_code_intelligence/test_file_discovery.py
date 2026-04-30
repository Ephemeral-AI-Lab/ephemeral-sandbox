"""Unit tests for code intelligence file discovery helpers."""

from __future__ import annotations

import logging
from types import SimpleNamespace
from typing import Any

from sandbox.code_intelligence.indexing.file_discovery import collect_remote_files


class _FakeDaytonaFs:
    __module__ = "daytona_sdk._sync.filesystem"

    def __init__(
        self,
        *,
        search_files: list[str] | Exception | None = None,
        list_files: dict[str, list[Any]] | None = None,
    ) -> None:
        self.search_files_result = search_files
        self.list_files_result = list_files or {}
        self.search_calls: list[tuple[str, str]] = []
        self.list_calls: list[str] = []

    def search_files(self, path: str, pattern: str) -> SimpleNamespace:
        self.search_calls.append((path, pattern))
        if isinstance(self.search_files_result, Exception):
            raise self.search_files_result
        return SimpleNamespace(files=self.search_files_result or [])

    def list_files(self, path: str) -> list[Any]:
        self.list_calls.append(path)
        return self.list_files_result.get(path, [])


def test_collect_remote_files_uses_documented_search_wildcard() -> None:
    fs = _FakeDaytonaFs(
        search_files=[
            "/repo/pkg/app.py",
            "/repo/pkg/build.bin",
            "/repo/node_modules/skip.ts",
            "/repo/pkg/view.tsx",
        ]
    )

    files = collect_remote_files(SimpleNamespace(fs=fs), "/repo", max_files=10)

    assert files == ["/repo/pkg/app.py"]
    assert fs.search_calls == [("/repo", "*")]
    assert fs.list_calls == []


def test_collect_remote_files_fallback_omits_expected_search_traceback(caplog) -> None:
    fs = _FakeDaytonaFs(
        search_files=RuntimeError("invalid SearchFilesResponse: files is null"),
        list_files={
            "/repo": [
                SimpleNamespace(name="pkg", is_dir=True),
                SimpleNamespace(name="archive.bin", is_dir=False),
            ],
            "/repo/pkg": [SimpleNamespace(name="module.py", is_dir=False)],
        },
    )

    with caplog.at_level(
        logging.DEBUG,
        logger="sandbox.code_intelligence.indexing.file_discovery",
    ):
        files = collect_remote_files(SimpleNamespace(fs=fs), "/repo", max_files=10)

    assert files == ["/repo/pkg/module.py"]
    assert fs.list_calls == ["/repo", "/repo/pkg"]
    search_logs = [
        record
        for record in caplog.records
        if "search_files failed, falling back to list_files" in record.message
    ]
    assert len(search_logs) == 1
    assert search_logs[0].exc_info is None
