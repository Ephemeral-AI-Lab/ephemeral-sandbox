"""Tests for advisory write-scope enforcement.

Write-scope was changed from hard-blocking to advisory: developers can write
outside their assigned scope_paths with a warning instead of an error.
Validators remain hard-blocked. The coordination warning gate in posthook
no longer blocks submission.
"""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit._daytona_utils import (
    _normalize_write_scope,
    _path_under_write_scope,
    _team_repo_write_error,
    _team_repo_write_warning,
    is_coordinated_team_agent,
    record_coordination_warning,
)
from tools.posthook.toolkit import _coordination_warning_gate, _coordination_warnings


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


# ---------------------------------------------------------------------------
# _team_repo_write_error: hard block removed for developers
# ---------------------------------------------------------------------------


def test_write_error_returns_none_for_developer_outside_scope():
    """Core change: developer outside write_scope is no longer blocked."""
    ctx = _ctx({
        "agent_name": "developer",
        "team_mode_enabled": True,
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/config.py"],
    })
    result = _team_repo_write_error(ctx, "/testbed/dask/tests/test_config.py", tool_name="edit")
    assert result is None


def test_write_error_returns_none_for_developer_inside_scope():
    """In-scope writes remain allowed (no change)."""
    ctx = _ctx({
        "agent_name": "developer",
        "team_mode_enabled": True,
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/"],
    })
    result = _team_repo_write_error(ctx, "/testbed/dask/config.py", tool_name="edit")
    assert result is None


def test_write_error_returns_none_when_no_write_scope():
    """No write_scope set means unconstrained — no error."""
    ctx = _ctx({
        "agent_name": "developer",
        "team_mode_enabled": True,
        "daytona_cwd": "/testbed",
    })
    result = _team_repo_write_error(ctx, "/testbed/anything.py", tool_name="edit")
    assert result is None


def test_write_error_blocks_validator_even_inside_scope():
    """Validators must never write repo files, regardless of scope."""
    ctx = _ctx({
        "agent_name": "validator",
        "team_mode_enabled": True,
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/"],
    })
    result = _team_repo_write_error(ctx, "/testbed/dask/config.py", tool_name="edit")
    assert result is not None
    assert "validator lanes must not write" in result


def test_write_error_blocks_validator_without_scope():
    """Validators are blocked even when no write_scope is set."""
    ctx = _ctx({
        "agent_name": "validator",
        "team_mode_enabled": True,
        "daytona_cwd": "/testbed",
    })
    result = _team_repo_write_error(ctx, "/testbed/dask/config.py", tool_name="edit")
    assert result is not None
    assert "validator" in result


def test_write_error_returns_none_for_non_team_mode():
    """Non-team-mode agents are never constrained."""
    ctx = _ctx({
        "agent_name": "developer",
        "team_mode_enabled": False,
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/config.py"],
    })
    result = _team_repo_write_error(ctx, "/testbed/dask/tests/test_config.py", tool_name="edit")
    assert result is None


def test_write_error_returns_none_for_absolute_path_outside_repo():
    """Paths that don't normalize to a repo-relative path are not blocked."""
    ctx = _ctx({
        "agent_name": "developer",
        "team_mode_enabled": True,
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/"],
    })
    result = _team_repo_write_error(ctx, "/tmp/scratch.py", tool_name="edit")
    assert result is None


# ---------------------------------------------------------------------------
# _team_repo_write_warning: advisory for all out-of-scope writes
# ---------------------------------------------------------------------------


def test_write_warning_emitted_for_developer_outside_scope():
    """Out-of-scope writes now always produce an advisory warning."""
    ctx = _ctx({
        "agent_name": "developer",
        "team_mode_enabled": True,
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/config.py"],
    })
    result = _team_repo_write_warning(ctx, "/testbed/dask/tests/test_config.py", tool_name="edit")
    assert result is not None
    assert "advisory" in result
    assert "outside write_scope" in result


def test_write_warning_none_for_in_scope_write():
    """No warning for writes within scope."""
    ctx = _ctx({
        "agent_name": "developer",
        "team_mode_enabled": True,
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/"],
    })
    result = _team_repo_write_warning(ctx, "/testbed/dask/config.py", tool_name="edit")
    assert result is None


def test_write_warning_none_when_no_scope_set():
    """No warning when write_scope is not set (unconstrained)."""
    ctx = _ctx({
        "agent_name": "developer",
        "team_mode_enabled": True,
        "daytona_cwd": "/testbed",
    })
    result = _team_repo_write_warning(ctx, "/testbed/anything.py", tool_name="edit")
    assert result is None


def test_write_warning_none_for_non_team_mode():
    """Non-team-mode agents get no warnings."""
    ctx = _ctx({
        "agent_name": "developer",
        "team_mode_enabled": False,
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/config.py"],
    })
    result = _team_repo_write_warning(ctx, "/testbed/other.py", tool_name="edit")
    assert result is None


def test_write_warning_includes_tool_name_and_path():
    """Warning message includes the tool name and target path for debugging."""
    ctx = _ctx({
        "agent_name": "developer",
        "team_mode_enabled": True,
        "daytona_cwd": "/testbed",
        "write_scope": ["src/auth/"],
    })
    result = _team_repo_write_warning(ctx, "/testbed/src/utils/helpers.py", tool_name="daytona_edit_file")
    assert result is not None
    assert "daytona_edit_file" in result
    assert "src/utils/helpers.py" in result
    assert "src/auth" in result


def test_write_warning_for_non_verification_surface_path():
    """Advisory warnings apply to all out-of-scope paths, not just verification surfaces."""
    ctx = _ctx({
        "agent_name": "developer",
        "team_mode_enabled": True,
        "daytona_cwd": "/testbed",
        "write_scope": ["dask/compatibility.py"],
        "verification_surface_write_enforcement": "warn",
        "owned_failures": ["dask/tests/test_cli.py"],
    })
    # _compatibility.py is NOT in the verification surface
    result = _team_repo_write_warning(ctx, "/testbed/dask/_compatibility.py", tool_name="edit")
    assert result is not None
    assert "advisory" in result


# ---------------------------------------------------------------------------
# _coordination_warning_gate: never blocks submission
# ---------------------------------------------------------------------------


def test_coordination_gate_returns_none_with_warnings():
    """Gate must never block — warnings are advisory."""
    ctx = _ctx({
        "coordination_warnings": [
            {"category": "write_scope", "message": "some warning"},
        ],
    })
    result = _coordination_warning_gate(ctx, action="submit_summary()")
    assert result is None


def test_coordination_gate_returns_none_without_warnings():
    """Gate returns None even with empty warnings list."""
    ctx = _ctx({"coordination_warnings": []})
    result = _coordination_warning_gate(ctx, action="submit_summary()")
    assert result is None


def test_coordination_gate_returns_none_with_no_metadata():
    """Gate returns None when no coordination_warnings key exists."""
    ctx = _ctx({})
    result = _coordination_warning_gate(ctx, action="request_retry()")
    assert result is None


def test_coordination_gate_returns_none_with_many_warnings():
    """Gate returns None even with multiple accumulated warnings."""
    ctx = _ctx({
        "coordination_warnings": [
            {"category": "write_scope", "message": f"warning {i}"}
            for i in range(10)
        ],
    })
    result = _coordination_warning_gate(ctx, action="submit_summary()")
    assert result is None


# ---------------------------------------------------------------------------
# _coordination_warnings: helper still collects correctly
# ---------------------------------------------------------------------------


def test_coordination_warnings_collects_messages():
    """The warning collector still works — it feeds advisory context, not gates."""
    ctx = _ctx({
        "coordination_warnings": [
            {"category": "write_scope", "message": "first"},
            {"category": "write_scope", "message": "second"},
        ],
    })
    warnings = _coordination_warnings(ctx)
    assert len(warnings) == 2
    assert "first" in warnings
    assert "second" in warnings


def test_coordination_warnings_deduplicates():
    ctx = _ctx({
        "coordination_warnings": [
            {"category": "write_scope", "message": "same"},
            {"category": "write_scope", "message": "same"},
        ],
    })
    warnings = _coordination_warnings(ctx)
    assert len(warnings) == 1


def test_coordination_warnings_returns_empty_when_none():
    ctx = _ctx({})
    warnings = _coordination_warnings(ctx)
    assert warnings == []


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
