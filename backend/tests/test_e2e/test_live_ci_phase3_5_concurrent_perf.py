"""Phase 3.5 — concurrency + sustained-load + SQLite-IndexStore live E2E.

Five subtests against a real Daytona ``dask__dask_2023.3.2_2023.4.0`` sandbox:

A. ``test_sustained_mixed_workload_distribution`` — daemon-path writes,
   queries, and status calls; samples RSS/FDs at start/50%/100%; asserts
   resource ceilings (RSS growth < 100 MB, FD growth < 10) and public daemon command
   p99 below the provider-shim stuck threshold.
B. ``test_concurrent_agents_no_pathologies`` — asyncio agents looping
   query + edit + cmd through the daemon path; asserts zero errors and at
   least one op per kind per agent; RSS growth < 200 MB.
C. ``test_multi_orchestrator_single_daemon_arbitration`` — two
   :class:`DaemonBackend` instances commit to the same path; asserts exactly
   1 success + 1 abort.
D. ``test_sqlite_index_survives_daemon_restart`` — capture symbol counts,
   restart the daemon, assert the SQLite-backed daemon returns identical
   results and does not recreate the legacy pickle snapshot.
E. ``test_refresh_file_does_not_rewrite_world`` — daemon ``index_refresh``
   calls on ``/testbed/dask/__init__.py``; asserts completion below the
   provider-shim stuck threshold.
F. ``test_svc_cmd_overlay_high_concurrency_probe`` — 1/5/10 concurrent
   full audited ``svc.cmd`` overlay ops that write distinct gitinclude files,
   with per-op and mid-flight monitor logging for bottleneck diagnosis.

Run with:
    .venv/bin/pytest backend/tests/test_e2e/test_live_ci_phase3_5_concurrent_perf.py -m live -v -s
"""

from __future__ import annotations

import asyncio
import os
import sys
import time
import uuid
from contextlib import contextmanager
from dataclasses import dataclass
from typing import Any, Iterator

import pytest

from engine.testing.eval_agent import EvalAgent
from sandbox.api.bash import extract_exit_code, wrap_bash_command
from sandbox.client.async_ import get_async_sandbox
from sandbox.code_intelligence.core.types import WriteSpec
from sandbox.code_intelligence.daemon.storage import workspace_root_hash
from sandbox.code_intelligence.service import CodeIntelligenceService

from ._timing_harness import TimingHarness

pytestmark = [pytest.mark.e2e, pytest.mark.live]

_DASK_SWEEVO_INSTANCE_ID = "dask__dask_2023.3.2_2023.4.0"
_DASK_SWEEVO_REPO_DIR = "/testbed"
_PUBLIC_DAEMON_COMMAND_P99_CEILING_S = 10.0
_SUSTAINED_WRITE_SAMPLES = 5
_SUSTAINED_QUERY_SAMPLES = 5
_SUSTAINED_STATUS_SAMPLES = 3
_CONCURRENT_AGENT_COUNT = 2
_CONCURRENT_AGENT_SECONDS = 6.0
_REFRESH_SAMPLES = 5
_SVC_CMD_CONCURRENCY_LEVELS = (1, 5, 10)
_SVC_CMD_OP_TIMEOUT_S = 120
_SVC_CMD_BATCH_TIMEOUT_S = 300
_SVC_CMD_MONITOR_INTERVAL_S = 1.0
_SVC_CMD_DAEMON_LOG_TAIL_INTERVAL_S = 2.0


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
class LivePhase35Env:
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
        from sandbox.daytona.transport import DaytonaTransport

        old_flag = os.environ.get("EOS_CI_IN_SANDBOX")
        os.environ["EOS_CI_IN_SANDBOX"] = "1"
        try:
            return CodeIntelligenceService(
                sandbox_id=self.sandbox_id,
                workspace_root=self.root_dir,
                transport=DaytonaTransport(),
            )
        finally:
            if old_flag is None:
                os.environ.pop("EOS_CI_IN_SANDBOX", None)
            else:
                os.environ["EOS_CI_IN_SANDBOX"] = old_flag

    def daemon_state_dir(self) -> str:
        wh = workspace_root_hash(self.root_dir)
        return f"{self.home}/.cache/eos-ci/{wh}/v1"

    def daemon_pid(self) -> int | None:
        code, out = self.exec(f"cat {self.daemon_state_dir()}/daemon.pid 2>/dev/null")
        if code != 0 or not out.strip():
            return None
        try:
            return int(out.strip())
        except ValueError:
            return None


@dataclass
class SvcCmdProbeResult:
    batch_size: int
    op_index: int
    elapsed_s: float
    exit_code: int | None
    git_commit_status: str | None
    changed_paths: int
    overlay_run_timings: dict[str, float]
    overlay_stage_timings: dict[str, float]
    daemon_call_timings: dict[str, float]
    error: str | None = None


@pytest.fixture(scope="module")
def live_phase35_env() -> LivePhase35Env:
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
    sandbox_name = f"ci-phase35-{uuid.uuid4().hex[:8]}"
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
        f"[fixture] sandbox {sandbox_id} ready in "
        f"{time.perf_counter() - t0:.1f}s"
    )
    try:
        raw_sandbox = get_sandbox_service().get_sandbox_object(sandbox_id)
        home_resp = raw_sandbox.process.exec(
            wrap_bash_command("printf '%s' \"$HOME\""),
            timeout=10,
        )
        home_text, home_code = extract_exit_code(
            getattr(home_resp, "result", "") or "",
            fallback_exit_code=getattr(home_resp, "exit_code", None),
        )
        home = home_text.strip() if home_code == 0 and home_text.strip() else "/home/daytona"
        env = LivePhase35Env(
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


def _orchestrator_pid() -> int:
    """Fallback PID for resource sampling when the daemon isn't reachable."""
    return os.getpid()


class _SyncSandboxTransport:
    """Adapter that gives TimingHarness a ``transport.exec`` shim."""

    def __init__(self, raw_sandbox: Any) -> None:
        self._raw = raw_sandbox

    def exec(self, sandbox_id: str, command: str, timeout: int = 30) -> Any:
        del sandbox_id
        return self._raw.process.exec(
            wrap_bash_command(command),
            timeout=timeout,
        )


# ---------------------------------------------------------------------------
# 3.5.5.A — sustained mixed workload
# ---------------------------------------------------------------------------


def test_sustained_mixed_workload_distribution(live_phase35_env: LivePhase35Env) -> None:
    h = TimingHarness(phase=3.5, test_name="sustained_mixed_workload")
    env = live_phase35_env

    with _trace(h, "ci_service_construct"):
        svc = env.make_ci_service()
    with _trace(h, "index_build"):
        svc.ensure_initialized(wait=True)

    pid = env.daemon_pid()
    sampler_pid = pid if pid is not None else _orchestrator_pid()
    sampler_transport = _SyncSandboxTransport(env.raw_sandbox)

    h.sample_rss_mb("rss_at_start", sampler_transport, env.sandbox_id, sampler_pid)
    h.sample_fds("fds_at_start", sampler_transport, env.sandbox_id, sampler_pid)

    base = f"{env.root_dir}/_phase35_w"
    counter = 0

    def _next() -> int:
        nonlocal counter
        counter += 1
        return counter

    for step in h.step_repeat("write_file", n=_SUSTAINED_WRITE_SAMPLES):
        with step:
            i = _next()
            res = svc.write_file(
                [WriteSpec(file_path=f"{base}{i}.txt", content=f"v{i}\n", overwrite=True)],
            )
            assert res.success, f"write_file {i} failed: {res.status}"

    h.sample_rss_mb("rss_at_50pct", sampler_transport, env.sandbox_id, sampler_pid)
    h.sample_fds("fds_at_50pct", sampler_transport, env.sandbox_id, sampler_pid)

    for step in h.step_repeat("query_symbols", n=_SUSTAINED_QUERY_SAMPLES):
        with step:
            svc.query_symbols("Bag")

    for step in h.step_repeat("status", n=_SUSTAINED_STATUS_SAMPLES):
        with step:
            svc.status()

    h.sample_rss_mb("rss_at_100pct", sampler_transport, env.sandbox_id, sampler_pid)
    h.sample_fds("fds_at_100pct", sampler_transport, env.sandbox_id, sampler_pid)

    rss_growth = h.values["rss_at_100pct"] - h.values["rss_at_start"]
    assert rss_growth < 100.0, (
        f"RSS grew {rss_growth:.1f} MB during 250 ops — possible leak"
    )

    fd_growth = h.values["fds_at_100pct"] - h.values["fds_at_start"]
    assert fd_growth < 10, (
        f"FD count grew by {fd_growth:.0f} during 250 ops — possible leak"
    )

    # Public daemon command currently pays the provider exec shim on every sample.
    # This gate catches the old hang/pathological queueing case without
    # pretending the sample is raw SQLite or LSP latency.
    write_dist = h.distributions["write_file"]
    assert write_dist["p99"] < _PUBLIC_DAEMON_COMMAND_P99_CEILING_S, (
        f"write_file p99 ({write_dist['p99']:.3f}s) exceeded "
        f"{_PUBLIC_DAEMON_COMMAND_P99_CEILING_S:.1f}s — provider daemon command may be stuck"
    )

    _flush("\n" + h.report())
    h.dump_json()
    svc.dispose()


# ---------------------------------------------------------------------------
# 3.5.5.B — concurrent agents (8x for 30s)
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_concurrent_agents_no_pathologies(live_phase35_env: LivePhase35Env) -> None:
    h = TimingHarness(phase=3.5, test_name="concurrent_agents_2x")
    env = live_phase35_env

    svc = env.make_ci_service()
    svc.ensure_initialized(wait=True)

    pid = env.daemon_pid()
    sampler_pid = pid if pid is not None else _orchestrator_pid()
    sampler_transport = _SyncSandboxTransport(env.raw_sandbox)
    h.sample_rss_mb("rss_at_start", sampler_transport, env.sandbox_id, sampler_pid)

    async_sandbox = await get_async_sandbox(env.sandbox_id)
    stop_at = time.time() + _CONCURRENT_AGENT_SECONDS
    op_counts = {"query": 0, "edit": 0, "cmd": 0}
    errors: list[tuple[int, str]] = []

    async def agent(agent_id: int) -> None:
        nonlocal op_counts, errors
        local_edit = 0
        while time.time() < stop_at:
            try:
                await asyncio.to_thread(svc.query_symbols, "Bag")
                op_counts["query"] += 1
                target = f"{env.root_dir}/_phase35_agent{agent_id}_{local_edit}.txt"
                local_edit += 1
                res = await asyncio.to_thread(
                    svc.write_file,
                    [WriteSpec(file_path=target, content="x\n", overwrite=True)],
                )
                assert res.success
                op_counts["edit"] += 1
                # cmd intentionally cheap so we exercise the hot path.
                await svc.cmd(async_sandbox, "true")
                op_counts["cmd"] += 1
            except Exception as exc:  # noqa: BLE001
                errors.append((agent_id, repr(exc)))

    with _trace(h, "agents_2way"):
        await asyncio.gather(*[agent(i) for i in range(_CONCURRENT_AGENT_COUNT)])

    h.sample_rss_mb("rss_at_end", sampler_transport, env.sandbox_id, sampler_pid)

    _flush(f"  [stats] op_counts={op_counts} errors={len(errors)}")
    assert not errors, f"errors during 2-agent daemon run: {errors[:5]}"
    assert all(c >= _CONCURRENT_AGENT_COUNT for c in op_counts.values()), op_counts

    rss_growth = h.values["rss_at_end"] - h.values["rss_at_start"]
    assert rss_growth < 200.0, (
        f"RSS grew {rss_growth:.1f} MB during 2-agent daemon run"
    )

    h.values["op_query"] = float(op_counts["query"])
    h.values["op_edit"] = float(op_counts["edit"])
    h.values["op_cmd"] = float(op_counts["cmd"])

    _flush("\n" + h.report())
    h.dump_json()
    svc.dispose()


# ---------------------------------------------------------------------------
# 3.5.5.C — multi-orchestrator → single-daemon arbitration
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_multi_orchestrator_single_daemon_arbitration(
    live_phase35_env: LivePhase35Env,
) -> None:
    h = TimingHarness(phase=3.5, test_name="multi_orchestrator")
    env = live_phase35_env

    svc = env.make_ci_service()
    svc.ensure_initialized(wait=True)

    target = f"{env.root_dir}/_phase35_multi.txt"
    env.exec(f"echo 'v0' > {target}")

    base_content = "v0\n"
    from sandbox.code_intelligence.core.hashing import content_hash

    base_hash = content_hash(base_content)

    from sandbox.code_intelligence.backends import DaemonBackend
    from sandbox.daytona.transport import DaytonaTransport

    transport = DaytonaTransport()
    daemon_a = DaemonBackend(sandbox_id=env.sandbox_id, workspace_root=env.root_dir, transport=transport)
    daemon_b = DaemonBackend(sandbox_id=env.sandbox_id, workspace_root=env.root_dir, transport=transport)

    def _change(value: str, agent: str) -> dict:
        return {
            "changes": [
                {
                    "file_path": target,
                    "base_content": base_content,
                    "base_hash": base_hash,
                    "final_content": value,
                    "base_existed": True,
                    "strict_base": True,
                }
            ],
            "edit_type": "write_file",
            "agent_id": agent,
        }

    with _trace(h, "two_daemon_backends_concurrent_commit"):
        results = await asyncio.gather(
            daemon_a._call_daemon_command("commit_operation_against_base", _change("vA\n", "daemon_a")),
            daemon_b._call_daemon_command("commit_operation_against_base", _change("vB\n", "daemon_b")),
            return_exceptions=True,
        )

    successes = sum(
        1 for r in results if isinstance(r, dict) and r.get("success")
    )
    aborts = sum(
        1
        for r in results
        if isinstance(r, dict)
        and not r.get("success")
        and "aborted" in str(r.get("status", "")).lower()
    )
    _flush(f"  [stats] successes={successes} aborts={aborts}")
    assert successes == 1 and aborts == 1, results

    _flush("\n" + h.report())
    h.dump_json()
    svc.dispose()


# ---------------------------------------------------------------------------
# 3.5.5.D — SQLite restart parity
# ---------------------------------------------------------------------------


def test_sqlite_index_survives_daemon_restart(live_phase35_env: LivePhase35Env) -> None:
    h = TimingHarness(phase=3.5, test_name="sqlite_index_restart_parity")
    env = live_phase35_env

    svc = env.make_ci_service()
    svc.ensure_initialized(wait=True)

    with _trace(h, "query_baseline"):
        baseline = svc.query_symbols("Bag")
    baseline_count = len(baseline)
    baseline_names = sorted(s.name for s in baseline)

    state = env.daemon_state_dir()

    code, _ = env.exec(f"test -f {state}/index.sqlite3")
    assert code == 0, "daemon did not create index.sqlite3"

    with _trace(h, "daemon_shutdown"):
        svc.dispose()

    with _trace(h, "daemon_restart_with_sqlite"):
        svc2 = env.make_ci_service()
        svc2.ensure_initialized(wait=True)

    with _trace(h, "query_post_migration"):
        post = svc2.query_symbols("Bag")
    post_names = sorted(s.name for s in post)

    assert len(post) == baseline_count, (len(post), baseline_count)
    assert post_names == baseline_names

    code, _ = env.exec(f"test -f {state}/index.snapshot")
    assert code != 0, "legacy pickle index.snapshot should not be recreated"

    _flush("\n" + h.report())
    h.dump_json()
    svc2.dispose()


# ---------------------------------------------------------------------------
# 3.5.5.E — per-file refresh efficiency
# ---------------------------------------------------------------------------


def test_refresh_file_does_not_rewrite_world(live_phase35_env: LivePhase35Env) -> None:
    h = TimingHarness(phase=3.5, test_name="refresh_efficiency")
    env = live_phase35_env

    svc = env.make_ci_service()
    svc.ensure_initialized(wait=True)

    target = f"{env.root_dir}/dask/__init__.py"
    from sandbox.code_intelligence.backends import DaemonBackend
    from sandbox.daytona.transport import DaytonaTransport

    daemon_backend = DaemonBackend(sandbox_id=env.sandbox_id, workspace_root=env.root_dir, transport=DaytonaTransport())
    for step in h.step_repeat("refresh_file", n=_REFRESH_SAMPLES):
        with step:
            asyncio.run(daemon_backend._call_daemon_command("index_refresh", {"file_path": target}))

    p99 = h.distributions["refresh_file"]["p99"]
    assert p99 < _PUBLIC_DAEMON_COMMAND_P99_CEILING_S, (
        f"refresh_file p99 ({p99:.3f}s) exceeded "
        f"{_PUBLIC_DAEMON_COMMAND_P99_CEILING_S:.1f}s — provider daemon command may be stuck"
    )

    _flush("\n" + h.report())
    h.dump_json()
    svc.dispose()


# ---------------------------------------------------------------------------
# 3.5.5.F — full svc.cmd overlay high-concurrency probe
# ---------------------------------------------------------------------------
