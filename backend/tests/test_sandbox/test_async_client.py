"""Tests for sandbox.async_client."""

from __future__ import annotations

import asyncio

import pytest
from unittest.mock import AsyncMock, MagicMock, patch


class TestGetAsyncSandbox:
    @pytest.mark.anyio
    async def test_fetch_sandbox(self, monkeypatch):
        monkeypatch.setenv("DAYTONA_API_KEY", "async-key")
        monkeypatch.setenv("DAYTONA_API_URL", "https://async-url")

        mock_client = MagicMock()
        mock_sandbox = MagicMock()
        mock_client.get = AsyncMock(return_value=mock_sandbox)

        import sandbox.providers.daytona.client.async_ as mod

        loop = asyncio.get_running_loop()
        monkeypatch.setattr(mod, "_load_credentials", lambda: ("async-key", "https://async-url", ""))
        with mod._client_lock:
            mod._cached_clients.clear()
            mod._cached_clients[loop] = (("async-key", "https://async-url", ""), mock_client)

        result = await mod.get_async_sandbox("sb-async-123")

        assert result == mock_sandbox
        mock_client.get.assert_awaited_once_with("sb-async-123")

    @pytest.mark.anyio
    async def test_fetch_sandbox_not_found(self, monkeypatch):
        monkeypatch.setenv("DAYTONA_API_KEY", "async-key")
        monkeypatch.setenv("DAYTONA_API_URL", "https://async-url")

        mock_client = MagicMock()
        mock_client.get = AsyncMock(return_value=None)

        import sandbox.providers.daytona.client.async_ as mod

        loop = asyncio.get_running_loop()
        monkeypatch.setattr(mod, "_load_credentials", lambda: ("async-key", "https://async-url", ""))
        with mod._client_lock:
            mod._cached_clients.clear()
            mod._cached_clients[loop] = (("async-key", "https://async-url", ""), mock_client)

        with pytest.raises(ValueError, match="not found"):
            await mod.get_async_sandbox("sb-nonexistent")

    @pytest.mark.anyio
    async def test_fetch_sandbox_recovers_once_when_initial_lookup_missing(self, monkeypatch):
        monkeypatch.setenv("DAYTONA_API_KEY", "async-key")
        monkeypatch.setenv("DAYTONA_API_URL", "https://async-url")

        recovered = MagicMock()
        mock_client = MagicMock()
        mock_client.get = AsyncMock(side_effect=[None, recovered])

        import sandbox.providers.daytona.client.async_ as mod

        loop = asyncio.get_running_loop()
        monkeypatch.setattr(mod, "_load_credentials", lambda: ("async-key", "https://async-url", ""))
        with mod._client_lock:
            mod._cached_clients.clear()
            mod._cached_clients[loop] = (("async-key", "https://async-url", ""), mock_client)

        with patch(
            "sandbox.providers.daytona.lifecycle.SandboxService.ensure_sandbox_running"
        ) as ensure_mock:
            result = await mod.get_async_sandbox("sb-recoverable")

        assert result is recovered
        ensure_mock.assert_called_once_with("sb-recoverable")
        assert mock_client.get.await_count == 2
