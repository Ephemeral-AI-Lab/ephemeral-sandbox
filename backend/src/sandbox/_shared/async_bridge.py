"""Loop-aware sync bridge for possibly-awaitable sandbox runtime results.

Several sandbox subsystems call provider methods that return either sync values
(local tests / in-process sandboxes) or coroutines bound to a parent event loop.
They all need one shim that can resolve a coroutine synchronously without
running it on a different event loop from its owning provider client.

The old bridge spun up a fresh event loop for calls without a registered
parent loop. That defeated loop-local async SDK caches and made each sync
runtime command re-enter slow provider client/session setup paths.

The fix is **loop-aware**: async tools dispatch sync work through
``run_sync_in_executor``; that helper explicitly publishes the parent loop to a
``ContextVar`` inside the worker; ``run_sync`` submits provider coroutines back
to the parent loop via ``run_coroutine_threadsafe``. Pure sync callers use one
reusable standalone sandbox I/O loop instead of creating a loop per call.

Public surface:

* :func:`run_sync` — resolve a sync value or a coroutine synchronously.
* :func:`run_sync_in_executor` — dispatch sync work in a worker thread
  while seeding the parent loop in :data:`sandbox_io_loop`.
"""

from __future__ import annotations

import asyncio
import atexit
import concurrent.futures
import contextvars
import inspect
import logging
import threading
from collections.abc import Awaitable, Callable
from typing import Any

logger = logging.getLogger(__name__)


sandbox_io_loop: contextvars.ContextVar[asyncio.AbstractEventLoop | None] = (
    contextvars.ContextVar("sandbox_io_loop", default=None)
)
"""Parent loop that owns sandbox I/O (e.g., the agent's event loop).

Seeded by :func:`run_sync_in_executor` inside the worker thread before the
worker calls :func:`run_sync`, so coroutines are resubmitted onto the
correct loop.
"""


DEFAULT_RUN_SYNC_TIMEOUT_SECONDS = 120.0
"""Upper bound for a single ``run_sync`` call.

Picked slightly above the longest individual provider exec timeout used by
sandbox mutation tools so timeouts surface as timeouts, not as silent hangs.
Callers that need longer budgets should pass ``timeout=`` explicitly.
"""


_DEFAULT_EXECUTOR_WORKERS = 200

_STANDALONE_LOOP_LOCK = threading.Lock()
_STANDALONE_LOOP: asyncio.AbstractEventLoop | None = None
_STANDALONE_THREAD: threading.Thread | None = None
_STANDALONE_LOOP_READY_TIMEOUT_SECONDS = 5.0
_SANDBOX_EXECUTOR_LOCK = threading.Lock()
_SANDBOX_EXECUTOR: concurrent.futures.ThreadPoolExecutor | None = None


def run_sync(result: Any, *, timeout: float | None = None) -> Any:
    """Resolve *result* synchronously if awaitable, else return it.

    Dispatch order for awaitables:

    1. A sandbox I/O loop is registered in the current context and it is
       running on another thread → submit via
       ``run_coroutine_threadsafe`` and wait for the result. This is the
       common path for sync sandbox helper code reached from executor workers.
    2. No parent loop is registered → schedule on a reusable standalone
       sandbox I/O loop. This keeps loop-local async SDK clients warm for
       sync callers that need one-shot bridge access into an async provider
       client.

    ``timeout`` bounds the wait on paths 1 and 2; defaults to
    :data:`DEFAULT_RUN_SYNC_TIMEOUT_SECONDS`.
    """
    if not inspect.isawaitable(result):
        return result

    wait_timeout = timeout if timeout is not None else DEFAULT_RUN_SYNC_TIMEOUT_SECONDS

    parent_loop = sandbox_io_loop.get()
    running_loop = _running_loop_on_this_thread()

    if parent_loop is not None and parent_loop is not running_loop and parent_loop.is_running():
        future = asyncio.run_coroutine_threadsafe(_await_any(result), parent_loop)
        try:
            return future.result(timeout=wait_timeout)
        except concurrent.futures.TimeoutError as exc:
            future.cancel()
            raise TimeoutError(
                f"run_sync: sandbox I/O loop did not complete within "
                f"{wait_timeout:.1f}s"
            ) from exc

    return _run_on_standalone_loop(result, timeout=wait_timeout)


async def _await_any(awaitable: Any) -> Any:
    """Normalize non-coroutine awaitables (Futures, tasks) for ``asyncio.run``.

    ``asyncio.run`` refuses to run non-coroutine awaitables. Wrapping with
    an ``async def`` coerces anything the caller handed us into a coroutine.
    """
    return await awaitable


def _running_loop_on_this_thread() -> asyncio.AbstractEventLoop | None:
    try:
        return asyncio.get_running_loop()
    except RuntimeError:
        return None


def _run_on_standalone_loop(result: Any, *, timeout: float) -> Any:
    loop = _ensure_standalone_loop()
    future = asyncio.run_coroutine_threadsafe(_await_any(result), loop)
    try:
        return future.result(timeout=timeout)
    except concurrent.futures.TimeoutError as exc:
        future.cancel()
        raise TimeoutError(
            f"run_sync: standalone sandbox I/O loop did not complete within "
            f"{timeout:.1f}s"
        ) from exc


def _ensure_standalone_loop() -> asyncio.AbstractEventLoop:
    global _STANDALONE_LOOP, _STANDALONE_THREAD

    with _STANDALONE_LOOP_LOCK:
        if _STANDALONE_LOOP is not None and _STANDALONE_LOOP.is_running():
            return _STANDALONE_LOOP

    # Spawn the thread OUTSIDE the lock so a slow loop startup doesn't
    # serialize every other run_sync caller for the full ready-wait timeout.
    # On timeout, stop any loop the orphan thread managed to publish so it
    # doesn't accumulate across repeated failures.
    ready = threading.Event()
    loop_holder: dict[str, asyncio.AbstractEventLoop] = {}

    def _run_loop() -> None:
        loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)
        loop_holder["loop"] = loop
        ready.set()
        try:
            loop.run_forever()
        finally:
            pending = asyncio.all_tasks(loop)
            for task in pending:
                task.cancel()
            if pending:
                loop.run_until_complete(
                    asyncio.gather(*pending, return_exceptions=True)
                )
            loop.run_until_complete(loop.shutdown_asyncgens())
            loop.run_until_complete(loop.shutdown_default_executor())
            loop.close()

    thread = threading.Thread(
        target=_run_loop,
        name="sandbox-standalone-io",
        daemon=True,
    )
    thread.start()
    if not ready.wait(_STANDALONE_LOOP_READY_TIMEOUT_SECONDS):
        orphan = loop_holder.get("loop")
        if orphan is not None and orphan.is_running():
            orphan.call_soon_threadsafe(orphan.stop)
        raise RuntimeError("standalone sandbox I/O loop did not start")
    loop = loop_holder.get("loop")
    if loop is None:
        raise RuntimeError("standalone sandbox I/O loop did not publish loop")

    with _STANDALONE_LOOP_LOCK:
        if _STANDALONE_LOOP is not None and _STANDALONE_LOOP.is_running():
            # Another caller raced ahead and registered first; stop our loop.
            loop.call_soon_threadsafe(loop.stop)
            return _STANDALONE_LOOP
        _STANDALONE_LOOP = loop
        _STANDALONE_THREAD = thread
        return loop


def _shutdown_standalone_loop() -> None:
    global _STANDALONE_LOOP, _STANDALONE_THREAD

    with _STANDALONE_LOOP_LOCK:
        loop = _STANDALONE_LOOP
        thread = _STANDALONE_THREAD
        _STANDALONE_LOOP = None
        _STANDALONE_THREAD = None

    if loop is not None and loop.is_running():
        try:
            cleanup = asyncio.run_coroutine_threadsafe(
                _shutdown_standalone_loop_clients(),
                loop,
            )
            cleanup.result(timeout=2.0)
        except Exception:
            logger.debug("standalone sandbox I/O client cleanup failed", exc_info=True)
        loop.call_soon_threadsafe(loop.stop)
    else:
        _shutdown_standalone_loop_clients_sync()
    if thread is not None and thread is not threading.current_thread():
        thread.join(timeout=2.0)
    _shutdown_sandbox_executor()


def _shutdown_sandbox_executor() -> None:
    global _SANDBOX_EXECUTOR

    with _SANDBOX_EXECUTOR_LOCK:
        executor = _SANDBOX_EXECUTOR
        _SANDBOX_EXECUTOR = None
    if executor is not None:
        executor.shutdown(wait=False, cancel_futures=True)


def _shutdown_standalone_loop_clients_sync() -> None:
    if not _STANDALONE_LOOP_CLEANUPS:
        return
    try:
        asyncio.run(_shutdown_standalone_loop_clients())
    except Exception:
        logger.debug("standalone sandbox I/O client cleanup failed", exc_info=True)


async def _shutdown_standalone_loop_clients() -> None:
    for cleanup in list(_STANDALONE_LOOP_CLEANUPS):
        await cleanup()


_STANDALONE_LOOP_CLEANUPS: list[Callable[[], Awaitable[None]]] = []


def register_standalone_loop_cleanup(cleanup: Callable[[], Awaitable[None]]) -> None:
    """Register a provider cleanup hook for the standalone sandbox I/O loop."""
    if cleanup not in _STANDALONE_LOOP_CLEANUPS:
        _STANDALONE_LOOP_CLEANUPS.append(cleanup)


atexit.register(_shutdown_standalone_loop)


async def run_sync_in_executor(func: Any, /, *args: Any, **kwargs: Any) -> Any:
    """Run *func* in the default executor without full contextvars propagation.

    Python 3.12's ``asyncio.to_thread`` wraps the call with
    ``contextvars.copy_context().run(func, ...)``, activating the asyncio
    task's contextvars inside the worker thread. That interacts badly
    with some sync provider SDK instrumentation paths, which serialize on shared
    state under propagated contextvars,
    capping parallelism at ~6-7 concurrent regardless of executor size.

    This helper dispatches via a dedicated sandbox executor — which does NOT
    copy contextvars by default — but explicitly re-seeds the
    one contextvar :mod:`sandbox._shared.async_bridge` *does* need in
    the worker thread: :data:`sandbox_io_loop`. Without that seed,
    :func:`run_sync` (called transitively from ``ContentManager``) would
    fall through to the standalone sandbox I/O loop in the worker, which
    may be disconnected from an async provider client bound to the caller's
    loop — surfacing as "Future attached to a different loop".

    Verified at N=72 against live sandbox I/O: ``asyncio.to_thread`` → 6.4x
    parallelism; this helper → 45x.

    Use everywhere a sandbox-bound sync call is dispatched from an async
    caller, such as command commits or Git workspace commits.
    """
    loop = asyncio.get_running_loop()

    def _call() -> Any:
        token = sandbox_io_loop.set(loop)
        try:
            return func(*args, **kwargs)
        finally:
            sandbox_io_loop.reset(token)

    return await loop.run_in_executor(_sandbox_executor(), _call)


def _sandbox_executor() -> concurrent.futures.ThreadPoolExecutor:
    global _SANDBOX_EXECUTOR

    with _SANDBOX_EXECUTOR_LOCK:
        if _SANDBOX_EXECUTOR is None:
            _SANDBOX_EXECUTOR = concurrent.futures.ThreadPoolExecutor(
                max_workers=_DEFAULT_EXECUTOR_WORKERS,
                thread_name_prefix="sandbox-io",
            )
        return _SANDBOX_EXECUTOR


__all__ = [
    "DEFAULT_RUN_SYNC_TIMEOUT_SECONDS",
    "register_standalone_loop_cleanup",
    "run_sync",
    "run_sync_in_executor",
    "sandbox_io_loop",
]
