"""Phase 2 live E2E — CI daemon lifecycle probe suite.

Run with::

    .venv/bin/pytest backend/tests/test_e2e/test_live_ci_phase2_daemon_lifecycle.py -m live -v -s
"""

from __future__ import annotations

import asyncio
import logging
import os
import sys
import time
import uuid
from contextlib import contextmanager
from dataclasses import dataclass
from typing import Any
from collections.abc import Iterator
from unittest import mock

import pytest

from engine.testing.eval_agent import EvalAgent
from sandbox.api.bash import extract_exit_code, wrap_bash_command
from sandbox.code_intelligence.backends import DaemonBackend
from sandbox.code_intelligence.daemon.launcher import DaemonLauncher

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


def _dask_image() -> str:
    from benchmarks.sweevo.dataset import select_sweevo_instance
    from benchmarks.sweevo.sandbox import _normalize_sweevo_image_ref

    instance = select_sweevo_instance(instance_id=_DASK_SWEEVO_INSTANCE_ID)
    return _normalize_sweevo_image_ref(instance.docker_image)


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
        logging.getLogger("sandbox.lifecycle.proxy"),
        logging.getLogger("sandbox.lifecycle.workspace"),
        logging.getLogger("sandbox.code_intelligence.daemon.launcher"),
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


@contextmanager
def _print_eager_bootstrap_progress() -> Iterator[None]:
    """Print lifecycle-hook progress while ``create_sandbox`` is still running."""
    from sandbox.lifecycle import service as lifecycle_service

    original = lifecycle_service.bootstrap_in_sandbox_ci_runtime

    async def traced_bootstrap(
        sandbox_id: str,
        workspace_root: str,
        *,
        transport: Any,
    ) -> None:
        _flush_print(
            f"  -> eager_bootstrap_start sandbox={sandbox_id} "
            f"workspace={workspace_root}"
        )
        t0 = time.perf_counter()
        try:
            await original(
                sandbox_id=sandbox_id,
                workspace_root=workspace_root,
                transport=transport,
            )
        except Exception as exc:
            _flush_print(
                f"  !! eager_bootstrap_failed ({time.perf_counter() - t0:.3f}s): "
                f"{type(exc).__name__}: {exc}"
            )
            raise
        _flush_print(f"  ok eager_bootstrap_done ({time.perf_counter() - t0:.3f}s)")

    with mock.patch(
        "sandbox.lifecycle.service.bootstrap_in_sandbox_ci_runtime",
        new=traced_bootstrap,
    ):
        yield


@dataclass
class LivePhase2Env:
    sandbox_id: str
    raw_sandbox: Any
    repo_dir: str
    transport: Any

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
        return DaemonBackend(sandbox_id=self.sandbox_id, workspace_root=self.repo_dir, transport=self.transport)

    def launcher(self) -> DaemonLauncher:
        return DaemonLauncher(self.transport, self.sandbox_id, self.repo_dir)


@pytest.fixture(scope="module")
def live_phase2_env() -> Iterator[LivePhase2Env]:
    if not EvalAgent.has_daytona():
        pytest.skip("Daytona credentials not configured")

    from benchmarks.sweevo.dataset import select_sweevo_instance
    from benchmarks.sweevo.models import _CONDA_ACTIVATE
    from benchmarks.sweevo.sandbox import create_sweevo_test_sandbox
    from sandbox.daytona.transport import DaytonaTransport
    from sandbox.testing import delete_test_sandbox, get_sandbox_service

    instance = select_sweevo_instance(instance_id=_DASK_SWEEVO_INSTANCE_ID)
    sandbox_name = f"ci-phase2-{uuid.uuid4().hex[:8]}"
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
        env = LivePhase2Env(
            sandbox_id=sandbox_id,
            raw_sandbox=raw_sandbox,
            repo_dir=_DASK_SWEEVO_REPO_DIR,
            transport=DaytonaTransport(),
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


def test_daemon_ready_after_create_sandbox() -> None:
    """create_sandbox returns only after eager daemon bootstrap succeeds."""
    if not EvalAgent.has_daytona():
        pytest.skip("Daytona credentials not configured")

    from sandbox.daytona.transport import DaytonaTransport
    from sandbox.testing import delete_test_sandbox, get_sandbox_service

    h = TimingHarness(phase=2, test_name="daemon_ready_after_create")
    sandbox_id = ""
    with (
        mock.patch.dict(os.environ, {"EOS_CI_IN_SANDBOX": "1"}),
        _stream_live_logs(),
        _print_eager_bootstrap_progress(),
    ):
        with _traced_step(h, "create_sandbox_with_ci_bootstrap"):
            sandbox = get_sandbox_service().create_sandbox(
                name=f"ci-phase2-eager-{uuid.uuid4().hex[:8]}",
                image=_dask_image(),
                language="python",
                labels={"purpose": "ci-phase2", "project_dir": _DASK_SWEEVO_REPO_DIR},
            )
            sandbox_id = str(sandbox["id"])

        try:
            with _traced_step(h, "daemon_first_ping_no_retry"):
                daemon_backend = DaemonBackend(sandbox_id=sandbox_id, workspace_root=_DASK_SWEEVO_REPO_DIR, transport=DaytonaTransport())
                result = _asyncio_run(daemon_backend._call_daemon_command("ping"))
            assert result["pong"] is True

            raw = get_sandbox_service().get_sandbox_object(sandbox_id)
            env = LivePhase2Env(
                sandbox_id,
                raw,
                _DASK_SWEEVO_REPO_DIR,
                DaytonaTransport(),
            )
            pid_path = _asyncio_run(env.launcher().pid_path())
            with _traced_step(h, "pid_liveness_check"):
                code, out = env.exec(
                    f"pid=$(cat {pid_path}) && kill -0 \"$pid\" && "
                    "("
                    "if command -v ps >/dev/null 2>&1; then "
                    'ps -o pid,ppid,command -p "$pid"; '
                    "else echo \"pid=$pid ps=missing\"; "
                    "fi"
                    ")",
                    timeout=10,
                )
            assert code == 0, out
            _flush_print(f"  daemon_process:\n{out.rstrip()}")
        finally:
            delete_test_sandbox(sandbox_id)

    _flush_print(h.report())
    h.dump_json()


@pytest.mark.asyncio
async def test_daemon_kill_and_respawn(live_phase2_env: LivePhase2Env) -> None:
    h = TimingHarness(phase=2, test_name="kill_and_respawn")
    env = live_phase2_env
    daemon_backend = env.daemon_backend()
    launcher = env.launcher()

    with _traced_step(h, "initial_spawn_and_ping"):
        assert (await daemon_backend._call_daemon_command("ping"))["pong"] is True

    pid_path = await launcher.pid_path()
    with _traced_step(h, "daemon_kill9"):
        code, out = env.exec(f"kill -9 $(cat {pid_path})", timeout=10)
        assert code == 0, out
        await asyncio.sleep(0.3)

    with _traced_step(h, "daemon_respawn_via_call"):
        assert (await daemon_backend._call_daemon_command("ping"))["pong"] is True

    _flush_print(h.report())
    h.dump_json()


@pytest.mark.asyncio
async def test_daemon_clean_shutdown(live_phase2_env: LivePhase2Env) -> None:
    h = TimingHarness(phase=2, test_name="clean_shutdown")
    env = live_phase2_env
    daemon_backend = env.daemon_backend()
    launcher = env.launcher()

    with _traced_step(h, "initial_spawn"):
        assert (await daemon_backend._call_daemon_command("ping"))["pong"] is True

    pid_path = await launcher.pid_path()
    socket_path = await launcher.socket_path()
    with _traced_step(h, "shutdown_daemon_command"):
        assert (await daemon_backend._call_daemon_command("shutdown"))["shutting_down"] is True

    with _traced_step(h, "post_shutdown_settle"):
        await asyncio.sleep(0.5)

    with _traced_step(h, "verify_pid_cleanup"):
        code, _ = env.exec(f"test -f {pid_path}", timeout=10)
        assert code != 0
    with _traced_step(h, "verify_socket_cleanup"):
        code, _ = env.exec(f"test -S {socket_path}", timeout=10)
        assert code != 0

    _flush_print(h.report())
    h.dump_json()


@pytest.mark.asyncio
async def test_concurrent_pings(live_phase2_env: LivePhase2Env) -> None:
    env = live_phase2_env
    daemon_backend = env.daemon_backend()
    results = await asyncio.gather(*[daemon_backend._call_daemon_command("ping") for _ in range(8)])
    assert all(r["pong"] is True for r in results)


def test_dispose_sandbox_no_orphan_daemon() -> None:
    """Daytona disposal tears down the full sandbox, so daemon cleanup is implicit."""
    if not EvalAgent.has_daytona():
        pytest.skip("Daytona credentials not configured")

    from sandbox.daytona.transport import DaytonaTransport
    from sandbox.testing import delete_test_sandbox, get_sandbox_service

    h = TimingHarness(phase=2, test_name="dispose_no_orphan")
    sandbox_id = ""
    with (
        mock.patch.dict(os.environ, {"EOS_CI_IN_SANDBOX": "1"}),
        _stream_live_logs(),
        _print_eager_bootstrap_progress(),
    ):
        with _traced_step(h, "create_sandbox"):
            sandbox = get_sandbox_service().create_sandbox(
                name=f"ci-phase2-dispose-{uuid.uuid4().hex[:8]}",
                image=_dask_image(),
                language="python",
                labels={"purpose": "ci-phase2", "project_dir": _DASK_SWEEVO_REPO_DIR},
            )
            sandbox_id = str(sandbox["id"])

        with _traced_step(h, "spawn_daemon"):
            daemon_backend = DaemonBackend(sandbox_id=sandbox_id, workspace_root=_DASK_SWEEVO_REPO_DIR, transport=DaytonaTransport())
            assert _asyncio_run(daemon_backend._call_daemon_command("ping"))["pong"] is True

        with _traced_step(h, "dispose_sandbox"):
            delete_test_sandbox(sandbox_id)
            sandbox_id = ""

    if sandbox_id:
        delete_test_sandbox(sandbox_id)
    _flush_print(h.report())
    h.dump_json()
