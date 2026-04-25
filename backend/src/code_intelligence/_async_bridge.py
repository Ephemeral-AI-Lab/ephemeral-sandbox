"""Loop-aware sync bridge for possibly-awaitable sandbox results.

Several code-intelligence subsystems call SDK methods that return either
sync values (local tests / in-process sandboxes) or coroutines bound to a
parent event loop (the async Daytona client). They all need one shim that
can resolve a coroutine synchronously without destroying the SDK's aiohttp
session by running it on a different event loop.

The old bridge spun up a fresh ``ThreadPoolExecutor(max_workers=1)`` and
called ``asyncio.run(coro)`` on every call — a brand-new event loop per
invocation inside a brand-new worker thread. That corrupts the async
Daytona SDK (its aiohttp session is bound to the parent loop) and is the
root cause of the ``_ci_sandbox_for_sync_occ`` workaround that used to
re-resolve a sync sandbox handle before every OCC file op.

The fix is **loop-aware**: async tools publish the parent loop to a
``ContextVar`` before handing off to ``asyncio.to_thread``; the worker
thread inherits the contextvar (``to_thread`` copies the current
``Context`` automatically on Python 3.9+); ``run_sync`` submits the
coroutine back to the parent loop via ``run_coroutine_threadsafe``.

Public surface:

* :func:`run_sync` — resolve a sync value or a coroutine synchronously.
* :func:`use_sandbox_io_loop` — context manager for tool/helper code to
  register the current event loop before ``to_thread`` dispatch.
* :func:`current_sandbox_io_loop` — accessor (useful in tests).
* :func:`configure_default_executor` — raise the default
  ``ThreadPoolExecutor`` size so bulk svc ops aren't capped at ~32
  workers. Called once at runtime startup.
"""

from __future__ import annotations

import asyncio
import concurrent.futures
import contextlib
import contextvars
import inspect
import logging
from collections.abc import Iterator
from typing import Any

logger = logging.getLogger(__name__)


sandbox_io_loop: contextvars.ContextVar[asyncio.AbstractEventLoop | None] = (
    contextvars.ContextVar("sandbox_io_loop", default=None)
)
"""Parent loop that owns sandbox I/O (e.g., the agent's event loop).

Set by async tools via :func:`use_sandbox_io_loop` before dispatching to
``asyncio.to_thread``. Read by :func:`run_sync` inside the worker thread
so coroutines are resubmitted onto the correct loop.
"""


DEFAULT_RUN_SYNC_TIMEOUT_SECONDS = 120.0
"""Upper bound for a single ``run_sync`` call.

Picked slightly above the longest individual Daytona exec timeout used by
CI mutation tools so timeouts surface as timeouts, not as silent hangs.
Callers that need longer budgets should pass ``timeout=`` explicitly.
"""


_DEFAULT_EXECUTOR_WORKERS = 200


def use_sandbox_io_loop(
    loop: asyncio.AbstractEventLoop | None = None,
) -> contextlib.AbstractContextManager[None]:
    """Register *loop* as the sandbox I/O loop for the current context.

    Intended usage from an async tool that is about to call
    ``asyncio.to_thread(svc.xxx, ...)`` where ``svc.xxx`` internally uses
    :func:`run_sync` on coroutines returned by an async sandbox SDK::

        async with use_sandbox_io_loop():
            await asyncio.to_thread(svc.write_file, specs, ...)

    Passing ``loop=None`` uses ``asyncio.get_running_loop()`` (the common
    case). Tests may pass an explicit loop.
    """
    effective = loop if loop is not None else asyncio.get_running_loop()
    return _SandboxIoLoopScope(effective)


class _SandboxIoLoopScope:
    """Context manager that scopes :data:`sandbox_io_loop` to a single run."""

    __slots__ = ("_loop", "_token")

    def __init__(self, loop: asyncio.AbstractEventLoop) -> None:
        self._loop = loop
        self._token: contextvars.Token[asyncio.AbstractEventLoop | None] | None = None

    def __enter__(self) -> None:
        self._token = sandbox_io_loop.set(self._loop)

    def __exit__(self, exc_type: Any, exc: Any, tb: Any) -> None:
        if self._token is not None:
            sandbox_io_loop.reset(self._token)
            self._token = None


def current_sandbox_io_loop() -> asyncio.AbstractEventLoop | None:
    """Return the registered sandbox I/O loop, or ``None`` if unset."""
    return sandbox_io_loop.get()


def run_sync(result: Any, *, timeout: float | None = None) -> Any:
    """Resolve *result* synchronously if awaitable, else return it.

    Dispatch order for awaitables:

    1. A sandbox I/O loop is registered in the current context and it is
       running on another thread → submit via
       ``run_coroutine_threadsafe`` and wait for the result. This is the
       common path for sync CI code (``ContentManager``, ``LspClient``)
       reached from ``asyncio.to_thread``.
    2. An event loop is already running on **this** thread → schedule on
       a fresh background loop (rare — indicates a reentrancy bug in the
       caller, not a sandbox hop). Keep the old behavior for
       compatibility.
    3. No running loop anywhere → ``asyncio.run(coro)`` on this thread.

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

    if running_loop is not None:
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
            return pool.submit(asyncio.run, _await_any(result)).result(timeout=wait_timeout)

    return asyncio.run(_await_any(result))


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


async def run_sync_in_executor(func: Any, /, *args: Any, **kwargs: Any) -> Any:
    """Run *func* in the default executor without full contextvars propagation.

    Python 3.12's ``asyncio.to_thread`` wraps the call with
    ``contextvars.copy_context().run(func, ...)``, activating the asyncio
    task's contextvars inside the worker thread. That interacts badly
    with the sync Daytona SDK (its ``@with_instrumentation`` OpenTelemetry
    path serializes on shared state under propagated contextvars),
    capping parallelism at ~6-7 concurrent regardless of executor size.

    This helper dispatches via ``loop.run_in_executor(None, ...)`` — which
    does NOT copy contextvars by default — but explicitly re-seeds the
    one contextvar :mod:`code_intelligence._async_bridge` *does* need in
    the worker thread: :data:`sandbox_io_loop`. Without that seed,
    :func:`run_sync` (called transitively from ``ContentManager``) would
    fall through to ``asyncio.run(coro)`` in the worker, creating a
    fresh event loop disconnected from any AsyncDaytona aiohttp client
    bound to the caller's loop — surfacing as "Future attached to a
    different loop".

    Verified at N=72 against live Daytona: ``asyncio.to_thread`` → 6.4x
    parallelism; this helper → 45x.

    Use everywhere a sandbox-bound sync call is dispatched from an async
    caller (``submit_commit``, Git workspace commit, rename).
    """
    loop = asyncio.get_running_loop()

    def _call() -> Any:
        token = sandbox_io_loop.set(loop)
        try:
            return func(*args, **kwargs)
        finally:
            sandbox_io_loop.reset(token)

    return await loop.run_in_executor(None, _call)


def configure_default_executor(
    loop: asyncio.AbstractEventLoop | None = None,
    *,
    max_workers: int = _DEFAULT_EXECUTOR_WORKERS,
) -> concurrent.futures.ThreadPoolExecutor:
    """Raise the event loop's default executor for bulk sandbox I/O.

    Called once at runtime startup. Python's default is
    ``min(32, (os.cpu_count() or 1) + 4)`` which throttles concurrent
    ``asyncio.to_thread`` dispatches when many tool calls fan out in
    parallel. The returned executor is also attached to *loop* (or the
    running loop when *loop* is ``None``) so subsequent ``to_thread``
    calls use it directly.
    """
    target = loop if loop is not None else asyncio.get_event_loop()
    pool = concurrent.futures.ThreadPoolExecutor(
        max_workers=max_workers,
        thread_name_prefix="sandbox-io",
    )
    target.set_default_executor(pool)
    logger.debug(
        "async-bridge: default executor raised to %d workers", max_workers,
    )
    return pool


@contextlib.contextmanager
def running_on_sandbox_io_loop(
    loop: asyncio.AbstractEventLoop,
) -> Iterator[None]:
    """Backwards-compatible alias for :func:`use_sandbox_io_loop`.

    Useful where older call sites imported ``running_on_sandbox_io_loop``
    as a plain context manager. New code should prefer
    :func:`use_sandbox_io_loop`.
    """
    with _SandboxIoLoopScope(loop):
        yield


__all__ = [
    "DEFAULT_RUN_SYNC_TIMEOUT_SECONDS",
    "configure_default_executor",
    "current_sandbox_io_loop",
    "run_sync",
    "run_sync_in_executor",
    "running_on_sandbox_io_loop",
    "sandbox_io_loop",
    "use_sandbox_io_loop",
]
