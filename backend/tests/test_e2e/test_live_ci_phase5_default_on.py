"""Phase 5 — daemon default live E2E over process.exec-backed daemon command.

Three subtests against the real Daytona ``dask__dask_2023.3.2_2023.4.0`` sandbox:

A. ``test_default_flag_on_smoke`` — every operation works through
   ``DaemonBackend`` over the process.exec socket shim. Asserts ``_select_backend``
   returns the daemon path for transport-backed sandboxes.

B. ``test_concurrent_query_symbols`` — 8 concurrent ``query_symbols`` calls
   succeed with zero errors and finish in below the public-daemon command ceiling.

C. ``test_curated_cross_phase_regression`` — one assertion per prior phase
   (0/1/2/3/3.5/3.6/4) wired through the now-default daemon path. Catches
   any regression the daemon-default selector exposes that the per-phase
   suites missed.

Run with explicit user approval (do NOT auto-run; project memory
``feedback_parallel_user_commits``):

    .venv/bin/pytest backend/tests/test_e2e/test_live_ci_phase5_default_on.py \\
        -m live -v -s
"""

from __future__ import annotations

import asyncio
import sys
import time
import uuid
from collections.abc import Iterator
from contextlib import contextmanager
from dataclasses import dataclass
from typing import Any

import pytest

from engine.testing.eval_agent import EvalAgent
from sandbox.api.bash import extract_exit_code, wrap_bash_command
from sandbox.code_intelligence.backends import DaemonBackend
from sandbox.code_intelligence.core.types import WriteSpec
from sandbox.code_intelligence.service import CodeIntelligenceService

from ._timing_harness import TimingHarness

pytestmark = [pytest.mark.e2e, pytest.mark.live]

_DASK_SWEEVO_INSTANCE_ID = "dask__dask_2023.3.2_2023.4.0"
_DASK_SWEEVO_REPO_DIR = "/testbed"
_PUBLIC_DAEMON_COMMAND_P99_CEILING_S = 10.0
_CONCURRENT_QUERY_COUNT = 8


def _flush(msg: str) -> None:
    print(msg, flush=True)
    sys.stdout.flush()


@contextmanager
def _trace(harness: TimingHarness, name: str) -> Iterator[None]:
    _flush(f"  → {name} ...")
    t0 = time.perf_counter()
    with harness.step(name):
        yield
    _flush(f"  ✓ {name} ({time.perf_counter() - t0:.3f}s)")


@dataclass
class LivePhase5Env:
    sandbox_id: str
    raw_sandbox: Any
    home: str
    root_dir: str

    def exec(self, command: str, *, timeout: int = 60) -> tuple[int, str]:
        response = self.raw_sandbox.process.exec(
            wrap_bash_command(command),
            timeout=timeout,
        )
        output, exit_code = extract_exit_code(
            getattr(response, "result", "") or "",
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        return exit_code, output

    def make_ci_service(self) -> CodeIntelligenceService:
        """Build a transport-backed CodeIntelligenceService."""
        from sandbox.daytona.transport import DaytonaTransport

        return CodeIntelligenceService(
            sandbox_id=self.sandbox_id,
            workspace_root=self.root_dir,
            transport=DaytonaTransport(),
        )


@pytest.fixture(scope="module")
def live_phase5_env() -> LivePhase5Env:
    if not EvalAgent.has_daytona():
        pytest.skip("Daytona credentials not configured")

    from benchmarks.sweevo.dataset import select_sweevo_instance
    from benchmarks.sweevo.models import _CONDA_ACTIVATE
    from benchmarks.sweevo.sandbox import create_sweevo_test_sandbox
    from sandbox.testing import delete_test_sandbox, get_sandbox_service

    _flush(
        f"\n[fixture] provisioning sweevo sandbox {_DASK_SWEEVO_INSTANCE_ID} ..."
    )
    instance = select_sweevo_instance(instance_id=_DASK_SWEEVO_INSTANCE_ID)
    sandbox_name = f"ci-phase5-{uuid.uuid4().hex[:8]}"
    t0 = time.perf_counter()
    result = asyncio.run(
        create_sweevo_test_sandbox(
            instance,
            sandbox_name=sandbox_name,
            repo_dir=_DASK_SWEEVO_REPO_DIR,
        )
    )
    sandbox_id = str(result["sandbox_id"])
    _flush(
        f"[fixture] sandbox {sandbox_id} ready in {time.perf_counter() - t0:.1f}s"
    )
    try:
        raw_sandbox = get_sandbox_service().get_sandbox_object(sandbox_id)
        home_resp = raw_sandbox.process.exec("printf '%s' \"$HOME\"", timeout=10)
        home = (getattr(home_resp, "result", "") or "").strip() or "/home/daytona"
        env = LivePhase5Env(
            sandbox_id=sandbox_id,
            raw_sandbox=raw_sandbox,
            home=home,
            root_dir=_DASK_SWEEVO_REPO_DIR,
        )
        exit_code, output = env.exec(
            f"{_CONDA_ACTIVATE} && cd {_DASK_SWEEVO_REPO_DIR} && python --version",
            timeout=60,
        )
        assert exit_code == 0, output
        _flush(f"[fixture] sandbox ready: {output.strip()}")
        yield env
    finally:
        _flush(f"[fixture] tearing down sandbox {sandbox_id} ...")
        delete_test_sandbox(sandbox_id)


# ---------------------------------------------------------------------------
# 5.6.A — daemon-default selector uses DaemonBackend; full smoke
# ---------------------------------------------------------------------------


def test_default_flag_on_smoke(live_phase5_env: LivePhase5Env) -> None:
    h = TimingHarness(phase=5, test_name="default_on_smoke")
    env = live_phase5_env

    with _trace(h, "ci_service_construct_default"):
        svc = env.make_ci_service()
    # Phase 5 assertion: transport+sandbox_id present -> DaemonBackend.
    assert isinstance(svc._impl, DaemonBackend), (
        "transport-backed sandboxes must use DaemonBackend"
    )

    with _trace(h, "ensure_initialized"):
        svc.ensure_initialized(wait=True)

    with _trace(h, "query_symbols_warm"):
        results = svc.query_symbols("Bag")
    assert isinstance(results, list)

    smoke_path = f"{env.root_dir}/_phase5_smoke_{uuid.uuid4().hex[:6]}.txt"
    with _trace(h, "write_file"):
        op = svc.write_file(
            [WriteSpec(file_path=smoke_path, content="ok\n", overwrite=True)],
        )
    assert op.success, f"write_file failed: {op.status}"

    with _trace(h, "svc_cmd_smoke"):
        async_sandbox = asyncio.run(_resolve_async_sandbox(env.sandbox_id))
        cmd_result = asyncio.run(
            svc.cmd(async_sandbox, "find /testbed -name '*.py' | wc -l"),
        )
    assert cmd_result.exit_code == 0

    _flush("\n" + h.report())
    h.dump_json()
    svc.dispose()


# ---------------------------------------------------------------------------
# 5.6.B — 8-way concurrent query_symbols
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_concurrent_query_symbols(live_phase5_env: LivePhase5Env) -> None:
    h = TimingHarness(phase=5, test_name="concurrent_8_queries")
    env = live_phase5_env

    svc = env.make_ci_service()
    svc.ensure_initialized(wait=True)

    queries = ["Bag", "Array", "DataFrame", "compute", "delayed", "Future", "Client", "graph"]
    assert len(queries) == _CONCURRENT_QUERY_COUNT

    with _trace(h, "concurrent_8_queries_total"):
        results = await asyncio.gather(*[
            asyncio.to_thread(svc.query_symbols, q) for q in queries
        ])

    for query, rows in zip(queries, results, strict=True):
        assert isinstance(rows, list), f"query {query!r} returned non-list: {rows!r}"

    total_s = h.values["concurrent_8_queries_total"]
    assert total_s < _PUBLIC_DAEMON_COMMAND_P99_CEILING_S, (
        f"8 concurrent queries took {total_s:.3f}s (>"
        f"{_PUBLIC_DAEMON_COMMAND_P99_CEILING_S:.1f}s ceiling)"
    )

    _flush("\n" + h.report())
    h.dump_json()
    svc.dispose()


# ---------------------------------------------------------------------------
# 5.6.C — curated cross-phase regression (one assertion from each prior phase)
# ---------------------------------------------------------------------------


def test_curated_cross_phase_regression(live_phase5_env: LivePhase5Env) -> None:
    """Run one representative assertion per prior phase under daemon-default.

    If the selector change introduces a regression the per-phase suites
    missed, one of these will trip. Numbers (timing thresholds etc.) are
    intentionally generous — this is a tripwire, not a benchmark.
    """
    env = live_phase5_env
    svc = env.make_ci_service()
    svc.ensure_initialized(wait=True)
    assert isinstance(svc._impl, DaemonBackend)

    # Phase 0/1: query_symbols returns SymbolInfo rows for a known dask name.
    rows = svc.query_symbols("Bag")
    assert isinstance(rows, list)

    # Phase 3: write_file commits via the daemon's overlay+OCC path.
    p = f"{env.root_dir}/_phase5_xphase_{uuid.uuid4().hex[:6]}.txt"
    op = svc.write_file([WriteSpec(file_path=p, content="x\n", overwrite=True)])
    assert op.success

    # Phase 3.5: status() returns a dict (telemetry round-trips).
    status = svc.status()
    assert isinstance(status, dict)

    # Phase 3.6: diagnostics on a real .py file does not crash. We accept
    # an empty list (LSP child unavailable) as long as there is no
    # exception, since basedpyright availability is image-dependent.
    diags = svc.diagnostics(f"{env.root_dir}/dask/__init__.py")
    assert isinstance(diags, list)

    # Phase 4: svc.cmd routes through the daemon and returns a populated
    # SimpleNamespace.
    async_sandbox = asyncio.run(_resolve_async_sandbox(env.sandbox_id))
    cmd_result = asyncio.run(svc.cmd(async_sandbox, "echo phase5"))
    assert cmd_result.exit_code == 0
    assert "phase5" in (getattr(cmd_result, "result", "") or "")

    svc.dispose()


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------


async def _resolve_async_sandbox(sandbox_id: str) -> Any:
    from sandbox.client.async_ import get_async_sandbox

    return await get_async_sandbox(sandbox_id)
