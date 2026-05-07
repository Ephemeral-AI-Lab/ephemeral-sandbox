"""Tests for sandbox.async_client."""

from __future__ import annotations

import asyncio

import pytest
from unittest.mock import AsyncMock, MagicMock


class TestGetAsyncSandbox:
    @pytest.mark.anyio
    async def test_fetch_sandbox(self, monkeypatch):
        monkeypatch.setenv("DAYTONA_API_KEY", "async-key")
        monkeypatch.setenv("DAYTONA_API_URL", "https://async-url")

        mock_client = MagicMock()
        mock_sandbox = MagicMock()
        mock_client.get = AsyncMock(return_value=mock_sandbox)

        import sandbox.provider.daytona.client.async_client as mod

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

        import sandbox.provider.daytona.client.async_client as mod

        loop = asyncio.get_running_loop()
        monkeypatch.setattr(mod, "_load_credentials", lambda: ("async-key", "https://async-url", ""))
        with mod._client_lock:
            mod._cached_clients.clear()
            mod._cached_clients[loop] = (("async-key", "https://async-url", ""), mock_client)

        with pytest.raises(ValueError, match="not found"):
            await mod.get_async_sandbox("sb-nonexistent")
