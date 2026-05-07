"""Regression tests for the changeset → guarded-result projection helpers."""

from __future__ import annotations

from sandbox.occ.result_projection import committed_paths, conflict_and_status
from sandbox.occ.changeset.types import FileResult, FileStatus


def test_committed_paths_returns_committed_when_present() -> None:
    files = (
        FileResult(path="/ws/a.py", status=FileStatus.COMMITTED),
        FileResult(path="/ws/b.py", status=FileStatus.ABORTED_VERSION, message="content"),
    )
    assert committed_paths(files, fallback_path="/ws/x.py") == ("/ws/a.py",)


def test_committed_paths_treats_accepted_as_published() -> None:
    files = (
        FileResult(path="/ws/a.py", status=FileStatus.ACCEPTED),
        FileResult(path="/ws/.git/config", status=FileStatus.DROPPED),
    )
    assert committed_paths(files, fallback_path="/ws/x.py") == ("/ws/a.py",)


def test_committed_paths_falls_back_to_aborted_path() -> None:
    files = (
        FileResult(
            path="/ws/app.py",
            status=FileStatus.ABORTED_OVERLAP,
            message="concurrent edit overlaps the operation window",
        ),
    )
    assert committed_paths(files, fallback_path="/ws/x.py") == ("/ws/app.py",)


def test_committed_paths_uses_fallback_when_no_files() -> None:
    assert committed_paths((), fallback_path="/ws/x.py") == ("/ws/x.py",)


def test_conflict_and_status_returns_committed_when_no_failures() -> None:
    conflict, status = conflict_and_status(
        (FileResult(path="/ws/a.py", status=FileStatus.COMMITTED),)
    )
    assert conflict is None
    assert status == "committed"


def test_conflict_and_status_treats_accepted_and_dropped_as_success() -> None:
    conflict, status = conflict_and_status(
        (
            FileResult(path="/ws/a.py", status=FileStatus.ACCEPTED),
            FileResult(path="/ws/.git/config", status=FileStatus.DROPPED),
        )
    )
    assert conflict is None
    assert status == "committed"


def test_conflict_and_status_surfaces_first_failure() -> None:
    conflict, status = conflict_and_status(
        (
            FileResult(
                path="/ws/app.py",
                status=FileStatus.ABORTED_VERSION,
                message="file content changed before delete",
            ),
        )
    )
    assert status == "aborted_version"
    assert conflict is not None
    assert conflict.reason == "aborted_version"
    assert conflict.conflict_file == "/ws/app.py"
    assert conflict.message == "file content changed before delete"


def test_conflict_and_status_falls_back_to_status_string_when_message_empty() -> None:
    conflict, status = conflict_and_status(
        (FileResult(path="/ws/a.py", status=FileStatus.FAILED),)
    )
    assert status == "failed"
    assert conflict is not None
    assert conflict.message == "failed"


def test_conflict_and_status_handles_empty_files() -> None:
    conflict, status = conflict_and_status(())
    assert conflict is None
    assert status == "committed"
