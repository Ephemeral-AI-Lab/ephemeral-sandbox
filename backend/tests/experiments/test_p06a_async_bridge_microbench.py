"""P0.6a — Async-bridge throughput microbenchmark (D6a gate).

Three arms compared under identical mocked sandbox latency (100 ms per
``process.exec``):

  Arm A — default ``ThreadPoolExecutor`` (~36 workers) + D6a bridge.
  Arm B — executor raised to 200 workers + D6a bridge.
  Arm C — fully-async control (direct ``await``, no ``to_thread``).

Gate (from ``.omc/plans/svc-cmd-occ-migration.md`` §4 P0.6a):

  Arm B wall-clock ≤ 1.2 × Arm C wall-clock at N ≤ 128.

Run:

  .venv/bin/python -m pytest backend/tests/experiments/test_p06a_async_bridge_microbench.py -v -s

The ``-s`` flag exposes the table so the gate can be eyeballed.
"""

from __future__ import annotations

import asyncio
import concurrent.futures
import contextvars
import inspect
import statistics
import time
from typing import Any

import pytest

# --- D6a bridge under test ---------------------------------------------------

sandbox_io_loop: contextvars.ContextVar[asyncio.AbstractEventLoop | None] = (
    contextvars.ContextVar("sandbox_io_loop", default=None)
)


def run_sync(awaitable: Any) -> Any:
    """Proposed D6a replacement for ``code_intelligence._async_bridge.run_sync``."""
    if not inspect.isawaitable(awaitable):
        return awaitable
    parent_loop = sandbox_io_loop.get()
    if parent_loop is not None and parent_loop.is_running():
        return asyncio.run_coroutine_threadsafe(awaitable, parent_loop).result(
            timeout=30
        )
    # Fallback for standalone callers (no parent loop registered).
    try:
        loop = asyncio.get_running_loop()
    except RuntimeError:
        loop = None
    if loop and loop.is_running():
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
            return pool.submit(asyncio.run, awaitable).result()
    return asyncio.run(awaitable)


# --- Fake service -------------------------------------------------------------

_MOCK_LATENCY_S = 0.1  # simulates one 100ms Daytona round-trip


async def _fake_process_exec() -> str:
    await asyncio.sleep(_MOCK_LATENCY_S)
    return "ok"


def svc_sync_op() -> str:
    """Emulates ``svc.edit_file`` doing one sandbox read via ``run_sync``."""
    return run_sync(_fake_process_exec())


async def svc_async_op() -> str:
    """Fully-async control: direct await, no bridge."""
    return await _fake_process_exec()


# --- Benchmark harness --------------------------------------------------------


async def _run_arm(
    *,
    n: int,
    mode: str,
    executor_workers: int | None,
) -> float:
    """Fire ``n`` concurrent ops and return wall-clock seconds."""
    loop = asyncio.get_running_loop()

    if executor_workers is not None:
        loop.set_default_executor(
            concurrent.futures.ThreadPoolExecutor(max_workers=executor_workers)
        )

    async def one_op_bridged() -> str:
        token = sandbox_io_loop.set(loop)
        try:
            return await asyncio.to_thread(svc_sync_op)
        finally:
            sandbox_io_loop.reset(token)

    async def one_op_async() -> str:
        return await svc_async_op()

    start = time.perf_counter()
    if mode == "bridged":
        await asyncio.gather(*(one_op_bridged() for _ in range(n)))
    elif mode == "async":
        await asyncio.gather(*(one_op_async() for _ in range(n)))
    else:
        raise ValueError(mode)
    return time.perf_counter() - start


async def _run_arm_trials(
    *,
    n: int,
    mode: str,
    executor_workers: int | None,
    trials: int = 3,
) -> float:
    timings = []
    for _ in range(trials):
        timings.append(
            await _run_arm(n=n, mode=mode, executor_workers=executor_workers)
        )
    return statistics.median(timings)


# --- The test -----------------------------------------------------------------


CONCURRENCIES = [32, 64, 128, 200, 256, 400]


@pytest.mark.asyncio
async def test_bridge_throughput_meets_async_parity() -> None:
    """Gate: Arm B ≤ 1.2× Arm C at N ≤ 128."""
    rows = []
    fail_reasons = []

    for n in CONCURRENCIES:
        arm_a = await _run_arm_trials(n=n, mode="bridged", executor_workers=None)
        arm_b = await _run_arm_trials(n=n, mode="bridged", executor_workers=200)
        arm_c = await _run_arm_trials(n=n, mode="async", executor_workers=None)
        rows.append((n, arm_a, arm_b, arm_c, arm_b / arm_c if arm_c > 0 else float("inf")))

        if n <= 128 and arm_c > 0 and arm_b / arm_c > 1.2:
            fail_reasons.append(
                f"N={n}: Arm B ({arm_b:.3f}s) > 1.2 × Arm C ({arm_c:.3f}s) = "
                f"{1.2 * arm_c:.3f}s. Ratio {arm_b / arm_c:.2f}x."
            )

    print()
    print(
        f"{'N':>4}  {'ArmA (def exec)':>16}  {'ArmB (200)':>12}  "
        f"{'ArmC (async)':>13}  {'B/C':>6}"
    )
    for n, a, b, c, ratio in rows:
        print(f"{n:>4}  {a:>14.3f}s  {b:>10.3f}s  {c:>11.3f}s  {ratio:>5.2f}x")

    # Sanity: mock latency is 100 ms, so Arm C wall-clock should be ~0.1 s
    # regardless of N (full concurrency). Arm A should plateau around
    # ceil(N / 36) * 0.1 s. Arm B should match Arm C up to N=200.
    for n, _, _, c, _ in rows:
        assert c < 0.6, (
            f"Arm C at N={n} took {c:.3f}s — async control is unexpectedly "
            "slow; bench is unreliable."
        )

    if fail_reasons:
        pytest.fail(
            "D6a bridge fails P0.6a gate (Arm B must be ≤ 1.2× Arm C at N ≤ 128):\n"
            + "\n".join(fail_reasons)
            + "\n\nEscalate to D6b (dedicated I/O loop thread) before Phase 2."
        )


# --- Manual driver (if pytest-asyncio isn't configured) -----------------------


if __name__ == "__main__":
    asyncio.run(test_bridge_throughput_meets_async_parity())
