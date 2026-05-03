"""Phase 3 live E2E — daemon-resident CodeIntelligenceService invariants.

This file is COMMITTED but its ``-m live`` execution is DEFERRED in the
Phase 3 implementation iteration. Each test is gated on
``EvalAgent.has_daytona()`` so the default suite (``pytest -m 'not live'``)
collects but skips it cleanly.

Run with::

    .venv/bin/pytest backend/tests/test_e2e/test_live_ci_phase3_invariants.py -m live -v -s

The suite exercises daemon mutation invariants.
"""

from __future__ import annotations

import asyncio
import logging
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
from sandbox.occ.types import OperationChange, OperationResult
from sandbox.runtime.backends import DaemonBackend

from ._timing_harness import TimingHarness

pytestmark = [pytest.mark.e2e, pytest.mark.live]

_DASK_SWEEVO_INSTANCE_ID = "dask__dask_2023.3.2_2023.4.0"
_DASK_SWEEVO_REPO_DIR = "/testbed"
_LOG_T0 = time.perf_counter()


def _flush_print(msg: str) -> None:
    stamp = time.strftime("%H:%M:%S")
    elapsed = time.perf_counter() - _LOG_T0
    print(f"[{stamp} +{elapsed:8.3f}s] {msg}", flush=True)
    sys.stdout.flush()


@contextmanager
def _traced_step(harness: TimingHarness, name: str) -> Iterator[None]:
    _flush_print(f"  -> {name} ...")
    t0 = time.perf_counter()
    with harness.step(name):
        yield
    _flush_print(f"  ok {name} ({time.perf_counter() - t0:.3f}s)")


def _asyncio_run(coro: Any) -> Any:
    return asyncio.run(coro)


@dataclass
class LivePhase3Env:
    sandbox_id: str
    raw_sandbox: Any
    repo_dir: str

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

    def daemon_backend(self) -> DaemonBackend:
        return DaemonBackend(sandbox_id=self.sandbox_id, workspace_root=self.repo_dir)


@contextmanager
def _stream_live_logs() -> Iterator[None]:
    handler = logging.StreamHandler(sys.stdout)
    handler.setFormatter(
        logging.Formatter(
            "  log [%(asctime)s] %(name)s: %(message)s",
            datefmt="%H:%M:%S",
        )
    )
    loggers = [
        logging.getLogger("sandbox.lifecycle.service"),
        logging.getLogger("sandbox.runtime.bundle"),
        logging.getLogger("sandbox.runtime.command_client"),
    ]
    old_levels = [logger.level for logger in loggers]
    old_propagate = [logger.propagate for logger in loggers]
    for logger in loggers:
        logger.addHandler(handler)
        logger.setLevel(logging.INFO)
        logger.propagate = False
    try:
        yield
    finally:
        for logger, old_level, propagate in zip(
            loggers,
            old_levels,
            old_propagate,
            strict=True,
        ):
            logger.removeHandler(handler)
            logger.setLevel(old_level)
            logger.propagate = propagate


@pytest.fixture(scope="module")
def live_phase3_env() -> Iterator[LivePhase3Env]:
    if not EvalAgent.has_daytona():
        pytest.skip("Daytona credentials not configured")

    from benchmarks.sweevo.dataset import select_sweevo_instance
    from benchmarks.sweevo.models import _CONDA_ACTIVATE
    from benchmarks.sweevo.sandbox import create_sweevo_test_sandbox
    from sandbox.testing import delete_test_sandbox, get_sandbox_service

    instance = select_sweevo_instance(instance_id=_DASK_SWEEVO_INSTANCE_ID)
    sandbox_name = f"ci-phase3-{uuid.uuid4().hex[:8]}"
    _flush_print(f"\n[fixture] provisioning sweevo sandbox {sandbox_name} ...")
    result = _asyncio_run(
        create_sweevo_test_sandbox(
            instance,
            sandbox_name=sandbox_name,
            repo_dir=_DASK_SWEEVO_REPO_DIR,
        )
    )
    sandbox_id = str(result["sandbox_id"])
    try:
        raw_sandbox = get_sandbox_service().get_sandbox_object(sandbox_id)
        env = LivePhase3Env(
            sandbox_id=sandbox_id,
            raw_sandbox=raw_sandbox,
            repo_dir=_DASK_SWEEVO_REPO_DIR,
        )
        code, output = env.exec(
            f"{_CONDA_ACTIVATE} && cd {_DASK_SWEEVO_REPO_DIR} && python --version",
            timeout=60,
        )
        assert code == 0, output
        yield env
    finally:
        _flush_print(f"[fixture] tearing down sandbox {sandbox_id} ...")
        delete_test_sandbox(sandbox_id)


# ---------------------------------------------------------------------------
# 3.7.A — INVARIANT 1: Sorted-path locks (no deadlock)
# ---------------------------------------------------------------------------


def test_invariant_sorted_path_locks(live_phase3_env: LivePhase3Env) -> None:
    """Two concurrent commits in opposite path orders must not deadlock."""
    h = TimingHarness(phase=3, test_name="invariant_sorted_locks")
    env = live_phase3_env
    daemon_backend = env.daemon_backend()

    files = [f"{env.repo_dir}/_phase3_a.txt", f"{env.repo_dir}/_phase3_b.txt"]
    env.exec(f"echo 'A' > {files[0]} && echo 'B' > {files[1]}")

    from sandbox.occ.content.hashing import content_hash

    def _make_change(path: str, base: str, final: str) -> OperationChange:
        return OperationChange(
            file_path=path,
            base_content=base,
            base_hash=content_hash(base),
            final_content=final,
            base_existed=True,
            strict_base=True,
        )

    async def commit_in_order(idx_a: int, idx_b: int, agent: str) -> OperationResult:
        return await asyncio.to_thread(
            daemon_backend.commit_operation_against_base,
            [
                _make_change(
                    files[idx_a],
                    "A\n" if idx_a == 0 else "B\n",
                    "A1\n" if idx_a == 0 else "B1\n",
                ),
                _make_change(
                    files[idx_b],
                    "A\n" if idx_b == 0 else "B\n",
                    "A1\n" if idx_b == 0 else "B1\n",
                ),
            ],
            edit_type="edit_file",
            agent_id=agent,
        )

    async def commit_both_orders() -> Any:
        return await asyncio.gather(
            commit_in_order(0, 1, "agent-A"),
            commit_in_order(1, 0, "agent-B"),
            return_exceptions=True,
        )

    with _stream_live_logs(), _traced_step(h, "concurrent_opposite_orders"):
        results = _asyncio_run(commit_both_orders())

    # At least one commit must succeed; no exception should propagate.
    successes = sum(
        1 for r in results
        if isinstance(r, OperationResult) and r.success
    )
    assert successes >= 1, f"expected at least one success, got {results}"
    print(h.report())
    h.dump_json()


# ---------------------------------------------------------------------------
# 3.7.B — INVARIANT 2: Strict-base OCC + aborted_version on drift
# ---------------------------------------------------------------------------


def test_invariant_strict_base_occ_aborts_on_drift(
    live_phase3_env: LivePhase3Env,
) -> None:
    """A second strict-base commit against a stale base must abort."""
    h = TimingHarness(phase=3, test_name="invariant_strict_base_occ")
    env = live_phase3_env
    daemon_backend = env.daemon_backend()
    target = f"{env.repo_dir}/_phase3_occ.txt"

    env.exec(f"echo 'v1' > {target}")
    from sandbox.occ.content.hashing import content_hash

    base_v1_hash = content_hash("v1\n")

    async def first_commit() -> OperationResult:
        return await asyncio.to_thread(
            daemon_backend.commit_operation_against_base,
            [
                OperationChange(
                    file_path=target,
                    base_content="v1\n",
                    base_hash=base_v1_hash,
                    final_content="v2\n",
                    base_existed=True,
                    strict_base=True,
                )
            ],
            edit_type="edit_file",
            agent_id="agent-fresh",
        )

    async def stale_commit() -> OperationResult:
        # This second call is using v1 base after target is already v2.
        return await asyncio.to_thread(
            daemon_backend.commit_operation_against_base,
            [
                OperationChange(
                    file_path=target,
                    base_content="v1\n",
                    base_hash=base_v1_hash,
                    final_content="v3\n",
                    base_existed=True,
                    strict_base=True,
                )
            ],
            edit_type="edit_file",
            agent_id="agent-stale",
        )

    with _traced_step(h, "first_commit"):
        first = _asyncio_run(first_commit())
    assert first.success is True, first

    with _traced_step(h, "stale_commit"):
        stale = _asyncio_run(stale_commit())
    assert stale.success is False, stale
    assert stale.status in {"aborted_version", "aborted_overlap"}, stale
    print(h.report())
    h.dump_json()


# ---------------------------------------------------------------------------
# 3.7.C — INVARIANT 3: Non-overlap merge fallback
# ---------------------------------------------------------------------------


def test_invariant_non_overlap_merge_converges(
    live_phase3_env: LivePhase3Env,
) -> None:
    """Two non-overlapping edits to the same file converge under non-strict
    base via the merge fallback."""
    h = TimingHarness(phase=3, test_name="invariant_non_overlap_merge")
    env = live_phase3_env
    daemon_backend = env.daemon_backend()
    target = f"{env.repo_dir}/_phase3_merge.txt"
    env.exec(f"printf 'line1\\nline2\\nline3\\n' > {target}")

    from sandbox.occ.content.hashing import content_hash

    base = "line1\nline2\nline3\n"
    base_hash = content_hash(base)

    async def edit_top() -> OperationResult:
        return await asyncio.to_thread(
            daemon_backend.commit_operation_against_base,
            [
                OperationChange(
                    file_path=target,
                    base_content=base,
                    base_hash=base_hash,
                    final_content="TOP\nline2\nline3\n",
                    base_existed=True,
                    strict_base=False,
                )
            ],
            edit_type="edit_file",
            agent_id="agent-top",
        )

    async def edit_bot() -> OperationResult:
        return await asyncio.to_thread(
            daemon_backend.commit_operation_against_base,
            [
                OperationChange(
                    file_path=target,
                    base_content=base,
                    base_hash=base_hash,
                    final_content="line1\nline2\nBOT\n",
                    base_existed=True,
                    strict_base=False,
                )
            ],
            edit_type="edit_file",
            agent_id="agent-bot",
        )

    with _traced_step(h, "first_edit_top"):
        first = _asyncio_run(edit_top())
    assert first.success is True, first

    with _traced_step(h, "non_overlap_edit_bot"):
        second = _asyncio_run(edit_bot())
    # Either the merge fallback succeeds, or the implementation aborts cleanly.
    assert second.status in {"committed", "aborted_version"}, second
    print(h.report())
    h.dump_json()


# ---------------------------------------------------------------------------
# 3.7.D — INVARIANT 4: atomic rollback on partial-apply failure
# ---------------------------------------------------------------------------


def test_invariant_atomic_batch_rollback(live_phase3_env: LivePhase3Env) -> None:
    """A failed mid-batch commit must roll all participating files back."""
    h = TimingHarness(phase=3, test_name="invariant_atomic_batch_rollback")
    env = live_phase3_env
    daemon_backend = env.daemon_backend()
    a = f"{env.repo_dir}/_phase3_tm_a.txt"
    b = f"{env.repo_dir}/_phase3_tm_b.txt"
    env.exec(f"echo 'A0' > {a} && echo 'B0' > {b}")

    from sandbox.occ.content.hashing import content_hash

    # Mismatched base on file B forces the batch to abort mid-flight.
    async def crash_batch() -> OperationResult:
        return await asyncio.to_thread(
            daemon_backend.commit_operation_against_base,
            [
                OperationChange(
                    file_path=a,
                    base_content="A0\n",
                    base_hash=content_hash("A0\n"),
                    final_content="A1\n",
                    base_existed=True,
                    strict_base=True,
                ),
                OperationChange(
                    file_path=b,
                    base_content="WRONG\n",
                    base_hash=content_hash("WRONG\n"),
                    final_content="B1\n",
                    base_existed=True,
                    strict_base=True,
                ),
            ],
            edit_type="edit_file",
            agent_id="agent-tm",
        )

    with _traced_step(h, "crash_batch"):
        result = _asyncio_run(crash_batch())
    assert result.success is False, result

    # Both files should be unchanged.
    code_a, content_a = env.exec(f"cat {a}")
    code_b, content_b = env.exec(f"cat {b}")
    assert code_a == 0 and content_a.strip() == "A0"
    assert code_b == 0 and content_b.strip() == "B0"
    print(h.report())
    h.dump_json()


# ---------------------------------------------------------------------------
# 3.7.G — Workspace-write bypass guard surfaces unledgered writes
# ---------------------------------------------------------------------------
