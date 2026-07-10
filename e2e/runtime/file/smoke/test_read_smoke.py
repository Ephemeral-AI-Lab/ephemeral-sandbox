"""Live e2e: Read Smoke (5 cases)."""

from runtime.file.helpers import (
    assert_content,
    assert_error,
    assert_ok,
    file_read,
    file_write,
    workspace_session,
)


def test_sessionless_read_of_file_created_by_sessionless_file_write(sandbox):
    """Sessionless read of a file created by sessionless `file_write`."""
    result = file_write(sandbox, "read-smoke/sessionless.txt", "alpha\nbeta")
    assert_ok(result)

    read = file_read(sandbox, "read-smoke/sessionless.txt")
    assert_content(read, "alpha\nbeta")
    assert read["start_line"] == 1
    assert read["num_lines"] == 2
    assert read["total_lines"] == 2


def test_session_read_of_file_created_by_session_file_write(sandbox, workspace_session):
    """Session read of a file created by session `file_write`."""
    result = file_write(
        sandbox,
        "read-smoke/session.txt",
        "session-only",
        workspace_session_id=workspace_session,
    )
    assert_ok(result)

    read = file_read(
        sandbox, "read-smoke/session.txt", workspace_session_id=workspace_session
    )
    assert_content(read, "session-only")


def test_sessionless_read_with_offset_and_limit_over_multiline_file(sandbox):
    """Sessionless read with `offset` and `limit` over a multi-line file."""
    content = "\n".join(f"line-{index}" for index in range(1, 8))
    assert_ok(file_write(sandbox, "read-smoke/window.txt", content))

    read = file_read(sandbox, "read-smoke/window.txt", offset=3, limit=2)
    assert_content(read, "line-3\nline-4")
    assert read["start_line"] == 3
    assert read["num_lines"] == 2
    assert read["total_lines"] == 7
    assert read["next_offset"] == 5
    assert read["truncated"] is True


def test_sessionless_read_of_missing_file_returns_not_found(sandbox):
    """Sessionless read of a missing file returns `not_found`."""
    assert_error(file_read(sandbox, "read-smoke/missing.txt"), "not_found")


def test_sessionless_read_rejects_absolute_path_outside_workspace_root(sandbox):
    """Sessionless read rejects an absolute path outside the workspace root."""
    assert_error(file_read(sandbox, "/tmp/outside-workspace.txt"), "invalid_request")
