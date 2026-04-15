"""Async client lifecycle — cleanup on interpreter shutdown."""

from __future__ import annotations

import atexit
import asyncio
import inspect
import logging
import threading
from typing import Any

logger = logging.getLogger(__name__)


def close_client(client: Any) -> None:
    if client is None:
        return
    close_fn = getattr(client, "close", None)
    if not callable(close_fn):
        return
    try:
        close_result = close_fn()
    except Exception:
        logger.debug("Failed to close cached AsyncDaytona client", exc_info=True)
        return
    if not inspect.isawaitable(close_result):
        return

    def _run_close() -> None:
        close_loop: asyncio.AbstractEventLoop | None = None
        try:
            close_loop = asyncio.new_event_loop()
            asyncio.set_event_loop(close_loop)
            close_loop.run_until_complete(close_result)
        except Exception:
            logger.debug("Failed to await AsyncDaytona close", exc_info=True)
        finally:
            if close_loop is not None:
                close_loop.close()

    closer = threading.Thread(target=_run_close, name="daytona-async-client-close", daemon=True)
    closer.start()
    closer.join(timeout=1.0)


def shutdown_cached_client() -> None:
    from sandbox import async_client as async_client_mod

    with async_client_mod._client_lock:
        clients = [client for _, client in async_client_mod._cached_clients.values()]
        async_client_mod._cached_clients.clear()
    for client in clients:
        close_client(client)


atexit.register(shutdown_cached_client)
