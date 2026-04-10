"""Tests for tools.daytona_toolkit.ci_integration."""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock

from team.runtime.registry import register, unregister
from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit.ci_integration import (
    abort_ci_write,
    finalize_ci_write,
    get_ci_service,
    prepare_ci_write,
    prime_cache_after_write,
    record_edit_in_ledger,
)
from tools.daytona_toolkit.coordination import build_scope_packet


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


def test_prime_cache_marks_atlas_dirty_for_team_run():
    seen: list[tuple[str, str]] = []
    register(
        SimpleNamespace(
            id="T1",
            note_atlas_edit=lambda path, reason="edit": seen.append((path, reason)),
        )
    )
    try:
        ctx = _ctx({"team_run_id": "T1"})
        prime_cache_after_write(ctx, "/repo/file.py", "hello")
        assert seen == [("/repo/file.py", "write")]
    finally:
        unregister("T1")


# ---------------------------------------------------------------------------
# finalize_ci_write
# ---------------------------------------------------------------------------


def test_finalize_ci_write_marks_atlas_dirty_for_team_run():
    seen: list[tuple[str, str]] = []
    svc = MagicMock()
    svc.commit_prepared_write.return_value = SimpleNamespace(success=True)
    register(
        SimpleNamespace(
            id="T1",
            note_atlas_edit=lambda path, reason="edit": seen.append((path, reason)),
        )
    )
    try:
        ctx = _ctx({"ci_service": svc, "team_run_id": "T1"})
        prepared = SimpleNamespace(file_path="/repo/file.py")
        result = finalize_ci_write(
            ctx,
            prepared,
            content="hello",
            edit_type="write",
            description="desc",
        )
        assert result.success is True
        assert seen == [("/repo/file.py", "write")]
    finally:
        unregister("T1")


def test_finalize_ci_write_skips_atlas_dirty_mark_on_failed_commit():
    seen: list[tuple[str, str]] = []
    svc = MagicMock()
    svc.commit_prepared_write.return_value = SimpleNamespace(success=False)
    register(
        SimpleNamespace(
            id="T1",
            note_atlas_edit=lambda path, reason="edit": seen.append((path, reason)),
        )
    )
    try:
        ctx = _ctx({"ci_service": svc, "team_run_id": "T1"})
        prepared = SimpleNamespace(file_path="/repo/file.py")
        result = finalize_ci_write(
            ctx,
            prepared,
            content="hello",
            edit_type="edit",
            description="desc",
        )
        assert result.success is False
        assert seen == []
    finally:
        unregister("T1")


def test_prepare_ci_write_refreshes_scope_baseline_after_reservation(monkeypatch):
    svc = MagicMock()
    prepared = SimpleNamespace(file_path="/repo/file.py", token_id="tok-1")
    svc.prepare_write.return_value = prepared
    packets = [
        {"scope_paths": ["src"], "coherence_token": "base-token"},
        {"scope_paths": ["src"], "coherence_token": "reserved-token"},
    ]
    monkeypatch.setattr(
        "tools.daytona_toolkit.ci_integration.build_scope_packet_for_context",
        lambda *args, **kwargs: dict(packets.pop(0)),
    )
    ctx = _ctx(
        {
            "ci_service": svc,
            "agent_run_id": "worker-1",
            "scope_packet": {"scope_paths": ["src"], "coherence_token": "base-token"},
            "coherence_token": "base-token",
        }
    )

    result, packet, err = prepare_ci_write(ctx, "/repo/file.py")

    assert err is None
    assert result is prepared
    assert packet["coherence_token"] == "reserved-token"
    assert ctx.metadata["scope_packet"]["coherence_token"] == "reserved-token"
    assert ctx.metadata["coherence_token"] == "reserved-token"


def test_abort_ci_write_refreshes_scope_baseline_after_release(monkeypatch):
    svc = MagicMock()
    packets = [{"scope_paths": ["src"], "coherence_token": "released-token"}]
    monkeypatch.setattr(
        "tools.daytona_toolkit.ci_integration.build_scope_packet_for_context",
        lambda *args, **kwargs: dict(packets.pop(0)),
    )
    ctx = _ctx(
        {
            "ci_service": svc,
            "scope_packet": {"scope_paths": ["src"], "coherence_token": "reserved-token"},
            "coherence_token": "reserved-token",
        }
    )
    prepared = SimpleNamespace(file_path="/repo/file.py", token_id="tok-1")

    abort_ci_write(ctx, prepared)

    svc.abort_prepared_write.assert_called_once_with(prepared)
    assert ctx.metadata["scope_packet"]["coherence_token"] == "released-token"
    assert ctx.metadata["coherence_token"] == "released-token"


def test_finalize_ci_write_refreshes_scope_baseline_after_commit(monkeypatch):
    svc = MagicMock()
    svc.commit_prepared_write.return_value = SimpleNamespace(success=True)
    packets = [{"scope_paths": ["src"], "coherence_token": "after-commit"}]
    monkeypatch.setattr(
        "tools.daytona_toolkit.ci_integration.build_scope_packet_for_context",
        lambda *args, **kwargs: dict(packets.pop(0)),
    )
    ctx = _ctx(
        {
            "ci_service": svc,
            "scope_packet": {"scope_paths": ["src"], "coherence_token": "reserved-token"},
            "coherence_token": "reserved-token",
        }
    )
    prepared = SimpleNamespace(file_path="/repo/file.py")

    result = finalize_ci_write(
        ctx,
        prepared,
        content="hello",
        edit_type="write",
        description="desc",
    )

    assert result.success is True
    assert ctx.metadata["scope_packet"]["coherence_token"] == "after-commit"
    assert ctx.metadata["coherence_token"] == "after-commit"


def test_build_scope_packet_coherence_ignores_unrelated_global_generation_changes():
    svc = MagicMock()
    svc.ledger.generation = 1
    svc.ledger.recent_entries.return_value = []
    svc.arbiter.generation = 2
    svc.arbiter.active_reservations.return_value = []
    svc.arbiter.hotspots.return_value = []
    svc.symbol_index.generation = 3

    first = build_scope_packet(scope_paths=["src/app.py"], svc=svc)

    svc.ledger.generation = 11
    svc.arbiter.generation = 12
    svc.symbol_index.generation = 13
    second = build_scope_packet(scope_paths=["src/app.py"], svc=svc, baseline_packet=first)

    assert first["coherence_token"] == second["coherence_token"]
    assert second["freshness"] == "fresh"


def test_build_scope_packet_coherence_changes_when_scope_local_changes_change():
    svc = MagicMock()
    svc.ledger.generation = 1
    svc.arbiter.generation = 2
    svc.arbiter.active_reservations.return_value = []
    svc.arbiter.hotspots.return_value = []
    svc.symbol_index.generation = 3
    svc.ledger.recent_entries.return_value = []

    first = build_scope_packet(scope_paths=["src/app.py"], svc=svc)

    svc.ledger.recent_entries.return_value = [
        SimpleNamespace(
            file_path="src/app.py",
            agent_id="worker-2",
            timestamp=123.0,
            edit_type="edit",
        )
    ]
    second = build_scope_packet(scope_paths=["src/app.py"], svc=svc, baseline_packet=first)

    assert first["coherence_token"] != second["coherence_token"]
    assert second["freshness"] == "touched"


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
