"""Tests for coordinated Daytona repo-write enforcement.

Write-scope was changed from hard-blocking to advisory: developers can write
outside their assigned scope_paths with a warning instead of an error. Test-file
mutations are still blocked in coordinated team lanes so verification surfaces
remain read-only unless explicitly authorized.
"""

from __future__ import annotations

from pathlib import Path

from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit._daytona_utils import (
    _normalize_write_scope,
    _path_under_write_scope,
    _team_repo_write_error,
    _team_repo_write_warning,
    record_coordination_warning,
)
from tools.daytona_toolkit.edit_tool import daytona_edit_file
from tools.daytona_toolkit.tools import daytona_write_file


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


# ---------------------------------------------------------------------------
# _team_repo_write_error: only test-file writes hard-block coordinated writes
# ---------------------------------------------------------------------------


def test_write_error_returns_none_for_developer_outside_scope():
    """Core change: developer outside write_scope is no longer blocked."""
    ctx = _ctx({
        "agent_name": "developer",
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/config.py"],
    })
    result = _team_repo_write_error(ctx, "/testbed/dask/_compatibility.py", tool_name="edit")
    assert result is None


def test_write_error_returns_none_for_developer_inside_scope():
    """In-scope writes remain allowed (no change)."""
    ctx = _ctx({
        "agent_name": "developer",
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/"],
    })
    result = _team_repo_write_error(ctx, "/testbed/dask/config.py", tool_name="edit")
    assert result is None


def test_write_error_returns_none_when_no_write_scope():
    """No write_scope set means unconstrained — no error."""
    ctx = _ctx({
        "agent_name": "developer",
        "daytona_cwd": "/testbed",
    })
    result = _team_repo_write_error(ctx, "/testbed/anything.py", tool_name="edit")
    assert result is None


def test_write_error_allows_validator_inside_scope():
    """Validators may write repo files for targeted fixes."""
    ctx = _ctx({
        "agent_name": "validator",
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/"],
    })
    result = _team_repo_write_error(ctx, "/testbed/dask/config.py", tool_name="edit")
    assert result is None


def test_write_error_allows_validator_without_scope():
    """Validators may write repo files even when no write_scope is set."""
    ctx = _ctx({
        "agent_name": "validator",
        "daytona_cwd": "/testbed",
    })
    result = _team_repo_write_error(ctx, "/testbed/dask/config.py", tool_name="edit")
    assert result is None


def _assert_test_file_write_error(result: str | None, rel_path: str) -> None:
    assert result is not None
    assert "BLOCKED_TEST_FILE_EDIT" in result
    assert rel_path in result
    assert "read/verify-only" in result
    assert "production owner" in result
    assert "submit_task_summary(type='request_replan'" in result
    assert "runtime-authorized test-edit lane" in result


def test_write_error_blocks_test_file_even_inside_scope():
    """Test files in scope_paths remain verification evidence, not edit owners."""
    ctx = _ctx({
        "agent_name": "developer",
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/tests/test_config.py"],
    })
    result = _team_repo_write_error(ctx, "/testbed/dask/tests/test_config.py", tool_name="edit")
    _assert_test_file_write_error(result, "dask/tests/test_config.py")


def test_write_error_blocks_nested_test_file_without_separate_mode_flag():
    ctx = _ctx({
        "agent_name": "developer",
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/"],
    })
    result = _team_repo_write_error(
        ctx,
        "/testbed/dask/dataframe/io/tests/test_hdf.py",
        tool_name="daytona_edit_file",
    )
    _assert_test_file_write_error(result, "dask/dataframe/io/tests/test_hdf.py")


def test_write_error_allows_test_file_outside_team_lane():
    ctx = _ctx({
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/tests/test_cli.py"],
    })
    result = _team_repo_write_error(
        ctx,
        "/testbed/dask/tests/test_cli.py",
        tool_name="daytona_edit_file",
    )
    assert result is None


def test_write_error_allows_explicitly_authorized_test_file_edit():
    ctx = _ctx({
        "agent_name": "developer",
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/tests/test_cli.py"],
        "allow_test_file_edits": True,
    })
    result = _team_repo_write_error(
        ctx,
        "/testbed/dask/tests/test_cli.py",
        tool_name="daytona_edit_file",
    )
    assert result is None


def test_write_error_returns_none_for_absolute_path_outside_repo():
    """Paths that don't normalize to a repo-relative path are not blocked."""
    ctx = _ctx({
        "agent_name": "developer",
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/"],
    })
    result = _team_repo_write_error(ctx, "/tmp/scratch.py", tool_name="edit")
    assert result is None


def test_write_error_returns_none_for_benchmark_outside_scope():
    """Benchmark metadata must not make write_scope a hard block."""
    ctx = _ctx({
        "agent_name": "developer",
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/dataframe/io/hdf.py"],
        "benchmark_test_ids": ["dask/dataframe/io/tests/test_hdf.py::test_to_hdf"],
    })

    result = _team_repo_write_error(
        ctx,
        "/testbed/dask/_compatibility.py",
        tool_name="daytona_write_file",
    )

    assert result is None


# ---------------------------------------------------------------------------
# _team_repo_write_warning: advisory for all out-of-scope writes
# ---------------------------------------------------------------------------


def test_write_warning_emitted_for_developer_outside_scope():
    """Out-of-scope writes now always produce an advisory warning."""
    ctx = _ctx({
        "agent_name": "developer",
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/config.py"],
    })
    result = _team_repo_write_warning(
        ctx,
        "/testbed/dask/tests/test_config.py",
        tool_name="edit",
    )
    assert result is not None
    assert "advisory" in result
    assert "outside write_scope" in result
    assert "adjacent shim" not in result
    assert "not a hard failure" in result
    assert "justified production owner" in result
    assert "submit_task_summary(type='request_replan')" in result
    assert "widened path, rationale, and verification" in result


def test_write_warning_repeated_scope_mismatch_redirects_without_blocking():
    ctx = _ctx({
        "agent_name": "developer",
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/dataframe/io/hdf.py"],
    })

    _team_repo_write_warning(
        ctx,
        "/testbed/dask/_compatibility.py",
        tool_name="daytona_write_file",
    )
    _team_repo_write_warning(
        ctx,
        "/testbed/dask/compatibility.py",
        tool_name="daytona_edit_file",
    )
    result = _team_repo_write_warning(
        ctx,
        "/testbed/dask/base.py",
        tool_name="daytona_edit_file",
    )

    assert result is not None
    assert "3+ outside-scope warnings" in result
    assert "Request replanning unless" in result
    assert "one coherent production owner" in result
    assert _team_repo_write_error(
        ctx,
        "/testbed/dask/base.py",
        tool_name="daytona_edit_file",
    ) is None


def test_write_and_edit_schema_keeps_scope_rules_simple():
    write_schema = daytona_write_file.to_api_schema()
    edit_schema = daytona_edit_file.to_api_schema()

    write_description = write_schema["description"]
    assert "there is no `write_file` tool" in write_description
    assert "test-file writes are blocked unless runtime metadata allows them" in write_description
    assert "Outside-scope production writes need a clear ownership reason" in write_description
    assert "If scope is unclear, request replanning" in write_description

    edit_description = edit_schema["description"]
    assert "Use exactly one mode" in edit_description
    assert "Do not send `new_text` with `edits`" in edit_description
    assert "test-file writes are blocked unless runtime metadata allows them" in edit_description
    assert "Outside-scope production edits need a clear ownership reason" in edit_description
    assert "If scope is unclear, request replanning" in edit_description


def test_write_warning_none_for_in_scope_write():
    """No warning for writes within scope."""
    ctx = _ctx({
        "agent_name": "developer",
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/"],
    })
    result = _team_repo_write_warning(ctx, "/testbed/dask/config.py", tool_name="edit")
    assert result is None


def test_write_warning_none_when_no_scope_set():
    """No warning when write_scope is not set (unconstrained)."""
    ctx = _ctx({
        "agent_name": "developer",
        "daytona_cwd": "/testbed",
    })
    result = _team_repo_write_warning(ctx, "/testbed/anything.py", tool_name="edit")
    assert result is None


def test_write_warning_emitted_without_team_mode_flag():
    """Developer coordination safeguards no longer require a separate mode flag."""
    ctx = _ctx({
        "agent_name": "developer",
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/config.py"],
    })
    result = _team_repo_write_warning(ctx, "/testbed/other.py", tool_name="edit")
    assert result is not None
    assert "outside write_scope" in result


def test_write_warning_includes_tool_name_and_path():
    """Warning message includes the tool name and target path for debugging."""
    ctx = _ctx({
        "agent_name": "developer",
        "daytona_cwd": "/testbed",
        "write_scope": ["src/auth/"],
    })
    result = _team_repo_write_warning(
        ctx,
        "/testbed/src/utils/helpers.py",
        tool_name="daytona_edit_file",
    )
    assert result is not None
    assert "daytona_edit_file" in result
    assert "src/utils/helpers.py" in result
    assert "src/auth" in result


def test_write_warning_for_non_test_path():
    """Advisory warnings apply to non-test out-of-scope paths."""
    ctx = _ctx({
        "agent_name": "developer",
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/compatibility.py"],
        "verification_surface_write_enforcement": "warn",
        "owned_failures": ["dask/tests/test_cli.py"],
    })
    # _compatibility.py is NOT in the verification surface
    result = _team_repo_write_warning(
        ctx,
        "/testbed/dask/_compatibility.py",
        tool_name="edit",
    )
    assert result is not None
    assert "advisory" in result




# ---------------------------------------------------------------------------
# record_coordination_warning: still records (for observability)
# ---------------------------------------------------------------------------


def test_record_coordination_warning_persists_on_context():
    """Warnings are still recorded for observability even though they don't block."""
    ctx = _ctx({})
    record_coordination_warning(ctx, category="write_scope", message="test warning")
    warnings = ctx.metadata["coordination_warnings"]
    assert len(warnings) == 1
    assert warnings[0]["message"] == "test warning"
    assert ctx.metadata["coordination_warning_present"] is True


def test_record_coordination_warning_deduplicates():
    ctx = _ctx({})
    record_coordination_warning(ctx, category="write_scope", message="dup")
    record_coordination_warning(ctx, category="write_scope", message="dup")
    assert len(ctx.metadata["coordination_warnings"]) == 1


def test_record_coordination_warning_allows_different_messages():
    ctx = _ctx({})
    record_coordination_warning(ctx, category="write_scope", message="a")
    record_coordination_warning(ctx, category="write_scope", message="b")
    assert len(ctx.metadata["coordination_warnings"]) == 2


# ---------------------------------------------------------------------------
# Path helpers: unchanged behavior, regression tests
# ---------------------------------------------------------------------------


def test_normalize_write_scope_basic():
    result = _normalize_write_scope(["dask/config.py", "dask/tests/"], "/testbed")
    assert result == ["dask/config.py", "dask/tests"]


def test_normalize_write_scope_with_absolute_paths():
    result = _normalize_write_scope(["/testbed/dask/config.py"], "/testbed")
    assert result == ["dask/config.py"]


def test_normalize_write_scope_empty():
    assert _normalize_write_scope(None, "/testbed") == []
    assert _normalize_write_scope([], "/testbed") == []


def test_path_under_write_scope_exact_match():
    assert _path_under_write_scope("dask/config.py", ["dask/config.py"]) is True


def test_path_under_write_scope_directory_prefix():
    assert _path_under_write_scope("dask/tests/test_config.py", ["dask/"]) is True
    assert _path_under_write_scope("dask/tests/test_config.py", ["dask/tests/"]) is True


def test_path_under_write_scope_no_match():
    assert _path_under_write_scope("other/file.py", ["dask/"]) is False


def test_path_under_write_scope_partial_name_no_match():
    """'dask_extra/foo.py' should not match scope 'dask/'."""
    assert _path_under_write_scope("dask_extra/foo.py", ["dask/"]) is False
