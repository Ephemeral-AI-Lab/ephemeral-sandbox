"""Sync ↔ async bridge for the persistent LSP child process.

:class:`LspClient` exposes synchronous methods (``goto_definition`` etc.) but
the underlying :class:`LspBackendChild` is async. The orchestrator-side
:class:`InProcessBackend` may be invoked from either a sync caller (orchestrator
test, fall-back path) or from inside the daemon's asyncio loop (when the daemon
constructs the in-process backend with ``sandbox=None`` and a handler awaits
``svc.find_definitions``).

A naive ``run_sync(...)`` blows up in the second case (``asyncio.run`` from
inside a running loop is forbidden). :class:`_LspAsyncHost` solves this by
running the LspBackendChild in a dedicated daemon thread that owns its own
event loop, and exposing thread-safe ``run`` / ``shutdown`` entry points.

Design choices:

* One host per LspClient. Re-using a single host keeps the basedpyright
  child warm across queries (the daemon spawns one langserver per workspace).
* Lazy spawn — the host doesn't start its thread until the first query.
  Cold-sandbox ``ping`` / ``index`` paths never pay the LSP startup cost.
* Bounded restart-on-crash — first crash respawns the child once; second
  consecutive crash escalates :class:`LspChildUnavailable` to the caller.
"""

from __future__ import annotations

import asyncio
import concurrent.futures
import logging
import threading
from typing import Any, Awaitable, Callable, TypeVar

from sandbox.code_intelligence.language_server.lsp_child import (
    LspBackendChild,
    LspChildCrashed,
    LspChildUnavailable,
)

__all__ = ["LspChildCrashed", "LspChildUnavailable", "LspAsyncHost"]

logger = logging.getLogger(__name__)

_T = TypeVar("_T")


class LspAsyncHost:
    """Owns a daemon thread + asyncio loop + a single :class:`LspBackendChild`."""

    def __init__(self, workspace_root: str) -> None:
        self._workspace_root = workspace_root
        self._loop: asyncio.AbstractEventLoop | None = None
        self._thread: threading.Thread | None = None
        self._ready = threading.Event()
        self._child: LspBackendChild | None = None
        self._restart_used = False
        self._lock = threading.Lock()
        self._closed = False

    # -------------------------------------------------------------- lifecycle

    def _ensure_thread(self) -> asyncio.AbstractEventLoop:
        with self._lock:
            if self._loop is not None:
                return self._loop
            if self._closed:
                raise LspChildUnavailable("LspAsyncHost is closed")
            self._ready = threading.Event()
            self._thread = threading.Thread(
                target=self._run_loop,
                name="lsp-async-host",
                daemon=True,
            )
            self._thread.start()
            self._ready.wait(timeout=5.0)
            assert self._loop is not None  # noqa: S101 - narrowed by the wait
            return self._loop

    def _run_loop(self) -> None:
        loop = asyncio.new_event_loop()
        self._loop = loop
        asyncio.set_event_loop(loop)
        self._ready.set()
        try:
            loop.run_forever()
        finally:
            try:
                loop.run_until_complete(loop.shutdown_asyncgens())
            except Exception:  # pragma: no cover - defensive
                pass
            loop.close()

    async def _ensure_child_async(self) -> LspBackendChild:
        if self._child is not None:
            return self._child
        child = LspBackendChild(self._workspace_root)
        await child.start()
        self._child = child
        return child

    # ----------------------------------------------------------------- run

    def run(
        self,
        fn: Callable[[LspBackendChild], Awaitable[_T]],
    ) -> _T:
        """Execute *fn(child)* on the host loop synchronously.

        Restart-on-crash is bounded to one consecutive failure: the second
        :class:`LspChildCrashed` in a row escalates to
        :class:`LspChildUnavailable` so the operator sees the failure
        instead of an indefinite respawn loop.
        """
        loop = self._ensure_thread()

        async def _runner() -> _T:
            try:
                child = await self._ensure_child_async()
                result = await fn(child)
            except LspChildCrashed:
                # First crash → respawn once.
                if self._restart_used:
                    self._restart_used = False
                    raise LspChildUnavailable(
                        "LSP child crashed twice in a row — escalating"
                    )
                self._restart_used = True
                self._child = None
                child = await self._ensure_child_async()
                try:
                    result = await fn(child)
                except LspChildCrashed as exc:
                    raise LspChildUnavailable(
                        f"LSP child crashed again on retry: {exc}"
                    ) from exc
                else:
                    self._restart_used = False
                    return result
            else:
                self._restart_used = False
                return result

        future = asyncio.run_coroutine_threadsafe(_runner(), loop)
        try:
            return future.result(timeout=60.0)
        except concurrent.futures.TimeoutError as exc:
            future.cancel()
            raise LspChildUnavailable(
                f"LSP host timed out waiting for child: {exc}"
            ) from exc

    def close(self) -> None:
        """Stop the host loop + tear down the child."""
        with self._lock:
            self._closed = True
            loop = self._loop
            child = self._child
            self._loop = None
            self._child = None
        if loop is None:
            return

        async def _shutdown() -> None:
            if child is not None:
                try:
                    await child.shutdown(timeout_s=2.0)
                except Exception:  # pragma: no cover - defensive
                    logger.debug("lsp child shutdown failed", exc_info=True)

        try:
            future = asyncio.run_coroutine_threadsafe(_shutdown(), loop)
            try:
                future.result(timeout=5.0)
            except Exception:  # pragma: no cover - defensive
                pass
        finally:
            try:
                loop.call_soon_threadsafe(loop.stop)
            except RuntimeError:
                pass
        if self._thread is not None:
            self._thread.join(timeout=5.0)


def _make_unavailable_error(message: str) -> Any:
    """Helper used by tests to surface a clear LspChildUnavailable."""
    return LspChildUnavailable(message)
