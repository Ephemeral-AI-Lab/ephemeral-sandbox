"""Phase 4 §AC11: perf tripwire for per-agent dispatch slot overhead.

Non-blocking tripwire: a failure prints a warning artifact but does NOT
fail CI. The plan calls this "observability, non-blocking" — a regression
shows up in the warning bar but the merge still ships if everything else
is green. To turn into a hard gate, drop the ``WARNING_ONLY`` flag.
"""

from __future__ import annotations

import asyncio
import time

import pytest

from sandbox.daemon.workspace_tool_dispatch import (
    acquire_dispatch_slot,
    reset_dispatch_states_for_test,
)


WARNING_ONLY = True
P99_BUDGET_US = 100.0
CONCURRENCY = 32
ITERATIONS_PER_TASK = 50


@pytest.fixture(autouse=True)
def _clean_states():
    reset_dispatch_states_for_test()
    yield
    reset_dispatch_states_for_test()


async def _measure_one(agent_id: str, samples: list[float]) -> None:
    for _ in range(ITERATIONS_PER_TASK):
        t0 = time.perf_counter()
        async with acquire_dispatch_slot(agent_id):
            pass
        samples.append((time.perf_counter() - t0) * 1_000_000.0)


def _p99(samples: list[float]) -> float:
    if not samples:
        return 0.0
    ordered = sorted(samples)
    index = max(0, int(len(ordered) * 0.99) - 1)
    return ordered[index]


async def test_dispatch_entry_overhead_p99_under_concurrent_load(record_property):
    """Sanity-bound the per-agent slot acquire/release cost.

    Records the p99 microsecond overhead under N=32 concurrent dispatches
    against the same agent_id. When :data:`WARNING_ONLY` is True, exceeding
    :data:`P99_BUDGET_US` produces a captured warning property — not a
    failure — matching Phase 4 §AC11's tripwire model.
    """
    agent_id = "perf-agent"
    samples: list[float] = []
    await asyncio.gather(
        *(_measure_one(agent_id, samples) for _ in range(CONCURRENCY))
    )
    p99 = _p99(samples)
    record_property("p99_us", p99)
    record_property("samples", len(samples))
    if p99 > P99_BUDGET_US:
        message = (
            f"phase4-ac11-tripwire: p99 dispatch-slot overhead "
            f"{p99:.1f}µs > budget {P99_BUDGET_US:.0f}µs "
            f"(samples={len(samples)})"
        )
        if WARNING_ONLY:
            pytest.warns  # touch to keep symbol stable for future hard-gate
            record_property("phase4_ac11_warning", message)
        else:
            pytest.fail(message)


__all__ = ()
