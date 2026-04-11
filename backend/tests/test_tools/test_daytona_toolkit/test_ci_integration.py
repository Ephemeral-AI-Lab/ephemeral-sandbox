"""Tests for tools.daytona_toolkit.ci_integration."""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock

from team.context.project import ProjectContext
from team.models import Briefing
from team.runtime.registry import register, unregister
from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit.ci_integration import (
    abort_ci_write,
    finalize_ci_write,
    get_ci_service,
    prepare_declared_shell_outputs,
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
            "scope_packet": {"scope_paths": ["/repo"], "coherence_token": "base-token"},
            "coherence_token": "base-token",
        }
    )

    result, packet, err = prepare_ci_write(ctx, "/repo/file.py")

    assert err is None
    assert result is prepared
    assert packet["coherence_token"] == "reserved-token"
    assert ctx.metadata["scope_packet"]["coherence_token"] == "reserved-token"
    assert ctx.metadata["coherence_token"] == "reserved-token"


def test_prepare_ci_write_allows_scope_drift_when_opted_in(monkeypatch):
    svc = MagicMock()
    prepared = SimpleNamespace(file_path="/repo/file.py", token_id="tok-1")
    svc.prepare_write.return_value = prepared
    packets = [
        {"scope_paths": ["src"], "coherence_token": "drifted-token"},
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
            "scope_packet": {"scope_paths": ["/repo"], "coherence_token": "base-token"},
            "coherence_token": "base-token",
        }
    )

    result, packet, err = prepare_ci_write(
        ctx,
        "/repo/file.py",
        allow_scope_drift=True,
    )

    assert err is None
    assert result is prepared
    assert packet["coherence_token"] == "reserved-token"
    assert ctx.metadata["scope_packet"]["coherence_token"] == "reserved-token"
    assert ctx.metadata["coherence_token"] == "reserved-token"


def test_prepare_ci_write_auto_expands_scope_for_adjacent_write(monkeypatch):
    svc = MagicMock()
    prepared = SimpleNamespace(file_path="/repo/tests/test_owned.py", token_id="tok-1")
    svc.prepare_write.return_value = prepared
    packets = [
        {
            "scope_paths": ["/repo/src/owned.py", "/repo/tests/test_owned.py"],
            "coherence_token": "base-token",
        },
        {
            "scope_paths": ["/repo/src/owned.py", "/repo/tests/test_owned.py"],
            "coherence_token": "reserved-token",
        },
    ]
    monkeypatch.setattr(
        "tools.daytona_toolkit.ci_integration.build_scope_packet_for_context",
        lambda *args, **kwargs: dict(packets.pop(0)),
    )
    ctx = _ctx(
        {
            "ci_service": svc,
            "scope_packet": {"scope_paths": ["/repo/src/owned.py"], "coherence_token": "base-token"},
            "coherence_token": "base-token",
            "agent_run_id": "worker-1",
        }
    )

    result, packet, err = prepare_ci_write(ctx, "/repo/tests/test_owned.py")

    assert err is None
    assert result is prepared
    assert packet["scope_paths"] == ["/repo/src/owned.py", "/repo/tests/test_owned.py"]
    assert ctx.metadata["scope_packet"]["scope_paths"] == [
        "/repo/src/owned.py",
        "/repo/tests/test_owned.py",
    ]
    svc.prepare_write.assert_called_once_with(
        "/repo/tests/test_owned.py",
        agent_id="worker-1",
        expected_hash="",
        allow_missing=True,
    )


def test_prepare_declared_shell_outputs_allows_scope_drift(monkeypatch):
    svc = MagicMock()
    prepared = SimpleNamespace(file_path="/repo/new.py", token_id="tok-1")
    svc.prepare_write.return_value = prepared
    packets = [
        {"scope_paths": ["/repo/new.py"], "coherence_token": "drifted-token"},
        {"scope_paths": ["/repo/new.py"], "coherence_token": "reserved-token"},
        {"scope_paths": ["/repo/new.py"], "coherence_token": "reserved-token"},
    ]
    monkeypatch.setattr(
        "tools.daytona_toolkit.ci_integration.build_scope_packet_for_context",
        lambda *args, **kwargs: dict(packets.pop(0)),
    )
    ctx = _ctx(
        {
            "ci_service": svc,
            "agent_run_id": "worker-1",
            "scope_packet": {"scope_paths": ["/repo/new.py"], "coherence_token": "base-token"},
            "coherence_token": "base-token",
        }
    )

    prepared_items, packet, err = prepare_declared_shell_outputs(
        ctx,
        declared_output_paths=["/repo/new.py"],
    )

    assert err is None
    assert prepared_items == [prepared]
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


def test_finalize_ci_write_enriches_prepared_write_with_symbol_boundaries(monkeypatch):
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


def test_build_scope_packet_includes_shared_context_summary_and_tracks_freshness():
    pc = ProjectContext(goal="g", user_request="u", repo_root="/repo")
    pc.shared_briefings["src/auth"] = Briefing(name="src/auth", source="inline", inline="runtime note")
    pc.shared_briefing_meta["src/auth"] = {
        "kind": "runtime",
        "provenance": "manual-inline",
        "stale_on_write": True,
        "scope_paths": ["src/auth"],
        "repo_epoch": 0,
        "scope_write_epoch": 0,
        "render_count": 2,
        "consumer_lane_ids": {"dev-a"},
        "consumer_roles": {"developer"},
    }
    team_run = SimpleNamespace(project_context=pc)

    first = build_scope_packet(scope_paths=["src/auth/service.py"], team_run=team_run)

    assert first["shared_context"] == [
        {
            "scope": "src/auth",
            "kind": "runtime",
            "provenance": "manual-inline",
            "freshness": "fresh",
            "consumer_count": 1,
            "render_count": 2,
            "scope_write_epoch": 0,
        }
    ]

    pc.repo_epoch = 1
    pc.scope_write_epochs["src/auth"] = 1
    second = build_scope_packet(
        scope_paths=["src/auth/service.py"],
        team_run=team_run,
        baseline_packet=first,
    )

    assert second["shared_context"][0]["freshness"] == "caution"
    assert second["coherence_token"] != first["coherence_token"]


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
