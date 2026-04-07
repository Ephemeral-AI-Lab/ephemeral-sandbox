"""Tests for tools.daytona_toolkit.ci_integration."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

import pytest

from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit.ci_integration import (
    get_ci_service,
    prime_cache_after_write,
    record_edit_in_ledger,
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
    svc.tree_cache.put_content.assert_called_once_with("/file.py", "hello")
    svc.symbol_index.refresh.assert_called_once_with("/file.py", "hello")
    svc.lsp_client.invalidate.assert_called_once_with("/file.py")


def test_prime_cache_swallows_exceptions():
    svc = MagicMock()
    svc.tree_cache.put_content.side_effect = RuntimeError("boom")
    ctx = _ctx({"ci_service": svc})
    prime_cache_after_write(ctx, "/file.py", "hello")  # must not raise


# ---------------------------------------------------------------------------
# record_edit_in_ledger
# ---------------------------------------------------------------------------

def test_record_edit_no_service_is_noop():
    ctx = _ctx()
    record_edit_in_ledger(ctx, "/file.py")  # should not raise


def test_record_edit_calls_ledger():
    svc = MagicMock()
    ctx = _ctx({"ci_service": svc})
    record_edit_in_ledger(
        ctx, "/file.py", agent_id="a1", edit_type="edit",
        old_hash="abc", new_hash="def", description="fix",
    )
    svc.ledger.record.assert_called_once_with(
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
    record_edit_in_ledger(ctx, "/file.py")
    svc.ledger.record.assert_called_once_with(
        file_path="/file.py",
        agent_id="",
        edit_type="edit",
        old_hash="",
        new_hash="",
        description="",
    )


def test_record_edit_swallows_exceptions():
    svc = MagicMock()
    svc.ledger.record.side_effect = RuntimeError("boom")
    ctx = _ctx({"ci_service": svc})
    record_edit_in_ledger(ctx, "/file.py")  # must not raise
