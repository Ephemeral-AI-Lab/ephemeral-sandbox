"""Tests for the shared ``_team_repo_scope_deny_errors`` helper.

Used by the Daytona write-scope guards for delete and move operations. The
helper is pure — it returns only the offending subset so callers can build an
offender-only deny message.
"""

from __future__ import annotations

from pathlib import Path

from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit._daytona_utils import _team_repo_scope_deny_errors


def _ctx(**metadata) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=dict(metadata))


def test_returns_empty_when_not_coordinated_team_agent():
    ctx = _ctx(daytona_cwd="/ws", write_scope=["allowed/"])
    result = _team_repo_scope_deny_errors(
        ctx, ["/ws/other/a.py"], tool_name="daytona_delete_file",
    )
    assert result == []


def test_returns_empty_when_no_write_scope_configured():
    ctx = _ctx(agent_name="developer", daytona_cwd="/ws")
    result = _team_repo_scope_deny_errors(
        ctx, ["/ws/other/a.py"], tool_name="daytona_delete_file",
    )
    assert result == []


def test_returns_empty_for_all_in_scope_paths():
    ctx = _ctx(
        agent_name="developer",
        daytona_cwd="/ws",
        write_scope=["allowed/"],
    )
    result = _team_repo_scope_deny_errors(
        ctx,
        ["/ws/allowed/a.py", "/ws/allowed/sub/b.py"],
        tool_name="daytona_delete_file",
    )
    assert result == []


def test_returns_only_offenders():
    ctx = _ctx(
        agent_name="developer",
        daytona_cwd="/ws",
        write_scope=["allowed/"],
    )
    result = _team_repo_scope_deny_errors(
        ctx,
        [
            "/ws/allowed/a.py",
            "/ws/other/b.py",
            "/ws/allowed/sub/c.py",
            "/ws/elsewhere/d.py",
        ],
        tool_name="daytona_move_file",
    )
    paths = [p for p, _ in result]
    assert paths == ["/ws/other/b.py", "/ws/elsewhere/d.py"]
    for path, msg in result:
        assert "daytona_move_file" in msg
        assert "outside write_scope" in msg


def test_skips_test_files_to_defer_to_higher_priority_block():
    """Test files are caught by _team_repo_write_error (test-file block) with a
    distinct message; this helper intentionally skips them to avoid duplicate
    deny messages."""
    ctx = _ctx(
        agent_name="developer",
        daytona_cwd="/ws",
        write_scope=["allowed/"],
    )
    result = _team_repo_scope_deny_errors(
        ctx,
        [
            "/ws/other/tests/test_foo.py",
            "/ws/other/real.py",
        ],
        tool_name="daytona_delete_file",
    )
    paths = [p for p, _ in result]
    assert paths == ["/ws/other/real.py"]


def test_test_file_paths_counted_when_explicitly_authorized():
    """allow_test_file_edits removes the test-file block, so outside-scope test
    files are then valid Deny offenders here."""
    ctx = _ctx(
        agent_name="developer",
        daytona_cwd="/ws",
        write_scope=["allowed/"],
        allow_test_file_edits=True,
    )
    result = _team_repo_scope_deny_errors(
        ctx,
        ["/ws/other/tests/test_foo.py"],
        tool_name="daytona_delete_file",
    )
    assert [p for p, _ in result] == ["/ws/other/tests/test_foo.py"]
