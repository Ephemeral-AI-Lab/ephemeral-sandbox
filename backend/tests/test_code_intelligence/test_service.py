"""Unit tests for code intelligence service lifecycle and edits."""

from __future__ import annotations

import time
from types import SimpleNamespace
from unittest.mock import MagicMock

import pytest

from code_intelligence.analysis.symbol_index import SymbolIndex
from code_intelligence.routing.service import (
    CodeIntelligenceService,
    dispose_all_code_intelligence,
    get_code_intelligence,
    get_code_intelligence_if_exists,
)
from code_intelligence.types import EditRequest
from server.routers.code_intelligence import initialize


@pytest.fixture(autouse=True)
def _clear_registry() -> None:
    dispose_all_code_intelligence()
    yield
    dispose_all_code_intelligence()


def test_apply_edit_sandbox_upload_uses_content_then_path(tmp_path) -> None:
    file_path = tmp_path / "sample.py"
    file_path.write_text("value = 1\n", encoding="utf-8")

    sandbox = SimpleNamespace(fs=MagicMock())
    sandbox.fs.download_file.return_value = b"value = 1\n"
    svc = CodeIntelligenceService(
        sandbox_id="sandbox-edit",
        workspace_root=str(tmp_path),
        sandbox=sandbox,
    )

    result = svc.apply_edit(
        EditRequest(
            file_path=str(file_path),
            old_text="value = 1",
            new_text="value = 2",
            description="bump value",
        )
    )

    assert result.success is True
    sandbox.fs.upload_file.assert_called_once_with(
        b"value = 2\n",
        str(file_path),
    )


def test_prepare_write_allows_multiple_same_file_reservations(tmp_path) -> None:
    file_path = tmp_path / "sample.py"
    file_path.write_text("value = 1\n", encoding="utf-8")

    svc = CodeIntelligenceService(
        sandbox_id="sandbox-reserve",
        workspace_root=str(tmp_path),
    )

    first = svc.prepare_write(str(file_path), agent_id="agent-a")
    second = svc.prepare_write(str(file_path), agent_id="agent-b")

    assert first.file_path == str(file_path)
    assert second.file_path == str(file_path)
    assert first.token_id != second.token_id

    svc.abort_prepared_write(first)
    svc.abort_prepared_write(second)


def test_commit_prepared_write_merges_disjoint_same_file_edits(tmp_path) -> None:
    file_path = tmp_path / "multi.py"
    original = (
        "# Region A\n"
        "def region_a():\n"
        "    return 'original-A'\n"
        "\n"
        "# Region B\n"
        "def region_b():\n"
        "    return 'original-B'\n"
    )
    file_path.write_text(original, encoding="utf-8")

    svc = CodeIntelligenceService(
        sandbox_id="sandbox-merge",
        workspace_root=str(tmp_path),
    )

    prepared_a = svc.prepare_write(str(file_path), agent_id="agent-a")
    prepared_b = svc.prepare_write(str(file_path), agent_id="agent-b")

    result_a = svc.commit_prepared_write(
        prepared_a,
        original.replace("'original-A'", "'modified-A'"),
        edit_type="edit",
        description="edit region a",
    )
    result_b = svc.commit_prepared_write(
        prepared_b,
        original.replace("'original-B'", "'modified-B'"),
        edit_type="edit",
        description="edit region b",
    )

    assert result_a.success is True
    assert result_b.success is True
    final = file_path.read_text(encoding="utf-8")
    assert "'modified-A'" in final
    assert "'modified-B'" in final


def test_commit_prepared_write_rejects_overlapping_same_file_edits(tmp_path) -> None:
    file_path = tmp_path / "multi.py"
    original = (
        "# Region A\n"
        "def region_a():\n"
        "    return 'original-A'\n"
        "\n"
        "# Region B\n"
        "def region_b():\n"
        "    return 'original-B'\n"
    )
    file_path.write_text(original, encoding="utf-8")

    svc = CodeIntelligenceService(
        sandbox_id="sandbox-overlap",
        workspace_root=str(tmp_path),
    )

    prepared_a = svc.prepare_write(str(file_path), agent_id="agent-a")
    prepared_b = svc.prepare_write(str(file_path), agent_id="agent-b")

    result_a = svc.commit_prepared_write(
        prepared_a,
        original.replace("'original-A'", "'modified-A'"),
        edit_type="edit",
        description="first edit",
    )
    result_b = svc.commit_prepared_write(
        prepared_b,
        original.replace("'original-A'", "'modified-B'"),
        edit_type="edit",
        description="overlapping edit",
    )

    assert result_a.success is True
    assert result_b.success is False
    assert result_b.conflict is True
    assert result_b.conflict_reason == "overlapping_range"
    assert "'modified-B'" not in file_path.read_text(encoding="utf-8")
    svc.abort_prepared_write(prepared_b)


def test_refresh_prepared_write_reissues_token_after_file_change(tmp_path) -> None:
    file_path = tmp_path / "sample.py"
    file_path.write_text("value = 1\n", encoding="utf-8")

    svc = CodeIntelligenceService(
        sandbox_id="sandbox-refresh",
        workspace_root=str(tmp_path),
    )

    prepared = svc.prepare_write(str(file_path), agent_id="agent-a")
    original_token = prepared.token_id

    file_path.write_text("value = 2\n", encoding="utf-8")
    refreshed = svc.refresh_prepared_write(prepared)

    assert refreshed.token_id != original_token
    assert refreshed.current_content == "value = 2\n"
    assert refreshed.current_hash != prepared.current_hash


def test_service_publishes_and_releases_edit_intents(tmp_path) -> None:
    file_path = tmp_path / "sample.py"
    file_path.write_text("value = 1\n", encoding="utf-8")

    svc = CodeIntelligenceService(
        sandbox_id="sandbox-intent",
        workspace_root=str(tmp_path),
    )

    intent_id = svc.publish_edit_intent(
        filepath=str(file_path),
        agent_id="agent-a",
        symbols=["Example.method"],
        scope="symbol",
    )
    packet = svc.scope_status([str(file_path)])

    assert packet["active_edit_intents"][0]["intent_id"] == intent_id
    assert packet["active_edit_intents"][0]["scope"] == "symbol"
    assert packet["active_edit_intents"][0]["symbols"] == ["Example.method"]

    assert svc.heartbeat_edit_intent(intent_id) is True
    svc.release_edit_intent(intent_id)
    packet = svc.scope_status([str(file_path)])
    assert packet["active_edit_intents"] == []


def test_get_code_intelligence_recreates_service_when_workspace_root_changes() -> None:
    first = get_code_intelligence("sandbox-reinit", "/tmp/first")
    second = get_code_intelligence("sandbox-reinit", "/tmp/second")

    assert second is not first
    assert get_code_intelligence_if_exists("sandbox-reinit") is second
    assert second.workspace_root == "/tmp/second"
    assert second.symbol_index._workspace_root == "/tmp/second"
    assert second.lsp_client._workspace_root == "/tmp/second"


def test_get_code_intelligence_rebind_resets_lsp_backend_cache_for_new_sandbox() -> None:
    first_sandbox = SimpleNamespace(name="first")
    second_sandbox = SimpleNamespace(name="second")

    service = get_code_intelligence("sandbox-rebind", "/tmp/workspace", sandbox=first_sandbox)
    service.lsp_client._py_available = False
    service.lsp_client._ts_available = False

    rebound = get_code_intelligence("sandbox-rebind", "/tmp/workspace", sandbox=second_sandbox)

    assert rebound is service
    assert rebound.lsp_client._sandbox is second_sandbox
    assert rebound.lsp_client._py_available is None
    assert rebound.lsp_client._ts_available is None


@pytest.mark.asyncio
async def test_initialize_endpoint_passes_requested_workspace_root(monkeypatch) -> None:
    calls: list[tuple[str, str]] = []

    fake_service = SimpleNamespace(
        workspace_root="",
        ensure_initialized=lambda wait=True: True,
    )

    def fake_get_code_intelligence(
        sandbox_id: str,
        workspace_root: str = "/workspace",
        sandbox=None,
    ):
        calls.append((sandbox_id, workspace_root))
        return fake_service

    monkeypatch.setattr(
        "code_intelligence.routing.service.get_code_intelligence",
        fake_get_code_intelligence,
    )

    result = await initialize("sandbox-init", workspace_root="/tmp/project")

    assert result == {"sandbox_id": "sandbox-init", "initialized": True}
    assert calls == [("sandbox-init", "/tmp/project")]


def test_symbol_index_missing_root_unblocks_waiters() -> None:
    idx = SymbolIndex("/tmp/definitely-missing-symbol-index-root")

    ready = idx.ensure_built(wait=True, timeout=0.05)
    time.sleep(0.01)

    assert ready is False
    assert idx._building is False
    assert idx._build_event.is_set() is True


def test_symbol_index_returns_symbol_boundaries_for_python_symbols(tmp_path) -> None:
    file_path = tmp_path / "sample.py"
    content = (
        "class Example:\n"
        "    def method(self):\n"
        "        return 1\n"
    )
    file_path.write_text(content, encoding="utf-8")
    idx = SymbolIndex(str(tmp_path))

    idx.refresh(str(file_path), content)
    boundaries = idx.symbol_boundaries_for_file(str(file_path))

    assert ("Example", 1, 3) in boundaries
    assert ("Example.method", 2, 3) in boundaries
