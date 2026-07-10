"""Live e2e: Write Smoke (5 cases)."""

from runtime.file.helpers import (
    assert_content,
    assert_error,
    assert_ok,
    assert_single_owner,
    exec_command,
    file_blame,
    file_read,
    file_write,
    workspace_session,
)


def test_sessionless_write_creates_new_file_and_sessionless_read_sees_it(sandbox):
    """Sessionless write creates a new file and sessionless read sees it."""
    result = file_write(sandbox, "write-smoke/new.txt", "new\nfile")
    assert result["type"] == "create"
    assert_content(file_read(sandbox, "write-smoke/new.txt"), "new\nfile")
    assert_single_owner(sandbox, "write-smoke/new.txt", prefix="operation:")


def test_sessionless_write_updates_existing_file_and_file_blame_shows_operation_owner(
    sandbox,
):
    """Sessionless write updates an existing file and `file_blame` shows
    `operation:<request_id>`."""
    assert_ok(file_write(sandbox, "write-smoke/update.txt", "before"))
    result = file_write(sandbox, "write-smoke/update.txt", "after")
    assert result["type"] == "update"
    assert_content(file_read(sandbox, "write-smoke/update.txt"), "after")
    owner = assert_single_owner(sandbox, "write-smoke/update.txt", prefix="operation:")
    assert owner in {item["owner"] for item in file_blame(sandbox, "write-smoke/update.txt")["ranges"]}


def test_session_write_visible_with_workspace_session_id_and_invisible_sessionless(
    sandbox, workspace_session
):
    """Session write is visible with `workspace_session_id` and invisible to
    sessionless read before capture."""
    result = file_write(
        sandbox,
        "write-smoke/session-only.txt",
        "draft",
        workspace_session_id=workspace_session,
    )
    assert result["type"] == "create"
    assert_content(
        file_read(
            sandbox,
            "write-smoke/session-only.txt",
            workspace_session_id=workspace_session,
        ),
        "draft",
    )
    assert_error(file_read(sandbox, "write-smoke/session-only.txt"), "not_found")


def test_session_write_creates_missing_parent_directories(sandbox, workspace_session):
    """Session write creates missing parent directories."""
    result = file_write(
        sandbox,
        "write-smoke/a/b/c.txt",
        "nested",
        workspace_session_id=workspace_session,
    )
    assert result["type"] == "create"
    assert_content(
        file_read(sandbox, "write-smoke/a/b/c.txt", workspace_session_id=workspace_session),
        "nested",
    )


def test_write_to_existing_directory_is_rejected(sandbox):
    """Write to an existing directory is rejected."""
    result = exec_command(sandbox, "mkdir write-smoke-dir && printf keep > write-smoke-dir/keep.txt")
    assert result["status"] == "ok"
    assert result["exit_code"] == 0
    assert_single_owner(sandbox, "write-smoke-dir/keep.txt", prefix="workspace_session:")
    assert_error(
        file_write(sandbox, "write-smoke-dir", "not a file"),
        "invalid_request",
    )
