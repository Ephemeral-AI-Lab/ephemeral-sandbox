"""Phase 4 §D1/§D2/§D3/§AC7–§AC10: daemon per-agent quiesce tests.

Covers the engine-independent half of the two-layer enforcement:

* :func:`acquire_dispatch_slot` blocks new dispatches while
  ``exit_pending`` is set.
* :func:`begin_exit_drain` drains in-flight dispatches with the configured
  ``grace_s``; the timeout path returns ``exit_drain_timeout`` and leaves
  ``exit_pending`` reset so the agent can retry.
* The plugin gate sees ``forbidden_in_isolated_workspace`` when the
  isolated workspace is open, even when invoked through the new
  dispatch-slot wrapper.
* The lock-order assertion fires when ``_map_lock`` is acquired without
  ``entry_lock`` outer (test-only, gated by ``EOS_TEST_MODE``).
"""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest

from sandbox.daemon.rpc import dispatcher
from sandbox.daemon.workspace_tool_dispatch import (
    LifecycleInProgressError,
    _OrderedLock,
    _existing_dispatch_state,
    _ensure_dispatch_state,
    acquire_dispatch_slot,
    begin_exit_drain,
    finalize_exit_drain,
    lifecycle_exit_critical_section,
    reset_dispatch_states_for_test,
)


@pytest.fixture(autouse=True)
def _clean_state_between_tests():
    reset_dispatch_states_for_test()
    yield
    reset_dispatch_states_for_test()


# ---------------------------------------------------------------------------
# AC7 — deterministic test: exit waits for in-flight dispatch; post-exit
# dispatch routes correctly; timeout path returns exit_drain_timeout.
# ---------------------------------------------------------------------------


async def test_agent_dispatch_state_serializes_exit_against_inflight_dispatch():
    agent_id = "agent-a"
    proceed = asyncio.Event()
    started = asyncio.Event()

    async def long_dispatch() -> str:
        async with acquire_dispatch_slot(agent_id):
            started.set()
            await proceed.wait()
            return "dispatch-done"

    dispatch_task = asyncio.create_task(long_dispatch())
    await started.wait()

    # Exit drain must block until inflight reaches zero.
    drain_task = asyncio.create_task(begin_exit_drain(agent_id, grace_s=2.0))
    # Yield to let the drain task arm exit_pending.
    await asyncio.sleep(0)
    state = await _existing_dispatch_state(agent_id)
    assert state is not None
    assert state.exit_pending is True
    assert state.inflight == 1
    assert not drain_task.done()

    # Let the dispatch finish; drain completes shortly after.
    proceed.set()
    drain_mode, inflight_observed = await asyncio.wait_for(drain_task, timeout=1.0)
    assert drain_mode == "drained"
    assert inflight_observed == 1
    assert (await dispatch_task) == "dispatch-done"
    assert state.exit_pending is True  # caller (the exit path) clears later

    # Caller now mutates maps and finalizes.
    async with lifecycle_exit_critical_section(agent_id):
        pass
    await finalize_exit_drain(agent_id)
    assert await _existing_dispatch_state(agent_id) is None


# ---------------------------------------------------------------------------
# AC8a — inflight==0 fast path: exit drain returns drained immediately.
# ---------------------------------------------------------------------------


async def test_exit_drain_inflight_zero_fast_path():
    agent_id = "agent-fast"
    # Force state creation through a touch-and-release dispatch slot.
    async with acquire_dispatch_slot(agent_id):
        pass
    drain_mode, observed = await begin_exit_drain(agent_id, grace_s=0.5)
    assert drain_mode == "drained"
    assert observed == 0


async def test_exit_drain_fast_path_when_no_state_exists():
    drain_mode, observed = await begin_exit_drain("never-dispatched", grace_s=0.5)
    assert drain_mode == "fast_path"
    assert observed == 0


# ---------------------------------------------------------------------------
# AC8b — inflight=N: exit blocks until N->0.
# ---------------------------------------------------------------------------


async def test_exit_drain_waits_for_inflight():
    agent_id = "agent-wait"
    proceed = asyncio.Event()
    started = asyncio.Event()

    async def slow_dispatch():
        async with acquire_dispatch_slot(agent_id):
            started.set()
            await proceed.wait()

    task = asyncio.create_task(slow_dispatch())
    await started.wait()

    drain_task = asyncio.create_task(begin_exit_drain(agent_id, grace_s=1.0))
    # Drain must NOT complete before the in-flight dispatch releases.
    await asyncio.sleep(0.05)
    assert not drain_task.done()

    proceed.set()
    drain_mode, observed = await asyncio.wait_for(drain_task, timeout=1.0)
    assert drain_mode == "drained"
    assert observed == 1
    await task


# ---------------------------------------------------------------------------
# AC8c — timeout then retry succeeds.
# ---------------------------------------------------------------------------


async def test_exit_drain_timeout_then_retry_succeeds():
    agent_id = "agent-timeout"
    started = asyncio.Event()
    proceed = asyncio.Event()

    async def stuck_dispatch():
        async with acquire_dispatch_slot(agent_id):
            started.set()
            await proceed.wait()

    task = asyncio.create_task(stuck_dispatch())
    await started.wait()

    drain_mode, observed = await begin_exit_drain(agent_id, grace_s=0.05)
    assert drain_mode == "timeout"
    assert observed == 1

    state = await _existing_dispatch_state(agent_id)
    assert state is not None
    # exit_pending was reset so subsequent dispatch and retry can proceed.
    assert state.exit_pending is False

    # Retry after dispatch releases.
    proceed.set()
    await task
    retry_mode, retry_observed = await begin_exit_drain(agent_id, grace_s=0.5)
    assert retry_mode == "drained"
    assert retry_observed == 0
    await finalize_exit_drain(agent_id)


# ---------------------------------------------------------------------------
# acquire_dispatch_slot rejects new arrivals when exit_pending is set.
# ---------------------------------------------------------------------------


async def test_acquire_dispatch_slot_raises_when_exit_pending():
    agent_id = "agent-pending"
    # Touch to create state, then manually flip exit_pending.
    state = await _ensure_dispatch_state(agent_id)
    state.exit_pending = True
    with pytest.raises(LifecycleInProgressError) as exc:
        async with acquire_dispatch_slot(agent_id):
            pass
    assert exc.value.agent_id == agent_id
    assert state.inflight == 0


# ---------------------------------------------------------------------------
# acquire_dispatch_slot decrements inflight on exception / cancellation.
# ---------------------------------------------------------------------------


async def test_acquire_dispatch_slot_finally_decrements_on_exception():
    agent_id = "agent-exc"

    class _Boom(Exception):
        pass

    with pytest.raises(_Boom):
        async with acquire_dispatch_slot(agent_id):
            raise _Boom()

    state = await _existing_dispatch_state(agent_id)
    assert state is not None
    assert state.inflight == 0
    assert state.inflight_zero.is_set()


# ---------------------------------------------------------------------------
# D3 — plugin gate returns forbidden when isolated workspace is open.
# ---------------------------------------------------------------------------


def test_plugin_gate_exit_pending_returns_forbidden(monkeypatch):
    class _Iws:
        @staticmethod
        def get_handle(agent_id: str) -> object | None:
            return object() if agent_id == "agent-x" else None

    monkeypatch.setattr(dispatcher, "get_active_pipeline", lambda: _Iws())
    blocked = dispatcher._plugin_block_decision("api.plugin.ensure", "agent-x")
    assert blocked is not None
    assert blocked["error"]["kind"] == "forbidden_in_isolated_workspace"

    # No isolated workspace -> proceed.
    allowed = dispatcher._plugin_block_decision("api.plugin.ensure", "agent-y")
    assert allowed is None


# ---------------------------------------------------------------------------
# AC9 — lock order assertion: acquiring _map_lock without entry_lock fires.
# ---------------------------------------------------------------------------


async def test_lock_order_entry_outer_map_inner_assertion(monkeypatch):
    monkeypatch.setenv("EOS_TEST_MODE", "true")
    entry = _OrderedLock("entry_lock")
    map_lock = _OrderedLock("_map_lock")

    # Correct order: entry first, then map.
    async with entry:
        async with map_lock:
            pass

    # Reverse order should raise.
    with pytest.raises(AssertionError) as exc:
        async with map_lock:
            pass
    assert "_map_lock" in str(exc.value)
    assert "entry_lock" in str(exc.value)


async def test_lock_order_assertion_silent_outside_test_mode(monkeypatch):
    monkeypatch.delenv("EOS_TEST_MODE", raising=False)
    map_lock = _OrderedLock("_map_lock")
    # No assertion: production path stays silent.
    async with map_lock:
        pass


# ---------------------------------------------------------------------------
# AC9 (production wiring): the assertion fires through a real
# ``IsolatedPipeline._map_lock`` instance, not just a synthetic wrapper.
# ---------------------------------------------------------------------------


async def test_real_pipeline_map_lock_uses_ordered_lock(monkeypatch):
    """Phase 4 §AC9: the production ``_map_lock`` is the wrapped variant
    so the assertion fires when ``_map_lock`` is acquired without
    ``entry_lock`` outer."""
    monkeypatch.setenv("EOS_TEST_MODE", "true")
    # Import inside the test to avoid a top-level dependency on the
    # full IsolatedPipeline construction graph; the lazy import inside
    # ``__init__`` does the heavy lifting.
    from sandbox.isolated_workspace.pipeline import IsolatedPipeline

    class _LayerStackDouble:
        def __init__(self):
            self.released = []

        def prepare_workspace_snapshot(self, *, request_id):  # pragma: no cover
            raise NotImplementedError

        def release_lease(self, *, lease_id):
            self.released.append(lease_id)

    pipeline = IsolatedPipeline(
        scratch_root=Path("/tmp/phase4-test"),
        layer_stack=_LayerStackDouble(),
    )
    assert isinstance(pipeline._map_lock, _OrderedLock)
    assert pipeline._map_lock.name == "_map_lock"
    # Acquiring ``_map_lock`` alone fires the assertion because no
    # ``entry_lock`` is held in the current task.
    with pytest.raises(AssertionError) as exc:
        async with pipeline._map_lock:
            pass
    assert "_map_lock" in str(exc.value)
    assert "entry_lock" in str(exc.value)


__all__ = ()
