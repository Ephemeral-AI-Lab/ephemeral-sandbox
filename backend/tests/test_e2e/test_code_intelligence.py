# ruff: noqa
"""US-016: Code intelligence system integration tests.

Unit-level tests for CodeIntelligenceService creation, status,
telemetry, and the global service registry.
These do NOT require a live sandbox.
"""

from __future__ import annotations

import pytest

pytestmark = [pytest.mark.e2e]


# ---------------------------------------------------------------------------
# CI Service creation and initialization
# ---------------------------------------------------------------------------


class TestCIServiceCreation:
    """Test CodeIntelligenceService creation and basic functionality."""

    def test_ci_service_creation(self):
        """Create a CI service and verify basic attributes."""
        from sandbox.code_intelligence.service import CodeIntelligenceService

        svc = CodeIntelligenceService(
            sandbox_id="test-sandbox-001",
            workspace_root="/workspace",
            sandbox=None,
        )
        assert svc.sandbox_id == "test-sandbox-001"
        assert svc.workspace_root == "/workspace"
        assert svc.is_initialized is False

    def test_ci_service_status(self):
        """Verify status() returns expected fields."""
        from sandbox.code_intelligence.service import CodeIntelligenceService

        svc = CodeIntelligenceService(
            sandbox_id="test-status-001",
            workspace_root="/workspace",
        )
        status = svc.status()

        assert status["sandbox_id"] == "test-status-001"
        assert status["initialized"] is False
        assert status["workspace_root"] == "/workspace"
        assert "symbol_index" in status
        assert "arbiter" in status
        assert "lsp" in status

        # Symbol index details
        si = status["symbol_index"]
        assert "built" in si
        assert "files" in si
        assert "symbols" in si
        assert "generation" in si

        # LSP details
        lsp = status["lsp"]
        assert "connected" in lsp
        assert "queries" in lsp
        assert "cache_hits" in lsp

    def test_ci_telemetry(self):
        """Verify get_telemetry() returns CITelemetry with correct types."""
        from sandbox.code_intelligence.service import CodeIntelligenceService
        from sandbox.code_intelligence.core.types import CITelemetry

        svc = CodeIntelligenceService(
            sandbox_id="test-telemetry-001",
            workspace_root="/workspace",
        )
        tel = svc.get_telemetry()

        assert isinstance(tel, CITelemetry)
        assert isinstance(tel.symbol_index_size, int)
        assert isinstance(tel.symbol_index_generation, int)
        assert isinstance(tel.indexed_files, int)
        assert isinstance(tel.lsp_connected, bool)
        assert isinstance(tel.lsp_query_count, int)
        assert isinstance(tel.lsp_cache_hits, int)
        assert isinstance(tel.arbiter_active_locks, int)
        assert isinstance(tel.total_edits, int)

        # Initial counters should be zero; backend connectivity depends on the
        # local developer environment.
        assert tel.symbol_index_size == 0
        assert tel.total_edits == 0

    def test_ci_service_dispose(self):
        """Verify dispose cleans up without error."""
        from sandbox.code_intelligence.service import CodeIntelligenceService

        svc = CodeIntelligenceService(
            sandbox_id="test-dispose-001",
            workspace_root="/workspace",
        )
        # Should not raise
        svc.dispose()


# ---------------------------------------------------------------------------
# CI Service registry (singleton per sandbox)
# ---------------------------------------------------------------------------


class TestCIServiceRegistry:
    """Test the global CI service registry manages singletons correctly."""

    def setup_method(self):
        """Clean up the global registry before each test."""
        from sandbox.code_intelligence.service import dispose_all_code_intelligence

        dispose_all_code_intelligence()

    def teardown_method(self):
        """Clean up after each test."""
        from sandbox.code_intelligence.service import dispose_all_code_intelligence

        dispose_all_code_intelligence()

    def test_ci_service_registry_returns_singleton(self):
        """get_code_intelligence should return the same instance for the same sandbox_id."""
        from sandbox.code_intelligence.service import get_code_intelligence

        svc1 = get_code_intelligence("registry-test-001", "/workspace")
        svc2 = get_code_intelligence("registry-test-001", "/workspace")
        assert svc1 is svc2, "Should return the same instance"

    def test_ci_service_registry_different_sandboxes(self):
        """Different sandbox_ids should get different instances."""
        from sandbox.code_intelligence.service import get_code_intelligence

        svc1 = get_code_intelligence("sandbox-a", "/workspace")
        svc2 = get_code_intelligence("sandbox-b", "/workspace")
        assert svc1 is not svc2, "Different sandboxes should have different instances"
        assert svc1.sandbox_id == "sandbox-a"
        assert svc2.sandbox_id == "sandbox-b"

    def test_ci_service_if_exists(self):
        """get_code_intelligence_if_exists should return None if not created."""
        from sandbox.code_intelligence.service import (
            get_code_intelligence,
            get_code_intelligence_if_exists,
        )

        assert get_code_intelligence_if_exists("nonexistent") is None

        # Create one
        svc = get_code_intelligence("exists-test", "/workspace")
        found = get_code_intelligence_if_exists("exists-test")
        assert found is svc

    def test_ci_service_dispose_removes_from_registry(self):
        """dispose_code_intelligence should remove the service from the registry."""
        from sandbox.code_intelligence.service import (
            dispose_code_intelligence,
            get_code_intelligence,
            get_code_intelligence_if_exists,
        )

        get_code_intelligence("dispose-test", "/workspace")
        assert get_code_intelligence_if_exists("dispose-test") is not None

        dispose_code_intelligence("dispose-test")
        assert get_code_intelligence_if_exists("dispose-test") is None

    def test_ci_service_all_status(self):
        """get_all_services_status should return status for all services."""
        from sandbox.code_intelligence.service import (
            get_all_services_status,
            get_code_intelligence,
        )

        get_code_intelligence("status-a", "/workspace")
        get_code_intelligence("status-b", "/workspace")

        all_status = get_all_services_status()
        assert "status-a" in all_status
        assert "status-b" in all_status
        assert all_status["status-a"]["sandbox_id"] == "status-a"
        assert all_status["status-b"]["sandbox_id"] == "status-b"


# ---------------------------------------------------------------------------
# CI Types
# ---------------------------------------------------------------------------


class TestCITypes:
    """Test CI type constructors and fields."""

    def test_edit_request(self):
        """EditRequest should hold all required fields."""
        from sandbox.code_intelligence.core.types import EditRequest

        req = EditRequest(
            file_path="/workspace/test.py",
            old_text="def foo():",
            new_text="def bar():",
            agent_id="test-agent",
            description="Rename function",
        )
        assert req.file_path == "/workspace/test.py"
        assert req.old_text == "def foo():"
        assert req.new_text == "def bar():"
        assert req.agent_id == "test-agent"

    def test_edit_result(self):
        """EditResult should represent success/failure."""
        from sandbox.code_intelligence.core.types import EditResult

        success = EditResult(success=True, file_path="/test.py", message="OK")
        assert success.success is True
        assert success.file_path == "/test.py"

        failure = EditResult(success=False, file_path="/test.py", message="Conflict", conflict=True)
        assert failure.success is False
        assert failure.conflict is True

    def test_hover_result(self):
        """HoverResult should hold documentation text."""
        from sandbox.code_intelligence.core.types import HoverResult

        hr = HoverResult(content="def foo() -> int", language="python")
        assert hr.content == "def foo() -> int"
        assert hr.language == "python"

    def test_symbol_info(self):
        """SymbolInfo should hold location data."""
        from sandbox.code_intelligence.core.types import SymbolInfo

        si = SymbolInfo(
            name="MyClass",
            kind="class",
            file_path="/workspace/models.py",
            line=10,
            character=0,
        )
        assert si.name == "MyClass"
        assert si.kind == "class"
        assert si.line == 10

    def test_diagnostic(self):
        """Diagnostic should hold error/warning info."""
        from sandbox.code_intelligence.core.types import Diagnostic

        d = Diagnostic(
            file_path="/test.py",
            line=5,
            character=10,
            severity="error",
            message="Undefined variable 'x'",
            source="pyright",
        )
        assert d.severity == "error"
        assert d.message == "Undefined variable 'x'"
        assert d.source == "pyright"
