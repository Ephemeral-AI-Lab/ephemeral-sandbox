"""Tests for the daytona client shutdown helpers (post-lifecycle migration)."""

from __future__ import annotations

import asyncio
from unittest.mock import MagicMock


class TestCloseClient:
    def test_does_nothing_when_client_is_none(self):
        from sandbox.providers.daytona.client.async_shutdown import close_client

        close_client(None)

    def test_calls_close_method(self):
        from sandbox.providers.daytona.client.async_shutdown import close_client

        async def fake_close():
            pass

        close_mock = MagicMock()
        close_mock.close = MagicMock(return_value=fake_close())

        close_client(close_mock)

        close_mock.close.assert_called_once()

    def test_handles_missing_close_method(self):
        from sandbox.providers.daytona.client.async_shutdown import close_client

        client = MagicMock(spec=[])
        close_client(client)


class TestAsyncCloseClient:
    def test_awaits_close_method(self):
        from sandbox.providers.daytona.client.async_shutdown import async_close_client

        closed = False

        class Client:
            async def close(self):
                nonlocal closed
                closed = True

        asyncio.run(async_close_client(Client()))

        assert closed is True

    def test_handles_missing_close_method(self):
        from sandbox.providers.daytona.client.async_shutdown import async_close_client

        client = MagicMock(spec=[])
        asyncio.run(async_close_client(client))


class TestShutdownCachedClient:
    def test_async_shutdown_closes_fallback_loop_clients(self):
        import sandbox.providers.daytona.client.async_ as async_client_mod
        import sandbox.providers.daytona.client.async_shutdown as mod

        async def fake_close():
            pass

        mock_client = MagicMock()
        mock_client.close = MagicMock(return_value=fake_close())
        loop = asyncio.new_event_loop()
        async_client_mod._cached_clients[loop] = (("key", "url", "target"), mock_client)

        try:
            asyncio.run(mod.shutdown_cached_client_async())
        finally:
            loop.close()

        assert len(async_client_mod._cached_clients) == 0

    def test_async_shutdown_closes_active_loop_client(self):
        import sandbox.providers.daytona.client.async_ as async_client_mod
        import sandbox.providers.daytona.client.async_shutdown as mod

        closed = False

        class Client:
            async def close(self):
                nonlocal closed
                closed = True

        async def run() -> None:
            loop = asyncio.get_running_loop()
            with async_client_mod._client_lock:
                async_client_mod._cached_clients.clear()
                async_client_mod._cached_clients[loop] = (("key", "url", "target"), Client())
            await mod.shutdown_cached_client_async()

        asyncio.run(run())

        assert closed is True
        assert len(async_client_mod._cached_clients) == 0
