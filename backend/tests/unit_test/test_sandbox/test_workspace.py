"""Tests for sandbox.workspace."""

from __future__ import annotations

import pytest
from unittest.mock import MagicMock, AsyncMock

from tools.core.base import ToolExecutionContextService


class TestDiscoverWorkspace:
    def test_returns_project_dir_when_present(self):
        from sandbox.provider.daytona.workspace import discover_workspace

        sandbox = MagicMock()
        sandbox.configure_mock(project_dir="/workspace/my-project")
        result = discover_workspace(sandbox)

        assert result == "/workspace/my-project"

    def test_falls_back_to_pwd(self):
        from sandbox.provider.daytona.workspace import discover_workspace

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
        from sandbox.provider.daytona.workspace import discover_workspace

        sandbox = MagicMock()
        sandbox.configure_mock(project_dir=None)
        exec_mock = MagicMock(side_effect=RuntimeError("broken"))
        sandbox.configure_mock(process=MagicMock(exec=exec_mock))

        result = discover_workspace(sandbox)

        assert result is None


class TestDiscoverWorkspaceAsync:
    @pytest.mark.anyio
    async def test_returns_project_dir_when_present(self):
        from sandbox.provider.daytona.workspace import discover_workspace_async

        sandbox = MagicMock()
        sandbox.configure_mock(project_dir="/workspace/my-project")

        result = await discover_workspace_async(sandbox)

        assert result == "/workspace/my-project"

    @pytest.mark.anyio
    async def test_falls_back_to_pwd(self):
        from sandbox.provider.daytona.workspace import discover_workspace_async

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
        from sandbox.provider.daytona.workspace import discover_workspace_async

        sandbox = MagicMock()
        sandbox.configure_mock(project_dir=None)
        exec_mock = AsyncMock(side_effect=RuntimeError("broken"))
        sandbox.configure_mock(process=MagicMock(exec=exec_mock))

        result = await discover_workspace_async(sandbox)

        assert result is None


class TestSandboxRuntimeContext:
    def test_sets_runtime_metadata(self):
        import sandbox.provider.daytona.workspace as workspace_module

        mock_context = ToolExecutionContextService(cwd="/tmp")
        mock_sandbox = MagicMock()

        workspace_module.prepare_sandbox_runtime_context(
            mock_context,
            sandbox=mock_sandbox,
            workspace_root="/workspace",
        )

        assert "daytona_sandbox" not in mock_context
        assert mock_context["repo_root"] == "/workspace"
        assert mock_context["exec_cwd"] == "/workspace"

    def test_respects_existing_repo_root(self):
        from sandbox.provider.daytona.workspace import prepare_sandbox_runtime_context

        mock_context = ToolExecutionContextService(
            cwd="/tmp",
            services={"repo_root": "/testbed"},
        )
        mock_sandbox = MagicMock()

        prepare_sandbox_runtime_context(
            mock_context,
            sandbox=mock_sandbox,
            workspace_root="/workspace",
        )

        assert mock_context["repo_root"] == "/testbed"
        assert mock_context["exec_cwd"] == "/testbed"


class TestProviderNeutralRuntimeContext:
    def test_context_runtime_does_not_attach_legacy_api_handles(self):
        from sandbox.provider.daytona.workspace import prepare_sandbox_runtime_context

        mock_context = ToolExecutionContextService(cwd="/tmp")

        prepare_sandbox_runtime_context(
            mock_context,
            sandbox=MagicMock(),
            workspace_root="/workspace",
        )

        assert mock_context.get("sandbox_api") is None
        assert mock_context.get("sandbox_transport") is None
