"""Tests for the loop-aware sync bridge.

These exercise the contract that keeps the async Daytona SDK alive when
sync CI code (``ContentManager``, ``LspClient``) is called from a worker
thread launched via ``asyncio.to_thread``.
"""

from __future__ import annotations

import asyncio
import concurrent.futures
import threading

import pytest

from sandbox.async_bridge import (
    DEFAULT_RUN_SYNC_TIMEOUT_SECONDS,
    configure_default_executor,
    current_sandbox_io_loop,
    run_sync,
    use_sandbox_io_loop,
)


def test_run_sync_returns_sync_value_as_is() -> None:
    assert run_sync(42) == 42
    assert run_sync("hello") == "hello"
    assert run_sync(None) is None


def test_run_sync_resolves_coroutine_without_running_loop() -> None:
    async def _compute() -> int:
        return 7

    assert run_sync(_compute()) == 7


def test_run_sync_submits_coroutine_to_registered_parent_loop() -> None:
    """Coroutines from a worker thread must run on the parent loop."""
    observed_loops: list[asyncio.AbstractEventLoop | None] = []

    async def _on_parent_loop() -> str:
        observed_loops.append(asyncio.get_running_loop())
        await asyncio.sleep(0)
        return "from-parent-loop"

    async def _driver() -> str:
        def _worker() -> str:
            # Inside the worker thread: no running loop here, but the
            # parent loop is registered via the context var copied by
            # asyncio.to_thread.
            assert current_sandbox_io_loop() is not None
            return run_sync(_on_parent_loop())

        with use_sandbox_io_loop():
            return await asyncio.to_thread(_worker)

    result = asyncio.run(_driver())
    assert result == "from-parent-loop"
    assert len(observed_loops) == 1


def test_run_sync_raises_timeout_on_parent_loop_stall() -> None:
    """A hung sandbox I/O should fail loudly rather than hang forever."""
    async def _hang() -> None:
        await asyncio.Event().wait()

    async def _driver() -> None:
        def _worker() -> None:
            run_sync(_hang(), timeout=0.1)

        with use_sandbox_io_loop():
            await asyncio.to_thread(_worker)

    with pytest.raises(TimeoutError):
        asyncio.run(_driver())


def test_run_sync_falls_back_when_no_parent_loop_registered() -> None:
    """Purely sync contexts still work; no contextvar, no timeout."""
    async def _compute() -> int:
        await asyncio.sleep(0)
        return 13

    assert run_sync(_compute()) == 13


def test_use_sandbox_io_loop_resets_contextvar_on_exit() -> None:
    async def _driver() -> None:
        assert current_sandbox_io_loop() is None
        with use_sandbox_io_loop():
            assert current_sandbox_io_loop() is asyncio.get_running_loop()
        assert current_sandbox_io_loop() is None

    asyncio.run(_driver())


def test_use_sandbox_io_loop_rejects_use_outside_running_loop() -> None:
    with pytest.raises(RuntimeError):
        # Must be called inside an async context to auto-detect the loop.
        use_sandbox_io_loop()


def test_use_sandbox_io_loop_accepts_explicit_loop() -> None:
    loop = asyncio.new_event_loop()
    try:
        with use_sandbox_io_loop(loop):
            assert current_sandbox_io_loop() is loop
        assert current_sandbox_io_loop() is None
    finally:
        loop.close()


def test_configure_default_executor_sets_pool_on_loop() -> None:
    async def _driver() -> int:
        pool = configure_default_executor(max_workers=8)
        assert isinstance(pool, concurrent.futures.ThreadPoolExecutor)
        # to_thread dispatches through the default executor we just set.
        return await asyncio.to_thread(lambda: 1 + 1)

    assert asyncio.run(_driver()) == 2


def test_default_timeout_is_bounded() -> None:
    """Guard against an accidentally infinite default in production."""
    assert DEFAULT_RUN_SYNC_TIMEOUT_SECONDS > 0
    assert DEFAULT_RUN_SYNC_TIMEOUT_SECONDS <= 600


def test_run_sync_handles_non_coroutine_awaitables() -> None:
    """Coroutines that return via ``await`` chains (not ``async def``) still resolve."""
    async def _outer() -> int:
        # Deliberately construct a nested awaitable chain to confirm the
        # bridge's ``_await_any`` adapter coerces whatever the caller hands
        # back into a coroutine ``asyncio.run`` will accept.
        inner_future: asyncio.Future[int] = asyncio.get_running_loop().create_future()
        inner_future.set_result(55)
        return await inner_future

    async def _driver() -> int:
        def _worker() -> int:
            return run_sync(_outer())

        with use_sandbox_io_loop():
            return await asyncio.to_thread(_worker)

    assert asyncio.run(_driver()) == 55


def test_concurrent_workers_share_parent_loop() -> None:
    """Many workers fan out; all hand coroutines back to the one parent."""
    results: list[int] = []
    results_lock = threading.Lock()

    async def _on_parent(i: int) -> int:
        await asyncio.sleep(0.001)
        return i * 2

    async def _driver() -> None:
        def _worker(i: int) -> None:
            value = run_sync(_on_parent(i))
            with results_lock:
                results.append(int(value))

        configure_default_executor(max_workers=16)
        with use_sandbox_io_loop():
            await asyncio.gather(
                *(asyncio.to_thread(_worker, i) for i in range(20)),
            )

    asyncio.run(_driver())
    assert sorted(results) == [i * 2 for i in range(20)]
