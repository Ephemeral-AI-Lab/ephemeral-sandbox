"""Tests for sandbox.workspace."""

from __future__ import annotations

import pytest
from unittest.mock import MagicMock, AsyncMock


class TestDiscoverWorkspace:
    def test_returns_project_dir_when_present(self):
        from sandbox.workspace import discover_workspace

        sandbox = MagicMock()
        sandbox.configure_mock(project_dir="/workspace/my-project")
        result = discover_workspace(sandbox)

        assert result == "/workspace/my-project"

    def test_falls_back_to_pwd(self):
        from sandbox.workspace import discover_workspace

        sandbox = MagicMock()
        sandbox.configure_mock(project_dir=None)
        resp = MagicMock()
        resp.configure_mock(exit_code=0, result="/home/daytona\n")
        exec_mock = MagicMock(return_value=resp)
        sandbox.configure_mock(process=MagicMock(exec=exec_mock))

        result = discover_workspace(sandbox)

        assert result == "/home/daytona"
        exec_mock.assert_called_once_with("pwd")

    def test_returns_none_when_both_fail(self):
        from sandbox.workspace import discover_workspace

        sandbox = MagicMock()
        sandbox.configure_mock(project_dir=None)
        exec_mock = MagicMock(side_effect=RuntimeError("broken"))
        sandbox.configure_mock(process=MagicMock(exec=exec_mock))

        result = discover_workspace(sandbox)

        assert result is None


class TestDiscoverWorkspaceAsync:
    @pytest.mark.anyio
    async def test_returns_project_dir_when_present(self):
        from sandbox.workspace import discover_workspace_async

        sandbox = MagicMock()
        sandbox.configure_mock(project_dir="/workspace/my-project")

        result = await discover_workspace_async(sandbox)

        assert result == "/workspace/my-project"

    @pytest.mark.anyio
    async def test_falls_back_to_pwd(self):
        from sandbox.workspace import discover_workspace_async

        sandbox = MagicMock()
        sandbox.configure_mock(project_dir=None)
        resp = MagicMock()
        resp.configure_mock(exit_code=0, result="/home/daytona\n")
        exec_mock = AsyncMock(return_value=resp)
        sandbox.configure_mock(process=MagicMock(exec=exec_mock))

        result = await discover_workspace_async(sandbox)

        assert result == "/home/daytona"

    @pytest.mark.anyio
    async def test_returns_none_when_both_fail(self):
        from sandbox.workspace import discover_workspace_async

        sandbox = MagicMock()
        sandbox.configure_mock(project_dir=None)
        exec_mock = AsyncMock(side_effect=RuntimeError("broken"))
        sandbox.configure_mock(process=MagicMock(exec=exec_mock))

        result = await discover_workspace_async(sandbox)

        assert result is None


class TestInjectCodeIntelligence:
    def test_injects_ci_service(self, monkeypatch):
        from sandbox.workspace import inject_code_intelligence

        mock_context = MagicMock()
        mock_context.metadata = {}
        mock_sandbox = MagicMock()
        mock_svc = MagicMock()

        def fake_get_ci(sandbox_id, workspace_root, sandbox):
            return mock_svc

        import sys
        import types

        fake_ci_module = types.ModuleType("code_intelligence.routing.service")
        fake_ci_module.get_code_intelligence = fake_get_ci
        monkeypatch.setitem(sys.modules, "code_intelligence.routing.service", fake_ci_module)

        inject_code_intelligence(mock_context, "sb-123", mock_sandbox, "/workspace")

        assert mock_context.metadata["ci_service"] == mock_svc

    def test_prefers_sandbox_project_dir_for_ci_workspace(self, monkeypatch):
        from sandbox.workspace import inject_code_intelligence

        mock_context = MagicMock()
        mock_context.metadata = {}
        mock_sandbox = MagicMock()
        mock_sandbox.configure_mock(project_dir="/testbed")
        mock_svc = MagicMock()
        captured = {}

        def fake_get_ci(sandbox_id, workspace_root, sandbox):
            captured["workspace_root"] = workspace_root
            return mock_svc

        import sys
        import types

        fake_ci_module = types.ModuleType("code_intelligence.routing.service")
        fake_ci_module.get_code_intelligence = fake_get_ci
        monkeypatch.setitem(sys.modules, "code_intelligence.routing.service", fake_ci_module)

        inject_code_intelligence(mock_context, "sb-123", mock_sandbox, "/workspace")

        assert captured["workspace_root"] == "/testbed"
        mock_svc.ensure_initialized.assert_called_once_with(wait=True)

    def test_skips_when_ci_import_fails(self, monkeypatch):
        from sandbox.workspace import inject_code_intelligence

        mock_context = MagicMock()
        mock_context.metadata = {}
        mock_sandbox = MagicMock()

        import sys

        monkeypatch.setitem(sys.modules, "code_intelligence.routing.service", None)

        inject_code_intelligence(mock_context, "sb-123", mock_sandbox, "/workspace")

        assert "ci_service" not in mock_context.metadata

    def test_uses_sync_handle_for_async_remote_sandbox_prewarm(self, monkeypatch):
        from sandbox.workspace import inject_code_intelligence

        mock_context = MagicMock()
        mock_context.metadata = {}
        async_sandbox = MagicMock()
        async_sandbox.process = MagicMock(exec=AsyncMock())
        sync_sandbox = MagicMock()
        mock_svc = MagicMock()
        mock_svc.lsp_client = MagicMock()
        captured = {}

        def fake_get_ci(sandbox_id, workspace_root, sandbox):
            captured["sandbox"] = sandbox
            return mock_svc

        import sys
        import types

        fake_ci_module = types.ModuleType("code_intelligence.routing.service")
        fake_ci_module.get_code_intelligence = fake_get_ci
        monkeypatch.setitem(sys.modules, "code_intelligence.routing.service", fake_ci_module)

        class FakeSandboxService:
            def get_sandbox_object(self, sandbox_id):
                return sync_sandbox

        fake_service_module = types.ModuleType("sandbox.service")
        fake_service_module.SandboxService = FakeSandboxService
        monkeypatch.setitem(sys.modules, "sandbox.service", fake_service_module)

        inject_code_intelligence(
            mock_context,
            "sb-123",
            async_sandbox,
            "/definitely-not-a-local-workspace",
        )

        assert mock_context.metadata["ci_service"] == mock_svc
        assert captured["sandbox"] is sync_sandbox
        mock_svc.ensure_initialized.assert_called_once_with(wait=True)
        mock_svc.lsp_client.ensure_ready.assert_not_called()

    def test_skips_eager_warmup_when_async_remote_sandbox_has_no_sync_handle(self, monkeypatch):
        from sandbox.workspace import inject_code_intelligence

        mock_context = MagicMock()
        mock_context.metadata = {}
        async_sandbox = MagicMock()
        async_sandbox.process = MagicMock(exec=AsyncMock())
        mock_svc = MagicMock()
        mock_svc.lsp_client = MagicMock()
        captured = {}

        def fake_get_ci(sandbox_id, workspace_root, sandbox):
            captured["sandbox"] = sandbox
            return mock_svc

        import sys
        import types

        fake_ci_module = types.ModuleType("code_intelligence.routing.service")
        fake_ci_module.get_code_intelligence = fake_get_ci
        monkeypatch.setitem(sys.modules, "code_intelligence.routing.service", fake_ci_module)

        class FakeSandboxService:
            def get_sandbox_object(self, sandbox_id):
                raise RuntimeError("sync handle unavailable")

        fake_service_module = types.ModuleType("sandbox.service")
        fake_service_module.SandboxService = FakeSandboxService
        monkeypatch.setitem(sys.modules, "sandbox.service", fake_service_module)

        inject_code_intelligence(
            mock_context,
            "sb-123",
            async_sandbox,
            "/definitely-not-a-local-workspace",
        )

        assert mock_context.metadata["ci_service"] == mock_svc
        assert captured["sandbox"] is async_sandbox
        # Full ensure_initialized is NOT called (LSP bootstrap unsafe),
        # but the symbol index background build IS started eagerly.
        mock_svc.ensure_initialized.assert_not_called()
        mock_svc.lsp_client.ensure_ready.assert_not_called()
        mock_svc.symbol_index.ensure_built.assert_called_once_with(wait=False)

    def test_async_sandbox_symbol_index_start_failure_is_silent(self, monkeypatch):
        """If ensure_built raises when starting background build, it is swallowed."""
        from sandbox.workspace import inject_code_intelligence

        mock_context = MagicMock()
        mock_context.metadata = {}
        async_sandbox = MagicMock()
        async_sandbox.process = MagicMock(exec=AsyncMock())
        mock_svc = MagicMock()
        mock_svc.lsp_client = MagicMock()
        mock_svc.symbol_index.ensure_built.side_effect = RuntimeError("boom")

        def fake_get_ci(sandbox_id, workspace_root, sandbox):
            return mock_svc

        import sys
        import types

        fake_ci_module = types.ModuleType("code_intelligence.routing.service")
        fake_ci_module.get_code_intelligence = fake_get_ci
        monkeypatch.setitem(sys.modules, "code_intelligence.routing.service", fake_ci_module)

        class FakeSandboxService:
            def get_sandbox_object(self, sandbox_id):
                raise RuntimeError("sync handle unavailable")

        fake_service_module = types.ModuleType("sandbox.service")
        fake_service_module.SandboxService = FakeSandboxService
        monkeypatch.setitem(sys.modules, "sandbox.service", fake_service_module)

        # Should not raise
        inject_code_intelligence(
            mock_context, "sb-123", async_sandbox, "/workspace",
        )
        assert mock_context.metadata["ci_service"] == mock_svc
