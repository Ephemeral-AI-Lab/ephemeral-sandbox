"""Tests for the loop-aware sync bridge.

Covers the public surface (``run_sync`` and the bounded default timeout). The
context-var seeding path is exercised end-to-end through ``run_sync_in_executor``
in higher-level tests; here we only assert the loop-agnostic primitives.
"""

from __future__ import annotations

import asyncio

from sandbox.async_bridge import (
    DEFAULT_RUN_SYNC_TIMEOUT_SECONDS,
    run_sync,
)


def test_run_sync_returns_sync_value_as_is() -> None:
    assert run_sync(42) == 42
    assert run_sync("hello") == "hello"
    assert run_sync(None) is None


def test_run_sync_resolves_coroutine_without_running_loop() -> None:
    async def _compute() -> int:
        return 7

    assert run_sync(_compute()) == 7


def test_run_sync_falls_back_when_no_parent_loop_registered() -> None:
    """Purely sync contexts still work; no contextvar, no timeout."""
    async def _compute() -> int:
        await asyncio.sleep(0)
        return 13

    assert run_sync(_compute()) == 13


def test_run_sync_reuses_standalone_loop_without_parent_loop() -> None:
    """Sync callers should not create a new async sandbox loop per call."""
    async def _running_loop() -> asyncio.AbstractEventLoop:
        return asyncio.get_running_loop()

    first = run_sync(_running_loop())
    second = run_sync(_running_loop())

    assert first is second
    assert first.is_running()


def test_default_timeout_is_bounded() -> None:
    """Guard against an accidentally infinite default in production."""
    assert DEFAULT_RUN_SYNC_TIMEOUT_SECONDS > 0
    assert DEFAULT_RUN_SYNC_TIMEOUT_SECONDS <= 600
