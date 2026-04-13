"""Tests for shared CI runtime helpers."""

from __future__ import annotations

import hashlib
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock, patch

from tools.core.base import ToolExecutionContext
from tools.core.ci_runtime import (
    abort_ci_write,
    finalize_ci_write,
    get_ci_service,
    prepare_ci_write,
    prime_cache_after_write,
    record_edit_in_arbiter,
)


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


# ---------------------------------------------------------------------------
# get_ci_service
# ---------------------------------------------------------------------------


def test_get_ci_service_returns_none_when_missing():
    ctx = _ctx()
    assert get_ci_service(ctx) is None


def test_get_ci_service_returns_value():
    svc = MagicMock()
    ctx = _ctx({"ci_service": svc})
    assert get_ci_service(ctx) is svc


# ---------------------------------------------------------------------------
# prime_cache_after_write
# ---------------------------------------------------------------------------


def test_prime_cache_no_service_is_noop():
    ctx = _ctx()
    prime_cache_after_write(ctx, "/some/file.py", "content")  # should not raise


def test_prime_cache_calls_service_methods():
    svc = MagicMock()
    ctx = _ctx({"ci_service": svc})
    prime_cache_after_write(ctx, "/file.py", "hello")
    svc.symbol_index.refresh.assert_called_once_with("/file.py", "hello")
    svc.lsp_client.invalidate.assert_called_once_with("/file.py")


def test_prime_cache_swallows_exceptions():
    svc = MagicMock()
    svc.symbol_index.refresh.side_effect = RuntimeError("boom")
    ctx = _ctx({"ci_service": svc})
    prime_cache_after_write(ctx, "/file.py", "hello")  # must not raise


# ---------------------------------------------------------------------------
# prepare_ci_write
# ---------------------------------------------------------------------------


def test_prepare_ci_write_returns_none_without_service():
    ctx = _ctx()
    result, packet, err = prepare_ci_write(ctx, "/repo/file.py")
    assert result is None
    assert err is None


def test_prepare_ci_write_calls_service():
    svc = MagicMock()
    prepared = SimpleNamespace(file_path="/repo/file.py", token_id="tok-1")
    svc.prepare_write.return_value = prepared
    ctx = _ctx({"ci_service": svc, "agent_run_id": "worker-1"})

    result, packet, err = prepare_ci_write(ctx, "/repo/file.py")

    assert err is None
    assert result is prepared
    svc.prepare_write.assert_called_once_with(
        "/repo/file.py",
        agent_id="worker-1",
        expected_hash="",
        allow_missing=True,
    )


def test_prepare_ci_write_reports_failure():
    svc = MagicMock()
    svc.prepare_write.return_value = SimpleNamespace(
        file_path="/repo/file.py", success=False, message="locked"
    )
    ctx = _ctx({"ci_service": svc, "agent_run_id": "worker-1"})

    result, packet, err = prepare_ci_write(ctx, "/repo/file.py")

    assert result is None
    assert err == "locked"


# ---------------------------------------------------------------------------
# finalize_ci_write
# ---------------------------------------------------------------------------


def test_finalize_ci_write_commits():
    svc = MagicMock()
    svc.commit_prepared_write.return_value = SimpleNamespace(success=True)
    ctx = _ctx({"ci_service": svc})
    prepared = SimpleNamespace(file_path="/repo/file.py")

    result = finalize_ci_write(
        ctx, prepared, content="hello", edit_type="write", description="desc",
    )

    assert result.success is True


def test_finalize_ci_write_enriches_prepared_write_with_symbol_boundaries():
    captured: dict[str, object] = {}

    def commit(prepared, content, *, edit_type, description):
        captured["prepared"] = prepared
        captured["content"] = content
        captured["edit_type"] = edit_type
        captured["description"] = description
        return SimpleNamespace(success=True)

    svc = MagicMock()
    svc.commit_prepared_write.side_effect = commit
    svc.symbol_index.symbol_boundaries_for_file.return_value = [("foo", 3, 4)]
    ctx = _ctx({"ci_service": svc})
    prepared = SimpleNamespace(
        file_path="/repo/file.py",
        current_content="header\n\ndef foo():\n    return 1\n",
        current_hash="hash-1",
    )

    result = finalize_ci_write(
        ctx,
        prepared,
        content="header\n\ndef foo():\n    return 2\n",
        edit_type="edit",
        description="change foo",
    )

    enriched = captured["prepared"]
    assert result.success is True
    assert getattr(enriched, "line_start", None) == 3
    assert getattr(enriched, "line_end", None) == 5
    assert getattr(enriched, "operation_type", None) == "replace"


def test_finalize_ci_write_mirrors_team_edit_and_notifies_listener():
    svc = MagicMock()
    svc.commit_prepared_write.return_value = SimpleNamespace(success=True)
    svc.arbiter = SimpleNamespace(file_change_store=object())
    team_store = MagicMock()
    team_store.initialized = True
    listener = SimpleNamespace(publish_change=MagicMock())
    team_run = SimpleNamespace(file_change_store=team_store, scope_listener=listener)
    ctx = _ctx(
        {
            "ci_service": svc,
            "team_run_id": "team-1",
            "agent_run_id": "agent-run-1",
            "agent_name": "developer",
        }
    )
    prepared = SimpleNamespace(
        file_path="/repo/file.py",
        current_content="before\n",
        current_hash="old-hash",
    )

    with patch("tools.core.ci_runtime._get_team_run", return_value=team_run):
        result = finalize_ci_write(
            ctx,
            prepared,
            content="after\n",
            edit_type="write",
            description="update file",
        )

    assert result.success is True
    team_store.record.assert_called_once_with(
        team_run_id="team-1",
        file_path="/repo/file.py",
        agent_id="developer",
        agent_run_id="agent-run-1",
        edit_type="write",
        old_hash="old-hash",
        new_hash=hashlib.sha256("after\n".encode("utf-8")).hexdigest()[:16],
        description="update file",
    )
    listener.publish_change.assert_called_once_with(
        file_path="/repo/file.py",
        agent_id="developer",
        agent_run_id="agent-run-1",
        edit_type="write",
    )


# ---------------------------------------------------------------------------
# abort_ci_write
# ---------------------------------------------------------------------------


def test_abort_ci_write_calls_service():
    svc = MagicMock()
    ctx = _ctx({"ci_service": svc})
    prepared = SimpleNamespace(file_path="/repo/file.py", token_id="tok-1")

    abort_ci_write(ctx, prepared)

    svc.abort_prepared_write.assert_called_once_with(prepared)


def test_abort_ci_write_noop_when_none():
    ctx = _ctx()
    abort_ci_write(ctx, None)  # should not raise


# ---------------------------------------------------------------------------
# record_edit_in_arbiter
# ---------------------------------------------------------------------------


def test_record_edit_no_service_is_noop():
    ctx = _ctx()
    record_edit_in_arbiter(ctx, "/file.py")  # should not raise


def test_record_edit_calls_arbiter():
    svc = MagicMock()
    ctx = _ctx({"ci_service": svc})
    record_edit_in_arbiter(
        ctx,
        "/file.py",
        agent_id="a1",
        edit_type="edit",
        old_hash="abc",
        new_hash="def",
        description="fix",
    )
    svc.arbiter.record_edit.assert_called_once_with(
        file_path="/file.py",
        agent_id="a1",
        edit_type="edit",
        old_hash="abc",
        new_hash="def",
        description="fix",
    )


def test_record_edit_default_args():
    svc = MagicMock()
    ctx = _ctx({"ci_service": svc})
    record_edit_in_arbiter(ctx, "/file.py")
    svc.arbiter.record_edit.assert_called_once_with(
        file_path="/file.py",
        agent_id="",
        edit_type="edit",
        old_hash="",
        new_hash="",
        description="",
    )


def test_record_edit_mirrors_team_store_and_scope_listener():
    svc = MagicMock()
    svc.arbiter.file_change_store = object()
    team_store = MagicMock()
    team_store.initialized = True
    listener = SimpleNamespace(publish_change=MagicMock())
    team_run = SimpleNamespace(file_change_store=team_store, scope_listener=listener)
    ctx = _ctx(
        {
            "ci_service": svc,
            "team_run_id": "team-1",
            "agent_run_id": "agent-run-1",
            "agent_name": "developer",
        }
    )

    with patch("tools.core.ci_runtime._get_team_run", return_value=team_run):
        record_edit_in_arbiter(ctx, "/file.py")

    svc.arbiter.record_edit.assert_called_once_with(
        file_path="/file.py",
        agent_id="developer",
        edit_type="edit",
        old_hash="",
        new_hash="",
        description="",
    )
    team_store.record.assert_called_once_with(
        team_run_id="team-1",
        file_path="/file.py",
        agent_id="developer",
        agent_run_id="agent-run-1",
        edit_type="edit",
        old_hash="",
        new_hash="",
        description="",
    )
    listener.publish_change.assert_called_once_with(
        file_path="/file.py",
        agent_id="developer",
        agent_run_id="agent-run-1",
        edit_type="edit",
    )


def test_record_edit_swallows_exceptions():
    svc = MagicMock()
    svc.arbiter.record_edit.side_effect = RuntimeError("boom")
    ctx = _ctx({"ci_service": svc})
    record_edit_in_arbiter(ctx, "/file.py")  # must not raise


# ---------------------------------------------------------------------------
# destructive_shell_command_error
# ---------------------------------------------------------------------------

import pytest
from tools.daytona_toolkit.ci_integration import destructive_shell_command_error


@pytest.mark.parametrize(
    "command",
    [
        "rm -rf /testbed/dask",
        "rm -rF /testbed",
        "rm --recursive /workspace/project",
        "mv /testbed/dask /tmp/trash",
        "mv /home/user /tmp",
        "mkfs.ext4 /dev/sda1",
        "dd if=/dev/zero of=/dev/sda",
        "rm -rf .",
        "echo ok; rm -rf /testbed/dask",
    ],
)
def test_destructive_shell_command_error_blocks(command):
    err = destructive_shell_command_error(command)
    assert err is not None, f"Should block: {command}"
    assert "BLOCKED" in err


@pytest.mark.parametrize(
    "command",
    [
        "rm /testbed/dask/file.py",
        "rm -f /testbed/dask/file.py",
        "mv /testbed/dask/file.py /testbed/dask/new.py",
        "cp -r /testbed/dask /testbed/backup",
        "pytest /testbed/dask/tests",
        "python -c 'import os'",
        "",
    ],
)
def test_destructive_shell_command_error_allows_safe(command):
    err = destructive_shell_command_error(command)
    assert err is None, f"Should allow: {command}"


def test_shell_mutation_declaration_error_blocks_destructive_unconditionally():
    """Destructive commands are blocked even outside team coordination mode."""
    from tools.daytona_toolkit.ci_integration import shell_mutation_declaration_error

    ctx = _ctx()  # no team mode metadata
    err = shell_mutation_declaration_error(
        ctx,
        command="rm -rf /testbed/dask",
        declared_output_paths=["/testbed/dask"],
    )
    assert err is not None
    assert "BLOCKED" in err
