"""Tests for tools.daytona_toolkit.edit_tool."""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit.edit_tool import (
    _content_hash,
    _scope_overlap_warning,
    daytona_edit_file,
)


# pytest-asyncio runs in auto mode — async tests are handled
# automatically. A module-level `pytestmark = pytest.mark.asyncio` would
# emit a warning for every sync test in this file.


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


def _make_sandbox(*, download_content: str = "original content"):
    sb = MagicMock()
    sb.fs.download_file = AsyncMock(return_value=download_content.encode("utf-8"))
    sb.fs.upload_file = AsyncMock()
    return sb


# ---------------------------------------------------------------------------
# _content_hash
# ---------------------------------------------------------------------------

def test_content_hash_returns_16_chars():
    h = _content_hash("hello world")
    assert len(h) == 16


def test_content_hash_deterministic():
    assert _content_hash("abc") == _content_hash("abc")


def test_content_hash_different_for_different_content():
    assert _content_hash("abc") != _content_hash("xyz")


# ---------------------------------------------------------------------------
# No sandbox in context
# ---------------------------------------------------------------------------

async def test_edit_no_sandbox_returns_error():
    ctx = _ctx()
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py", old_text="old", new_text="new"
        ),
        ctx,
    )
    assert result.is_error
    assert "No Daytona sandbox" in result.output


# ---------------------------------------------------------------------------
# Read failure
# ---------------------------------------------------------------------------

async def test_edit_file_read_failure():
    sb = _make_sandbox()
    sb.fs.download_file = AsyncMock(side_effect=FileNotFoundError("gone"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/missing.py", old_text="old", new_text="new"
        ),
        ctx,
    )
    assert result.is_error
    assert "does not exist" in result.output


async def test_edit_file_read_generic_exception():
    sb = _make_sandbox()
    sb.fs.download_file = AsyncMock(side_effect=RuntimeError("network"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py", old_text="old", new_text="new"
        ),
        ctx,
    )
    assert result.is_error
    assert "network" in result.output


# ---------------------------------------------------------------------------
# Text not found
# ---------------------------------------------------------------------------

async def test_edit_old_text_not_found():
    sb = _make_sandbox(download_content="hello world")
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py", old_text="MISSING", new_text="new"
        ),
        ctx,
    )
    assert result.is_error
    assert "Search text not found" in result.output


# ---------------------------------------------------------------------------
# Dry run
# ---------------------------------------------------------------------------

async def test_edit_dry_run_shows_diff():
    sb = _make_sandbox(download_content="def foo():\n    pass\n")
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/ws"})
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/ws/file.py",
            old_text="    pass",
            new_text="    return 42",
            dry_run=True,
        ),
        ctx,
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["status"] == "dry_run"
    assert "diff" in data
    assert result.metadata.get("dry_run") is True
    # File should NOT have been written
    sb.fs.upload_file.assert_not_called()


async def test_edit_dry_run_no_actual_write():
    sb = _make_sandbox(download_content="original text here")
    ctx = _ctx({"daytona_sandbox": sb})
    await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py",
            old_text="original",
            new_text="replaced",
            dry_run=True,
        ),
        ctx,
    )
    sb.fs.upload_file.assert_not_called()


async def test_edit_dry_run_truncates_large_diff():
    # Each changed line must be long enough that the diff output itself
    # exceeds _OUTPUT_MAX_CHARS (8000). Use 40 lines each ~210 chars
    # → diff output ~ 40 * 210 * 2 (old+new) + headers > 8000 chars.
    old_text = "\n".join("old_" + "x" * 200 for _ in range(40)) + "\n"
    new_text = "\n".join("new_" + "y" * 200 for _ in range(40)) + "\n"
    content = old_text  # whole file is old_text
    sb = _make_sandbox(download_content=content)
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py",
            old_text=old_text,
            new_text=new_text,
            dry_run=True,
        ),
        ctx,
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["status"] == "dry_run"
    assert "(truncated)" in data["diff"]


# ---------------------------------------------------------------------------
# Direct write (no CI service)
# ---------------------------------------------------------------------------

async def test_edit_direct_write_success():
    sb = _make_sandbox(download_content="hello world\nfoo bar\n")
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/ws"})
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/ws/file.py",
            old_text="hello world",
            new_text="goodbye world",
        ),
        ctx,
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["status"] == "edited"
    assert data["occ"] is False
    sb.fs.upload_file.assert_called_once()
    # Check the written content
    written_bytes = sb.fs.upload_file.call_args[0][0]
    assert b"goodbye world" in written_bytes


async def test_edit_warns_write_outside_write_scope():
    """Write-scope is advisory — out-of-scope writes succeed with a warning."""
    sb = _make_sandbox(download_content="original")
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "team_mode_enabled": True,
            "write_scope": ["dask/config.py"],
        }
    )

    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/testbed/dask/tests/test_config.py",
            old_text="original",
            new_text="patched",
        ),
        ctx,
    )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["warnings"]
    assert any("outside write_scope" in w for w in data["warnings"])


async def test_edit_allows_write_inside_write_scope():
    sb = _make_sandbox(download_content="original")
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "team_mode_enabled": True,
            "write_scope": ["dask/"],
        }
    )

    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/testbed/dask/tests/test_config.py",
            old_text="original",
            new_text="patched",
        ),
        ctx,
    )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["status"] == "edited"
    sb.fs.upload_file.assert_called_once()


async def test_edit_records_scope_warning_on_advisory_verification_surface_write():
    sb = _make_sandbox(download_content="original")
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "team_mode_enabled": True,
            "write_scope": ["dask/cli.py"],
            "verification_surface_write_enforcement": "warn",
            "owned_failures": ["dask/tests/test_cli.py"],
            "verify": ["pytest dask/tests/test_cli.py -q"],
        }
    )

    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/testbed/dask/tests/test_cli.py",
            old_text="original",
            new_text="patched",
        ),
        ctx,
    )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["warnings"]
    warnings = ctx.metadata["coordination_warnings"]
    assert warnings
    assert "outside write_scope" in warnings[0]["message"]


async def test_edit_warns_non_verify_surface_write_in_warn_mode():
    """Write-scope is advisory — non-verify-surface writes also succeed with a warning."""
    sb = _make_sandbox(download_content="original")
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "team_mode_enabled": True,
            "write_scope": ["dask/compatibility.py"],
            "verification_surface_write_enforcement": "warn",
            "owned_failures": ["dask/tests/test_cli.py"],
            "verify": ["pytest dask/tests/test_cli.py -q"],
        }
    )

    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/testbed/dask/_compatibility.py",
            old_text="original",
            new_text="patched",
        ),
        ctx,
    )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["warnings"]
    assert any("outside write_scope" in w for w in data["warnings"])


def test_scope_overlap_warning_ignores_same_agent_run_id():
    own_change = SimpleNamespace(
        file_path="dask/config.py",
        edit_type="edit",
        agent_id="developer",
        agent_run_id="run-1",
        created_at=SimpleNamespace(timestamp=lambda: 0),
    )
    other_change = SimpleNamespace(
        file_path="dask/compatibility.py",
        edit_type="edit",
        agent_id="peer",
        agent_run_id="run-2",
        created_at=SimpleNamespace(timestamp=lambda: 0),
    )
    ctx = _ctx(
        {
            "file_change_store": SimpleNamespace(
                initialized=True,
                changes_since=lambda _since: [own_change, other_change],
            ),
            "agent_run_id": "run-1",
            "write_scope": ["dask/"],
            "work_item_started_at": 1.0,
        }
    )

    warning = _scope_overlap_warning(ctx, "dask/config.py")

    assert "dask/compatibility.py" in warning
    assert "dask/config.py (" not in warning


async def test_edit_rejects_repo_write_from_validator():
    sb = _make_sandbox(download_content="original")
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "validator",
            "team_mode_enabled": True,
        }
    )

    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/testbed/dask/config.py",
            old_text="original",
            new_text="patched",
        ),
        ctx,
    )

    assert result.is_error
    assert "validator lanes must not write repository files" in result.output
    sb.fs.upload_file.assert_not_called()


async def test_edit_direct_write_exception():
    sb = _make_sandbox(download_content="content here")
    sb.fs.upload_file = AsyncMock(side_effect=RuntimeError("write fail"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py",
            old_text="content here",
            new_text="new content",
        ),
        ctx,
    )
    assert result.is_error
    assert "write fail" in result.output


async def test_edit_replaces_only_first_occurrence():
    sb = _make_sandbox(download_content="x x x")
    ctx = _ctx({"daytona_sandbox": sb})
    await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py", old_text="x", new_text="y"
        ),
        ctx,
    )
    written_bytes = sb.fs.upload_file.call_args[0][0]
    # Only first x replaced → "y x x"
    assert written_bytes == b"y x x"


async def test_edit_line_range_rejected():
    sb = _make_sandbox(download_content="a\nb\nc\n")
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py",
            edits=[
                {
                    "strategy": "line_range",
                    "start_line": 2,
                    "end_line": 2,
                    "new_content": "beta",
                }
            ],
        ),
        ctx,
    )
    assert result.is_error
    assert "unknown strategy" in result.output


async def test_edit_batch_direct_write_success():
    sb = _make_sandbox(download_content="alpha\nbeta\ngamma\n")
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py",
            edits=[
                {"strategy": "search_replace", "search": "alpha", "replace": "ALPHA"},
                {"strategy": "search_replace", "search": "gamma", "replace": "GAMMA"},
            ],
        ),
        ctx,
    )
    assert not result.is_error
    written_bytes = sb.fs.upload_file.call_args[0][0]
    assert written_bytes == b"ALPHA\nbeta\nGAMMA\n"


async def test_edit_rejects_mixed_legacy_and_batch_inputs():
    sb = _make_sandbox(download_content="alpha\n")
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py",
            old_text="alpha",
            new_text="beta",
            edits=[{"strategy": "search_replace", "search": "alpha", "replace": "beta"}],
        ),
        ctx,
    )
    assert result.is_error
    assert "Provide either `old_text`/`new_text` or `edits`" in result.output


# ---------------------------------------------------------------------------
# OCC path (with CI arbiter)
# ---------------------------------------------------------------------------

async def test_edit_occ_path_success():
    sb = _make_sandbox(download_content="old content\n")
    svc = MagicMock()
    svc.prepare_write.return_value = SimpleNamespace(
        file_path="/file.py",
        current_content="old content\n",
        current_hash=_content_hash("old content\n"),
        token_id="tok-1",
        existed=True,
    )
    svc.commit_prepared_write.return_value = SimpleNamespace(success=True, message="ok")
    svc.abort_prepared_write = MagicMock()
    ctx = _ctx({"daytona_sandbox": sb, "ci_service": svc})

    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py",
            old_text="old content",
            new_text="new content",
        ),
        ctx,
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["occ"] is True
    svc.prepare_write.assert_called_once()
    svc.commit_prepared_write.assert_called_once()
    svc.abort_prepared_write.assert_called_once()


async def test_edit_occ_refreshes_and_repatches_latest_content():
    sb = _make_sandbox(download_content="old content\n")
    initial = SimpleNamespace(
        file_path="/file.py",
        current_content="alpha\nold\nomega\n",
        current_hash=_content_hash("alpha\nold\nomega\n"),
        token_id="tok-1",
        existed=True,
    )
    refreshed = SimpleNamespace(
        file_path="/file.py",
        current_content="prefix\nalpha\nold\nomega\n",
        current_hash=_content_hash("prefix\nalpha\nold\nomega\n"),
        token_id="tok-2",
        existed=True,
    )
    commit = MagicMock(return_value=SimpleNamespace(success=True, message="ok"))
    svc = SimpleNamespace(
        prepare_write=MagicMock(return_value=initial),
        refresh_prepared_write=MagicMock(side_effect=[refreshed, refreshed]),
        commit_prepared_write=commit,
        abort_prepared_write=MagicMock(),
    )
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "ci_service": svc,
            "scope_packet": {"scope_paths": ["/file.py"], "coherence_token": "stale-token"},
            "coherence_token": "stale-token",
        }
    )

    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py",
            old_text="old",
            new_text="new",
        ),
        ctx,
    )

    assert not result.is_error
    commit.assert_called_once()
    assert commit.call_args.args[1] == "prefix\nalpha\nnew\nomega\n"


async def test_edit_occ_publishes_and_releases_symbol_intent():
    sb = _make_sandbox(download_content="def foo():\n    return 1\n")
    initial = SimpleNamespace(
        file_path="/file.py",
        current_content="def foo():\n    return 1\n",
        current_hash=_content_hash("def foo():\n    return 1\n"),
        token_id="tok-1",
        existed=True,
    )
    publish = MagicMock(return_value="intent-1")
    release = MagicMock()
    svc = SimpleNamespace(
        prepare_write=MagicMock(return_value=initial),
        commit_prepared_write=MagicMock(return_value=SimpleNamespace(success=True, message="ok")),
        abort_prepared_write=MagicMock(),
        publish_edit_intent=publish,
        heartbeat_edit_intent=MagicMock(),
        release_edit_intent=release,
        symbol_index=SimpleNamespace(
            symbol_boundaries_for_file=MagicMock(return_value=[("foo", 1, 2)])
        ),
    )
    ctx = _ctx({"daytona_sandbox": sb, "ci_service": svc})

    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py",
            old_text="return 1",
            new_text="return 2",
        ),
        ctx,
    )

    assert not result.is_error
    publish.assert_called_once()
    assert publish.call_args.kwargs["symbols"] == ["foo"]
    assert publish.call_args.kwargs["scope"] == "symbol"
    release.assert_called_once_with("intent-1")


async def test_edit_occ_lock_conflict():
    sb = _make_sandbox(download_content="content")
    svc = MagicMock()
    svc.prepare_write.return_value = SimpleNamespace(
        success=False,
        message="Could not acquire edit lock for /file.py (conflict)",
        conflict=True,
    )
    ctx = _ctx({"daytona_sandbox": sb, "ci_service": svc})

    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py", old_text="content", new_text="new"
        ),
        ctx,
    )
    assert result.is_error
    assert "conflict" in result.output
    assert result.metadata.get("conflict") is True


async def test_edit_occ_no_arbiter_falls_back_to_direct():
    """CI service present but no arbiter → direct write path."""
    sb = _make_sandbox(download_content="content here")
    svc = SimpleNamespace(arbiter=None)
    ctx = _ctx({"daytona_sandbox": sb, "ci_service": svc})

    result = await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="/file.py", old_text="content here", new_text="replaced"
        ),
        ctx,
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["occ"] is False


async def test_edit_resolves_relative_path():
    sb = _make_sandbox(download_content="stuff")
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/workspace"})
    await daytona_edit_file.execute(
        daytona_edit_file.input_model(
            file_path="relative.py", old_text="stuff", new_text="other"
        ),
        ctx,
    )
    sb.fs.download_file.assert_called_once_with("/workspace/relative.py")
